"""Runtime limits of `barca serve` (#628): the pool follows the capacity, a capacity shared by
every run of the server, the run timeout, and the stop grace a supervisor must give.

Every serve test runs a real `barca serve` on a free port and drives it over HTTP.
"""

import contextlib
import json
import os
import signal
import subprocess
import textwrap
import time
from pathlib import Path

import pytest

from . import helpers
from .helpers import RUN, clean_env, free_port, wait_until
from .served import Serve

HOLD = """
import os
import time
from pathlib import Path

from barca import Schedule, asset, parallel, task


def hold(name):
    with open(f"{name}.starts", "a") as f:
        f.write(f"{time.time()}\\n")
    deadline = time.time() + 120
    while not Path(f"release-{name}").exists() and time.time() < deadline:
        time.sleep(0.05)
    with open("order", "a") as f:
        f.write(name + "\\n")


def starts(name):
    path = Path(f"{name}.starts")
    return len(path.read_text().splitlines()) if path.exists() else 0
"""


def write(project: Path, body: str, toml: str = "") -> Path:
    project.mkdir(parents=True, exist_ok=True)
    (project / "pipeline.py").write_text(HOLD + textwrap.dedent(body))
    (project / "barca.toml").write_text(textwrap.dedent(toml))
    return project


def started(project: Path, name: str) -> int:
    path = project / f"{name}.starts"
    return len(path.read_text().splitlines()) if path.exists() else 0


def order(project: Path) -> list[str]:
    path = project / "order"
    return path.read_text().split() if path.exists() else []


def release(project: Path, name: str) -> None:
    (project / f"release-{name}").write_text("")


def environment(**extra: str) -> dict[str, str]:
    """A scrubbed shell without a pinned pool size, so sizing is barca's own, and not under a
    supervisor: a test session can itself run under systemd or Kubernetes."""
    env = clean_env()
    for name in ("INVOCATION_ID", "KUBERNETES_SERVICE_HOST"):
        env.pop(name, None)
    env.update(extra)
    return env


