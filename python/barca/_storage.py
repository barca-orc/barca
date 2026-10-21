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
import errno
import json
import math
import os
import re
import shutil
import signal
import sys
import tempfile
import threading
from contextlib import contextmanager
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


def safe_error(message: str) -> str:
    """Remove URI passwords/signed queries and configured credentials from diagnostics."""

    def uri(match):
        value = match.group().split("?", 1)[0].split("#", 1)[0]
        return re.sub(r"(://)[^/@\s]*:[^/@\s]*@", r"\1<redacted>@", value)

    message = re.sub(r"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s'\"()]+", uri, message)
    try:
        options = json.loads(os.environ.get("BARCA_STORAGE_OPTIONS", "{}"))
    except ValueError:
        options = {}

    def redact(values):
        nonlocal message
        if not isinstance(values, dict):
            return
        for key, value in values.items():
            if isinstance(value, dict):
                redact(value)
            elif (
                isinstance(value, str)
                and value
                and any(
                    word in key.lower()
                    for word in (
                        "secret",
                        "password",
                        "token",
                        "key",
                        "credential",
                        "connection_string",
                    )
                )
            ):
                message = message.replace(value, "<redacted>")

    redact(options)
    for name in (
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_STORAGE_ACCOUNT_KEY",
        "AZURE_CLIENT_SECRET",
        "AZURE_STORAGE_SAS_TOKEN",
    ):
        value = os.environ.get(name)
        if value:
            message = message.replace(value, "<redacted>")
    return message


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


# ─── Putting a file in place ──────────────────────────────────────────────────

# Temp files that are being written now. Reentrant: `discard_staged` runs in a signal
# handler, which may interrupt the very thread that holds the lock.
_staged: set[str] = set()
_staged_lock = threading.RLock()
# Set by `discard_staged`: the process is on its way out and stages nothing more.
_leaving = False


@contextmanager
def _defer_staging_sigterm(cleanup_owned):
    """Delay main-thread termination only until a new stage has an owner."""
    pending = []
    if threading.current_thread() is not threading.main_thread():
        yield
        return
    original = signal.getsignal(signal.SIGTERM)
    if original == signal.SIG_IGN or original is None:
        yield
        return

    def defer(signum, frame):
        if not pending:
            pending.append((signum, frame))

    signal.signal(signal.SIGTERM, defer)
    try:
        yield
    finally:
        default_cleaned = False
        try:
            if pending and original == signal.SIG_DFL:
                # Default termination cannot unwind a Python finalizer.
                cleanup_owned()
                default_cleaned = True
        finally:
            signal.signal(signal.SIGTERM, original)
            # A signal can arrive at restoration entry, after the check above.
            if pending and original == signal.SIG_DFL:
                try:
                    if not default_cleaned:
                        cleanup_owned()
                finally:
                    os.kill(os.getpid(), pending[0][0])
            elif pending and callable(original):
                original(*pending[0])


@contextmanager
def staged_beside(dest: Path):
    """Yield a new temp file in ``dest``'s directory, to be written and renamed over ``dest``.

    The same directory means the same filesystem, so the rename is atomic and ``dest`` is
    never seen half written. The temp file is removed when the block ends without having
    renamed it, and by ``discard_staged`` when the process is told to stop in the middle.

    Creation and registration share the lock used by ``discard_staged``. Main-thread
    SIGTERM is deferred through registration and fd close, then replayed with the original
    handler inside the cleanup scope. A lifeline thread waits for the same lock.
    """
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = None

    def remove_owned():
        if tmp is not None:
            with _staged_lock:
                # Keep ownership visible to reentrant signal cleanup until removal succeeds.
                Path(tmp).unlink(missing_ok=True)
                _staged.discard(tmp)

    def cleanup_owned():
        with _defer_staging_sigterm(remove_owned):
            remove_owned()

    try:
        with _defer_staging_sigterm(cleanup_owned), _staged_lock:
            if _leaving:
                raise InterruptedError(f"not staging {dest.name}: the process is exiting")
            fd, tmp = tempfile.mkstemp(dir=dest.parent, prefix=f".{dest.name}.", suffix=".tmp")
            _staged.add(tmp)
            os.close(fd)
        yield Path(tmp)
    finally:
        cleanup_owned()


