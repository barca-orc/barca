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
import json
import os
import signal
import sqlite3
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

from barca.api import _find_binary

from .test_serve_schedule import SCRUB, _free_port, _rows

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

DEADLINE = 60


class Server:
    """`barca serve` on its own port, stopped (and its stderr kept) on exit."""

    def __init__(self, root: Path):
        self.root = root
        self.port = _free_port()
        self.stderr = ""

    def __enter__(self) -> "Server":
        env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--port", str(self.port)],
            cwd=self.root,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        return self

    def __exit__(self, *exc) -> None:
        self.proc.send_signal(signal.SIGTERM)
        try:
            _, self.stderr = self.proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            _, self.stderr = self.proc.communicate()

    def get(self, path: str):
        with urllib.request.urlopen(f"http://127.0.0.1:{self.port}{path}", timeout=5) as resp:
            return json.loads(resp.read())

    def poll(self, path: str, done, what: str):
        """GET `path` until `done(body)`; fail if the server dies or the deadline passes."""
        deadline = time.monotonic() + DEADLINE
        last = None
        while time.monotonic() < deadline:
            assert self.proc.poll() is None, f"the server exited while waiting for {what}"
            try:
                last = self.get(path)
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                last = None  # not listening yet
            if last is not None and done(last):
                return last
            time.sleep(0.05)
        raise AssertionError(f"timed out waiting for {what}; last answer: {last}")

    def schedule(self) -> dict:
        """The one job, once the scheduler has published it (after its catch-up)."""
        (job,) = self.poll("/schedule", lambda jobs: len(jobs) == 1, "the published schedule")
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
    with Server(tmp_path) as first:
        job = first.schedule()
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

    with Server(tmp_path) as second:
        job = second.schedule()
        handle = job["last_run"]
        assert handle is not None, f"no catch-up run was started: {job}"
        assert job["last_fired"] >= anchored
        run = second.poll(
            f"/status/{handle}",
            lambda r: r["status"] not in ("pending", "running"),
            "the catch-up run to finish",
        )
        assert run["status"] == "complete", run
        caught_up = job["last_fired"]
    assert f"catch-up run pipeline.py:yearly → {handle}" in second.stderr, second.stderr
    assert second.stderr.count("catch-up run") == 1, second.stderr
    assert _rows(tmp_path) == {"yearly": 1}
    # The record moved forward with the catch-up.
    assert _last_fired(tmp_path) == {"pipeline.py:yearly": caught_up}

    # A third start has nothing to catch up: the run does not repeat.
    with Server(tmp_path) as third:
        job = third.schedule()
        assert job["last_run"] is None, job
        assert job["last_fired"] == caught_up
    assert "catch-up" not in third.stderr, third.stderr
    assert _rows(tmp_path) == {"yearly": 1}
