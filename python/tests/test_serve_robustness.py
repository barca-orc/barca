"""`barca serve` started and stopped the way a supervisor or a container does it (#289)."""

import json
import os
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from barca.api import BarcaError, _find_binary
from barca.client import Client

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


# ─── POST /get/{target} and POST /run/{target} ───────────────────────────────


def history(cwd: Path) -> list[dict]:
    out = barca(cwd, "history", "--json")
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)["runs"]


def test_a_trigger_for_a_target_that_cannot_run_is_an_error_response(project):
    # Each of these used to be 200 with a run id, and the run then failed.
    (project / "other.py").write_text(
        "from barca import asset\n\n@asset()\ndef quick():\n    return 2\n"
    )
    server = Server(project, "other.py", "--no-schedule")
    try:
        cases = [
            ("POST", "/get/nope", 404, "Asset 'nope' not found. Available: "),
            ("POST", "/run/nope", 404, "Asset 'nope' not found. Available: "),
            ("POST", "/get/quick", 409, "'quick' matches more than one node: other.py:quick, "),
            ("POST", "/get/nightly", 400, "'nightly' is a task: use POST /run/nightly"),
            ("POST", "/run/slow", 400, "'slow' is an asset: use POST /get/slow"),
        ]
        for method, path, want, says in cases:
            status, body = server.request(method, path)
            assert status == want, (path, body)
            assert set(body) == {"error"}, (path, body)
            assert body["error"].startswith(says), (path, body)

        # The Python client raises with the server's message.
        client = Client(f"http://127.0.0.1:{server.port}")
        with pytest.raises(BarcaError, match=r"\(404\): Asset 'nope' not found"):
            client.get("nope")
        with pytest.raises(BarcaError, match=r"\(400\): 'slow' is an asset"):
            client.run("slow")

        # A full id picks one of two nodes with the same name, and that run happens.
        result = client.get("other.py:quick").wait(timeout=WAIT)
        assert result["status"] == "complete", result
        # It is the only run there has been: the refused requests started none.
        assert [r["status"] for r in history(project)] == ["success"]
    finally:
        server.stop()


# ─── stopping the server ─────────────────────────────────────────────────────


@pytest.mark.parametrize("sig", [signal.SIGTERM, signal.SIGINT])
def test_a_stop_signal_cancels_the_runs_and_exits_0(project, serve, sig):
    """SIGTERM used to keep its default action (and was discarded when barca was process 1 of
    a container); and an open /events stream kept a server stopped with SIGINT alive for ever."""
    server = serve("--no-schedule")
    base = f"http://127.0.0.1:{server.port}"

    # A run that has finished and one that is in the middle of a step, each with a client
    # that keeps its event stream open the way a browser tab on a run page does.
    done = server.request("POST", "/get/quick")[1]["run_id"]
    wait_for(
        lambda: server.request("GET", f"/status/{done}")[1]["status"] == "complete",
        "the quick run to complete",
    )
    going = server.request("POST", "/get/slow")[1]["run_id"]
    wait_for(lambda: (project / "slow.started").exists(), "the slow step to start")
    streams = [
        urllib.request.urlopen(f"{base}/events/{run}", timeout=WAIT) for run in (done, going)
    ]
    for stream in streams:
        assert stream.readline().startswith(b"data:"), "the stream has started"

    server.proc.send_signal(sig)
    assert server.proc.wait(timeout=WAIT) == 0, server.log.read_text()
    assert f"[barca] {sig.name} received: stopping runs and shutting down" in server.log.read_text()

    # Both streams ended, and the one of the cancelled run delivered its last event first.
    tails = [stream.read().decode() for stream in streams]
    assert f'{{"type":"run_finished","run_id":"{going}","ok":false}}' in tails[1]

    # The run in flight recorded itself as cancelled; it was not left `running`.
    assert [r["status"] for r in history(project)] == ["cancelled", "success"]
