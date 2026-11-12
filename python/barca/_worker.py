"""Barca worker: runs steps the coordinator sends over its Unix domain socket.

Started by Rust as ``python -m barca._worker --daemon`` (`io_loop.rs`, `worker_command`).
The wire protocol is `specs/interfaces/worker-protocol.md`; this module has no DB access.

A step runs in three parts that `_run_daemon_step` (one step) and `_run_daemon_call` (the keys
of one batched call, RFC-0013) share: `_begin` imports the module and finds the function,
`_resolve_inputs` reads the inputs, `_call` runs the function with its output captured, and
`_materialize` writes the result and reports it. What differs is how a call's keys are
assembled and how its result is split and reported, one key at a time.
"""

from __future__ import annotations

import json
import os
import sys
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from barca import _duckdb, _hooks, _runtime, _sinks, _storage, _telemetry
from barca._artifacts import (
    _FORMAT_EXTENSIONS,
    FORMAT_OF_EXTENSION,
    artifact_path,
    batch_staging,
    clean_staging,
    deserialize,
    detect_format,
    resolve_format,
    serialize_for_upload,
    serialize_hashed,
)
from barca._capture import _LineEmitter, _StderrTee, captured, stderr_for_step, stdout_for_step
from barca._lru import _ArtifactLRU, artifact_size, cacheable_frame_type, is_cacheable_frame_type
from barca._rss import RssWindow
from barca._signals import ctrl_c_does_nothing as _ctrl_c_does_nothing
from barca._signals import ctrl_c_interrupts_the_step as _ctrl_c_interrupts_the_step
from barca._signals import exit_on_sigterm, join_the_job
from barca._source_import import activate_source_path, load_pipeline_module

Step = dict[str, Any]
Modules = dict[str, Any]


def _default_artifact_dir() -> str:
    """Artifact store root: BARCA_ARTIFACT_URI if set, else local .barca/artifacts."""
    uri = os.environ.get("BARCA_ARTIFACT_URI")
    if uri:
        return uri
    return str(Path(".barca/artifacts").resolve())


def _emit(msg_type: str, **fields: Any) -> None:
    """Report a step's result to the coordinator over the worker socket (`msg_type` is
    `result`; errors go through `_runtime.emit_step_error` directly)."""
    if msg_type != "result":
        raise ValueError(f"unknown worker message {msg_type!r}")
    _runtime.emit_step_completed(fields["node_id"], fields["artifact"])


# Where a step failed (the `phase` of a `step_error`). A closed set: the coordinator reads any
# other value as unknown. Only `user_code` is the step's own code; the others are barca's.
USER_CODE = "user_code"  # importing the module, calling the function, checking what it returned
LOAD_INPUTS = "load_inputs"  # reading the step's input artifacts (collect() fan-in included)
SPLIT_OUTPUT = "split_output"  # dividing a batched call's result into its keys
WRITE_OUTPUT = "write_output"  # writing the result to its artifact
SINK = "sink"  # writing the step's @sink files
PHASES = (USER_CODE, LOAD_INPUTS, SPLIT_OUTPUT, WRITE_OUTPUT, SINK)


def _traceback_for(exc: BaseException, phase: str) -> str:
    """The traceback a failure in `phase` reports: the user's frames for a failure in user
    code, every frame for a failure in barca's own code, which has nowhere else to show."""
    if phase == USER_CODE:
        return _user_traceback(exc)
    import traceback

    return "".join(traceback.format_list(traceback.extract_tb(exc.__traceback__))).rstrip("\n")


def _output_phase(exc: BaseException) -> str:
    """The phase of a failure while a result was written: its sinks, or its artifact."""
    return SINK if isinstance(exc, _sinks.SinkError) else WRITE_OUTPUT


def _user_traceback(exc: BaseException) -> str:
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


def _failure_message(exc: BaseException, bound_views: Sequence[str] = ()) -> str:
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


def _run_with_timeout(
    fn: Callable[..., Any],
    kwargs: dict[str, Any],
    timeout_seconds: float,
    *,
    label: str | None = None,
) -> Any:
    """Run a function with a timeout. Raises TimeoutError if exceeded."""
    import threading
    from contextvars import copy_context

    result = None
    exception: BaseException | None = None

    def target() -> None:
        nonlocal result, exception
        try:
            result = fn(**kwargs) if kwargs else fn()
        except BaseException as e:
            # BaseException, not Exception: a SystemExit (sys.exit()) or KeyboardInterrupt
            # raised by the step must fail it. Caught as Exception, they ended the thread
            # silently and the step "succeeded" with a None result (issue #149).
            exception = e

    # Timeout execution uses a new thread; carry the active job trace into it.
    thread = threading.Thread(target=copy_context().run, args=(target,))
    thread.daemon = True
    thread.start()
    thread.join(timeout=timeout_seconds)

    if thread.is_alive():
        error = TimeoutError(
            f"Step '{label or getattr(fn, '__name__', 'step')}' exceeded timeout of "
            f"{timeout_seconds}s"
        )
        # The step is still running in its thread: this interpreter is not free for more work.
        error._barca_still_running = True  # ty: ignore[unresolved-attribute]
        raise error
    if exception is not None:
        raise exception
    return result


# ─── Inputs ───────────────────────────────────────────────────────────────────


