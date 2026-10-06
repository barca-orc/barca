"""A step that calls sys.exit() or raises KeyboardInterrupt must fail the run (issue #149).

Steps run in a timeout thread that used to catch only Exception, so SystemExit and
KeyboardInterrupt ended the thread silently: the step was recorded as a success with a None
result, the run exited 0, and for an asset the None was cached.
"""

import json
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
import sys
from barca import asset, task


@task()
def exit_one():
    sys.exit(1)


@task()
def exit_zero():
    sys.exit(0)


@task()
def interrupt():
    raise KeyboardInterrupt


@asset()
def exit_asset() -> dict:
    sys.exit("validation failed")
"""


def _barca(tmp_path: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=120,
    )


@pytest.fixture
def project(tmp_path: Path) -> Path:
    (tmp_path / "p.py").write_text(PIPELINE)
    return tmp_path


@pytest.mark.parametrize("target", ["exit_one", "exit_zero", "interrupt"])
def test_task_exit_fails_run(project: Path, target: str) -> None:
    r = _barca(project, "run", target, "p.py")
    assert r.returncode != 0, r.stdout + r.stderr


def test_sys_exit_message_explains(project: Path) -> None:
    r = _barca(project, "run", "exit_one", "p.py")
    assert "sys.exit()" in r.stderr


def test_asset_sys_exit_fails_and_is_not_cached(project: Path) -> None:
    first = _barca(project, "get", "exit_asset", "p.py")
    assert first.returncode != 0, first.stdout + first.stderr
    # A second run must re-execute (and fail again), not serve a cached None.
    second = _barca(project, "get", "exit_asset", "p.py")
    assert second.returncode != 0, second.stdout + second.stderr
    if second.stdout.strip():
        out = json.loads(second.stdout.strip().splitlines()[-1])
        assert all(s.get("status") != "cached" for s in out.get("steps", []))
