"""Schedule catch-up after downtime under `barca serve` (#248).

`needs_catchup` has unit tests in crates/barca-server/src/scheduler.rs; this runs the server,
stops it, backdates the last-fired record to look like days of downtime and starts it again.
The cron fires once a year, so every run seen here is the catch-up and not a live tick.
"""

import os
import signal
import sqlite3
import subprocess
import time
from pathlib import Path

from barca.api import _find_binary

from .test_serve_schedule import SCRUB, _free_port, _rows, _wait_for

PIPELINE = """
import time

from barca import asset, Schedule


@asset(freshness=Schedule("0 0 1 1 *"))
def yearly() -> dict:
    return {"t": time.time()}
"""


def _serve(root: Path) -> subprocess.Popen:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(_free_port())],
        cwd=root,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _stop(proc: subprocess.Popen) -> str:
    proc.send_signal(signal.SIGTERM)
    try:
        _, err = proc.communicate(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        _, err = proc.communicate()
    return err


def _last_fired(root: Path) -> dict[str, int]:
    db = sqlite3.connect(root / ".barca" / "metadata.db")
    try:
        return dict(db.execute("select node_id, last_fired_at from schedule_state"))
    finally:
        db.close()


def _wait_for_last_fired(root: Path, timeout: float = 30) -> dict[str, int]:
    deadline = time.monotonic() + timeout
    while True:
        try:
            state = _last_fired(root)
        except sqlite3.Error:
            state = {}
        if state:
            return state
        if time.monotonic() > deadline:
            raise AssertionError("the first start never recorded a last-fired time")
        time.sleep(0.25)


def test_a_missed_tick_runs_once_when_the_server_comes_back(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)

    # First start: no prior record, so the job is anchored to now and nothing runs.
    first = _serve(tmp_path)
    try:
        _wait_for_last_fired(tmp_path)
        time.sleep(1.5)
    finally:
        err = _stop(first)
    assert "catch-up" not in err, err
    assert _rows(tmp_path).get("yearly", 0) == 0, err

    # Downtime: the last fire was over a year ago, so a tick has elapsed since.
    db = sqlite3.connect(tmp_path / ".barca" / "metadata.db")
    try:
        db.execute("update schedule_state set last_fired_at = ?", (int(time.time()) - 400 * 86400,))
        db.commit()
    finally:
        db.close()

    second = _serve(tmp_path)
    try:
        rows = _wait_for(tmp_path, lambda r: r.get("yearly", 0) >= 1, "the catch-up run")
        time.sleep(1.5)  # a live tick would not come, and the catch-up must not repeat
    finally:
        err = _stop(second)
    assert "catch-up run" in err, err
    assert rows["yearly"] == 1
    assert _rows(tmp_path)["yearly"] == 1, err

    # The catch-up moved the record forward: a third start has nothing to catch up.
    assert _last_fired(tmp_path)  # still there
    third = _serve(tmp_path)
    try:
        time.sleep(2.5)
    finally:
        err = _stop(third)
    assert "catch-up" not in err, err
    assert _rows(tmp_path)["yearly"] == 1
