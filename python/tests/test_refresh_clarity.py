"""`--refresh` and long-running steps must be unambiguous for an agent driving the CLI.

Verified behaviors this guards (each used to be silent or cryptic):
  * a name that matches no upstream asset is an error, not a silent no-op;
  * `--refresh a b` (space separated) says to use a comma instead of "No such file";
  * refreshing an asset re-materializes everything downstream of it in the target's cone
    (cascade, the default since #151); `--no-cascade` refreshes only the named assets and then
    says that cached downstream assets do not reflect the refresh;
  * a step that runs for a long time announces itself instead of looking hung.
"""

import json
import os
import re
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
import time
from barca import asset, task


@asset()
def src() -> dict:
    return {"t": time.time()}          # different every time it re-runs


@asset(inputs={"s": src})
def mid(s: dict) -> dict:
    return {"t": s["t"]}


@task(inputs={"m": mid})
def report(m: dict) -> dict:
    return {"t": m["t"]}
"""

SLOW = """
import time
from barca import asset


@asset()
def slow() -> dict:
    time.sleep(3)
    return {"done": True}
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "slow.py").write_text(SLOW)
    return tmp_path


def barca(project: Path, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
    )


def final(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


@pytest.fixture()
def warm(project) -> Path:
    final(barca(project, "run", "report", "pipeline.py"))  # cold: src, mid, report
    return project


def test_unknown_refresh_name_is_an_error_that_lists_the_valid_names(warm):
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "nope")
    assert proc.returncode == 2  # usage error
    assert proc.stdout == ""
    assert "no upstream asset named 'nope'" in proc.stderr
    assert "src" in proc.stderr and "mid" in proc.stderr
    assert "steps done" not in proc.stderr  # failed before running anything


def test_one_valid_and_one_unknown_name_still_fails(warm):
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "src,nope")
    assert proc.returncode == 2
    assert "no upstream asset named 'nope'" in proc.stderr


def test_space_separated_refresh_says_to_use_commas(warm):
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "src", "mid")
    assert proc.returncode == 2
    assert "'mid' is not a .py file" in proc.stderr
    assert "--refresh src,mid" in proc.stderr or "comma" in proc.stderr
    assert "No such file" not in proc.stderr


def test_refresh_cascades_to_downstream_assets_by_default(warm):
    before = final(barca(warm, "run", "report", "pipeline.py"))["final_output"]["t"]
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "src")
    out = final(proc)
    # `src` re-ran, `mid` (downstream) re-ran with it, so the task sees the fresh value.
    assert out["steps_executed"] == 3
    assert out["final_output"]["t"] != before
    by = {s["id"].split(":")[-1]: s for s in out["steps"]}
    assert by["src"]["status"] == "ran" and by["src"]["reason"] == "refresh"
    assert by["mid"]["status"] == "ran" and by["mid"]["reason"] == "refresh_cascade"
    assert "src" in by["mid"]["detail"]
    assert "served from cache but depends on refreshed" not in proc.stderr


CHAIN = """
from barca import asset, task


@asset()
def src() -> int:
    return 1


@asset(inputs={"s": src})
def mid(s: int) -> int:
    return s + 1


@asset(inputs={"m": mid})
def leaf(m: int) -> int:
    return m + 1


@asset()
def other() -> int:
    return 7


@task(inputs={"l": leaf, "o": other})
def report(l: int, o: int) -> int:
    return l + o
"""


def test_cascade_reaches_transitive_assets_but_not_siblings(project):
    (project / "chain.py").write_text(CHAIN)
    final(barca(project, "run", "report", "chain.py"))
    out = final(barca(project, "run", "report", "chain.py", "--refresh", "src"))
    by = {s["id"].split(":")[-1]: s for s in out["steps"]}
    assert by["src"]["reason"] == "refresh"
    assert by["mid"]["reason"] == "refresh_cascade"
    assert by["leaf"]["status"] == "ran" and by["leaf"]["reason"] == "refresh_cascade"
    assert "'src'" in by["leaf"]["detail"]  # names the asset it cascaded from
    assert by["other"]["status"] == "cached"  # not downstream of src
    assert out["steps_executed"] == 4  # src, mid, leaf, and the task


def test_no_cascade_refreshes_only_the_named_asset_and_warns(warm):
    before = final(barca(warm, "run", "report", "pipeline.py"))["final_output"]["t"]
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "src", "--no-cascade")
    out = final(proc)
    # `src` re-ran, but `mid` (downstream) came from cache, so the task still saw the old value.
    assert out["steps_executed"] == 2
    assert out["final_output"]["t"] == before
    assert "'mid' was served from cache but depends on refreshed 'src'" in proc.stderr
    assert "--refresh src,mid" in proc.stderr


def test_no_cascade_requires_refresh(warm):
    proc = barca(warm, "run", "report", "pipeline.py", "--no-cascade")
    assert proc.returncode == 2
    assert "--refresh" in proc.stderr


def test_python_api_cascades_and_can_opt_out(warm):
    from barca import api

    cwd = os.getcwd()
    os.chdir(warm)
    try:
        before = api.run("report", "pipeline.py")["t"]
        assert api.run("report", "pipeline.py", refresh=["src"], cascade=False)["t"] == before
        assert api.run("report", "pipeline.py", refresh=["src"])["t"] != before
    finally:
        os.chdir(cwd)


def test_refreshing_the_whole_chain_propagates_and_does_not_warn(warm):
    before = final(barca(warm, "run", "report", "pipeline.py"))["final_output"]["t"]
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "src,mid", "--no-cascade")
    out = final(proc)
    assert out["final_output"]["t"] != before
    assert "served from cache but depends on refreshed" not in proc.stderr


def test_refreshing_a_leaf_asset_does_not_warn(warm):
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh", "mid")
    final(proc)
    assert "served from cache but depends on refreshed" not in proc.stderr


def test_refresh_all_does_not_warn(warm):
    proc = barca(warm, "run", "report", "pipeline.py", "--refresh-all")
    final(proc)
    assert "served from cache but depends on refreshed" not in proc.stderr


def test_a_long_running_step_announces_itself_in_agent_mode(project):
    proc = barca(project, "get", "slow", "slow.py", "--agent", env={"BARCA_PROGRESS_SECS": "1"})
    final(proc)
    assert re.search(r"still running \(\d+s\).*slow", proc.stderr), proc.stderr


def test_a_long_running_step_announces_itself_without_agent_mode_too(project):
    # stderr is a pipe here (no terminal), the usual situation for a script or agent that did
    # not pass --agent: the hidden progress bar must not swallow the notice.
    proc = barca(project, "get", "slow", "slow.py", env={"BARCA_PROGRESS_SECS": "1"})
    final(proc)
    assert re.search(r"still running \(\d+s\).*slow", proc.stderr), proc.stderr


def test_quick_steps_do_not_spam_progress_lines(project):
    proc = barca(
        project, "get", "slow", "slow.py", "--agent"
    )  # default interval is far longer than 3s
    final(proc)
    assert "still running" not in proc.stderr


def test_sink_failures_are_visible_without_agent_mode(project):
    # The docs tell automation to check stderr for "SINK FAILED"; keep that true in a non-terminal
    # run without --agent.
    (project / "sinky.py").write_text(
        "from barca import asset, sink\n\n\n"
        '@asset()\n@sink("/dev/null/cannot/write/here.json")\n'
        'def sinky() -> dict:\n    return {"x": 1}\n'
    )
    proc = barca(project, "get", "sinky", "sinky.py")
    final(proc)  # a failing sink never fails the asset
    assert "SINK FAILED" in proc.stderr, proc.stderr