def discard_staged() -> None:
    """Remove every temp file ``staged_beside`` has open, and let it open no more. For a
    process about to exit."""
    global _leaving
    with _staged_lock:
        _leaving = True
        paths = list(_staged)
    for tmp in paths:
        try:
            os.unlink(tmp)
        except OSError:
            pass


# What a directory found at an artifact's path is renamed to (`-2`, `-3`, ... when taken).
MOVED_ASIDE_SUFFIX = ".moved-aside"


class ArtifactPathError(OSError):
    """A directory sits where an artifact file belongs and could not be moved out of the way.

    Not an error of the step that was writing its result: barca's own artifact directory is
    in a state barca cannot repair (the coordinator reports it as an infrastructure failure,
    exit 3). The message names the path, the reason and what to do.
    """


def make_way(dest: "str | Path") -> "Path | None":
    """Clear a directory that sits where barca is about to put the artifact file ``dest``.

    Call this only for paths inside barca's own artifact directory, never for a path the
    user chose (a ``@sink``). An artifact is always one file, so a directory at its path is
    something barca did not make and cannot read. Nothing in it is ever deleted:

    - an empty directory is removed (there is nothing to lose);
    - any other directory is renamed to a sibling, ``<name>.moved-aside`` (``-2``, ``-3``,
      ... if that exists), with everything in it, and a warning on stderr names both paths.

    Only a directory is ever moved. Another process may be installing the same artifact at
    this moment (two runs, or the transfer helper and a worker), so what is at ``dest`` can
    change between looking and acting: both operations used here refuse anything that is not
    a directory at the moment they act (``rmdir``, and a rename of ``dest/``, which the
    kernel resolves only if it is a directory). A regular file, the artifact another process
    just put there, is never renamed.

    A symlink is left alone, whatever it points to: the rename that installs the artifact
    replaces the link itself and never reaches its target. (It is told apart before acting;
    barca's own processes never create one, so only a third party making a symlink there in
    that instant could have it followed.) Returns where a directory was
    moved to, or None. Raises ArtifactPathError when a directory is there and cannot be moved.
    """
    dest = Path(dest)
    if dest.is_symlink() or not dest.is_dir():
        return None
    try:
        dest.rmdir()
        return None
    except (FileNotFoundError, NotADirectoryError):
        return None  # another process cleared it, or put the artifact there, first
    except OSError as exc:
        if exc.errno not in (errno.ENOTEMPTY, errno.EEXIST):
            raise _blocked(dest, exc) from exc
    for n in range(1, 1000):
        aside = dest.with_name(dest.name + MOVED_ASIDE_SUFFIX + ("" if n == 1 else f"-{n}"))
        if os.path.lexists(aside):
            continue
        try:
            # The trailing separator makes this a rename of a directory or nothing.
            os.rename(f"{dest}{os.sep}", aside)
        except (FileNotFoundError, NotADirectoryError):
            return None  # another process moved it, or put the artifact there, first
        except OSError as exc:
            raise _blocked(dest, exc) from exc
        print(
            f"[barca] warning: {dest} is a directory, not an artifact. Moved it, with its "
            f"contents, to {aside}; barca does not use it, delete it if you do not need it.",
            file=sys.stderr,
            flush=True,
        )
        return aside
    raise _blocked(dest, FileExistsError("every .moved-aside name beside it is taken"))


def _blocked(dest: Path, exc: OSError) -> ArtifactPathError:
    return ArtifactPathError(
        f"a directory sits where the artifact {dest} belongs and could not be moved aside "
        f"({type(exc).__name__}: {exc}).\n"
        f"Barca renames such a directory to {dest.name}{MOVED_ASIDE_SUFFIX} and needs write "
        f"permission on {dest.parent} for that. Grant it, or move or remove the directory "
        "yourself, then run the command again. Nothing was deleted."
    )


