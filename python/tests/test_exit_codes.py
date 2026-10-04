"""Exit codes are the contract an agent loop branches on (issue #149).

| code | meaning                                                                      |
|------|------------------------------------------------------------------------------|
| 0    | success                                                                      |
| 1    | a user step failed (raised, called sys.exit(), crashed its worker)           |
| 2    | usage or definition error: nothing ran (bad args, unknown target, bad config) |
| 3    | barca infrastructure failure (metadata DB, worker spawn, shared remote state) |
| 130  | cancelled (Ctrl-C / SIGINT)                                                  |

Regression guarded here: a step that called `sys.exit(1)` -- the usual way a validation
script says "hard failure" -- ran under the default step timeout in a helper thread that only
caught `Exception`, so `SystemExit` vanished, the step was recorded as a success with a `null`
value (and cached, for assets) and barca exited 0.
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
import os
import sys
import time
from functools import partial
from pathlib import Path

from barca import asset, parallel, task


@asset()
def good() -> int:
    return 1


@asset()
def bad_asset() -> int:
    raise ValueError("asset boom")


@asset(inputs={"x": bad_asset})
def after_bad(x: int) -> int:
    return x


@task(inputs={"g": good})
def boom(g: int):
    raise RuntimeError("task boom")


@asset(retries=2, retry_backoff=0.01)
def always_fails() -> int:
    raise ValueError("fails on every attempt")


@task(inputs={"x": always_fails})
def after_retries(x: int):
    return x


@task()
def child(i: int) -> int:
    if i == 1:
        raise RuntimeError(f"child {i} boom")
    return i


@task()
def fan_out() -> list:
    results = parallel(partial(child, 0), partial(child, 1))
    for r in results:
        if not isinstance(r, int):
            raise RuntimeError(f"a branch failed: {r}")
    return results


@task()
def validate():
    print("3 rows with a null id", file=sys.stderr)
    sys.exit(1)


@task(timeout_seconds=0)
def validate_no_timeout():
    sys.exit(1)


@task()
def exits_zero():
    sys.exit(0)


@asset()
def validated_asset() -> int:
    sys.exit("validation failed")


@task()
def interrupted():
    raise KeyboardInterrupt()


@task()
def crashes():
    os._exit(0)


@task()
def sleeps():
    Path("started").write_text("1")
    time.sleep(60)
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def barca(cwd: Path, *args: str, env: dict | None = None, binary: str | None = None):
    return subprocess.run(
        [binary or _find_binary(), *args],
        cwd=cwd,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
        timeout=120,
    )


def run_failed_line(proc) -> str:
    """The greppable `[barca] run failed: ...` line. It comes right before the error, so in JSON
    mode it is the line before the error envelope (#154), which is always the last stderr line."""
    lines = [ln for ln in proc.stderr.strip().splitlines() if ln.startswith("[barca] run failed")]
    assert len(lines) == 1, proc.stderr
    return lines[0]


def last_json(proc) -> dict:
    return json.loads(proc.stdout.strip().splitlines()[-1])


# ─── 0: success ──────────────────────────────────────────────────────────────


def test_success_exits_0_and_says_so(project):
    proc = barca(project, "get", "good", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    out = last_json(proc)
    assert out["status"] == "success"
    assert out["final_output"] == 1


# ─── 1: a user step failed ───────────────────────────────────────────────────

OUTPUT_MODES = [
    [],
    ["-o", "json"],
    ["-o", "pretty"],
    ["-o", "value"],
    ["--agent"],
    ["-o", "pretty", "--agent"],
]


@pytest.mark.parametrize("mode", OUTPUT_MODES, ids=lambda m: " ".join(m) or "default")
def test_raising_task_exits_1_in_every_output_mode(project, mode):
    proc = barca(project, "run", "boom", "pipeline.py", *mode)
    assert proc.returncode == 1, proc.stderr
    assert "RuntimeError: task boom" in proc.stderr
    line = run_failed_line(proc)
    assert "pipeline.py:boom" in line


@pytest.mark.parametrize(
    "args",
    [
        ["run", "boom", "pipeline.py", "--refresh", "good"],
        ["run", "boom", "pipeline.py", "--refresh-all"],
        ["run", "after_retries", "pipeline.py"],  # retries exhausted
        ["run", "fan_out", "pipeline.py"],  # parallel() branch failure re-raised by the parent
        ["run", "fan_out", "pipeline.py", "--agent"],
        ["get", "bad_asset", "pipeline.py"],
        ["get", "after_bad", "pipeline.py"],  # failure upstream of the target
        ["get", "pipeline.py"],  # whole file
        ["pipeline.py"],  # `barca file.py` shorthand
        ["run", "crashes", "pipeline.py"],  # os._exit() kills the worker mid-step
    ],
    ids=lambda a: " ".join(a),
)
def test_step_failure_exits_1(project, args):
    proc = barca(project, *args)
    assert proc.returncode == 1, f"exit {proc.returncode}\n{proc.stderr}"
    run_failed_line(proc)


@pytest.mark.parametrize(
    "mode", [[], ["--agent"], ["-o", "pretty"]], ids=lambda m: " ".join(m) or "default"
)
@pytest.mark.parametrize("target", ["validate", "validate_no_timeout", "exits_zero", "interrupted"])
def test_sys_exit_and_keyboard_interrupt_in_a_step_exit_1(project, target, mode):
    """Regression: these used to be recorded as a success with a null value (exit 0)."""
    proc = barca(project, "run", target, "pipeline.py", *mode)
    assert proc.returncode == 1, (
        f"exit {proc.returncode}\nstdout:{proc.stdout}\nstderr:{proc.stderr}"
    )
    assert "sys.exit()" in proc.stderr or "KeyboardInterrupt" in proc.stderr


def test_sys_exit_in_an_asset_is_not_cached(project):
    for _ in range(2):  # the second run must re-run (and fail) rather than serve a cached null
        proc = barca(project, "get", "validated_asset", "pipeline.py")
        assert proc.returncode == 1, proc.stderr
        assert "validation failed" in proc.stderr
        assert "SystemExit" in proc.stderr


def test_parallel_branch_failure_the_parent_handles_is_not_a_run_failure(project):
    """parallel() returns a failed branch as a ParallelError; a parent that returns it succeeds."""
    (project / "fan.py").write_text(
        PIPELINE
        + """

@task()
def tolerant() -> list:
    return [str(r) for r in parallel(partial(child, 0), partial(child, 1))]
"""
    )
    proc = barca(project, "run", "tolerant", "fan.py")
    assert proc.returncode == 0, proc.stderr
    assert "child 1 boom" in last_json(proc)["final_output"][1]


def test_failed_run_prints_a_json_result_with_the_failing_node(project):
    barca(project, "get", "good", "pipeline.py")  # warm the cache so `good` is served cached
    proc = barca(project, "run", "boom", "pipeline.py")
    assert proc.returncode == 1
    out = last_json(proc)
    assert out["status"] == "failed"
    assert out["failed_node"] == "pipeline.py:boom"
    assert out["error"].startswith("RuntimeError: task boom")
    assert out["run_id"]
    steps = {s["id"]: s["status"] for s in out["steps"]}
    assert steps == {"pipeline.py:good": "cached", "pipeline.py:boom": "failed"}
    history = json.loads(barca(project, "history", "--json").stdout)["runs"]
    assert history[0]["run_id"] == out["run_id"] and history[0]["status"] == "failed"


def test_failed_run_names_the_upstream_node_that_failed(project):
    proc = barca(project, "get", "after_bad", "pipeline.py")
    assert proc.returncode == 1
    out = last_json(proc)
    assert out["status"] == "failed"
    assert out["failed_node"] == "pipeline.py:bad_asset"


def test_failed_run_prints_nothing_on_stdout_for_value_mode(project):
    proc = barca(project, "run", "boom", "pipeline.py", "-o", "value")
    assert proc.returncode == 1
    assert proc.stdout == ""


# ─── 2: usage error ──────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "args",
    [
        ["get", "--frobnicate", "pipeline.py"],  # clap: unknown flag
        ["run", "pipeline.py"],  # run without a target
        ["get", "good", "notes.txt"],  # not a .py file
        ["run", "boom", "pipeline.py", "--refresh", "good", "boom"],  # space-separated --refresh
        ["get", "missing", "pipeline.py"],  # unknown target
        ["get", "boom", "pipeline.py"],  # get on a task
        ["run", "good", "pipeline.py"],  # run on an asset
        ["run", "boom", "pipeline.py", "--refresh", "nope"],  # unknown --refresh name
        ["run", "boom", "pipeline.py", "--dry-run", "--refresh", "nope"],
        ["get", "good", "absent.py"],  # file does not exist
        ["get", "good", "pipeline.py", "--env", "bad/name"],  # invalid env name
        ["docs", "nosuchtopic"],
    ],
    ids=lambda a: " ".join(a),
)
def test_usage_errors_exit_2_and_run_nothing(project, args):
    proc = barca(project, *args)
    assert proc.returncode == 2, f"exit {proc.returncode}\n{proc.stderr}"
    assert proc.stdout == ""
    assert proc.stderr.strip()
    assert not (project / ".barca").exists() or "steps done" not in proc.stderr


def test_invalid_barca_toml_exits_2(project):
    (project / "barca.toml").write_text("this is = = not toml\n")
    proc = barca(project, "get", "good", "pipeline.py")
    assert proc.returncode == 2, proc.stderr
    assert "barca.toml" in proc.stderr


def test_unparseable_pipeline_exits_2(project):
    (project / "broken.py").write_text("from barca import asset\n\n@asset()\ndef f(:\n")
    proc = barca(project, "get", "broken.py")
    assert proc.returncode == 2, proc.stderr


# ─── 3: barca infrastructure failure ─────────────────────────────────────────


def test_metadata_db_unavailable_exits_3(project):
    (project / ".barca").write_text("")  # a file where the state directory should be
    proc = barca(project, "get", "good", "pipeline.py")
    assert proc.returncode == 3, proc.stderr
    assert "Database error" in proc.stderr


def test_shared_state_pull_failure_exits_3(project):
    proc = barca(
        project,
        "get",
        "good",
        "pipeline.py",
        env={"BARCA_STATE_URI": "nosuchproto://bucket/metadata.db"},
    )
    assert proc.returncode == 3, proc.stderr
    assert "shared state pull" in proc.stderr


def test_worker_spawn_failure_exits_3(project, tmp_path_factory):
    """barca uses the `python` next to its own binary; make that one unable to start a worker."""
    bindir = tmp_path_factory.mktemp("bin")
    binary = bindir / "barca"
    shutil.copy2(_find_binary(), binary)
    fake = bindir / "python"
    fake.write_text("#!/bin/sh\nexit 1\n")
    fake.chmod(0o755)
    proc = barca(project, "get", "good", "pipeline.py", binary=str(binary))
    assert proc.returncode == 3, proc.stderr
    assert "no workers available" in proc.stderr


# ─── 130: cancelled ──────────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX signals")
def test_sigint_exits_130(project):
    proc = subprocess.Popen(
        [_find_binary(), "run", "sleeps", "pipeline.py", "--agent"],
        cwd=project,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    deadline = time.time() + 30
    while not (project / "started").exists():
        assert time.time() < deadline, "step never started"
        assert proc.poll() is None, proc.communicate()
        time.sleep(0.05)
    proc.send_signal(signal.SIGINT)
    _, err = proc.communicate(timeout=30)
    assert proc.returncode == 130, err
    assert "cancelled" in err
    history = json.loads(barca(project, "history", "--json").stdout)["runs"]
    assert history[0]["status"] == "cancelled"
