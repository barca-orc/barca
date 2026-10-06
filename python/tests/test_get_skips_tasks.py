"""`barca get <file.py>` with no target materializes assets (and sensors), never tasks (#171).

get is for assets, run is for tasks. Tasks are the side-effecting half of the model (deploy,
notify) and always re-run, so a bare `barca get` must not fire them. Every entry point that
means "get with no target" is checked: the CLI, the `barca <file.py>` shorthand, `--dry-run`,
the Python API, and the HTTP server's `POST /run`. `barca status` is inspection and still lists
tasks.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import time
from pathlib import Path

import pytest

import barca
from barca.api import _find_binary

# Every step leaves a marker file in the working directory, so a test can tell what ran.
MIXED = """
from pathlib import Path
from barca import asset, sensor, task


def mark(name):
    Path(f"ran_{name}").write_text("1")


@sensor()
def feed() -> tuple[bool, str]:
    mark("feed")
    return True, "v1"


@sensor()
def lonely() -> tuple[bool, str]:
    mark("lonely")
    return True, "x"


@asset(inputs={"f": feed})
def src(f: str) -> dict:
    mark("src")
    return {"n": 1, "feed": f}


@asset(inputs={"s": src})
def mid(s: dict) -> dict:
    mark("mid")
    return {"n": s["n"] + 1}


@task(inputs={"m": mid})
def report(m: dict) -> dict:
    mark("report")
    return {"reported": m["n"]}


@task(inputs={"r": report})
def notify(r: dict) -> None:
    mark("notify")
"""

TASKS_ONLY = """
from pathlib import Path
from barca import task


@task()
def deploy() -> None:
    Path("ran_deploy").write_text("1")


@task()
def cleanup() -> None:
    Path("ran_cleanup").write_text("1")
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(MIXED)
    (tmp_path / "tasks.py").write_text(TASKS_ONLY)
    return tmp_path


def barca_cli(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, "BARCA_OUTPUT": "json"},
        capture_output=True,
        text=True,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def names(result: dict) -> set[str]:
    return {s["id"].split(":")[-1] for s in result["steps"]}


def ran(project: Path) -> set[str]:
    return {p.name.removeprefix("ran_") for p in project.glob("ran_*")}


ASSETS_AND_SENSORS = {"feed", "lonely", "src", "mid"}


class TestBareGet:
    def test_skips_tasks(self, project):
        result = ok(barca_cli(project, "get", "pipeline.py"))
        assert names(result) == ASSETS_AND_SENSORS
        assert ran(project) == ASSETS_AND_SENSORS
        # The final value is the last asset's, not a task's.
        assert result["final_output"] == {"n": 2}

    def test_says_which_tasks_it_skipped(self, project):
        proc = barca_cli(project, "get", "pipeline.py")
        assert proc.returncode == 0
        assert "report" in proc.stderr and "notify" in proc.stderr
        assert "barca run report pipeline.py" in proc.stderr

    def test_shorthand_skips_tasks(self, project):
        result = ok(barca_cli(project, "pipeline.py"))
        assert names(result) == ASSETS_AND_SENSORS
        assert ran(project) == ASSETS_AND_SENSORS

    def test_sensor_upstream_of_an_asset_runs(self, project):
        ok(barca_cli(project, "get", "pipeline.py"))
        assert "feed" in ran(project)

    def test_sensor_nothing_depends_on_runs(self, project):
        # `barca get lonely pipeline.py` is valid (get accepts sensors), so bare get, which means
        # "everything get can target", observes it too. Observing is read-only.
        ok(barca_cli(project, "get", "pipeline.py"))
        assert "lonely" in ran(project)

    def test_dry_run_matches(self, project):
        result = ok(barca_cli(project, "get", "pipeline.py", "--dry-run"))
        assert names(result) == ASSETS_AND_SENSORS
        assert ran(project) == set()

    def test_targeted_get_of_an_asset_still_works(self, project):
        result = ok(barca_cli(project, "get", "mid", "pipeline.py"))
        assert names(result) == {"feed", "src", "mid"}

    def test_run_still_runs_the_task(self, project):
        result = ok(barca_cli(project, "run", "notify", "pipeline.py"))
        assert {"report", "notify"} <= names(result)
        assert {"report", "notify"} <= ran(project)


class TestTaskTargetsStillRejected:
    def test_single_task_target_is_usage_error(self, project):
        proc = barca_cli(project, "get", "report", "pipeline.py")
        assert proc.returncode == 2, proc.stderr
        assert "barca run" in proc.stderr
        assert ran(project) == set()

    def test_multi_target_with_a_task_is_usage_error(self, project):
        proc = barca_cli(project, "get", "mid,report", "pipeline.py")
        assert proc.returncode == 2, proc.stderr
        assert "barca run" in proc.stderr
        assert ran(project) == set()


class TestTasksOnlyFile:
    def test_nothing_to_get_exits_zero_and_points_at_run(self, project):
        proc = barca_cli(project, "get", "tasks.py")
        result = ok(proc)
        assert result["steps"] == []
        assert result["steps_executed"] == 0
        assert result["final_output"] is None
        assert ran(project) == set()
        assert "barca run" in proc.stderr
        assert "deploy" in proc.stderr

    def test_dry_run_tasks_only(self, project):
        proc = barca_cli(project, "get", "tasks.py", "--dry-run")
        result = ok(proc)
        assert result["steps"] == []
        assert "barca run" in proc.stderr

    def test_shorthand_tasks_only(self, project):
        proc = barca_cli(project, "tasks.py")
        result = ok(proc)
        assert result["steps"] == []
        assert ran(project) == set()


class TestStatusStillListsTasks:
    def test_status_lists_tasks(self, project):
        proc = barca_cli(project, "status", "pipeline.py", "--json")
        assert proc.returncode == 0, proc.stderr
        result = json.loads(proc.stdout)
        listed = {n["name"] for n in result["nodes"]}
        assert {"report", "notify"} <= listed


class TestPythonApi:
    def test_get_file_skips_tasks(self, project, monkeypatch):
        monkeypatch.chdir(project)
        value = barca.get("pipeline.py")
        assert value == {"n": 2}
        assert ran(project) == ASSETS_AND_SENSORS

    def test_get_tasks_only_file_returns_none(self, project, monkeypatch):
        monkeypatch.chdir(project)
        assert barca.get("tasks.py") is None
        assert ran(project) == set()


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def test_server_full_run_skips_tasks(project):
    from barca.api import BarcaError
    from barca.client import Client

    port = _free_port()
    proc = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
        cwd=project,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        client = Client(f"http://127.0.0.1:{port}")
        for _ in range(50):
            try:
                if client.health().get("status") == "ok":
                    break
            except BarcaError:
                time.sleep(0.2)
        else:
            pytest.skip("server did not come up")

        state = client.get().wait(timeout=60)  # no target: POST /run
        assert state["status"] == "complete", state
        assert names(state["result"]) == ASSETS_AND_SENSORS
        assert ran(project) == ASSETS_AND_SENSORS
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
