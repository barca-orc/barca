"""A step error says where the step failed (#664).

A failure in barca's own code around a step (reading its inputs, splitting a batched call's
result, writing its result or its sinks) used to be reported like an exception in the step's
code with `"traceback": null`: the frames that were dropped as barca-internal were the only
ones there were. Now every step error carries a `phase`; a failure outside `user_code` keeps
its full traceback, and the text says it failed in barca.
"""

import json
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset, partitions, sink


@asset()
def src() -> dict:
    return {"x": 1}


@asset(inputs={"s": src})
def reads_src(s: dict) -> int:
    return s["x"]


@asset()
def oops() -> float:
    return 1 / 0


@asset()
def unwritable():
    return lambda: 1


@asset()
@sink("/dev/null/cannot/write/here.json")
def sinky() -> dict:
    return {"x": 1}


@asset(partitions={"k": partitions(["a", "b"])}, batch=2)
def batched(k: list[str]) -> dict:
    return 7
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=cwd, capture_output=True, text=True, timeout=120
    )


def envelope(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 1, proc.stderr
    last = next(line for line in reversed(proc.stderr.splitlines()) if line.startswith("{"))
    env = json.loads(last)
    assert env["kind"] == "step_failed", env
    return env


def history_step(project: Path, proc: subprocess.CompletedProcess, node: str) -> dict:
    run_id = json.loads(proc.stdout.strip().splitlines()[-1])["run_id"]
    detail = barca(project, "history", run_id, "--json")
    assert detail.returncode == 0, detail.stderr
    steps = [s for s in json.loads(detail.stdout)["steps"] if s["status"] == "failed"]
    assert steps and steps[0]["node_id"].startswith(node), steps
    return steps[0]


def test_a_failure_in_user_code_keeps_the_trimmed_traceback(project):
    proc = barca(project, "get", "oops", "pipeline.py", "--json")
    env = envelope(proc)
    assert env["phase"] == "user_code"
    assert "pipeline.py" in env["traceback"]
    assert "_worker.py" not in env["traceback"]
    assert "in barca" not in proc.stderr
    step = history_step(project, proc, "pipeline.py:oops")
    assert step["error_phase"] == "user_code"
    assert "_worker.py" not in step["error_traceback"]


def test_a_failure_writing_the_result_keeps_barcas_frames(project):
    proc = barca(project, "get", "unwritable", "pipeline.py", "--json")
    env = envelope(proc)
    assert env["phase"] == "write_output", env
    # The frames are barca's: before #664 this was null.
    assert env["traceback"] and "barca" in env["traceback"], env
    assert "barca's own code" in env["remediation"]
    assert "run failed: step 'pipeline.py:unwritable' failed in barca (write_output): " in (
        proc.stderr
    )
    step = history_step(project, proc, "pipeline.py:unwritable")
    assert step["error_phase"] == "write_output"
    assert step["error_traceback"] and "File" in step["error_traceback"]

    human = barca(project, "get", "unwritable", "pipeline.py", "-o", "pretty")
    assert human.returncode == 1
    assert (
        "Step 'pipeline.py:unwritable' failed in barca while writing its result "
        "(phase write_output): " in human.stderr
    ), human.stderr


def test_a_failed_sink_is_phase_sink(project):
    env = envelope(barca(project, "get", "sinky", "pipeline.py", "--json"))
    assert env["phase"] == "sink", env
    assert "SinkError" in env["error"]
    assert env["traceback"] and "File" in env["traceback"]


def test_a_batched_result_that_cannot_be_split_is_phase_split_output(project):
    proc = barca(project, "get", "batched", "pipeline.py", "--json")
    env = envelope(proc)
    assert env["phase"] == "split_output", env
    assert env["traceback"] and "_batch.py" in env["traceback"], env


def test_an_unreadable_input_is_phase_load_inputs(project):
    first = barca(project, "get", "src", "pipeline.py", "--json")
    assert first.returncode == 0, first.stderr
    artifact = project / json.loads(first.stdout)["final_output"]["_barca_artifact"]["path"]
    # Same size, not JSON: the file is there, and reading it fails.
    artifact.write_text("{" * len(artifact.read_text()))
    proc = barca(project, "get", "reads_src", "pipeline.py", "--json")
    env = envelope(proc)
    assert env["node"] == "pipeline.py:reads_src", env
    assert env["phase"] == "load_inputs", env
    assert env["traceback"] and "_worker.py" in env["traceback"], env
