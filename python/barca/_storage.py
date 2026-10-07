"""Storage backends for Barca artifacts — local filesystem plus remote object stores.

Remote paths are URIs dispatched by scheme to fsspec filesystems:

  abfs:// abfss://   Azure ADLS Gen2 (adlfs)      pip install 'barca[azure]'
  s3:// s3a://       Amazon S3 (s3fs)             pip install 'barca[s3]'
  gs:// gcs://       Google Cloud Storage (gcsfs) pip install 'barca[gcs]'
  memory://          fsspec in-memory fs (tests only)

Local paths (no scheme, or file://) use the stdlib only — fsspec is never
imported unless a remote URI is actually used.

Credentials are never passed explicitly: adlfs falls through to
DefaultAzureCredential, s3fs to the boto env/instance chain, gcsfs to
google.auth defaults. The BARCA_STORAGE_OPTIONS env var (a JSON object keyed
by fsspec protocol, e.g. '{"abfs": {"account_name": "myacct"}}') is splatted
into the filesystem constructor as an escape hatch.
"""

import datetime
import json
import os
import shutil
import threading
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

# scheme -> (fsspec protocol, pip package, barca extra)
_SCHEMES: dict[str, tuple[str, str, str | None]] = {
    "abfs": ("abfs", "adlfs", "azure"),
    "abfss": ("abfs", "adlfs", "azure"),
    "s3": ("s3", "s3fs", "s3"),
    "s3a": ("s3", "s3fs", "s3"),
    "gs": ("gcs", "gcsfs", "gcs"),
    "gcs": ("gcs", "gcsfs", "gcs"),
    "memory": ("memory", "fsspec", None),
}

_fs_cache: dict[str, Any] = {}
# Filesystems are built from worker threads (fan-in reads, the transfer
# helper's pool); construct each protocol's instance exactly once.
_fs_lock = threading.Lock()


def _scheme(path: "str | Path") -> str | None:
    """Return the URI scheme of path, or None for plain local paths."""
    s = str(path)
    if "://" not in s:
        return None
    return s.split("://", 1)[0].lower()


def is_remote(path: "str | Path") -> bool:
    """True iff path is a URI handled by a remote backend (not local/file://)."""
    scheme = _scheme(path)
    return scheme is not None and scheme != "file"


def local_path_of(uri: "str | Path") -> "Path | None":
    """Path for file:// URIs and plain local paths; None for remote URIs."""
    s = str(uri)
    if s.startswith("file://"):
        return Path(s[len("file://") :])
    if "://" not in s:
        return Path(s)
    return None


def storage_options(protocol: str) -> dict:
    """Per-protocol fsspec options from the BARCA_STORAGE_OPTIONS env var."""
    raw = os.environ.get("BARCA_STORAGE_OPTIONS")
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
    except ValueError as exc:
        raise ValueError(f"BARCA_STORAGE_OPTIONS is not valid JSON: {exc}") from exc
    if not isinstance(parsed, dict):
        raise ValueError("BARCA_STORAGE_OPTIONS must be a JSON object keyed by protocol")
    opts = parsed.get(protocol, {})
    if not isinstance(opts, dict):
        raise ValueError(f"BARCA_STORAGE_OPTIONS[{protocol!r}] must be a JSON object")
    return opts


def get_fs(path: "str | Path"):
    """Return the fsspec filesystem for a remote URI (cached per protocol)."""
    scheme = _scheme(path)
    if scheme is None or scheme == "file":
        raise ValueError(f"get_fs called with a local path: {path}")
    entry = _SCHEMES.get(scheme)
    if entry is None:
        supported = ", ".join(sorted({f"{s}://" for s in _SCHEMES}))
        raise ValueError(
            f"Unsupported storage scheme '{scheme}://' in {path} (supported: {supported})"
        )
    protocol, package, extra = entry

    fs = _fs_cache.get(protocol)
    if fs is not None:
        return fs

    with _fs_lock:
        if protocol in _fs_cache:
            return _fs_cache[protocol]

        try:
            import fsspec
        except ImportError as exc:
            hint = f"pip install 'barca[{extra}]'" if extra else "pip install fsspec"
            raise ImportError(f"{scheme}:// paths require fsspec ({hint})") from exc

        try:
            fs = fsspec.filesystem(protocol, **storage_options(protocol))
        except ImportError as exc:
            hint = f"pip install 'barca[{extra}]'" if extra else f"pip install {package}"
            raise ImportError(
                f"{scheme}:// paths require the '{package}' package ({hint})"
            ) from exc

        _fs_cache[protocol] = fs
        return fs


def _copy_local(src: "str | Path", dst: "str | Path") -> None:
    """Copy into a local store path, creating parents. Atomic at the destination."""
    dst = Path(dst)
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.with_name(f".{dst.name}.{os.getpid()}.{threading.get_ident()}.tmp")
    try:
        shutil.copyfile(src, tmp)
        os.replace(tmp, dst)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def put_file(local_path: "str | Path", dest: str) -> None:
    """Upload a local file to the store at dest (chunked from disk, never in memory).

    dest may be a remote URI or a plain-path / file:// store root (a shared
    local or network directory), which is a stdlib copy.
    """
    local_dest = local_path_of(dest)
    if local_dest is not None:
        _copy_local(local_path, local_dest)
        return
    fs = get_fs(dest)
    parent = dest.rsplit("/", 1)[0]
    if "://" not in parent:
        parent = dest  # degenerate URI with no path segment; let put_file fail clearly
    else:
        try:
            fs.makedirs(parent, exist_ok=True)
        except Exception:
            # Object stores have no real directories; makedirs is best-effort.
            pass
    fs.put_file(str(local_path), dest)