def sha256_file(path: "str | Path") -> str:
    """The SHA-256 of a local file, hex: what the transfer and state helpers compare copies by."""
    import hashlib

    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _copy_local(src: "str | Path", dst: "str | Path") -> None:
    """Copy into a local store path, creating parents. Atomic at the destination."""
    dst = Path(dst)
    with staged_beside(dst) as tmp:
        shutil.copyfile(src, tmp)
        os.replace(tmp, dst)


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


def describe_error(uri: str, exc: BaseException) -> str:
    """Provider error with configured identity/recovery when authentication failed."""
    from barca import _identity

    message = f"{type(exc).__name__}: {exc}"
    hint = _identity.error_hint(uri, exc)
    return safe_error(f"{hint}\n{message}" if hint else message)


def check_store(root: str) -> None:
    """Raise unless the store holding `root` is positively there and can be listed.

    For a directory store that is the root directory itself. For an object store it is the
    bucket or container: a listing of it has to succeed (an empty one is fine). A bucket that
    was deleted, a misspelled name, an endpoint that answers 404 to everything and a store
    that cannot be reached all raise here, which is what tells them apart from one object
    being absent from a store that is otherwise fine.

    The error says which it was: FileNotFoundError when the store answers that the bucket is
    not there, PermissionError (naming the permission to grant) when it refuses the listing.

    Nothing is created: this never makes a bucket, a container or a directory.
    """
    local = local_path_of(root)
    if local is not None:
        with os.scandir(local) as entries:  # raises unless it is a readable directory
            next(entries, None)
        return
    try:
        fs = get_fs(root)
    except Exception as exc:
        from barca import _identity

        hint = _identity.error_hint(root, exc)
        if hint:
            raise PermissionError(f"{hint}\n{type(exc).__name__}: {exc}") from exc
        raise
    container = fs._strip_protocol(root).strip("/").split("/", 1)[0]
    if not container:
        raise ValueError(f"no bucket or container in {root}")
    fs.invalidate_cache()
    # The listing comes first: it is what tells "not permitted" from "not there". An
    # existence check answers False for both.
    try:
        fs.ls(container, detail=False)
    except Exception as exc:
        status = http_status(exc)
        from barca import _identity

        hint = _identity.error_hint(root, exc, fs)
        if hint:
            raise PermissionError(
                f"{hint}\nlisting {container!r} is not permitted ({type(exc).__name__}: {exc}). "
                f"These credentials may read and write objects but cannot list the "
                f"{_container_word(root)}; barca needs {list_permission(root)} to tell a "
                f"missing artifact from a missing store"
            ) from exc
        if isinstance(exc, FileNotFoundError) or status == 404:
            raise FileNotFoundError(
                f"{_container_word(root)} {container!r} was not found ({type(exc).__name__}: {exc})"
            ) from exc
        raise
    # A listing alone is not proof: some servers answer an unknown bucket with an empty one.
    if not fs.exists(container):
        raise FileNotFoundError(f"{_container_word(root)} {container!r} was not found")


def _container_word(root: str) -> str:
    return "container" if _scheme(root) in ("abfs", "abfss", "az") else "bucket"


def list_permission(root: str) -> str:
    """The permission that lets these credentials list the store holding `root`."""
    scheme = _scheme(root)
    if scheme in ("s3", "s3a"):
        return "s3:ListBucket on the bucket"
    if scheme in ("gs", "gcs"):
        return "storage.objects.list (in the Storage Object User role)"
    if scheme in ("abfs", "abfss", "az"):
        return "the Storage Blob Data Reader or Contributor role (list blobs)"
    return "permission to list it"


