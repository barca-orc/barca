"""Every CLI error path emits one parseable JSON envelope on stderr in JSON mode (#154).

The envelope is `{"error", "code", "kind", "remediation"}`; `step_failed` adds `node`,
`traceback` and `artifact_dir`. `code` is the exit code: 1 step_failed, 2 usage, 3 infra,
130 cancelled. In human mode (`-o pretty`/`-o value`, or no `--json` on inspection commands)
the prose is printed with the remediation appended. Errors never go to stdout; a failed step in
JSON mode still prints its result line (`"status": "failed"`) there (#149).
"""

import json
import os

import signal
import subprocess
import time
from pathlib import Path

import pytest

import barca
from barca.api import BarcaError, _find_binary

PIPELINE = """
import time
from barca import asset, task


@asset()
def src() -> int:
    return 1


@asset(inputs={"x": src})
def oops(x: int) -> float:
    return x / 0


@task(inputs={"x": src})
def deploy(x: int) -> int:
    return x


@asset()
def slow() -> int:
    time.sleep(30)
    return 1
"""

KIND_CODES = {"step_failed": 1, "usage": 2, "infra": 3, "cancelled": 130}


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "broken.py").write_text(
        "from barca import asset\n\n@asset()\ndef b():\n    return {\n"
    )
    # A DAG error: an input that names no definition.
    (tmp_path / "dangling.py").write_text(
        "from barca import asset\n\n@asset(inputs={'x': nowhere})\ndef a(x):\n    return x\n"
    )
    return tmp_path


