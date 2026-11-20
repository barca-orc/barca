"""Several barca processes on one project at once: runs with `--local`, readers, and a
`barca serve` with runs of its own (#1056, A7 and A8).

Each process keeps a connection to the metadata database for its lifetime and takes the
cross-process lock for each operation. This checks what that must never cost: no
`database is locked`, no lock timeout, no `malformed` database, and the same history as the
same commands leave when they run one after another.
"""

import json
import sqlite3
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .helpers import barca, wait_until

PIPELINE = """
from barca import asset, partitions


@asset()
def base() -> dict:
    return {"n": 1}


@asset(inputs={"base": base})
def total(base: dict) -> dict:
    return {"n": base["n"] + 1}


@asset(partitions={"k": partitions([str(i) for i in range(40)])})
def keyed(k: str) -> dict:
    return {"k": k}


@asset(inputs={"total": total})
def last(total: dict) -> dict:
    return {"n": total["n"] + 1}
"""

WRITERS = 4
ROUNDS = 3
NEVER = ("database is locked", "timed out", "malformed", "disk image", "locked by another")


def history(project: Path) -> dict:
    """What the runs left, without what differs from one execution to the next (ids, times)."""
    db = project / ".barca" / "metadata.db"
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as conn:
        assert conn.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        return {
            "runs": sorted(
                conn.execute(
                    "SELECT command, status, steps_total, steps_executed, steps_cached FROM runs"
                ).fetchall()
            ),
            "results": sorted(
                conn.execute(
                    "SELECT node_id, run_hash, status, count(*) FROM materializations "
                    "GROUP BY node_id, run_hash, status"
                ).fetchall()
            ),
            "rows_without_a_run": conn.execute(
                "SELECT count(*) FROM materializations WHERE run_id NOT IN (SELECT run_id FROM runs)"
            ).fetchone()[0],
            "unfinished": conn.execute(
                "SELECT count(*) FROM runs WHERE status IN ('running', 'pending')"
            ).fetchone()[0],
        }


def refresh(project: Path):
    return barca(project, "get", "pipeline.py", "--local", "--refresh-all", "--json")


def test_local_runs_and_readers_beside_a_server_leave_the_history_of_a_serial_run(tmp_path, serve):
    serial = tmp_path / "serial"
    busy = tmp_path / "busy"
    for project in (serial, busy):
        project.mkdir()
        (project / "pipeline.py").write_text(PIPELINE)

    # One after another, no server: what the same commands leave.
    for _ in range(WRITERS * ROUNDS):
        done = refresh(serial)
        assert done.returncode == 0, done.stderr
    expected = history(serial)
    assert expected["unfinished"] == 0 and expected["rows_without_a_run"] == 0, expected

    server = serve("--no-schedule", root=busy)
    stop = threading.Event()
    read_failures: list[str] = []
    reads = [0]

    def reader(n: int) -> None:
        commands = (
            ("status", "pipeline.py", "--local", "--json"),
            ("history", "--local", "--json"),
            ("stats", "last", "pipeline.py", "--local", "--json"),
            ("status", "keyed", "pipeline.py", "--keys", "--local", "--json"),
        )
        i = n
        while not stop.is_set():
            args = commands[i % len(commands)]
            i += 1
            done = barca(busy, *args)
            reads[0] += 1
            if done.returncode != 0 or any(word in done.stderr for word in NEVER):
                read_failures.append(f"barca {' '.join(args)}: {done.stderr.strip()[-400:]}")
            elif not done.stdout.strip() or not isinstance(json.loads(done.stdout), dict):
                read_failures.append(f"barca {' '.join(args)}: no JSON")

    readers = [threading.Thread(target=reader, args=(n,)) for n in range(3)]
    for thread in readers:
        thread.start()
    try:
        with ThreadPoolExecutor(WRITERS) as pool:
            results = list(pool.map(lambda _: refresh(busy), range(WRITERS * ROUNDS)))
    finally:
        stop.set()
        for thread in readers:
            thread.join()
    failed = [r.stderr.strip()[-400:] for r in results if r.returncode != 0]
    assert not failed, f"{len(failed)}/{len(results)} runs failed: {failed[:2]}"
    noisy = [r.stderr for r in results if any(word in r.stderr for word in NEVER)]
    assert not noisy, noisy[:2]
    assert not read_failures, f"{len(read_failures)}/{reads[0]} reads failed: {read_failures[:2]}"
    assert reads[0] >= len(readers), "the readers read"

    # The server is alive and reads the same database: its own view of the runs.
    listed = json.loads(barca(busy, "history", "--json", "--all").stdout)
    assert len(listed["runs"]) == WRITERS * ROUNDS, len(listed["runs"])
    errors = server.err_path.read_text()
    assert not any(word in errors for word in NEVER), errors[-800:]

    server.stop()
    wait_until(lambda: not (busy / ".barca" / "serve.sock").exists(), timeout=30)
    assert history(busy) == expected