def _load_artifact(
    path: str, lru: _ArtifactLRU, fmt: str | None = None, *, frame_type: str | None = None
) -> Any:
    """Resolve one artifact path to its deserialized value via the tier-1 LRU
    cache, falling through to the artifact store on miss."""
    cacheable = is_cacheable_frame_type(frame_type)
    hot = lru.get(path, frame_type) if cacheable else None
    if hot is not None:
        return hot
    if not _storage.exists(path):
        raise FileNotFoundError(f"Input artifact not found: {path}")
    if fmt is None:
        fmt = FORMAT_OF_EXTENSION.get(_storage.suffix(path), "json")
    value = deserialize(path, fmt, frame_type=frame_type)
    if cacheable:
        lru.admit(path, value, frame_type)
    return value


# Fan-in (collect()) reads are I/O-bound (local disk or a remote fsspec
# fetch-to-temp-file), so worker threads spend most of their time blocked
# with the GIL released — a thread pool collapses wall time toward the
# slowest single artifact instead of their sum. Capped rather than one
# thread per artifact so a 10k-partition collect() doesn't open 10k files
# at once.
_COLLECT_IO_MAX_WORKERS = 8


def _load_collected_artifacts(
    artifacts: list[dict[str, Any]],
    lru: _ArtifactLRU | None = None,
    *,
    param: str | None = None,
    frame_type: str | None = None,
) -> list[Any]:
    """Load every artifact of a collect() fan-in param, in order.

    Tier-1 LRU lookups happen up front on the calling thread (cheap,
    in-memory — `_ArtifactLRU` isn't safe for concurrent mutation from
    worker threads). Only genuine cache misses — the actual blocking I/O —
    are dispatched to the thread pool; results are written back to the LRU
    on the calling thread as they arrive.

    `param` (the destination parameter name, when known) is folded into a
    missing-artifact error at the point it's raised, matching
    `_load_artifact`'s message shape — the caller doesn't need to catch and
    rewrap.
    """
    results: list[Any] = [None] * len(artifacts)
    to_fetch: list[tuple[int, dict[str, Any]]] = []
    if not is_cacheable_frame_type(frame_type):
        lru = None
    for i, artifact in enumerate(artifacts):
        hot = lru.get(artifact["path"], frame_type) if lru is not None else None
        if hot is not None:
            results[i] = hot
        else:
            to_fetch.append((i, artifact))

    if not to_fetch:
        return results

    def _fetch(item: tuple[int, dict[str, Any]]) -> tuple[Any, int | None]:
        _, artifact = item
        path = artifact["path"]
        if not _storage.exists(path):
            if param is not None:
                raise FileNotFoundError(f"Input artifact for parameter '{param}' not found: {path}")
            raise FileNotFoundError(f"Input artifact not found: {path}")
        fmt = artifact.get("format") or FORMAT_OF_EXTENSION.get(_storage.suffix(path), "json")
        value = deserialize(path, fmt, frame_type=frame_type)
        # The size lookup is I/O too (a request, for a remote path), so it runs in the pool.
        return value, (artifact_size(path) if lru is not None else None)

    def _store(fetched: Any) -> None:
        for (i, artifact), (value, size) in zip(to_fetch, fetched, strict=False):
            results[i] = value
            if lru is not None and size is not None:
                lru.admit(artifact["path"], value, frame_type, size)

    if frame_type == "duckdb":
        # Relations are made on the worker's one shared duckdb connection, which takes one call
        # at a time: duckdb 2 refuses a concurrent one ("connection is busy with another call",
        # #663). Making one reads only the file's metadata, so they are made here, in order.
        _store(map(_fetch, to_fetch))
        return results

    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=min(len(to_fetch), _COLLECT_IO_MAX_WORKERS)) as ex:
        _store(ex.map(_fetch, to_fetch))

    return results


# A dict `partitions_from` source's artifact, read once per worker however many of its keys run
# here: path -> the dict. Bounded: a run reads one version of each source.
_KEYED_SOURCES: dict[str, dict[str, Any]] = {}
_KEYED_SOURCES_MAX = 4
# Whether a step function takes a parameter: (function, name) -> bool.
_ACCEPTS: dict[tuple[Callable[..., Any], str], bool] = {}


def _accepts(fn: Callable[..., Any], param: str) -> bool:
    """Whether `fn` can be called with `param=` (a parameter of that name, or `**kwargs`)."""
    key = (fn, param)
    if key not in _ACCEPTS:
        import inspect

        try:
            params = inspect.signature(fn).parameters
        except (TypeError, ValueError):
            _ACCEPTS[key] = True
        else:
            p = params.get(param)
            _ACCEPTS[key] = (p is not None and p.kind != p.POSITIONAL_ONLY) or any(
                q.kind == q.VAR_KEYWORD for q in params.values()
            )
    return _ACCEPTS[key]


def _keyed_value(path: str, key: str) -> Any:
    """`key`'s value in the dict a `partitions_from` source wrote to `path`."""
    source = _KEYED_SOURCES.get(path)
    if source is None:
        if len(_KEYED_SOURCES) >= _KEYED_SOURCES_MAX:
            _KEYED_SOURCES.pop(next(iter(_KEYED_SOURCES)))
        if not _storage.exists(path):
            raise FileNotFoundError(f"Input artifact not found: {path}")
        source = deserialize(path, "json")
        _KEYED_SOURCES[path] = source
    if key not in source:
        raise KeyError(f"key {key!r} is not in the partitions_from source written to {path}")
    value = source[key]
    if isinstance(value, dict | list):
        import copy

        # The step may change what it receives; the next key of this worker must not see it.
        value = copy.deepcopy(value)
    return value


