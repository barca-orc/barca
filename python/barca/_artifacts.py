"""Artifact serialization for Barca — format detection, read/write, path management.

Supports three formats:
  - json:    dicts, lists, primitives (stdlib json)
  - pickle:  arbitrary Python objects (stdlib pickle, protocol 5)
  - parquet: pandas/polars DataFrames, pyarrow Tables, duckdb relations (requires pyarrow)

Destinations may be local paths or remote URIs (abfss://, s3://, gs://, ...
— see barca._storage). Every write is staged through a local temp file and
then finalized with an atomic os.replace (local) or a chunked upload
(remote), so serialized payloads are never buffered fully in memory and a
crash mid-write never leaves a partial artifact at the destination.
"""

import hashlib
import json
import os
import pickle
import re
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from barca import _storage

_FORMAT_EXTENSIONS = {
    "json": ".json",
    "pickle": ".pkl",
    "parquet": ".parquet",
}

# Staging area for remote uploads/downloads. Deliberately on project disk
# rather than the system tempdir: /tmp is commonly tmpfs on Linux, which
# would put multi-hundred-MB payloads back in RAM.
_STAGING_DIR = ".barca/staging"


def _frame_kind(value: Any) -> str | None:
    """Classify a value as a frame-like type we can write to parquet, without imports.

    Returns "pandas", "polars", "pyarrow", "duckdb", or None. Matching is by
    type/module name so none of these libraries has to be importable here.
    """
    type_name = type(value).__name__
    module = type(value).__module__ or ""

    if type_name in ("DataFrame", "LazyFrame"):
        if module.startswith("pandas"):
            return "pandas"
        if module.startswith("polars"):
            return "polars"
    if type_name == "Table" and module.startswith("pyarrow"):
        return "pyarrow"
    # The relation class lives in the `_duckdb` extension module.
    if type_name == "DuckDBPyRelation" and module.lstrip("_").startswith("duckdb"):
        return "duckdb"
    return None


def detect_format(value: Any, explicit: str | None = None) -> str:
    """Auto-detect the best serialization format for a value.

    Priority:
      1. Explicit override (if provided)
      2. pandas/polars DataFrame, pyarrow Table, duckdb relation → parquet
      3. JSON-serializable → json
      4. Fallback → pickle
    """
    if explicit is not None:
        return explicit

    # Check for frame types without requiring the imports at module level.
    if _frame_kind(value) is not None:
        return "parquet"

    # Try JSON — must succeed without default=str to be considered safe.
    if _is_json_serializable(value):
        return "json"

    return "pickle"


def resolve_format(value: Any, fmt: str, warn: bool = True) -> str:
    """Downgrade parquet to pickle when the value has no parquet writer.

    Must be called before computing the artifact path so the extension,
    the receipt, and the bytes on disk all agree.
    """
    if fmt != "parquet":
        return fmt
    if _frame_kind(value) is not None or hasattr(value, "to_parquet"):
        return fmt

    if warn:
        import sys

        print(
            f"[barca] warning: parquet format requested but value is "
            f"{type(value).__name__}, falling back to pickle",
            file=sys.stderr,
        )
    return "pickle"


def _is_json_serializable(value: Any) -> bool:
    """Check if a value can be losslessly serialized as JSON."""
    try:
        json.dumps(value)
        return True
    except (TypeError, ValueError, OverflowError):
        return False


def _staging_dir() -> Path:
    """This process's staging directory, ``.barca/staging/{pid}/``.

    One directory per process: workers start and stop throughout a run, and a shared
    directory would let one worker's cleanup remove a file another is still using.
    """
    d = Path(_STAGING_DIR) / str(os.getpid())
    d.mkdir(parents=True, exist_ok=True)
    return d


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True  # exists, owned by someone else
    return True


# Loose temp files directly in .barca/staging/ come from versions that shared one directory.
# They have no owner to check, so only ones old enough to be abandoned are removed.
_LEGACY_TEMP_MAX_AGE_SECONDS = 3600


