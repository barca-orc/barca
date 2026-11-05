"""`barca serve` started and stopped the way a supervisor or a container does it (#289)."""

import json
import re
import signal
import subprocess
import time
import urllib.request
from pathlib import Path

import pytest

from barca.api import BarcaError, _find_binary
from barca.client import Client

from . import helpers
from .helpers import QUICK, clean_env, wait_until
from .served import Serve

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


def _pool_env() -> dict[str, str]:
    """This module's environment: a scrubbed shell with a pool of two workers."""
    return clean_env(BARCA_POOL_SIZE="2")


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


def barca(cwd: Path, *args: str, timeout: float = QUICK) -> subprocess.CompletedProcess:
    return helpers.barca(cwd, *args, env=_pool_env(), timeout=timeout)


def process_state(pid: int) -> str | None:
    """The process's `ps` state (`T` stopped, `S` sleeping, ...; the same letters on Linux
    and macOS), or None once it is gone. A zombie counts as gone: it has exited."""
    out = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    state = out.stdout.strip()
    return None if out.returncode != 0 or not state or state.startswith("Z") else state


class Server(Serve):
    """`barca serve pipeline.py` on a free port with this module's environment, for the modules
    that still import it from here; `stop()` kills it, as the old wrapper did."""

    def __init__(self, cwd: Path, *args: str) -> None:
        super().__init__(cwd, *args, env=_pool_env())

    @property
    def log(self) -> Path:
        return self.err_path

    def stop(self, sig: int = signal.SIGKILL, timeout: float = 30) -> int:
        return super().stop(sig, timeout)


@pytest.fixture()
def serve(project, serve):
    """The shared `serve` factory with this module's environment."""

    def start(*args: str, **options) -> Serve:
        options.setdefault("env", _pool_env())
        return serve(*args, **options)

    return start


# ─── --timezone ──────────────────────────────────────────────────────────────


@pytest.mark.parametrize("value", ["Mars/Phobos", "america/new_york", ""])
def test_an_unknown_timezone_is_a_usage_error(project, value):
    # It used to start the server on local time with one line on stderr. `timeout` ends the
    # test if it starts one again.
    port = str(helpers.free_port())
    out = barca(project, "serve", "pipeline.py", "--port", port, "--timezone", value)
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
    jobs = wait_until(lambda: server.request("GET", "/schedule")[1], "the scheduler to publish")
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


def test_a_trigger_for_a_target_that_cannot_run_is_an_error_response(project, serve):
    # Each of these used to be 200 with a run id, and the run then failed.
    (project / "other.py").write_text(
        "from barca import asset\n\n@asset()\ndef quick():\n    return 2\n"
    )
    server = serve("--no-schedule", files=("pipeline.py", "other.py"))
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
    client = Client(server.url)
    with pytest.raises(BarcaError, match=r"\(404\): Asset 'nope' not found"):
        client.get("nope")
    with pytest.raises(BarcaError, match=r"\(400\): 'slow' is an asset"):
        client.run("slow")

    # A full id picks one of two nodes with the same name, and that run happens.
    result = client.get("other.py:quick").wait(timeout=30)
    assert result["status"] == "complete", result
    # It is the only run there has been: the refused requests started none.
    assert [r["status"] for r in history(project)] == ["success"]


# ─── stopping the server ─────────────────────────────────────────────────────


def start_slow_run(server: Serve, project: Path) -> str:
    """Start the run of `slow` and wait until its step is executing; return its handle."""
    going = server.request("POST", "/get/slow")[1]["run_id"]
    wait_until(lambda: (project / "slow.started").exists(), "the slow step to start")
    return going


def draining(server: Serve) -> bool:
    return server.request("GET", "/health")[1]["status"] == "draining"


def run_logs(cwd: Path, run_id: str) -> str:
    out = barca(cwd, "logs", run_id, "--json")
    assert out.returncode == 0, out.stderr
    return out.stdout + out.stderr


