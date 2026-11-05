"""Schedule catch-up after downtime under `barca serve`, end to end (#248).

The decision itself (which jobs run after which downtime) is tested with fixed times in
crates/barca-server/src/scheduler.rs (`catch_up`). This runs the real server: start it, stop
it, backdate the last-fired record so that a tick lies in the downtime, and start it again.

Nothing here waits a fixed time and then looks. Each start is observed through `GET
/schedule`, which the scheduler publishes once its start-up catch-up is decided: `last_run`
is the catch-up run's handle, or null when there was nothing to catch up. The cron fires once
a year, half a year from now, so no live tick can fall inside the test.
"""

import datetime
import sqlite3
from pathlib import Path

from .served import Serve
from .served import rows as _rows

# Midnight on the 1st of the month six months from now: always months away.
MONTH = (datetime.date.today().month + 5) % 12 + 1
CRON = f"0 0 1 {MONTH} *"

PIPELINE = f"""
import time

from barca import asset, Schedule


@asset(freshness=Schedule("{CRON}"))
def yearly() -> dict:
    return {{"t": time.time()}}
"""


def schedule(server: Serve) -> dict:
    """The one job, once the scheduler has published it (after its catch-up)."""
    (job,) = server.schedule().values()
    assert job["id"] == "pipeline.py:yearly" and job["cron"] == CRON, job
    return job


def _last_fired(root: Path) -> dict[str, int]:
    db = sqlite3.connect(root / ".barca" / "metadata.db")
    try:
        return dict(db.execute("select node_id, last_fired_at from schedule_state"))
    finally:
        db.close()


def test_a_missed_tick_runs_once_when_the_server_comes_back(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)

    # First start: no prior record, so the job is anchored to now and nothing runs.
    with Serve(tmp_path) as first:
        job = schedule(first)
        assert job["last_run"] is None, job
        anchored = job["last_fired"]
        assert anchored is not None
    assert "catch-up" not in first.stderr, first.stderr
    assert _last_fired(tmp_path) == {"pipeline.py:yearly": anchored}
    assert _rows(tmp_path).get("yearly", 0) == 0

    # Downtime: the last fire was 400 days ago, so the yearly tick lies in between.
    db = sqlite3.connect(tmp_path / ".barca" / "metadata.db")
    try:
        db.execute("update schedule_state set last_fired_at = ?", (anchored - 400 * 86400,))
        db.commit()
    finally:
        db.close()

    with Serve(tmp_path) as second:
        job = schedule(second)
        handle = job["last_run"]
        assert handle is not None, f"no catch-up run was started: {job}"
        assert job["last_fired"] >= anchored
        run = second.wait(
            "the catch-up run to finish",
            lambda: (r := second.get(f"/status/{handle}"))["status"] not in ("pending", "running")
            and r,
        )
        assert run["status"] == "complete", run
        caught_up = job["last_fired"]
    assert f"catch-up run pipeline.py:yearly → {handle}" in second.stderr, second.stderr
    assert second.stderr.count("catch-up run") == 1, second.stderr
    assert _rows(tmp_path) == {"yearly": 1}
    # The record moved forward with the catch-up.
    assert _last_fired(tmp_path) == {"pipeline.py:yearly": caught_up}

    # A third start has nothing to catch up: the run does not repeat.
    with Serve(tmp_path) as third:
        job = schedule(third)
        assert job["last_run"] is None, job
        assert job["last_fired"] == caught_up
    assert "catch-up" not in third.stderr, third.stderr
    assert _rows(tmp_path) == {"yearly": 1}