def http_status(exc: BaseException) -> int | None:
    """The HTTP status a cloud SDK attached to its error, if any.

    azure.core's HttpResponseError carries `status_code`; gcsfs and
    google.api_core errors carry `code`; requests-style errors carry
    `response.status_code`.
    """
    for value in (
        getattr(exc, "status_code", None),
        getattr(exc, "code", None),
        getattr(getattr(exc, "response", None), "status_code", None),
    ):
        if value is None:
            continue
        try:
            status = int(value)
        except (TypeError, ValueError):
            continue
        if 100 <= status <= 599:
            return status
    return None


def first_line(exc: BaseException) -> str:
    """The first line of an exception's message, or "" when it has none: storage errors
    (botocore, azure-core) often carry the whole request on the following lines."""
    lines = str(exc).strip().splitlines()
    return lines[0] if lines else ""


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
# - A local cache of the ranges read (#577). Every range fetched is kept under
#   `.barca/range-cache/<run hash>-<version>/`, one file per range, and a later read of the same
#   object on this machine is served from there, fetching only what no earlier read fetched.
#   The directory is keyed by the artifact's run hash and a digest of the object's version as the
#   store reports it (ETag, generation, size, modified time), so a rewritten object gets a new
#   directory and a cached range is never stale. The cache size limit counts the directories
#   and evicts them as leftovers (`barca docs cache`, "Disk use").

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


def suffix(path: "str | Path") -> str:
    """File extension of the last path segment (URI-safe — never pathlib on URIs)."""
    s = str(path)
    if "://" in s:
        s = urlsplit(s).path
    name = s.rstrip("/").rsplit("/", 1)[-1]
    dot = name.rfind(".")
    return name[dot:] if dot > 0 else ""


# ─── Signed URLs ──────────────────────────────────────────────────────────────
#
# A signed URL is a time-limited HTTPS link that anyone holding it can GET without
# credentials. fsspec's own `sign()` covers only part of what barca's default credentials are:
# adlfs signs only with an account key (barca authenticates with DefaultAzureCredential) and
# gcsfs only with a service-account key file. So each cloud is signed here, with the
# filesystem barca already built for the store (`get_fs`), so its credentials and options
# (BARCA_STORAGE_OPTIONS: an S3 endpoint for R2 or MinIO, an Azure connection string) are the
# same ones every other store access uses.
#
# A signed URL is a bearer credential: it is returned, never printed or logged here, and no
# error message below contains one.

# The longest a link may live: SigV4 and Azure user delegation keys allow at most 7 days.
MAX_SIGNED_URL_SECONDS = 7 * 24 * 3600
DEFAULT_SIGNED_URL_SECONDS = 3600
_SIGNABLE = ("s3", "s3a", "gs", "gcs", "abfs", "abfss")


class SignError(Exception):
    """A URL could not be signed. ``kind`` says whose problem it is:

    - ``usage``: the request is wrong (not a bucket URI, an expiry over 7 days);
    - ``missing``: the object is not in the store;
    - ``credentials``: the credentials in use cannot sign, or may not read the object;
    - ``store``: the store could not be reached or answered with an error.
    """

    def __init__(self, kind: str, message: str):
        super().__init__(message)
        self.kind = kind


def expiry_seconds(expires: Any) -> int:
    """``expires`` (seconds, or a ``datetime.timedelta``) as whole seconds, at most 7 days."""
    if isinstance(expires, datetime.timedelta):
        seconds = expires.total_seconds()
    elif isinstance(expires, (int, float)) and not isinstance(expires, bool):
        seconds = float(expires)
    else:
        raise SignError(
            "usage",
            f"expires must be seconds or a datetime.timedelta, not {type(expires).__name__}",
        )
    if not math.isfinite(seconds) or seconds < 1:
        raise SignError("usage", f"expires must be at least 1 second (got {expires!r})")
    if seconds > MAX_SIGNED_URL_SECONDS:
        raise SignError(
            "usage",
            f"expires is at most 7 days ({MAX_SIGNED_URL_SECONDS} seconds); got {int(seconds)} "
            "seconds. A signed URL cannot be revoked before it expires, so keep it short.",
        )
    return int(seconds)


