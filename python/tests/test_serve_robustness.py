"""`barca serve` started and stopped the way a supervisor or a container does it (#289)."""

import json
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from barca.api import _find_binary

# Settings from the developer's shell that would point the server at a real store.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

WAIT = 30.0

PIPELINE = """
import time
from pathlib import Path

from barca import asset, task, Schedule


@asset()
def quick() -> int:
    return 1


@asset(inputs={"x": quick})
def slow(x: int) -> int:
    Path("slow.started").write_text("")
    deadline = time.time() + 120
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return x + 1


@task(freshness=Schedule("0 5 * * *"))
def nightly() -> None:
    print("nightly")
"""


def _env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_POOL_SIZE"] = "2"
    return env


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


def barca(cwd: Path, *args: str, timeout: float = WAIT) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_env(),
        timeout=timeout,
    )


def wait_for(predicate, what: str, timeout: float = WAIT):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    pytest.fail(f"timed out after {timeout:.0f}s waiting for {what}")


class Server:
    """A `barca serve` child on a free port; its stderr goes to `serve.log` in the project."""

    def __init__(self, cwd: Path, *args: str) -> None:
        self.cwd = cwd
        self.port = _free_port()
        self.log = cwd / "serve.log"
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--port", str(self.port), *args],
            cwd=cwd,
            stdout=subprocess.DEVNULL,
            stderr=self.log.open("w"),
            env=_env(),
        )
        wait_for(self._up, "the server to answer /health")

    def _up(self) -> bool:
        assert self.proc.poll() is None, f"the server exited early:\n{self.log.read_text()}"
        try:
            return self.request("GET", "/health")[0] == 200
        except OSError:
            return False

    def request(self, method: str, path: str) -> tuple[int, object]:
        """One request: the HTTP status and the decoded JSON body, error responses included."""
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", method=method)
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.status, json.loads(resp.read())
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read())

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=WAIT)


@pytest.fixture()
def serve(project):
    servers: list[Server] = []

    def start(*args: str) -> Server:
        servers.append(Server(project, *args))
        return servers[-1]

    yield start
    for s in servers:
        s.stop()


# ─── --timezone ──────────────────────────────────────────────────────────────


@pytest.mark.parametrize("value", ["Mars/Phobos", "america/new_york", ""])
def test_an_unknown_timezone_is_a_usage_error(project, value):
    # It used to start the server on local time with one line on stderr. `timeout` ends the
    # test if it starts one again.
    out = barca(project, "serve", "pipeline.py", "--port", str(_free_port()), "--timezone", value)
    assert out.returncode == 2, out.stderr
    assert f"unknown timezone '{value}'" in out.stderr
    assert "`local`, `utc`, or an IANA name such as `America/New_York`" in out.stderr
    assert out.stdout == ""
    assert not (project / ".barca").exists(), "nothing may start before the flag is checked"


@pytest.mark.parametrize("value", ["local", "utc", "UTC", "Europe/Berlin", "Etc/UTC"])
def test_a_known_timezone_starts_the_server(serve, value):
    server = serve("--timezone", value)
    assert server.request("GET", "/health")[0] == 200


# ─── next fire times ─────────────────────────────────────────────────────────


def test_the_server_reports_next_fire_in_its_own_timezone(serve):
    # `nightly` is `0 5 * * *`. Kolkata is UTC+5:30 all year, so 05:00 there is 23:30 UTC,
    # whatever zone this machine is in. /schedule and /state used to use the machine's zone.
    server = serve("--timezone", "Asia/Kolkata")
    jobs = wait_for(lambda: server.request("GET", "/schedule")[1], "the scheduler to publish")
    assert [j["id"] for j in jobs] == ["pipeline.py:nightly"]
    assert jobs[0]["next_fire"] % 86400 == 23 * 3600 + 1800
    status, nodes = server.request("GET", "/state")
    assert status == 200
    next_run = {n["id"]: n["next_run"] for n in nodes}
    assert next_run["pipeline.py:nightly"] == jobs[0]["next_fire"]
    assert next_run["pipeline.py:quick"] is None


def test_list_says_its_next_fire_times_are_local(project):
    out = barca(project, "list", "pipeline.py", "--pretty")
    assert out.returncode == 0, out.stderr
    header = out.stdout.splitlines()[0]
    assert "NEXT FIRE (LOCAL TIME)" in header
    # JSON keeps the same wall-clock string; `barca docs contract` says which zone it is in.
    nodes = json.loads(barca(project, "list", "pipeline.py", "--json").stdout)["nodes"]
    nightly = next(n for n in nodes if n["id"] == "pipeline.py:nightly")
    assert nightly["next_fire"].endswith(" 05:00:00")
