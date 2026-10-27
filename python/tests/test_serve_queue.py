"""The durable run queue of `barca serve`: a restart (a deploy, a reboot, a config change)
loses and cancels nothing it accepted.

Every test runs a real `barca serve`, stops it with the signals a supervisor sends, and starts
it again on the same port and project, as `systemctl restart` or `docker restart` does.
"""

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

from barca.api import _find_binary

# Settings from the developer's shell that would point the server at a real store or server.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

WAIT = 60.0

PIPELINE = """
import time
from pathlib import Path

from barca import task, Schedule


def hold(name):
    with open(f"{name}.starts", "a") as f:
        f.write("start\\n")
    deadline = time.time() + 120
    while not Path(f"release-{name}").exists() and time.time() < deadline:
        time.sleep(0.05)


def mark(name):
    with open("order", "a") as f:
        f.write(name + "\\n")
    print(f"{name} ran")


@task()
def long() -> None:
    hold("long")
    print("long done")


@task()
def one() -> None:
    mark("one")


@task()
def two() -> None:
    mark("two")


@task()
def three() -> None:
    mark("three")
"""

TICKER = """

@task(freshness=Schedule("* * * * * *"))
def ticker() -> None:
    mark("ticker")
"""


def _env(**extra: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_POOL_SIZE"] = "2"
    env.update(extra)
    return env


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(predicate, what: str, timeout: float = WAIT):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    pytest.fail(f"timed out after {timeout:.0f}s waiting for {what}")


class Server:
    """`barca serve pipeline.py` in `cwd` on `port`; its stderr appends to `serve.log`."""

    def __init__(self, cwd: Path, port: int, *args: str) -> None:
        self.cwd = cwd
        self.port = port
        self.url = f"http://127.0.0.1:{port}"
        self.log = cwd / "serve.log"
        self.start_at = self.log.stat().st_size if self.log.exists() else 0
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--port", str(port), *args],
            cwd=cwd,
            stdout=subprocess.DEVNULL,
            stderr=self.log.open("a"),
            env=_env(),
        )
        wait_for(self._up, "the server to answer /health")

    def _up(self) -> bool:
        assert self.proc.poll() is None, f"the server exited early:\n{self.output()}"
        try:
            return self.request("GET", "/health")[0] == 200
        except OSError:
            return False

    def output(self) -> str:
        """What this server printed (the log holds every server of the test)."""
        return self.log.read_text()[self.start_at :]

    def request(self, method: str, path: str, body: dict | None = None) -> tuple[int, dict]:
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(
            f"{self.url}{path}",
            data=data,
            method=method,
            headers={"Content-Type": "application/json"} if data else {},
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.status, json.loads(resp.read())
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read())

    def submit(self, target: str) -> dict:
        status, body = self.request("POST", "/runs", {"command": "run", "targets": [target]})
        assert status == 202, body
        return body

    def run(self, run_id: str) -> dict:
        return self.request("GET", f"/runs/{run_id}")[1]

    def finished(self, run_id: str) -> dict:
        return wait_for(
            lambda: (r := self.run(run_id))["status"] not in ("pending", "running") and r,
            f"run {run_id} to finish",
        )

    def stop(self, sig=signal.SIGTERM) -> int:
        """Send `sig` and wait for the exit code."""
        if self.proc.poll() is None:
            self.proc.send_signal(sig)
        return self.proc.wait(timeout=WAIT)

    def exited(self) -> int:
        """Wait for the server to exit on its own (after a signal already sent)."""
        return self.proc.wait(timeout=WAIT)

    def kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=WAIT)


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


@pytest.fixture()
def serve(project):
    """Start a server; the same port each time, as a restarted service has."""
    port = _free_port()
    servers: list[Server] = []

    def start(*args: str) -> Server:
        servers.append(Server(project, port, *args))
        return servers[-1]

    yield start
    for s in servers:
        s.kill()


def barca(cwd: Path, *args: str, timeout: float = WAIT, **env: str):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_env(**env),
        timeout=timeout,
    )


CLIENTS: list[subprocess.Popen] = []