@dataclass(frozen=True)
class _Keys:
    """The keys of one batched call (RFC-0013), as `barca/_batch.py` names them."""

    dims: list[str]  # the partition dimensions, sorted
    keys: list[Any]  # what the function sees for each step: a value, or a tuple of values
    texts: list[str]  # `retailer=r7`, for messages

    @classmethod
    def of(cls, steps: list[Step]) -> _Keys:
        from barca import _batch

        dims = _batch.key_dims(steps)
        keys = [_batch.key_of(st, dims) for st in steps]
        return cls(dims, keys, [_batch.key_text(k, dims) for k in keys])


def _resolve_inputs(
    fn: Callable[..., Any], steps: list[Step], lru: _ArtifactLRU, batch: _Keys | None = None
) -> dict[str, Any]:
    """The keyword arguments of the step function, from the steps' `inputs`.

    For one step (`batch` is None) each input is that step's value. For the keys of one
    batched call, each partition dimension is the list of the call's keys, an `aligned_inputs`
    upstream is a dict of key to value (or one lazy table), a keyed source a dict of key to
    value, and any other input the whole value, once (from the first key, as every key shares
    it).

    Inputs come in three shapes: an artifact path; `{"_collected": true, "artifacts": [...]}`,
    a collect() fan-in, loaded concurrently (`_load_collected_artifacts`); and `{"_keyed": true,
    "path", "key"}`, one key's value of a dict `partitions_from` source, passed only when the
    function takes the parameter. An underscore-prefixed parameter is an ordering-only
    dependency and gets None.
    """
    first = steps[0]
    param_types: dict[str, str | None] = first.get("param_types") or {}
    kwargs: dict[str, Any]
    params: list[tuple[str, Any]]
    aligned: set[str] = set()
    if batch is None:
        kwargs = dict(first.get("direct_kwargs") or {})
        params = list((first.get("inputs") or {}).items())
    else:
        kwargs = {d: [st["direct_kwargs"][d] for st in steps] for d in batch.dims}
        names: list[str] = []
        for st in steps:
            for p in st.get("inputs") or {}:
                if p not in names:
                    names.append(p)
        params = [(p, (first.get("inputs") or {}).get(p)) for p in names]
        aligned = set(first.get("aligned_inputs") or [])
    for param, value in params:
        frame_type = param_types.get(param)
        if param.startswith("_"):
            kwargs[param] = None
            continue
        if isinstance(value, dict) and value.get("_collected"):
            kwargs[param] = _load_collected_artifacts(
                value.get("artifacts", []), lru, param=param, frame_type=frame_type
            )
            continue
        if isinstance(value, dict) and value.get("_keyed"):
            if _accepts(fn, param):
                if batch is None:
                    kwargs[param] = _keyed_value(value["path"], value["key"])
                else:
                    kwargs[param] = {
                        k: _keyed_value(st["inputs"][param]["path"], st["inputs"][param]["key"])
                        for k, st in zip(batch.keys, steps, strict=True)
                    }
            continue
        if batch is None or param not in aligned:
            if value:
                try:
                    kwargs[param] = _load_artifact(value, lru, frame_type=frame_type)
                except FileNotFoundError:
                    raise FileNotFoundError(
                        f"Input artifact for parameter '{param}' not found: {value}"
                    ) from None
            continue
        paths: list[str] = []
        for text, st in zip(batch.texts, steps, strict=True):
            path = (st.get("inputs") or {}).get(param)
            if not path:
                raise FileNotFoundError(
                    f"Input artifact for parameter '{param}' of {text} was not produced"
                )
            paths.append(path)
        from barca import _batch

        if frame_type in _batch.LAZY_TYPES:
            kwargs[param] = _batch.lazy_table(
                param, frame_type, batch.dims, batch.keys, paths, batch.texts
            )
        else:
            kwargs[param] = {
                k: _load_artifact(path, lru, frame_type=frame_type)
                for k, path in zip(batch.keys, paths, strict=True)
            }
    return kwargs


# ─── Results ──────────────────────────────────────────────────────────────────

_SOURCE_TYPES = "a list of keys or a dict of key -> value"
_JSON_VALUES = "str, int, float, bool, None, and lists and dicts of them"


