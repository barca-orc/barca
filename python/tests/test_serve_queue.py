"""The durable run queue of `barca serve`: a restart (a deploy, a reboot, a config change)
loses and cancels nothing it accepted.

Every test runs a real `barca serve`, stops it with the signals a supervisor sends, and starts
it again on the same port and project, as `systemctl restart` or `docker restart` does.
"""

import json
import signal
import sqlite3
import subprocess
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import helpers
from .helpers import RUN, clean_env, free_port, wait_until
from .served import Serve

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
    """This module's environment: a scrubbed shell with a pool of two workers."""
    return clean_env(BARCA_POOL_SIZE="2", **extra)


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


@pytest.fixture()
def serve(project, serve):
    """Start a server; the same port each time, as a restarted service has."""
    port = free_port()

    def start(*args: str) -> Serve:
        return serve(*args, port=port, env=_env())

    return start


def barca(cwd: Path, *args: str, timeout: float = RUN, **env: str):
    return helpers.barca(cwd, *args, env=_env(**env), timeout=timeout)


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


def start_long(server: Serve, project: Path) -> str:
    """Start `long` and wait until its step is executing; return its run id."""
    before = started(project, "long")
    run_id = server.submit("long")["run_id"]
    wait_until(lambda: started(project, "long") > before, "the long step to start")
    return run_id


def draining(server: Serve) -> bool:
    return server.request("GET", "/health")[1]["status"] == "draining"


def test_multi_target_counts_match_queue_history_and_run_detail(project, serve):
    with (project / "pipeline.py").open("a") as pipeline:
        pipeline.write("\nfrom barca import asset\n")
        for name in ("alpha", "beta", "gamma"):
            pipeline.write(f"\n@asset()\ndef {name}() -> int:\n    return 1\n")
    server = serve("--no-schedule", "--max-runs", "1")
    start_long(server, project)
    targets = ["alpha", "beta", "gamma"]
    status, accepted = server.request("POST", "/runs", {"command": "get", "targets": targets})
    assert status == 202, accepted
    run_id = accepted["run_id"]
    queue = server.request("GET", "/queue")[1]
    assert queue["slots"] == "1 of 1 running"
    assert queue["queued"][0]["target_line"] == "alpha +2 more"
    cli = barca(project, "queue", "--server", server.url, "--pretty")
    assert cli.returncode == 0, cli.stderr
    assert "alpha +2 more" in cli.stdout and "alpha,beta,gamma" not in cli.stdout
    (project / "release-long").touch()
    detail = server.finished(run_id)["explanation"]
    counts = detail["counts"]
    assert counts["steps"] == counts["ran"] == counts["attempts"] == 3
    assert counts["ok"] + counts["failed"] + counts["cached"] + counts["not_reached"] == 3
    assert len(detail["outputs"]) == 3
    rows = server.request("GET", "/runs")[1]["runs"]
    row = next(row for row in rows if row["run_id"] == run_id)["line"]
    assert row["steps"] == counts["outcome"]
    assert row["target"] == detail["target_line"] == "alpha +2 more"


def test_failed_run_accounts_for_the_downstream_steps_it_never_reached(project, serve):
    with (project / "pipeline.py").open("a") as pipeline:
        pipeline.write("""
from barca import asset

@asset()
def seed() -> int:
    raise ValueError("synthetic failure")

@asset(inputs={"seed": seed})
def middle(seed: int) -> int:
    return seed

@asset(inputs={"middle": middle})
def output(middle: int) -> int:
    return middle
""")
    server = serve("--no-schedule", "--max-runs", "1")
    run_id = server.submit("output", command="get")["run_id"]
    detail = server.finished(run_id)["explanation"]
    counts = detail["counts"]
    assert counts["steps"] == counts["nodes"] == 3
    assert counts["ran"] == counts["failed"] == counts["attempts"] == 1
    assert counts["ok"] == counts["cached"] == counts["retries"] == 0
    assert counts["not_reached"] == counts["not_reached_nodes"] == 2
    assert counts["outcome"] == "0 ok · 1 failed · 0 cached · 2 not reached"
    row = next(r for r in server.get("/runs")["runs"] if r["run_id"] == run_id)
    assert row["line"]["steps"] == counts["outcome"]
    history = barca(project, "history", run_id, "--server", server.url, "--json")
    assert history.returncode == 0, history.stderr
    assert json.loads(history.stdout)["explanation"]["counts"] == counts


