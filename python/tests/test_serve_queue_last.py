"""`queue: "last"` of `POST /runs` and `--queue last` of `barca get` / `barca run`: a requested
run that gives up its place ahead of scheduled work.

By default a run someone asked for starts ahead of the scheduled runs that wait, behind the
requests accepted before it, and its steps go ahead of scheduled steps in the shared capacity's
line. A run queued last is ordered with scheduled work instead, in both places: it waits for a
run slot behind every run accepted before it, scheduled ones included, and its steps wait for
capacity as a scheduled run's do, the aging rule included.

Every test runs a real `barca serve`.
"""

import json
import signal
import subprocess
import textwrap
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import helpers
from .helpers import RUN, clean_env, wait_until
from .served import Serve

HELPERS = """
import time
from pathlib import Path

from barca import Schedule, asset, task


def hold(name):
    with open(f"{name}.starts", "a") as f:
        f.write("start\\n")
    deadline = time.time() + 120
    while not Path(f"release-{name}").exists() and time.time() < deadline:
        time.sleep(0.05)
    mark(name)


def mark(name):
    with open("order", "a") as f:
        f.write(name + "\\n")
"""

TASKS = """
@task()
def long() -> None:
    hold("long")

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

TICKERS = """
@task(freshness=Schedule("* * * * * *"))
def tick_a() -> None:
    mark("tick_a")

@task(freshness=Schedule("* * * * * *"))
def tick_b() -> None:
    mark("tick_b")
"""


def write(project: Path, body: str, toml: str = "") -> Path:
    (project / "pipeline.py").write_text(HELPERS + textwrap.dedent(body))
    (project / "barca.toml").write_text(textwrap.dedent(toml))
    return project


@pytest.fixture()
def project(tmp_path) -> Path:
    root = tmp_path / "p"
    root.mkdir()
    return write(root, TASKS)


@pytest.fixture()
def serve(project, serve):
    def start(*args: str, **options) -> Serve:
        return serve(*args, root=project, env=clean_env(BARCA_POOL_SIZE="2"), **options)

    return start


def barca(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    return helpers.barca(cwd, *args, env=clean_env(BARCA_POOL_SIZE="2", **env), timeout=RUN)


def order(project: Path) -> list[str]:
    path = project / "order"
    return path.read_text().split() if path.exists() else []


def started(project: Path, name: str) -> bool:
    return (project / f"{name}.starts").exists()


def release(project: Path, name: str) -> None:
    (project / f"release-{name}").write_text("")


def submit(server: Serve, target: str, *, command: str = "run", **more) -> dict:
    body = {"command": command, "targets": [target], **more}
    status, answer = server.request("POST", "/runs", body)
    assert status == 202, answer
    return answer


def queue(server: Serve) -> dict:
    return server.request("GET", "/queue")[1]


def names(entries: list[dict]) -> list[str]:
    """Each entry's target, as its short name."""
    return [(e["target"] or "").rsplit(":", 1)[-1] for e in entries]


# ─── a run slot ──────────────────────────────────────────────────────────────