def get_file(src: str, local_path: "str | Path") -> None:
    """Download src from the store to a local file (chunked to disk).

    src may be a remote URI or a plain-path / file:// store path.
    """
    local_src = local_path_of(src)
    if local_src is not None:
        shutil.copyfile(local_src, local_path)
        return
    get_fs(src).get_file(src, str(local_path))


def check_store(root: str) -> None:
    """Raise unless the store holding `root` is positively there and can be listed.

    For a directory store that is the root directory itself. For an object store it is the
    bucket or container: a listing of it has to succeed (an empty one is fine). A bucket that
    was deleted, a misspelled name, an endpoint that answers 404 to everything and a store
    that cannot be reached all raise here, which is what tells them apart from one object
    being absent from a store that is otherwise fine.

    Nothing is created: this never makes a bucket, a container or a directory.
    """
    local = local_path_of(root)
    if local is not None:
        with os.scandir(local) as entries:  # raises unless it is a readable directory
            next(entries, None)
        return
    fs = get_fs(root)
    container = fs._strip_protocol(root).strip("/").split("/", 1)[0]
    if not container:
        raise ValueError(f"no bucket or container in {root}")
    fs.invalidate_cache()
    # Both have to hold: the store says the bucket exists, and it can be listed. A listing
    # alone is not proof (some servers answer an unknown bucket with an empty listing).
    if not fs.exists(container):
        raise FileNotFoundError(f"bucket or container {container!r} does not exist")
    fs.ls(container, detail=False)


def exists(path: "str | Path") -> bool:
    """Existence check that works for local paths and remote URIs."""
    if is_remote(path):
        return get_fs(path).exists(str(path))
    return os.path.exists(str(path))


def size(path: "str | Path") -> int:
    """Size in bytes for a local path or remote URI."""
    if is_remote(path):
        return int(get_fs(path).size(str(path)))
    return os.stat(str(path)).st_size


def join(base: "str | Path", name: str) -> "str | Path":
    """Join a filename onto a base dir or URI prefix without mangling the URI."""
    if is_remote(base) or _scheme(base) == "file":
        return str(base).rstrip("/") + "/" + name
    return Path(base) / name


# ─── In-place reads for lazy readers ──────────────────────────────────────────
#
# A duckdb relation or polars LazyFrame input reads only what the step's query touches, so a
# remote artifact is read in place rather than downloaded (see _artifacts.deserialize). Two
# details make that cheap and safe:
#
# - Exact range reads. fsspec's buffered files read ahead by a block (adlfs: 50 MB), which turns
#   a parquet reader's many small column-chunk reads into far more bytes than the whole file.
#   The view below opens every file with cache_type="none": each read fetches exactly its range.
# - A private scheme. duckdb routes a URL to a registered fsspec filesystem ahead of its own
#   httpfs/azure extensions, so registering the store under `abfss://` would take over the URLs
#   in the user's own SQL. The view is registered as `barca<protocol>://` instead, and only
#   barca's artifact URIs are rewritten to it.

_RANGE_SCHEME_PREFIX = "barca"
# duckdb caches what it reads per (path, mtime). A refresh can rewrite an artifact path, so the
# store's real mtime is passed through; a store that has none gets this constant.
_UNKNOWN_MTIME = datetime.datetime(2000, 1, 1, tzinfo=datetime.timezone.utc)
_range_fs_cache: dict[str, Any] = {}
_range_fs_lock = threading.Lock()


def range_read_uri(path: str) -> str:
    """``path`` (a remote artifact URI) under the private scheme of its range-read view."""
    scheme, rest = str(path).split("://", 1)
    return f"{_RANGE_SCHEME_PREFIX}{_SCHEMES[scheme.lower()][0]}://{rest}"


def range_read_fs(path: str):
    """A read-only fsspec view of the store holding ``path`` that reads exact byte ranges.

    It wraps the store's own filesystem (``get_fs``), so credentials and options are the same.
    Paths may be private-scheme URIs (``range_read_uri``, what duckdb passes) or the store
    filesystem's own stripped paths (what pyarrow passes).
    """
    inner = get_fs(path)  # validates the scheme
    protocol = _SCHEMES[str(_scheme(path))][0]
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
            return inner.open(self._real(path), "rb", cache_type="none")

    RangeReadFileSystem.protocol = (private,)
    return RangeReadFileSystem()


def suffix(path: "str | Path") -> str:
    """File extension of the last path segment (URI-safe — never pathlib on URIs)."""
    s = str(path)
    if "://" in s:
        s = urlsplit(s).path
    name = s.rstrip("/").rsplit("/", 1)[-1]
    dot = name.rfind(".")
    return name[dot:] if dot > 0 else ""
