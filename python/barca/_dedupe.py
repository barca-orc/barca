"""Collapse repeated warnings from library logging and `warnings` in a worker.

A library that warns on every call (aiohttp's "Could not parse .netrc file" on each client
session, for one) prints the same line hundreds of times in a long run and buries the progress
output. Workers write straight to the terminal (they inherit barca's stderr), so the only place
to collapse the repeats is here, in the worker process.

What is collapsed: a `logging` record at exactly WARNING level on its way to a `StreamHandler`
writing to the terminal (that includes logging's last-resort handler, which is what prints a
library's warning when the step configured no logging), and a warning shown by the `warnings`
module. Nothing else is touched: `print`, raw writes to stdout/stderr, records at other levels,
records with a traceback attached, handlers writing to files, and exceptions all pass unchanged.

The first occurrence of a text in a run is printed where it happens, by whichever handler would
have printed it. "First in the run" is decided across workers with a claim file per text in a
directory the coordinator creates next to its socket (`O_CREAT | O_EXCL`: one worker wins).
Later occurrences are counted, and the counts go back to the coordinator ahead of each step
result (`_runtime`), which prints `[barca] N more: <text>` once the run is over.

`BARCA_WARNINGS=all` turns this off.
"""

import hashlib
import logging
import os
import sys
import threading
import warnings

# Distinct texts tracked per worker. A library that puts a counter or an id in each warning
# produces unbounded distinct texts; past this many they are printed as they come.
_MAX_DISTINCT = 512

_lock = threading.Lock()
_seen: set[str] = set()
_repeats: dict[str, int] = {}
_claim_dir: str | None = None
_installed = False


def _claim(text: str) -> bool:
    """True if this process is the first in the run to see `text` (or cannot tell)."""
    if _claim_dir is None:
        return True
    name = hashlib.sha256(text.encode("utf-8", "replace")).hexdigest()
    try:
        os.close(os.open(os.path.join(_claim_dir, name), os.O_CREAT | os.O_EXCL | os.O_WRONLY))
    except FileExistsError:
        return False
    except OSError:
        return True  # no claim directory: fall back to once per worker
    return True


def is_repeat(text: str) -> bool:
    """Record one occurrence of `text`; True if it has already been printed in this run."""
    if not text:
        return False
    with _lock:
        if text not in _seen:
            if len(_seen) >= _MAX_DISTINCT:
                return False
            _seen.add(text)
            if _claim(text):
                return False
        _repeats[text] = _repeats.get(text, 0) + 1
        return True


def take() -> dict[str, int]:
    """Repeat counts since the last call, keyed by the first line of each text."""
    if not _repeats:
        return {}
    with _lock:
        out: dict[str, int] = {}
        for text, n in _repeats.items():
            line = text.strip().split("\n", 1)[0].strip()
            out[line] = out.get(line, 0) + n
        _repeats.clear()
    return out


def _to_terminal(handler) -> bool:
    stream = getattr(handler, "stream", None)
    return (
        stream is sys.stderr
        or stream is sys.stdout
        or stream is sys.__stderr__
        or stream is sys.__stdout__
    )


def install(socket_path: str | None) -> None:
    """Hook `logging.StreamHandler.emit` and `warnings.showwarning` (once per process)."""
    global _claim_dir, _installed
    if _installed or os.environ.get("BARCA_WARNINGS", "").strip().lower() == "all":
        return
    _installed = True
    if socket_path:
        _claim_dir = socket_path + ".warnings"

    stream_emit = logging.StreamHandler.emit

    def emit(self, record):
        if (
            record.levelno == logging.WARNING
            and not record.exc_info
            and not record.stack_info
            and _to_terminal(self)
        ):
            # Decide once per record: it may reach several terminal handlers.
            repeat = record.__dict__.get("_barca_repeat")
            if repeat is None:
                try:
                    repeat = is_repeat(record.getMessage())
                except Exception:
                    repeat = False
                record.__dict__["_barca_repeat"] = repeat
            if repeat:
                return
        stream_emit(self, record)

    logging.StreamHandler.emit = emit  # ty: ignore[invalid-assignment]

    showwarning = warnings.showwarning

    def show(message, category, filename, lineno, file=None, line=None):
        if file is None and is_repeat(f"{category.__name__}: {message}"):
            return
        showwarning(message, category, filename, lineno, file, line)

    warnings.showwarning = show  # ty: ignore[invalid-assignment]
