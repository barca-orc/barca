"""How a `parallel()` branch's return value gets back to the step that called it.

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

A large value never travels over the socket: the caller reads it from its file. The text of a
small json artifact (a few KB at most) is sent along with its path, as it is on disk, so that
a fan-out of thousands of small branches does not open thousands of files here.
Branch artifacts stay in the artifact directory under the branch's name and number and are
overwritten by later runs, as before.
"""

import json
import os

# `error_type` of a branch that finished but whose return value could not be written. No
# Python class name contains a dot, so no exception raised by user code reports this.
UNRETURNABLE = "barca.BranchResultError"


def describe(fn_ref: str) -> str:
    """`pipeline.py:work` for a branch's `"/abs/path/pipeline.py:work"`."""
    source, _, name = fn_ref.rpartition(":")
    return f"{os.path.basename(source)}:{name}" if source else name


def unwritable(step: dict, value, exc: BaseException) -> Exception:
    """The error a branch's worker reports when the branch's return value cannot be written."""
    from barca import BranchResultError

    fn = describe(f"{step.get('source_file', '')}:{step.get('function_name', '?')}")
    return BranchResultError(
        f"{fn} returned a {type(value).__module__}.{type(value).__qualname__}, which cannot be "
        f"passed back to the step that called parallel(): {type(exc).__name__}: {exc}. "
        "A branch may return anything a step may return: a value JSON or pickle can represent, "
        "or a DataFrame."
    )


def frame_type(value, fmt: str) -> str | None:
    """The reader that gives back the type `value` has, when it was written as parquet."""
    if fmt != "parquet":
        return None
    from barca._artifacts import _frame_kind

    kind = _frame_kind(value)
    if kind == "polars" and type(value).__name__ == "LazyFrame":
        return "polars_lazy"
    return kind


def _read(artifact: dict, source_file: str):
    path, fmt = artifact["path"], artifact["format"]
    text = artifact.get("json")
    if text is not None:
        # The file's text, sent along by the coordinator for a small json artifact.
        return json.loads(text)
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
        from barca._worker import load_module

        load_module(source_file)
        return deserialize(path, fmt)


def collect(results: list[dict], items: list[dict]) -> list:
    """Turn the coordinator's answer to `submit` into what `parallel()` returns.

    One entry per branch, in order: its return value, or a `ParallelError` if it raised.
    Raises `BranchResultError` if any branch returned a value that could not be passed.
    """
    from barca import BranchResultError, ParallelError

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
            # A coordinator from 0.18.1 or earlier: JSON values inline.
            out.append(result.get("result"))
            continue
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
