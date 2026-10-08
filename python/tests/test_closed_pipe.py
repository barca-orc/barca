"""barca does not panic, and loses no work, when the reader of its output goes away (#286).

Until 0.18.1 `barca list pipeline.py | head -1` could end in a Rust panic ("failed printing to
stdout: Broken pipe", exit 101), and `barca run deploy 2>&1 | head -5` abandoned the run in
the middle: the coordinator panicked on its next progress line and the run stayed
`interrupted` in the history.

The rule (`barca docs contract`, "A closed stdout or stderr"):

- a closed stdout or stderr never stops a command: a run finishes and is recorded;
- output for a closed stream is dropped, with no panic and no traceback;
- the exit code is the one the command would have had with a reader: 0 for a command that
  succeeded, and 1, 2, 3 or 130 for one with an error of its own. `barca list | grep -q x`
  under `set -o pipefail` therefore does not fail because grep stopped reading.

The tests do not race a reader: barca is started with a pipe whose read end is already
closed, so its first write fails, every time. The one test of a reader that closes after
reading part of the output uses an output larger than any pipe buffer, so barca is still
writing when the reader leaves.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
import sys

from barca import asset, task


@asset()
def raw() -> dict:
    # More than a pipe buffer holds, on both streams.
    for i in range(3000):
        print("raw says", i, "x" * 40)
        print("raw warns", i, "x" * 40, file=sys.stderr)
    return {"n": 1}


@asset(inputs={"raw": raw})
def report(raw: dict) -> int:
    return raw["n"]


@task(inputs={"report": report})
def publish(report: int) -> int:
    print("publishing", report, file=sys.stderr)
    return report


@task()
def broken() -> None:
    print("about to fail", file=sys.stderr)
    raise ValueError("no good")
"""


def closed_pipe() -> int:
    """The write end of a pipe nobody reads: every write to it fails with EPIPE."""
    read_end, write_end = os.pipe()
    os.close(read_end)
    return write_end


def run_barca(
    cwd: Path, *args: str, stdout_closed: bool = False, stderr_closed: bool = False
) -> subprocess.CompletedProcess:
    fds = []

    def stream(closed: bool):
        if not closed:
            return subprocess.PIPE
        fds.append(closed_pipe())
        return fds[-1]

    try:
        return subprocess.run(
            [_find_binary(), *args],
            cwd=cwd,
            stdout=stream(stdout_closed),
            stderr=stream(stderr_closed),
            text=True,
            timeout=120,
        )
    finally:
        for fd in fds:
            os.close(fd)


def assert_quiet(proc: subprocess.CompletedProcess) -> None:
    assert "panicked" not in proc.stderr, proc.stderr
    assert "Traceback" not in proc.stderr, proc.stderr
    assert "Broken pipe" not in proc.stderr, proc.stderr
    assert "BrokenPipe" not in proc.stderr, proc.stderr


def history(cwd: Path) -> list[tuple[str, str | None, str, int]]:
    proc = run_barca(cwd, "history", "--all", "--json")
    assert proc.returncode == 0, proc.stderr
    return [
        (r["command"], r["target"], r["status"], r["steps_executed"])
        for r in json.loads(proc.stdout)["runs"]
    ]


