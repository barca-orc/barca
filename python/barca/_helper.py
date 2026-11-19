"""The envelope of the helpers Rust runs as `python -m barca._x`.

A helper is a short process that takes one request and answers with one JSON document on
stdout (`crates/barca-engine/src/helper_proc.rs`: `run_json`, `run_to_output`). What each one
is sent and what it answers is its own, and stays in its module; what they share is here:

- the request: one JSON document on stdin (`stdin_request`), or in an environment variable
  for a helper whose stdin is its lifeline (`env_request`);
- stdout: only the answer is written to it. While the helper works, `sys.stdout` is stderr, so
  what a library or unpickled user code prints cannot corrupt the answer (`answering`,
  `answer`). A long-lived helper that speaks a line protocol takes the file descriptor
  itself (`protocol_stdout`);
- the lifeline and the way out (`run`): start watching for a coordinator that is gone, run
  the helper, and exit with its code. A helper that reports failures as an `error: ...` line
  on stderr names how its errors are worded; the others leave the traceback, whose last line
  Rust reports (`helper_proc::last_line`).

A helper that reads its request from stdin cannot have the lifeline: stdin is the request,
and `BARCA_LIFELINE=stdin` is set by the launcher that keeps stdin open (`run_to_output`).
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
from collections.abc import Callable, Generator
from typing import IO, Any, NoReturn


def stdin_request() -> Any:
    """The request `run_json` wrote to stdin: one JSON document, then end-of-file."""
    return json.load(sys.stdin)


def env_request(name: str) -> Any:
    """The request in the environment variable `name` (`{}` when unset): for a helper whose
    stdin is its lifeline."""
    return json.loads(os.environ.get(name, "{}"))


@contextlib.contextmanager
def answering() -> Generator[IO[str]]:
    """Keep stdout for the answer: inside, `sys.stdout` is stderr, and the stream yielded is
    the real stdout to write the answer to."""
    out = sys.stdout
    sys.stdout = sys.stderr
    try:
        yield out
    finally:
        sys.stdout = out


def write_answer(
    out: IO[str], document: Any, *, newline: bool = False, default: Any = None
) -> None:
    """Write the answer, one JSON document, to `out`. `newline` and `default` are what each
    helper has always written (a trailing newline or none, `str` for values JSON lacks)."""
    out.write(json.dumps(document, default=default))
    if newline:
        out.write("\n")
    out.flush()


def answer(work: Callable[[], Any], *, newline: bool = False, default: Any = None) -> None:
    """Run `work` with stdout kept for the answer, then write what it returned as the answer.
    Nothing is written when `work` raises."""
    with answering() as out:
        document = work()
    write_answer(out, document, newline=newline, default=default)


def protocol_stdout() -> IO[str]:
    """Take stdout for a line protocol, for the life of the process: the stream returned is the
    only way to it, and file descriptor 1 becomes stderr, so neither Python's `print` nor a
    native library's write reaches the protocol."""
    out = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    return out


def run(
    main: Callable[[], int | None],
    *,
    lifeline: bool = False,
    describe: Callable[[Exception], str] | None = None,
) -> NoReturn:
    """What a helper's `if __name__ == "__main__"` does: watch the lifeline when the launcher
    gives one (`lifeline`), run `main` and exit with the code it returns.

    With `describe`, an exception ends the helper with `error: <describe(exc)>` on stderr and
    exit code 1 (what `helper_proc::helper_cause` reads). Without it the exception propagates:
    a traceback on stderr and exit code 1.
    """
    if lifeline:
        from barca import _lifeline

        _lifeline.watch()
    try:
        code = main()
    except Exception as exc:
        if describe is None:
            raise
        print(f"error: {describe(exc)}", file=sys.stderr)
        code = 1
    sys.exit(code)
