"""Barca worker — executes a batch of steps sequentially.

Invoked by Rust: python -m barca._worker <batch.json>

Protocol:
  - Input: batch JSON file with steps, provided_inputs, and artifact_dir
  - Protocol output: prefixed JSON lines on STDERR: BARCA:2:{...}
  - User output: stdout passes through to terminal (print() works normally)
  - Non-prefixed stderr lines are treated as errors/tracebacks
  - No DB access — Rust owns all persistence
"""

import contextlib
import io
import json
import os
import sys
import time
from pathlib import Path

from barca import _duckdb, _sinks, _storage
from barca._artifacts import (
    _FORMAT_EXTENSIONS,
    LAZY_FRAME_TYPES,
    _frame_kind,
    artifact_path,
    clean_staging,
    deserialize,
    detect_format,
    resolve_format,
    safe_node_id,
    serialize,
    serialize_for_upload,
    serialize_hashed,
)
from barca._source_import import activate_source_path, load_pipeline_module

_EXT_FORMATS = {
    ".json": "json",
    ".pkl": "pickle",
    ".pickle": "pickle",
    ".parquet": "parquet",
    ".csv": "csv",
}


class _LineEmitter(io.TextIOBase):
    """Capture Python stdout as complete node-attributed metadata lines.

    The coordinator owns display policy; this stream never writes user text directly
    to the worker's inherited descriptors. Partial lines flush on return or failure.
    """

    stream = "stdout"

    def __init__(self, node_id):
        self.node_id = node_id
        self._buf = ""
        # The coordinator renders this metadata event; user stdout never bypasses it.

    def write(self, s):
        self._buf += s
        while "\n" in self._buf:
            line, self._buf = self._buf.split("\n", 1)
            self._record(line)
        return len(s)

    def flush(self):
        if self._buf and not sys.is_finalizing():
            line, self._buf = self._buf, ""
            self._record(line)

    def _record(self, line):
        from barca import _runtime

        _runtime.emit_log(self.node_id, line, self.stream)


class _StderrTee(_LineEmitter):
    """Python-level stderr of a step: written through to the worker's own stderr as it comes
    (what the terminal, or `barca serve`'s stderr, showed before), and recorded line by line
    as the step's `stderr` stream. That covers `logging`'s default handler, `warnings` and
    anything printed to `sys.stderr`; writes to file descriptor 2 (native code, subprocesses)
    do not pass through here. barca's own `[barca]` lines are written but not recorded."""

    stream = "stderr"

    def __init__(self, node_id, out):
        super().__init__(node_id)
        self._out = out

    @property
    def encoding(self):  # ty: ignore[invalid-property-type-override]
        return getattr(self._out, "encoding", "utf-8")

    @property
    def errors(self):  # ty: ignore[invalid-property-type-override]
        return getattr(self._out, "errors", "strict")

    def fileno(self):
        # Code that needs a descriptor (faulthandler, subprocess) gets the real one, and its
        # writes go there uncaptured.
        return self._out.fileno()

    def isatty(self):
        try:
            return self._out.isatty()
        except (AttributeError, ValueError, OSError):
            return False

    def write(self, s):
        try:
            self._out.write(s)
        except (AttributeError, ValueError, OSError):
            pass
        return super().write(s)

    def flush(self):
        super().flush()
        try:
            self._out.flush()
        except (AttributeError, ValueError, OSError):
            pass

    def _record(self, line):
        # A progress bar redraws its line with \r: record what the terminal ended up showing.
        if "\r" in line:
            line = line.rstrip("\r").rsplit("\r", 1)[-1]
        if line.startswith(("[barca]", "BARCA:")):
            return
        super()._record(line)


_step_stdout = None
_step_stderr = None


def _stdout_for_step(node_id):
    # Import-time stdout handlers may retain this stream. Reuse it so they follow
    # the current step rather than retaining whichever step first imported a module.
    global _step_stdout
    if _step_stdout is None:
        _step_stdout = _LineEmitter(node_id)
    else:
        _step_stdout.flush()
        _step_stdout.node_id = node_id
    return _step_stdout


def _stderr_for_step(node_id):
    # As _stdout_for_step: a logging handler made at import time (logging.basicConfig())
    # keeps this stream, so it is one stream that follows the current step.
    global _step_stderr
    if _step_stderr is None:
        _step_stderr = _StderrTee(node_id, sys.stderr)
    else:
        _step_stderr.flush()
        _step_stderr.node_id = node_id
    return _step_stderr


@contextlib.contextmanager
def _captured(out, err):
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            yield
        finally:
            # Emit any trailing partial line, even if the step raised.
            out.flush()
            err.flush()


def _peak_rss_bytes() -> int:
    """Peak RSS of this process over its whole life, in bytes (0 if unavailable).

    `ru_maxrss` is kilobytes on Linux and bytes on macOS.
    """
    try:
        import resource

        peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        if sys.platform == "darwin":
            return int(peak)
        return int(peak) * 1024
    except Exception:
        return 0


def _vm_hwm_bytes() -> int | None:
    """`VmHWM` from /proc/self/status (Linux): the peak RSS since it was last reset."""
    try:
        with open("/proc/self/status", "rb") as status:
            for line in status:
                if line.startswith(b"VmHWM:"):
                    return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        return None
    return None


def _start_rss_window():
    """Start measuring one step's peak RSS (#627).

    A worker runs many steps, so the process's lifetime peak (`ru_maxrss`) is the largest step
    it has run so far, not this one. On Linux, writing `5` to /proc/self/clear_refs resets the
    peak (`VmHWM`) to the current RSS, so the peak read when the step ends is this step's own:
    the most memory the worker held while it ran, the worker's baseline (Python, imports, a
    DuckDB connection) included. Elsewhere, the lifetime peak before the step is kept, and the
    step's peak is known only when it raised it. Returns what `_step_peak_rss_bytes` needs.
    """
    try:
        with open("/proc/self/clear_refs", "w") as refs:
            refs.write("5")
        if _vm_hwm_bytes() is not None:
            return ("hwm", 0)
    except OSError:
        pass
    return ("lifetime", _peak_rss_bytes())


def _step_peak_rss_bytes(window) -> int | None:
    """The step's peak RSS in bytes, from `_start_rss_window`'s window; None when unknown."""
    kind, before = window
    if kind == "hwm":
        return _vm_hwm_bytes()
    after = _peak_rss_bytes()
    # The lifetime peak grew during the step: that is the step's peak. Otherwise the step
    # stayed under an earlier step's peak, and how far under is not known.
    return after if after > before else None