def barca(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    return helpers.barca(cwd, *args, env=clean_env(**env), timeout=RUN)


def start_server(cwd: Path, *args: str, port: int | None = None, env: dict[str, str] | None = None):
    """A `barca serve` with this module's environment, which the caller stops."""
    return Serve(cwd, *args, env=environment(**(env or {})), port=port)


@contextlib.contextmanager
def serving(cwd: Path, *args: str, env: dict[str, str] | None = None):
    """[`start_server`], killed on exit; its output is printed when the block fails."""
    started = start_server(cwd, *args, env=env)
    try:
        yield started
    except BaseException:
        print(started.output())
        raise
    finally:
        started.kill()


def submit(server: Serve, target: str) -> str:
    status, body = server.request("POST", "/runs", {"command": "run", "targets": [target]})
    assert status == 202, body
    return body["run_id"]


def queue(server: Serve) -> dict:
    return server.request("GET", "/queue")[1]


# ─── #628.2: the pool follows the capacity ───────────────────────────────────

SIZING = """
@asset()
def a() -> int:
    return 1
"""


def plan_sizing(project: Path, **env: str) -> tuple:
    out = barca(project, "plan", "pipeline.py", **env)
    assert out.returncode == 0, out.stderr
    plan = json.loads(out.stdout)
    return plan["pool_size"], plan["pool_source"], plan["capacity"], plan["capacity_source"]


def test_the_pool_defaults_to_the_capacity_and_pool_size_sets_it(tmp_path):
    project = write(tmp_path / "p", SIZING, "[execution]\ncapacity = 1\n")
    assert plan_sizing(project) == (1, "[execution] capacity", 1, "[execution] capacity")
    # The environment variable wins over the file, as BARCA_CAPACITY does.
    assert plan_sizing(project, BARCA_POOL_SIZE="3") == (
        3,
        "BARCA_POOL_SIZE",
        1,
        "[execution] capacity",
    )
    write(project, SIZING, "[execution]\ncapacity = 1\npool_size = 2\n")
    assert plan_sizing(project) == (2, "[execution] pool_size", 1, "[execution] capacity")
    # Without a capacity, the pool is the cores and the capacity is the pool.
    write(project, SIZING, "[execution]\npool_size = 2\n")
    assert plan_sizing(project) == (2, "[execution] pool_size", 2, "pool size")
    write(project, SIZING, "")
    cores = os.cpu_count()
    pool, source, capacity, _ = plan_sizing(project)
    assert (source, capacity) == ("cores", pool), (pool, cores)

    write(project, SIZING, "[execution]\npool_size = 0\n")
    bad = barca(project, "plan", "pipeline.py")
    assert bad.returncode == 2
    assert "[execution] pool_size" in bad.stderr


def test_doctor_reports_the_pool_size_and_where_it_came_from(tmp_path):
    project = write(tmp_path / "p", SIZING, "[execution]\ncapacity = 1\n\n[server]\ncapacity = 4\n")
    out = barca(project, "doctor", "pipeline.py", "--json", BARCA_REMOTE="off")
    checks = {c["name"]: c for c in json.loads(out.stdout)["checks"]}
    cap = checks["capacity"]
    assert cap["data"]["pool_size"] == 1, cap
    assert cap["data"]["pool_source"] == "[execution] capacity", cap
    assert checks["server_capacity"]["data"]["capacity"] == 4
    assert "pool of 1 worker ([execution] capacity)" in cap["detail"], cap
    assert "[server] capacity" in checks["server_capacity"]["detail"]


PIDS = """
@asset()
def w1() -> int:
    return os.getpid()

@asset()
def w2() -> int:
    return os.getpid()

@asset()
def w3() -> int:
    return os.getpid()

@asset()
def w4() -> int:
    return os.getpid()

@asset(inputs={"a": w1, "b": w2, "c": w3, "d": w4})
def pids(a: int, b: int, c: int, d: int) -> list:
    return sorted({a, b, c, d})


def branch(n: int) -> int:
    return n * 2


@task()
def fan_out() -> list:
    from functools import partial
    return parallel(*(partial(branch, n) for n in range(3)))
"""


def test_a_capacity_of_one_starts_one_worker_and_parallel_still_works(tmp_path):
    project = write(tmp_path / "p", PIDS, "[execution]\ncapacity = 1\n")
    out = barca(project, "get", "pids", "pipeline.py", "-o", "value", BARCA_REMOTE="off")
    assert out.returncode == 0, out.stderr
    assert len(json.loads(out.stdout)) == 1, f"four steps, one worker: {out.stdout}"
    # parallel() freezes the caller and starts a replacement, even with a pool of one.
    out = barca(project, "run", "fan_out", "pipeline.py", "-o", "value", BARCA_REMOTE="off")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout) == [0, 2, 4]


# ─── #628.3: a capacity shared by every run ──────────────────────────────────

SHARED = """
@asset(weight=3)
def customer_a() -> int:
    hold("customer_a")
    return 1

@asset(weight=3)
def customer_b() -> int:
    hold("customer_b")
    return 2

@task(inputs={"a": customer_a, "b": customer_b})
def rebuild(a: int, b: int) -> None:
    pass

@task()
def sync() -> None:
    with open("sync.runs", "a") as f:
        f.write("ran\\n")

@task()
def light() -> None:
    hold("light")

@task()
def light_two() -> None:
    hold("light_two")

@task(weight=2)
def heavy() -> None:
    hold("heavy")
"""


def test_a_light_run_proceeds_while_a_heavy_run_holds_the_rest(tmp_path):
    project = write(tmp_path / "p", SHARED)
    with serving(project, "--capacity", "4", "--max-runs", "4") as server:
        # The rebuild has two heavy steps ready at once; weight 3 each, so one at a time.
        rebuild = submit(server, "rebuild")
        wait_until(
            lambda: started(project, "customer_a") + started(project, "customer_b") == 1,
            "a heavy step to start",
        )
        first = "customer_a" if started(project, "customer_a") else "customer_b"
        second = "customer_b" if first == "customer_a" else "customer_a"
        # The light run's steps run beside the heavy one, one after another.
        for _ in range(3):
            run_id = submit(server, "sync")
            assert server.finished(run_id)["run"]["status"] == "success"
        assert len((project / "sync.runs").read_text().split()) == 3
        assert started(project, second) == 0, "the heavy steps stay serialized"
        view = queue(server)
        assert view["capacity"]["total"] == 4
        assert view["capacity"]["in_use"] == 3
        assert view["capacity"]["runs"] == [{"run_id": rebuild, "weight": 3}]
        health = server.request("GET", "/health")[1]
        assert health["queue"]["capacity"] == {"total": 4, "in_use": 3, "waiting": 0}
        release(project, first)
        wait_until(lambda: started(project, second) == 1, "the second heavy step")
        release(project, second)
        assert server.finished(rebuild)["run"]["status"] == "success"
        assert queue(server)["capacity"]["in_use"] == 0