def test_a_run_queued_last_starts_with_the_scheduled_runs_in_the_order_accepted(project, serve):
    """The rule, with one run slot. Accepted in this order: a tick of `tick_a`, a request
    (`one`), a request queued last (`three`), a later request (`two`), a tick of `tick_b`.
    They start: `one`, `two` (requests, in the order accepted), then `tick_a`, `three`,
    `tick_b` (scheduled runs and the run queued last, together, in the order accepted)."""
    write(project, TASKS + TICKERS)
    server = serve("--max-runs", "1")
    # tick_b fires again only when it is resumed below, after `three` is accepted.
    assert server.request("POST", "/schedule/tick_b/pause")[0] == 200
    wait_until(
        lambda: "tick_b" not in names(queue(server)["running"] + queue(server)["queued"]),
        "tick_b's last run to end",
    )
    assert server.request("POST", "/queue/pause")[0] == 200
    wait_until(
        lambda: (q := queue(server))["running"] == [] and names(q["queued"]) == ["tick_a"],
        "one tick of tick_a to wait in the paused queue",
    )
    before = len(order(project))

    first = submit(server, "one")
    last = submit(server, "three", queue="last")
    second = submit(server, "two", queue="next")
    assert (first["position"], first["queue"]) == (1, None), first
    # Behind the tick that was already waiting, and the answer says why.
    assert (last["position"], last["queue"], last["status"]) == (3, "last", "pending"), last
    # A default request goes ahead of both, behind the request before it.
    assert (second["position"], second["queue"]) == (2, None), second

    assert server.request("POST", "/schedule/tick_b/resume")[0] == 200
    wait_until(lambda: "tick_b" in names(queue(server)["queued"]), "a tick of tick_b to queue")
    waiting = queue(server)["queued"]
    assert names(waiting) == ["one", "two", "tick_a", "three", "tick_b"], waiting
    assert [e["position"] for e in waiting] == [1, 2, 3, 4, 5], waiting
    assert [e["queue"] for e in waiting] == [None, None, None, "last", None], waiting
    assert [e["triggered_by"] for e in waiting] == ["api", "api", "schedule", "api", "schedule"]
    assert server.run(last["run_id"])["run"]["queue"] == "last"
    assert server.run(first["run_id"])["run"]["queue"] is None

    # `barca queue`: the same key in --json, and the row says so in the table.
    listed = barca(project, "queue", "--server", server.url, "--json")
    assert listed.returncode == 0, listed.stderr
    assert [e["queue"] for e in json.loads(listed.stdout)["queued"]] == [
        None,
        None,
        None,
        "last",
        None,
    ]
    table = barca(project, "queue", "--server", server.url, "--pretty")
    assert table.returncode == 0, table.stderr
    marked = [line for line in table.stdout.splitlines() if "queued last" in line]
    assert len(marked) == 1 and last["run_id"] in marked[0], table.stdout

    assert server.request("POST", "/queue/resume")[0] == 200
    wait_until(lambda: len(order(project)) >= before + 5, "the five runs to start")
    assert order(project)[before : before + 5] == ["one", "two", "tick_a", "three", "tick_b"]
    assert server.finished(last["run_id"])["run"]["status"] == "success"
    # Not recorded with the run: the key is `null` once it has finished.
    assert server.run(last["run_id"])["run"]["queue"] is None


def test_a_run_queued_last_takes_no_extra_slot_and_a_request_runs_past_it(project, serve):
    """A default request uses one slot past `max_runs` while scheduled work holds the slots.
    A run queued last is that kind of work: it takes no extra slot, and a request made while
    it runs does."""
    server = serve("--no-schedule", "--max-runs", "1")
    held = submit(server, "long", queue="last")
    wait_until(lambda: started(project, "long"), "the run queued last to hold the slot")
    assert held["status"] == "running", "a free slot: it starts at once"
    # Another run queued last waits for the slot.
    waiting = submit(server, "one", queue="last")
    assert (waiting["status"], waiting["position"]) == ("pending", 1), waiting
    # A default request starts beside the run queued last, in the extra slot.
    asked = submit(server, "two")
    assert server.finished(asked["run_id"])["run"]["status"] == "success"
    assert order(project) == ["two"]
    assert names(queue(server)["queued"]) == ["one"]
    release(project, "long")
    assert server.finished(waiting["run_id"])["run"]["status"] == "success"
    assert order(project) == ["two", "long", "one"]


STEADY = """
@task(freshness=Schedule("* * * * * *"))
def s_a() -> None:
    mark("s_a")
    time.sleep(0.6)

@task(freshness=Schedule("* * * * * *"))
def s_b() -> None:
    mark("s_b")
    time.sleep(0.6)

@task(freshness=Schedule("* * * * * *"))
def s_c() -> None:
    mark("s_c")
    time.sleep(0.6)
"""


