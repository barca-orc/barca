"""`parallel()`: run callables in other workers, and get each branch's return value back.

The branch runs in another worker, which writes what it returned as an artifact exactly as it
would write a step's output: json, pickle or parquet, chosen by type (`_artifacts.py`). The
coordinator tells the caller where each artifact is, and the caller reads it here with the
reader steps use for their inputs. So a branch may return whatever a step may return, and the
value arrives as a step's output arrives at the next step that reads it from disk:

- a value JSON can represent comes back as JSON gives it back (a tuple as a list, a dict's
  non-string keys as strings);
- anything else is pickled and comes back equal and of the same type;
- a frame is written as parquet and read with the reader for the type the branch returned
  (a step picks the reader from its parameter's annotation; the caller of `parallel()` has no
  annotation for a result, so the branch's worker records the type).

Nothing is ever replaced by `None`. A value that cannot be written, or cannot be read back, is
a `BranchResultError` raised from `parallel()`: the calling step fails and the run reports it.
(The coordinator used to read the artifacts itself and send the values inline. It could do
that for JSON only, and every other value reached the caller as `None`, with no error: #285.)

All branch results travel by file reference, including small JSON values.

Branch results are files of the run, not artifacts: `.barca/branches/<run>-<pid>/<group>/`,
one directory per parallel group, written nowhere else and never uploaded. The coordinator
removes a group's directory when the step that called `parallel()` ends (so a lazily read
frame stays readable for as long as that step runs), the run's directory when the run ends,
however it ends, and the directories of runs that were killed when the next run starts.
"""

from __future__ import annotations

import json
import os
from collections.abc import Callable, Iterable
from functools import partial
from typing import Any

# `error_type` of a branch that finished but whose return value could not be written. No
# Python class name contains a dot, so no exception raised by user code reports this.
UNRETURNABLE = "barca.BranchResultError"


class ParallelError:
    """Represents a failed branch in a parallel() call."""

    def __init__(self, error: str) -> None:
        self.error = error

    def __repr__(self) -> str:
        return f"ParallelError({self.error!r})"

    def __str__(self) -> str:
        return self.error

    def to_dict(self) -> dict[str, Any]:
        return {"__parallel_error__": True, "error": self.error}


class BranchResultError(RuntimeError):
    """A `parallel()` branch returned, and its return value could not be passed to the caller.

    Raised by `parallel()` in the calling step, which fails like any step that raises. A
    branch that itself raised is different: it comes back as a `ParallelError` in the list.

    The message names the branch, the type of the value and the reason: the value cannot be
    written as json, pickle or parquet (an open file, a lambda, a generator), or what was
    written cannot be read back in the calling worker.
    """


def parallel(*callables: partial[Any]) -> list[Any]:
    """Run callables in parallel across worker processes.

    Each argument should be a `functools.partial` wrapping a @task-decorated
    function. Returns a list of results (or ParallelError objects) in argument
    order.

    When running inside a barca worker (BARCA_SOCKET set), uses the Unix socket
    protocol to request Rust to dispatch branches as separate workers. A branch's
    return value comes back the way a step's output reaches the next step: written
    as an artifact (json, pickle or parquet, by type) and read from it here. A value
    that cannot be passed raises `BranchResultError`; it is never replaced by `None`.
    When running standalone, executes sequentially.
    """
    if not callables:
        return []

    # Build work items from partials
    items = []
    for c in callables:
        if hasattr(c, "func") and hasattr(c, "args") and hasattr(c, "keywords"):
            fn = c.func
            fn_name = getattr(fn, "__name__", type(fn).__name__)
            source_file = getattr(getattr(fn, "__code__", None), "co_filename", "") or ""
            fn_ref = f"{source_file}:{fn_name}" if source_file else fn_name
            items.append(
                {
                    "fn_ref": fn_ref,
                    "args": list(c.args),
                    "kwargs": dict(c.keywords) if c.keywords else {},
                }
            )
        else:
            raise TypeError(f"parallel() expects functools.partial objects, got {type(c).__name__}")

    from barca import _runtime

    if _runtime.is_worker() and _runtime.connect() is not None:
        # Inside a barca worker — dispatch via Unix socket to executor
        return collect(_runtime.submit_and_wait(items), items)

    # Not inside a worker — execute sequentially (standalone/testing)
    return _sequential(callables)


def _sequential(callables: tuple[partial[Any], ...]) -> list[Any]:
    """The standalone fallback: each branch in turn, in this process, a raise becoming its
    `ParallelError`, as it would be in a worker."""
    results: list[Any] = []
    for c in callables:
        try:
            results.append(c())
        except Exception as e:
            results.append(ParallelError(f"{type(e).__name__}: {e}"))
    return results