def test_a_heavy_step_is_not_starved_by_light_ones(tmp_path):
    project = write(tmp_path / "p", SHARED)
    with serving(project, "--capacity", "2", "--max-runs", "4") as server:
        first = submit(server, "light")
        wait_until(lambda: started(project, "light") == 1, "the first light step")
        heavy = submit(server, "heavy")
        wait_until(lambda: queue(server)["capacity"]["waiting"], "the heavy step to wait")
        # One unit is free, and a light step fits it, but it asked after the heavy step.
        later = submit(server, "light_two")
        wait_until(lambda: len(queue(server)["capacity"]["waiting"]) == 2, "the light step to wait")
        time.sleep(1)  # a fixed wait: the light step must not start while the heavy one waits
        assert started(project, "light_two") == 0, "a light step overtook the heavy one"
        waiting = [w["node"] for w in queue(server)["capacity"]["waiting"]]
        assert waiting == ["pipeline.py:heavy", "pipeline.py:light_two"]
        release(project, "light")
        wait_until(lambda: started(project, "heavy") == 1, "the heavy step to start")
        assert started(project, "light_two") == 0
        release(project, "heavy")
        release(project, "light_two")
        for run_id in (first, heavy, later):
            assert server.finished(run_id)["run"]["status"] == "success"
        assert order(project) == ["light", "heavy", "light_two"]


# ─── #1003: requested runs first in the shared capacity's line ───────────────

# A schedule's tick runs s1 (held until released), then s2..s4; a request runs r1..r3. Every
# step but s1 is instant and appends its name to `order` as it ends.
ADMISSION = """
def done(name):
    with open("order", "a") as f:
        f.write(name + "\\n")

@asset(weight=WEIGHT_S)
def s1() -> int:
    hold("s1")
    return 1

@asset(inputs={"s1": s1})
def s2(s1: int) -> int:
    done("s2")
    return s1 + 1

@asset(inputs={"s2": s2})
def s3(s2: int) -> int:
    done("s3")
    return s2 + 1

@asset(inputs={"s3": s3})
def s4(s3: int) -> int:
    done("s4")
    return s3 + 1

@task(inputs={"s4": s4}, freshness=Schedule("* * * * * *"))
def tick(s4: int) -> None:
    assert s4 == 4

@asset(weight=WEIGHT_R)
def r1() -> int:
    done("r1")
    return 1

@asset(weight=WEIGHT_R, inputs={"r1": r1})
def r2(r1: int) -> int:
    done("r2")
    return r1 + 1

@asset(weight=WEIGHT_R, inputs={"r2": r2})
def r3(r2: int) -> int:
    done("r3")
    return r2 + 1
"""


def overlap(
    project: Path, capacity: int, *, weight_s: int = 1, weight_r: int = 1, toml: str = ""
) -> list[str]:
    """A request for r3 made while the first tick's s1 holds the capacity; the order the steps
    ended in, once both runs succeeded."""
    body = ADMISSION.replace("WEIGHT_S", str(weight_s)).replace("WEIGHT_R", str(weight_r))
    write(project, body, toml)
    with serving(project, "--capacity", str(capacity), "--max-runs", "4") as server:
        wait_until(lambda: started(project, "s1") == 1, "the scheduled run's first step")
        status, body = server.request("POST", "/runs", {"command": "get", "targets": ["r3"]})
        assert status == 202, body
        requested = body["run_id"]
        wait_until(
            lambda: [w["run_id"] for w in queue(server)["capacity"]["waiting"]] == [requested],
            "the request to wait for the capacity",
        )
        waiting = queue(server)["capacity"]["waiting"][0]
        assert (waiting["origin"], waiting["node"]) == ("requested", "pipeline.py:r1"), waiting
        release(project, "s1")
        assert server.finished(requested)["run"]["status"] == "success"
        wait_until(lambda: "s4" in order(project), "the scheduled run's last step")
        wait_until(
            lambda: any(
                r["status"] == "success"
                for r in server.get("/history?triggered_by=schedule")["runs"]
            ),
            "the scheduled run to succeed",
        )
        return order(project)