def client(cwd: Path, *args: str, **env: str) -> subprocess.Popen:
    """A `barca ... --server` command left running, its output in files beside the project."""
    n = len(list(cwd.glob("client-*.out")))
    out_path, err_path = cwd / f"client-{n}.out", cwd / f"client-{n}.err"
    proc = subprocess.Popen(
        [_find_binary(), *args],
        cwd=cwd,
        stdout=out_path.open("w"),
        stderr=err_path.open("w"),
        text=True,
        env=_env(**env),
    )
    proc.paths = (out_path, err_path)  # type: ignore[attr-defined]
    CLIENTS.append(proc)
    return proc


def client_output(_cwd: Path, proc: subprocess.Popen) -> tuple[str, str]:
    out_path, err_path = proc.paths  # type: ignore[attr-defined]
    return out_path.read_text(), err_path.read_text()


@pytest.fixture(autouse=True)
def reap_clients():
    yield
    while CLIENTS:
        proc = CLIENTS.pop()
        if proc.poll() is None:
            proc.kill()
            proc.wait()


def started(project: Path, name: str) -> int:
    path = project / f"{name}.starts"
    return len(path.read_text().splitlines()) if path.exists() else 0


def order(project: Path) -> list[str]:
    path = project / "order"
    return path.read_text().split() if path.exists() else []


def start_long(server: Server, project: Path) -> str:
    """Start `long` and wait until its step is executing; return its run id."""
    before = started(project, "long")
    run_id = server.submit("long")["run_id"]
    wait_for(lambda: started(project, "long") > before, "the long step to start")
    return run_id


# ─── a restart resumes the queue ─────────────────────────────────────────────


def test_a_restart_finishes_the_run_in_flight_and_resumes_the_queued_runs_in_order(project, serve):
    server = serve("--no-schedule", "--max-runs", "1")
    long = start_long(server, project)
    queued = [server.submit(t) for t in ("one", "two", "three")]
    assert [q["status"] for q in queued] == ["pending"] * 3, queued
    assert [q["position"] for q in queued] == [1, 2, 3], queued
    ids = [q["run_id"] for q in queued]
    queue = server.request("GET", "/queue")[1]
    assert [r["run_id"] for r in queue["running"]] == [long], queue
    assert [r["run_id"] for r in queue["queued"]] == ids, queue
    assert queue["max_runs"] == 1, queue

    # Clients following a queued run: `barca logs -f` and the GET /runs/{id} poll.
    follower = client(project, "logs", ids[1], "--follow", "--server", server.url)

    server.proc.send_signal(signal.SIGTERM)
    wait_for(lambda: server.request("GET", "/health")[1]["status"] == "draining", "draining")
    assert server.proc.poll() is None, "the server waits for the run in flight"
    (project / "release-long").write_text("")
    assert server.exited() == 0, server.output()
    log = server.output()
    assert "draining — the queue is paused and new runs are queued" in log, log
    assert "[barca] drained: every run finished; shutting down" in log, log
    assert order(project) == [], "nothing queued started during the drain"

    restarted = serve("--no-schedule", "--max-runs", "1")
    assert "[barca] resuming 3 queued runs" in restarted.output(), restarted.output()
    for run_id in ids:
        assert restarted.finished(run_id)["status"] == "success"
    assert order(project) == ["one", "two", "three"], "in the order they were accepted"
    assert restarted.run(long)["status"] == "success", "the run in flight finished"
    assert started(project, "long") == 1

    assert follower.wait(timeout=WAIT) == 0
    out, err = client_output(project, follower)
    assert "two ran" in out, (out, err)
    assert "lost the event stream of run" in err, err
    assert "the server restarted; following run" in err, err
    assert restarted.request("GET", "/queue")[1]["queued"] == []


