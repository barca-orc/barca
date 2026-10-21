"""`barca serve` started and stopped the way a supervisor or a container does it (#289)."""

import json
import os
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
from email.message import Message
from pathlib import Path

import pytest

from barca.api import BarcaError, _find_binary
from barca.client import Client

# Settings from the developer's shell that would point the server at a real store.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

WAIT = 30.0

PIPELINE = """
import time
from email.message import Message
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
        status, _, body = self.exchange(method, path)
        return status, body

    def exchange(self, method: str, path: str) -> tuple[int, Message, object]:
        """[`request`], with the response headers."""
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", method=method)
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.status, resp.headers, json.loads(resp.read())
        except urllib.error.HTTPError as e:
            return e.code, e.headers, json.loads(e.read())

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
            # The CLI's error envelope.
            assert {"error", "code", "kind", "remediation"} <= set(body), (path, body)
            assert (body["kind"], body["code"]) == ("usage", 2), (path, body)
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


def start_slow_run(server: Server, project: Path) -> str:
    """Start the run of `slow` and wait until its step is executing; return its handle."""
    going = server.request("POST", "/get/slow")[1]["run_id"]
    wait_for(lambda: (project / "slow.started").exists(), "the slow step to start")
    return going


def draining(server: Server) -> bool:
    return server.request("GET", "/health")[1]["status"] == "draining"


def run_logs(cwd: Path, run_id: str) -> str:
    out = barca(cwd, "logs", run_id, "--json")
    assert out.returncode == 0, out.stderr
    return out.stdout + out.stderr


@pytest.mark.parametrize("sig", [signal.SIGTERM, signal.SIGINT])
def test_a_stop_signal_drains_the_runs_in_flight_and_exits_0(project, serve, sig):
    """#190: the first signal stops admission and waits for the runs in flight. SIGTERM used
    to be discarded when barca was process 1 of a container (#289), and an open /events
    stream kept a stopped server alive for ever."""
    server = serve("--no-schedule")
    base = f"http://127.0.0.1:{server.port}"

    # A run that has finished and one that is in the middle of a step, each with a client
    # that keeps its event stream open the way a browser tab on a run page does.
    done = server.request("POST", "/get/quick")[1]["run_id"]
    wait_for(
        lambda: server.request("GET", f"/status/{done}")[1]["status"] == "complete",
        "the quick run to complete",
    )
    going = start_slow_run(server, project)
    streams = [
        urllib.request.urlopen(f"{base}/events/{run}", timeout=WAIT) for run in (done, going)
    ]
    for stream in streams:
        # Each event is numbered: `id: 0`, then its data.
        assert stream.readline() == b"id: 0\n", "the stream has started"
        assert stream.readline().startswith(b"data:")

    server.proc.send_signal(sig)
    wait_for(lambda: draining(server), "/health to report draining")

    # No new run is admitted, whatever the route; reads and the run in flight go on.
    for path in ("/run", "/get/quick", "/run/nightly"):
        status, headers, body = server.exchange("POST", path)
        assert status == 503, (path, body)
        assert headers["Retry-After"] == "30", headers
        assert "shutting down" in body["error"], body
    assert server.request("GET", f"/status/{going}")[1]["status"] == "running"
    assert server.proc.poll() is None, "the server waits for the run"

    (project / "release").write_text("")
    assert server.proc.wait(timeout=WAIT) == 0, server.log.read_text()
    log = server.log.read_text()
    assert f"[barca] {sig.name} received: draining" in log, log
    assert "[barca] drained: every run finished; shutting down" in log, log

    # Both streams ended, and the drained run's delivered its successful end first.
    tails = [stream.read().decode() for stream in streams]
    assert f'{{"type":"run_finished","run_id":"{going}","ok":true,' in tails[1]
    assert '"status":"success"' in tails[1], "with its outcome"

    # The run finished and was recorded; the refused requests started none.
    assert [r["status"] for r in history(project)] == ["success", "success"]


@pytest.mark.parametrize("setting", ["flag", "barca.toml"])
def test_the_drain_timeout_cancels_the_runs_left_with_reason_server_shutdown_and_exits_1(
    project, serve, setting
):
    if setting == "flag":
        server = serve("--no-schedule", "--drain-timeout", "1")
    else:
        (project / "barca.toml").write_text("[server]\ndrain_timeout = 1\n")
        server = serve("--no-schedule")
    start_slow_run(server, project)

    started = time.monotonic()
    server.proc.send_signal(signal.SIGTERM)
    assert server.proc.wait(timeout=WAIT) == 1, server.log.read_text()
    assert time.monotonic() - started < 15
    log = server.log.read_text()
    assert "the drain timeout (1s) passed: cancelling 1 run (server shutdown)" in log, log
    assert "stopped before the runs in flight finished: 1 run cancelled (server shutdown)" in log

    # Recorded `cancelled`, with the reason in its logs; the step that finished is kept.
    (run,) = history(project)
    assert run["status"] == "cancelled", run
    assert "[barca] run cancelled: server shutdown" in run_logs(project, run["run_id"])
    assert run["steps_executed"] >= 1, run


def test_a_second_signal_stops_the_drain_at_once(project, serve):
    server = serve("--no-schedule")  # the default drain timeout: 30 s
    start_slow_run(server, project)
    server.proc.send_signal(signal.SIGTERM)
    wait_for(lambda: draining(server), "/health to report draining")

    started = time.monotonic()
    server.proc.send_signal(signal.SIGINT)
    assert server.proc.wait(timeout=WAIT) == 1, server.log.read_text()
    assert time.monotonic() - started < 15, "the second signal did not wait out the drain"
    assert "SIGINT received again: cancelling 1 run (server shutdown)" in server.log.read_text()
    assert [r["status"] for r in history(project)] == ["cancelled"]


def test_a_tick_during_the_drain_starts_no_run(project, serve):
    (project / "pipeline.py").write_text(
        PIPELINE
        + """

@asset(freshness=Schedule("* * * * * *"))
def ticker() -> int:
    with open("ticks", "a") as f:
        f.write("tick\\n")
    return 1
"""
    )
    server = serve()
    wait_for(lambda: (project / "ticks").exists(), "a first scheduled run")
    start_slow_run(server, project)
    server.proc.send_signal(signal.SIGTERM)
    wait_for(lambda: draining(server), "/health to report draining")
    time.sleep(3)  # three ticks fall in the drain

    log = server.log.read_text()
    assert "[barca] scheduler stopped: the server is draining" in log, log
    after = log.split("received: draining", 1)[1]
    assert "scheduled run" not in after, log
    (project / "release").write_text("")
    assert server.proc.wait(timeout=WAIT) == 0, server.log.read_text()


@pytest.mark.parametrize("stop_signal", [signal.SIGTERM, signal.SIGINT])
def test_stop_reaps_parallel_parent_and_active_children(project, stop_signal):
    """parallel() freezes its parent with SIGSTOP; cancellation must reap it too."""
    (project / "pipeline.py").write_text("""
import os
import time
from functools import partial
from email.message import Message
from pathlib import Path
from barca import asset, parallel

def slow(index):
    Path(f"child{index}.pid").write_text(str(os.getpid()))
    time.sleep(120)
    return index

@asset()
def work():
    Path("parent.pid").write_text(str(os.getpid()))
    return parallel(partial(slow, 1), partial(slow, 2))
""")
    proc = subprocess.Popen(
        [_find_binary(), "get", "work", "pipeline.py", "--json"],
        cwd=project, env=_env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    try:
        wait_for(lambda: (project / "child2.pid").exists(), "parallel children")
        pids = [int(p.read_text()) for p in project.glob("*.pid")]
        assert len(pids) == 3
        # Observe the actual frozen parent rather than infer it from parallel's API.
        parent = int((project / "parent.pid").read_text())
        wait_for(lambda: "State:\tT" in Path(f"/proc/{parent}/status").read_text(), "frozen parent")
        proc.send_signal(stop_signal)
        _, stderr = proc.communicate(timeout=WAIT)
        assert proc.returncode == 130, stderr
        for pid in pids:
            assert not Path(f"/proc/{pid}").exists(), f"worker {pid} survived cancellation"
        history = json.loads(barca(project, "history", "--json", "--all").stdout)
        assert history["runs"][0]["status"] == "cancelled"
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=WAIT)
