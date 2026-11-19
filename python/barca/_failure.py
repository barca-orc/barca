"""How a failed step is reported: where it failed, the traceback shown, and the message.

The worker's runners (`barca/_worker.py`) and the per-key writes of a batched call
(`barca/_batch.py`) report through this, so a failure reads the same wherever it is caught.
The report itself is `_runtime.emit_step_error`.
"""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path

from barca import _duckdb, _sinks

# Where a step failed (the `phase` of a `step_error`). A closed set: the coordinator reads any
# other value as unknown. Only `user_code` is the step's own code; the others are barca's.
USER_CODE = "user_code"  # importing the module, calling the function, checking what it returned
LOAD_INPUTS = "load_inputs"  # reading the step's input artifacts (collect() fan-in included)
SPLIT_OUTPUT = "split_output"  # dividing a batched call's result into its keys
WRITE_OUTPUT = "write_output"  # writing the result to its artifact
SINK = "sink"  # writing the step's @sink files
PHASES = (USER_CODE, LOAD_INPUTS, SPLIT_OUTPUT, WRITE_OUTPUT, SINK)


def traceback_for(exc: BaseException, phase: str) -> str:
    """The traceback a failure in `phase` reports: the user's frames for a failure in user
    code, every frame for a failure in barca's own code, which has nowhere else to show."""
    if phase == USER_CODE:
        return user_traceback(exc)
    import traceback

    return "".join(traceback.format_list(traceback.extract_tb(exc.__traceback__))).rstrip("\n")


def output_phase(exc: BaseException) -> str:
    """The phase of a failure while a result was written: its sinks, or its artifact."""
    return SINK if isinstance(exc, _sinks.SinkError) else WRITE_OUTPUT


def user_traceback(exc: BaseException) -> str:
    """Format the traceback with barca-internal frames stripped.

    Keeps only frames from user code so surfaced errors point at the user's
    file/line, never at _worker.py plumbing. Returns "" when every frame is
    internal (e.g. a TypeError raised by the fn(**kwargs) call itself) — the
    caller always leads with "ErrorType: message", which carries the detail.
    """
    import importlib
    import traceback  # only on failure: it costs ~10 ms at start-up on Python 3.14

    barca_dir = str(Path(__file__).resolve().parent)
    importlib_dir = str(Path(importlib.__file__).resolve().parent)

    def internal(filename: str) -> bool:
        # importlib's own frames (`<frozen importlib._bootstrap>`, importlib/__init__.py) say
        # nothing about the user's code: an import that fails shows them and nothing else.
        if filename.startswith("<frozen importlib"):
            return True
        path = str(Path(filename).resolve())
        return path.startswith((barca_dir, importlib_dir))

    frames = [f for f in traceback.extract_tb(exc.__traceback__) if not internal(f.filename)]
    text = "".join(traceback.format_list(frames))
    if isinstance(exc, SyntaxError) and exc.filename:
        # Where the syntax error is (file, line, the source line and a caret), which no frame
        # shows; the caller leads with "SyntaxError: message", so that last line is left out.
        text += "".join(traceback.format_exception_only(exc)[:-1])
    return text.rstrip("\n")


def failure_message(exc: BaseException, bound_views: Sequence[str] = ()) -> str:
    """What a failed step's error says: the exception's text, with a note when the step called
    `sys.exit()` and, for a DuckDB error from mixing connections, what to do instead."""
    message = str(exc)
    if isinstance(exc, SystemExit):
        message = (
            f"{message} (the step called sys.exit(); a step must return a value or raise "
            "an exception, and barca reports any sys.exit() as a failure)"
        )
    note = _duckdb.explain_error(exc, list(bound_views))
    if note:
        message = f"{message}\n\n{note}"
    return message