# Tier-1 cache limits. Sizes are bytes of the serialized artifact: known without touching the
# value, and the same unit for every type and for local and remote artifacts.
#
# One artifact: above this the copy that isolates a cached value costs more than the read it
# saves, and the cached value is a second copy of a large frame in memory.
_LRU_MAX_ARTIFACT_BYTES = 8 * 1024 * 1024
# All artifacts cached by one worker: room for eight of the largest, so what a worker holds
# does not grow with the number of steps it runs. (Serialized bytes: a compressed parquet
# file is larger in memory.)
_LRU_MAX_TOTAL_BYTES = 8 * _LRU_MAX_ARTIFACT_BYTES
# Entries: bounds the per-object overhead of many tiny artifacts, which the byte limits miss.
_LRU_MAX_ENTRIES = 16


def _lru_frame_type(frame_type: str | None) -> bool:
    """Whether values read with this frame type may be cached.

    Lazy values (duckdb relations, polars LazyFrames) are cheap to recreate and read their file
    when queried; a remote input's file is deleted when the step ends, so a cached one would
    break the next step that used it.
    """
    return frame_type not in LAZY_FRAME_TYPES


def _result_frame_type(value) -> "str | None | bool":
    """The reader frame type a step's result is equivalent to, for caching it.

    Returns the frame type (None for non-frame values, which every reader ignores), or False
    when the result must not be cached: a lazy value, or a frame of a type no reader returns.
    """
    kind = _frame_kind(value)
    if kind is None:
        return None
    if kind == "duckdb" or type(value).__name__ == "LazyFrame":
        return False
    return kind


def _artifact_size(path: str) -> "int | None":
    """Serialized size of a local or remote artifact in bytes, or None when it can't be read."""
    try:
        return _storage.size(path)
    except Exception:
        return None


# What `pandas.api.types.infer_dtype` calls an object column whose cells are all immutable
# values. A column of any other kind (lists, dicts, arrays, arbitrary objects) holds cells a
# step can edit in place.
_PANDAS_IMMUTABLE_CELL_KINDS = frozenset(
    {
        "string", "bytes", "floating", "integer", "mixed-integer-float", "decimal", "complex",
        "boolean", "datetime64", "datetime", "date", "timedelta64", "timedelta", "time",
        "period", "interval", "categorical", "empty",
    }
)  # fmt: skip


def _cacheable(value) -> bool:
    """Whether the tier-1 cache can hand out isolated copies of `value` for less than a read.

    False for a pandas DataFrame with an object-dtype column of mutable cells (lists, dicts,
    arrays: what parquet list and struct columns become). `DataFrame.copy(deep=True)` copies
    the arrays but not the objects such a column points to, and copying them cell by cell
    costs more than reading the file again, so the frame is left out and every consumer reads
    its own. The check is one `infer_dtype` scan per object column, made once when a value is
    offered to the cache; frames without object columns are not scanned at all.
    """
    if _frame_kind(value) != "pandas":
        return True
    from pandas.api.types import infer_dtype, is_object_dtype

    for position, dtype in enumerate(value.dtypes):
        if not is_object_dtype(dtype):
            continue
        cells = value.iloc[:, position].to_numpy()
        if infer_dtype(cells, skipna=True) not in _PANDAS_IMMUTABLE_CELL_KINDS:
            return False
    return True


def _isolated_copy(value):
    """A copy of a cached value: an in-place edit of one does not reach the other.

    The one place that decides how each type goes into and comes out of the tier-1 cache
    (`_cacheable` decides which values go in at all):

    - polars DataFrame: `clone()`. Constant time and memory: columns are reference-counted and
      copied on write, so an in-place edit of either frame (`df[0, "a"] = x`, `insert_column`,
      `extend`, `drop_in_place`, ...) never reaches the other.
    - everything else: `copy.deepcopy`, a real copy. That includes pandas frames (a shallow
      copy shares its arrays), containers, and pyarrow Tables: a Table has no mutating
      methods, but its buffers are writable through the buffer protocol
      (`np.frombuffer(table.column(0).chunk(0).buffers()[1])`), so a shared one is not isolated.

    Raises whatever `copy.deepcopy` raises for a value that can't be copied.

    Not covered (each has an expected-failure test in test_artifact_lru.py):

    - A polars frame built over a numpy array (`pl.DataFrame({"a": arr})` does not copy
      `arr`) still changes when the step that returned it writes to that array afterwards.
    - A pandas object that is not itself the value: a DataFrame inside a dict or list, or a
      Series, is cached, and `copy.deepcopy` shares list and dict cells of object-dtype
      columns between the copies.
    - Arrow-backed pandas columns (`ArrowDtype`, and the default `str` dtype of pandas 3):
      the copies share Arrow buffers. Edits made through pandas never write to them, but
      converting to pyarrow (`pa.Table.from_pandas(df)`) and writing through
      `np.frombuffer` on a column's buffer does.
    """
    if _frame_kind(value) == "polars":
        return value.clone()
    import copy

    return copy.deepcopy(value)


class _ArtifactLRU:
    """Tier-1 read-through cache: deserialized artifacts hot in this process.

    Keyed by (path, frame_type) — paths are named by run hash
    ({node}/{run_hash}{ext}), so a path uniquely identifies content and
    invalidation is automatic (changed input → changed hash → new path → miss).
    Frame type is part of the key so a polars consumer never hits a cached pandas
    materialization of the same path.

    Isolation: the cache keeps its own copy of every value and hands out a fresh
    copy on every hit (`_isolated_copy`), so a task mutating its input can never
    poison a later task's view; a value that can't be copied, or whose copy would
    cost more than reading its file (`_cacheable`), is not held and the caller
    falls through to the store (tier 2).

    Bounds: `admit` takes an artifact only when its serialized size is known and at
    most `_LRU_MAX_ARTIFACT_BYTES`, wherever it is stored; the least recently used
    entries are evicted to stay within `max_total_bytes` and `max_entries`.

    Pure luck-optimization: always safe to miss, never persisted, never gates
    correctness.
    """

    def __init__(
        self,
        max_entries: int = _LRU_MAX_ENTRIES,
        max_total_bytes: int = _LRU_MAX_TOTAL_BYTES,
    ):
        from collections import OrderedDict

        # key -> (value, serialized size in bytes)
        self._entries: "OrderedDict[tuple[str, str], tuple[object, int]]" = OrderedDict()
        self._max = max_entries
        self._max_total_bytes = max_total_bytes
        self._total_bytes = 0

    @staticmethod
    def _key(path: str, frame_type: str | None) -> tuple[str, str]:
        return (path, frame_type or "pandas")

    def _drop(self, key) -> None:
        """Remove the entry for `key`, if there is one, and release its bytes."""
        entry = self._entries.pop(key, None)
        if entry is not None:
            self._total_bytes -= entry[1]

    def get(self, path: str, frame_type: str | None = None):
        """Return a safe copy of the cached value, or None on miss."""
        key = self._key(path, frame_type)
        if key not in self._entries:
            return None
        self._entries.move_to_end(key)
        try:
            return _isolated_copy(self._entries[key][0])
        except Exception:
            self._drop(key)
            return None

    def put(self, path: str, value, frame_type: str | None = None, *, size_bytes: int) -> bool:
        """Cache a copy of `value`, counting `size_bytes` (its serialized size) against the
        byte limit; return whether the cache now holds it.

        Applies only the cache-wide limits; `admit` is the entry point that also applies the
        per-artifact one. A value that can't be isolated cheaply (`_cacheable`) or can't be
        copied is declined. Whatever the outcome, an older entry for the same key is gone:
        the cache never answers for a key with a value it was not just given.
        """
        if size_bytes < 0:
            raise ValueError(f"size_bytes must not be negative, got {size_bytes}")
        key = self._key(path, frame_type)
        self._drop(key)
        if size_bytes > self._max_total_bytes:
            return False  # can never fit: don't evict the others to find that out
        try:
            if not _cacheable(value):
                return False
            cached = _isolated_copy(value)
        except Exception:
            return False
        self._entries[key] = (cached, size_bytes)
        self._total_bytes += size_bytes
        while self._entries and (
            len(self._entries) > self._max or self._total_bytes > self._max_total_bytes
        ):
            self._drop(next(iter(self._entries)))
        return key in self._entries

    def admit(
        self, path: str, value, frame_type: str | None = None, size_bytes: "int | None" = None
    ) -> bool:
        """Cache the artifact at `path` if its serialized size allows; return whether it did.

        `size_bytes` is the size when the caller already knows it; otherwise the store is
        asked. An artifact whose size is unknown or over `_LRU_MAX_ARTIFACT_BYTES` is not
        cached, and an entry already held for it is dropped.
        """
        if size_bytes is None:
            size_bytes = _artifact_size(path)
        if size_bytes is None or size_bytes > _LRU_MAX_ARTIFACT_BYTES:
            self._drop(self._key(path, frame_type))
            return False
        return self.put(path, value, frame_type, size_bytes=size_bytes)