@pytest.fixture
def project(tmp_path: Path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


@pytest.fixture
def materialized(project: Path) -> Path:
    proc = run_barca(project, "get", "report", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    return project


# ─── Commands that only print ─────────────────────────────────────────────────


@pytest.mark.parametrize(
    "args",
    [
        ("list", "pipeline.py"),
        ("list", "pipeline.py", "--pretty"),
        ("plan", "pipeline.py"),
        ("status", "pipeline.py"),
        ("status", "pipeline.py", "--pretty"),
        ("status", "pipeline.py", "--sample", "2"),
        ("history",),
        ("history", "--pretty"),
        ("stats", "report", "pipeline.py"),
        ("stats", "report", "pipeline.py", "--pretty"),
        ("sql", "select * from report", "pipeline.py"),
        ("sql", "select * from report", "pipeline.py", "--pretty"),
        ("get", "report", "pipeline.py", "--dry-run"),
        ("get", "report", "pipeline.py", "--dry-run", "--pretty"),
        ("run", "publish", "pipeline.py", "--dry-run"),
        ("docs",),
        ("docs", "contract"),
        ("docs", "--all", "--json"),
        ("version",),
        ("--version",),
        ("--help",),
        ("get", "--help"),
        ("help", "run"),
    ],
)
def test_a_command_that_prints_exits_0_without_a_word(
    materialized: Path, args: tuple[str, ...]
) -> None:
    proc = run_barca(materialized, *args, stdout_closed=True)
    assert_quiet(proc)
    assert proc.returncode == 0, proc.stderr
    # Nothing on stderr is about the pipe (a plan warning or a note may still be printed).
    assert "stdout" not in proc.stderr and "pipe" not in proc.stderr.lower()

    # With stderr closed as well, the same.
    proc = run_barca(materialized, *args, stdout_closed=True, stderr_closed=True)
    assert proc.returncode == 0


def test_a_reader_that_leaves_after_part_of_the_output(project: Path) -> None:
    """`barca docs --all | head -5`: the reader takes five lines and closes."""
    whole = run_barca(project, "docs", "--all")
    assert whole.returncode == 0
    # Larger than a pipe buffer (64 KiB by default on Linux and at most on macOS), so barca
    # cannot have finished writing before the reader closes.
    assert len(whole.stdout.encode()) > 128 * 1024

    proc = subprocess.Popen(
        [_find_binary(), "docs", "--all"],
        cwd=project,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert proc.stdout is not None and proc.stderr is not None
    head = [proc.stdout.readline() for _ in range(5)]
    proc.stdout.close()
    stderr = proc.stderr.read().decode()
    proc.stderr.close()
    assert proc.wait(timeout=60) == 0
    assert b"".join(head) == "".join(whole.stdout.splitlines(keepends=True)[:5]).encode()
    assert stderr == ""


# ─── get and run ──────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "flags",
    [("--json",), ("--pretty",), ("--agent",), (), ("-o", "value")],
)
def test_a_run_whose_stdout_is_closed_finishes_and_is_recorded(
    project: Path, flags: tuple[str, ...]
) -> None:
    proc = run_barca(project, "run", "publish", "pipeline.py", *flags, stdout_closed=True)
    assert_quiet(proc)
    assert proc.returncode == 0, proc.stderr
    # The run was not cut short: its end-of-run line is there, and it is in the history as a
    # success with all three steps.
    assert "[barca] 3/3 steps | done in" in proc.stderr
    assert history(project) == [("run", "publish", "success", 3)]

    # What it computed is cached: the same target again runs only the task.
    again = run_barca(project, "run", "publish", "pipeline.py", "--json")
    assert again.returncode == 0, again.stderr
    statuses = {s["id"]: s["status"] for s in json.loads(again.stdout)["steps"]}
    assert statuses == {
        "pipeline.py:raw": "cached",
        "pipeline.py:report": "cached",
        "pipeline.py:publish": "ran",
    }


def test_get_with_several_targets_and_a_closed_stdout(project: Path) -> None:
    proc = run_barca(project, "get", "raw,report", "pipeline.py", stdout_closed=True)
    assert_quiet(proc)
    assert proc.returncode == 0, proc.stderr
    assert history(project) == [("get", "raw,report", "success", 2)]


@pytest.mark.parametrize("flags", [("--json",), ("--agent",), ("--pretty",)])
def test_a_closed_stderr_does_not_stop_a_run_or_change_its_exit_code(
    project: Path, flags: tuple[str, ...]
) -> None:
    """`barca run publish 2>&1 >result.json | head -1`: progress has no reader. The steps
    print thousands of lines to stdout and stderr on the way; none of that may fail them."""
    proc = run_barca(project, "run", "publish", "pipeline.py", *flags, stderr_closed=True)
    assert proc.returncode == 0
    if flags != ("--pretty",):
        result = json.loads(proc.stdout)
        assert result["status"] == "success"
        assert result["final_output"] == 1
        assert [s["status"] for s in result["steps"]] == ["ran", "ran", "ran"]
    assert history(project) == [("run", "publish", "success", 3)]


def test_a_run_with_both_streams_closed_finishes_and_is_recorded(project: Path) -> None:
    """`barca run publish 2>&1 | head -1`."""
    proc = run_barca(
        project, "run", "publish", "pipeline.py", "--agent", stdout_closed=True, stderr_closed=True
    )
    assert proc.returncode == 0
    assert history(project) == [("run", "publish", "success", 3)]


# ─── A command with an error of its own keeps its exit code ───────────────────


def test_a_failed_step_is_still_exit_1(project: Path) -> None:
    proc = run_barca(project, "run", "broken", "pipeline.py", "--json", stdout_closed=True)
    assert_quiet(proc)
    assert proc.returncode == 1
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "step_failed"
    assert envelope["node"] == "pipeline.py:broken"

    for closed in ({"stderr_closed": True}, {"stdout_closed": True, "stderr_closed": True}):
        proc = run_barca(project, "run", "broken", "pipeline.py", "--json", **closed)
        assert proc.returncode == 1, closed
    assert [h[2] for h in history(project)] == ["failed", "failed", "failed"]


def test_a_usage_error_is_still_exit_2(project: Path) -> None:
    proc = run_barca(project, "get", "nope", "pipeline.py", "--json", stdout_closed=True)
    assert_quiet(proc)
    assert proc.returncode == 2
    assert json.loads(proc.stderr.strip().splitlines()[-1])["kind"] == "usage"

    proc = run_barca(
        project, "get", "nope", "pipeline.py", "--json", stdout_closed=True, stderr_closed=True
    )
    assert proc.returncode == 2
    proc = run_barca(project, "get", "--no-such-flag", stdout_closed=True, stderr_closed=True)
    assert proc.returncode == 2


def test_a_write_that_fails_for_another_reason_is_an_error(materialized: Path) -> None:
    """Not a reader that left: stdout that cannot be written (here a terminal that has hung
    up, EIO; in practice also a full disk behind `> result.json`) is exit 3, and barca says so."""
    for flag, check in (
        ("--json", lambda err: json.loads(err.strip().splitlines()[-1])["kind"] == "infra"),
        ("--pretty", lambda err: err.startswith("could not write the result to stdout\n")),
    ):
        master, terminal = os.openpty()
        os.close(master)
        try:
            proc = subprocess.run(
                [_find_binary(), "list", "pipeline.py", flag],
                cwd=materialized,
                stdout=terminal,
                stderr=subprocess.PIPE,
                text=True,
                timeout=120,
            )
        finally:
            os.close(terminal)
        assert_quiet(proc)
        assert proc.returncode == 3, proc.stderr
        assert check(proc.stderr), proc.stderr


# ─── The worker's own guard ───────────────────────────────────────────────────


def test_the_pipe_guard_drops_output_and_keeps_the_descriptor_usable() -> None:
    """`barca._pipes`: what keeps a step's `print()` from raising in the worker."""
    import sys

    script = (
        "import os, sys\n"
        "from barca import _pipes\n"
        "_pipes.install(); _pipes.install()\n"
        "for i in range(3):\n"
        "    print('to stdout', i)\n"
        "    sys.stdout.flush()\n"
        "    print('to stderr', i, file=sys.stderr)\n"
        "os.write(1, b'raw write to the descriptor')\n"
        "os.write(2, b'raw write to the descriptor')\n"
        "assert sys.stdout.isatty() is False and sys.stderr.fileno() == 2\n"
    )
    out, err = closed_pipe(), closed_pipe()
    try:
        proc = subprocess.run([sys.executable, "-c", script], stdout=out, stderr=err)
    finally:
        os.close(out)
        os.close(err)
    assert proc.returncode == 0

    # An open stream is passed through unchanged.
    proc = subprocess.run(
        [sys.executable, "-c", "from barca import _pipes\n_pipes.install()\nprint('kept')"],
        capture_output=True,
        text=True,
    )
    assert (proc.returncode, proc.stdout) == (0, "kept\n")
