"""In-place reads of remote parquet artifacts for lazy readers, with a local range cache.

A duckdb relation or polars LazyFrame input reads only what the step's query touches, so a
remote artifact is read in place rather than downloaded (see `_artifacts.deserialize`). Two
details make that cheap and safe:

- Exact range reads. fsspec's buffered files read ahead by a block (adlfs: 50 MB), which turns
  a parquet reader's many small column-chunk reads into far more bytes than the whole file.
  The view below opens every file with cache_type="none": each read fetches exactly its range.
- A private scheme. duckdb routes a URL to a registered fsspec filesystem ahead of its own
  httpfs/azure extensions, so registering the store under `abfss://` would take over the URLs
  in the user's own SQL. The view is registered as `barca<protocol>://` instead, and only
  barca's artifact URIs are rewritten to it.
- A local cache of the ranges read (#577). Every range fetched is kept under
  `.barca/range-cache/<run hash>-<version>/`, one file per range, and a later read of the same
  object on this machine is served from there, fetching only what no earlier read fetched.
  The directory is keyed by the artifact's run hash and a digest of the object's version as the
  store reports it (ETag, generation, size, modified time), so a rewritten object gets a new
  directory and a cached range is never stale. The cache size limit counts the directories
  and evicts them as leftovers (`barca docs cache`, "Disk use").
"""

from __future__ import annotations

import datetime
import json
import os
import re
import tempfile
import threading
from contextlib import contextmanager
from typing import Any

from barca import _storage

_RANGE_SCHEME_PREFIX = "barca"
# Where fetched ranges are kept, relative to the project root (the worker's working directory).
RANGE_CACHE_DIR = os.path.join(".barca", "range-cache")
# What identifies one version of an object, across s3fs, gcsfs, adlfs and memory://.
VERSION_KEYS = (
    "size",
    "ETag",
    "etag",
    "generation",
    "LastModified",
    "last_modified",
    "updated",
    "created",
)
# duckdb caches what it reads per (path, mtime). A refresh can rewrite an artifact path, so the
# store's real mtime is passed through; a store that has none gets this constant.
_UNKNOWN_MTIME = datetime.datetime(2000, 1, 1, tzinfo=datetime.timezone.utc)
_range_fs_cache: dict[str, Any] = {}
_range_fs_lock = threading.Lock()


def range_read_uri(path: str) -> str:
    """``path`` (a remote artifact URI) under the private scheme of its range-read view."""
    scheme, rest = str(path).split("://", 1)
    return f"{_RANGE_SCHEME_PREFIX}{_storage._SCHEMES[scheme.lower()][0]}://{rest}"


def range_read_fs(path: str):
    """A read-only fsspec view of the store holding ``path`` that reads exact byte ranges.

    It wraps the store's own filesystem (``get_fs``), so credentials and options are the same.
    Paths may be private-scheme URIs (``range_read_uri``, what duckdb passes) or the store
    filesystem's own stripped paths (what pyarrow passes).
    """
    inner = _storage.get_fs(path)  # validates the scheme
    protocol = _storage._SCHEMES[str(_storage._scheme(path))][0]
    with _range_fs_lock:
        fs = _range_fs_cache.get(protocol)
        if fs is None or fs.inner is not inner:
            fs = _make_range_fs(inner, protocol)
            _range_fs_cache[protocol] = fs
        return fs


def _make_range_fs(inner, protocol: str):
    from fsspec.spec import AbstractFileSystem

    private = f"{_RANGE_SCHEME_PREFIX}{protocol}"

    class RangeReadFileSystem(AbstractFileSystem):
        cachable = False

        def __init__(self):
            super().__init__()
            self.inner = inner

        @classmethod
        def _strip_protocol(cls, path):
            return str(path)  # _real() translates; the inner fs strips its own scheme

        def _real(self, path) -> str:
            path = str(path)
            if path.startswith(f"{private}://"):
                return inner._strip_protocol(f"{protocol}://{path[len(private) + 3 :]}")
            return path

        def info(self, path, **kwargs):
            return inner.info(self._real(path), **kwargs)

        def ls(self, path, detail=True, **kwargs):
            return inner.ls(self._real(path), detail=detail, **kwargs)

        def modified(self, path):
            try:
                return inner.modified(self._real(path))
            except NotImplementedError:
                return _UNKNOWN_MTIME

        def _open(
            self, path, mode="rb", block_size=None, autocommit=True, cache_options=None, **kwargs
        ):
            if mode != "rb":
                raise PermissionError(f"{private}:// is read-only")
            real = self._real(path)
            cached = _cached_range_file(self, inner, real)
            if cached is not None:
                return cached
            return inner.open(real, "rb", cache_type="none")

    RangeReadFileSystem.protocol = (private,)
    return RangeReadFileSystem()


def object_version(info: dict) -> str:
    """What identifies this version of an object, from the store's ``info()``: a JSON object of
    the version keys the store reports (``{}`` when it reports none)."""
    seen = {k: str(info[k]) for k in VERSION_KEYS if info.get(k) is not None}
    return json.dumps(seen, sort_keys=True)


