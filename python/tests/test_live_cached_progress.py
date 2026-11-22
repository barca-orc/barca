"""Cached decisions are visible while remaining work is blocked (#942)."""

import json
import signal
import sqlite3
from contextlib import closing

import pytest

from .helpers import barca, clean_env, wait_until

PIPELINE = """
import time
from pathlib import Path
from barca import asset, task

@asset()
def first() -> int:
    return 1

@asset(inputs={"x": first})
def second(x: int) -> int:
    return x + 1

@task(inputs={"x": second})
def hold(x: int) -> int:
    Path("started").touch()
    while not Path("release").exists():
        time.sleep(0.02)
    return x
"""


@pytest.mark.parametrize("kind", ["plain", "static", "dynamic"])
@pytest.mark.parametrize("proofs", [True, False])
def test_live_cached_counts_reach_history_list_and_progress(tmp_path, serve, kind, proofs):
    source, cached, executed = pipeline(kind)
    (tmp_path / "pipeline.py").write_text(source)
    env = clean_env(BARCA_POOL_SIZE="2")
    seed = barca(tmp_path, "get", "second", "pipeline.py", env=env)
    assert seed.returncode == 0, seed.stderr
    if not proofs:
        # Before any server starts: force ordinary per-key decisions across worker chunks.
        with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
            db.execute("DELETE FROM freshness_proofs")
            db.commit()
    server = serve("--no-schedule", env=env)
    run_id = server.post("/runs", {"command": "run", "targets": ["hold"]})["run_id"]
    try:
        wait_until((tmp_path / "started").exists, "remaining work to block")
        detail = server.get(f"/runs/{run_id}")
        assert detail["status"] == "running", detail
        assert detail["steps_cached"] == cached, detail
        listed = next(r for r in server.get("/runs")["runs"] if r["id"] == run_id)
        assert listed["steps_cached"] == cached, listed
        history = barca(tmp_path, "history", run_id, "--json", "--server", server.url, env=env)
        assert history.returncode == 0, history.stderr
        doc = json.loads(history.stdout)
        assert doc["run"]["steps_cached"] == cached, doc
        progress = doc["explanation"]["progress"]
        assert progress["cached"] == cached, progress
        assert progress["done"] == cached + progress["executed"], progress
        assert progress["total"] == cached + executed + 1, progress
        assert "steps planned" in doc["explanation"]["counts"]["planned"]
        assert "nodes planned" not in doc["explanation"]["counts"]["planned"]
        assert f"{cached} cached" in doc["explanation"]["counts"]["outcome"]
    finally:
        (tmp_path / "release").touch()
    done = wait_until(
        lambda: (r := server.get(f"/runs/{run_id}"))["status"] == "success" and r,
        "the run to finish",
    )
    assert done["steps_cached"] == cached, done
    assert done["steps_executed"] == executed + 1, done


def pipeline(kind):
    if kind == "plain":
        return PIPELINE, 2, 0
    # Seven keys spread over worker chunks; two partitioned nodes, then a fan-in task.
    source = PIPELINE.replace(
        "asset, task", "asset, task, sensor, collect, partitions, partitions_from"
    )
    domain = 'partitions(["a", "b", "c", "d", "e", "f", "g"])'
    executed = 0
    if kind == "dynamic":
        source = source.replace(
            "@asset()",
            '@sensor()\ndef keys():\n    return ["a", "b", "c", "d", "e", "f", "g"]\n\n@asset()',
            1,
        )
        domain = "partitions_from(keys)"
        executed = 1
    source = source.replace("@asset()", f'@asset(partitions={{"key": {domain}}})')
    source = source.replace("def first()", "def first(key: str)")
    source = source.replace(
        '@asset(inputs={"x": first})',
        f'@asset(inputs={{"x": first}}, partitions={{"key": {domain}}})',
    )
    source = source.replace("def second(x: int)", "def second(x: int, key: str)")
    source = source.replace('@task(inputs={"x": second})', '@task(inputs={"x": collect(second)})')
    source = source.replace("def hold(x: int) -> int:", "def hold(x):")
    return source, 14, executed