def _check_partition_source(value: Any, step: Step) -> list[Any] | dict[str, Any]:
    """What a step that a `partitions_from(...)` reads for keys returned, made JSON-writable.

    A list (or tuple) gives the keys; a dict gives the keys and passes each key's value to the
    steps that name the source. Dict keys must be strings or integers; an integer becomes its
    string, as JSON writes it. Values must be JSON-writable. Anything else raises naming the
    key, or the types a source may return.
    """
    name = step.get("function_name", "source")
    serializer = step.get("serializer")
    if serializer not in (None, "json"):
        raise TypeError(
            f"partitions_from source '{name}' declares serializer={serializer!r}; a source is "
            f"read for keys and must be written as JSON"
        )
    if isinstance(value, list | tuple):
        try:
            json.dumps(value, allow_nan=False)
        except (TypeError, ValueError):
            for i, item in enumerate(value):
                try:
                    json.dumps(item, allow_nan=False)
                except (TypeError, ValueError) as exc:
                    raise TypeError(
                        f"partitions_from source '{name}' returned a list whose item {i} "
                        f"({type(item).__name__}) cannot be written as JSON: {exc}"
                    ) from None
            raise
        return list(value)
    if not isinstance(value, dict):
        raise TypeError(
            f"partitions_from source '{name}' returned {type(value).__name__}; it must return "
            f"{_SOURCE_TYPES}"
        )
    keys: dict[str, Any] = {}
    for k in value:
        if isinstance(k, bool) or not isinstance(k, str | int):
            raise TypeError(
                f"partitions_from source '{name}' returned a dict with the key {k!r} "
                f"({type(k).__name__}); keys must be strings or integers (an integer key is "
                f"passed as its string, as JSON writes it)"
            )
        s = k if isinstance(k, str) else str(k)
        if s in keys:
            raise ValueError(
                f"partitions_from source '{name}' returned the keys {keys[s]!r} and {k!r}, "
                f"which are both the partition key '{s}'"
            )
        keys[s] = k
    out = value if all(type(k) is str for k in value) else {s: value[k] for s, k in keys.items()}
    try:
        json.dumps(out, allow_nan=False)
    except (TypeError, ValueError):
        for s, v in out.items():
            try:
                json.dumps(v, allow_nan=False)
            except (TypeError, ValueError) as exc:
                raise TypeError(
                    f"partitions_from source '{name}': the value for key '{s}' "
                    f"({type(v).__name__}) cannot be written as JSON: {exc}. A dict source's "
                    f"values must be {_JSON_VALUES}"
                ) from None
        raise
    return out


def _sensor_result(result: Any) -> tuple[bool | None, Any]:
    if isinstance(result, bool):
        return result, None
    if isinstance(result, tuple) and len(result) == 2:
        changed, value = result
        if not isinstance(changed, bool):
            raise TypeError("a sensor's change signal must be a bool")
        return changed, value
    return None, result