def _default_artifact_dir() -> str:
    """Artifact store root: BARCA_ARTIFACT_URI if set, else local .barca/artifacts."""
    uri = os.environ.get("BARCA_ARTIFACT_URI")
    if uri:
        return uri
    return str(Path(".barca/artifacts").resolve())


_PROTOCOL_VERSION = 2
_use_socket = False


def _emit(msg_type, **fields):
    """Emit a protocol message — via socket if available, else stderr."""
    if _use_socket:
        from barca import _runtime

        if msg_type == "result":
            _runtime.emit_step_completed(fields["node_id"], fields["artifact"])
        elif msg_type == "error":
            _runtime.emit_step_error(
                node_id=fields["node_id"],
                error_type=fields["error_type"],
                message=fields["message"],
                traceback=fields["traceback"],
                elapsed=fields.get("elapsed", 0.0),
            )
        elif msg_type == "blocked":
            _runtime.emit_blocked(fields["node_id"], fields["reason"])
        return
    # Original stderr protocol
    payload = json.dumps({"type": msg_type, **fields})
    print(f"BARCA:{_PROTOCOL_VERSION}:{payload}", file=sys.stderr, flush=True)


def _user_traceback(exc) -> str:
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


def _emit_error(node_id, exc, elapsed=0.0):
    """Emit a structured failure for a single step. Rust owns the retry decision."""
    _emit(
        "error",
        node_id=node_id,
        error_type=type(exc).__name__,
        message=str(exc),
        traceback=_user_traceback(exc),
        elapsed=elapsed,
    )


def load_module(source_file):
    return load_pipeline_module(source_file)


def _run_with_timeout(fn, kwargs, timeout_seconds, *, label=None):
    """Run a function with a timeout. Raises TimeoutError if exceeded."""
    import threading
    from contextvars import copy_context

    result = None
    exception = None

    def target():
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
            f"Step '{label or fn.__name__}' exceeded timeout of {timeout_seconds}s"
        )
        # The step is still running in its thread: this interpreter is not free for more work.
        error._barca_still_running = True  # ty: ignore[unresolved-attribute]
        raise error
    if exception is not None:
        raise exception
    return result


def _resolve_input(raw_value, *, frame_type=None):
    """Resolve a provided input: artifact ref → deserialized value, else raw.

    For collected (fan-in) inputs, deserializes each partition artifact into a list.
    """
    if isinstance(raw_value, dict):
        if raw_value.get("_collected") and "artifacts" in raw_value:
            return _load_collected_artifacts(raw_value["artifacts"], frame_type=frame_type)
        if "path" in raw_value and "format" in raw_value:
            return deserialize(raw_value["path"], raw_value["format"], frame_type=frame_type)
    return raw_value


def _load_artifact(path, lru, fmt=None, *, frame_type=None):
    """Resolve one artifact path to its deserialized value via the tier-1 LRU
    cache, falling through to the artifact store on miss."""
    cacheable = _lru_frame_type(frame_type)
    hot = lru.get(path, frame_type) if cacheable else None
    if hot is not None:
        return hot
    if not _storage.exists(path):
        raise FileNotFoundError(f"Input artifact not found: {path}")
    if fmt is None:
        fmt = _EXT_FORMATS.get(_storage.suffix(path), "json")
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


def _load_collected_artifacts(artifacts, lru=None, *, param=None, frame_type=None):
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
    results = [None] * len(artifacts)
    to_fetch = []
    if not _lru_frame_type(frame_type):
        lru = None
    for i, artifact in enumerate(artifacts):
        hot = lru.get(artifact["path"], frame_type) if lru is not None else None
        if hot is not None:
            results[i] = hot
        else:
            to_fetch.append((i, artifact))

    if not to_fetch:
        return results

    def _fetch(item):
        _, artifact = item
        path = artifact["path"]
        if not _storage.exists(path):
            if param is not None:
                raise FileNotFoundError(f"Input artifact for parameter '{param}' not found: {path}")
            raise FileNotFoundError(f"Input artifact not found: {path}")
        fmt = artifact.get("format") or _EXT_FORMATS.get(_storage.suffix(path), "json")
        value = deserialize(path, fmt, frame_type=frame_type)
        # The size lookup is I/O too (a request, for a remote path), so it runs in the pool.
        return value, (_artifact_size(path) if lru is not None else None)

    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=min(len(to_fetch), _COLLECT_IO_MAX_WORKERS)) as ex:
        for (i, artifact), (value, size) in zip(to_fetch, ex.map(_fetch, to_fetch)):
            results[i] = value
            if lru is not None and size is not None:
                lru.admit(artifact["path"], value, frame_type, size)

    return results


# A dict `partitions_from` source's artifact, read once per worker however many of its keys run
# here: path -> the dict. Bounded: a run reads one version of each source.
_KEYED_SOURCES: dict = {}
_KEYED_SOURCES_MAX = 4
# Whether a step function takes a parameter: (function, name) -> bool.
_ACCEPTS: dict = {}


def _accepts(fn, param: str) -> bool:
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


def _keyed_value(path: str, key: str):
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
    if isinstance(value, (dict, list)):
        import copy

        # The step may change what it receives; the next key of this worker must not see it.
        value = copy.deepcopy(value)
    return value


