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

A step is covered whatever it does: its `print()`, a child process it starts, a raw write to
descriptor 1 or 2 and C stdio all go to a pipe barca owns, never to the caller's.

The tests do not race a reader: barca is started with a pipe whose read end is already
closed, so its first write fails, every time. The one test of a reader that closes after
reading part of the output uses an output larger than any pipe buffer, so barca is still
writing when the reader leaves.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
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


# ─── What a step does below Python's print ────────────────────────────────────
#
# A worker's stdout and stderr are barca's stderr. When that is a pipe, the workers get a pipe
# barca owns and barca forwards (`crates/barca-core/src/term.rs`), so nothing a step does can
# meet a pipe without a reader: not a child process, not a raw write, not a C library.

CHILDREN = """
import ctypes
import os
import subprocess
import sys
import time

from barca import task


@task()
def child() -> int:
    done = subprocess.run(["sh", "-c", "echo child-out; echo child-err >&2; echo child-again"])
    # -13 here is the child killed by SIGPIPE.
    assert done.returncode == 0, done.returncode
    return done.returncode


@task()
def raw_writes() -> int:
    for i in range(3000):
        os.write(1, b"fd1 line %d\\n" % i)
        os.write(2, b"fd2 line %d\\n" % i)
    return 3000


@task()
def c_library() -> int:
    libc = ctypes.CDLL(None)
    for _ in range(3000):
        assert libc.puts(b"a line from C stdio") >= 0
    assert libc.fflush(None) == 0
    return 3000


@task()
def shell_pipeline() -> str:
    # SIGPIPE is at its default for what a step starts: `yes` ends when `head` has its line.
    done = subprocess.run("yes | head -1", shell=True, capture_output=True, text=True, timeout=30)
    assert done.returncode == 0, done
    return done.stdout.strip()


@task()
def ordered() -> int:
    os.write(2, b"marker: start\\n")
    for i in range(200):
        os.write(1, b"out %d\\n" % i)
        os.write(2, b"err %d\\n" % i)
    subprocess.run(["sh", "-c", "echo child-out; echo child-err >&2"], check=True)
    print("printed to stdout")
    print("printed to stderr", file=sys.stderr)
    return 200


@task()
def leaves_a_process() -> int:
    # Started by the step and still running when barca exits (`late.py`, below).
    stream, redirect = open("leave").read().split()
    out = subprocess.DEVNULL if redirect == "redirected" else None
    child = subprocess.Popen(
        [sys.executable, "late.py", stream], stdout=out, stderr=out, start_new_session=True
    )
    while not os.path.exists("locked"):
        assert child.poll() is None
        time.sleep(0.01)
    return child.pid


@task()
def flood() -> int:
    open("started", "w").close()
    for i in range(20000):
        os.write(2, b"flood line %06d\\n" % i)
    return 20000
"""

CLOSED = {
    "stdout closed": {"stdout_closed": True},
    "stderr closed": {"stderr_closed": True},
    "both closed": {"stdout_closed": True, "stderr_closed": True},
}


# What `leaves_a_process` starts: holds a lock for as long as it lives, waits for `go`, writes
# to the descriptor named in argv, then records that it got that far.
LATE = """
import fcntl
import os
import sys
import time

lock = open("alive.lock", "w")
fcntl.flock(lock, fcntl.LOCK_EX)
open("locked", "w").close()
while not os.path.exists("go"):
    time.sleep(0.01)
os.write(int(sys.argv[1]), b"late line")
open("survived", "w").close()
"""


@pytest.fixture
def children(tmp_path: Path) -> Path:
    (tmp_path / "pipeline.py").write_text(CHILDREN)
    (tmp_path / "late.py").write_text(LATE)
    yield tmp_path
    # Whatever a failed test left waiting ends now.
    (tmp_path / "go").touch()