def range_cache_key(real: str, info: dict) -> "str | None":
    """The cache directory name for an artifact object: its run hash (the file name without
    the extension) and a digest of its version. ``None`` when the store reports nothing that
    identifies the version beyond its size, so nothing about it can be cached safely."""
    import hashlib

    version = object_version(info)
    if set(json.loads(version)) <= {"size"}:
        return None
    name = str(real).rstrip("/").rsplit("/", 1)[-1]
    run_hash = name.split(".", 1)[0]
    if not run_hash or not re.fullmatch(r"[A-Za-z0-9_-]+", run_hash):
        return None
    digest = hashlib.sha256(f"{real}\n{version}".encode()).hexdigest()[:16]
    return f"{run_hash}-{digest}"


def _cached_range_file(fs, inner, real: str):
    """A file over ``real`` whose ranges are kept in the range cache, or ``None`` when its
    version cannot be identified or the cache cannot be written."""
    try:
        info = inner.info(real)
        key = range_cache_key(real, info)
        if key is None or info.get("size") is None:
            return None
        directory = os.path.join(RANGE_CACHE_DIR, key)
        os.makedirs(directory, exist_ok=True)
        os.utime(directory)  # the cache size limit evicts the least recently used first
    except OSError:
        return None
    return _CachedRangeFile(fs, inner, real, int(info["size"]), directory)


def _range_file_name(start: int, end: int) -> str:
    return f"{start:016x}-{end:016x}"


def _cached_ranges(directory: str) -> list[tuple[int, int, str]]:
    """The ranges kept in ``directory``: (start, end, file), end exclusive."""
    out = []
    try:
        names = os.listdir(directory)
    except OSError:
        return out
    for name in names:
        start, sep, end = name.partition("-")
        if not sep or len(start) != 16 or len(end) != 16:
            continue  # a temporary file being written
        try:
            a, b = int(start, 16), int(end, 16)
        except ValueError:
            continue
        if a < b:
            out.append((a, b, os.path.join(directory, name)))
    return out


def _make_cached_range_file_class():
    from fsspec.spec import AbstractBufferedFile

    class CachedRangeFile(AbstractBufferedFile):
        """A read-only file over a store object that reads exact byte ranges, keeping each
        fetched range in its range-cache directory and serving later reads from there."""

        def __init__(self, fs, inner, real, size, directory):
            self._inner = inner
            self._real_path = real
            self._directory = directory
            self._ranges = _cached_ranges(directory)
            super().__init__(fs, real, mode="rb", cache_type="none", size=size)

        def _fetch_range(self, start, end):
            end = min(end, self.size)
            if start >= end:
                return b""
            out = bytearray()
            pos = start
            while pos < end:
                covering = [r for r in self._ranges if r[0] <= pos < r[1]]
                if covering:
                    a, b, path = max(covering, key=lambda r: r[1])
                    stop = min(b, end)
                    data = _read_kept(path, pos - a, stop - pos)
                    if data is not None:
                        out += data
                        pos = stop
                        continue
                    # Deleted under us (evicted): fetch it again.
                    self._ranges = [r for r in self._ranges if r[2] != path]
                    continue
                later = [r[0] for r in self._ranges if r[0] > pos]
                stop = min([end, *later])
                data = self._inner.cat_file(self._real_path, start=pos, end=stop)
                if len(data) != stop - pos:
                    raise OSError(
                        f"short read from {self._real_path}: asked for bytes {pos}-{stop}, "
                        f"got {len(data)}"
                    )
                kept = self._keep(pos, stop, data)
                if kept is not None:
                    self._ranges.append(kept)
                out += data
                pos = stop
            return bytes(out)

        def _keep(self, start, end, data):
            path = os.path.join(self._directory, _range_file_name(start, end))
            try:
                fd, tmp = tempfile.mkstemp(dir=self._directory, prefix=".tmp-")
                try:
                    with os.fdopen(fd, "wb") as f:
                        f.write(data)
                    os.replace(tmp, path)
                except BaseException:
                    with _suppress_oserror():
                        os.unlink(tmp)
                    raise
            except OSError:
                return None  # the cache is best effort; the bytes were read
            return (start, end, path)

    return CachedRangeFile


@contextmanager
def _suppress_oserror():
    try:
        yield
    except OSError:
        pass


def _read_kept(path: str, offset: int, length: int) -> "bytes | None":
    try:
        with open(path, "rb") as f:
            f.seek(offset)
            data = f.read(length)
    except OSError:
        return None
    return data if len(data) == length else None


_cached_range_file_class: Any = None


def _CachedRangeFile(fs, inner, real, size, directory):
    global _cached_range_file_class
    if _cached_range_file_class is None:
        _cached_range_file_class = _make_cached_range_file_class()
    return _cached_range_file_class(fs, inner, real, size, directory)