_SOURCE_TYPES = "a list of keys or a dict of key -> value"
_JSON_VALUES = "str, int, float, bool, None, and lists and dicts of them"


def _check_partition_source(value, step):
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
    if isinstance(value, (list, tuple)):
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
    keys = {}
    for k in value:
        if isinstance(k, bool) or not isinstance(k, (str, int)):
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


def _sensor_result(result):
    if isinstance(result, bool):
        return result, None
    if isinstance(result, tuple) and len(result) == 2:
        changed, value = result
        if not isinstance(changed, bool):
            raise TypeError("a sensor's change signal must be a bool")
        return changed, value
    return None, result


def _execute(fn, kwargs, step):
    """Run a step function with optional timeout, unpacking sensor tuples."""
    timeout = step.get("timeout_seconds", 0)
    t0 = time.perf_counter()
    if timeout and timeout > 0:
        result = _run_with_timeout(fn, kwargs, timeout, label=step.get("node_id"))
    else:
        result = fn(**kwargs) if kwargs else fn()
    elapsed = time.perf_counter() - t0

    if step.get("kind") == "sensor":
        step["sensor_changed"], result = _sensor_result(result)
    return result, elapsed


def _sink_dest(path: str, node_id: str) -> str:
    """Sink destination path, with a partition suffix injected before the
    extension for partitioned assets so partitions don't clobber each other
    (e.g. out.parquet → out_ticker_AAPL.parquet). A partition step's id is
    `<base>[<key>]`; the suffix comes from the bracketed key."""
    bracket = node_id.find("[")
    if bracket < 0:
        return path
    part = safe_node_id(node_id[bracket:])
    ext = _storage.suffix(path)
    if ext:
        return path[: -len(ext)] + part + ext
    return path + part


def _through_symlink(dest: str) -> str:
    """Where a sink is really written when its path is a symlink.

    A sink path is the user's: barca writes the file there and changes nothing else.
    `serialize` installs a file by renaming a temp file over its path, which would replace a
    symlink by a regular file. So the link is followed first: the file it points to is
    written (as `open(path, "w")` would do) and the link stays. A link to a directory then
    fails the sink like a directory does.
    """
    local = _storage.local_path_of(dest)
    if local is not None and os.path.islink(local):
        return os.path.realpath(local)
    return dest


def _partition_values(step, node_id):
    """The partition key of the step being written, by dimension name."""
    values = dict(step.get("partition") or {})
    values.update(step.get("direct_kwargs") or {})
    bracket = node_id.find("[")
    if bracket >= 0 and node_id.endswith("]"):
        for pair in node_id[bracket + 1 : -1].split(","):
            name, sep, value = pair.partition("=")
            if sep:
                values.setdefault(name, value)
    return values


def _write_sink(result, sink, step, node_id, primary_fmt):
    dest = sink.get("path", "")
    if _sinks.is_dataset(dest):
        base = node_id.split("[", 1)[0]
        return _sinks.write(result, sink, _partition_values(step, node_id), base)
    fmt = sink.get("serializer") or _EXT_FORMATS.get(_storage.suffix(dest)) or primary_fmt
    if fmt not in ("json", "pickle", "parquet", "csv"):
        raise ValueError(
            f"sink serializer '{fmt}' is not supported yet (supported: json, pickle, parquet, csv)"
        )
    if fmt == "parquet" and resolve_format(result, fmt, warn=False) != "parquet":
        # An artifact may fall back to pickle (barca picks its file name), but a sink's
        # path is the user's promise to another system: never write pickle bytes there.
        raise ValueError(
            f"a {type(result).__name__} cannot be written as parquet; return a DataFrame, "
            "Arrow table or DuckDB relation, or sink it as json or pickle"
        )
    dest = _sink_dest(dest, node_id)
    size = serialize(result, _through_symlink(dest), fmt)
    return {"path": str(dest), "status": "ok", "size_bytes": size}


def _write_sinks(result, step, node_id, primary_fmt):
    """Write each @sink declared on the step.

    Every sink is attempted. If any failed, the step fails with a `SinkError` naming each
    failed sink: a missing export must not pass unnoticed in automation.
    """
    outcomes = []
    failures = []
    for sink in step.get("sinks") or []:
        dest = sink.get("path", "")
        try:
            outcomes.append(_write_sink(result, sink, step, node_id, primary_fmt))
        except Exception as exc:
            failures.append(f"sink {dest}: {type(exc).__name__}: {exc}")
    if failures:
        raise _sinks.SinkError("; ".join(_storage.safe_error(f) for f in failures))
    return outcomes


def _materialize(
    result,
    node_id,
    art_dir,
    step,
    elapsed,
    elapsed_in_artifact=False,
    timing=None,
    before_emit=None,
    prewritten=None,
    extra=None,
):
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
    explicit_fmt = step.get("serializer")
    if prewritten is not None:
        fmt = "parquet"
    else:
        fmt = resolve_format(result, detect_format(result, explicit=explicit_fmt))
    # Run-hash layout when the coordinator supplies a run hash.
    # Batch mode's legacy partitioned loop reuses the step-level hash only for
    # unpartitioned steps (a per-step hash is wrong per-partition; the daemon
    # path gets a per-item hash from Rust and batch mode is test-only).
    run_hash = step.get("run_hash") if node_id == step.get("node_id") else None
    branch_path = step.get("branch_path")
    if branch_path:
        # A parallel() branch: the coordinator names a file that belongs to this run and this
        # group alone, so the caller can only read what its own branches wrote (#332).
        path = Path(f"{branch_path}{_FORMAT_EXTENSIONS[fmt]}")
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
            import duckdb  # ty: ignore[unresolved-import]

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
        peak = _step_peak_rss_bytes(timing.pop("rss_window"))
        if peak:
            timing["max_rss_bytes"] = peak
    artifact: dict = {"path": str(path), "format": fmt, "size_bytes": size}
    if upload_snapshot is not None:
        artifact["upload_snapshot"] = upload_snapshot
    if content_hash is not None:
        artifact["content_hash"] = content_hash
    if isinstance(step.get("sensor_changed"), bool):
        artifact["sensor_changed"] = step["sensor_changed"]
    if step.get("branch") and fmt in ("parquet", "csv"):
        # The caller of parallel() gets back the frame type the branch returned.
        from barca import _branches

        artifact["frame_type"] = _branches.frame_type(result, fmt)
    if extra:
        artifact.update(extra)
    if elapsed_in_artifact:
        artifact["elapsed_seconds"] = elapsed
    if timing:
        artifact.update(timing)
    # When the step finished and how long it took, for telemetry: a span needs wall-clock times.
    artifact["finished_at"] = time.time()
    artifact["wall_seconds"] = elapsed
    sink_outcomes = _write_sinks(result, step, node_id, fmt)
    if sink_outcomes:
        artifact["sinks"] = sink_outcomes
    if before_emit is not None:
        before_emit()
    _emit("result", node_id=node_id, artifact=artifact, elapsed=elapsed)
    return artifact


