"""What a cron tick does under `barca serve`, end to end.

A tick brings the node it fires up to date; it does not force it. Sensors upstream are
polled, an asset is recomputed when something on its input side changed and served from
cache when nothing did, and a task itself always runs (#244, #245).
"""

import json
import os
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.request
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


@task(freshness=Schedule("* * * * * *"), inputs={"feed": feed, "model": model})
def publish(feed: dict, model: dict) -> None:
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


SHARED = """
import time
from pathlib import Path

from barca import asset, sensor, task, Schedule

@sensor()
def version() -> tuple[bool, str]:
    return True, Path("version.txt").read_text()


@asset(freshness=Schedule("* * * * * *"), inputs={"version": version})
def tracked(version: str) -> dict:
    return {"version": version, "t": time.time()}


@task(freshness=Schedule("* * * * * *"), inputs={"tracked": tracked})
def report(tracked: dict) -> None:
    print("report", tracked["version"])


@task(freshness=Schedule("* * * * * *"), inputs={"tracked": tracked})
def broken(tracked: dict) -> None:
    raise RuntimeError("boom")
"""


def _get_json(port: int, path: str):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5) as r:
        return json.load(r)


def _a_finished_shared_run(port: int, deadline: float = 60.0) -> tuple[dict, dict]:
    """Poll until `broken`'s latest run is over; return (`/schedule` by name, its `/status`).

    The jobs fire every second, so any single read can land mid-run. This reads until it
    catches the run after it finished and before `broken` fired again: both answers then
    describe the same finished run.
    """
    end = time.time() + deadline
    seen = None
    while time.time() < end:
        schedule = {j["id"].rsplit(":", 1)[1]: j for j in _get_json(port, "/schedule")}
        broken = schedule.get("broken", {})
        if broken.get("last_run") and broken.get("last_status") == "failed":
            status = _get_json(port, f"/status/{broken['last_run']}")
            seen = (schedule, status)
            if status["status"] == "failed" and status["result"]:
                return schedule, status
        time.sleep(0.05)
    raise AssertionError(f"no finished shared run with `broken` in it within {deadline}s: {seen}")


def test_nodes_due_at_one_tick_share_one_run(tmp_path):
    """#253: a scheduled asset that is also upstream of two scheduled tasks on the same cron
    is computed once per sensor value, not once by each node's own run. The task that fails
    does not stop the others; the shared run is `failed`, as `barca get a,b` is when one target
    fails, and `/status`, `/schedule` and `barca history` agree on which node failed."""
    (tmp_path / "pipeline.py").write_text(SHARED)
    _set_version(tmp_path, "v1")
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    port = _free_port()
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
        cwd=tmp_path,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    schedule, status = {}, {}
    try:
        _wait_for(tmp_path, lambda r: r.get("report", 0) >= 3, "ticks on v1")
        _set_version(tmp_path, "v2")
        _wait_for(
            tmp_path,
            lambda r: r.get("tracked", 0) >= 2 and r.get("report", 0) >= 6,
            "ticks on v2",
        )
        schedule, status = _a_finished_shared_run(port)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            _, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            _, err = serve.communicate()

    rows = _rows(tmp_path)
    context = f"{rows}\n{err}"
    assert rows.get("tracked") == 2, f"shared upstream computed more than once per value: {context}"
    assert rows.get("report", 0) >= 6, f"the task did not keep running: {context}"
    # `broken` fails every tick without stopping the others.
    assert "broken" not in rows
    assert set(schedule) == {"tracked", "report", "broken"}, schedule
    for name, job in schedule.items():
        assert job["last_run"] and job["last_fired"], (name, job)

    # The run `broken` was last in: failed, naming it, with every node's own outcome.
    targets = status["result"]["targets"]
    assert len(targets) > 1, f"`broken` fired alone, not in a shared run: {status}"
    assert status["status"] == "failed", status
    assert status["error"].endswith("targets failed: pipeline.py:broken"), status
    assert "final_output" not in status["result"], status
    assert targets["pipeline.py:broken"]["status"] == "failed", targets
    assert targets["pipeline.py:broken"]["failed_node"] == "pipeline.py:broken", targets
    assert "RuntimeError: boom" in targets["pipeline.py:broken"]["error"], targets
    others = {name: t["status"] for name, t in targets.items() if name != "pipeline.py:broken"}
    assert set(others.values()) == {"success"}, f"a failure stopped another node: {targets}"

    # `barca history` has that run as one failed row: its targets comma-separated, under
    # `serve` when it held both an asset and a task.
    db = sqlite3.connect(tmp_path / ".barca" / "metadata.db")
    try:
        command, target, run_status = db.execute(
            "select command, target, status from runs where run_id = ?",
            (status["result"]["run_id"],),
        ).fetchone()
    finally:
        db.close()
    assert run_status == "failed", (command, target, run_status)
    assert target == ",".join(targets), (target, list(targets))
    # `tracked` is the asset; a run it was left out of (still going in an earlier one) has
    # tasks only, which is what `barca run a,b` records.
    assert command == ("serve" if "pipeline.py:tracked" in targets else "run"), (command, target)