# ─── a restart resumes the queue ─────────────────────────────────────────────


def test_a_restart_finishes_the_run_in_flight_and_resumes_the_queued_runs_in_order(project, serve):
    server = serve("--no-schedule", "--max-runs", "1")
    long = start_long(server, project)
    queued = [server.submit(t) for t in ("one", "two", "three")]
    assert [q["status"] for q in queued] == ["pending"] * 3, queued
    assert [q["position"] for q in queued] == [1, 2, 3], queued
    # Queued behind a run, not a restart: nothing else holds them (#1011).
    assert [q["waiting_for"] for q in queued] == [None] * 3, queued
    ids = [q["run_id"] for q in queued]
    queue = server.request("GET", "/queue")[1]
    assert [r["run_id"] for r in queue["running"]] == [long], queue
    assert [r["run_id"] for r in queue["queued"]] == ids, queue
    assert [r["waiting_for"] for r in queue["running"] + queue["queued"]] == [None] * 4, queue
    assert server.run(ids[0])["run"]["waiting_for"] is None
    assert queue["max_runs"] == 1, queue

    # Clients following a queued run: `barca logs -f` and the GET /runs/{id} poll.
    follower = client(project, "logs", ids[1], "--follow", "--server", server.url)

    server.send(signal.SIGTERM)
    wait_until(lambda: draining(server), "draining")
    assert server.proc.poll() is None, "the server waits for the run in flight"
    (project / "release-long").write_text("")
    assert server.exited() == 0, server.stderr
    log = server.stderr
    assert "draining — the queue is paused and new runs are queued" in log, log
    assert "[barca] drained: every run finished; shutting down" in log, log
    assert order(project) == [], "nothing queued started during the drain"

    restarted = serve("--no-schedule", "--max-runs", "1")
    assert "[barca] resuming 3 queued runs" in restarted.stderr, restarted.stderr
    for run_id in ids:
        assert restarted.finished(run_id)["run"]["status"] == "success"
    assert order(project) == ["one", "two", "three"], "in the order they were accepted"
    assert restarted.run(long)["run"]["status"] == "success", "the run in flight finished"
    assert started(project, "long") == 1

    assert follower.wait(timeout=RUN) == 0, client_output(project, follower)
    out, err = client_output(project, follower)
    assert "two ran" in out, (out, err)
    assert "lost the event stream of run" in err, err
    assert "the server restarted; following run" in err, err
    assert restarted.request("GET", "/queue")[1]["queued"] == []


def test_a_run_submitted_during_the_drain_is_accepted_and_runs_after_the_restart(project, serve):
    server = serve("--no-schedule")
    start_long(server, project)
    server.send(signal.SIGTERM)
    wait_until(lambda: draining(server), "draining")

    # Accepted and recorded (202, pending), not refused with 503; the older route too. The
    # answer says the run waits for the next server, not for the runs ahead of it (#1011).
    accepted = server.submit("one")
    assert accepted["status"] == "pending", accepted
    assert accepted["position"] == 1, accepted
    assert accepted["waiting_for"] == "restart", accepted
    status, alias = server.request("POST", "/run/two")
    assert status == 200 and alias["run_id"], alias
    queue = server.request("GET", "/queue")[1]
    assert queue["draining"] is True, queue
    assert [r["run_id"] for r in queue["queued"]] == [accepted["run_id"], alias["run_id"]]
    assert [r["waiting_for"] for r in queue["queued"]] == ["restart"] * 2, queue
    assert [r["waiting_for"] for r in queue["running"]] == [None], "the run in flight finishes"
    view = server.run(accepted["run_id"])
    assert (view["run"]["status"], view["run"]["waiting_for"]) == ("pending", "restart"), view
    # A `--server` client submits, says it waits for a restart, and rides through it.
    rider = client(project, "run", "three", "--server", server.url, "--json")
    wait_until(lambda: len(server.request("GET", "/queue")[1]["queued"]) == 3, "the third run")
    third = server.request("GET", "/queue")[1]["queued"][2]
    wait_until(
        lambda: "is queued behind a restart" in client_output(project, rider)[1], "the rider's line"
    )
    assert (
        f"[barca] run {third['run_id']} is queued behind a restart (position 3): the server at "
        f"{server.url} is draining and starts no run; it starts when the next server is up"
    ) in client_output(project, rider)[1]

    (project / "release-long").write_text("")
    assert server.exited() == 0, server.stderr
    assert "3 runs wait in the queue for the next start" in server.stderr, server.stderr

    restarted = serve("--no-schedule", "--max-runs", "1")
    assert "[barca] resuming 3 queued runs" in restarted.stderr
    for run_id in (accepted["run_id"], alias["run_id"]):
        finished = restarted.finished(run_id)
        assert (finished["run"]["status"], finished["run"]["waiting_for"]) == ("success", None), (
            finished
        )
    assert rider.wait(timeout=RUN) == 0
    out, err = client_output(project, rider)
    assert json.loads(out)["status"] == "success", (out, err)
    assert "the server restarted; following run" in err, err
    assert order(project) == ["one", "two", "three"]