def run_batch(batch):
    cache = {}
    modules = {}
    # node_ids (base or partition-suffixed) that failed or were blocked. A step is
    # skipped (blocked) if any input it depends on is unavailable — this lets
    # independent chains bundled in the same batch finish even when one fails.
    unavailable = set()

    # Artifact directory for writing outputs (local path or remote URI).
    art_dir = batch.get("artifact_dir") or os.environ.get("BARCA_ARTIFACT_URI")
    if art_dir and not _storage.is_remote(art_dir):
        Path(art_dir).mkdir(parents=True, exist_ok=True)
    clean_staging()

    # Pre-load provided inputs (cross-phase values injected by Rust).
    # Values may be artifact references — resolve them lazily when accessed.
    provided = batch.get("provided_inputs", {})
    for key, value in provided.items():
        cache[key] = _resolve_input(value)

    for step in batch["steps"]:
        partition_keys = step.get("partition_keys", [])
        if partition_keys:
            # Late partition expansion: worker loops over partition_keys internally.
            # Each partition key is a dict like {"ticker": "AAPL"}. Partitions are
            # independent — one bad partition does not block the others.
            for pk in partition_keys:
                suffix = ",".join(f"{k}={v}" for k, v in sorted(pk.items()))
                full_node_id = f"{step['node_id']}[{suffix}]"

                # Is any upstream this partition depends on unavailable?
                blocked_on = None
                for _param, upstream_id in step.get("inputs", {}).items():
                    aligned_id = f"{upstream_id}[{suffix}]"
                    if aligned_id in unavailable or upstream_id in unavailable:
                        blocked_on = upstream_id
                        break
                if blocked_on is not None:
                    unavailable.add(full_node_id)
                    _emit(
                        "blocked",
                        node_id=full_node_id,
                        reason=f"upstream '{blocked_on}' unavailable",
                    )
                    continue

                try:
                    source = str(Path(step["source_file"]).resolve())
                    activate_source_path(source)
                    if source not in modules:
                        modules[source] = load_module(source)
                    fn = getattr(modules[source], step["function_name"])

                    # Direct args/kwargs from parallel() dispatch — skip artifact lookup.
                    if "direct_args" in step or "direct_kwargs" in step:
                        d_args = step.get("direct_args", [])
                        d_kwargs = step.get("direct_kwargs", {})
                        timeout = step.get("timeout_seconds", 0)
                        t0 = time.time()
                        if timeout and timeout > 0:
                            result = _run_with_timeout(
                                lambda: fn(*d_args, **d_kwargs),
                                {},
                                timeout,
                                label=step.get("node_id"),
                            )
                        else:
                            result = fn(*d_args, **d_kwargs)
                        elapsed = time.time() - t0
                        cache[full_node_id] = result
                        _materialize(result, full_node_id, art_dir, step, elapsed)
                        continue
                    else:
                        kwargs = {}
                        for param_name, upstream_id in step.get("inputs", {}).items():
                            if param_name.startswith("_"):
                                kwargs[param_name] = None
                                continue
                            aligned_id = f"{upstream_id}[{suffix}]"
                            if aligned_id in cache:
                                kwargs[param_name] = cache[aligned_id]
                            elif upstream_id in cache:
                                kwargs[param_name] = cache[upstream_id]
                            else:
                                raise RuntimeError(
                                    f"Input '{param_name}' (from '{upstream_id}') not found in cache. "
                                    f"Tried aligned '{aligned_id}' and base '{upstream_id}'. "
                                    f"Available: {list(cache.keys())}"
                                )
                        kwargs.update(pk)  # inject partition values (e.g., ticker="AAPL").

                    result, elapsed = _execute(fn, kwargs, step)
                except Exception as exc:
                    unavailable.add(full_node_id)
                    _emit_error(full_node_id, exc)
                    continue

                try:
                    _materialize(result, full_node_id, art_dir, step, elapsed)
                except _sinks.SinkError as exc:
                    unavailable.add(full_node_id)
                    _emit_error(full_node_id, exc)
                    continue
                cache[full_node_id] = result
        else:
            node_id = step["node_id"]

            # Is any upstream this step depends on unavailable?
            blocked_on = None
            for _param, upstream_id in step.get("inputs", {}).items():
                if upstream_id in unavailable:
                    blocked_on = upstream_id
                    break
            if blocked_on is not None:
                unavailable.add(node_id)
                _emit("blocked", node_id=node_id, reason=f"upstream '{blocked_on}' unavailable")
                continue

            try:
                source = str(Path(step["source_file"]).resolve())
                activate_source_path(source)
                if source not in modules:
                    modules[source] = load_module(source)
                fn = getattr(modules[source], step["function_name"])

                # Direct args/kwargs from parallel() dispatch — skip artifact lookup.
                if "direct_args" in step or "direct_kwargs" in step:
                    d_args = step.get("direct_args", [])
                    d_kwargs = step.get("direct_kwargs", {})
                    timeout = step.get("timeout_seconds", 0)
                    t0 = time.time()
                    if timeout and timeout > 0:
                        result = _run_with_timeout(
                            lambda: fn(*d_args, **d_kwargs), {}, timeout, label=step.get("node_id")
                        )
                    else:
                        result = fn(*d_args, **d_kwargs)
                    elapsed = time.time() - t0
                    cache[node_id] = result
                    _materialize(result, node_id, art_dir, step, elapsed)
                    continue
                else:
                    kwargs = {}
                    for param_name, upstream_id in step.get("inputs", {}).items():
                        if param_name.startswith("_"):
                            kwargs[param_name] = None
                            continue
                        if upstream_id in cache:
                            kwargs[param_name] = cache[upstream_id]
                        else:
                            raise RuntimeError(
                                f"Input '{param_name}' (from '{upstream_id}') not found in cache. "
                                f"Available: {list(cache.keys())}"
                            )
                    if "partition" in step:
                        kwargs.update(step["partition"])

                result, elapsed = _execute(fn, kwargs, step)
            except Exception as exc:
                unavailable.add(node_id)
                _emit_error(node_id, exc)
                continue

            try:
                _materialize(result, node_id, art_dir, step, elapsed)
            except _sinks.SinkError as exc:
                unavailable.add(node_id)
                _emit_error(node_id, exc)
                continue
            cache[node_id] = result


