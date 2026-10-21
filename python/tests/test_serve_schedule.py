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
    with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
        lines = [row[0] for row in db.execute("SELECT line FROM logs")]
    assert "publish v1" in lines and "publish v2" in lines, (
        f"task did not see the new value: {lines}\n{err}"
    )
    # A scheduled asset is recomputed when its sensor changed, and skipped when nothing did.
    assert rows.get("tracked") == 2, f"tracked should run once per sensor value: {context}"
    assert rows.get("no_inputs") == 1, f"an asset with nothing changed was recomputed: {context}"


# ─── a CLI run on the same project holds a tick back (#479) ──────────────────

CLI_PIPELINE = """
import os
import time
from pathlib import Path

from barca import asset, Schedule


@asset(freshness=Schedule("* * * * * *"))
def ticking() -> dict:
    # Slow only in the run started from the shell, so a tick the server fires ends at once.
    if os.environ.get("TICK_TEST_SLOW"):
        Path("cli.started").write_text("")
        deadline = time.time() + 120
        while not Path("release").exists() and time.time() < deadline:
            time.sleep(0.05)
    return {"t": time.time()}
"""


def _env(**extra: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env.update(extra)
    return env


def _until(predicate, what: str, timeout: float = 60):
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(0.1)


def _running(root: Path) -> list[tuple[str, str]]:
    """The `running` rows of the project's history: (run_id, job). The connection is closed
    before returning: barca refuses a database another program holds open."""
    try:
        db = sqlite3.connect(root / ".barca" / "metadata.db")
        try:
            return list(db.execute("SELECT run_id, job FROM runs WHERE status = 'running'"))
        finally:
            db.close()
    except sqlite3.Error:
        return []


def _schedule(port: int):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/schedule", timeout=5) as resp:
            entries = json.loads(resp.read())
    except OSError:
        return None
    return entries[0] if entries else None


def _start_slow_get(root: Path) -> subprocess.Popen:
    """`barca get ticking` from a shell, in a session of its own, held until `release` exists."""
    cli = subprocess.Popen(
        [_find_binary(), "get", "ticking"],
        cwd=root,
        env=_env(TICK_TEST_SLOW="1"),
        stdout=subprocess.DEVNULL,
        stderr=(root / "cli.log").open("w"),
        start_new_session=True,
    )
    _until(lambda: (root / "cli.started").exists() or cli.poll() is not None, "the CLI run")
    assert cli.poll() is None, (root / "cli.log").read_text()
    return cli


def _serve(root: Path) -> tuple[subprocess.Popen, int]:
    port = _free_port()
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
        cwd=root,
        env=_env(),
        stdout=subprocess.DEVNULL,
        stderr=(root / "serve.log").open("w"),
    )
    return serve, port


def _stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.kill()
    proc.wait(timeout=30)


def test_a_tick_is_skipped_while_a_cli_run_of_the_node_is_going(tmp_path):
    (tmp_path / "pipeline.py").write_text(CLI_PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    cli = _start_slow_get(tmp_path)
    serve = None
    try:
        # The run records its job when it starts, which is how the server sees what it covers.
        [(run_id, job)] = _running(tmp_path)
        assert job.endswith(":ticking"), job
        serve, port = _serve(tmp_path)
        entry = _until(
            lambda: (e := _schedule(port)) and e["last_skipped"] is not None and e,
            "a skipped tick",
        )
        assert entry["skipped_for"] == run_id, entry
        assert entry["skip_reason"] == "in_flight", entry
        assert entry["last_run"] is None, f"the server started a second run: {entry}"
        log = (tmp_path / "serve.log").read_text()
        assert f"run {run_id} of the same node still in flight" in log, log

        # Once the CLI run has ended, the next tick fires.
        (tmp_path / "release").write_text("")
        assert cli.wait(timeout=60) == 0, (tmp_path / "cli.log").read_text()
        _until(lambda: (e := _schedule(port)) and e["last_run"] is not None, "a tick after it")
    finally:
        (tmp_path / "release").write_text("")
        _stop(cli)
        if serve is not None:
            _stop(serve)


def test_a_running_row_whose_cli_run_was_killed_does_not_hold_a_tick_back(tmp_path):
    (tmp_path / "pipeline.py").write_text(CLI_PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    cli = _start_slow_get(tmp_path)
    os.killpg(cli.pid, signal.SIGKILL)
    cli.wait(timeout=30)
    [(stale, _)] = _running(tmp_path)
    serve, port = _serve(tmp_path)
    try:
        entry = _until(
            lambda: (e := _schedule(port)) and e["last_run"] is not None and e,
            "a tick to fire",
        )
        assert entry["last_run"] != stale, entry
        assert entry["skipped_for"] is None, f"a dead run held the tick back: {entry}"
    finally:
        (tmp_path / "release").write_text("")
        _stop(serve)