def test_the_drain_timeout_puts_the_run_in_flight_back_in_the_queue_and_exits_0(project, serve):
    server = serve("--no-schedule", "--drain-timeout", "1", "--max-runs", "1")
    long = start_long(server, project)
    behind = server.submit("one")["run_id"]
    follower = client(project, "logs", long, "--follow", "--server", server.url)
    # A fixed wait: the follower opens its event stream before the server stops, and nothing
    # observable marks that it has.
    time.sleep(0.5)

    started_at = time.monotonic()
    assert server.stop() == 0, server.stderr
    assert time.monotonic() - started_at < 20
    log = server.stderr
    assert "the drain timeout (1s) passed: stopping 1 run and putting it back in the queue" in log
    assert f"[barca] run {long} stopped and put back in the queue" in log, log
    assert "2 runs wait in the queue for the next start" in log, log

    # Not cancelled: the next server starts it again from the beginning, first.
    (project / "release-long").write_text("")
    restarted = serve("--no-schedule", "--max-runs", "1")
    assert "[barca] resuming 2 queued runs" in restarted.stderr, restarted.stderr
    run = restarted.finished(long)
    assert run["run"]["status"] == "success", run
    assert started(project, "long") == 2, "the stopped attempt and the new one"
    assert restarted.finished(behind)["run"]["status"] == "success"
    assert follower.wait(timeout=RUN) == 0, client_output(project, follower)
    out, err = client_output(project, follower)
    assert "long done" in out, (out, err)
    # Its history has the attempt that finished, not a cancelled one.
    history = barca(project, "history", "--json", "--local")
    statuses = {r["run_id"]: r["status"] for r in json.loads(history.stdout)["runs"]}
    assert statuses[long] == "success", statuses


def test_cli_cancel_acknowledges_a_queued_run(project, serve):
    server = serve("--no-schedule")
    server.post("/queue/pause", {})
    run_id = server.submit("one")["run_id"]
    result = barca(project, "cancel", run_id, "--server", server.url, "--json")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {"run_id": run_id, "status": "cancelled"}
    assert server.finished(run_id)["run"]["status"] == "cancelled"
    again = barca(project, "cancel", run_id, "--server", server.url, "--json")
    assert again.returncode != 0
    assert "already finished" in again.stderr


def test_a_second_signal_cancels_the_runs_in_flight_and_keeps_the_queued_ones(project, serve):
    server = serve("--no-schedule", "--max-runs", "1")
    long = start_long(server, project)
    behind = server.submit("one")["run_id"]
    server.send(signal.SIGTERM)
    wait_until(lambda: draining(server), "draining")
    assert server.stop(signal.SIGINT) == 1, server.stderr
    log = server.stderr
    assert "SIGINT received again: cancelling 1 run (server shutdown)" in log, log
    assert "1 run cancelled (server shutdown)" in log, log

    restarted = serve("--no-schedule")
    assert restarted.run(long)["run"]["status"] == "cancelled"
    assert "[barca] resuming 1 queued run" in restarted.stderr, restarted.stderr
    assert restarted.finished(behind)["run"]["status"] == "success"
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
    time.sleep(1)  # a fixed wait: nothing may start while the queue is paused
    assert order(project) == [], "a paused queue starts nothing"
    assert server.request("GET", "/health")[1]["queue"]["paused"] is True

    assert server.stop() == 0, server.stderr
    restarted = serve("--no-schedule")
    log = restarted.stderr
    assert "[barca] resuming 1 queued run" in log, log
    assert "[barca] the run queue is paused (since" in log, log
    shown = barca(project, "queue", "--server", restarted.url, "--pretty")
    assert shown.returncode == 0, shown.stderr
    assert "queue: paused since" in shown.stdout, shown.stdout
    assert waiting["run_id"] in shown.stdout
    time.sleep(1)  # a fixed wait: the restarted server may start nothing either
    assert order(project) == []

    resumed = barca(project, "queue", "resume", "--server", restarted.url, "--json")
    assert resumed.returncode == 0, resumed.stderr
    assert json.loads(resumed.stdout)["paused"] is False
    assert restarted.finished(waiting["run_id"])["run"]["status"] == "success"


