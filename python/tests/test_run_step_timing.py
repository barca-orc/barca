"""Run timing, retry counts, and import diagnostics (#386)."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


def cli(root, *args, code=0):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env={**os.environ, "BARCA_REMOTE": "off"},
        capture_output=True,
        text=True,
        timeout=45,
    )
    assert proc.returncode == code, proc.stderr
    return proc


def test_retry_success_and_failure_have_timing_attempts_and_history(tmp_path):
    (tmp_path / "p.py").write_text("""
from pathlib import Path
import time
from barca import asset
@asset(retries=2)
def flaky():
    path = Path("attempts")
    count = int(path.read_text()) if path.exists() else 0
    path.write_text(str(count + 1))
    time.sleep(0.02)
    if count == 0: raise ValueError("retry me")
    return 42
@asset(retries=2)
def broken():
    time.sleep(0.02)
    raise ValueError("still broken")
""")
    run = cli(tmp_path, "get", "flaky", "p.py", "--json", "--agent")
    doc = json.loads(run.stdout)
    assert doc["steps"][0]["attempts"] == 2
    assert doc["steps"][0]["elapsed_seconds"] >= 0.04
    assert "completed" in run.stderr and "attempts=2" in run.stderr
    history = json.loads(cli(tmp_path, "history", "--json").stdout)
    assert history["runs"][0]["attempts"] == 2
    stats = json.loads(cli(tmp_path, "stats", "flaky", "p.py", "--json").stdout)
    assert stats["recent_runs"][0]["attempts"] == 2
    cached = json.loads(cli(tmp_path, "get", "flaky", "p.py", "--json").stdout)
    assert "attempts" not in cached["steps"][0]
    assert "elapsed_seconds" not in cached["steps"][0]
    failed = json.loads(cli(tmp_path, "get", "broken", "p.py", "--json", code=1).stdout)
    assert failed["steps"][0]["attempts"] == 2
    assert failed["steps"][0]["elapsed_seconds"] >= 0.04


def test_import_time_is_separate_and_timeout_names_the_step(tmp_path):
    (tmp_path / "p.py").write_text("""
import time
from barca import asset
# Deliberately exceed the one-second reporting threshold.
time.sleep(1.1)
@asset(timeout_seconds=1)
def slow():
    time.sleep(5)
""")
    proc = cli(tmp_path, "get", "slow", "p.py", "--json", code=1)
    doc = json.loads(proc.stdout)
    step = doc["steps"][0]
    assert step["attempts"] == 1
    assert step["import_seconds"] >= 1
    assert step["elapsed_seconds"] >= step["import_seconds"] + 0.9
    assert "[barca] importing " in proc.stderr
    assert "p.py:slow" in doc["error"] and "<lambda>" not in doc["error"]


def test_partition_attempts_are_per_key_and_worker_crashes_are_timed(tmp_path):
    (tmp_path / "p.py").write_text("""
from pathlib import Path
import os
import time
from barca import asset, partitions
@asset(partitions={"k": partitions(["a", "b"])}, retries=2)
def value(k):
    time.sleep(0.02)
    path = Path(k)
    if k == "b" and not path.exists():
        path.touch()
        os._exit(9)
    return k
""")
    proc = cli(tmp_path, "get", "value", "p.py", "--json")
    doc = json.loads(proc.stdout)
    assert doc["steps"][0]["attempts"] == 2
    assert doc["steps"][0]["elapsed_seconds"] >= 0.06
    history = json.loads(cli(tmp_path, "history", "--json").stdout)
    assert history["runs"][0]["attempts"] == 3


def test_parallel_branches_do_not_inflate_an_independent_dag_steps_timing(tmp_path):
    (tmp_path / "p.py").write_text("""
from functools import partial
from barca import asset, parallel
@asset()
def leaf(value=0):
    return value
@asset()
def root():
    return parallel(partial(leaf, 1), partial(leaf, 2))
""")
    doc = json.loads(cli(tmp_path, "get", "p.py", "--json").stdout)
    leaf = next(step for step in doc["steps"] if step["id"] == "p.py:leaf")
    stats = json.loads(cli(tmp_path, "stats", "leaf", "p.py", "--json").stdout)
    assert leaf["elapsed_seconds"] == pytest.approx(stats["recent_runs"][0]["elapsed_seconds"])
    assert leaf["attempts"] == 1
