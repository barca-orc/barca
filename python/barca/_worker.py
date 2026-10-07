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
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from barca import _duckdb, _storage
from barca._source_import import load_package_module, load_source_module
from barca._artifacts import (
    LAZY_FRAME_TYPES,
    _frame_kind,
    artifact_path,
    clean_staging,
    deserialize,
    detect_format,
    resolve_format,
    safe_node_id,
    serialize,
    serialize_hashed,
)

_EXT_FORMATS = {
    ".json": "json",
    ".pkl": "pickle",
    ".pickle": "pickle",
    ".parquet": "parquet",
}


class _LineEmitter(io.TextIOBase):
    """A stdout proxy that streams complete lines to the coordinator as they
    are written, so user `print()` output shows up live in the UI — while still
    passing the text through to the real stdout, preserving the terminal
    behaviour CLI users expect. Buffers a partial trailing line until the next
    newline or an explicit flush."""

    def __init__(self, node_id):
        self.node_id = node_id
        self._buf = ""
        # The real stdout, captured before redirect_stdout swaps sys.stdout.
        self._passthrough = sys.stdout

    def write(self, s):
        from barca import _runtime

        # Tee to the real stdout so `print()` still shows on the terminal.
        try:
            self._passthrough.write(s)
        except Exception:
            pass
        self._buf += s
        while "\n" in self._buf:
            line, self._buf = self._buf.split("\n", 1)
            _runtime.emit_log(self.node_id, line)
        return len(s)

    def flush(self):
        from barca import _runtime

        try:
            self._passthrough.flush()
        except Exception:
            pass
        if self._buf:
            _runtime.emit_log(self.node_id, self._buf)
            self._buf = ""


