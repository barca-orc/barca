"""get/run usage errors state the fix and point at `barca list` (issue #148).

Positionals in the wrong order have exactly one valid reading, so barca prints the corrected
command instead of guessing. There is deliberately no fuzzy "did you mean" matching: an agent
could read a guess as confirmation. Every get/run usage error exits 2 and ends with
`barca list <files>`, the discovery entry point.

`get`/`run` default to JSON output, so their errors are a JSON envelope on stderr (#154); these
tests read `error` and `remediation` from it. One test checks the human-mode prose.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset, task


@asset()
def src() -> dict:
    return {"v": 1}


@asset(inputs={"s": src})
def mid(s: dict) -> dict:
    return {"v": s["v"] + 1}


@task(inputs={"m": mid})
def report(m: dict) -> dict:
    return m
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "other.py").write_text("")
    return tmp_path


def barca(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, env=dict(os.environ), capture_output=True, text=True
    )


def usage_error(proc: subprocess.CompletedProcess) -> str:
    """The envelope as text: `error: <error>`, a blank line, then the remediation."""
    assert proc.returncode == 2, (proc.returncode, proc.stderr)
    assert proc.stdout == ""
    assert "steps done" not in proc.stderr  # rejected before anything ran
    env = json.loads(proc.stderr.strip().splitlines()[-1])
    assert env["kind"] == "usage" and env["code"] == 2, env
    return f"error: {env['error']}\n\n{env['remediation']}\n"


def last_line(stderr: str) -> str:
    return stderr.strip().splitlines()[-1]


def test_run_with_files_before_target_prints_the_corrected_command(project):
    err = usage_error(barca(project, "run", "pipeline.py", "report"))
    assert "error: the target comes before the files" in err
    assert "\nbarca run report pipeline.py\n" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."
    assert "did you mean" not in err.lower()


def test_get_with_files_before_target_prints_the_corrected_command(project):
    err = usage_error(barca(project, "get", "pipeline.py", "mid"))
    assert "error: the target comes before the files" in err
    assert "\nbarca get mid pipeline.py\n" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."


def test_the_corrected_command_keeps_every_file_and_flag(project):
    err = usage_error(
        barca(project, "run", "pipeline.py", "report", "other.py", "--refresh", "src", "--agent")
    )
    assert "\nbarca run report pipeline.py other.py --refresh src --agent\n" in err
    assert "Run `barca list pipeline.py other.py`" in err


def test_the_corrected_command_runs(project):
    err = usage_error(barca(project, "run", "pipeline.py", "report"))
    corrected = err.split("\nbarca ", 1)[1].splitlines()[0].split()
    proc = barca(project, *corrected)
    assert proc.returncode == 0, proc.stderr


def test_shorthand_with_target_after_file_is_corrected_to_get(project):
    err = usage_error(barca(project, "pipeline.py", "mid"))
    assert "\nbarca get mid pipeline.py\n" in err


def test_several_non_py_positionals_after_a_file_state_the_rule_without_guessing(project):
    err = usage_error(barca(project, "run", "pipeline.py", "report", "mid"))
    assert "error: the target comes before the files" in err
    assert "Usage: barca run <TARGET> <FILES>..." in err
    assert "\nbarca run report pipeline.py\n" not in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."


def test_run_without_a_target(project):
    err = usage_error(barca(project, "run", "pipeline.py"))
    assert "error: a target task is required" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."


def test_target_without_files_reads_the_whole_project(project):
    # No files means tree discovery (#202), so a bare target just works...
    ran = barca(project, "run", "report")
    assert ran.returncode == 0, ran.stderr
    # ...and misuse errors point at the project-wide `barca list`.
    err = usage_error(barca(project, "get", "report"))
    assert "'report' is a task" in err
    assert last_line(err) == "Run `barca list` to see available assets and tasks."


def test_space_separated_refresh_still_says_to_use_commas(project):
    err = usage_error(barca(project, "run", "report", "pipeline.py", "--refresh", "src", "mid"))
    assert "'mid' is not a .py file" in err
    assert "--refresh src,mid" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."


def test_unknown_refresh_name(project):
    err = usage_error(barca(project, "run", "report", "pipeline.py", "--refresh", "nope"))
    assert "no upstream asset named 'nope'" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."


def test_unknown_target(project):
    for cmd in ("get", "run"):
        err = usage_error(barca(project, cmd, "nope", "pipeline.py"))
        assert "'nope' not found" in err
        assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."


def test_task_asset_misuse(project):
    err = usage_error(barca(project, "get", "report", "pipeline.py"))
    assert "use `barca run` instead" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."
    err = usage_error(barca(project, "run", "mid", "pipeline.py"))
    assert "use `barca get` instead" in err


def test_a_correct_command_still_succeeds(project):
    assert barca(project, "run", "report", "pipeline.py").returncode == 0


def test_human_mode_prints_the_prose(project):
    proc = barca(project, "run", "pipeline.py", "report", "-o", "pretty")
    assert proc.returncode == 2
    err = proc.stderr
    assert err.startswith("error: the target comes before the files\n\n")
    assert "\n  barca run report pipeline.py -o pretty\n" in err
    assert last_line(err) == "Run `barca list pipeline.py` to see available assets and tasks."
