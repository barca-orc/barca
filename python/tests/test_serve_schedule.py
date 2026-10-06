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

from .test_remote_inspect import SCRUB, _find_binary

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


@task(freshness=Schedule("* * * * * *"), inputs={"feed": feed, "model": model})
def publish(feed: dict, model: dict) -> None:
    print("publish", feed["version"])
"""


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _rows(root: Path) -> dict[str, int]:
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


def test_a_tick_recomputes_only_what_changed_on_the_input_side(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "version.txt").write_text("v1")
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
        time.sleep(5)
        (tmp_path / "version.txt").write_text("v2")
        time.sleep(5)
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
    assert rows.get("publish", 0) >= 5, f"scheduled task did not run per tick: {context}"
    assert rows.get("model") == 1, f"an unchanged upstream of a task was recomputed: {context}"
    assert rows.get("feed") == 2, f"feed should run once per sensor value: {context}"
    assert "publish v1" in err and "publish v2" in err, f"task did not see the new value: {err}"
    # A scheduled asset is recomputed when its sensor changed, and skipped when nothing did.
    assert rows.get("tracked") == 2, f"tracked should run once per sensor value: {context}"
    assert rows.get("no_inputs") == 1, f"an asset with nothing changed was recomputed: {context}"