def test_recovery_retracts_cached_hit_before_recomputation(tmp_path, serve):
    source = PIPELINE.replace(
        "    return x + 1",
        '    if Path("repair").exists():\n        Path("repair.started").touch()\n        while not Path("repair.release").exists():\n            time.sleep(0.02)\n    return x + 1',
    )
    (tmp_path / "pipeline.py").write_text(source)
    env = clean_env(BARCA_POOL_SIZE="2")
    seed = barca(tmp_path, "get", "second", "pipeline.py", "--json", env=env)
    assert seed.returncode == 0, seed.stderr
    # Remove the returned artifact without changing code/freshness evidence.
    (tmp_path / json.loads(seed.stdout)["final_output"]["_barca_artifact"]["path"]).unlink()
    (tmp_path / "repair").touch()
    server = serve("--no-schedule", env=env)
    run_id = server.post("/runs", {"command": "run", "targets": ["hold"]})["run_id"]
    try:
        wait_until((tmp_path / "repair.started").exists, "the missing artifact to be recomputed")
        live = server.get(f"/runs/{run_id}")
        assert live["status"] == "running"
        assert live["steps_cached"] == 1, live
        assert live["steps_executed"] == 0, live
        (tmp_path / "repair.release").touch()
        wait_until((tmp_path / "started").exists, "the final task to block")
        assert server.get(f"/runs/{run_id}")["steps_cached"] == 1
    finally:
        (tmp_path / "repair.release").touch()
        (tmp_path / "release").touch()
    done = wait_until(lambda: (r := server.get(f"/runs/{run_id}"))["status"] == "success" and r)
    assert (done["steps_cached"], done["steps_executed"], done["steps_total"]) == (1, 2, 3)


def test_requeued_attempt_does_not_add_previous_cached_count(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("[server]\ndrain_timeout = 1\n")
    env = clean_env(BARCA_POOL_SIZE="2")
    seed = barca(tmp_path, "get", "second", "pipeline.py", env=env)
    assert seed.returncode == 0, seed.stderr
    server = serve("--no-schedule", env=env)
    run_id = server.post("/runs", {"command": "run", "targets": ["hold"]})["run_id"]
    try:
        wait_until((tmp_path / "started").exists, "the first attempt to block")
        assert server.get(f"/runs/{run_id}")["steps_cached"] == 2
        server.send(signal.SIGTERM)
        assert server.exited() == 0, server.stderr
        (tmp_path / "started").unlink()
        restarted = serve("--no-schedule", env=env)
        wait_until((tmp_path / "started").exists, "the next attempt to block")
        live = restarted.get(f"/runs/{run_id}")
        assert (live["steps_cached"], live["steps_total"]) == (2, 3), live
    finally:
        (tmp_path / "release").touch()
    done = wait_until(lambda: (r := restarted.get(f"/runs/{run_id}"))["status"] == "success" and r)
    assert (done["steps_cached"], done["steps_executed"]) == (2, 1), done


def test_unreached_phase_is_not_counted_cached(tmp_path, serve):
    source = PIPELINE.replace("@asset()", "@asset(cutoff=True)", 1).replace(
        "    return 1",
        """    if Path("refresh").exists():
        Path("refresh.started").touch()
        while not Path("refresh.release").exists():
            time.sleep(0.02)
    return 1""",
        1,
    )
    (tmp_path / "pipeline.py").write_text(source)
    env = clean_env(BARCA_POOL_SIZE="2")
    seed = barca(tmp_path, "get", "second", "pipeline.py", env=env)
    assert seed.returncode == 0, seed.stderr
    (tmp_path / "refresh").touch()
    server = serve("--no-schedule", env=env)
    run_id = server.post("/runs", {"command": "run", "targets": ["hold"], "refresh": ["first"]})[
        "run_id"
    ]
    try:
        wait_until((tmp_path / "refresh.started").exists, "the cutoff phase to block")
        before = server.get(f"/runs/{run_id}")
        assert before["steps_cached"] == 0, before
        (tmp_path / "refresh.release").touch()
        wait_until((tmp_path / "started").exists, "the task after cutoff to block")
        after = server.get(f"/runs/{run_id}")
        assert after["steps_cached"] == 1, after
    finally:
        (tmp_path / "refresh.release").touch()
        (tmp_path / "release").touch()
    wait_until(lambda: server.get(f"/runs/{run_id}")["status"] == "success")
    cli = barca(tmp_path, "run", "hold", "--server", server.url, "--agent", env=env)
    assert cli.returncode == 0, cli.stderr
    assert "(3/3)" in cli.stderr, cli.stderr
    assert "3/3 steps" in cli.stderr, cli.stderr