@pytest.mark.parametrize("sig", [signal.SIGTERM, signal.SIGINT])
def test_a_stop_signal_drains_the_runs_in_flight_and_exits_0(project, serve, sig):
    """#190: the first signal pauses the queue and waits for the runs in flight. SIGTERM used
    to be discarded when barca was process 1 of a container (#289), and an open /events
    stream kept a stopped server alive for ever."""
    server = serve("--no-schedule")

    # A run that has finished and one that is in the middle of a step, each with a client
    # that keeps its event stream open the way a browser tab on a run page does.
    done = server.request("POST", "/get/quick")[1]["run_id"]
    wait_until(
        lambda: server.request("GET", f"/status/{done}")[1]["status"] == "complete",
        "the quick run to complete",
    )
    going = start_slow_run(server, project)
    streams = [
        urllib.request.urlopen(f"{server.url}/events/{run}", timeout=30) for run in (done, going)
    ]
    for stream in streams:
        # The first event carries a random attempt epoch and one-based sequence.
        assert re.fullmatch(rb"id: [0-9a-f]{32}:1\n", stream.readline()), "the stream has started"
        assert stream.readline().startswith(b"data:")

    server.send(sig)
    wait_until(lambda: draining(server), "/health to report draining")

    # New runs are accepted into the queue for the next start, whatever the route, and none
    # starts; reads and the run in flight go on.
    for path in ("/run", "/get/quick", "/run/nightly"):
        status, body = server.request("POST", path)
        assert status == 200, (path, body)
        assert server.request("GET", f"/status/{body['run_id']}")[1]["status"] == "pending"
    assert server.request("GET", f"/status/{going}")[1]["status"] == "running"
    assert server.proc.poll() is None, "the server waits for the run"

    (project / "release").write_text("")
    assert server.exited() == 0, server.stderr
    log = server.stderr
    assert f"[barca] {sig.name} received: draining" in log, log
    assert "[barca] drained: every run finished; shutting down" in log, log

    # Both streams ended, and the drained run's delivered its successful end first.
    tails = [stream.read().decode() for stream in streams]
    assert f'{{"type":"run_finished","run_id":"{going}","ok":true,' in tails[1]
    assert '"status":"success"' in tails[1], "with its outcome"

    # The run finished and was recorded; the queued ones wait for the next server.
    assert [r["status"] for r in history(project)] == ["success", "success"]
    assert "3 runs wait in the queue for the next start" in log, log


@pytest.mark.parametrize("setting", ["flag", "barca.toml"])
def test_the_drain_timeout_requeues_the_runs_left_and_exits_0(project, serve, setting):
    if setting == "flag":
        server = serve("--no-schedule", "--drain-timeout", "1")
    else:
        (project / "barca.toml").write_text("[server]\ndrain_timeout = 1\n")
        server = serve("--no-schedule")
    going = start_slow_run(server, project)

    started = time.monotonic()
    server.send(signal.SIGTERM)
    assert server.exited() == 0, server.stderr
    assert time.monotonic() - started < 15
    log = server.stderr
    assert "the drain timeout (1s) passed: stopping 1 run and putting it back in the queue" in log
    assert f"[barca] run {going} stopped and put back in the queue" in log, log

    # Not recorded as an attempt that ended: the next server starts it again (its first step,
    # which finished, is a cache hit then).
    assert history(project) == []
    (project / "release").write_text("")
    restarted = serve("--no-schedule")
    assert "[barca] resuming 1 queued run" in restarted.stderr
    run = wait_until(
        lambda: (r := restarted.request("GET", f"/runs/{going}")[1])["status"] == "success" and r,
        "the requeued run to finish",
    )
    assert {s["node_id"].rsplit(":", 1)[1] for s in run["steps"]} >= {"quick", "slow"}, run


def test_a_second_signal_stops_the_drain_at_once(project, serve):
    server = serve("--no-schedule")  # the default drain timeout: 30 s
    start_slow_run(server, project)
    server.send(signal.SIGTERM)
    wait_until(lambda: draining(server), "/health to report draining")

    started = time.monotonic()
    server.send(signal.SIGINT)
    assert server.exited() == 1, server.stderr
    assert time.monotonic() - started < 15, "the second signal did not wait out the drain"
    assert "SIGINT received again: cancelling 1 run (server shutdown)" in server.stderr
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
    wait_until(lambda: (project / "ticks").exists(), "a first scheduled run")
    start_slow_run(server, project)
    server.send(signal.SIGTERM)
    wait_until(lambda: draining(server), "/health to report draining")
    time.sleep(3)  # a fixed wait: three ticks fall in the drain, and none may start a run

    log = server.stderr
    assert "[barca] scheduler stopped: the server is draining" in log, log
    after = log.split("received: draining", 1)[1]
    assert "scheduled run" not in after, log
    (project / "release").write_text("")
    assert server.exited() == 0, server.stderr


@pytest.mark.parametrize("stop_signal", [signal.SIGTERM, signal.SIGINT])
def test_stop_reaps_parallel_parent_and_active_children(project, stop_signal):
    """parallel() freezes its parent with SIGSTOP; cancellation must reap it too."""
    (project / "pipeline.py").write_text("""
import os
import re
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
        cwd=project,
        env=_pool_env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait_until(lambda: (project / "child2.pid").exists(), "parallel children")
        pids = [int(p.read_text()) for p in project.glob("*.pid")]
        assert len(pids) == 3
        # Observe the actual frozen parent rather than infer it from parallel's API.
        parent = int((project / "parent.pid").read_text())
        wait_until(lambda: (process_state(parent) or "").startswith("T"), "frozen parent")
        proc.send_signal(stop_signal)
        _, stderr = proc.communicate(timeout=QUICK)
        assert proc.returncode == 130, stderr
        for pid in pids:
            assert process_state(pid) is None, f"worker {pid} survived cancellation"
        history = json.loads(barca(project, "history", "--json", "--all").stdout)
        assert history["runs"][0]["status"] == "cancelled"
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=QUICK)
