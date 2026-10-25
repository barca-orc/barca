"""Incremental work is bounded by affected identities, for ordinary and keyed DAGs."""

import json
import os
import re
import sqlite3
import subprocess
from contextlib import closing

import pytest
from barca.api import _find_binary

SOURCE = """import json
import os
from pathlib import Path
from barca import asset, sensor, partitions_from

@sensor()
def versions():
    return True, json.loads(Path("versions.json").read_text())

@asset(partitions={"customer": partitions_from(versions)})
def cleaned(customer: str, versions):
    return {"value": versions}

@asset(partitions={"customer": partitions_from(cleaned)}, env=["INCREMENTAL_FACTOR"])
def report(customer: str, cleaned):
    if Path("fail").exists():
        raise ValueError("retry this update")
    return {"score": cleaned["value"] * int(os.environ.get("INCREMENTAL_FACTOR", "2"))}
"""


def project(tmp_path, count=3):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(SOURCE)
    versions = {f"c{i}": 1 for i in range(count)}
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    return versions


def run(root, *args, factor="2", expected=0):
    env = os.environ.copy()
    env.update(BARCA_TRACE_TIMING="1", INCREMENTAL_FACTOR=factor)
    for name in ["BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"]:
        env.pop(name, None)
    result = subprocess.run(
        [_find_binary(), *args, "--json"],
        cwd=root,
        check=False,
        env=env,
        capture_output=True,
        text=True,
        timeout=90,
    )
    assert result.returncode == expected, result.stdout + result.stderr
    return json.loads(result.stdout), result.stderr


def get(root, *args, **kwargs):
    return run(root, "get", "report", "pipeline.py", *args, **kwargs)


def identities(stderr):
    match = re.search(r"incremental identities (\d+)", stderr)
    assert match, stderr
    return int(match[1])


def test_one_key_update_does_not_register_fresh_partition_chains(tmp_path):
    versions = project(tmp_path, 2001)
    get(tmp_path)
    versions["c17"] = 2
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    result, trace = get(tmp_path)
    assert result["steps_executed"] == 3
    assert identities(trace) == 3
    summary = result["final_output"]["_barca_partitions"]
    assert (summary["keys"], summary["computed"], summary["cached"]) == (2001, 1, 2000)
    assert list(summary["outputs"]) == ["customer=c17"]
    for step in result["steps"][1:]:
        assert step["partitions"]["cached"] == 2000
        assert step["partitions"]["will_run"] == 1
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        cached = db.execute(
            "SELECT steps_cached FROM runs WHERE run_id = ?", (result["run_id"],)
        ).fetchone()[0]
    assert cached == 4000
    result, trace = get(tmp_path)
    assert result["steps_executed"] == 1
    assert identities(trace) == 1
    assert result["final_output"]["_barca_partitions"]["keys"] == 2001
    result, trace = get(tmp_path)
    assert result["steps_executed"] == 1
    assert identities(trace) == 1  # An unchanged sensor still writes a new generation.