def test_a_request_is_not_interleaved_with_a_scheduled_run(tmp_path):
    # Capacity 1: the request waits only for the scheduled step running when it asked. Under
    # first come, first served the two took turns: s1 r1 s2 r2 s3 r3 s4.
    ended = overlap(tmp_path / "p", 1)
    assert ended[:7] == ["s1", "r1", "r2", "r3", "s2", "s3", "s4"], ended


def test_first_come_takes_turns_as_before(tmp_path):
    ended = overlap(tmp_path / "p", 1, toml='[server]\nadmission = "first_come"\n')
    assert ended[:7] == ["s1", "r1", "s2", "r2", "s3", "r3", "s4"], ended


def test_requested_first_with_a_capacity_above_one_and_heavy_steps(tmp_path):
    # Capacity 2; s1 and the requested steps weigh 2, the other scheduled steps 1. The
    # scheduled run's next step asks as s1 ends and still goes behind the request.
    ended = overlap(tmp_path / "p", 2, weight_s=2, weight_r=2)
    assert ended[:7] == ["s1", "r1", "r2", "r3", "s2", "s3", "s4"], ended


def test_the_queue_shows_the_admission_order_in_effect(tmp_path):
    project = write(tmp_path / "p", "", "[server]\ncapacity = 2\nscheduled_max_wait_seconds = 30\n")
    with serving(project, "--no-schedule") as server:
        capacity = queue(server)["capacity"]
        assert (capacity["admission"], capacity["scheduled_max_wait_seconds"]) == (
            "requested_first",
            30,
        )
    write(project, "", '[server]\ncapacity = 2\nadmission = "first_come"\n')
    with serving(project, "--no-schedule") as server:
        capacity = queue(server)["capacity"]
        assert (capacity["admission"], capacity["scheduled_max_wait_seconds"]) == (
            "first_come",
            None,
        )


def test_admission_settings_are_checked(tmp_path):
    for toml, message in [
        ('admission = "fastest"', '[server] admission: expected "requested_first" or "first_come"'),
        (
            "scheduled_max_wait_seconds = 0",
            "[server] scheduled_max_wait_seconds: expected a whole number",
        ),
        (
            'admission = "first_come"\nscheduled_max_wait_seconds = 60',
            "scheduled_max_wait_seconds applies only to",
        ),
    ]:
        project = write(
            tmp_path / "p",
            "@asset()\ndef a() -> int:\n    return 1\n",
            f"[server]\ncapacity = 2\n{toml}\n",
        )
        result = barca(project, "get", "pipeline.py")
        assert result.returncode == 2, (toml, result.stderr)
        assert message in json.loads(result.stderr.splitlines()[-1])["error"], (toml, result.stderr)


# ─── #628.4: the run timeout ─────────────────────────────────────────────────

TIMEOUT = """
@task()
def slow() -> None:
    hold("slow")

@task(freshness=Schedule("* * * * * *", timeout_seconds=1))
def ticking() -> None:
    hold("ticking")
"""


def assert_timed_out(server: Serve, run_id: str, source: str) -> None:
    run = server.finished(run_id)
    assert run["run"]["status"] == "failed", run
    text = json.dumps(run)
    assert "run timed out after" in text and source in text, text


def test_no_run_timeout_by_default(tmp_path):
    project = write(tmp_path / "p", TIMEOUT)
    with serving(project, "--no-schedule") as server:
        run_id = submit(server, "slow")
        wait_until(lambda: started(project, "slow"), "the step to start")
        time.sleep(3)  # a fixed wait: no timeout may end the run
        assert server.run(run_id)["run"]["status"] == "running"
        release(project, "slow")
        assert server.finished(run_id)["run"]["status"] == "success"


