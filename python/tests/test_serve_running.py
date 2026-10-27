"""`barca serve` shows a `barca get` from a shell on the same project as running (#534).

A CLI run streams nothing to the server; its `running` row in `.barca/metadata.db`, with a live
owner, is what `GET /state` and `GET /runs?status=running` report. A killed CLI run is not shown.
"""

import json
import os
import signal
import socket
import subprocess
import time
import urllib.request
from pathlib import Path

from barca.api import _find_binary

# Settings from the developer's shell that would point the service at a real store.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
import os
import time
from pathlib import Path

from barca import asset


@asset()
def upstream() -> int:
    return 1


@asset(inputs={"upstream": upstream})
def slow(upstream: int) -> dict:
    Path("cli.started").write_text("")
    deadline = time.time() + 120
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return {"n": upstream}
"""


def _env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _until(predicate, what: str, timeout: float = 60):
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(0.1)


def _get(port: int, path: str):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5) as resp:
            return json.loads(resp.read())
    except OSError:
        return None


def _running_of(port: int, name: str):
    nodes = _get(port, "/state")
    if nodes is None:
        return None
    return next(n for n in nodes if n["name"] == name)["running"]


def _start(root: Path, *args: str, log: str) -> subprocess.Popen:
    return subprocess.Popen(
        [_find_binary(), *args],
        cwd=root,
        env=_env(),
        stdout=subprocess.DEVNULL,
        stderr=(root / log).open("w"),
        start_new_session=True,
    )


def _stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGKILL)
    proc.wait(timeout=30)


def _project(root: Path) -> int:
    (root / "pipeline.py").write_text(PIPELINE)
    (root / "barca.toml").write_text("")
    return _free_port()


def test_a_cli_run_on_the_same_project_shows_as_running_until_it_ends(tmp_path):
    port = _project(tmp_path)
    serve = _start(
        tmp_path, "serve", "pipeline.py", "--no-schedule", "--port", str(port), log="serve.log"
    )
    cli = None
    try:
        _until(lambda: _get(port, "/health"), "the server")
        cli = _start(tmp_path, "get", "slow", log="cli.log")
        _until(lambda: (tmp_path / "cli.started").exists() or cli.poll() is not None, "the CLI run")
        assert cli.poll() is None, (tmp_path / "cli.log").read_text()

        # Within one poll: the CLI run's target is running; what it already finished is not.
        running = _until(lambda: _running_of(port, "slow"), "slow to show as running", timeout=10)
        assert running["triggered_by"] == "manual", running
        assert running["step"] == "running", running
        assert running["partitions"] is None, running
        assert _running_of(port, "upstream") is None
        detail = _get(port, "/assets/slow")
        assert detail["running"]["run_id"] == running["run_id"], detail
        [run] = _get(port, "/runs?status=running")["runs"]
        assert run["id"] == running["run_id"], run
        assert run["status"] == "running", run
        assert [s["node_id"].split(":")[-1] for s in run["active_steps"]] == ["slow"], run

        (tmp_path / "release").write_text("")
        assert cli.wait(timeout=60) == 0, (tmp_path / "cli.log").read_text()
        _until(
            lambda: _get(port, "/state") and _running_of(port, "slow") is None,
            "slow to clear",
            timeout=10,
        )
        assert _get(port, "/runs?status=running")["runs"] == []
    finally:
        (tmp_path / "release").write_text("")
        if cli is not None:
            _stop(cli)
        _stop(serve)


def test_a_killed_cli_run_is_not_shown_as_running(tmp_path):
    port = _project(tmp_path)
    cli = _start(tmp_path, "get", "slow", log="cli.log")
    _until(lambda: (tmp_path / "cli.started").exists() or cli.poll() is not None, "the CLI run")
    os.killpg(cli.pid, signal.SIGKILL)
    cli.wait(timeout=30)
    serve = _start(
        tmp_path, "serve", "pipeline.py", "--no-schedule", "--port", str(port), log="serve.log"
    )
    try:
        _until(lambda: _get(port, "/health"), "the server")
        assert _running_of(port, "slow") is None
        assert _get(port, "/runs?status=running")["runs"] == []
    finally:
        _stop(serve)