def test_plain_dag_reuses_fresh_branch_and_invalidates_changed_code(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    source = """from barca import asset
@asset()
def left(): return 1
@asset(inputs={"x": left})
def left_result(x): return x + 1
@asset()
def right(): return 10
@asset(inputs={"x": right})
def right_result(x): return x + 1
"""
    pipeline = tmp_path / "pipeline.py"
    pipeline.write_text(source)
    run(tmp_path, "get", "pipeline.py")
    pipeline.write_text(source.replace("return 1\n", "return 2\n"))
    result, trace = run(tmp_path, "get", "pipeline.py")
    assert result["steps_executed"] == 2
    right = [s for s in result["steps"] if s["id"].split(":")[-1].startswith("right")]
    assert all(s["status"] == "cached" for s in right)
    assert identities(trace) <= 3  # Two changed nodes and the returned cached result.
    result, _ = run(tmp_path, "get", "pipeline.py")
    assert result["steps_executed"] == 0


@pytest.mark.parametrize("change", ["code", "env"])
def test_changed_definition_or_environment_invalidates_every_affected_key(tmp_path, change):
    project(tmp_path)
    get(tmp_path)
    if change == "code":
        path = tmp_path / "pipeline.py"
        path.write_text(
            path.read_text().replace('return {"value": versions}', 'return {"value": versions + 1}')
        )
    result, _ = get(tmp_path, factor="3" if change == "env" else "2")
    assert result["steps_executed"] == (4 if change == "env" else 7)
    for output in result["final_output"].values():
        data = json.loads((tmp_path / output["_barca_artifact"]["path"]).read_text())
        assert data["score"] == (3 if change == "env" else 4)


def test_failed_update_does_not_publish_a_freshness_proof(tmp_path):
    versions = project(tmp_path)
    get(tmp_path)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        before = db.execute("SELECT generation FROM freshness_proofs").fetchone()[0]
    versions["c0"] = 2
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    (tmp_path / "fail").touch()
    get(tmp_path, expected=1)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        assert db.execute("SELECT generation FROM freshness_proofs").fetchone()[0] == before
    (tmp_path / "fail").unlink()
    result, _ = get(tmp_path)
    assert result["steps_executed"] == 2  # sensor and failed report; cleaned was persisted.
    result, _ = get(tmp_path)
    assert result["steps_executed"] == 1


def test_needed_missing_cached_artifacts_recover_through_the_same_chain(tmp_path):
    project(tmp_path)
    result, _ = get(tmp_path)
    pointer = result["final_output"]["customer=c0"]["_barca_artifact"]
    (tmp_path / pointer["path"]).unlink()
    # Its input is also missing; recovery must restore keyed arguments and dependencies.
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        path = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id = 'pipeline.py:cleaned[customer=c0]' ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    (tmp_path / path).unlink()
    result, _ = get(tmp_path)
    assert result["steps_executed"] == 3
    path = result["final_output"]["customer=c0"]["_barca_artifact"]["path"]
    assert json.loads((tmp_path / path).read_text()) == {"score": 2}


def test_key_addition_and_removal_keep_counts_and_outputs_current(tmp_path):
    versions = project(tmp_path)
    get(tmp_path)
    versions.pop("c0")
    versions["new"] = 7
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    result, _ = get(tmp_path)
    assert result["steps_executed"] == 3
    assert set(result["final_output"]) == {"customer=c1", "customer=c2", "customer=new"}
    result, _ = get(tmp_path)
    assert result["steps_executed"] == 1


def test_explicit_refresh_invalidates_the_proof_before_a_default_get(tmp_path):
    project(tmp_path)
    get(tmp_path)
    result, _ = get(tmp_path, "--partition", "customer=c0", "--refresh", "cleaned")
    assert result["steps_executed"] == 3
    result, _ = get(tmp_path)
    assert result["steps_executed"] == 1


def test_fan_in_reads_all_inputs_only_when_its_chain_changes(tmp_path):
    versions = project(tmp_path)
    with (tmp_path / "pipeline.py").open("a") as stream:
        stream.write("""
from barca import collect
@asset(inputs={"reports": collect(report)})
def total(reports):
    return sum(value["score"] for value in reports)
""")
    result, _ = run(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result["final_output"] == 6
    result, trace = run(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result["steps_executed"] == 1
    assert identities(trace) == 2  # poll plus returned aggregate; no per-key input state.
    versions["c1"] = 2
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    result, _ = run(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result["steps_executed"] == 4
    assert result["final_output"] == 8
    del versions["c1"]
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    result, _ = run(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result["steps_executed"] == 2  # source and aggregate; surviving keys are fresh.
    assert result["final_output"] == 4


def test_whole_dict_input_does_not_acquire_keyed_source_semantics(tmp_path):
    versions = project(tmp_path)
    with (tmp_path / "pipeline.py").open("a") as stream:
        stream.write("""
from barca import partitions
@asset(inputs={"lookup": versions}, partitions={"customer": partitions(["a", "b"])})
def whole(customer, lookup):
    return sum(lookup.values())
""")
    run(tmp_path, "get", "whole", "pipeline.py")
    versions["c0"] = 2
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    result, _ = run(tmp_path, "get", "whole", "pipeline.py", "-o", "value")
    assert result["steps_executed"] == 3  # whole-map change invalidates both static keys.
    assert set(result["final_output"].values()) == {4}


def test_freshness_proofs_are_scoped_to_requested_nodes(tmp_path):
    project(tmp_path)
    with (tmp_path / "pipeline.py").open("a") as stream:
        stream.write("""
@asset()
def unrelated(): return 99
""")
    run(tmp_path, "get", "pipeline.py")
    # A target with a smaller cone can reuse the already validated node witnesses.
    result, _ = run(tmp_path, "get", "unrelated", "pipeline.py", "-o", "value")
    assert result["steps_executed"] == 0
    assert result["final_output"] == 99


def test_a_complete_cache_hit_keeps_all_multi_target_outcomes_successful(tmp_path):
    project(tmp_path, 1001)
    run(tmp_path, "get", "versions,cleaned,report", "pipeline.py")
    result, trace = run(tmp_path, "get", "versions,cleaned,report", "pipeline.py")
    assert result["steps_executed"] == 1
    assert identities(trace) == 1
    assert all(target["status"] == "success" for target in result["targets"].values())
    assert result["targets"]["report"]["final_output"]["_barca_partitions"]["cached"] == 1001


def test_cached_ancestors_keep_output_references_when_a_descendant_runs(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    path = tmp_path / "pipeline.py"
    source = """from barca import asset
@asset()
def root(): return 10
@asset(inputs={"value": root})
def middle(value): return value + 1
@asset(inputs={"value": middle})
def leaf(value): return value + 2
"""
    path.write_text(source)
    run(tmp_path, "get", "leaf", "pipeline.py", "--refresh-all")
    for extra in (3, 4, 5):
        path.write_text(source.replace("value + 2", f"value + {extra}"))
        result, _ = run(tmp_path, "get", "leaf", "pipeline.py", "-o", "value")
        assert result["steps_executed"] == 1
        assert result["final_output"] == 11 + extra


def test_small_target_returns_every_cached_value_across_lookup_groups(tmp_path):
    project(tmp_path, 501)
    get(tmp_path)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        db.execute(
            "UPDATE materializations SET last_read_at = datetime('now', '-10 minutes') WHERE node_id LIKE 'pipeline.py:report[%'"
        )
        recent = db.execute(
            "SELECT last_read_at FROM materializations WHERE node_id LIKE 'pipeline.py:report[%' LIMIT 1"
        ).fetchone()[0]
        db.commit()
    result, trace = get(tmp_path, "-o", "value")
    assert result["steps_executed"] == 1
    assert identities(trace) == 502
    assert len(result["final_output"]) == 501
    assert all(value == {"score": 2} for value in result["final_output"].values())
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        stamps = db.execute(
            "SELECT DISTINCT last_read_at FROM materializations WHERE node_id LIKE 'pipeline.py:report[%'"
        ).fetchall()
    assert stamps == [(recent,)]  # Recent reads retain the existing one-hour stamp policy.
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        db.execute(
            "UPDATE materializations SET last_read_at = '2000-01-01 00:00:00' WHERE node_id LIKE 'pipeline.py:report[%'"
        )
        db.commit()
    get(tmp_path, "-o", "value")
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        stamps = db.execute(
            "SELECT DISTINCT last_read_at FROM materializations WHERE node_id LIKE 'pipeline.py:report[%'"
        ).fetchall()
    assert all(stamp > recent for (stamp,) in stamps)


def test_large_source_proofs_and_updates_retain_only_compact_observations(tmp_path):
    versions = project(tmp_path, count=1200)
    get(tmp_path)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        proofs = db.execute("SELECT snapshot FROM freshness_proofs").fetchall()
        assert len(proofs) == 3
        assert max(len(row[0]) for row in proofs) < 2500
        source = next(
            json.loads(row[0])["source"] for row in proofs if json.loads(row[0])["source"]
        )
        assert source["count"] == 1200
        assert "keys" not in source
    versions["c1199"] = 2
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    updated, trace = get(tmp_path)
    assert updated["steps_executed"] == 3
    assert identities(trace) == 3
    for i in range(17):
        versions[f"c{i * 53}"] += 1
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    batched, trace = get(tmp_path)
    assert batched["steps_executed"] == 35
    assert identities(trace) == 35


def test_cached_list_source_keeps_whole_list_inputs_during_artifact_recovery(tmp_path):
    source = """from barca import asset, sensor, partitions_from, collect

@sensor()
def keys():
    return ["a", "b"]

@asset(inputs={"keys": keys}, partitions={"k": partitions_from(keys)})
def leaf(k: str, keys):
    return {"k": k, "count": len(keys)}

@asset(inputs={"values": collect(leaf)})
def total(values):
    return sum(v["count"] for v in values) * FACTOR
"""
    pipeline = tmp_path / "pipeline.py"
    pipeline.write_text(source.replace("FACTOR", "1"))
    result, _ = run(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result["final_output"] == 4
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        path = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id LIKE '%:leaf[k=a]' "
            "ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    (tmp_path / path).unlink()
    pipeline.write_text(source.replace("FACTOR", "2"))
    result, _ = run(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result["final_output"] == 8
    assert result["steps_executed"] == 3  # Sensor, one recovered key, and total.


def test_a_source_with_equal_bytes_moves_its_proof_to_the_new_artifact(tmp_path):
    project(tmp_path)
    first, _ = get(tmp_path, "-o", "value")

    def source_path():
        with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
            return db.execute(
                "SELECT artifact_path FROM materializations WHERE node_id = 'pipeline.py:versions' "
                "ORDER BY id DESC LIMIT 1"
            ).fetchone()[0]

    old = source_path()
    pipeline = tmp_path / "pipeline.py"
    pipeline.write_text(
        pipeline.read_text().replace("return True, json.loads", "return bool(1), json.loads")
    )
    second, _ = get(tmp_path, "-o", "value")
    assert source_path() != old
    assert second["final_output"] == first["final_output"]
    (tmp_path / old).unlink()
    third, _ = get(tmp_path, "-o", "value")
    assert third["final_output"] == first["final_output"]