@pytest.mark.parametrize("closed", list(CLOSED))
@pytest.mark.parametrize("step", ["child", "raw_writes", "c_library", "shell_pipeline"])
def test_a_step_that_writes_below_python_is_not_failed_by_a_closed_stream(
    children: Path, step: str, closed: str
) -> None:
    """A child process, `os.write` to descriptors 1 and 2, and C stdio. Before barca owned the
    workers' pipe, the child was killed by SIGPIPE and the raw write raised BrokenPipeError
    whenever barca's stderr had lost its reader: the step failed because nobody was reading."""
    proc = run_barca(children, "run", step, "pipeline.py", "--json", **CLOSED[closed])
    assert proc.returncode == 0, proc.stderr
    if proc.stderr is not None:
        assert_quiet(proc)
    if proc.stdout is not None:
        result = json.loads(proc.stdout)
        assert result["status"] == "success"
        assert (
            result["final_output"]
            == {
                "child": 0,
                "raw_writes": 3000,
                "c_library": 3000,
                "shell_pipeline": "y",
            }[step]
        )
    assert history(children) == [("run", step, "success", 1)]


def step_output(stderr: str) -> list[str]:
    """What the step wrote, without barca's own lines."""
    return [line for line in stderr.splitlines() if not line.startswith("[barca]")]


def test_with_a_reader_a_steps_output_is_what_it_always_was(children: Path) -> None:
    """Normal operation. The order of a step's writes to its two streams, of its child's, and
    of barca's own line that follows them is kept, and the output through barca's pipe (stderr
    is a pipe) is the same as when the workers hold stderr itself (stderr is a file, the path
    that did not change)."""
    piped = run_barca(children, "run", "ordered", "pipeline.py", "--agent", "--json")
    assert piped.returncode == 0, piped.stderr
    lines = piped.stderr.splitlines()

    expected = ["marker: start"]
    for i in range(200):
        expected += [f"out {i}", f"err {i}"]
    expected += ["child-out", "child-err"]
    start = lines.index("marker: start")
    assert lines[start : start + len(expected)] == expected
    # Python's own buffered print() lines come when Python flushes them, as before.
    assert "printed to stdout" in lines and "printed to stderr" in lines
    # barca's line about the step comes after everything the step wrote.
    completed = next(
        i for i, line in enumerate(lines) if "step:pipeline.py:ordered completed" in line
    )
    assert completed > max(lines.index("printed to stdout"), lines.index("printed to stderr"))
    # Nothing of it is on stdout: that is the result alone.
    assert json.loads(piped.stdout)["final_output"] == 200

    with open(children / "stderr.txt", "w") as to_file:
        direct = subprocess.run(
            [_find_binary(), "run", "ordered", "pipeline.py", "--agent", "--json"],
            cwd=children,
            stdout=subprocess.PIPE,
            stderr=to_file,
            text=True,
            timeout=120,
        )
    assert direct.returncode == 0
    assert step_output((children / "stderr.txt").read_text()) == step_output(piped.stderr)


