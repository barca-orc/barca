"""`barca status`: one read-only view of every node — definition, cache state, last
materialization and artifact shape.

Status shares its cache decisions with `--dry-run` (so they cannot disagree), reads history from
the metadata DB, and reads artifact shape from the artifact file alone: user code is never
imported, and nothing is written.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset, task


@asset()
def src() -> list:
    return [{"id": 1, "name": "a"}, {"id": 2, "name": "b"}, {"id": 3, "name": None}]


@asset(inputs={"s": src})
def mid(s: list) -> dict:
    return {"n": len(s)}


@task(inputs={"m": mid})
def report(m: dict) -> dict:
    return m
"""

FRAMES = """
import pandas as pd
from barca import asset


@asset()
def table() -> pd.DataFrame:
    return pd.DataFrame({"x": [1, 2, 3, 4], "y": ["a", "b", "c", "d"]})
"""

# Importing this module leaves a marker file: status must never import it.
PICKLED = """
from pathlib import Path
from barca import asset

Path("imported.marker").write_text("imported")


class Thing:
    def __init__(self, n):
        self.n = n


@asset()
def thing() -> Thing:
    return Thing(3)


@asset()
def numbers() -> set:
    return {1, 2, 3}
"""

PARTITIONED = """
import os
from barca import asset, collect, partitions

KEYS = os.environ.get("BARCA_TEST_KEYS", "a,b").split(",")


@asset(partitions={"k": partitions([x for x in KEYS])})
def part(k: str) -> dict:
    return {"k": k}


@asset(inputs={"parts": collect(part)})
def summary(parts: list) -> dict:
    return {"keys": sorted(p["k"] for p in parts)}
"""