def clean_staging() -> None:
    """Best-effort removal of temp files left by workers that are no longer running.

    Removes the staging directories of dead processes and never touches a live one's: its
    owner may be in the middle of an upload or download.
    """
    root = Path(_STAGING_DIR)
    if not root.is_dir():
        return
    now = time.time()
    for entry in root.iterdir():
        try:
            if entry.is_dir():
                if not entry.name.isdigit() or _pid_alive(int(entry.name)):
                    continue
                for tmp in entry.iterdir():
                    tmp.unlink()
                entry.rmdir()
            elif entry.suffix == ".tmp":
                if now - entry.stat().st_mtime > _LEGACY_TEMP_MAX_AGE_SECONDS:
                    entry.unlink()
        except OSError:
            pass


# Frame types whose value reads its parquet file when queried, not when it is loaded.
LAZY_FRAME_TYPES = frozenset({"duckdb", "polars_lazy"})

# Fetched files a returned value still reads from (see deserialize). Appended from the
# collect() thread pool; list.append is atomic.
_held_fetches: list[Path] = []


def release_fetched() -> None:
    """Remove the fetched files kept for lazy readers. Call once their values are done with."""
    while _held_fetches:
        _held_fetches.pop().unlink(missing_ok=True)


def _make_temp(directory: Path, prefix: str = "stage-") -> Path:
    fd, name = tempfile.mkstemp(dir=directory, prefix=prefix, suffix=".tmp")
    os.close(fd)
    return Path(name)


@contextmanager
def _staged_write(dest: "Path | str"):
    """Yield a local temp path to write into; finalize to dest on success.

    Local dest: temp file in the destination directory (guarantees same
    filesystem), atomic os.replace on success. Remote dest: temp file in
    .barca/staging/, chunked upload on success. Either way the temp file is
    removed on failure and the destination is never left partially written.
    """
    if _storage.is_remote(dest):
        tmp = _make_temp(_staging_dir())
        try:
            yield tmp
            _storage.put_file(tmp, str(dest))
        finally:
            tmp.unlink(missing_ok=True)
    else:
        dest = Path(dest)
        dest.parent.mkdir(parents=True, exist_ok=True)
        tmp = _make_temp(dest.parent, prefix=f".{dest.name}.")
        try:
            yield tmp
            os.replace(tmp, dest)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise


def serialize(value: Any, path: "Path | str", fmt: str) -> int:
    """Write value to path (local or remote URI) in the given format.

    Returns size in bytes. The caller is responsible for having resolved
    the format first (see resolve_format) so path and fmt agree.
    """
    return _serialize(value, path, fmt, want_hash=False)[0]


def serialize_hashed(value: Any, path: "Path | str", fmt: str) -> tuple[int, str]:
    """Like serialize, and also return the SHA-256 (hex) of the bytes written.

    Used for sensors: the coordinator folds the hash of a sensor's output into the run hash of
    every asset that reads it, so a changed output re-runs them.
    """
    size, digest = _serialize(value, path, fmt, want_hash=True)
    assert digest is not None
    return size, digest