def _peak_rss_bytes() -> int:
    """Peak RSS of this process in bytes (0 if unavailable).

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
    barca_dir = str(Path(__file__).resolve().parent)
    frames = [
        f
        for f in traceback.extract_tb(exc.__traceback__)
        if not str(Path(f.filename).resolve()).startswith(barca_dir)
    ]
    if not frames:
        return ""
    return "".join(traceback.format_list(frames)).rstrip("\n")


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
    # Compiled from the source on disk, never a cached .pyc (#176); the file's directory
    # goes on sys.path so cross-file imports work, and those compile from source too.
    path = Path(source_file).resolve()
    dotted = package_module_name(path)
    if dotted is not None:
        return load_package_module(dotted)
    return load_source_module(str(path), module_name_for(path))


def package_module_name(path: Path) -> str | None:
    """The importable name of a step's file when it sits in a package under the project root
    (every directory from the root down has an `__init__.py`): `pipelines/reconcile.py` is
    `pipelines.reconcile`. Loading it under that name gives it a parent package, so relative
    imports (`from .sources import x`) work, and `from pipelines.reconcile import y` elsewhere
    gets the same module. `None` for a file in the root or outside a package."""
    root = Path.cwd().resolve()
    try:
        parts = list(path.relative_to(root).with_suffix("").parts)
    except ValueError:
        return None
    if parts and parts[-1] == "__init__":
        parts.pop()
    if len(parts) < 2:
        return None
    for i in range(1, len(parts)):
        if not root.joinpath(*parts[:i], "__init__.py").is_file():
            return None
    return ".".join(parts)


def module_name_for(path: Path) -> str:
    """`sys.modules` name for a step's file: `_barca_` plus its path relative to the project
    root (the cwd), so `east/assets.py` and `west/assets.py` stay distinct modules. A file in the
    root keeps the plain `_barca_<stem>` name, which pickled artifacts refer to."""
    try:
        rel = path.relative_to(Path.cwd().resolve()).with_suffix("")
    except ValueError:
        return f"_barca_{path.stem}"
    return "_barca_" + "__".join(rel.parts)


def _run_with_timeout(fn, kwargs, timeout_seconds):
    """Run a function with a timeout. Raises TimeoutError if exceeded."""
    import threading

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

    thread = threading.Thread(target=target)
    thread.daemon = True
    thread.start()
    thread.join(timeout=timeout_seconds)

    if thread.is_alive():
        raise TimeoutError(f"Function '{fn.__name__}' exceeded timeout of {timeout_seconds}s")
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

    with ThreadPoolExecutor(max_workers=min(len(to_fetch), _COLLECT_IO_MAX_WORKERS)) as ex:
        for (i, artifact), (value, size) in zip(to_fetch, ex.map(_fetch, to_fetch)):
            results[i] = value
            if lru is not None and size is not None:
                lru.admit(artifact["path"], value, frame_type, size)

    return results


def _execute(fn, kwargs, step):
    """Run a step function with optional timeout, unpacking sensor tuples."""
    timeout = step.get("timeout_seconds", 0)
    t0 = time.perf_counter()
    if timeout and timeout > 0:
        result = _run_with_timeout(fn, kwargs, timeout)
    else:
        result = fn(**kwargs) if kwargs else fn()
    elapsed = time.perf_counter() - t0

    # Sensors return (updated: bool, data) tuples — unpack for downstream.
    if step.get("kind") == "sensor" and isinstance(result, tuple) and len(result) == 2:
        _updated, result = result
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


def _write_sinks(result, step, node_id, primary_fmt):
    """Write each @sink declared on the step. Error-isolated: a sink failure
    never fails the parent asset — it is logged and reported in the outcome."""
    outcomes = []
    for sink in step.get("sinks") or []:
        dest = sink.get("path", "")
        try:
            fmt = sink.get("serializer") or _EXT_FORMATS.get(_storage.suffix(dest)) or primary_fmt
            if fmt not in ("json", "pickle", "parquet"):
                raise ValueError(
                    f"sink serializer '{fmt}' is not supported yet "
                    "(supported: json, pickle, parquet)"
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
            outcomes.append({"path": str(dest), "status": "ok", "size_bytes": size})
        except Exception as exc:
            print(
                f"[barca] SINK FAILED: {node_id} -> {dest}: {type(exc).__name__}: {exc}",
                file=sys.stderr,
                flush=True,
            )
            outcomes.append(
                {
                    "path": str(dest),
                    "status": "error",
                    "error": f"{type(exc).__name__}: {exc}",
                }
            )
    return outcomes


def _materialize(result, node_id, art_dir, step, elapsed, elapsed_in_artifact=False, timing=None):
    """Serialize a result to its artifact and emit a `result` protocol message.

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
    fmt = resolve_format(result, detect_format(result, explicit=explicit_fmt))
    # Run-hash layout when the coordinator supplies a run hash.
    # Batch mode's legacy partitioned loop reuses the step-level hash only for
    # unpartitioned steps (a per-step hash is wrong per-partition; the daemon
    # path gets a per-item hash from Rust and batch mode is test-only).
    run_hash = step.get("run_hash") if node_id == step.get("node_id") else None
    path = artifact_path(art_dir, node_id, fmt, run_hash)
    # A directory where the artifact file belongs is not an artifact: it is moved out of the
    # way (never deleted) so the step's result can be written. Sinks are the user's paths and
    # never get this treatment.
    local = _storage.local_path_of(path)
    if local is not None:
        _storage.make_way(local)
    _ser_wall0 = time.perf_counter()
    _ser_cpu0 = time.process_time()
    # A sensor's output is hashed: the coordinator folds the hash into the run hash of every
    # asset that reads the sensor, so a changed output re-runs them.
    content_hash = None
    if step.get("kind") == "sensor":
        size, content_hash = serialize_hashed(result, path, fmt)
    else:
        size = serialize(result, path, fmt)
    elapsed += time.perf_counter() - _ser_wall0
    if timing and timing.get("cpu_seconds") is not None:
        timing = {
            **timing,
            "cpu_seconds": timing["cpu_seconds"] + (time.process_time() - _ser_cpu0),
        }
    artifact: dict = {"path": str(path), "format": fmt, "size_bytes": size}
    if content_hash is not None:
        artifact["content_hash"] = content_hash
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
                            result = _run_with_timeout(lambda: fn(*d_args, **d_kwargs), {}, timeout)
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

                cache[full_node_id] = result
                _materialize(result, full_node_id, art_dir, step, elapsed)
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
                        result = _run_with_timeout(lambda: fn(*d_args, **d_kwargs), {}, timeout)
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

            cache[node_id] = result
            _materialize(result, node_id, art_dir, step, elapsed)


def _ignore_further_interrupts() -> None:
    import signal

    try:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
    except ValueError:
        pass  # not the main thread: nothing to change