def test_a_run_queued_last_starts_under_a_steady_stream_of_scheduled_runs(project, serve):
    """Three schedules fire every second and each run takes 0.6 s, so the one slot is always
    busy and scheduled runs always wait. A run queued last still starts: it is first come,
    first served among the scheduled runs, so only the runs accepted before it start first."""
    write(project, TASKS + STEADY)
    server = serve("--max-runs", "1")
    wait_until(
        lambda: len(queue(server)["queued"]) >= 2 and queue(server)["running"],
        "scheduled runs to wait behind a scheduled run",
    )
    # Nothing starts between counting what is ahead and joining behind it.
    assert server.request("POST", "/queue/pause")[0] == 200
    wait_until(lambda: queue(server)["running"] == [], "the run in flight to end")
    ahead = len(queue(server)["queued"])
    before = len(order(project))
    last = submit(server, "one", queue="last")
    assert last["position"] == ahead + 1, last
    assert server.request("POST", "/queue/resume")[0] == 200
    assert server.finished(last["run_id"])["run"]["status"] == "success"
    ran = order(project)[before:]
    assert ran.index("one") == ahead, (ahead, ran)
    wait_until(lambda: len(order(project)) > before + ahead + 2, "the schedules to carry on")


# ─── a restart ───────────────────────────────────────────────────────────────


def test_the_place_survives_a_restart_while_the_run_waits(project, serve):
    port = helpers.free_port()
    server = serve("--no-schedule", "--max-runs", "1", port=port)
    long = submit(server, "long")
    wait_until(lambda: started(project, "long"), "the long step to start")
    last = submit(server, "one", queue="last")
    asked = submit(server, "two")
    assert names(queue(server)["queued"]) == ["two", "one"]
    # Paused, so the restarted server shows the queue before it starts anything.
    assert server.request("POST", "/queue/pause")[0] == 200

    server.send(signal.SIGTERM)
    wait_until(lambda: server.get("/health")["status"] == "draining", "draining")
    release(project, "long")
    assert server.exited() == 0, server.stderr

    restarted = serve("--no-schedule", "--max-runs", "1", port=port)
    assert "[barca] resuming 2 queued runs" in restarted.stderr, restarted.stderr
    waiting = queue(restarted)["queued"]
    assert names(waiting) == ["two", "one"], waiting
    assert [e["queue"] for e in waiting] == [None, "last"], waiting
    assert restarted.run(last["run_id"])["run"]["queue"] == "last"
    assert restarted.request("POST", "/queue/resume")[0] == 200
    assert restarted.finished(last["run_id"])["run"]["status"] == "success"
    assert restarted.finished(asked["run_id"])["run"]["status"] == "success"
    assert order(project) == ["long", "two", "one"], "accepted: long, one (last), two"
    assert restarted.run(long["run_id"])["run"]["status"] == "success"


def test_a_read_only_server_lists_the_recorded_queue_in_the_same_order(project, serve):
    server = serve("--no-schedule", "--max-runs", "1")
    submit(server, "long")
    wait_until(lambda: started(project, "long"), "the long step to start")
    submit(server, "one", queue="last")
    submit(server, "two")
    reader = serve("--read-only")
    recorded = wait_until(
        lambda: len((q := queue(reader))["queued"]) == 2 and q, "the recorded queue"
    )
    assert names(recorded["queued"]) == ["two", "one"], recorded
    assert [e["queue"] for e in recorded["queued"]] == [None, "last"], recorded
    assert [e["position"] for e in recorded["queued"]] == [1, 2], recorded
    release(project, "long")


# ─── step admission under a shared capacity ──────────────────────────────────

# A schedule's tick runs s1 (held until released), then s2 and s3; a request runs r1..r3 and
# another q1..q3. Every step appends its name to `order` as it ends.
ADMISSION = """
@asset()
def s1() -> int:
    hold("s1")
    return 1

@asset(inputs={"s1": s1})
def s2(s1: int) -> int:
    mark("s2")
    return s1 + 1

@asset(inputs={"s2": s2})
def s3(s2: int) -> int:
    mark("s3")
    return s2 + 1

@task(inputs={"s3": s3}, freshness=Schedule("* * * * * *"))
def tick(s3: int) -> None:
    assert s3 == 3

"""