def _disposition(filename: str) -> str:
    """`Content-Disposition` that makes a browser download the object as ``filename``."""
    safe = "".join(c if c.isprintable() and c not in '"\\/' else "_" for c in filename)
    return f'attachment; filename="{safe or "download"}"'


def sign_url(
    uri: str, expires: Any = DEFAULT_SIGNED_URL_SECONDS, filename: "str | None" = None
) -> "tuple[str, datetime.datetime]":
    """A read-only HTTPS URL for the object at ``uri`` and when it stops working (UTC).

    ``uri`` is an ``s3://``/``s3a://`` (also R2 and MinIO, through the endpoint in
    BARCA_STORAGE_OPTIONS), ``gs://``/``gcs://`` or ``abfs://``/``abfss://`` object. The object
    must exist. The link downloads as ``filename`` (default: the object's own name). Raises
    SignError with the URI (never a signed URL) in its message.
    """
    seconds = expiry_seconds(expires)
    scheme = _scheme(uri)
    where = safe_error(str(uri))
    if scheme not in _SIGNABLE:
        raise SignError(
            "usage",
            f"cannot sign {where}: signed URLs are for objects in S3 (s3://, s3a://), GCS (gs://, "
            "gcs://) or Azure (abfs://, abfss://); a local path or file:// has no URL to share",
        )
    name = filename or urlsplit(str(uri)).path.rstrip("/").rsplit("/", 1)[-1] or "download"
    try:
        fs = get_fs(uri)
    except ImportError as exc:
        raise SignError("usage", f"cannot sign {where}: {exc}") from exc
    except ValueError as exc:
        raise SignError("usage", f"cannot sign {where}: {safe_error(str(exc))}") from exc
    if scheme in ("s3", "s3a"):
        signer = _sign_s3
    elif scheme in ("gs", "gcs"):
        signer = _sign_gcs
    else:
        signer = _sign_azure
    try:
        return signer(fs, str(uri), seconds, _disposition(name))
    except SignError:
        raise
    except Exception as exc:  # noqa: BLE001 — every SDK error becomes one SignError
        raise SignError(
            _error_kind(exc), f"cannot sign {where}: {describe_error(uri, exc)}"
        ) from exc


def _error_kind(exc: BaseException) -> str:
    if isinstance(exc, FileNotFoundError) or http_status(exc) == 404:
        return "missing"
    if isinstance(exc, PermissionError) or http_status(exc) in (401, 403):
        return "credentials"
    name = type(exc).__name__
    if name in (
        "NoCredentialsError",
        "DefaultCredentialsError",
        "ClientAuthenticationError",
        "CredentialUnavailableError",
        "RefreshError",
        "ResourceNotFoundError",
    ):
        return "missing" if name == "ResourceNotFoundError" else "credentials"
    return "store"


def _require_object(fs, uri: str) -> None:
    """One HEAD: a typo or a missing upload must not become a link that answers 404 later."""
    try:
        info = fs.info(uri)
    except FileNotFoundError as exc:
        raise SignError("missing", f"{safe_error(uri)} is not in the store") from exc
    if info.get("type") == "directory":
        raise SignError("usage", f"{safe_error(uri)} is a directory, not an object")


def _now() -> datetime.datetime:
    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)


def _sign_s3(fs, uri: str, seconds: int, disposition: str):
    """Presigned GET with SigV4, through an s3fs client built from the store's own options (so
    the endpoint, profile and credential chain are the same)."""
    fs.invalidate_cache()
    _require_object(fs, uri)
    fs = _s3_signing_fs()
    expires_at = _now() + datetime.timedelta(seconds=seconds)
    url = fs.url(uri, expires=seconds, ResponseContentDisposition=disposition)
    # A URL signed with temporary credentials stops working when they expire.
    credential_expiry = _s3_credential_expiry(fs)
    if credential_expiry is not None and credential_expiry < expires_at:
        expires_at = credential_expiry.replace(microsecond=0)
    return url, expires_at


