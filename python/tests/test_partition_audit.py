"""INV-SELECT-01 / INV-MEMBER-01: selected history survives reversion and domain changes."""

import json
import os
import random
import sqlite3
import subprocess
from collections import Counter

import pytest
from barca.api import _find_binary


def environment():
    return {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")} | {
        "BARCA_POOL_SIZE": "2"
    }


@pytest.mark.parametrize("batch", ["None", "3", '"all"'])
def test_mutating_partition_domain_matches_oracle(tmp_path, batch):
    body = """import json
from pathlib import Path
from barca import sensor, asset, partitions_from

@sensor()
def source():
    return json.loads(Path("values.json").read_text())

@asset(partitions={"key": partitions_from(source)}, batch=3)
def first(key, source):
    keys = key if isinstance(key, list) else [key]
    with Path("calls").open("a") as f:
        f.write(json.dumps(keys) + "\\n")
    if isinstance(key, list):
        return {k: source[k] * 2 for k in key}
    return source * 2

@asset(partitions={"key": partitions_from(first)})
def second(key, first):
    return first + 1
""".replace("batch=3", f"batch={batch}")
    (tmp_path / "pipeline.py").write_text(body)
    (tmp_path / "barca.toml").write_text("")
    values = {f"k{i}": i for i in range(11)}
    rng = random.Random(713)
    materialized = set()
    removed = {}
    original = values.copy()
    for turn in range(23):
        if turn == 20:
            values = {}
        elif turn == 21:
            values = original.copy()
        elif turn == 22:
            pass
        elif turn % 8 == 6 and removed:
            key = next(iter(removed))
            values[key] = removed.pop(key)
        elif turn % 4 == 1:
            key = rng.choice(list(values))
            removed[key] = values.pop(key)
        elif turn % 4 == 2:
            values[f"new{turn}"] = turn
        elif turn % 4 == 3:
            for key in rng.sample(list(values), min(3, len(values))):
                values[key] += 10
        (tmp_path / "values.json").write_text(json.dumps(values))
        calls = tmp_path / "calls"
        calls.write_text("")
        result = subprocess.run(
            [_find_binary(), "get", "second", "pipeline.py", "--json", "-o", "value"],
            check=False,
            cwd=tmp_path,
            env=environment(),
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        actual = json.loads(result.stdout)["final_output"]
        assert actual == {f"key={k}": v * 2 + 1 for k, v in values.items()}, (turn, actual)
        changed = {k for k, v in values.items() if (k, v) not in materialized}
        executed = [k for line in calls.read_text().splitlines() for k in json.loads(line)]
        assert Counter(executed) == Counter(changed), (turn, executed, changed)
        materialized.update(values.items())
        with sqlite3.connect(tmp_path / ".barca/metadata.db") as db:
            for key in removed:
                assert (
                    db.execute(
                        "SELECT COUNT(*) FROM materializations WHERE node_id = ? AND status = 'success'",
                        (f"pipeline.py:first[key={key}]",),
                    ).fetchone()[0]
                    > 0
                )


def test_partial_batch_commits_successes_and_only_retries_failures(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text("""from pathlib import Path
import json
from barca import sensor, asset, partitions_from, Failed

@sensor()
def source():
    return {"a": 1, "b": 2, "c": 3}

@asset(partitions={"key": partitions_from(source)}, batch="all")
def first(key, source):
    with Path("calls").open("a") as f:
        f.write(json.dumps(key) + "\\n")
    return {k: Failed("injected") if k == "b" and Path("fail").exists() else source[k] * 2 for k in key}

@asset(partitions={"key": partitions_from(first)})
def second(key, first):
    with Path("downstream").open("a") as f:
        f.write(key + "\\n")
    return first + 1
""")
    (tmp_path / "fail").touch()

    def run():
        return subprocess.run(
            [_find_binary(), "get", "second", "pipeline.py", "--json", "-o", "value"],
            check=False,
            cwd=tmp_path,
            env=environment(),
            capture_output=True,
            text=True,
            timeout=30,
        )

    result = run()
    assert result.returncode == 1, result.stdout + result.stderr
    assert set((tmp_path / "downstream").read_text().splitlines()) == {"a", "c"}
    (tmp_path / "fail").unlink()
    (tmp_path / "calls").write_text("")
    (tmp_path / "downstream").write_text("")
    result = run()
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(result.stdout)["final_output"] == {"key=a": 3, "key=b": 5, "key=c": 7}
    assert json.loads((tmp_path / "calls").read_text()) == ["b"]
    assert (tmp_path / "downstream").read_text().splitlines() == ["b"]


@pytest.mark.parametrize("producer", ["sensor", "asset"])
@pytest.mark.parametrize("partitioned", [False, True])
def test_return_to_old_source_value_stays_correct_on_following_cache_hits(
    tmp_path, partitioned, producer
):
    source = """import json
from pathlib import Path
from barca import sensor, asset, partitions_from

@sensor()
def source():
    return json.loads(Path("value.json").read_text())

@asset(inputs={"source": source})
def report(source):
    return source * 2
"""
    if producer == "asset":
        source = source.replace("@sensor()", '@asset(env=["AUDIT_SOURCE_VERSION"])')
    if partitioned:
        source = source.replace(
            '@asset(inputs={"source": source})',
            '@asset(partitions={"key": partitions_from(source)})',
        ).replace("def report(source):", "def report(key, source):")
    (tmp_path / "pipeline.py").write_text(source)
    (tmp_path / "barca.toml").write_text("")
    for value in [1, 2, 1, 1]:
        (tmp_path / "value.json").write_text(json.dumps({"a": value} if partitioned else value))
        result = subprocess.run(
            [_find_binary(), "get", "report", "pipeline.py", "--json", "-o", "value"],
            check=False,
            cwd=tmp_path,
            env=environment() | {"AUDIT_SOURCE_VERSION": str(value)},
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout)["final_output"] == (
            {"key=a": value * 2} if partitioned else value * 2
        )


@pytest.mark.parametrize("producer", ["sensor", "asset"])
def test_historical_selections_over_budget_fall_back_without_stale_values(tmp_path, producer):
    (tmp_path / "pipeline.py").write_text("""import json
from pathlib import Path
from barca import sensor, asset, partitions_from, collect

@sensor()
def source():
    return json.loads(Path("value.json").read_text())

@asset(partitions={"key": partitions_from(source)}, batch="all")
def report(key, source):
    return {k: source[k] * 2 for k in key}

@asset(inputs={"reports": collect(report)})
def total(reports):
    return sum(reports)
""")
    if producer == "asset":
        pipeline = tmp_path / "pipeline.py"
        pipeline.write_text(
            pipeline.read_text().replace("@sensor()", '@asset(env=["AUDIT_SOURCE_VERSION"])')
        )
    (tmp_path / "barca.toml").write_text("")
    for turn, value in enumerate([1, 2, 1, 1, 2, 2]):
        (tmp_path / "value.json").write_text(json.dumps({f"k{i}": value for i in range(3000)}))
        result = subprocess.run(
            [_find_binary(), "get", "total", "pipeline.py", "--json", "-o", "value"],
            cwd=tmp_path,
            env=environment() | {"AUDIT_SOURCE_VERSION": str(value)},
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout)["final_output"] == 6000 * value
        with sqlite3.connect(tmp_path / ".barca/metadata.db") as db:
            proofs = db.execute("SELECT id, snapshot FROM freshness_proofs").fetchall()
            if turn == 2:
                assert not any(id.endswith(":report") for id, _ in proofs)
            for _, snapshot in proofs:
                assert (
                    len(json.dumps(json.loads(snapshot).get("selected", {})).encode()) <= 256 * 1024
                )