# What a worker does with Ctrl-C (SIGINT). The terminal sends it to every process of the job,
# and the coordinator, which gets it too, decides what it means for the run and stops the
# workers. A worker acts on it only while a step's function runs:
#
# - While the interpreter starts and barca is imported: it does not get the signal at all.
#   The coordinator starts a worker in a process group of its own
#   (`helper_proc::start_outside_the_job`), since nothing written here can keep an interrupt
#   that arrives that early from ending the process with a traceback. `_join_the_job` moves
#   the worker into the coordinator's group once the handler below is installed.
# - While a step's module is imported, between steps, and while a result or an error is
#   reported: nothing happens (`_ctrl_c_does_nothing`). An interrupt there used to end the
#   worker with a `KeyboardInterrupt` traceback of barca's own frames or of the module's
#   import, which tells the user nothing.
# - While a step runs, from loading its inputs to writing its result: `KeyboardInterrupt` is
#   raised in the step (`_ctrl_c_interrupts_the_step`), as before. The worker reports the step
#   as interrupted and the coordinator leaves it out of the cancelled run. The coordinator's
#   SIGTERM follows the interrupt and ends the worker at once (`_on_sigterm`), so a step
#   cannot count on its `except` or `finally` code running.
#
# One exception to all of it: a barca that was itself started with SIGINT ignored (`nohup`, a
# background job of a non-interactive shell, cron, some supervisors). Its workers inherit that,
# and keep it: SIGINT stays ignored at every stage, a step included, which is what Python does
# for any program started that way and what a step saw before these stages existed. The
# coordinator still acts on a SIGINT sent to it and stops the workers.

# Whether this process was started with SIGINT ignored. Read before anything changes it.
_ctrl_c_inherited_ignored: bool | None = None


def _started_with_ctrl_c_ignored() -> bool:
    global _ctrl_c_inherited_ignored
    if _ctrl_c_inherited_ignored is None:
        import signal

        _ctrl_c_inherited_ignored = signal.getsignal(signal.SIGINT) is signal.SIG_IGN
    return _ctrl_c_inherited_ignored


def _no_interrupt(_signum, _frame) -> None:
    pass


def _ctrl_c_does_nothing() -> None:
    """Ctrl-C has no effect on this process until a step runs.

    A handler that does nothing, not `SIG_IGN`: an ignored signal is inherited as ignored by
    every process the user's code starts, a handled one is not.
    """
    import signal

    if _started_with_ctrl_c_ignored():
        return
    try:
        signal.signal(signal.SIGINT, _no_interrupt)
    except ValueError:
        pass  # not the main thread: nothing to change


def _ctrl_c_interrupts_the_step() -> None:
    """Ctrl-C raises `KeyboardInterrupt` in the main thread, as in any Python program."""
    import signal

    if _started_with_ctrl_c_ignored():
        return
    try:
        signal.signal(signal.SIGINT, signal.default_int_handler)
    except ValueError:
        pass  # not the main thread: nothing to change


def _join_the_job() -> None:
    """Move this worker into the coordinator's process group, where the terminal's Ctrl-C is
    delivered. Called once `_ctrl_c_does_nothing` is in place; until then the worker is in a
    group of its own, out of the terminal's reach."""
    job = os.environ.get("BARCA_JOB_PGID")
    if not job:
        return  # started by hand, or by a coordinator that leaves workers in its group
    try:
        os.setpgid(0, int(job))
    except (OSError, ValueError):
        pass  # the coordinator's group is gone; so is the coordinator, and the socket says so


def _worker_survives(exc, imported: bool) -> bool:
    """Whether this worker may take more work after a step failed with `exc` (#523).

    An exception raised by user code (or while reading its inputs or writing its result)
    unwinds cleanly: the worker reports it and goes on, as it does after a success. It stops,
    and the coordinator starts a fresh worker, when this interpreter may not be sound: the
    module failed to import (a half-run module body), the step called sys.exit() or was
    interrupted, it ran out of memory, it is still running past its timeout, or a parallel()
    exchange with the coordinator was left unfinished.
    """
    from barca import _runtime

    return (
        imported
        and isinstance(exc, Exception)
        and not isinstance(exc, MemoryError)
        and not getattr(exc, "_barca_still_running", False)
        and not _runtime.exchange_broken
    )