def _s3_signing_fs():
    """The store's s3fs options with SigV4 forced: botocore may otherwise presign with SigV2,
    which buckets created since 2020 reject and which has no 7-day limit."""
    import fsspec

    options = dict(storage_options("s3"))
    config = dict(options.pop("config_kwargs", None) or {})
    config["signature_version"] = "s3v4"
    return fsspec.filesystem("s3", config_kwargs=config, skip_instance_cache=True, **options)


def _s3_credential_expiry(fs) -> "datetime.datetime | None":
    try:
        signer = fs.s3._request_signer
        credentials = getattr(signer, "_credentials", None)
        expiry = getattr(credentials, "_expiry_time", None)
    except Exception:  # noqa: BLE001 — best effort: unknown means "as requested"
        return None
    if isinstance(expiry, datetime.datetime):
        if expiry.tzinfo is None:
            expiry = expiry.replace(tzinfo=datetime.timezone.utc)
        return expiry.astimezone(datetime.timezone.utc)
    return None


GCS_IMPERSONATE_HINT = (
    "user credentials (gcloud auth application-default login) cannot sign URLs. Sign as a "
    "service account: gcloud auth application-default login "
    "--impersonate-service-account=SA_EMAIL (you need roles/iam.serviceAccountTokenCreator on "
    "it), or point GOOGLE_APPLICATION_CREDENTIALS at a service-account key"
)


def _gcs_signing_credentials(fs):
    """The credentials gcsfs authenticates with, or Google's default ones when it has none."""
    credentials = getattr(getattr(fs, "credentials", None), "credentials", None)
    from google.auth.credentials import AnonymousCredentials

    if credentials is None or isinstance(credentials, AnonymousCredentials):
        import google.auth

        credentials, _ = google.auth.default(
            scopes=["https://www.googleapis.com/auth/cloud-platform"]
        )
    return credentials


def _sign_gcs(fs, uri: str, seconds: int, disposition: str):
    """V4 signing: locally with a key, else through IAM signBlob as the service account."""
    fs.invalidate_cache()
    _require_object(fs, uri)
    bucket, key = fs.split_path(uri)[:2]
    credentials = _gcs_signing_credentials(fs)
    expires_at = _now() + datetime.timedelta(seconds=seconds)
    endpoint = getattr(fs, "_location", None) or "https://storage.googleapis.com"
    return gcs_signed_url(credentials, bucket, key, seconds, disposition, endpoint), expires_at


def gcs_signed_url(credentials, bucket: str, key: str, seconds: int, disposition: str, endpoint):
    """A V4 signed GET for ``gs://bucket/key`` with ``credentials``."""
    from google.auth import credentials as google_credentials
    from google.cloud import storage

    # The client only supplies defaults (the universe domain); the signing credentials are
    # passed explicitly below.
    blob = storage.Client.create_anonymous_client().bucket(bucket).blob(key)
    common = {
        "version": "v4",
        "expiration": datetime.timedelta(seconds=seconds),
        "method": "GET",
        "response_disposition": disposition,
        "api_access_endpoint": endpoint,
    }
    if isinstance(credentials, google_credentials.Signing) and getattr(
        credentials, "service_account_email", None
    ):
        # A service-account key signs locally; impersonated credentials sign through IAM.
        return blob.generate_signed_url(credentials=credentials, **common)
    if type(credentials).__module__ == "google.oauth2.credentials":
        raise SignError("credentials", GCS_IMPERSONATE_HINT)
    # No key: workload identity, the GCE/GKE metadata server, an external account. Sign through
    # IAM signBlob as the credential's own service account.
    import google.auth.transport.requests

    if not getattr(credentials, "valid", False) or not getattr(credentials, "token", None):
        credentials.refresh(google.auth.transport.requests.Request())
    email = getattr(credentials, "service_account_email", None)
    if not email or email == "default":
        raise SignError(
            "credentials",
            f"the Google credentials in use ({type(credentials).__name__}) have no service "
            "account to sign as. Use a service account (workload identity, a key, or "
            "impersonation: gcloud auth application-default login "
            "--impersonate-service-account=SA_EMAIL)",
        )
    try:
        return blob.generate_signed_url(
            service_account_email=email, access_token=credentials.token, **common
        )
    except Exception as exc:  # noqa: BLE001 — google.auth TransportError and friends
        raise SignError(
            "credentials",
            f"IAM signBlob as {email} failed: it needs iam.serviceAccounts.signBlob on that "
            f"account (roles/iam.serviceAccountTokenCreator). {type(exc).__name__}: "
            f"{safe_error(first_line(exc))}",
        ) from exc


