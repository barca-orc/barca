"""A durable refresh request keeps completed materializations across drain/restart (#928)."""

import pytest

from .helpers import clean_env, wait_until
from .test_drain_keeps_computed_steps import PIPELINE as PIPELINE


@pytest.mark.parametrize("refresh", [{"refresh_all": True}, {"refresh": ["quick"]}])
@pytest.mark.parametrize("changed", [False, True])
def test_refresh_keeps_completed_asset_after_drain(project, serve, refresh, changed):
    server = serve("--no-schedule", "--drain-timeout", "1", env=clean_env(BARCA_POOL_SIZE="2"))
    response = server.post("/runs", {"command": "get", "targets": ["slow"], **refresh})
    run_id = response["run_id"]
    wait_until((project / "slow.started").exists, "the dependent step to start")
    assert server.stop() == 0, server.stderr
    assert f"run {run_id} stopped and put back in the queue" in server.stderr
    (project / "release").write_text("")
    if changed:
        source = project / "pipeline.py"
        source.write_text(source.read_text().replace('return {"n": 1}', 'return {"n": 2}'))
    restarted = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    result = restarted.finished(run_id)
    assert result["status"] == "success", result
    assert (project / "slow.runs").read_text() == "xx"
    assert (project / "quick.runs").read_text() == ("xx" if changed else "x")
    if not changed:
        quick = next(s for s in result["outcome"]["steps"] if s["id"].endswith(":quick"))
        assert quick["reason"] == "refreshed_by_this_run"



PARTITIONED = '''
import time
from pathlib import Path
from barca import asset, collect, partitions

@asset(partitions={"key": partitions(["a", "b"])})
def quick(key: str) -> str:
    with Path(key + ".runs").open("a") as log:
        log.write("x")
    if key == "b":
        Path("slow.started").touch()
        while not Path("release").exists():
            time.sleep(0.02)
    return key

@asset(inputs={"parts": collect(quick)})
def slow(parts: list) -> list:
    return parts
'''


@pytest.mark.parametrize("refresh", [{"refresh_all": True}, {"refresh": ["quick"]}])
def test_partition_refresh_keeps_only_completed_keys(tmp_path, serve, refresh):
    (tmp_path / "pipeline.py").write_text(PARTITIONED)
    server = serve("--no-schedule", "--drain-timeout", "1", env=clean_env(BARCA_POOL_SIZE="2"))
    run_id = server.post("/runs", {"command": "get", "targets": ["slow"], **refresh})["run_id"]
    wait_until((tmp_path / "slow.started").exists, "key b to start")
    wait_until((tmp_path / "a.runs").exists, "key a to finish")
    assert server.stop() == 0, server.stderr
    (tmp_path / "release").touch()
    restarted = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    assert restarted.finished(run_id)["status"] == "success"
    assert (tmp_path / "a.runs").read_text() == "x"
    assert (tmp_path / "b.runs").read_text() == "xx"


def test_new_cli_refresh_recomputes_an_earlier_success(project):
    from .helpers import barca

    (project / "release").touch()
    for refresh in ["--refresh-all", "--refresh=quick", "--refresh-all"]:
        result = barca(project, "get", "slow", "pipeline.py", refresh, "--local", "--json", env=clean_env(BARCA_POOL_SIZE="2"))
        assert result.returncode == 0, result.stderr
    assert (project / "quick.runs").read_text() == "xxx"
    assert (project / "slow.runs").read_text() == "xxx"


def test_queued_refresh_accepts_another_runs_success(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text('''
import time
from pathlib import Path
from barca import asset, task

@task()
def hold() -> None:
    Path("holding").touch()
    while not Path("release").exists():
        time.sleep(0.02)

@asset()
def quick() -> int:
    with Path("quick.runs").open("a") as log:
        log.write("x")
    return 1
''')
    server = serve("--no-schedule", "--max-runs", "1", env=clean_env(BARCA_POOL_SIZE="2"))
    held = server.post("/runs", {"command": "run", "targets": ["hold"], "queue": "last"})
    wait_until((tmp_path / "holding").exists, "the scheduled slot to be occupied")
    waiting = server.post(
        "/runs", {"command": "get", "targets": ["quick"], "refresh_all": True, "queue": "last"}
    )
    other = server.post("/runs", {"command": "get", "targets": ["quick"]})
    assert server.finished(other["run_id"])["status"] == "success"
    (tmp_path / "release").touch()
    assert server.finished(held["run_id"])["status"] == "success"
    result = server.finished(waiting["run_id"])
    assert result["status"] == "success", result
    assert (tmp_path / "quick.runs").read_text() == "x"


def test_completed_tasks_can_repeat_after_drain(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(PIPELINE.replace("asset", "task"))
    server = serve("--no-schedule", "--drain-timeout", "1", env=clean_env(BARCA_POOL_SIZE="2"))
    run_id = server.post("/runs", {"command": "run", "targets": ["slow"]})["run_id"]
    wait_until((tmp_path / "slow.started").exists, "the dependent task to start")
    assert server.stop() == 0, server.stderr
    (tmp_path / "release").touch()
    restarted = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    assert restarted.finished(run_id)["status"] == "success"
    assert (tmp_path / "quick.runs").read_text() == "xx"