def test_the_run_timeout_flag_stops_a_long_run(tmp_path):
    project = write(tmp_path / "p", TIMEOUT)
    with serving(project, "--no-schedule", "--run-timeout", "2") as server:
        run_id = submit(server, "slow")
        assert_timed_out(server, run_id, "run timed out after 2s (--run-timeout)")
    history = barca(project, "history", "--json", "--local", BARCA_REMOTE="off")
    assert json.loads(history.stdout)["runs"][0]["status"] == "failed", history.stdout
    logs = barca(project, "logs", run_id, "--local", BARCA_REMOTE="off")
    assert "[barca] run cancelled: run timed out after 2s (--run-timeout)" in logs.stdout + (
        logs.stderr
    ), logs


def test_the_server_setting_stops_a_long_run_and_the_flag_wins(tmp_path):
    project = write(tmp_path / "p", TIMEOUT, "[server]\nrun_timeout_seconds = 2\n")
    with serving(project, "--no-schedule") as server:
        run_id = submit(server, "slow")
        assert_timed_out(server, run_id, "run timed out after 2s ([server] run_timeout_seconds)")
    with serving(project, "--no-schedule", "--run-timeout", "0") as server:
        run_id = submit(server, "slow")
        wait_until(lambda: started(project, "slow") >= 2, "the step to start")
        time.sleep(3)  # a fixed wait: no timeout may end the run
        assert server.run(run_id)["run"]["status"] == "running", "0 is no limit"
        release(project, "slow")
        assert server.finished(run_id)["run"]["status"] == "success"


def test_a_schedule_timeout_stops_its_ticks(tmp_path):
    project = write(tmp_path / "p", TIMEOUT, "[server]\nrun_timeout_seconds = 600\n")
    with serving(project) as server:
        tick = wait_until(
            lambda: next(
                (
                    r
                    for r in server.request("GET", "/runs?limit=50")[1]["runs"]
                    if r["status"] not in ("pending", "running")
                ),
                None,
            ),
            "a tick's run to time out",
        )
        assert_timed_out(
            server, tick["run_id"], "run timed out after 1s (Schedule(timeout_seconds=) of ticking)"
        )
        release(project, "ticking")


def test_an_invalid_schedule_timeout_is_a_parse_error(tmp_path):
    project = write(
        tmp_path / "p",
        """
        @task(freshness=Schedule("* * * * *", timeout_seconds="x"))
        def t() -> None:
            pass
        """,
    )
    out = barca(project, "plan", "pipeline.py")
    assert out.returncode == 2
    assert "invalid Schedule timeout_seconds" in out.stderr


# ─── #628.5: the stop grace and a drain the supervisor killed ────────────────


def test_the_startup_prints_the_stop_grace_under_a_supervisor(tmp_path):
    project = write(tmp_path / "p", SIZING, "[server]\ndrain_timeout = 5\n")
    with serving(project, "--no-schedule", env={"INVOCATION_ID": "abc"}) as server:
        pass
    assert (
        "[barca] stop grace: systemd must allow at least 9s between SIGTERM and SIGKILL "
        "(drain timeout 5s + 4s for stopping runs and closing connections); set "
        "TimeoutStopSec= (default 90s)"
    ) in server.stderr
    with serving(project, "--no-schedule") as server:
        pass
    assert "stop grace" not in server.stderr


def test_a_run_killed_during_the_drain_is_put_back_in_the_queue_once(tmp_path):
    project = write(tmp_path / "p", TIMEOUT, "[server]\ndrain_timeout = 60\n")
    port = free_port()
    first = start_server(project, "--no-schedule", port=port)
    run_id = submit(first, "slow")
    wait_until(lambda: started(project, "slow") == 1, "the step to start")
    # The supervisor's SIGTERM starts the drain; its SIGKILL comes before the drain ends.
    first.send(signal.SIGTERM)
    wait_until(lambda: "draining" in first.stderr, "the drain to start")
    time.sleep(0.5)  # a fixed wait: the SIGKILL lands in the middle of the drain
    first.kill()

    second = start_server(project, "--no-schedule", port=port)
    try:
        assert f"run {run_id} was in flight when the last server was killed during its drain" in (
            second.stderr
        )
        wait_until(lambda: started(project, "slow") == 2, "the run to start again")
        # Killed in a drain again: this time it is interrupted, not requeued.
        second.send(signal.SIGTERM)
        wait_until(lambda: "draining" in second.stderr, "the second drain")
        time.sleep(0.5)  # a fixed wait: the SIGKILL lands in the middle of the drain
    finally:
        second.kill()

    third = start_server(project, "--no-schedule", port=port)
    try:
        assert f"run {run_id} was interrupted" in third.stderr, third.stderr
        run = third.finished(run_id)
        assert run["run"]["status"] != "success", run
    finally:
        third.kill()
    release(project, "slow")


