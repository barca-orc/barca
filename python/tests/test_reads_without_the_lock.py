"""The CLI's read commands read the metadata DB in place, without barca's lock (#1056, A7).

Every database operation of a barca process takes `<db>.lock`. The read commands used to as
well, so `barca status` waited behind every write of a run or a server in the same project
(60 s, then a timeout), and held the lock a run then waited for. They now read through a
read-only view of the WAL database, as the routes of `barca serve` do.

Each test holds the lock itself, as another process in the middle of a long write would, for
the whole command. The reads must finish while it is held.
"""

import fcntl
import json
import sqlite3
import time
from contextlib import contextmanager
from pathlib import Path

from .helpers import barca

PIPELINE = """
from barca import asset, partitions


@asset()
def a() -> dict:
    print("line from a")
    return {"x": 1}


@asset(inputs={"a": a})
def b(a: dict) -> dict:
    return {"x": a["x"] + 1}


@asset(partitions={"k": partitions(["k1", "k2"])})
def keyed(k: str) -> dict:
    return {"key": k}
"""

# Well under the 60 s a command waits for the lock before it gives up.
PROMPT = 20.0


@contextmanager
def lock_held(project: Path):
    """Hold `.barca/metadata.db.lock` exclusively, as a process writing the database does."""
    with open(project / ".barca" / "metadata.db.lock", "w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def read(project: Path, *args: str):
    started = time.monotonic()
    result = barca(project, *args)
    elapsed = time.monotonic() - started
    assert result.returncode == 0, f"barca {' '.join(args)}: {result.stderr}"
    assert elapsed < PROMPT, f"barca {' '.join(args)} took {elapsed:.1f}s with the lock held"
    assert "timed out" not in result.stderr, result.stderr
    return result


def seeded(project: Path) -> str:
    """One finished run of the pipeline; its run id."""
    assert barca(project, "get", "pipeline.py").returncode == 0
    runs = json.loads(read(project, "history", "--json").stdout)["runs"]
    return runs[0]["run_id"]


def test_read_commands_finish_while_another_process_holds_the_lock(project):
    run_id = seeded(project)
    with lock_held(project):
        status = json.loads(read(project, "status", "pipeline.py", "--json").stdout)
        assert {n["name"] for n in status["nodes"]} >= {"a", "b", "keyed"}
        assert all(n["cache"]["state"] == "cached" for n in status["nodes"]), status["nodes"]
        read(project, "status", "pipeline.py", "--rollup", "--json")
        keys = json.loads(
            read(project, "status", "keyed", "pipeline.py", "--keys", "--json").stdout
        )
        assert len(keys["keys"]) == 2, keys
        read(project, "status", "keyed", "pipeline.py", "--key", "k=k1", "--json")
        history = json.loads(read(project, "history", "--json").stdout)
        assert [r["run_id"] for r in history["runs"]] == [run_id]
        read(project, "history", "--activity", "--json")
        detail = json.loads(read(project, "history", run_id, "--json").stdout)
        assert detail["run"]["status"] == "success", detail["run"]
        stats = json.loads(read(project, "stats", "b", "pipeline.py", "--json").stdout)
        assert stats["total_runs"] == 1, stats
        assert "line from a" in read(project, "logs", run_id).stdout
        assert "line from a" in read(project, "logs").stdout
        assert "line from a" in read(project, "logs", run_id, "-f").stdout
        rows = json.loads(
            read(project, "sql", "SELECT run_id FROM barca.runs", "pipeline.py", "--json").stdout
        )
        assert rows["rows"] == [{"run_id": run_id}], rows
        doctor = json.loads(read(project, "doctor", "pipeline.py", "--json").stdout)
        checks = {c["name"]: c for c in doctor["checks"]}
        assert checks["metadata"]["ok"], checks["metadata"]


def test_a_read_command_reads_what_was_committed_and_writes_nothing(project):
    seeded(project)
    db = project / ".barca" / "metadata.db"

    def rows() -> list:
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
            return conn.execute(
                "SELECT (SELECT count(*) FROM runs), (SELECT count(*) FROM materializations),"
                " (SELECT max(last_read_at) FROM materializations)"
            ).fetchall()

    before = rows()
    with lock_held(project):
        read(project, "status", "pipeline.py", "--json")
        read(project, "history", "--json")
        read(project, "stats", "a", "pipeline.py", "--json")
    assert rows() == before


def test_a_read_command_still_creates_and_completes_a_database_under_the_lock(project):
    """What cannot be read in place is opened as before: a missing database is created by
    `barca history`, and one an earlier release wrote (here: an index this release adds is
    gone) is completed, both under the lock; the reads after that are in place."""
    history = json.loads(read(project, "history", "--json").stdout)
    assert history["runs"] == []
    db = project / ".barca" / "metadata.db"
    assert db.exists()
    seeded(project)
    with sqlite3.connect(db) as conn:
        conn.execute("DROP INDEX idx_mat_failed")
    status = json.loads(read(project, "status", "pipeline.py", "--json").stdout)
    assert all(n["cache"]["state"] == "cached" for n in status["nodes"]), status["nodes"]
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
        indexes = {r[0] for r in conn.execute("SELECT name FROM sqlite_schema WHERE type='index'")}
    assert "idx_mat_failed" in indexes
    with lock_held(project):
        read(project, "status", "pipeline.py", "--json")
