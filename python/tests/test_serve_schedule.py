"""What a cron tick does under `barca serve`, end to end.

A tick brings the node it fires up to date; it does not force it. Sensors upstream are
polled, an asset is recomputed when something on its input side changed and served from
cache when nothing did, and a task itself always runs (#244, #245).
"""

import os
import signal
import socket
import sqlite3
import subprocess
import time
from pathlib import Path

from barca.api import _find_binary

# Settings from the developer's shell that would point the service at a real store.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
import time
from pathlib import Path

from barca import asset, sensor, task, Schedule

@sensor()
def version() -> tuple[bool, str]:
    return True, Path("version.txt").read_text()


@asset(freshness=Schedule("* * * * * *"), inputs={"version": version})
def tracked(version: str) -> dict:
    return {"version": version, "t": time.time()}


@asset(freshness=Schedule("* * * * * *"))
def no_inputs() -> dict:
    return {"t": time.time()}


@asset(inputs={"version": version})
def feed(version: str) -> dict:
    return {"version": version, "t": time.time()}


@asset()
def model() -> dict:
    return {"t": time.time()}


@task(freshness=Schedule("* * * * * *"), inputs={"feed": feed, "_model": model})
def publish(feed: dict, _model) -> None:
    print("publish", feed["version"])
"""


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _rows(root: Path) -> dict[str, int]:
    """Successful materializations per node; empty while the database is not there yet."""
    try:
        db = sqlite3.connect(root / ".barca" / "metadata.db")
        try:
            return {
                node.rsplit(":", 1)[1]: count
                for node, count in db.execute(
                    "select node_id, count(*) from materializations"
                    " where status = 'success' group by node_id"
                )
            }
        finally:
            db.close()
    except sqlite3.Error:
        return {}


def _wait_for(root: Path, done, what: str, timeout: float = 60) -> dict[str, int]:
    deadline = time.monotonic() + timeout
    while True:
        rows = _rows(root)
        if done(rows):
            return rows
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}: {rows}")
        time.sleep(0.25)


def _set_version(root: Path, value: str) -> None:
    """Replace the file in one step, so a sensor poll never reads it half-written."""
    tmp = root / "version.txt.tmp"
    tmp.write_text(value)
    os.replace(tmp, root / "version.txt")


def test_a_tick_recomputes_only_what_changed_on_the_input_side(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    _set_version(tmp_path, "v1")
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(_free_port())],
        cwd=tmp_path,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        # Several ticks on the first value, then several on the second.
        first = _wait_for(
            tmp_path,
            lambda r: r.get("publish", 0) >= 3 and r.get("tracked", 0) >= 1,
            "ticks on v1",
        )
        _set_version(tmp_path, "v2")
        _wait_for(
            tmp_path,
            lambda r: (
                r.get("publish", 0) >= first["publish"] + 3
                and r.get("tracked", 0) >= 2
                and r.get("feed", 0) >= 2
            ),
            "ticks on v2",
        )
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            _, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            _, err = serve.communicate()

    rows = _rows(tmp_path)
    context = f"{rows}\n{err}"
    # The task runs on every tick; its upstream assets only when their inputs changed.
    assert rows.get("publish", 0) >= 6, f"scheduled task did not run per tick: {context}"
    assert rows.get("model") == 1, f"an unchanged upstream of a task was recomputed: {context}"
    assert rows.get("feed") == 2, f"feed should run once per sensor value: {context}"
    assert "publish v1" in err and "publish v2" in err, f"task did not see the new value: {err}"
    # A scheduled asset is recomputed when its sensor changed, and skipped when nothing did.
    assert rows.get("tracked") == 2, f"tracked should run once per sensor value: {context}"
    assert rows.get("no_inputs") == 1, f"an asset with nothing changed was recomputed: {context}"
