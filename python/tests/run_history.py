"""A recorded run history, written straight into a project's metadata database.

A server that ticks every minute records about 1,440 runs a day; real runs cannot make that
many in a test. `seed` writes the rows a finished run leaves in `runs` (and a failed step in
`materializations`), with the start times a test asks for, so the run list, its filters and
the activity of `GET /runs` can be checked over a history shaped like a production one: mostly
schedule ticks, a few runs someone started, some of them failed, and rows from before barca
recorded what started a run.

Only while no barca process has the database open (`served.py` says why): make the database
with one real `barca get`, seed it, then start the server.

As a script it builds a project to look at the Runs page with (#969):

    python -m tests.run_history DIR [--days 3]     # from python/, with barca on PATH
    cd DIR && barca serve --no-schedule
"""

from __future__ import annotations

import argparse
import sqlite3
import subprocess
import sys
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path

PIPELINE = '''
import time
from pathlib import Path

from barca import asset


@asset()
def poll_inbox() -> int:
    return 1


@asset(inputs={"poll_inbox": poll_inbox})
def orders(poll_inbox: int) -> dict:
    return {"orders": poll_inbox}


@asset(inputs={"orders": orders})
def daily_report(orders: dict) -> dict:
    return orders


@asset(inputs={"orders": orders})
def backfill(orders: dict) -> dict:
    """Runs until `release` exists: a run in progress for as long as a test needs one."""
    Path("backfill.started").write_text("")
    deadline = time.time() + 3600
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return orders
'''


@dataclass
class Run:
    """One finished run as its row records it. `started` is UTC."""

    run_id: str
    started: datetime
    status: str = "success"
    triggered_by: str | None = "schedule"
    target: str = "poll_inbox"
    seconds: float = 1.0
    #: `(node id, error type, message)` of its first failed step.
    failure: tuple[str, str, str] | None = None
    #: Written the way a client's posted run is (`2026-10-22T08:00:00Z`), not SQLite's.
    rfc3339: bool = False


def stamp(at: datetime, rfc3339: bool = False) -> str:
    return at.strftime("%Y-%m-%dT%H:%M:%SZ" if rfc3339 else "%Y-%m-%d %H:%M:%S")


def seed(db: Path, runs: list[Run], file: str = "pipeline.py") -> None:
    """Insert `runs` into the database at `db`, in the order given (a later one is newer)."""
    conn = sqlite3.connect(db)
    try:
        for run in runs:
            node = f"{file}:{run.target}"
            scheduled = run.triggered_by == "schedule"
            conn.execute(
                "INSERT INTO runs (run_id, command, files, target, job, status, steps_total, "
                "steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, "
                "triggered_by, scheduled_at, schedule_id) "
                "VALUES (?, 'get', ?, ?, ?, ?, 1, 1, 0, ?, ?, ?, ?, ?, ?)",
                (
                    run.run_id,
                    f'["{file}"]',
                    run.target,
                    node,
                    run.status,
                    stamp(run.started, run.rfc3339),
                    stamp(run.started + timedelta(seconds=run.seconds), run.rfc3339),
                    run.seconds,
                    run.triggered_by,
                    stamp(run.started, True) if scheduled else None,
                    node if scheduled else None,
                ),
            )
            if run.failure:
                step, error_type, message = run.failure
                conn.execute(
                    "INSERT INTO materializations (node_id, run_id, status, error_type, "
                    "error_message, created_at) VALUES (?, ?, 'failed', ?, ?, ?)",
                    (step, run.run_id, error_type, message, stamp(run.started)),
                )
        conn.commit()
    finally:
        conn.close()


def production_like(now: datetime, days: int = 3) -> list[Run]:
    """`days` of every-minute ticks ending at `now`, and the runs people started among them.

    Per day: 1,440 ticks (one in 400 fails, on a timeout), nine runs from the API (one in
    three fails on a missing column), three typed in a shell (one cancelled) and one from
    before barca recorded what started a run. Oldest first.
    """
    runs: list[Run] = []
    start = now.replace(second=0, microsecond=0) - timedelta(days=days)
    for minute in range(days * 1440):
        at = start + timedelta(minutes=minute)
        failed = minute % 400 == 399
        runs.append(
            Run(
                f"tick{minute:06d}",
                at,
                status="failed" if failed else "success",
                seconds=0.4 + (minute % 17) / 10,
                failure=("pipeline.py:poll_inbox", "TimeoutError", "inbox timed out")
                if failed
                else None,
            )
        )
        if minute % 160 == 37:
            n = minute // 160
            bad = n % 3 == 2
            runs.append(
                Run(
                    f"api{n:05d}",
                    at + timedelta(seconds=20),
                    status="failed" if bad else "success",
                    triggered_by="api",
                    target="daily_report",
                    seconds=38.0 + n % 9,
                    failure=("pipeline.py:orders", "KeyError", "'region'") if bad else None,
                )
            )
        if minute % 480 == 211:
            n = minute // 480
            runs.append(
                Run(
                    f"manual{n:04d}",
                    at + timedelta(seconds=40),
                    status="cancelled" if n % 3 == 1 else "success",
                    triggered_by="manual",
                    target="orders",
                    seconds=12.5,
                )
            )
        if minute % 1440 == 600:
            runs.append(
                Run(
                    f"legacy{minute // 1440:03d}",
                    at + timedelta(seconds=50),
                    triggered_by=None,
                    target="orders",
                    seconds=7.0,
                    rfc3339=True,
                )
            )
    return runs


def project(root: Path, barca: str = "barca") -> Path:
    """A project in `root` with one real run recorded; the path of its metadata database."""
    root.mkdir(parents=True, exist_ok=True)
    (root / "pipeline.py").write_text(PIPELINE)
    (root / "barca.toml").write_text("")
    subprocess.run(
        [barca, "get", "daily_report", "pipeline.py"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return root / ".barca" / "metadata.db"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("root", type=Path)
    parser.add_argument("--days", type=int, default=3)
    parser.add_argument("--barca", default="barca")
    args = parser.parse_args()
    db = project(args.root, args.barca)
    runs = production_like(datetime.now(UTC), args.days)
    seed(db, runs)
    sys.stdout.write(f"{len(runs)} runs seeded in {db}\n")


if __name__ == "__main__":
    main()