def _run_daemon_step(step, modules, art_dir, lru):
    """Execute one step in daemon mode and emit its result or error.

    Returns True when this worker may go on to its next step (the step succeeded, or failed
    with an exception that left the interpreter sound), False when it must stop. Socket
    errors raised while emitting propagate to the caller (the connection is gone — exit the
    loop).
    Per-task self-timing: CPU time (`process_time`, the truest measure of
    work), wall time, and peak RSS ride back on the completion message.
    """
    from barca import _runtime
    from barca._telemetry import Execution

    tracing = Execution(step)
    node_id = step.get("node_id", "unknown")
    t0 = time.perf_counter()
    c0 = time.process_time()
    rss_window = _start_rss_window()

    # Views bound for duckdb-typed inputs. They must outlive materialization: a returned
    # relation is lazy and may still reference them when it is written to parquet.
    bound_views: list[str] = []
    # Set when this step is a parallel() branch whose return value could not be written.
    unwritable = False
    import_seconds = None
    # Whether the step's module is imported and its function found: a failure before that
    # stops the worker (`_worker_survives`).
    imported = False

    try:
        source = str(Path(step["source_file"]).resolve())
        activate_source_path(source)
        emitter = _stdout_for_step(node_id)
        err_emitter = _stderr_for_step(node_id)
        if source not in modules:
            # Only under `--agent` (and in `barca serve`): a person's terminal shows the progress
            # bar, and the import time is in the run JSON as `import_seconds` (#496).
            if os.environ.get("BARCA_WORKER_AGENT") == "1":
                print(f"[barca] importing {source}", file=sys.stderr, flush=True)
            import_start = time.perf_counter()
            try:
                with _captured(emitter, err_emitter):
                    modules[source] = load_module(source)
            finally:
                import_seconds = time.perf_counter() - import_start
        fn = getattr(modules[source], step["function_name"])
        imported = True

        # From here until the result is written, Ctrl-C interrupts the step.
        _ctrl_c_interrupts_the_step()

        # Direct args/kwargs from parallel() dispatch.
        d_args = step.get("direct_args", [])
        d_kwargs = step.get("direct_kwargs", {})

        # Resolve dag_inputs as function arguments.
        inputs = step.get("inputs", {})
        param_types = step.get("param_types") or {}
        kwargs = dict(d_kwargs) if d_kwargs else {}
        for param, value in inputs.items():
            frame_type = param_types.get(param)
            # Skip ordering-only deps (underscore-prefixed params carry no data).
            if param.startswith("_"):
                kwargs[param] = None
                continue
            # Fan-in (collect()): every partition artifact of the upstream,
            # deserialized into a list — matches batch mode's _resolve_input.
            # Cache misses load concurrently (see _load_collected_artifacts).
            if isinstance(value, dict) and value.get("_collected"):
                kwargs[param] = _load_collected_artifacts(
                    value.get("artifacts", []),
                    lru,
                    param=param,
                    frame_type=frame_type,
                )
                continue
            # A dict `partitions_from` source: this key's value, when the function takes it.
            if isinstance(value, dict) and value.get("_keyed"):
                if _accepts(fn, param):
                    kwargs[param] = _keyed_value(value["path"], value["key"])
                continue
            if not value:
                continue
            try:
                kwargs[param] = _load_artifact(value, lru, frame_type=frame_type)
            except FileNotFoundError:
                raise FileNotFoundError(
                    f"Input artifact for parameter '{param}' not found: {value}"
                ) from None

        bound_views = _duckdb.bind_inputs(kwargs, param_types)
        # Legacy pickle recovery may have activated a different producer directory.
        activate_source_path(source)

        timeout = step.get("timeout_seconds", 0)
        # Capture user stdout and stderr and stream them live, line by line.
        with _captured(emitter, err_emitter):
            if d_args:
                if timeout and timeout > 0:
                    result = _run_with_timeout(
                        lambda: fn(*d_args, **kwargs), {}, timeout, label=node_id
                    )
                else:
                    result = fn(*d_args, **kwargs)
            else:
                if timeout and timeout > 0:
                    result = _run_with_timeout(lambda: fn(**kwargs), {}, timeout, label=node_id)
                else:
                    result = fn(**kwargs)

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
            peak = _step_peak_rss_bytes(rss_window)
            if peak is not None:
                artifact["max_rss_bytes"] = peak
            tracing.finish()
            _emit("result", node_id=node_id, artifact=artifact, elapsed=wall)
            return True

        # Convert ParallelError instances so results are JSON-serializable.
        from barca import ParallelError

        def _make_serializable(v):
            if isinstance(v, ParallelError):
                return v.to_dict()
            if isinstance(v, list):
                return [_make_serializable(x) for x in v]
            return v

        result = _make_serializable(result)

        # Read by a `partitions_from(...)` for keys: a list, or a dict with JSON values.
        materialize_step = step
        if step.get("partition_source"):
            result = _check_partition_source(result, step)
            materialize_step = {**step, "serializer": "json"}

        source_observation = None
        if step.get("partition_source"):
            from barca._source_observation import observe

            source_observation = observe(result, step.get("previous_sensor"))

        # Source validation and comparison are producer work too, not coordinator overhead.
        wall = time.perf_counter() - t0
        cpu = time.process_time() - c0
        # Serialize result to artifact (and write any declared sinks).
        try:
            artifact = _materialize(
                result,
                node_id,
                art_dir,
                materialize_step,
                wall,
                elapsed_in_artifact=True,
                timing={
                    "cpu_seconds": cpu,
                    # Read after the result is written: a returned DuckDB relation or lazy
                    # frame does its work while it is serialized.
                    "rss_window": rss_window,
                    **(
                        {"import_seconds": import_seconds}
                        if import_seconds and import_seconds > 1
                        else {}
                    ),
                },
                extra={
                    **({"sensor_changed": sensor_changed} if sensor_changed is not None else {}),
                    **(
                        {"source_observation": source_observation}
                        if source_observation is not None
                        else {}
                    ),
                },
                before_emit=tracing.finish,
            )
        except Exception as exc:
            if not step.get("branch"):
                raise
            # A parallel() branch whose return value cannot be written: say so in terms of
            # the branch, for the step that called it (barca/_branches.py).
            from barca import _branches

            unwritable = True
            raise _branches.unwritable(step, result, exc) from exc
        # A downstream step in this worker may consume what we just produced, keyed by the
        # reader it is equivalent to so a consumer never gets a different frame type.
        # (Not a branch's result: no step of this worker reads it, and its file is removed
        # when the calling step ends.)
        result_type = _result_frame_type(result)
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
        message = str(exc)
        if isinstance(exc, SystemExit):
            message = (
                f"{message} (the step called sys.exit(); a step must return a value or raise "
                "an exception, and barca reports any sys.exit() as a failure)"
            )
        note = _duckdb.explain_error(exc, bound_views)
        if note:
            message = f"{message}\n\n{note}"
        tracing.finish(exc)
        from barca import _branches

        survives = _worker_survives(exc, imported)
        _runtime.emit_step_error(
            node_id=node_id,
            # Only this branch's own return value that could not be written is reported as
            # such. A branch that raises, a BranchResultError from a parallel() of its own
            # included, is a branch that raised.
            error_type=_branches.UNRETURNABLE if unwritable else type(exc).__name__,
            message=message,
            traceback=_user_traceback(exc),
            elapsed=wall,
            import_seconds=import_seconds if import_seconds and import_seconds > 1 else None,
            worker_continues=survives,
        )
        return survives

    finally:
        _ctrl_c_does_nothing()
        tracing.finish()
        _duckdb.unbind_inputs(bound_views)