# Two requests' chains: r1..r3 and q1..q3.
CHAINS = """
@asset()
def r1() -> int:
    mark("r1")
    return 1

@asset(inputs={"r1": r1})
def r2(r1: int) -> int:
    mark("r2")
    return r1 + 1

@asset(inputs={"r2": r2})
def r3(r2: int) -> int:
    mark("r3")
    return r2 + 1

@asset()
def q1() -> int:
    mark("q1")
    return 1

@asset(inputs={"q1": q1})
def q2(q1: int) -> int:
    mark("q2")
    return q1 + 1

@asset(inputs={"q2": q2})
def q3(q2: int) -> int:
    mark("q3")
    return q2 + 1
"""


def waiting_steps(server: Serve) -> list[tuple[str, str]]:
    return [
        (w["node"].rsplit(":", 1)[1], w["origin"]) for w in queue(server)["capacity"]["waiting"]
    ]


def test_the_steps_of_a_run_queued_last_wait_for_capacity_as_scheduled_steps_do(project, serve):
    """Capacity 1, held by the scheduled run's s1. A run queued last (q3) asks, then a default
    request (r3). The request's steps go first; then the scheduled run's remaining steps
    (its run asked first); then the steps of the run queued last. Without `queue: "last"`,
    q1..q3 would run before s2 (`test_serve_runtime_limits.py`)."""
    write(project, ADMISSION + CHAINS)
    server = serve("--capacity", "1", "--max-runs", "4")
    wait_until(lambda: started(project, "s1"), "the scheduled run's first step")
    last = submit(server, "q3", command="get", queue="last")
    assert last["status"] == "running", "a free run slot: it starts, and its steps wait"
    wait_until(lambda: waiting_steps(server) == [("q1", "scheduled")], "q1 to wait")
    asked = submit(server, "r3", command="get")
    # The line, in the order it is admitted: the request ahead of the run queued last.
    wait_until(
        lambda: waiting_steps(server) == [("r1", "requested"), ("q1", "scheduled")],
        "r1 to wait ahead of q1",
    )
    running = {e["run_id"]: e["queue"] for e in queue(server)["running"]}
    assert (running[last["run_id"]], running[asked["run_id"]]) == ("last", None), running
    release(project, "s1")
    assert server.finished(asked["run_id"])["run"]["status"] == "success"
    assert server.finished(last["run_id"])["run"]["status"] == "success"
    assert order(project)[:9] == ["s1", "r1", "r2", "r3", "s2", "s3", "q1", "q2", "q3"]


AGING = """
@task()
def holder() -> None:
    hold("holder")
"""


def test_a_step_queued_last_that_has_waited_long_enough_goes_ahead_of_requests(project, serve):
    """The aging rule of scheduled steps applies: after `scheduled_max_wait_seconds` in line,
    the step of a run queued last is admitted before a requested one, so requests cannot
    starve it. Its next step asks afresh, behind the request again."""
    write(project, CHAINS + AGING, "[server]\nscheduled_max_wait_seconds = 2\n")
    server = serve("--no-schedule", "--capacity", "1", "--max-runs", "4")
    submit(server, "holder")
    wait_until(lambda: started(project, "holder"), "the requested step that holds the capacity")
    last = submit(server, "q3", command="get", queue="last")
    wait_until(lambda: waiting_steps(server) == [("q1", "scheduled")], "q1 to wait")
    time.sleep(2.5)  # a fixed wait: q1 has now waited longer than the bound
    asked = submit(server, "r3", command="get")
    wait_until(
        lambda: waiting_steps(server) == [("q1", "scheduled"), ("r1", "requested")],
        "the aged step to be first in line",
    )
    release(project, "holder")
    assert server.finished(asked["run_id"])["run"]["status"] == "success"
    assert server.finished(last["run_id"])["run"]["status"] == "success"
    assert order(project) == ["holder", "q1", "r1", "r2", "r3", "q2", "q3"]