def parallel_map(fn: Callable[..., Any], items: Iterable[Any], **kwargs: Any) -> list[Any]:
    """Map a @task function over items in parallel.

    Sugar for `parallel(*(partial(fn, item, **kwargs) for item in items))`.
    """
    return parallel(*(partial(fn, item, **kwargs) for item in items))


def describe(fn_ref: str) -> str:
    """`pipeline.py:work` for a branch's `"/abs/path/pipeline.py:work"`."""
    source, _, name = fn_ref.rpartition(":")
    return f"{os.path.basename(source)}:{name}" if source else name


def unwritable(step: dict, value, exc: BaseException) -> Exception:
    """The error a branch's worker reports when the branch's return value cannot be written."""
    fn = describe(f"{step.get('source_file', '')}:{step.get('function_name', '?')}")
    return BranchResultError(
        f"{fn} returned a {type(value).__module__}.{type(value).__qualname__}, which cannot be "
        f"passed back to the step that called parallel(): {type(exc).__name__}: {exc}. "
        "A branch may return anything a step may return: a value JSON or pickle can represent, "
        "or a DataFrame."
    )


def reader_name(value: Any, fmt: str) -> str | None:
    """The name of the reader (`_artifacts.deserialize`'s `frame_type`) that gives back the type
    `value` has, when it was written as parquet or CSV; None for another format."""
    if fmt not in ("parquet", "csv"):
        return None
    from barca._artifacts import dataframe_library

    library = dataframe_library(value)
    if library == "polars" and type(value).__name__ == "LazyFrame":
        return "polars_lazy"
    return library


def _read(artifact: dict, source_file: str):
    path, fmt = artifact["path"], artifact["format"]
    if "json" in artifact or not path:
        raise ValueError("branch results require an artifact file reference; reinstall barca")
    if fmt == "json":
        # What `_artifacts.deserialize` gives for a local json artifact (the same parser on
        # the same bytes; `json.dump` writes ASCII), read with as few calls as it takes: a
        # fan-out of thousands of small branches spends its time here, mostly opening files.
        fd = os.open(path, os.O_RDONLY)
        try:
            chunks = []
            while chunk := os.read(fd, 1 << 20):
                chunks.append(chunk)
        finally:
            os.close(fd)
        return json.loads(chunks[0] if len(chunks) == 1 else b"".join(chunks))
    from barca._artifacts import deserialize

    try:
        return deserialize(path, fmt, frame_type=artifact.get("frame_type"))
    except (ModuleNotFoundError, AttributeError):
        if fmt != "pickle" or not source_file:
            raise
        # The value's class lives in the branch's module, which the branch's worker loaded
        # under barca's name for it. This worker may know the file only as an ordinary import.
        from barca._source_import import load_pipeline_module

        load_pipeline_module(source_file)
        return deserialize(path, fmt)


def serializable(value):
    """`value` with every `ParallelError` in it (a failed branch a step returns as part of its
    result) replaced by its dict, so the result can be written as JSON."""
    if isinstance(value, ParallelError):
        return value.to_dict()
    if isinstance(value, list):
        return [serializable(v) for v in value]
    return value


def collect(results: list[dict], items: list[dict]) -> list:
    """Turn the coordinator's answer to `submit` into what `parallel()` returns.

    One entry per branch, in order: its return value, or a `ParallelError` if it raised.
    Raises `BranchResultError` if any branch returned a value that could not be passed.
    """
    out: list = []
    lost: list[str] = []
    for index, result in enumerate(results):
        fn_ref = items[index]["fn_ref"] if index < len(items) else "?"
        if result.get("status") != "ok":
            error = result.get("error", "unknown")
            if error.startswith(UNRETURNABLE + ": "):
                message = error[len(UNRETURNABLE) + 2 :].split("\n", 1)[0]
                lost.append(f"parallel() branch {index}: {message}")
            out.append(ParallelError(error))
            continue
        artifact = result.get("artifact")
        if artifact is None:
            raise BranchResultError(
                "branch results require an artifact file reference; reinstall barca"
            )
        try:
            out.append(_read(artifact, fn_ref.rpartition(":")[0]))
        except Exception as exc:
            raise BranchResultError(
                f"parallel() branch {index}: the value {describe(fn_ref)} returned was written "
                f"to {artifact.get('path')} ({artifact.get('format')}) and could not be read "
                f"back: {type(exc).__name__}: {exc}"
            ) from exc
    if lost:
        more = f" (and {len(lost) - 1} more)" if len(lost) > 1 else ""
        raise BranchResultError(lost[0] + more)
    return out
