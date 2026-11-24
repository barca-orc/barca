"""Compact inspection agrees with full membership prediction and rejects invalid evidence."""

import json
import sqlite3
from contextlib import closing

from .test_incremental_dag import get, project, run
from .helpers import clean_env


def status(root):
    result, trace = run(root, "status", "pipeline.py")
    return {node["name"]: node for node in result["nodes"]}, trace


def test_compact_inspection_revalidates_changes(tmp_path):
    versions = project(tmp_path, 17)
    get(tmp_path)
    get(tmp_path)
    nodes, trace = status(tmp_path)
    assert "dry run: phase" not in trace, trace
    assert nodes["report"]["partitions"]["cached"] == 17
    assert nodes["report"]["cache"]["state"] == "cached"
    assert "assumes sensor" in nodes["cleaned"]["cache"]["detail"]

    # A new observation with the same keyed values retains compact evidence.
    get(tmp_path)
    nodes, trace = status(tmp_path)
    assert "dry run: phase" not in trace, trace
    assert nodes["report"]["partitions"]["cached"] == 17

    # Current membership is not all historical materializations.
    del versions["c3"]
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    get(tmp_path)
    nodes, _ = status(tmp_path)
    assert nodes["report"]["partitions"]["total"] == 16

    # A code edit cannot retain a proof under the former definition.
    source = tmp_path / "pipeline.py"
    source.write_text(source.read_text().replace('"score":', '"changed_score":'))
    nodes, trace = status(tmp_path)
    assert "dry run: phase" in trace
    assert nodes["report"]["cache"]["state"] == "stale"
    get(tmp_path)
    # A failed new attempt invalidates the successful generation.
    (tmp_path / "fail").touch()
    versions["c4"] = 5
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    get(tmp_path, expected=1)
    nodes, trace = status(tmp_path)
    assert "dry run: phase" in trace
    assert nodes["report"]["partitions"]["failed"] == 1


def test_compact_inspection_checks_selected_missing_artifact(tmp_path):
    project(tmp_path, 7)
    get(tmp_path)
    get(tmp_path)
    nodes, trace = status(tmp_path)
    assert "dry run: phase" not in trace, trace
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        artifact = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id LIKE '%:report[%' "
            "AND status = 'success' ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    (tmp_path / artifact).unlink()
    nodes, trace = status(tmp_path)
    assert "dry run: phase" in trace
    assert nodes["report"]["cache"]["state"] != "cached"


def test_compact_and_full_predictions_agree(tmp_path):
    project(tmp_path, 11)
    get(tmp_path)
    get(tmp_path)
    compact, _ = status(tmp_path)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        db.execute("DELETE FROM freshness_proofs")
        db.commit()
    expanded, _ = status(tmp_path)
    assert compact == expanded


def test_historical_selection_is_checked_not_newest_materialization(tmp_path):
    versions = project(tmp_path, 5)
    get(tmp_path)
    versions["c1"] = 2
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    get(tmp_path)
    versions["c1"] = 1
    (tmp_path / "versions.json").write_text(json.dumps(versions))
    get(tmp_path)
    nodes, trace = status(tmp_path)
    assert "dry run: phase" not in trace, trace
    assert nodes["report"]["partitions"]["cached"] == 5
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        selected = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id = 'pipeline.py:report[customer=c1]' ORDER BY id LIMIT 1"
        ).fetchone()[0]
    (tmp_path / selected).unlink()
    nodes, _ = status(tmp_path)
    assert nodes["report"]["cache"]["state"] != "cached"


