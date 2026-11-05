"""The tier-1 artifact cache: deserialized values hot in one worker process.

Keyed by (path, frame type). Paths are named by run hash (`{node}/{run_hash}{ext}`), so a path
identifies its content and invalidation is automatic. The cache never gates correctness: a miss
reads the artifact from the store (tier 2).
"""

from __future__ import annotations

from collections import OrderedDict
from typing import Any, Literal

from barca import _storage
from barca._artifacts import LAZY_FRAME_TYPES, _frame_kind

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


def is_cacheable_frame_type(frame_type: str | None) -> bool:
    """Whether values read with this frame type may be cached.

    Lazy values (duckdb relations, polars LazyFrames) are cheap to recreate and read their file
    when queried; a remote input's file is deleted when the step ends, so a cached one would
    break the next step that used it.
    """
    return frame_type not in LAZY_FRAME_TYPES


def cacheable_frame_type(value: Any) -> str | None | Literal[False]:
    """The frame type a step's result may be cached under, for a reader of that type.

    Returns the frame type (None for non-frame values, which every reader ignores), or False
    when the result must not be cached: a lazy value, or a frame of a type no reader returns.
    """
    library = _frame_kind(value)
    if library is None:
        return None
    if library == "duckdb" or type(value).__name__ == "LazyFrame":
        return False
    return library


def artifact_size(path: str) -> int | None:
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


def _cacheable(value: Any) -> bool:
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


def _isolated_copy(value: Any) -> Any:
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
    ) -> None:
        # key -> (value, serialized size in bytes)
        self._entries: OrderedDict[tuple[str, str], tuple[object, int]] = OrderedDict()
        self._max = max_entries
        self._max_total_bytes = max_total_bytes
        self._total_bytes = 0

    @staticmethod
    def _key(path: str, frame_type: str | None) -> tuple[str, str]:
        return (path, frame_type or "pandas")

    def _drop(self, key: tuple[str, str]) -> None:
        """Remove the entry for `key`, if there is one, and release its bytes."""
        entry = self._entries.pop(key, None)
        if entry is not None:
            self._total_bytes -= entry[1]

    def get(self, path: str, frame_type: str | None = None) -> Any:
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

    def put(self, path: str, value: Any, frame_type: str | None = None, *, size_bytes: int) -> bool:
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
        self,
        path: str,
        value: Any,
        frame_type: str | None = None,
        size_bytes: int | None = None,
    ) -> bool:
        """Cache the artifact at `path` if its serialized size allows; return whether it did.

        `size_bytes` is the size when the caller already knows it; otherwise the store is
        asked. An artifact whose size is unknown or over `_LRU_MAX_ARTIFACT_BYTES` is not
        cached, and an entry already held for it is dropped.
        """
        if size_bytes is None:
            size_bytes = artifact_size(path)
        if size_bytes is None or size_bytes > _LRU_MAX_ARTIFACT_BYTES:
            self._drop(self._key(path, frame_type))
            return False
        return self.put(path, value, frame_type, size_bytes=size_bytes)
