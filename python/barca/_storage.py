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


def suffix(path: "str | Path") -> str:
    """File extension of the last path segment (URI-safe — never pathlib on URIs)."""
    s = str(path)
    if "://" in s:
        s = urlsplit(s).path
    name = s.rstrip("/").rsplit("/", 1)[-1]
    dot = name.rfind(".")
    return name[dot:] if dot > 0 else ""