def test_a_scheduled_tick_while_paused_waits_in_the_queue_once(project, serve):
    (project / "pipeline.py").write_text(PIPELINE + TICKER)
    server = serve()
    wait_until(lambda: "ticker" in order(project), "a first scheduled run")
    status, _ = server.request("POST", "/queue/pause")
    assert status == 200
    time.sleep(1)  # a fixed wait: a tick that was starting finishes
    before = len(order(project))
    time.sleep(3)  # a fixed wait: three ticks pass while paused, and none may run
    queue = server.request("GET", "/queue")[1]
    ticks = [r for r in queue["queued"] if r["triggered_by"] == "schedule"]
    # One waits; the ticks after it are skipped while a run of the node is pending.
    assert len(ticks) == 1, queue
    assert ticks[0]["schedule_id"].endswith(":ticker"), ticks
    jobs = {j["id"].rsplit(":", 1)[1]: j for j in server.request("GET", "/schedule")[1]}
    assert jobs["ticker"]["skip_reason"] == "in_flight", jobs["ticker"]
    assert len(order(project)) == before, "nothing ran while paused"
    server.request("POST", "/queue/resume")
    wait_until(lambda: len(order(project)) > before, "the queued tick to run")


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
    # A fixed wait: the follower opens its event stream before the server stops, and nothing
    # observable marks that it has.
    time.sleep(0.5)
    assert server.stop() == 0, server.stderr
    assert follower.wait(timeout=RUN) == 3
    _, err = client_output(project, follower)
    assert "the server did not come back within 2s (BARCA_SERVER_RECONNECT)" in err, err
    assert f"lost the event stream of run {long}" in err, err
    # 0 is the behaviour before 0.29.4: no wait at all.
    bad = barca(
        project, "logs", long, "--follow", "--server", server.url, BARCA_SERVER_RECONNECT="soon"
    )
    assert bad.returncode == 2, bad.stderr
    assert "BARCA_SERVER_RECONNECT=soon is not a number of seconds" in bad.stderr


# ─── a resumed run re-plans on the code it resumes on ───────────────────────

CHANGING = """
import time
from pathlib import Path

from barca import asset


@asset()
def a() -> int:
    return VERSION


@asset(inputs={"a": a})
def b(a: int) -> int:
    with open("b.starts", "a") as f:
        f.write("start\\n")
    deadline = time.time() + 120
    while not Path("release-b").exists() and time.time() < deadline:
        time.sleep(0.05)
    return a * 10
"""


def test_a_run_requeued_onto_new_code_records_its_steps_under_the_new_run_hash(project, serve):
    """#794: a run a deploy requeues resumes under the same run id on the new code. A step its
    stopped attempt finished re-executes under the new definition and is recorded under its new
    run hash, so the run publishes the new results and the next run finds them cached."""
    (project / "pipeline.py").write_text(CHANGING.replace("VERSION", "1"))
    server = serve("--no-schedule", "--drain-timeout", "1")
    run_id = server.submit("b", "get")["run_id"]
    wait_until(lambda: started(project, "b") > 0, "b to start")
    assert server.stop() == 0, server.stderr
    assert f"[barca] run {run_id} stopped and put back in the queue" in server.stderr

    # The deploy: `a` and `b` change while the run waits in the queue.
    (project / "pipeline.py").write_text(
        CHANGING.replace("VERSION", "2").replace("a * 10", "a * 100")
    )
    (project / "release-b").write_text("")
    restarted = serve("--no-schedule")
    assert "[barca] resuming 1 queued run" in restarted.stderr, restarted.stderr
    run = restarted.finished(run_id)
    assert run["run"]["status"] == "success", run

    # Both steps are recorded under the new run hashes: cached for the current code, and the
    # run's output is the new code's.
    status = barca(project, "status", "pipeline.py", "--json")
    assert status.returncode == 0, status.stderr
    nodes = {n["id"]: n["cache"] for n in json.loads(status.stdout)["nodes"]}
    assert nodes["pipeline.py:a"]["state"] == "cached", nodes
    assert nodes["pipeline.py:b"]["state"] == "cached", nodes
    assert json.loads((project / nodes["pipeline.py:b"]["artifact"]).read_text()) == 200
    assert started(project, "b") == 2, "the stopped attempt and the resumed one"