def test_a_run_submitted_during_the_drain_is_accepted_and_runs_after_the_restart(project, serve):
    server = serve("--no-schedule")
    start_long(server, project)
    server.proc.send_signal(signal.SIGTERM)
    wait_for(lambda: server.request("GET", "/health")[1]["status"] == "draining", "draining")

    # Accepted and recorded (202, pending), not refused with 503; the older route too.
    accepted = server.submit("one")
    assert accepted["status"] == "pending", accepted
    status, alias = server.request("POST", "/run/two")
    assert status == 200 and alias["run_id"], alias
    queue = server.request("GET", "/queue")[1]
    assert queue["draining"] is True, queue
    assert [r["run_id"] for r in queue["queued"]] == [accepted["run_id"], alias["run_id"]]
    # A `--server` client submits and rides through the restart.
    rider = client(project, "run", "three", "--server", server.url, "--json")
    wait_for(lambda: len(server.request("GET", "/queue")[1]["queued"]) == 3, "the third run")

    (project / "release-long").write_text("")
    assert server.exited() == 0, server.output()
    assert "3 runs wait in the queue for the next start" in server.output(), server.output()

    restarted = serve("--no-schedule", "--max-runs", "1")
    assert "[barca] resuming 3 queued runs" in restarted.output()
    for run_id in (accepted["run_id"], alias["run_id"]):
        assert restarted.finished(run_id)["status"] == "success"
    assert rider.wait(timeout=WAIT) == 0
    out, err = client_output(project, rider)
    assert json.loads(out)["status"] == "success", (out, err)
    assert "the server restarted; following run" in err, err
    assert order(project) == ["one", "two", "three"]


def test_the_drain_timeout_puts_the_run_in_flight_back_in_the_queue_and_exits_0(project, serve):
    server = serve("--no-schedule", "--drain-timeout", "1", "--max-runs", "1")
    long = start_long(server, project)
    behind = server.submit("one")["run_id"]
    follower = client(project, "logs", long, "--follow", "--server", server.url)
    time.sleep(0.5)

    started_at = time.monotonic()
    assert server.stop() == 0, server.output()
    assert time.monotonic() - started_at < 20
    log = server.output()
    assert "the drain timeout (1s) passed: stopping 1 run and putting it back in the queue" in log
    assert f"[barca] run {long} stopped and put back in the queue" in log, log
    assert "2 runs wait in the queue for the next start" in log, log

    # Not cancelled: the next server starts it again from the beginning, first.
    (project / "release-long").write_text("")
    restarted = serve("--no-schedule", "--max-runs", "1")
    assert "[barca] resuming 2 queued runs" in restarted.output(), restarted.output()
    run = restarted.finished(long)
    assert run["status"] == "success", run
    assert started(project, "long") == 2, "the stopped attempt and the new one"
    assert restarted.finished(behind)["status"] == "success"
    assert follower.wait(timeout=WAIT) == 0
    out, err = client_output(project, follower)
    assert "long done" in out, (out, err)
    # Its history has the attempt that finished, not a cancelled one.
    history = barca(project, "history", "--json", "--local")
    statuses = {r["run_id"]: r["status"] for r in json.loads(history.stdout)["runs"]}
    assert statuses[long] == "success", statuses


def test_a_second_signal_cancels_the_runs_in_flight_and_keeps_the_queued_ones(project, serve):
    server = serve("--no-schedule", "--max-runs", "1")
    long = start_long(server, project)
    behind = server.submit("one")["run_id"]
    server.proc.send_signal(signal.SIGTERM)
    wait_for(lambda: server.request("GET", "/health")[1]["status"] == "draining", "draining")
    assert server.stop(signal.SIGINT) == 1, server.output()
    log = server.output()
    assert "SIGINT received again: cancelling 1 run (server shutdown)" in log, log
    assert "1 run cancelled (server shutdown)" in log, log

    restarted = serve("--no-schedule")
    assert restarted.run(long)["status"] == "cancelled"
    assert "[barca] resuming 1 queued run" in restarted.output(), restarted.output()
    assert restarted.finished(behind)["status"] == "success"
    assert started(project, "long") == 1, "a cancelled run is not started again"


# ─── pausing the queue ───────────────────────────────────────────────────────


