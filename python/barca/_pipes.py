"""Keep a helper process working when the reader of its output goes away (#286).

A worker's stdout and stderr are the coordinator's stderr. When that is a pipe whose reader
has exited (`barca run deploy 2>&1 | head -5`), Python raises BrokenPipeError from the next
`print()`, inside the user's step, and the step would fail because nobody was reading its
output. `install()` wraps `sys.stdout` and `sys.stderr` so that the first such error points the
file descriptor at the null device instead: the text is dropped, the step goes on, and later
writes (from this process, its C extensions and its child processes) succeed.
"""

from __future__ import annotations

import os
import sys


class _PipeGuard:
    """A text stream that drops output once its pipe is closed, instead of raising."""

    def __init__(self, stream):
        self._stream = stream

    def __getattr__(self, name):
        return getattr(self._stream, name)

    def __iter__(self):
        return iter(self._stream)

    def _to_devnull(self):
        try:
            fd = self._stream.fileno()
            devnull = os.open(os.devnull, os.O_WRONLY)
            try:
                os.dup2(devnull, fd)
            finally:
                os.close(devnull)
        except Exception:
            pass

    def write(self, s):
        try:
            return self._stream.write(s)
        except BrokenPipeError:
            self._to_devnull()
            return len(s)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        try:
            self._stream.flush()
        except BrokenPipeError:
            self._to_devnull()


def install() -> None:
    """Wrap `sys.stdout` and `sys.stderr` (once) so a closed pipe drops output."""
    for name in ("stdout", "stderr"):
        stream = getattr(sys, name)
        if stream is not None and not isinstance(stream, _PipeGuard):
            setattr(sys, name, _PipeGuard(stream))