def test_requeued_attempts_remain_readable_and_local_follower_waits(project, serve):
    """Two drains keep one request, three executions, and one uninterrupted local follower."""
    pipeline = PIPELINE.replace(
        '    hold("long")', '    print("attempt started", flush=True)\n    hold("long")'
    )
    (project / "pipeline.py").write_text(pipeline)
    server = serve("--no-schedule", "--drain-timeout", "0")
    run_id = start_long(server, project)
    accepted = server.request("GET", "/queue")[1]["running"][0]["queued_at"]
    follower = client(project, "logs", run_id, "--follow", "--local")
    wait_until(lambda: "attempt started" in client_output(project, follower)[0], "first log")
    for number in (1, 2):
        assert server.request("POST", "/queue/pause")[0] == 200
        assert server.stop() == 0, server.stderr
        wait_until(
            lambda: "requeued; waiting for the server" in client_output(project, follower)[1],
            "local follower to wait for restart",
        )
        assert follower.poll() is None, client_output(project, follower)
        logs = barca(project, "logs", run_id)
        assert logs.returncode == 0, logs.stderr
        assert logs.stdout.count("attempt started") == number
        assert f"attempt {number} stopped: server restart (requeued)" in logs.stdout
        detail = barca(project, "history", run_id, "--json")
        assert detail.returncode == 0, detail.stderr
        attempts = json.loads(detail.stdout)["run"]["previous_attempts"]
        assert len(attempts) == number
        # Terminal diagnostic evidence belongs to the archived execution, never the
        # synthetic pending view of the same logical request.
        failures = [
            {
                "node_id": "pipeline.py:long",
                "first_error": "worker failed before drain",
                "retry_error": None,
                "recovered": True,
            }
        ]
        with sqlite3.connect(project / ".barca" / "metadata.db") as db:
            db.execute(
                "UPDATE runs SET worker_failures = ? WHERE run_id = ?",
                (json.dumps(failures), attempts[-1]["run_id"]),
            )
        server = serve("--no-schedule", "--drain-timeout", "0")
        queued = server.request("GET", "/queue")[1]["queued"][0]
        assert queued["queued_at"] == accepted
        pending = server.request("GET", f"/runs/{run_id}")[1]
        assert pending["run"]["status"] == "pending"
        assert pending["steps"] == []
        assert pending["explanation"].get("worker_failures", []) == []
        assert len(pending["run"]["previous_attempts"]) == number
        status, recorded = server.request("GET", f"/runs/{run_id}/logs")
        assert status == 200, recorded
        assert sum(line["line"] == "attempt started" for line in recorded["logs"]) == number
        assert server.request("POST", "/queue/resume")[0] == 200
        wait_until(lambda: started(project, "long") == number + 1, "next execution")
        view = server.request("GET", f"/runs/{run_id}")[1]
        assert len(view["run"]["previous_attempts"]) == number
        archived = server.request("GET", f"/runs/{attempts[-1]['run_id']}")[1]
        assert archived["run"]["stop_reason"] == "server restart (requeued)"
        assert archived["explanation"]["worker_failures"] == failures
        assert any(line["line"] == "attempt started" for line in archived["logs"])
        listing = server.request("GET", "/runs")[1]
        assert listing["total"] == 1, listing
    (project / "release-long").touch()
    assert server.finished(run_id)["run"]["status"] == "success"
    assert follower.wait(timeout=RUN) == 0, client_output(project, follower)
    out, err = client_output(project, follower)
    assert out.count("attempt started") == 3, out
    assert "attempt 1 stopped" in out and "attempt 2 stopped" in out
    assert "interrupted" not in err, err