FAILING = """
from barca import asset


@asset()
def boom() -> dict:
    raise ValueError("kaboom")
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "frames.py").write_text(FRAMES)
    (tmp_path / "pickled.py").write_text(PICKLED)
    (tmp_path / "parts.py").write_text(PARTITIONED)
    (tmp_path / "failing.py").write_text(FAILING)
    return tmp_path


def barca(project: Path, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def status(project: Path, *args: str, env: dict | None = None) -> dict:
    return ok(barca(project, "status", *args, "--json", env=env))


def nodes(result: dict) -> dict:
    return {n["name"]: n for n in result["nodes"]}


def test_status_on_a_fresh_project_reports_never_run_and_writes_nothing(project):
    result = status(project, "pipeline.py")
    by = nodes(result)
    assert list(by) == ["src", "mid", "report"]
    assert by["src"]["kind"] == "asset" and by["src"]["inputs"] == []
    assert by["mid"]["inputs"] == ["pipeline.py:src"]
    assert by["src"]["partitioned"] is False
    assert by["src"]["cache"]["state"] == "never_run"
    assert by["src"]["cache"]["reason"] == "no_record"
    assert by["src"]["last_materialization"] is None
    assert by["report"]["kind"] == "task"
    assert by["report"]["cache"]["state"] == "always_runs"
    assert result["summary"]["never_run"] == 2
    assert not (project / ".barca").exists(), "status must not create .barca"


def test_status_after_a_run_shows_cache_last_materialization_and_json_shape(project):
    ok(barca(project, "get", "mid", "pipeline.py"))
    by = nodes(status(project, "pipeline.py"))
    src = by["src"]
    assert src["cache"]["state"] == "cached"
    last = src["last_materialization"]
    assert last["status"] == "success"
    assert last["run_hash"] == src["cache"]["run_hash"]
    assert last["format"] == "json" and last["artifact"].endswith(".json")
    assert last["size_bytes"] > 0 and last["created_at"]
    assert last["elapsed_seconds"] is not None
    assert src["shape"]["rows"] == 3
    assert src["shape"]["columns"] == [
        {"name": "id", "type": "int"},
        {"name": "name", "type": "str | null"},
    ]
    assert "sample" not in src["shape"], "sample rows are opt-in"
    # A dict artifact reports its keys.
    assert by["mid"]["shape"]["type"] == "dict"
    assert by["mid"]["shape"]["keys"] == ["n"]


def test_sample_rows_are_opt_in_and_bounded(project):
    ok(barca(project, "get", "src", "pipeline.py"))
    src = nodes(status(project, "pipeline.py", "--sample", "2"))["src"]
    assert src["shape"]["sample"] == [{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]


def test_changing_code_marks_the_asset_and_its_downstream_stale(project):
    ok(barca(project, "get", "mid", "pipeline.py"))
    path = project / "pipeline.py"
    path.write_text(path.read_text().replace('"name": "a"', '"name": "z"'))
    by = nodes(status(project, "pipeline.py"))
    assert by["src"]["cache"]["state"] == "stale"
    assert by["src"]["cache"]["reason"] == "changed"
    assert by["mid"]["cache"]["state"] == "stale"
    assert by["mid"]["cache"]["reason"] == "upstream_stale"
    assert "src" in by["mid"]["cache"]["detail"]
    # The previous materialization is still reported.
    assert by["src"]["last_materialization"]["status"] == "success"


def test_status_agrees_with_dry_run(project):
    ok(barca(project, "get", "src", "pipeline.py"))
    dry = ok(barca(project, "run", "report", "pipeline.py", "--dry-run"))
    by = nodes(status(project, "report", "pipeline.py"))
    for step in dry["steps"]:
        name = step["id"].split(":")[-1]
        expected = {"cached": "cached", "run": None}[step["action"]]
        if expected:
            assert by[name]["cache"]["state"] == expected
        else:
            assert by[name]["cache"]["state"] != "cached"


def test_target_scopes_status_to_the_upstream_cone(project):
    result = status(project, "mid", "pipeline.py")
    assert result["target"] == "mid"
    assert result["targets"] == ["mid"]
    assert list(nodes(result)) == ["src", "mid"]


def test_several_targets_scope_status_to_the_union_of_their_cones(project):
    # The same `a,b` parsing as get and run (#180).
    result = status(project, "mid,report", "pipeline.py")
    assert result["target"] is None
    assert result["targets"] == ["mid", "report"]
    assert list(nodes(result)) == ["src", "mid", "report"]
    whole = status(project, "pipeline.py")
    assert whole["target"] is None and whole["targets"] == []
    bad = barca(project, "status", "mid,,report", "pipeline.py", "--json")
    assert bad.returncode == 2 and "empty target name" in bad.stderr


def test_cache_states_use_one_spelling_in_state_and_summary(project):
    # `cache.state` values are exactly the `summary` keys (#180): snake_case everywhere.
    result = status(project, "pipeline.py")
    states = {n["cache"]["state"] for n in result["nodes"]}
    assert states <= set(result["summary"]), (states, result["summary"])
    assert "never_run" in states and "always_runs" in states


def test_an_unknown_target_is_an_error(project):
    proc = barca(project, "status", "nope", "pipeline.py", "--json")
    assert proc.returncode == 2  # usage error (#154)
    assert "nope" in proc.stderr
    err = json.loads(proc.stderr.strip().splitlines()[-1])
    assert err["kind"] == "usage"
    # One remediation wording on every command (#180).
    assert err["remediation"] == "Run `barca list pipeline.py` to see available assets and tasks."
    got = barca(project, "get", "nope", "pipeline.py", "--json")
    assert json.loads(got.stderr.strip().splitlines()[-1])["remediation"] == err["remediation"]


def test_parquet_shape_reports_rows_and_schema(project):
    ok(barca(project, "get", "table", "frames.py"))
    table = nodes(status(project, "frames.py", "--sample", "1"))["table"]
    assert table["last_materialization"]["format"] == "parquet"
    assert table["shape"]["rows"] == 4
    names = [c["name"] for c in table["shape"]["columns"]]
    assert names[:2] == ["x", "y"]
    assert table["shape"]["columns"][0]["type"] == "int64"
    assert table["shape"]["sample"] == [{"x": 1, "y": "a"}]


def test_pickle_reports_the_type_without_importing_user_code(project):
    ok(barca(project, "get", "pickled.py"))
    (project / "imported.marker").unlink()
    by = nodes(status(project, "pickled.py", "--sample", "3"))
    assert by["thing"]["last_materialization"]["format"] == "pickle"
    assert by["thing"]["shape"]["type"].endswith("Thing")
    assert by["numbers"]["shape"]["type"] == "set"
    assert "sample" not in by["thing"]["shape"], "pickles are never loaded"
    assert not (project / "imported.marker").exists(), "status imported user code"


def test_partitioned_assets_summarize_per_key_state(project):
    ok(barca(project, "get", "summary", "parts.py", env={"BARCA_TEST_KEYS": "a,b"}))
    cached = nodes(status(project, "parts.py", env={"BARCA_TEST_KEYS": "a,b"}))["part"]
    assert cached["partitioned"] is True
    assert cached["cache"]["state"] == "cached"
    assert cached["partitions"] == {"total": 2, "cached": 2, "missing": 0, "missing_keys": []}
    assert cached["last_materialization"]["partition"] in ("k=a", "k=b")
    grown = nodes(status(project, "parts.py", env={"BARCA_TEST_KEYS": "a,b,c"}))
    part = grown["part"]
    assert part["cache"]["state"] == "partial"
    assert part["partitions"] == {"total": 3, "cached": 2, "missing": 1, "missing_keys": ["k=c"]}
    assert grown["summary"]["cache"]["state"] == "stale"
    assert grown["summary"]["cache"]["reason"] == "upstream_stale"


def test_a_failed_last_attempt_is_reported(project):
    proc = barca(project, "get", "failing.py")
    assert proc.returncode != 0
    boom = nodes(status(project, "failing.py"))["boom"]
    assert boom["cache"]["state"] == "never_run"
    assert boom["cache"]["reason"] == "failed"
    last = boom["last_materialization"]
    assert last["status"] == "failed" and "kaboom" in last["error"]
    assert boom["shape"] is None


def test_human_table(project):
    ok(barca(project, "get", "mid", "pipeline.py"))
    # Piped stdout picks JSON (#153); ask for the table.
    proc = barca(project, "status", "pipeline.py", "--pretty")
    assert proc.returncode == 0, proc.stderr
    header = proc.stdout.splitlines()[0]
    for col in ("NAME", "KIND", "STATE", "LAST RUN", "SHAPE"):
        assert col in header
    assert "src" in proc.stdout and "cached" in proc.stdout and "3 rows" in proc.stdout


def test_the_inspect_helper_degrades_without_pyarrow(project, monkeypatch):
    ok(barca(project, "get", "table", "frames.py"))
    artifact = nodes(status(project, "frames.py"))["table"]["last_materialization"]["artifact"]
    from barca import _inspect

    monkeypatch.setitem(sys.modules, "pyarrow", None)
    monkeypatch.setitem(sys.modules, "pyarrow.parquet", None)
    shape = _inspect.shape(str(project / artifact), "parquet", sample=0)
    assert "rows" not in shape
    assert "pyarrow" in shape["note"]


DYNAMIC = """
from barca import asset, partitions, partitions_from


