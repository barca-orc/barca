"""A cached result whose artifact file is gone is a cache miss, not a broken input (#252)."""

import json
import os
import shutil
import subprocess
from pathlib import Path

from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
from barca import asset, task


@asset()
def model() -> dict:
    return {"v": 1}


@asset(inputs={"model": model})
def report(model: dict) -> dict:
    return {"from": model["v"]}


@task(inputs={"model": model})
def publish(model: dict) -> None:
    print("publish", model["v"])
"""


def cli(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base, **env},
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


def statuses(proc: subprocess.CompletedProcess) -> dict[str, str]:
    assert proc.returncode == 0, proc.stderr
    return {s["id"].rsplit(":", 1)[1]: s["status"] for s in json.loads(proc.stdout)["steps"]}


def project(tmp_path: Path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def drop(root: Path, node: str) -> None:
    (path,) = (root / ".barca" / "artifacts").glob(f"*{node}*/*")
    path.unlink()


def test_a_task_recomputes_an_upstream_whose_artifact_was_deleted(tmp_path):
    root = project(tmp_path)
    assert statuses(cli(root, "run", "publish", "--json")) == {"model": "ran", "publish": "ran"}
    drop(root, "model")
    again = cli(root, "run", "publish", "--json")
    assert statuses(again) == {"model": "ran", "publish": "ran"}
    assert "publish 1" in again.stderr
    assert statuses(cli(root, "run", "publish", "--json"))["model"] == "cached"


def test_a_deleted_target_artifact_is_recomputed_and_its_upstream_stays_cached(tmp_path):
    root = project(tmp_path)
    assert statuses(cli(root, "get", "report", "--json")) == {"model": "ran", "report": "ran"}
    drop(root, "report")
    proc = cli(root, "get", "report", "--json")
    assert statuses(proc) == {"model": "cached", "report": "ran"}
    assert json.loads(proc.stdout)["final_output"] == {"from": 1}


def test_a_dry_run_reports_the_step_as_not_cached(tmp_path):
    root = project(tmp_path)
    assert cli(root, "get", "report", "--json").returncode == 0
    drop(root, "model")
    plan = json.loads(cli(root, "get", "report", "--dry-run", "--json").stdout)
    actions = {s["id"].rsplit(":", 1)[1]: s["action"] for s in plan["steps"]}
    assert actions["model"] == "run", plan


def test_with_a_store_a_deleted_local_copy_is_fetched_not_recomputed(tmp_path):
    root = project(tmp_path)
    store = str(tmp_path / "store")
    assert cli(root, "get", "report", "--json", BARCA_REMOTE_URI=store).returncode == 0
    shutil.rmtree(root / ".barca" / "artifacts")
    proc = cli(root, "get", "report", "--json", BARCA_REMOTE_URI=store)
    assert statuses(proc) == {"model": "cached", "report": "cached"}
    assert json.loads(proc.stdout)["final_output"] == {"from": 1}