def _materialize(
    result: Any,
    node_id: str,
    art_dir: str,
    step: Step,
    elapsed: float,
    elapsed_in_artifact: bool = False,
    timing: dict[str, Any] | None = None,
    before_emit: Callable[[], None] | None = None,
    prewritten: str | None = None,
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Serialize a result to its artifact and emit a `result` protocol message.

    `prewritten` is a parquet file already holding the result (one key of a batched call that
    DuckDB split, RFC-0013): it is moved into place instead of `result` being written. `extra`
    is added to the artifact dict (a batched call's `batch` id).

    `timing` (cpu_seconds, max_rss_bytes) rides on the artifact dict — the
    completion message does triple duty: closes the lease, carries the output
    ref, and feeds the coordinator's cost estimator. `elapsed`/`timing` as
    passed in cover only the step function's own execution; this adds the
    serialization time measured here on top, so a step's true cost — what the
    cost model, `barca stats`, and `barca history` all see — isn't
    systematically undercounted for large payloads (serialization can be the
    majority of a step's real wall time and was previously invisible).
    """
    from barca import _rss

    # The step has returned: a result it left unread would block writing a returned relation.
    _duckdb.finish_open_result()
    explicit_fmt = step.get("serializer")
    if prewritten is not None:
        fmt = "parquet"
    else:
        fmt = resolve_format(result, detect_format(result, explicit=explicit_fmt))
    # Run-hash layout when the coordinator supplies a run hash.
    # The step-level hash names this step's own output only, never another node id's.
    run_hash = step.get("run_hash") if node_id == step.get("node_id") else None
    branch_path = step.get("branch_path")
    if branch_path:
        # A parallel() branch: the coordinator names a file that belongs to this run and this
        # group alone, so the caller can only read what its own branches wrote (#332).
        path: Path | str = Path(f"{branch_path}{_FORMAT_EXTENSIONS[fmt]}")
    else:
        path = artifact_path(art_dir, node_id, fmt, run_hash)
    _ser_wall0 = time.perf_counter()
    _ser_cpu0 = time.process_time()
    # Hash producer-owned bytes for every materialization; sensor hashes also
    # participate in downstream run hashes.
    content_hash = None
    upload_snapshot = None
    local = _storage.local_path_of(path)
    if local is not None:
        _storage.make_way(local)
    snapshot_prefix = os.environ.get("BARCA_UPLOAD_SNAPSHOT_PREFIX")
    if prewritten is not None:
        from barca._artifacts import publish_file

        size, content_hash, upload_snapshot = publish_file(
            prewritten, path, want_hash=True, snapshot_prefix=snapshot_prefix
        )
        if step.get("sinks"):
            import duckdb

            result = duckdb.read_parquet(str(path))
    elif snapshot_prefix and not step.get("branch"):
        size, content_hash, upload_snapshot = serialize_for_upload(
            result, path, fmt, snapshot_prefix, want_hash=True
        )
    else:
        size, content_hash = serialize_hashed(result, path, fmt)
    elapsed += time.perf_counter() - _ser_wall0
    if timing and timing.get("cpu_seconds") is not None:
        timing = {
            **timing,
            "cpu_seconds": timing["cpu_seconds"] + (time.process_time() - _ser_cpu0),
        }
    if timing and "rss_window" in timing:
        timing = dict(timing)
        peak = _rss.step_peak_bytes(timing.pop("rss_window"))
        if peak:
            timing["max_rss_bytes"] = peak
    artifact: dict[str, Any] = {"path": str(path), "format": fmt, "size_bytes": size}
    if upload_snapshot is not None:
        artifact["upload_snapshot"] = upload_snapshot
    if content_hash is not None:
        artifact["content_hash"] = content_hash
    if step.get("branch") and fmt in ("parquet", "csv"):
        # The caller of parallel() gets back the frame type the branch returned.
        from barca import _branches

        artifact["frame_type"] = _branches.reader_name(result, fmt)
    if extra:
        artifact.update(extra)
    if elapsed_in_artifact:
        artifact["elapsed_seconds"] = elapsed
    if timing:
        artifact.update(timing)
    # When the step finished and how long it took, for telemetry: a span needs wall-clock times.
    artifact["finished_at"] = time.time()
    artifact["wall_seconds"] = elapsed
    sink_outcomes = _sinks.write_sinks(result, step, node_id, fmt)
    if sink_outcomes:
        artifact["sinks"] = sink_outcomes
    if extra and "batch" in extra:
        artifact["write_seconds"] = time.perf_counter() - _ser_wall0
    if before_emit is not None:
        before_emit()
    _emit("result", node_id=node_id, artifact=artifact)
    return artifact


def _timing(
    cpu: float, rss_window: RssWindow, import_seconds: float | None = None
) -> dict[str, Any]:
    """The `timing` a step's `_materialize` adds to its artifact: CPU seconds, the RSS window
    (read after the result is written: a returned DuckDB relation or lazy frame does its work
    while it is serialized) and, when the module took over a second to import, that too."""
    timing: dict[str, Any] = {"cpu_seconds": cpu, "rss_window": rss_window}
    if import_seconds and import_seconds > 1:
        timing["import_seconds"] = import_seconds
    return timing


def _slow_import(import_seconds: float | None) -> float | None:
    """`import_seconds` as a step error reports it: only when the import took over a second."""
    return import_seconds if import_seconds and import_seconds > 1 else None


def _worker_survives(exc: BaseException, imported: bool) -> bool:
    """Whether this worker may take more work after a step failed with `exc` (#523).

    An exception raised by user code (or while reading its inputs or writing its result)
    unwinds cleanly: the worker reports it and goes on, as it does after a success. It stops,
    and the coordinator starts a fresh worker, when this interpreter may not be sound: the
    module failed to import (a half-run module body), the step called sys.exit() or was
    interrupted, it ran out of memory, it is still running past its timeout, or a parallel()
    exchange with the coordinator was left unfinished.
    """
    return (
        imported
        and isinstance(exc, Exception)
        and not isinstance(exc, MemoryError)
        and not getattr(exc, "_barca_still_running", False)
        and not _runtime.exchange_broken
    )


# ─── Running a step ───────────────────────────────────────────────────────────


@dataclass
class _Begun:
    """A step's function, found and ready to run: what `_begin` hands the runners."""

    source: str
    fn: Callable[..., Any]
    out: _LineEmitter
    err: _StderrTee
    import_seconds: float | None


def _tracing(step: Step, call_id: str | None = None, keys: int = 0) -> _telemetry.Execution:
    """The Datadog span of a step or a batched call, when the coordinator sent a trace context."""
    tracing = _telemetry.Execution(step)
    if call_id is not None and tracing.span is not None:
        try:
            tracing.span.set_metric("barca.batch.keys", keys)
            tracing.span.set_tag("barca.batch", call_id)
        except Exception:
            pass
    return tracing


def _begin(step: Step, modules: Modules, log_id: str) -> _Begun:
    """Import the step's module (once per worker) and find its function.

    The module's own output is recorded under `log_id`. Raises whatever the import or the
    lookup raised; a failure here leaves the interpreter unsound (`_worker_survives`).
    """
    source = str(Path(step["source_file"]).resolve())
    activate_source_path(source)
    out = stdout_for_step(log_id)
    err = stderr_for_step(log_id)
    import_seconds = None
    if source not in modules:
        # Only under `--agent` (and in `barca serve`): a person's terminal shows the progress
        # bar, and the import time is in the run JSON as `import_seconds` (#496).
        if os.environ.get("BARCA_WORKER_AGENT") == "1":
            print(f"[barca] importing {source}", file=sys.stderr, flush=True)
        import_start = time.perf_counter()
        try:
            with captured(out, err):
                modules[source] = load_pipeline_module(source)
        finally:
            import_seconds = time.perf_counter() - import_start
    fn = getattr(modules[source], step["function_name"])
    return _Begun(source, fn, out, err, import_seconds)


def _call(
    begun: _Begun, step: Step, kwargs: dict[str, Any], label: str, args: Sequence[Any] = ()
) -> Any:
    """Run the step's function with its stdout and stderr captured, within its timeout."""
    # Legacy pickle recovery may have activated a different producer directory.
    activate_source_path(begun.source)
    timeout = step.get("timeout_seconds", 0)
    fn = begun.fn
    with captured(begun.out, begun.err):
        if timeout and timeout > 0:
            return _run_with_timeout(lambda: fn(*args, **kwargs), {}, timeout, label=label)
        return fn(*args, **kwargs)


def _run_daemon_step(step: Step, modules: Modules, art_dir: str, lru: _ArtifactLRU) -> bool:
    """Execute one step in daemon mode and emit its result or error.

    Returns True when this worker may go on to its next step (the step succeeded, or failed
    with an exception that left the interpreter sound), False when it must stop. Socket
    errors raised while emitting propagate to the caller (the connection is gone — exit the
    loop).
    Per-task self-timing: CPU time (`process_time`, the truest measure of
    work), wall time, and peak RSS ride back on the completion message.
    """
    from barca import _rss

    tracing = _tracing(step)
    node_id = step.get("node_id", "unknown")
    t0 = time.perf_counter()
    c0 = time.process_time()
    rss_window = _rss.start_window()

    # Views bound for duckdb-typed inputs. They must outlive materialization: a returned
    # relation is lazy and may still reference them when it is written to parquet.
    bound_views: list[str] = []
    # Set when this step is a parallel() branch whose return value could not be written.
    unwritable = False
    import_seconds = None
    # Whether the step's module is imported and its function found: a failure before that
    # stops the worker (`_worker_survives`).
    imported = False
    # Where the step is: what a failure is reported as (`PHASES`).
    phase = USER_CODE

    try:
        begun = _begin(step, modules, node_id)
        import_seconds = begun.import_seconds
        imported = True

        # From here until the result is written, Ctrl-C interrupts the step.
        _ctrl_c_interrupts_the_step()

        # `barca.on_step_start` callbacks size the worker for this step before its inputs load.
        with captured(begun.out, begun.err):
            _hooks.run_step_start(step)

        phase = LOAD_INPUTS
        kwargs = _resolve_inputs(begun.fn, [step], lru)
        bound_views = _duckdb.bind_inputs(kwargs, step.get("param_types") or {})

        phase = USER_CODE
        # Direct args from parallel() dispatch; kwargs carry its direct kwargs already.
        result = _call(begun, step, kwargs, node_id, args=step.get("direct_args", []))

        wall = time.perf_counter() - t0
        cpu = time.process_time() - c0

        # Freshness is independent of the optional payload.
        sensor_changed = None
        if step.get("kind") == "sensor":
            sensor_changed, result = _sensor_result(result)

        if sensor_changed is False and step.get("previous_sensor"):
            artifact = {
                **step["previous_sensor"],
                "sensor_changed": False,
                "elapsed_seconds": wall,
                "wall_seconds": wall,
                "finished_at": time.time(),
                "cpu_seconds": cpu,
            }
            peak = _rss.step_peak_bytes(rss_window)
            if peak is not None:
                artifact["max_rss_bytes"] = peak
            tracing.finish()
            _emit("result", node_id=node_id, artifact=artifact)
            return True

        # Convert ParallelError instances so results are JSON-serializable.
        from barca import _branches

        result = _branches.serializable(result)

        # Read by a `partitions_from(...)` for keys: a list, or a dict with JSON values.
        materialize_step = step
        source_observation = None
        if step.get("partition_source"):
            result = _check_partition_source(result, step)
            materialize_step = {**step, "serializer": "json"}
            from barca._source_observation import observe

            source_observation = observe(result, step.get("previous_sensor"))

        # Source validation and comparison are producer work too, not coordinator overhead.
        wall = time.perf_counter() - t0
        cpu = time.process_time() - c0
        # Serialize result to artifact (and write any declared sinks).
        phase = WRITE_OUTPUT
        extra: dict[str, Any] = {}
        if sensor_changed is not None:
            extra["sensor_changed"] = sensor_changed
        if source_observation is not None:
            extra["source_observation"] = source_observation
        try:
            artifact = _materialize(
                result,
                node_id,
                art_dir,
                materialize_step,
                wall,
                elapsed_in_artifact=True,
                timing=_timing(cpu, rss_window, import_seconds),
                extra=extra,
                before_emit=tracing.finish,
            )
        except Exception as exc:
            phase = _output_phase(exc)
            if not step.get("branch"):
                raise
            # A parallel() branch whose return value cannot be written: say so in terms of
            # the branch, for the step that called it (barca/_branches.py).
            unwritable = True
            raise _branches.unwritable(step, result, exc) from exc
        # A downstream step in this worker may consume what we just produced, keyed by the
        # reader it is equivalent to so a consumer never gets a different frame type.
        # (Not a branch's result: no step of this worker reads it, and its file is removed
        # when the calling step ends.)
        result_type = cacheable_frame_type(result)
        # Keyed sources have a separate snapshot cache; this copy is never used by it.
        if result_type is not False and not step.get("branch") and not step.get("partition_source"):
            lru.admit(artifact["path"], result, result_type, artifact.get("size_bytes"))
        return True

    except BaseException as exc:
        # Any failure inside the step — including TimeoutError and OSError
        # from user code — is a step error. (TimeoutError and the socket
        # errors are OSError subclasses, so a socket-error catch here would
        # swallow them; genuine socket death surfaces when the emit below
        # fails, and that propagates to the caller.)
        # The step is over. A Ctrl-C from here on (people press it twice) must not interrupt
        # the report of how it ended: it would leave this function as an uncaught
        # KeyboardInterrupt and print a traceback. The coordinator has the same signal and
        # stops this worker.
        _ctrl_c_does_nothing()
        wall = time.perf_counter() - t0
        tracing.finish(exc)
        from barca import _branches

        survives = _worker_survives(exc, imported)
        _runtime.emit_step_error(
            node_id=node_id,
            # Only this branch's own return value that could not be written is reported as
            # such. A branch that raises, a BranchResultError from a parallel() of its own
            # included, is a branch that raised.
            error_type=_branches.UNRETURNABLE if unwritable else type(exc).__name__,
            message=_failure_message(exc, bound_views),
            traceback=_traceback_for(exc, phase),
            elapsed=wall,
            import_seconds=_slow_import(import_seconds),
            worker_continues=survives,
            phase=phase,
        )
        return survives

    finally:
        _ctrl_c_does_nothing()
        tracing.finish()
        _duckdb.unbind_inputs(bound_views)


def _key_result(value: Any, explicit: str | None) -> tuple[Any, str | None]:
    """One key's value as `_batch.split` left it: the value itself, or, for a key DuckDB wrote
    to parquet, `(None, file)` when that file can be installed as the artifact as it is, else a
    relation over the files."""
    if not (isinstance(value, tuple) and len(value) == 2 and value[0] == "parquet"):
        return value, None
    files = value[1]
    if (
        len(files) == 1
        and Path(files[0]).name != "__empty.parquet"
        and explicit in (None, "parquet")
    ):
        return None, files[0]
    import duckdb

    return duckdb.read_parquet(files), None


def _run_daemon_call(
    steps: list[Step], call: dict[str, Any], modules: Modules, art_dir: str, lru: _ArtifactLRU
) -> bool:
    """Run the keys of one batched call (RFC-0013): one call of the function for all of them,
    then each key written and reported on its own (`barca/_batch.py`).

    Returns True when this worker may go on, as `_run_daemon_step` does.
    """
    from barca import _batch, _rss

    first = steps[0]
    batch = _Keys.of(steps)
    base = first.get("node_id", "unknown").split("[", 1)[0]
    call_id = call.get("id", "")
    # The call's own output goes to the log under `<node>[batch=<id>]`: recorded once, and
    # found from each of its keys (`barca logs --step 'node[key]'`).
    log_id = f"{base}[batch={call_id}]"
    tracing = _tracing(first, call_id, len(steps))
    t0 = time.perf_counter()
    c0 = time.process_time()
    rss_window = _rss.start_window()
    import_seconds = None
    imported = False
    reported: set[str] = set()
    n = len(steps)
    bound_views: list[str] = []
    phase = USER_CODE
    staging = batch_staging(art_dir, call_id)

    def fail_all(exc: BaseException, survives: bool, wall: float) -> None:
        """Report every key of the call not yet reported as failed with `exc`."""
        message = _failure_message(exc, bound_views)
        traceback = _traceback_for(exc, phase)
        left = [st for st in steps if st["node_id"] not in reported]
        for i, st in enumerate(left):
            reported.add(st["node_id"])
            _runtime.emit_step_error(
                node_id=st["node_id"],
                error_type=type(exc).__name__,
                message=message,
                traceback=traceback,
                elapsed=wall / max(n, 1),
                import_seconds=_slow_import(import_seconds),
                # The coordinator stops an unsound worker at the first report that says so:
                # every key is reported before that.
                worker_continues=survives or i < len(left) - 1,
                phase=phase,
            )

    try:
        begun = _begin(first, modules, log_id)
        import_seconds = begun.import_seconds
        imported = True
        _ctrl_c_interrupts_the_step()

        # One call, one start: the keys of a batched call share their node's weight.
        with captured(begun.out, begun.err):
            _hooks.run_step_start(first)

        phase = LOAD_INPUTS
        load_start = time.perf_counter()
        kwargs = _resolve_inputs(begun.fn, steps, lru, batch)
        bound_views.extend(_duckdb.bind_inputs(kwargs, first.get("param_types") or {}))

        phase = USER_CODE
        compute_start = time.perf_counter()
        result = _call(begun, first, kwargs, log_id)
        # Splitting the result runs DuckDB on the shared connection: see finish_open_result.
        _duckdb.finish_open_result()

        explicit = first.get("serializer")
        phase = SPLIT_OUTPUT
        split_start = time.perf_counter()
        values = _batch.split(result, batch.keys, batch.dims, staging)
        split_end = time.perf_counter()
    except BaseException as exc:
        _ctrl_c_does_nothing()
        survives = _worker_survives(exc, imported)
        tracing.finish(exc)
        _duckdb.unbind_inputs(bound_views)
        _batch.unbind()
        fail_all(exc, survives, time.perf_counter() - t0)
        return survives

    # The call ran: write and report each key. Its time and memory are the call's, shared.
    wall = time.perf_counter() - t0
    cpu = time.process_time() - c0
    share = wall / max(n, 1)
    # Once per call, on the first key written: the coordinator adds the calls of a node up.
    phases: dict[str, float] | None = {
        "import_seconds": import_seconds or 0.0,
        "load_seconds": compute_start - load_start,
        "compute_seconds": split_start - compute_start,
        "split_seconds": split_end - split_start,
    }
    ok = True
    key_phase = SPLIT_OUTPUT
    try:
        for k, text, st in zip(batch.keys, batch.texts, steps, strict=True):
            node_id = st["node_id"]
            n_key = _batch._norm(k, batch.dims)
            # The call's own failure of this key (`barca.Failed`) is the step's.
            key_phase = SPLIT_OUTPUT
            try:
                if n_key not in values:
                    raise _batch.BatchFailed(f"batch returned no result for {text}")
                value = values[n_key]
                if isinstance(value, _batch.Failed):
                    key_phase = USER_CODE
                    raise _batch.BatchFailed(value.reason or f"{text} failed")
                value, prewritten = _key_result(value, explicit)
                key_phase = WRITE_OUTPUT
                _materialize(
                    value,
                    node_id,
                    art_dir,
                    st,
                    share,
                    elapsed_in_artifact=True,
                    timing=_timing(cpu / max(n, 1), rss_window),
                    prewritten=prewritten,
                    extra={
                        "batch": call_id,
                        "batch_keys": n,
                        **({"batch_phases": phases} if phases else {}),
                    },
                )
                phases = None
                reported.add(node_id)
            except (BrokenPipeError, ConnectionResetError):
                raise
            except Exception as exc:
                reported.add(node_id)
                if key_phase == WRITE_OUTPUT:
                    key_phase = _output_phase(exc)
                if isinstance(exc, _batch.BatchFailed):
                    error_type, traceback = "BatchKeyFailed", ""
                else:
                    error_type, traceback = type(exc).__name__, _traceback_for(exc, key_phase)
                _runtime.emit_step_error(
                    node_id=node_id,
                    error_type=error_type,
                    message=str(exc),
                    traceback=traceback,
                    elapsed=share,
                    worker_continues=True,
                    phase=key_phase,
                )
    except BaseException as exc:
        _ctrl_c_does_nothing()
        if isinstance(exc, BrokenPipeError | ConnectionResetError):
            raise
        ok = _worker_survives(exc, True)
        phase = key_phase
        fail_all(exc, ok, time.perf_counter() - t0)
    finally:
        _ctrl_c_does_nothing()
        tracing.finish()
        _duckdb.unbind_inputs(bound_views)
        _batch.unbind()
        import shutil

        shutil.rmtree(staging, ignore_errors=True)
    return ok


# ─── The daemon loop ──────────────────────────────────────────────────────────


def run_daemon() -> None:
    """Daemon mode: read execute commands from socket, run each step, send results."""

    _ctrl_c_does_nothing()
    join_the_job()

    if _runtime.connect() is None:
        print("BARCA_SOCKET not set", file=sys.stderr)
        sys.exit(1)

    # Collapse repeated library warnings (barca docs agents, "Repeated warnings").
    from barca import _dedupe

    _dedupe.install(_runtime.socket_path())

    # Install SIGTERM handler so graceful_kill flushes buffered progress output
    # before the process goes away.
    exit_on_sigterm()

    modules: Modules = {}
    lru = _ArtifactLRU()
    art_dir = _default_artifact_dir()
    if not _storage.is_remote(art_dir):
        Path(art_dir).mkdir(parents=True, exist_ok=True)
    clean_staging()

    while True:
        try:
            msg = _runtime.recv_message()
        except (BrokenPipeError, ConnectionResetError, OSError):
            break
        except Exception as exc:
            # A frame that is not a message (`RuntimeError` on a half frame, `ValueError` on an
            # oversized or malformed one): the connection cannot be trusted any further. Say
            # why this worker stops; the coordinator sees only that it did.
            print(
                f"[barca] worker: cannot read the coordinator's message, stopping: "
                f"{type(exc).__name__}: {exc}",
                file=sys.stderr,
                flush=True,
            )
            break

        if msg.get("type") == "done":
            break

        # Batch pull: K steps per round-trip. The lease closes per-step as
        # each result message goes back. A step that raised does not stop the
        # batch; one that left this interpreter unsound does (the coordinator
        # stops this worker, starts a fresh one and re-queues the unstarted
        # remainder; see `_worker_survives`).
        if msg.get("type") == "execute_batch":
            from barca import _wire

            steps = _wire.decode_steps(msg)
        elif msg.get("type") == "execute":
            steps = [msg.get("step", {})]
        else:
            continue

        try:
            with _runtime.result_batch(msg.get("result_journal")):
                if msg.get("call"):
                    _run_daemon_call(steps, msg["call"], modules, art_dir, lru)
                else:
                    for step in steps:
                        if not _run_daemon_step(step, modules, art_dir, lru):
                            break
        except (BrokenPipeError, ConnectionResetError, OSError):
            # Socket was closed (e.g. replacement worker killed) — exit cleanly.
            break
        except KeyboardInterrupt:
            # A Ctrl-C that landed in the instant between a step's last statement and
            # `_ctrl_c_does_nothing`. Leave without a traceback; the coordinator, which has
            # the same signal, is stopping the run.
            break

    _runtime.disconnect()


def main() -> None:
    if sys.argv[1:2] != ["--daemon"]:
        print("Usage: python -m barca._worker --daemon", file=sys.stderr)
        sys.exit(2)
    run_daemon()


if __name__ == "__main__":
    main()