def test_a_reader_that_falls_behind_loses_nothing(children: Path) -> None:
    """A slow reader: nothing is read until the step has started, by which time it is writing
    far more than barca's pipe and the caller's can hold together. The step waits, as it did on
    the caller's pipe; every line then arrives, in order, and barca keeps no backlog in memory
    (it copies through a fixed buffer)."""
    proc = subprocess.Popen(
        [_find_binary(), "run", "flood", "pipeline.py", "--json"],
        cwd=children,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    assert proc.stderr is not None
    deadline = time.monotonic() + 60
    while not (children / "started").exists():
        assert proc.poll() is None and time.monotonic() < deadline
        time.sleep(0.01)
    # 20,000 lines of 18 bytes: 360 kB against two 64 kB pipes.
    received = [line for line in proc.stderr if line.startswith("flood line")]
    stdout, _ = proc.communicate(timeout=60)
    assert proc.returncode == 0
    assert received == [f"flood line {i:06d}\n" for i in range(20000)]
    assert json.loads(stdout)["final_output"] == 20000


def wait_until_full(fd: int) -> int:
    """Wait until the pipe read from `fd` holds data and has stopped filling: its writer has
    more to write (the tests below write far more than a pipe holds) and cannot."""
    import fcntl
    import struct
    import termios

    def held() -> int:
        return struct.unpack("i", fcntl.ioctl(fd, termios.FIONREAD, b"\0\0\0\0"))[0]

    deadline = time.monotonic() + 60
    last, since = -1, time.monotonic()
    while True:
        assert time.monotonic() < deadline
        now = held()
        if now != last:
            last, since = now, time.monotonic()
        elif now > 0 and time.monotonic() - since > 0.5:
            return now
        time.sleep(0.01)


def test_a_non_blocking_stderr_whose_reader_stalls_loses_nothing(children: Path) -> None:
    """The caller's pipe is in non-blocking mode (an event loop's pipe is). While the reader
    is behind, a write to it fails with EAGAIN. That is not a closed stream: barca waits until
    it can write, and when the reader resumes every line arrives, in order."""
    read_end, write_end = os.pipe()
    os.set_blocking(write_end, False)
    proc = subprocess.Popen(
        [_find_binary(), "run", "flood", "pipeline.py", "--json"],
        cwd=children,
        stdout=subprocess.PIPE,
        stderr=write_end,
    )
    os.close(write_end)
    # Stall: read nothing until the pipe is full and has stayed full.
    assert wait_until_full(read_end) > 0
    assert proc.poll() is None
    with os.fdopen(read_end, "rb") as stderr:
        received = [line for line in stderr if line.startswith(b"flood line")]
    stdout, _ = proc.communicate(timeout=60)
    assert proc.returncode == 0
    assert received == [b"flood line %06d\n" % i for i in range(20000)]
    assert json.loads(stdout)["final_output"] == 20000


def test_a_non_blocking_stdout_whose_reader_stalls_loses_nothing(project: Path) -> None:
    """The same on stdout, with a result larger than a pipe holds."""
    whole = run_barca(project, "docs", "--all")
    assert whole.returncode == 0 and len(whole.stdout.encode()) > 128 * 1024

    read_end, write_end = os.pipe()
    os.set_blocking(write_end, False)
    proc = subprocess.Popen(
        [_find_binary(), "docs", "--all"], cwd=project, stdout=write_end, stderr=subprocess.PIPE
    )
    os.close(write_end)
    assert wait_until_full(read_end) > 0
    assert proc.poll() is None
    with os.fdopen(read_end, "rb") as stdout:
        received = stdout.read()
    _, stderr = proc.communicate(timeout=60)
    assert (proc.returncode, stderr) == (0, b"")
    assert received == whole.stdout.encode()


def is_running(cwd: Path) -> bool:
    """Whether the process started from `late.py` is alive: it holds `alive.lock` until it
    ends, however it ends."""
    import fcntl

    with open(cwd / "alive.lock", "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
    return False


@pytest.mark.parametrize("stream", ["1", "2"])
def test_a_process_a_step_leaves_running_loses_its_output_pipe_when_barca_exits(
    children: Path, stream: str
) -> None:
    """New with barca's own pipe (`barca docs contract`, "A closed stdout or stderr"): when
    barca's stderr is a pipe, a process a step leaves behind holds barca's pipe as its stdout
    and stderr, not the caller's. Once barca has exited, its next write to either ends it
    (SIGPIPE). Redirecting its output when starting it is the remedy; with barca's stderr a
    file, it holds the file and nothing changes."""

    def leave(redirect: str, stderr) -> bool:
        for name in ("go", "survived", "locked"):
            (children / name).unlink(missing_ok=True)
        (children / "leave").write_text(f"{stream} {redirect}")
        proc = subprocess.run(
            [_find_binary(), "run", "leaves_a_process", "pipeline.py", "--json"],
            cwd=children,
            stdout=subprocess.PIPE,
            stderr=stderr,
            timeout=120,
        )
        assert proc.returncode == 0
        assert is_running(children)  # barca has exited; the process it left has not
        (children / "go").touch()
        deadline = time.monotonic() + 60
        while is_running(children):
            assert time.monotonic() < deadline
            time.sleep(0.01)
        return (children / "survived").exists()

    # barca's stderr is a pipe: the write after barca's exit ends the process.
    assert leave("inherited", subprocess.PIPE) is False
    # The remedy: start it with its output redirected.
    assert leave("redirected", subprocess.PIPE) is True
    # barca's stderr is a file: the process holds the file, as it always did.
    with open(children / "stderr.txt", "w") as to_file:
        assert leave("inherited", to_file) is True
