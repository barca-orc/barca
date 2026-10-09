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
    fs = get_fs(root)
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
        if isinstance(exc, PermissionError) or status in (401, 403):
            raise PermissionError(
                f"listing {container!r} is not permitted ({type(exc).__name__}: {exc}). "
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