def _run_daemon_step(step, modules, art_dir, lru):
    """Execute one step in daemon mode and emit its result or error.

    Returns True on success, False on step failure. Socket errors raised while
    emitting propagate to the caller (the connection is gone — exit the loop).
    Per-task self-timing: CPU time (`process_time`, the truest measure of
    work), wall time, and peak RSS ride back on the completion message.
    """
    from barca import _runtime

    node_id = step.get("node_id", "unknown")
    t0 = time.perf_counter()
    c0 = time.process_time()

    # Views bound for duckdb-typed inputs. They must outlive materialization: a returned
    # relation is lazy and may still reference them when it is written to parquet.
    bound_views: list[str] = []

    try:
        source = str(Path(step["source_file"]).resolve())
        if source not in modules:
            modules[source] = load_module(source)
        fn = getattr(modules[source], step["function_name"])

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
            if not value:
                continue
            try:
                kwargs[param] = _load_artifact(value, lru, frame_type=frame_type)
            except FileNotFoundError:
                raise FileNotFoundError(
                    f"Input artifact for parameter '{param}' not found: {value}"
                ) from None

        bound_views = _duckdb.bind_inputs(kwargs, param_types)

        timeout = step.get("timeout_seconds", 0)
        # Capture user stdout and stream it live, line by line.
        emitter = _LineEmitter(node_id)
        with contextlib.redirect_stdout(emitter):
            try:
                if d_args:
                    if timeout and timeout > 0:
                        result = _run_with_timeout(lambda: fn(*d_args, **kwargs), {}, timeout)
                    else:
                        result = fn(*d_args, **kwargs)
                else:
                    if timeout and timeout > 0:
                        result = _run_with_timeout(lambda: fn(**kwargs), {}, timeout)
                    else:
                        result = fn(**kwargs)
            finally:
                # Emit any trailing partial line, even if the step raised.
                emitter.flush()

        wall = time.perf_counter() - t0
        cpu = time.process_time() - c0

        # Sensors return (updated: bool, data) tuples — unpack for downstream.
        if step.get("kind") == "sensor" and isinstance(result, tuple) and len(result) == 2:
            _updated, result = result

        # Convert ParallelError instances so results are JSON-serializable.
        from barca import ParallelError

        def _make_serializable(v):
            if isinstance(v, ParallelError):
                return v.to_dict()
            if isinstance(v, list):
                return [_make_serializable(x) for x in v]
            return v

        result = _make_serializable(result)

        # Serialize result to artifact (and write any declared sinks).
        artifact = _materialize(
            result,
            node_id,
            art_dir,
            step,
            wall,
            elapsed_in_artifact=True,
            timing={"cpu_seconds": cpu, "max_rss_bytes": _peak_rss_bytes()},
        )
        # A downstream step in this worker may consume what we just produced, keyed by the
        # reader it is equivalent to so a consumer never gets a different frame type.
        result_type = _result_frame_type(result)
        if result_type is not False:
            lru.admit(artifact["path"], result, result_type, artifact.get("size_bytes"))
        return True

    except BaseException as exc:
        # Any failure inside the step — including TimeoutError and OSError
        # from user code — is a step error. (TimeoutError and the socket
        # errors are OSError subclasses, so a socket-error catch here would
        # swallow them; genuine socket death surfaces when the emit below
        # fails, and that propagates to the caller.)
        if isinstance(exc, KeyboardInterrupt):
            # Ctrl-C reached this worker and interrupted the step. A second Ctrl-C (people
            # press it twice) must not interrupt the report of the first: it would leave
            # this function as an uncaught KeyboardInterrupt and print a traceback. The
            # coordinator has the same signal and stops this worker.
            _ignore_further_interrupts()
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
        _runtime.emit_step_error(
            node_id=node_id,
            error_type=type(exc).__name__,
            message=message,
            traceback=_user_traceback(exc),
            elapsed=wall,
        )
        return False

    finally:
        _duckdb.unbind_inputs(bound_views)


def run_daemon():
    """Daemon mode: read execute commands from socket, run each step, send results."""
    global _use_socket

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
        # each result message goes back; a failure stops the batch (the
        # coordinator kills this worker for a fresh interpreter and re-queues
        # the unstarted remainder).
        if msg.get("type") == "execute_batch":
            steps = msg.get("steps", [])
        elif msg.get("type") == "execute":
            steps = [msg.get("step", {})]
        else:
            continue

        try:
            for step in steps:
                if not _run_daemon_step(step, modules, art_dir, lru):
                    break
        except (BrokenPipeError, ConnectionResetError, OSError):
            # Socket was closed (e.g. replacement worker killed) — exit cleanly.
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