@pytest.mark.parametrize("drain", [False, True])
def test_killed_predecessor_with_unknown_owner_is_recovered(project, serve, drain):
    import sqlite3

    (project / "pipeline.py").write_text(
        PIPELINE.replace(
            '    hold("long")', '    print("before kill", flush=True)\n    hold("long")'
        )
    )
    server = serve("--no-schedule", "--drain-timeout", "60")
    run_id = start_long(server, project)
    if drain:
        server.send(signal.SIGTERM)
        wait_until(lambda: draining(server), "server to drain")
    server.stop(signal.SIGKILL)
    # Server is stopped: emulate a moved volume whose recorded owner cannot be checked.
    with sqlite3.connect(project / ".barca" / "metadata.db") as db:
        db.execute("UPDATE runs SET owner = NULL, pid = NULL, host = 'different-host'")
    restarted = serve("--no-schedule", "--drain-timeout", "60")
    if drain:
        wait_until(lambda: started(project, "long") == 2, "killed drain to requeue once")
        view = restarted.request("GET", f"/runs/{run_id}")[1]
        attempt = view["run"]["previous_attempts"][0]
        archived = restarted.request("GET", f"/runs/{attempt['run_id']}")[1]
        assert archived["run"]["status"] == "interrupted"
        assert any(line["line"] == "before kill" for line in archived["logs"])
        # The existing one-killed-drain retry rule remains bounded.
        restarted.send(signal.SIGTERM)
        wait_until(lambda: draining(restarted), "second drain")
        restarted.stop(signal.SIGKILL)
        with sqlite3.connect(project / ".barca" / "metadata.db") as db:
            db.execute("UPDATE runs SET owner = NULL, pid = NULL, host = 'different-host'")
        restarted = serve("--no-schedule")
    view = restarted.request("GET", f"/runs/{run_id}")[1]
    assert view["run"]["status"] == "interrupted", view
    assert view["run"]["finished_at"] is not None
    assert any(line["line"] == "before kill" for line in view["logs"])
    assert restarted.request("GET", "/queue")[1]["running"] == []


@pytest.mark.parametrize("refresh_all", [False, True])
def test_requeued_failed_key_records_success_under_the_same_hash(project, serve, refresh_all):
    """#929: failure evidence from a stopped attempt must not suppress its retry's success."""
    from .served import snapshot

    (project / "pipeline.py").write_text("""
import time
from pathlib import Path
from barca import asset, partitions

@asset(partitions={"key": partitions(["retry", "held"])})
def item(key: str) -> int:
    with open(f"{key}.starts", "a") as f:
        f.write("start\\n")
    if key == "retry" and not Path("allow-success").exists():
        raise ValueError("transient first-attempt failure")
    if key == "held":
        while not Path("release-held").exists():
            time.sleep(0.02)
    return 42
""")
    server = serve("--no-schedule", "--drain-timeout", "1")
    run_id = server.post(
        "/runs", {"command": "get", "targets": ["item"], "refresh_all": refresh_all}
    )["run_id"]
    wait_until(lambda: started(project, "retry") and started(project, "held"), "both keys to start")
    assert server.stop() == 0, server.stderr
    assert "stopped and put back in the queue" in server.stderr
    with snapshot(project) as db:
        failed = db.execute(
            "select node_id, run_hash from materializations "
            "where run_id in (select run_id from runs where parent_run_id=?) "
            "and status='failed'",
            (run_id,),
        ).fetchall()
    assert len(failed) == 1, failed
    node_id, run_hash = failed[0]
    (project / "allow-success").touch()
    (project / "release-held").touch()
    restarted = serve("--no-schedule")
    run = restarted.finished(run_id)
    assert run["run"]["status"] == "success", run
    assert len(run["run"]["previous_attempts"]) == 1, run
    attempt_id = run["run"]["previous_attempts"][0]["run_id"]
    archived = restarted.request("GET", f"/runs/{attempt_id}")[1]
    assert sum(step["status"] == "failed" for step in archived["steps"]) == 1, archived
    with snapshot(project) as db:
        successes = db.execute(
            "select artifact_path from materializations where run_id=? and node_id=? "
            "and run_hash=? and status='success'",
            (run_id, node_id, run_hash),
        ).fetchall()
    assert len(successes) == 1, run
    assert started(project, "retry") == 2
    again = restarted.finished(restarted.submit("item", "get")["run_id"])
    assert again["run"]["status"] == "success", again
    assert started(project, "retry") == 2, "the successful retry must be cached"
