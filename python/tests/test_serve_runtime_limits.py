"""Runtime limits of `barca serve` (#628): the pool follows the capacity, a capacity shared by
every run of the server, the run timeout, and the stop grace a supervisor must give.

Every serve test runs a real `barca serve` on a free port and drives it over HTTP.
"""

import json
import os
import signal
import subprocess
import textwrap
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import test_serve_queue as queue_tests
from .test_serve_queue import Server, _free_port, wait_for

# Extra environment for the next server started (`Serve(env=...)`).
EXTRA: dict[str, str] = {}

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


def barca(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    scrubbed = {k: v for k, v in os.environ.items() if not k.startswith(queue_tests.SCRUB)}
    scrubbed.update(env)
    return subprocess.run(
        [_find_binary(), *args], cwd=cwd, capture_output=True, text=True, env=scrubbed, timeout=120
    )


class Serve(Server):
    """`Server` with this module's environment: no `BARCA_POOL_SIZE` unless given."""

    def __init__(self, cwd: Path, *args: str, env: dict[str, str] | None = None) -> None:
        EXTRA.clear()
        EXTRA.update(env or {})
        try:
            super().__init__(cwd, _free_port(), *args)
        finally:
            EXTRA.clear()

    def __enter__(self) -> "Serve":
        return self

    def __exit__(self, exc_type, *_exc) -> None:
        self.kill()
        if exc_type is not None:
            print(self.output())


@pytest.fixture(autouse=True)
def _environment(monkeypatch):
    """`Server` starts with `_env()`; drop the pool size it pins, so sizing is barca's own."""

    def env(**extra: str) -> dict[str, str]:
        out = {k: v for k, v in os.environ.items() if not k.startswith(queue_tests.SCRUB)}
        # A test session can itself run under systemd or Kubernetes.
        for name in ("INVOCATION_ID", "KUBERNETES_SERVICE_HOST"):
            out.pop(name, None)
        out.update(EXTRA)
        out.update(extra)
        return out

    monkeypatch.setattr(queue_tests, "_env", env)


def submit(server: Server, target: str) -> str:
    status, body = server.request("POST", "/runs", {"command": "run", "targets": [target]})
    assert status == 202, body
    return body["run_id"]


def queue(server: Server) -> dict:
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
    project = write(
        tmp_path / "p", SIZING, "[execution]\ncapacity = 1\n\n[server]\ncapacity = 4\n"
    )
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
    with Serve(project, "--capacity", "4", "--max-runs", "4") as server:
        # The rebuild has two heavy steps ready at once; weight 3 each, so one at a time.
        rebuild = submit(server, "rebuild")
        wait_for(
            lambda: started(project, "customer_a") + started(project, "customer_b") == 1,
            "a heavy step to start",
        )
        first = "customer_a" if started(project, "customer_a") else "customer_b"
        second = "customer_b" if first == "customer_a" else "customer_a"
        # The light run's steps run beside the heavy one, one after another.
        for _ in range(3):
            run_id = submit(server, "sync")
            assert server.finished(run_id)["status"] == "success"
        assert len((project / "sync.runs").read_text().split()) == 3
        assert started(project, second) == 0, "the heavy steps stay serialized"
        view = queue(server)
        assert view["capacity"]["total"] == 4
        assert view["capacity"]["in_use"] == 3
        assert view["capacity"]["runs"] == [{"run_id": rebuild, "weight": 3}]
        health = server.request("GET", "/health")[1]
        assert health["queue"]["capacity"] == {"total": 4, "in_use": 3, "waiting": 0}
        release(project, first)
        wait_for(lambda: started(project, second) == 1, "the second heavy step")
        release(project, second)
        assert server.finished(rebuild)["status"] == "success"
        assert queue(server)["capacity"]["in_use"] == 0
        print(server.output())


def test_a_heavy_step_is_not_starved_by_light_ones(tmp_path):
    project = write(tmp_path / "p", SHARED)
    with Serve(project, "--capacity", "2", "--max-runs", "4") as server:
        first = submit(server, "light")
        wait_for(lambda: started(project, "light") == 1, "the first light step")
        heavy = submit(server, "heavy")
        wait_for(lambda: queue(server)["capacity"]["waiting"], "the heavy step to wait")
        # One unit is free, and a light step fits it, but it asked after the heavy step.
        later = submit(server, "light_two")
        wait_for(lambda: len(queue(server)["capacity"]["waiting"]) == 2, "the light step to wait")
        time.sleep(1)
        assert started(project, "light_two") == 0, "a light step overtook the heavy one"
        waiting = [w["node"] for w in queue(server)["capacity"]["waiting"]]
        assert waiting == ["pipeline.py:heavy", "pipeline.py:light_two"]
        release(project, "light")
        wait_for(lambda: started(project, "heavy") == 1, "the heavy step to start")
        assert started(project, "light_two") == 0
        release(project, "heavy")
        release(project, "light_two")
        for run_id in (first, heavy, later):
            assert server.finished(run_id)["status"] == "success"
        assert order(project) == ["light", "heavy", "light_two"]


# ─── #628.4: the run timeout ─────────────────────────────────────────────────

TIMEOUT = """
@task()
def slow() -> None:
    hold("slow")

@task(freshness=Schedule("* * * * * *", timeout_seconds=1))
def ticking() -> None:
    hold("ticking")
"""


def assert_timed_out(server: Server, run_id: str, source: str) -> None:
    run = server.finished(run_id)
    assert run["status"] == "error", run
    text = json.dumps(run)
    assert "run timed out after" in text and source in text, text


def test_no_run_timeout_by_default(tmp_path):
    project = write(tmp_path / "p", TIMEOUT)
    with Serve(project, "--no-schedule") as server:
        run_id = submit(server, "slow")
        wait_for(lambda: started(project, "slow"), "the step to start")
        time.sleep(3)
        assert server.run(run_id)["status"] == "running"
        release(project, "slow")
        assert server.finished(run_id)["status"] == "success"


def test_the_run_timeout_flag_stops_a_long_run(tmp_path):
    project = write(tmp_path / "p", TIMEOUT)
    with Serve(project, "--no-schedule", "--run-timeout", "2") as server:
        run_id = submit(server, "slow")
        assert_timed_out(server, run_id, "run timed out after 2s (--run-timeout)")
    history = barca(project, "history", "--json", "--local", BARCA_REMOTE="off")
    assert json.loads(history.stdout)["runs"][0]["status"] == "error", history.stdout
    logs = barca(project, "logs", run_id, "--local", BARCA_REMOTE="off")
    assert "[barca] run cancelled: run timed out after 2s (--run-timeout)" in logs.stdout + (
        logs.stderr
    ), logs


def test_the_server_setting_stops_a_long_run_and_the_flag_wins(tmp_path):
    project = write(tmp_path / "p", TIMEOUT, "[server]\nrun_timeout_seconds = 2\n")
    with Serve(project, "--no-schedule") as server:
        run_id = submit(server, "slow")
        assert_timed_out(
            server, run_id, "run timed out after 2s ([server] run_timeout_seconds)"
        )
    with Serve(project, "--no-schedule", "--run-timeout", "0") as server:
        run_id = submit(server, "slow")
        wait_for(lambda: started(project, "slow") >= 2, "the step to start")
        time.sleep(3)
        assert server.run(run_id)["status"] == "running", "0 is no limit"
        release(project, "slow")
        assert server.finished(run_id)["status"] == "success"


def test_a_schedule_timeout_stops_its_ticks(tmp_path):
    project = write(tmp_path / "p", TIMEOUT, "[server]\nrun_timeout_seconds = 600\n")
    with Serve(project) as server:
        tick = wait_for(
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
    with Serve(project, "--no-schedule", env={"INVOCATION_ID": "abc"}) as server:
        pass
    assert (
        "[barca] stop grace: systemd must allow at least 9s between SIGTERM and SIGKILL "
        "(drain timeout 5s + 4s for stopping runs and closing connections); set "
        "TimeoutStopSec= (default 90s)"
    ) in server.output()
    with Serve(project, "--no-schedule") as server:
        pass
    assert "stop grace" not in server.output()


def test_a_run_killed_during_the_drain_is_put_back_in_the_queue_once(tmp_path):
    project = write(tmp_path / "p", TIMEOUT, "[server]\ndrain_timeout = 60\n")
    port = _free_port()
    first = Server(project, port, "--no-schedule")
    run_id = submit(first, "slow")
    wait_for(lambda: started(project, "slow") == 1, "the step to start")
    # The supervisor's SIGTERM starts the drain; its SIGKILL comes before the drain ends.
    first.proc.send_signal(signal.SIGTERM)
    wait_for(lambda: "draining" in first.output(), "the drain to start")
    time.sleep(0.5)
    first.kill()

    second = Server(project, port, "--no-schedule")
    try:
        assert f"run {run_id} was in flight when the last server was killed during its drain" in (
            second.output()
        )
        wait_for(lambda: started(project, "slow") == 2, "the run to start again")
        # Killed in a drain again: this time it is interrupted, not requeued.
        second.proc.send_signal(signal.SIGTERM)
        wait_for(lambda: "draining" in second.output(), "the second drain")
        time.sleep(0.5)
    finally:
        second.kill()

    third = Server(project, port, "--no-schedule")
    try:
        assert f"run {run_id} was interrupted" in third.output(), third.output()
        run = third.finished(run_id)
        assert run["status"] != "success", run
    finally:
        third.kill()
    release(project, "slow")


def test_a_run_killed_without_a_drain_is_not_restarted(tmp_path):
    project = write(tmp_path / "p", TIMEOUT)
    port = _free_port()
    first = Server(project, port, "--no-schedule")
    run_id = submit(first, "slow")
    wait_for(lambda: started(project, "slow") == 1, "the step to start")
    first.kill()  # SIGKILL with no SIGTERM first: the OOM killer, a crash
    second = Server(project, port, "--no-schedule")
    try:
        assert f"run {run_id} was interrupted" in second.output(), second.output()
        time.sleep(1)
        assert started(project, "slow") == 1
    finally:
        second.kill()
    release(project, "slow")