def barca_cli(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([_find_binary(), *args], cwd=cwd, capture_output=True, text=True)


def envelope(proc: subprocess.CompletedProcess) -> dict:
    """The last stderr line, which must be the JSON envelope; checks the shared contract."""
    assert proc.returncode != 0, proc.stderr
    last = proc.stderr.strip().splitlines()[-1]
    env = json.loads(last)
    if env["kind"] == "step_failed" and proc.stdout:
        result = json.loads(proc.stdout)  # one line: the failed run's result, not the error
        assert result["status"] == "failed" and result["failed_node"] == env["node"], result
    else:
        assert proc.stdout == "", f"errors must not go to stdout: {proc.stdout!r}"
    assert env["kind"] in KIND_CODES, env
    assert env["code"] == KIND_CODES[env["kind"]] == proc.returncode, (env, proc.returncode)
    assert isinstance(env["error"], str) and env["error"], env
    assert isinstance(env["remediation"], str) and env["remediation"], env
    return env


# Every error path reachable from the command line in JSON mode, with its expected kind.
USAGE_CASES = [
    # argument parser (clap)
    ("get", "pipeline.py", "--bogus"),
    ("pipeline.py", "--bogus"),
    ("get",),
    ("run", "deploy", "pipeline.py", "-o", "nope"),
    ("list", "pipeline.py", "--json", "--bogus"),
    # barca's own argument checks
    ("get", "src"),
    ("run", "deploy"),
    ("run", "deploy", "pipeline.py", "--refresh", "src", "mid"),
    ("get", "src", "pipeline.py", "notpy.txt"),
    # engine: unknown target, wrong command for the node kind, bad --refresh name
    ("get", "nope", "pipeline.py"),
    ("get", "deploy", "pipeline.py"),
    ("run", "src", "pipeline.py"),
    ("run", "deploy", "pipeline.py", "--refresh", "nope"),
    ("get", "src", "pipeline.py", "--dry-run", "--env", "../bad"),
    # files that cannot be read or parsed, and invalid graphs
    ("get", "missing.py"),
    ("plan", "missing.py"),
    ("list", "missing.py", "--json"),
    ("stats", "src", "missing.py", "--json"),
    ("get", "broken.py"),
    ("get", "dangling.py"),
    # the manual
    ("docs", "nope", "--json"),
]


@pytest.mark.parametrize("args", USAGE_CASES, ids=" ".join)
def test_usage_errors_are_json_envelopes_with_exit_2(project, args):
    env = envelope(barca_cli(project, *args))
    assert env["kind"] == "usage"
    assert "node" not in env


def test_step_failure_envelope_has_node_traceback_and_artifact_dir(project):
    env = envelope(barca_cli(project, "get", "oops", "pipeline.py"))
    assert env["kind"] == "step_failed"
    assert env["node"] == "pipeline.py:oops"
    assert env["error"] == "step 'pipeline.py:oops' failed: ZeroDivisionError: division by zero"
    assert "line" in env["traceback"] and "return x / 0" in env["traceback"]
    assert env["artifact_dir"].endswith("pipeline.py--oops")


def test_step_failure_in_a_task_run(project):
    (project / "t.py").write_text(
        "from barca import task\n\n@task()\ndef boom():\n    raise RuntimeError('nope')\n"
    )
    env = envelope(barca_cli(project, "run", "boom", "t.py"))
    assert env["kind"] == "step_failed"
    assert env["node"] == "t.py:boom"
    assert "RuntimeError: nope" in env["error"]


def test_infra_failure_is_exit_3(project):
    (project / ".barca").write_text("a file where barca expects its state directory")
    for args in (("history", "--json"), ("get", "src", "pipeline.py")):
        env = envelope(barca_cli(project, *args))
        assert env["kind"] == "infra", env


def test_ctrl_c_is_cancelled_with_exit_130(project):
    proc = subprocess.Popen(
        [_find_binary(), "get", "slow", "pipeline.py"],
        cwd=project,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    time.sleep(2)
    proc.send_signal(signal.SIGINT)
    out, err = proc.communicate(timeout=30)
    env = envelope(subprocess.CompletedProcess(proc.args, proc.returncode, out, err))
    assert env["kind"] == "cancelled"


# ─── Human mode ───────────────────────────────────────────────────────────────


def test_human_mode_keeps_prose_and_appends_the_remediation(project):
    proc = barca_cli(project, "get", "oops", "pipeline.py", "-o", "pretty")
    assert proc.returncode == 1
    assert proc.stdout == ""
    assert "Worker failed: ZeroDivisionError: division by zero" in proc.stderr
    assert "return x / 0" in proc.stderr
    assert proc.stderr.rstrip().endswith("will not re-run.")
    assert not proc.stderr.strip().splitlines()[-1].startswith("{")


def test_human_mode_usage_error_is_prose(project):
    # Piped stdout picks JSON (#153), so ask for human output explicitly.
    proc = barca_cli(project, "list", "missing.py", "--pretty")
    assert proc.returncode == 2
    assert proc.stderr.startswith("missing.py: ")
    assert "barca list --help" in proc.stderr
    proc = barca_cli(project, "get", "pipeline.py", "--bogus", "-o", "pretty")
    assert proc.returncode == 2
    assert proc.stderr.startswith("error: unexpected argument '--bogus'")


def test_help_and_version_are_not_errors(project):
    for args in (("--help",), ("get", "--help"), ("--version",)):
        proc = barca_cli(project, *args)
        assert proc.returncode == 0
        assert proc.stdout and not proc.stderr


# ─── Python API ───────────────────────────────────────────────────────────────


def test_python_api_exposes_the_envelope(project, monkeypatch):
    monkeypatch.chdir(project)
    with pytest.raises(BarcaError) as exc:
        barca.get("oops", "pipeline.py")
    e = exc.value
    assert (e.kind, e.code, e.node) == ("step_failed", 1, "pipeline.py:oops")
    assert "ZeroDivisionError" in str(e) and "return x / 0" in str(e)
    with pytest.raises(BarcaError) as exc:
        barca.get("nope", "pipeline.py")
    assert (exc.value.kind, exc.value.code) == ("usage", 2)
    assert "barca list pipeline.py" in exc.value.remediation


def test_piped_inspection_command_errors_are_json(project):
    """With no flag, a piped `list` prints JSON, so its errors are the envelope too (#153)."""
    env = envelope(barca_cli(project, "list", "missing.py"))
    assert env["kind"] == "usage"


def test_invalid_barca_output_is_a_usage_error(project):
    proc = subprocess.run(
        [_find_binary(), "list", "pipeline.py"],
        cwd=project,
        capture_output=True,
        text=True,
        env={**os.environ, "BARCA_OUTPUT": "yaml"},
    )
    env = envelope(proc)
    assert env["kind"] == "usage"
    assert "BARCA_OUTPUT" in env["error"]