def test_served_availability_expires_without_database_changes(tmp_path, serve):
    import time

    project(tmp_path, 5)
    get(tmp_path)
    get(tmp_path)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        artifact = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id LIKE '%:report[%' "
            "AND status = 'success' ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    server = serve("--no-schedule", env=clean_env(INCREMENTAL_FACTOR="2", BARCA_POOL_SIZE="2"))
    rows = server.get("/state")
    assert next(n for n in rows if n["id"].endswith(":report"))["cache"]["state"] == "cached"
    (tmp_path / artifact).unlink()
    deadline = time.monotonic() + 32
    while True:
        rows = server.get("/state")
        node = next(n for n in rows if n["id"].endswith(":report"))
        if node["cache"]["state"] != "cached":
            break
        assert time.monotonic() < deadline, node
        time.sleep(0.2)
    assert server.get("/health")


def test_unchanged_served_run_reuses_availability_evidence(tmp_path, serve):
    project(tmp_path, 7)
    get(tmp_path)
    get(tmp_path)
    server = serve(
        "--no-schedule",
        env=clean_env(INCREMENTAL_FACTOR="2", BARCA_POOL_SIZE="2", BARCA_TRACE_TIMING="1"),
    )
    server.get("/state")
    before = server.stderr.count("inspection: selected artifacts")
    assert before == 2, server.stderr
    get(tmp_path)
    server.get("/state")
    server.get("/state")
    assert server.stderr.count("inspection: selected artifacts") == before, server.stderr


def test_gc_eviction_invalidates_compact_availability(tmp_path):
    project(tmp_path, 5)
    get(tmp_path)
    get(tmp_path)
    nodes, trace = status(tmp_path)
    assert "dry run: phase" not in trace
    report, _ = run(tmp_path, "gc", "--max-size", "1")
    assert report["evicted"], report
    nodes, trace = status(tmp_path)
    assert "dry run: phase" in trace
    assert nodes["report"]["cache"]["state"] != "cached"
    get(tmp_path)
    nodes, _ = status(tmp_path)
    assert nodes["report"]["cache"]["state"] == "cached"


def test_late_proof_cannot_revive_inspection_after_interrupted_refresh(tmp_path):
    import os
    import signal
    import subprocess

    from barca.api import _find_binary
    from .helpers import wait_until

    project(tmp_path, 7)
    source = tmp_path / "pipeline.py"
    source.write_text(
        source.read_text().replace(
            "def versions():\n",
            "def versions():\n"
            '    if Path("hold").exists():\n'
            "        import time\n"
            '        Path("started").touch()\n'
            '        while Path("hold").exists():\n'
            "            time.sleep(0.01)\n",
        )
    )
    get(tmp_path)
    get(tmp_path)
    _, trace = status(tmp_path)
    assert "dry run: phase" not in trace
    database = tmp_path / ".barca/metadata.db"
    with closing(sqlite3.connect(database)) as db:
        old_proofs = db.execute("SELECT id, generation, snapshot FROM freshness_proofs").fetchall()
        generation = db.execute("SELECT max(id) FROM materializations").fetchone()[0]
    assert old_proofs
    (tmp_path / "hold").touch()
    proc = subprocess.Popen(
        [_find_binary(), "get", "report", "pipeline.py", "--local", "--refresh-all", "--json"],
        cwd=tmp_path,
        env=clean_env(INCREMENTAL_FACTOR="2", BARCA_POOL_SIZE="2"),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        wait_until((tmp_path / "started").exists, "refresh sensor to start", proc=proc)
    finally:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)
        (tmp_path / "hold").unlink()
    with closing(sqlite3.connect(database)) as db:
        assert db.execute("SELECT max(id) FROM materializations").fetchone()[0] == generation
        # Simulate a publisher that prepared evidence before refresh and commits late.
        db.executemany(
            "INSERT OR REPLACE INTO freshness_proofs (id, generation, snapshot) VALUES (?, ?, ?)",
            old_proofs,
        )
        db.commit()
    nodes, trace = status(tmp_path)
    assert "dry run: phase" in trace, trace
    assert nodes["report"]["partitions"]["cached"] < 7, nodes["report"]
    get(tmp_path)
    nodes, _ = status(tmp_path)
    assert nodes["report"]["partitions"]["cached"] == 7