def test_a_run_killed_without_a_drain_is_not_restarted(tmp_path):
    project = write(tmp_path / "p", TIMEOUT)
    port = free_port()
    first = start_server(project, "--no-schedule", port=port)
    run_id = submit(first, "slow")
    wait_until(lambda: started(project, "slow") == 1, "the step to start")
    first.kill()  # SIGKILL with no SIGTERM first: the OOM killer, a crash
    second = start_server(project, "--no-schedule", port=port)
    try:
        assert f"run {run_id} was interrupted" in second.stderr, second.stderr
        time.sleep(1)  # a fixed wait: the interrupted run must not start again
        assert started(project, "slow") == 1
    finally:
        second.kill()
    release(project, "slow")


# ─── Where #628's runtime items meet the other 0.31.0 changes ────────────────

MEMORY = """
@asset()
def big() -> int:
    with open("pids", "a") as f:
        f.write(f"{os.getpid()}\\n")
    block = bytearray(300 * 1024 * 1024)
    for i in range(0, len(block), 4096):
        block[i] = 1
    return len(block)


@asset(inputs={"n": big})
def small(n: int) -> int:
    with open("pids", "a") as f:
        f.write(f"{os.getpid()}\\n")
    return n + 1
"""


@pytest.mark.skipif(os.uname().sysname != "Linux", reason="the per-step peak is Linux's")
def test_a_server_capacity_of_one_runs_one_worker_and_each_steps_peak_is_its_own(tmp_path):
    """The pool follows the server capacity (one worker), so both steps share a worker: the
    per-step peak (#627) must still give `small` its own peak, not the 300 MB of `big`."""
    project = write(tmp_path / "p", MEMORY)
    with serving(project, "--capacity", "1") as server:
        status, body = server.request("POST", "/runs", {"command": "get", "targets": ["small"]})
        assert status == 202, body
        view = server.finished(body["run_id"])
        assert view["run"]["status"] == "success", view
        assert len(set((project / "pids").read_text().split())) == 1, "one worker"
        rss = {s["node_id"].split(":")[1]: s["max_rss_bytes"] for s in view["steps"]}
        assert rss["big"] > 300 * 1024 * 1024, rss
        assert rss["small"] < 200 * 1024 * 1024, rss
        assert queue(server)["capacity"]["in_use"] == 0


PHASES = """
@asset(weight=2, cutoff=True)
def gate() -> int:
    hold("gate")
    return 1


@asset(inputs={"g": gate}, weight=2)
def after(g: int) -> int:
    hold("after")
    return g


@task()
def light() -> None:
    hold("light")
"""


def test_a_cutoff_asset_gives_the_shared_capacity_back_between_its_phases(tmp_path):
    """What reads a cutoff asset is planned in a later phase (#636). Between the phases the
    run holds no weight, so a step waiting in the shared capacity's line goes first, and the
    run's next phase queues behind it; nothing is left held at the end."""
    project = write(tmp_path / "p", PHASES)
    with serving(project, "--capacity", "2", "--max-runs", "4") as server:
        status, body = server.request("POST", "/runs", {"command": "get", "targets": ["after"]})
        assert status == 202, body
        chain = body["run_id"]
        wait_until(lambda: started(project, "gate") == 1, "the cutoff asset to start")
        other = submit(server, "light")
        wait_until(lambda: queue(server)["capacity"]["waiting"], "the light step to wait")
        release(project, "gate")
        wait_until(lambda: started(project, "light") == 1, "the light step to start")
        wait_until(lambda: queue(server)["capacity"]["waiting"], "the next phase to wait")
        assert started(project, "after") == 0, "the next phase overtook a waiting step"
        release(project, "light")
        wait_until(lambda: started(project, "after") == 1, "the next phase to start")
        release(project, "after")
        assert server.finished(other)["run"]["status"] == "success"
        assert server.finished(chain)["run"]["status"] == "success"
        assert order(project) == ["gate", "light", "after"]
        capacity = queue(server)["capacity"]
        assert (capacity["in_use"], capacity["waiting"]) == (0, []), capacity
