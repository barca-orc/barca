"""A worker that dies names how it died (#433), and a run says which env and store it uses."""

import json
import os
import subprocess

import pytest

from barca.api import _find_binary

PIPELINE = """
import os, signal
from barca import asset, task

@asset()
def killed():
    os.kill(os.getpid(), signal.SIGKILL)

@asset()
def exits():
    os._exit(3)

@asset()
def fine():
    return 1

@task()
def chore():
    return None
"""


def barca(root, *args, env=None):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env={**os.environ, "BARCA_REMOTE": "off", **(env or {})},
        text=True,
        capture_output=True,
        timeout=60,
    )


@pytest.fixture
def root(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


@pytest.mark.parametrize(
    "node,how",
    [("killed", "SIGKILL (possibly the OOM killer)"), ("exits", "code 3")],
)
def test_a_dead_worker_names_its_exit_status(root, node, how):
    proc = barca(root, "get", node, "pipeline.py", "--json")
    assert proc.returncode == 1, proc.stderr
    message = f"worker for pipeline.py:{node} exited with {how}"
    result = json.loads(proc.stdout)
    assert result["failed_node"] == f"pipeline.py:{node}"
    assert message in result["error"], result["error"]
    lines = proc.stderr.splitlines()
    envelope = json.loads(next(line for line in reversed(lines) if line.startswith("{")))
    assert message in envelope["error"], envelope
    summary = f"[barca] run failed: step 'pipeline.py:{node}' failed: {message}"
    assert lines[-1].startswith(summary), lines[-1]

    agent = barca(root, "get", node, "pipeline.py", "--agent")
    assert f"[barca] step:pipeline.py:{node} failed: {message}" in agent.stderr, agent.stderr


def test_default_env_from_barca_toml_is_named_once_at_startup(root):
    (root / "barca.toml").write_text('default_env = "prod"\n')
    proc = barca(root, "get", "fine", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    line = (
        "[barca] env prod (default_env in barca.toml); "
        "artifact store local (.barca/envs/prod/artifacts)"
    )
    assert proc.stderr.splitlines().count(line) == 1, proc.stderr
    assert "[barca] env" not in proc.stdout
    json.loads(proc.stdout)

    proc = barca(root, "run", "chore", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    assert proc.stderr.splitlines().count(line) == 1, proc.stderr

    proc = barca(root, "get", "fine", "pipeline.py", "--env", "prod", "--json")
    assert proc.returncode == 0, proc.stderr
    assert "[barca] env" not in proc.stderr, "--env on the command line says it already"


def test_no_startup_line_without_default_env_or_a_store(root):
    proc = barca(root, "get", "fine", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    assert "[barca] env" not in proc.stderr