# ─── the request ─────────────────────────────────────────────────────────────


def test_an_unknown_place_is_refused(project, serve):
    server = serve("--no-schedule")
    status, body = server.request(
        "POST", "/runs", {"command": "run", "targets": ["one"], "queue": "first"}
    )
    assert status == 400, body
    assert "unknown variant `first`, expected `next` or `last`" in body["error"], body
    assert queue(server)["queued"] == [] and order(project) == []


# ─── the CLI ─────────────────────────────────────────────────────────────────


def client(cwd: Path, *args: str, **env: str) -> subprocess.Popen:
    return subprocess.Popen(
        [_find_binary(), *args],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=clean_env(BARCA_POOL_SIZE="2", **env),
    )


def test_the_flag_reaches_the_server_by_each_route(project, serve, tmp_path):
    """`--server URL`, a configured server with a bare `--server`, and the project's own server
    through its socket: each `--queue last` is the request's `queue: "last"`."""
    server = serve("--no-schedule", "--max-runs", "1")
    assert server.request("POST", "/queue/pause")[0] == 200
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    clients = [
        # Named by URL, from a directory that is not the project.
        client(elsewhere, "run", "one", "--server", server.url, "--queue", "last"),
        # The configured server, run remotely.
        client(elsewhere, "run", "two", "--server", "--queue=last", BARCA_SERVER=server.url),
        # In the project: the server's socket, no flag naming a server.
        client(project, "run", "three", "--queue", "last"),
        # The default, spelled out.
        client(project, "run", "one", "--queue", "next"),
    ]
    try:
        waiting = wait_until(
            lambda: len(q := queue(server)["queued"]) == 4 and q, "the four runs to queue"
        )
        # The default request is first; the three queued last follow in the order accepted.
        assert [e["queue"] for e in waiting] == [None, "last", "last", "last"], waiting
        assert names(waiting)[0] == "one"
        assert sorted(names(waiting)[1:]) == ["one", "three", "two"]
        assert server.request("POST", "/queue/resume")[0] == 200
        for proc in clients:
            out, err = proc.communicate(timeout=RUN)
            assert proc.returncode == 0, err
            assert json.loads(out)["status"] == "success", out
        assert ".barca/serve.sock" in err, "the last client went through the socket"
    finally:
        for proc in clients:
            if proc.poll() is None:
                proc.kill()
                proc.wait()


def usage(done: subprocess.CompletedProcess) -> dict:
    assert done.returncode == 2, (done.returncode, done.stderr)
    assert done.stdout == ""
    envelope = json.loads(done.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "usage", envelope
    return envelope


def test_the_flag_is_a_usage_error_where_the_command_runs_on_this_machine(project, serve, tmp_path):
    wanted = "--queue places a run in a barca server's run queue"
    # No server at all.
    for command in (("get", "one", "pipeline.py"), ("run", "one", "pipeline.py")):
        for place in ("last", "next"):
            envelope = usage(barca(project, *command, "--queue", place))
            assert wanted in envelope["error"], envelope
            assert "--server" in envelope["remediation"], envelope
    assert order(project) == [], "nothing ran"
    server = serve("--no-schedule")
    # Beside the project's server, `--local` runs here.
    envelope = usage(barca(project, "run", "one", "pipeline.py", "--local", "--queue", "last"))
    assert wanted in envelope["error"], envelope
    # A configured server without `--server` is this run's ledger: the run is still here.
    other = tmp_path / "other"
    other.mkdir()
    write(other, TASKS)
    envelope = usage(
        barca(other, "run", "one", "pipeline.py", "--queue", "last", BARCA_SERVER=server.url)
    )
    assert wanted in envelope["error"], envelope
    assert order(project) == [] and order(other) == [], "nothing ran"
    # Not a place: clap's own usage error, naming the values.
    bad = barca(project, "run", "one", "--queue", "first")
    assert bad.returncode == 2, bad.stderr
    assert "next" in bad.stderr and "last" in bad.stderr, bad.stderr
