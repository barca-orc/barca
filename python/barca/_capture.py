"""A step's stdout and stderr, recorded line by line and sent to the coordinator as `log` frames.

The coordinator owns display policy: user stdout never reaches the worker's own descriptors,
stderr is written through as it comes and recorded as well (`_StderrTee`). One stream of each
kind lives for the whole worker and follows the current step, so a handler made at import time
(`logging.basicConfig()`) keeps recording under the right node id.
"""

from __future__ import annotations

import contextlib
import io
import sys
from collections.abc import Iterator


class _LineEmitter(io.TextIOBase):
    """Capture Python stdout as complete node-attributed metadata lines.

    The coordinator owns display policy; this stream never writes user text directly
    to the worker's inherited descriptors. Partial lines flush on return or failure.
    """

    stream = "stdout"

    def __init__(self, node_id: str) -> None:
        self.node_id = node_id
        self._buf = ""
        # The coordinator renders this metadata event; user stdout never bypasses it.

    def write(self, s: str) -> int:
        self._buf += s
        while "\n" in self._buf:
            line, self._buf = self._buf.split("\n", 1)
            self._record(line)
        return len(s)

    def flush(self) -> None:
        if self._buf and not sys.is_finalizing():
            line, self._buf = self._buf, ""
            self._record(line)

    def _record(self, line: str) -> None:
        from barca import _runtime

        _runtime.emit_log(self.node_id, line, self.stream)


class _StderrTee(_LineEmitter):
    """Python-level stderr of a step: written through to the worker's own stderr as it comes
    (what the terminal, or `barca serve`'s stderr, showed before), and recorded line by line
    as the step's `stderr` stream. That covers `logging`'s default handler, `warnings` and
    anything printed to `sys.stderr`; writes to file descriptor 2 (native code, subprocesses)
    do not pass through here. barca's own `[barca]` lines are written but not recorded."""

    stream = "stderr"

    def __init__(self, node_id: str, out: io.TextIOBase) -> None:
        super().__init__(node_id)
        self._out = out

    @property
    def encoding(self) -> str:  # ty: ignore[invalid-property-type-override]
        return getattr(self._out, "encoding", "utf-8")

    @property
    def errors(self) -> str:  # ty: ignore[invalid-property-type-override]
        return getattr(self._out, "errors", "strict")

    def fileno(self) -> int:
        # Code that needs a descriptor (faulthandler, subprocess) gets the real one, and its
        # writes go there uncaptured.
        return self._out.fileno()

    def isatty(self) -> bool:
        try:
            return self._out.isatty()
        except (AttributeError, ValueError, OSError):
            return False

    def write(self, s: str) -> int:
        try:
            self._out.write(s)
        except (AttributeError, ValueError, OSError):
            pass
        return super().write(s)

    def flush(self) -> None:
        super().flush()
        try:
            self._out.flush()
        except (AttributeError, ValueError, OSError):
            pass

    def _record(self, line: str) -> None:
        # A progress bar redraws its line with \r: record what the terminal ended up showing.
        if "\r" in line:
            line = line.rstrip("\r").rsplit("\r", 1)[-1]
        if line.startswith(("[barca]", "BARCA:")):
            return
        super()._record(line)


_step_stdout: _LineEmitter | None = None
_step_stderr: _StderrTee | None = None


def stdout_for_step(node_id: str) -> _LineEmitter:
    """The one captured stdout, now attributed to `node_id`."""
    # Import-time stdout handlers may retain this stream. Reuse it so they follow
    # the current step rather than retaining whichever step first imported a module.
    global _step_stdout
    if _step_stdout is None:
        _step_stdout = _LineEmitter(node_id)
    else:
        _step_stdout.flush()
        _step_stdout.node_id = node_id
    return _step_stdout


def stderr_for_step(node_id: str) -> _StderrTee:
    """The one captured stderr, now attributed to `node_id`."""
    # As stdout_for_step: a logging handler made at import time (logging.basicConfig())
    # keeps this stream, so it is one stream that follows the current step.
    global _step_stderr
    if _step_stderr is None:
        _step_stderr = _StderrTee(node_id, sys.stderr)  # ty: ignore[invalid-argument-type]
    else:
        _step_stderr.flush()
        _step_stderr.node_id = node_id
    return _step_stderr


@contextlib.contextmanager
def captured(out: _LineEmitter, err: _StderrTee) -> Iterator[None]:
    """Run a block with `sys.stdout` and `sys.stderr` recorded; trailing partial lines are
    emitted when it ends, even if it raised."""
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            yield
        finally:
            # Emit any trailing partial line, even if the step raised.
            out.flush()
            err.flush()