@asset()
def keys() -> list:
    return ["x", "y"]


@asset(partitions={"k": partitions_from(keys)})
def part(k: str) -> dict:
    return {"k": k}
"""


def test_dynamic_partitions_are_unknown_until_their_source_has_run(project):
    (project / "dynamic.py").write_text(DYNAMIC)
    cold = nodes(status(project, "dynamic.py"))
    assert cold["part"]["cache"]["state"] == "unknown"
    assert cold["part"]["cache"]["reason"] == "partitions_unknown"
    ok(barca(project, "get", "part", "dynamic.py"))
    warm = nodes(status(project, "dynamic.py"))["part"]
    assert warm["cache"]["state"] == "cached"
    assert warm["partitions"]["total"] == 2


# ─── combined with TTY output (#153), bounded output (#155) and declared env (#156) ─────────


def test_status_is_json_when_piped_and_bounded(project):
    ok(barca(project, "get", "mid", "pipeline.py"))
    doc = json.loads(barca(project, "status", "pipeline.py").stdout)  # no flag: piped -> JSON
    assert doc["truncated"] is False and doc["total"] == len(doc["nodes"])
    one = ok(barca(project, "status", "pipeline.py", "--limit", "1"))
    assert len(one["nodes"]) == 1 and one["truncated"] is True and "--all" in one["hint"]
    assert one["total"] == doc["total"]
    assert sum(one["summary"].values()) == doc["total"]  # the summary counts every node


def test_status_fields_trim_each_node(project):
    out = ok(barca(project, "status", "pipeline.py", "--fields", "id,cache"))
    assert all(set(n) == {"id", "cache"} for n in out["nodes"])
    bad = barca(project, "status", "pipeline.py", "--fields", "nope")
    assert bad.returncode == 2


def test_status_lists_declared_env(project):
    (project / "envd.py").write_text(
        "from barca import asset\n\n\n@asset(env=['SOURCE_CSV'])\ndef a() -> int:\n    return 1\n"
    )
    out = ok(barca(project, "status", "envd.py", "--json"))
    assert out["nodes"][0]["env"] == ["SOURCE_CSV"]