def _azure_service(fs):
    """A synchronous BlobServiceClient with adlfs's settings, and the account key if any."""
    from azure.storage.blob import BlobServiceClient

    connection_string = getattr(fs, "connection_string", None)
    if connection_string:
        service = BlobServiceClient.from_connection_string(connection_string)
        return service, getattr(service.credential, "account_key", None)
    account_url = getattr(fs, "account_url", None) or (
        f"https://{fs.account_name}.blob.core.windows.net"
    )
    if getattr(fs, "account_key", None):
        credential = {"account_name": fs.account_name, "account_key": fs.account_key}
        return BlobServiceClient(account_url, credential=credential), fs.account_key
    if getattr(fs, "sas_token", None):
        raise SignError(
            "credentials",
            "a SAS token cannot sign new links. Configure an account key, or sign in with an "
            "Entra identity (DefaultAzureCredential) that may create user delegation keys",
        )
    import azure.identity

    client_id = getattr(fs, "client_id", None)
    client_secret = getattr(fs, "client_secret", None)
    tenant_id = getattr(fs, "tenant_id", None)
    if client_id and client_secret and tenant_id:
        credential = azure.identity.ClientSecretCredential(tenant_id, client_id, client_secret)
    else:
        credential = azure.identity.DefaultAzureCredential()
    return BlobServiceClient(account_url, credential=credential), None


def _sign_azure(fs, uri: str, seconds: int, disposition: str):
    """A read-only blob SAS: a service SAS with an account key, else a user delegation SAS."""
    container, blob = fs.split_path(uri)[:2]
    service, account_key = _azure_service(fs)
    client = service.get_blob_client(container, blob)
    try:
        client.get_blob_properties()
    except Exception as exc:
        if type(exc).__name__ == "ResourceNotFoundError" or http_status(exc) == 404:
            raise SignError("missing", f"{safe_error(uri)} is not in the store") from exc
        raise
    expires_at = _now() + datetime.timedelta(seconds=seconds)
    sas = azure_blob_sas(service, container, blob, expires_at, disposition, account_key)
    return f"{client.url}?{sas}", expires_at


def azure_blob_sas(service, container, blob, expires_at, disposition, account_key=None) -> str:
    """The SAS query string: signed with the account key, or with a user delegation key that
    lives exactly as long as the link (needs generateUserDelegationKey on the account)."""
    from azure.storage.blob import BlobSasPermissions, generate_blob_sas

    common = {
        "account_name": service.account_name,
        "container_name": container,
        "blob_name": blob,
        "permission": BlobSasPermissions(read=True),
        "expiry": expires_at,
        "content_disposition": disposition,
    }
    if account_key:
        return generate_blob_sas(account_key=account_key, **common)
    try:
        key = service.get_user_delegation_key(_now(), expires_at)
    except Exception as exc:  # noqa: BLE001
        raise SignError(
            "credentials",
            "could not get a user delegation key: the identity needs "
            "Microsoft.Storage/storageAccounts/blobServices/generateUserDelegationKey/action "
            "(Storage Blob Delegator, or Storage Blob Data Contributor) on the account. "
            f"{type(exc).__name__}: {safe_error(first_line(exc))}",
        ) from exc
    return generate_blob_sas(user_delegation_key=key, **common)