def _run_daemon_call(steps, call, modules, art_dir, lru):
    """Run the keys of one batched call (RFC-0013): one call of the function for all of them,
    then each key written and reported on its own (`barca/_batch.py`).

    Returns True when this worker may go on, as `_run_daemon_step` does.
    """
    from barca import _batch, _runtime
    from barca._telemetry import Execution

    first = steps[0]
    dims = _batch.key_dims(steps)
    keys = [_batch.key_of(st, dims) for st in steps]
    texts = [_batch.key_text(k, dims) for k in keys]
    base = first.get("node_id", "unknown").split("[", 1)[0]
    call_id = call.get("id", "")
    # The call's own output goes to the log under `<node>[batch=<id>]`: recorded once, and
    # found from each of its keys (`barca logs --step 'node[key]'`).
    log_id = f"{base}[batch={call_id}]"
    tracing = Execution(first)
    if tracing.span is not None:
        try:
            tracing.span.set_metric("barca.batch.keys", len(steps))
            tracing.span.set_tag("barca.batch", call_id)
        except Exception:
            pass
    t0 = time.perf_counter()
    c0 = time.process_time()
    rss_window = _start_rss_window()
    import_seconds = None
    imported = False
    reported = set()
    n = len(steps)
    bound_views: list[str] = []

    def fail_all(exc, survives, wall):
        """Report every key of the call not yet reported as failed with `exc`."""
        message = str(exc)
        if isinstance(exc, SystemExit):
            message = (
                f"{message} (the step called sys.exit(); a step must return a value or raise "
                "an exception, and barca reports any sys.exit() as a failure)"
            )
        traceback = _user_traceback(exc)
        left = [st for st in steps if st["node_id"] not in reported]
        for i, st in enumerate(left):
            reported.add(st["node_id"])
            _runtime.emit_step_error(
                node_id=st["node_id"],
                error_type=type(exc).__name__,
                message=message,
                traceback=traceback,
                elapsed=wall / max(n, 1),
                import_seconds=import_seconds if import_seconds and import_seconds > 1 else None,
                # The coordinator stops an unsound worker at the first report that says so:
                # every key is reported before that.
                worker_continues=survives or i < len(left) - 1,
            )

    try:
        source = str(Path(first["source_file"]).resolve())
        activate_source_path(source)
        emitter = _stdout_for_step(log_id)
        err_emitter = _stderr_for_step(log_id)
        if source not in modules:
            if os.environ.get("BARCA_WORKER_AGENT") == "1":
                print(f"[barca] importing {source}", file=sys.stderr, flush=True)
            import_start = time.perf_counter()
            try:
                with _captured(emitter, err_emitter):
                    modules[source] = load_module(source)
            finally:
                import_seconds = time.perf_counter() - import_start
        fn = getattr(modules[source], first["function_name"])
        imported = True
        _ctrl_c_interrupts_the_step()

        param_types = first.get("param_types") or {}
        kwargs = {d: [st["direct_kwargs"][d] for st in steps] for d in dims}
        aligned = set(first.get("aligned_inputs") or [])
        params = []
        for st in steps:
            for p in st.get("inputs") or {}:
                if p not in params:
                    params.append(p)
        for param in params:
            frame_type = param_types.get(param)
            if param.startswith("_"):
                kwargs[param] = None
                continue
            value = (first.get("inputs") or {}).get(param)
            if isinstance(value, dict) and value.get("_collected"):
                kwargs[param] = _load_collected_artifacts(
                    value.get("artifacts", []), lru, param=param, frame_type=frame_type
                )
                continue
            if isinstance(value, dict) and value.get("_keyed"):
                if _accepts(fn, param):
                    kwargs[param] = {
                        k: _keyed_value(st["inputs"][param]["path"], st["inputs"][param]["key"])
                        for k, st in zip(keys, steps)
                    }
                continue
            if param not in aligned:
                if value:
                    kwargs[param] = _load_artifact(value, lru, frame_type=frame_type)
                continue
            paths = []
            for k, st in zip(texts, steps):
                path = (st.get("inputs") or {}).get(param)
                if not path:
                    raise FileNotFoundError(
                        f"Input artifact for parameter '{param}' of {k} was not produced"
                    )
                paths.append(path)
            if frame_type in _batch.LAZY_TYPES:
                kwargs[param] = _batch.lazy_table(param, frame_type, dims, keys, paths, texts)
            else:
                kwargs[param] = {
                    k: _load_artifact(path, lru, frame_type=frame_type)
                    for k, path in zip(keys, paths)
                }

        bound_views.extend(_duckdb.bind_inputs(kwargs, param_types))
        activate_source_path(source)
        timeout = first.get("timeout_seconds", 0)
        with _captured(emitter, err_emitter):
            if timeout and timeout > 0:
                result = _run_with_timeout(lambda: fn(**kwargs), {}, timeout, label=log_id)
            else:
                result = fn(**kwargs)

        staging = Path(art_dir if not _storage.is_remote(art_dir) else ".barca") / (
            f".batch-{os.getpid()}-{call_id}"
        )
        explicit = first.get("serializer")
        values = _batch.split(result, keys, dims, staging)
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
    ok = True
    try:
        for k, text, st in zip(keys, texts, steps):
            node_id = st["node_id"]
            n_key = _batch._norm(k, dims)
            try:
                if n_key not in values:
                    raise _batch.BatchFailed(f"batch returned no result for {text}")
                value = values[n_key]
                if isinstance(value, _batch.Failed):
                    raise _batch.BatchFailed(value.reason or f"{text} failed")
                prewritten = None
                if isinstance(value, tuple) and len(value) == 2 and value[0] == "parquet":
                    files = value[1]
                    if (
                        len(files) == 1
                        and Path(files[0]).name != "__empty.parquet"
                        and (explicit in (None, "parquet"))
                    ):
                        prewritten = files[0]
                        value = None
                    else:
                        import duckdb  # ty: ignore[unresolved-import]

                        value = duckdb.read_parquet(files)
                _materialize(
                    value,
                    node_id,
                    art_dir,
                    st,
                    share,
                    elapsed_in_artifact=True,
                    timing={
                        "cpu_seconds": cpu / max(n, 1),
                        "rss_window": rss_window,
                    },
                    prewritten=prewritten,
                    extra={"batch": call_id, "batch_keys": n},
                )
                reported.add(node_id)
            except (BrokenPipeError, ConnectionResetError):
                raise
            except Exception as exc:
                reported.add(node_id)
                if isinstance(exc, _batch.BatchFailed):
                    error_type, traceback = "BatchKeyFailed", ""
                else:
                    error_type, traceback = type(exc).__name__, _user_traceback(exc)
                _runtime.emit_step_error(
                    node_id=node_id,
                    error_type=error_type,
                    message=str(exc),
                    traceback=traceback,
                    elapsed=share,
                    worker_continues=True,
                )
    except BaseException as exc:
        _ctrl_c_does_nothing()
        if isinstance(exc, (BrokenPipeError, ConnectionResetError)):
            raise
        ok = _worker_survives(exc, True)
        fail_all(exc, ok, time.perf_counter() - t0)
    finally:
        _ctrl_c_does_nothing()
        tracing.finish()
        _duckdb.unbind_inputs(bound_views)
        _batch.unbind()
        import shutil

        shutil.rmtree(
            Path(art_dir if not _storage.is_remote(art_dir) else ".barca")
            / f".batch-{os.getpid()}-{call_id}",
            ignore_errors=True,
        )
    return ok


def run_daemon():
    """Daemon mode: read execute commands from socket, run each step, send results."""
    global _use_socket

    _ctrl_c_does_nothing()
    _join_the_job()

    from barca import _runtime

    if _runtime.connect() is None:
        print("BARCA_SOCKET not set", file=sys.stderr)
        sys.exit(1)
    _use_socket = True

    # Collapse repeated library warnings (barca docs agents, "Repeated warnings").
    from barca import _dedupe

    _dedupe.install(os.environ.get("BARCA_SOCKET"))

    # Install SIGTERM handler so graceful_kill flushes buffered progress output
    # before the process goes away. Exit via os._exit, not sys.exit(0): a
    # SystemExit raised from the handler while the interpreter is already
    # shutting down is uncatchable, and Python prints it as a noisy
    # "Exception ignored in: _on_sigterm ... SystemExit: 0" on stderr — which
    # then leaks into surfaced worker errors (reliably on Linux). os._exit
    # cannot raise, so that noise is impossible.
    import signal

    def _on_sigterm(_signum, _frame):
        try:
            sys.stdout.flush()
            sys.stderr.flush()
        except Exception:
            pass
        os._exit(0)

    signal.signal(signal.SIGTERM, _on_sigterm)

    modules = {}
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
        except Exception:
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


def main():
    global _use_socket

    if len(sys.argv) >= 2 and sys.argv[1] == "--daemon":
        run_daemon()
        return

    if len(sys.argv) < 2:
        print("Usage: python -m barca._worker <batch.json>", file=sys.stderr)
        sys.exit(1)

    # Connect to executor's Unix socket if available.
    from barca import _runtime

    if _runtime.connect() is not None:
        _use_socket = True
        _runtime.start_heartbeat()

    with open(sys.argv[1]) as f:
        batch = json.load(f)

    try:
        run_batch(batch)
    finally:
        if _use_socket:
            _runtime.stop_heartbeat()
            _runtime.disconnect()


if __name__ == "__main__":
    main()
