"""Runs of one `barca serve` share the asset steps they are computing (#654).

Two runs that need the same asset step at the same time (the same node, partition key and run
hash) compute it once: the run that reaches it second waits for the first and takes its result
as a cache hit. Tasks are never shared, keys the runs do not have in common run concurrently,
and a run whose owner fails or is cancelled executes the step itself.

Every step holds until the test releases it, so which run reaches a step first is decided by the
test, not by timing.
"""

import json
import os
import time
from pathlib import Path

import pytest

from .helpers import barca, clean_env, wait_until
from .served import Serve

PIPELINE = """
import os
import time
from pathlib import Path

from barca import asset, partitions, task


def hold(name):
    fd = os.open("calls", os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (name + "\\n").encode())
    finally:
        os.close(fd)
    deadline = time.time() + 120
    while not Path(f"release-{name}").exists() and time.time() < deadline:
        time.sleep(0.05)
    if Path(f"fail-{name}").exists():
        Path(f"fail-{name}").unlink()
        raise RuntimeError(f"{name} failed")


@asset()
def shared() -> int:
    hold("shared")
    return 42


@asset(inputs={"shared": shared})
def below(shared: int) -> int:
    return shared + 1


@task()
def explicit() -> None:
    hold("explicit")


@asset(partitions={"k": partitions(["1", "2", "3"])})
def part(k: str) -> str:
    hold(f"part-{k}")
    return k
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


@pytest.fixture()
def server(project, serve):
    return serve("--no-schedule", "--max-runs", "4", env=clean_env(BARCA_POOL_SIZE="2"))


def calls(project: Path) -> list[str]:
    path = project / "calls"
    return path.read_text().split() if path.exists() else []


def submit(server: Serve, command: str, target: str, partition: str | None = None) -> str:
    body: dict = {"command": command, "targets": [target]}
    if partition is not None:
        body["partition"] = partition
    status, run = server.request("POST", "/runs", body)
    assert status == 202, run
    return run["run_id"]


def release(project: Path, *names: str) -> None:
    for name in names:
        (project / f"release-{name}").write_text("")


def steps(server: Serve, run_id: str) -> dict[str, dict]:
    """The step reports of a finished run, by node id."""
    run = server.finished(run_id)
    assert run["status"] == "success", run
    outcome = run.get("outcome") or run.get("result") or run
    return {s["id"]: s for s in outcome["steps"]}


def settle() -> None:
    """Give a run just submitted time to plan and reach the step it waits for."""
    # A fixed wait: a run blocked on another run's step is not observable from outside.
    time.sleep(1.5)


def test_two_runs_of_the_same_uncached_asset_compute_it_once(project, server):
    first = submit(server, "get", "shared")
    wait_until(lambda: calls(project) == ["shared"], "the first run to start shared")
    second = submit(server, "get", "below")
    settle()
    release(project, "shared")

    assert steps(server, first)["pipeline.py:shared"]["status"] == "ran"
    reported = steps(server, second)
    assert reported["pipeline.py:shared"]["status"] == "cached", reported
    assert reported["pipeline.py:below"]["status"] == "ran", reported
    assert calls(project) == ["shared"], "computed once, for both runs"


def test_two_runs_of_a_task_run_it_twice(project, server):
    first = submit(server, "run", "explicit")
    second = submit(server, "run", "explicit")
    wait_until(lambda: calls(project).count("explicit") == 2, "both runs to start the task")
    release(project, "explicit")
    for run in (first, second):
        assert server.finished(run)["status"] == "success"


def test_runs_share_only_the_partition_keys_they_have_in_common(project, server):
    first = submit(server, "get", "part", partition="k=1")
    wait_until(lambda: calls(project) == ["part-1"], "the first run to start key 1")
    second = submit(server, "get", "part")
    # The keys the first run does not compute start in the second at once.
    wait_until(
        lambda: sorted(calls(project)) == ["part-1", "part-2", "part-3"],
        "the second run to start keys 2 and 3",
    )
    release(project, "part-1", "part-2", "part-3")
    assert server.finished(first)["status"] == "success"
    reported = steps(server, second)["pipeline.py:part"]
    assert reported["partitions"]["cached"] == 1, reported
    assert reported["partitions"]["will_run"] == 2, reported
    assert sorted(calls(project)) == ["part-1", "part-2", "part-3"], "key 1 once"


def test_a_run_waiting_on_a_step_that_fails_executes_it_itself(project, server):
    (project / "fail-shared").write_text("")
    first = submit(server, "get", "shared")
    wait_until(lambda: calls(project) == ["shared"], "the first run to start shared")
    second = submit(server, "get", "shared")
    settle()
    release(project, "shared")

    assert server.finished(first)["status"] == "failed"
    assert steps(server, second)["pipeline.py:shared"]["status"] == "ran"
    assert calls(project) == ["shared", "shared"], "the failed attempt and the waiter's own"


def test_a_run_waiting_on_a_cancelled_run_executes_the_step_itself(project, server):
    first = submit(server, "get", "shared")
    wait_until(lambda: calls(project) == ["shared"], "the first run to start shared")
    second = submit(server, "get", "shared")
    settle()
    status, body = server.request("DELETE", f"/runs/{first}")
    assert status in (200, 202), body
    assert server.finished(first)["status"] == "cancelled"
    wait_until(lambda: calls(project) == ["shared", "shared"], "the waiter to start shared itself")
    release(project, "shared")
    assert steps(server, second)["pipeline.py:shared"]["status"] == "ran"


def test_the_shared_result_is_recorded_once(project, server):
    first = submit(server, "get", "shared")
    wait_until(lambda: calls(project) == ["shared"], "the first run to start shared")
    second = submit(server, "get", "shared")
    settle()
    release(project, "shared")
    steps(server, first)
    steps(server, second)
    status = barca(project, "status", "pipeline.py", "--json", env=dict(os.environ))
    assert status.returncode == 0, status.stderr
    node = next(n for n in json.loads(status.stdout)["nodes"] if n["id"] == "pipeline.py:shared")
    assert node["cache"]["state"] == "cached", node
