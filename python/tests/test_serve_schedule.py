"""What a cron tick does under `barca serve`, end to end.

A tick recomputes the scheduled node and reuses cached upstreams: a scheduled asset is
refreshed on every tick together with the Always assets downstream of it (#244), and a
scheduled task runs without recomputing the assets it reads (#245).
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

from barca import asset, sensor, task, Manual, Schedule


@asset()
def base() -> dict:
    return {"t": time.time()}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def stamp(base: dict) -> dict:
    return {"t": time.time()}


@asset(inputs={"stamp": stamp})
def after(stamp: dict) -> dict:
    return stamp


@asset(freshness=Manual, inputs={"stamp": stamp})
def frozen(stamp: dict) -> dict:
    return stamp


@asset()
def model() -> dict:
    return {"t": time.time()}


@task(freshness=Schedule("* * * * * *"), inputs={"model": model})
def use(model: dict) -> None:
    print("tick")


@sensor(freshness=Schedule("* * * * * *"))
def probe() -> dict:
    return {"t": time.time()}
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


def test_a_tick_recomputes_the_scheduled_node_and_reuses_cached_upstreams(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
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
        time.sleep(8)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            _, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            _, err = serve.communicate()

    rows = _rows(tmp_path)
    assert rows.get("stamp", 0) >= 3, f"scheduled asset was not recomputed per tick: {rows}\n{err}"
    assert rows.get("use", 0) >= 3, f"scheduled task did not run per tick: {rows}\n{err}"
    assert rows.get("probe", 0) >= 3, f"scheduled sensor was not polled per tick: {rows}\n{err}"
    assert rows.get("after", 0) >= 3, (
        f"an Always asset downstream kept a stale result: {rows}\n{err}"
    )
    assert "frozen" not in rows, f"a Manual asset downstream was refreshed by a tick: {rows}\n{err}"
    assert rows.get("base") == 1, f"a scheduled asset's upstream was recomputed: {rows}\n{err}"
    assert rows.get("model") == 1, f"a scheduled task's upstream was recomputed: {rows}\n{err}"