def _serialize(value: Any, path: "Path | str", fmt: str, want_hash: bool) -> tuple[int, str | None]:
    if fmt not in ("json", "pickle", "parquet"):
        raise ValueError(f"Unknown format: {fmt}")

    size = 0
    digest = None
    with _staged_write(path) as tmp:
        if fmt == "json":
            with open(tmp, "w") as f:
                json.dump(value, f)
        elif fmt == "pickle":
            with open(tmp, "wb") as f:
                pickle.dump(value, f, protocol=5)
        else:
            _write_parquet(value, tmp)
        size = tmp.stat().st_size
        if want_hash:
            h = hashlib.sha256()
            with open(tmp, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            digest = h.hexdigest()
    return size, digest


def _write_parquet(value: Any, path: Path) -> None:
    """Write a frame to parquet: pandas, polars (incl. LazyFrame), pyarrow Table, duckdb relation."""
    type_name = type(value).__name__
    kind = _frame_kind(value)

    if kind == "polars":
        if type_name == "LazyFrame":
            value = value.collect()
        value.write_parquet(str(path))
        return

    if kind == "pyarrow":
        import pyarrow.parquet as pq

        pq.write_table(value, str(path))
        return

    if kind == "duckdb":
        # Materializes the relation (runs its query) straight to the parquet file.
        value.write_parquet(str(path))
        return

    if hasattr(value, "to_parquet"):
        value.to_parquet(str(path))
        return

    raise TypeError(
        f"parquet format requires a DataFrame, got {type_name} "
        "(use resolve_format() to downgrade to pickle first)"
    )


def deserialize(path: "Path | str", fmt: str, *, frame_type: str | None = None) -> Any:
    """Read an artifact from a local path or remote URI using the given format.

    ``frame_type`` selects the parquet reader when ``fmt == "parquet"``.
    Supported values: ``pandas`` (default), ``polars``, ``polars_lazy``, ``pyarrow``, ``duckdb``.

    A remote artifact is downloaded to a staging file that is removed before returning, except
    for the lazy types (``duckdb``, ``polars_lazy``): they read the file when queried, so it is
    kept until ``release_fetched()``.
    """
    if _storage.is_remote(path):
        tmp = _make_temp(_staging_dir(), prefix="fetch-")
        try:
            _storage.get_file(str(path), tmp)
            value = _deserialize_local(tmp, fmt, frame_type=frame_type)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
        if fmt == "parquet" and frame_type in LAZY_FRAME_TYPES:
            # A duckdb relation or polars LazyFrame scans the file when the step queries it, so
            # the file has to outlive this call. The caller removes it with release_fetched().
            _held_fetches.append(tmp)
        else:
            tmp.unlink(missing_ok=True)
        return value
    return _deserialize_local(Path(path), fmt, frame_type=frame_type)


def _deserialize_local(path: Path, fmt: str, *, frame_type: str | None = None) -> Any:
    if fmt == "json":
        with open(path) as f:
            return json.load(f)

    if fmt == "pickle":
        with open(path, "rb") as f:
            return pickle.load(f)

    if fmt == "parquet":
        return _deserialize_parquet(path, frame_type=frame_type or "pandas")

    raise ValueError(f"Unknown format: {fmt}")


def _deserialize_parquet(path: Path, *, frame_type: str = "pandas") -> Any:
    """Read a parquet file with the loader matching the declared frame type."""
    if frame_type == "polars":
        import polars as pl

        return pl.read_parquet(str(path))

    if frame_type == "polars_lazy":
        import polars as pl

        return pl.scan_parquet(str(path))

    if frame_type == "pyarrow":
        import pyarrow.parquet as pq

        return pq.read_table(str(path))

    if frame_type == "duckdb":
        import duckdb  # ty: ignore[unresolved-import]

        return duckdb.read_parquet(str(path))

    if frame_type == "pandas":
        import pandas as pd

        return pd.read_parquet(str(path))

    raise ValueError(
        f"Unknown frame type {frame_type!r} (supported: pandas, polars, polars_lazy, pyarrow, duckdb)"
    )


def safe_node_id(node_id: str) -> str:
    """Sanitize a node_id for use as a filename (no special chars)."""
    # Replace known special characters with safe alternatives.
    s = node_id
    s = s.replace("/", "__")
    s = s.replace(":", "--")
    s = s.replace("[", "_")
    s = s.replace("]", "")
    s = s.replace("=", "_")
    s = s.replace(",", "_")
    s = s.replace(" ", "_")
    # Remove any remaining problematic characters.
    s = re.sub(r"[^\w.\-]", "_", s)
    return s


def artifact_path(
    artifact_dir: "Path | str", node_id: str, fmt: str, run_hash: "str | None" = None
) -> "Path | str":
    """Compute the deterministic artifact path for a node + format.

    With a run_hash the artifact is content-addressed —
    ``{dir}/{safe_node_id}/{run_hash}{ext}`` — so objects are immutable and
    cache hits transfer across machines. Without one (older coordinators,
    parallel() children, batch mode) the legacy node-id-keyed layout is used.

    Returns a Path for a local artifact_dir, or a URI string when
    artifact_dir is a remote prefix (e.g. BARCA_ARTIFACT_URI).
    """
    ext = _FORMAT_EXTENSIONS.get(fmt, f".{fmt}")
    if run_hash:
        subdir = _storage.join(artifact_dir, safe_node_id(node_id))
        return _storage.join(subdir, f"{run_hash}{ext}")
    return _storage.join(artifact_dir, f"{safe_node_id(node_id)}{ext}")