def test_a_paused_queue_stays_paused_across_a_restart_until_resumed(project, serve):
    server = serve("--no-schedule")
    paused = barca(project, "queue", "pause", "--server", server.url, "--json")
    assert paused.returncode == 0, paused.stderr
    body = json.loads(paused.stdout)
    assert body["paused"] is True and body["paused_at"], body
    assert "[barca] run queue paused on" in paused.stderr
    waiting = server.submit("one")
    assert (waiting["status"], waiting["position"]) == ("pending", 1)
    time.sleep(1)
    assert order(project) == [], "a paused queue starts nothing"
    assert server.request("GET", "/health")[1]["queue"]["paused"] is True

    assert server.stop() == 0, server.output()
    restarted = serve("--no-schedule")
    log = restarted.output()
    assert "[barca] resuming 1 queued run" in log, log
    assert "[barca] the run queue is paused (since" in log, log
    shown = barca(project, "queue", "--server", restarted.url, "--pretty")
    assert shown.returncode == 0, shown.stderr
    assert "queue: paused since" in shown.stdout, shown.stdout
    assert waiting["run_id"] in shown.stdout
    time.sleep(1)
    assert order(project) == []

    resumed = barca(project, "queue", "resume", "--server", restarted.url, "--json")
    assert resumed.returncode == 0, resumed.stderr
    assert json.loads(resumed.stdout)["paused"] is False
    assert restarted.finished(waiting["run_id"])["status"] == "success"


def test_a_scheduled_tick_while_paused_waits_in_the_queue_once(project, serve):
    (project / "pipeline.py").write_text(PIPELINE + TICKER)
    server = serve()
    wait_for(lambda: "ticker" in order(project), "a first scheduled run")
    status, _ = server.request("POST", "/queue/pause")
    assert status == 200
    time.sleep(1)  # a tick that was starting finishes
    before = len(order(project))
    time.sleep(3)  # three ticks while paused
    queue = server.request("GET", "/queue")[1]
    ticks = [r for r in queue["queued"] if r["triggered_by"] == "schedule"]
    # One waits; the ticks after it are skipped while a run of the node is pending.
    assert len(ticks) == 1, queue
    assert ticks[0]["schedule_id"].endswith(":ticker"), ticks
    jobs = {j["id"].rsplit(":", 1)[1]: j for j in server.request("GET", "/schedule")[1]}
    assert jobs["ticker"]["skip_reason"] == "in_flight", jobs["ticker"]
    assert len(order(project)) == before, "nothing ran while paused"
    server.request("POST", "/queue/resume")
    wait_for(lambda: len(order(project)) > before, "the queued tick to run")


def test_a_read_only_server_refuses_pause_and_resume(project, serve):
    server = serve("--read-only")
    for path in ("/queue/pause", "/queue/resume"):
        status, body = server.request("POST", path)
        assert status == 403, body
        assert body["kind"] == "usage", body
    assert server.request("GET", "/queue")[0] == 200
    refused = barca(project, "queue", "pause", "--server", server.url)
    assert refused.returncode == 2, refused.stderr
    assert "read-only" in refused.stderr


def test_barca_queue_needs_a_server(project):
    out = barca(project, "queue", "--json")
    assert out.returncode == 2, out
    envelope = json.loads(out.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "usage"
    assert "no server is given" in envelope["error"]
    local = barca(project, "queue", "--local")
    assert local.returncode == 2, local


# ─── clients ride through a restart ──────────────────────────────────────────


def test_a_client_gives_up_when_the_server_does_not_come_back(project, serve):
    server = serve("--no-schedule", "--drain-timeout", "0")
    long = start_long(server, project)
    follower = client(
        project, "logs", long, "--follow", "--server", server.url, BARCA_SERVER_RECONNECT="2"
    )
    time.sleep(0.5)
    assert server.stop() == 0, server.output()
    assert follower.wait(timeout=WAIT) == 3
    _, err = client_output(project, follower)
    assert "the server did not come back within 2s (BARCA_SERVER_RECONNECT)" in err, err
    assert f"lost the event stream of run {long}" in err, err
    # 0 is the behaviour before 0.29.4: no wait at all.
    bad = barca(
        project, "logs", long, "--follow", "--server", server.url, BARCA_SERVER_RECONNECT="soon"
    )
    assert bad.returncode == 2, bad.stderr
    assert "BARCA_SERVER_RECONNECT=soon is not a number of seconds" in bad.stderr
