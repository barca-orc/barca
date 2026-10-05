"""Collapse repeated warnings from library logging and `warnings` in a worker.

A library that warns on every call (aiohttp's "Could not parse .netrc file" on each client
session, for one) prints the same line hundreds of times in a long run and buries the progress
output. Workers write straight to the terminal (they inherit barca's stderr), so the only place
to collapse the repeats is here, in the worker process.

What is collapsed: a `logging` record at exactly WARNING level that is printed by logging's
last-resort handler, i.e. only because nobody configured logging (that is how a library's
warning reaches the terminal in a step that sets up no logging), and a warning shown by the
`warnings` module. A handler that the project or a library installed is never touched: once
logging is configured, every record is printed. `print`, raw writes to stdout/stderr, records at
other levels, records with a traceback attached, and exceptions all pass unchanged.

The first occurrence of a text in a run is printed where it happens. "First in the run" is
decided across workers with a claim file per text in a directory the coordinator creates next to
its socket (`O_CREAT | O_EXCL`: one worker wins). Later occurrences are counted, and the counts
go back to the coordinator ahead of each step result (`_runtime`), which prints
`[barca] N more: <text>` once the run is over.
"""

import hashlib
import logging
import os
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


def install(socket_path: str | None) -> None:
    """Replace `logging.lastResort` and hook `warnings.showwarning` (once per process)."""
    global _claim_dir, _installed
    if _installed:
        return
    _installed = True
    if socket_path:
        _claim_dir = socket_path + ".warnings"

    last_resort = logging.lastResort
    if last_resort is not None:  # None: the project turned the last-resort output off

        class _LastResort(type(last_resort)):
            def emit(self, record):
                if (
                    record.levelno == logging.WARNING
                    and not record.exc_info
                    and not record.stack_info
                ):
                    try:
                        repeat = is_repeat(record.getMessage())
                    except Exception:
                        repeat = False
                    if repeat:
                        return
                super().emit(record)

        logging.lastResort = _LastResort(last_resort.level)

    showwarning = warnings.showwarning

    def show(message, category, filename, lineno, file=None, line=None):
        if file is None and is_repeat(f"{category.__name__}: {message}"):
            return
        showwarning(message, category, filename, lineno, file, line)

    warnings.showwarning = show  # ty: ignore[invalid-assignment]
