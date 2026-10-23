"""Dataset sinks: ``@sink("exports/scored/")`` writes a Hive-partitioned dataset.

A sink path ending in ``/`` is a dataset prefix. Each run of one of the asset's keys writes the
key's own directory (``retailer=r1/``, one level per partition dimension of the asset, in
declared order), and ``partition_by=[...]`` adds levels from columns of the returned table
below it. An unpartitioned asset's key directory is the prefix itself.

How a key is written:

1. The prefix is claimed: it must be empty, missing, or hold ``_barca_sink.json`` written for
   this asset. A prefix with other files is refused, so a typo can never delete someone's data.
2. The writer of the returned value (DuckDB for a relation, polars for a polars frame, pyarrow
   for a pandas frame or an Arrow table) writes the key into a local staging directory. File
   names are made the same for every writer: ``part-0.parquet``, ``part-1.parquet``, ...
3. The staging directory is swapped in for the key's directory. On a local disk the swap is a
   rename (an exchange of the two directories on Linux), so a reader sees the old key or the
   new one. On an object store each file is uploaded (one object at a time is atomic), then the
   key's old files that the new write does not have are deleted.

``python -m barca._sinks prune`` deletes the directories of keys that no longer exist (the
coordinator runs it after a run with the asset's whole key set).

stdlib only at import time: pyarrow, polars and duckdb are imported when a writer needs them.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import uuid
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote

from barca import _storage

MARKER = "_barca_sink.json"
# The Hive name of a null (or empty) partition value, as pyarrow, polars and DuckDB write it.
DEFAULT_PARTITION = "__HIVE_DEFAULT_PARTITION__"
# JSON is written as JSON Lines, `.jsonl`: a `**/*.json` glob would also match the marker.
EXTENSIONS = {"parquet": ".parquet", "csv": ".csv", "json": ".jsonl"}
FORMATS = tuple(EXTENSIONS)

# Size warnings: a write that creates more directories than this, or at least
# `SMALL_FILES_MIN_COUNT` files averaging under `SMALL_FILE_BYTES`, usually partitions by a
# column with too many distinct values.
WARN_DIRECTORIES = 1000
SMALL_FILES_MIN_COUNT = 100
SMALL_FILE_BYTES = 1 << 20

_STAGING_SUFFIX = ".barca-staging"


class SinkError(Exception):
    """A sink could not be written. The step that returned the value fails."""


def is_dataset(path: str) -> bool:
    return str(path).endswith("/")


def hive_value(value: Any) -> str:
    """A partition value as a Hive directory name segment, encoded as pyarrow encodes it."""
    if value is None or value == "":
        return DEFAULT_PARTITION
    return quote(str(value), safe="")


def key_dir(dims: list[str], values: dict[str, Any]) -> str:
    """``retailer=r1/region=eu`` for the asset key ``values``; "" for an unpartitioned asset."""
    return "/".join(f"{d}={hive_value(values[d])}" for d in dims)


def _prefix(path: str) -> str:
    """The dataset prefix without its trailing slash (``s3://b/x/`` -> ``s3://b/x``)."""
    stripped = str(path).rstrip("/")
    if stripped.endswith(":/") or stripped.endswith(":") or not stripped:
        # `s3://bucket/` keeps its bucket: `s3://bucket`.
        return str(path)[:-1] if str(path).endswith("/") else str(path)
    return stripped


# ─── Writing one key ─────────────────────────────────────────────────────────


def write(result: Any, sink: dict, key: dict[str, Any], asset: str) -> dict:
    """Write one key of a dataset sink. Returns the outcome recorded for the step."""
    from barca._artifacts import _frame_kind

    prefix = _prefix(sink["path"])
    dims = list(sink.get("dims") or [])
    partition_by = list(sink.get("partition_by") or [])
    rows_per_file = sink.get("rows_per_file")
    kind = _frame_kind(result)
    fmt = sink.get("serializer") or ("parquet" if kind else "json")
    if fmt not in FORMATS:
        raise SinkError(
            f"a dataset sink writes parquet, csv or json, not {fmt}; drop serializer= or "
            "set it to one of those"
        )
    if kind is None and (fmt != "json" or partition_by or rows_per_file):
        what = "partition_by=" if partition_by else "rows_per_file=" if rows_per_file else fmt
        raise SinkError(
            f"a {type(result).__name__} cannot be written as a dataset with {what}; return a "
            "DataFrame, Arrow table or DuckDB relation"
        )
    missing = [d for d in dims if d not in key]
    if missing:
        raise SinkError(f"the step has no value for the partition dimension {missing[0]!r}")
    rel = key_dir(dims, key)
    layout = {"dims": dims, "partition_by": partition_by, "format": fmt}
    target = _target(prefix)
    target.claim(asset, layout)
    stage = target.new_stage()
    try:
        if kind is None:
            _write_json_value(result, stage)
        else:
            _write_table(result, kind, stage, fmt, partition_by, rows_per_file, set(dims))
        files = _normalize(stage, EXTENSIONS[fmt])
        stats = _stats(stage, files)
        _crash_point()
        target.swap_in(stage, rel, layout if not rel else None)
    finally:
        target.drop_stage(stage)
    where = f"{prefix}/{rel}" if rel else prefix
    outcome: dict = {
        "path": where + "/",
        "status": "ok",
        "size_bytes": stats["bytes"],
        "files": stats["files"],
    }
    warning = _size_warning(sink["path"], partition_by, stats)
    if warning:
        outcome["warning"] = warning
    return outcome


def _crash_point() -> None:
    """Between staging and the swap. A test replaces it to crash a write there."""


def _stats(stage: Path, files: list[Path]) -> dict:
    size = sum(f.stat().st_size for f in files)
    dirs = {f.parent for f in files if f.parent != stage}
    return {"files": len(files), "bytes": size, "directories": len(dirs)}


def _size_warning(path: str, partition_by: list[str], stats: dict) -> str | None:
    cols = ", ".join(partition_by)
    if stats["directories"] > WARN_DIRECTORIES:
        return (
            f"sink {path}: one write created {stats['directories']} directories "
            f"(partition_by=[{cols}]). A column with this many distinct values makes many "
            "small files; partition by a coarser column (`barca docs sinks`, \"How to choose "
            'partition columns")'
        )
    if stats["files"] >= SMALL_FILES_MIN_COUNT and stats["bytes"] / stats["files"] < SMALL_FILE_BYTES:
        avg = stats["bytes"] / stats["files"] / 1024
        why = f"partition_by=[{cols}]" if partition_by else "rows_per_file="
        return (
            f"sink {path}: one write created {stats['files']} files averaging {avg:.0f} KB "
            f"({why}). Files under about 1 MB are slow to read; partition by a coarser column "
            "or raise rows_per_file (`barca docs sinks`, \"How to choose partition columns\")"
        )
    return None


def _normalize(stage: Path, ext: str) -> list[Path]:
    """Name the data files of every directory `part-0`, `part-1`, ... in the writer's order,
    so that every writer produces the same layout. Returns the data files."""
    out: list[Path] = []
    for root, dirnames, filenames in os.walk(stage):
        dirnames.sort()
        data = sorted(f for f in filenames if f.endswith(ext) and not f.startswith((".", "_")))
        for junk in set(filenames) - set(data):
            # Writers' side files (`.crc`, `_SUCCESS`) are not part of the dataset.
            os.unlink(os.path.join(root, junk))
        wanted = [f"part-{i}{ext}" for i in range(len(data))]
        if data != wanted and sorted(data) != sorted(wanted):
            moved = []
            for i, name in enumerate(data):
                tmp = os.path.join(root, f".rename-{i}")
                os.rename(os.path.join(root, name), tmp)
                moved.append(tmp)
            for tmp, name in zip(moved, wanted):
                os.rename(tmp, os.path.join(root, name))
        elif data != wanted:
            # The same names, sorted as text (`part-10` before `part-2`): already canonical.
            pass
        out.extend(Path(root) / name for name in wanted)
    return out


# ─── Writers ─────────────────────────────────────────────────────────────────


def _write_json_value(value: Any, stage: Path) -> None:
    """A value that is not a table, as one JSON document on one line."""
    with open(stage / "part-0.jsonl", "w") as f:
        json.dump(value, f)
        f.write("\n")


def _check_columns(columns: list[str], partition_by: list[str], drop: set[str]) -> list[str]:
    missing = [c for c in partition_by if c not in columns]
    if missing:
        raise SinkError(
            f"partition_by column {missing[0]!r} is not a column of the returned table "
            f"(columns: {', '.join(columns)})"
        )
    kept = [c for c in columns if c not in drop]
    if not [c for c in kept if c not in partition_by]:
        raise SinkError(
            "every column of the returned table is a partition column, so the files would "
            "have no columns; return at least one other column"
        )
    return kept


def _write_table(result, kind, stage: Path, fmt, partition_by, rows_per_file, drop) -> None:
    if kind == "duckdb":
        _write_duckdb(result, stage, fmt, partition_by, rows_per_file, drop)
    elif kind == "polars":
        _write_polars(result, stage, fmt, partition_by, rows_per_file, drop)
    else:
        import pyarrow as pa

        table = pa.Table.from_pandas(result) if kind == "pandas" else result
        kept = _check_columns(list(table.column_names), partition_by, drop)
        if len(kept) != len(table.column_names):
            table = table.select(kept)
        _write_arrow(table, stage, fmt, partition_by, rows_per_file)


def _write_duckdb(rel, stage: Path, fmt, partition_by, rows_per_file, drop) -> None:
    """DuckDB writes a relation itself, streaming: nothing is loaded into Python.

    DuckDB's COPY cannot combine PARTITION_BY with a file size limit, its row group sizes are
    approximate, and it has no partitioned JSON. For `rows_per_file=` and JSON the relation is
    streamed as Arrow record batches into pyarrow's dataset writer instead, still in batches.
    """
    kept = _check_columns(list(rel.columns), partition_by, drop)
    if len(kept) != len(rel.columns):
        rel = rel.project(", ".join('"' + c.replace('"', '""') + '"' for c in kept))
    if fmt == "json" or rows_per_file:
        reader = rel.fetch_record_batch()
        _write_arrow(reader, stage, fmt, partition_by, rows_per_file)
        return
    if fmt == "parquet":
        if partition_by:
            rel.to_parquet(str(stage), compression="zstd", partition_by=partition_by, overwrite=True)
        else:
            rel.to_parquet(str(stage / "part-0.parquet"), compression="zstd")
    else:
        if partition_by:
            rel.to_csv(str(stage), header=True, partition_by=partition_by, overwrite=True)
        else:
            rel.to_csv(str(stage / "part-0.csv"), header=True)


def _write_polars(frame, stage: Path, fmt, partition_by, rows_per_file, drop) -> None:
    """polars writes a polars frame; a LazyFrame streams through `sink_parquet`."""
    import polars as pl

    lazy = frame if isinstance(frame, pl.LazyFrame) else frame.lazy()
    columns = list(lazy.collect_schema().names())
    kept = _check_columns(columns, partition_by, drop)
    if len(kept) != len(columns):
        lazy = lazy.select(kept)
    partition = getattr(pl, "PartitionBy", None)
    if fmt == "json" or partition is None:
        # polars has no partitioned JSON writer, and polars before 2.0 no `PartitionBy`.
        _write_arrow(lazy.collect().to_arrow(), stage, fmt, partition_by, rows_per_file)
        return
    options: dict = {"approximate_bytes_per_file": None}
    if partition_by:
        options.update(key=partition_by, include_key=False)
    if rows_per_file:
        options["max_rows_per_file"] = rows_per_file
    target = partition(str(stage), **options)
    if fmt == "parquet":
        lazy.sink_parquet(target, compression="zstd", mkdir=True)
    else:
        lazy.sink_csv(target, include_header=True, mkdir=True)
    if not any(stage.iterdir()) and not partition_by:
        # An empty frame: polars writes no file, the others one with no rows.
        empty = lazy.limit(0).collect()
        if fmt == "parquet":
            empty.write_parquet(stage / "part-0.parquet", compression="zstd")
        else:
            empty.write_csv(stage / "part-0.csv")


def _write_arrow(data, stage: Path, fmt, partition_by, rows_per_file) -> None:
    """pyarrow's dataset writer, Hive flavor: an Arrow table or a stream of record batches."""
    import pyarrow.dataset as pds

    if fmt == "json":
        # Partitioned like parquet, then each file is rewritten as JSON lines in place, so the
        # directory names are the ones the parquet writers produce.
        _write_arrow(data, stage, "parquet", partition_by, None)
        for path in sorted(stage.rglob("*.parquet")):
            _parquet_to_json_lines(path, path.with_suffix(".jsonl"))
            path.unlink()
        return
    kwargs: dict = {
        "basename_template": "part-{i}" + EXTENSIONS[fmt],
        "existing_data_behavior": "overwrite_or_ignore",
    }
    if partition_by:
        kwargs.update(partitioning=partition_by, partitioning_flavor="hive")
    if rows_per_file:
        kwargs.update(max_rows_per_file=rows_per_file, max_rows_per_group=min(rows_per_file, 1 << 20))
    if fmt == "parquet":
        file_format = pds.ParquetFileFormat()
        kwargs["file_options"] = file_format.make_write_options(compression="zstd")
    else:
        file_format = pds.CsvFileFormat()
    schema = data.schema
    pds.write_dataset(data, str(stage), format=file_format, **kwargs)
    if not partition_by and not any(stage.iterdir()):
        # An empty table: write one file with no rows, so the dataset still has its columns.
        pds.write_dataset(schema.empty_table(), str(stage), format=file_format, **kwargs)


def _json_default(value):
    import datetime
    import decimal

    if isinstance(value, (datetime.date, datetime.datetime, datetime.time)):
        return value.isoformat()
    if isinstance(value, decimal.Decimal):
        return float(value)
    if isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    if isinstance(value, datetime.timedelta):
        return value.total_seconds()
    return str(value)


def _parquet_to_json_lines(src: Path, dest: Path) -> None:
    import pyarrow.parquet as pq

    pf = pq.ParquetFile(src)
    with open(dest, "w") as out:
        for batch in pf.iter_batches():
            for row in batch.to_pylist():
                out.write(json.dumps(row, default=_json_default))
                out.write("\n")


# ─── Targets: a local directory or an object store prefix ────────────────────


def _target(prefix: str):
    if _storage.is_remote(prefix):
        return _RemoteTarget(prefix)
    return _LocalTarget(prefix)


def _marker_body(asset: str, layout: dict, digest: str | None = None) -> dict:
    body = {"barca_sink": 1, "asset": asset, **layout}
    if digest is not None:
        body["keys_digest"] = digest
    return body


def _refuse_foreign(prefix: str, sample: str) -> SinkError:
    return SinkError(
        f"the dataset prefix {prefix}/ holds files barca did not write (for example "
        f"{sample}) and no {MARKER}. A dataset sink owns its prefix: barca replaces and "
        "deletes what is in it. Point the sink at an empty or new prefix, or empty this one"
    )


def _check_owner(prefix: str, marker: dict, asset: str) -> None:
    owner = marker.get("asset")
    if owner and owner != asset:
        raise SinkError(
            f"the dataset prefix {prefix}/ belongs to the asset {owner} ({MARKER}); two "
            "assets cannot share a dataset prefix. Give this sink its own prefix, or delete "
            f"{prefix}/{MARKER} to hand the prefix to {asset}"
        )


def _same_layout(marker: dict, asset: str, layout: dict) -> bool:
    return marker.get("asset") == asset and all(marker.get(k) == v for k, v in layout.items())


class _LocalTarget:
    def __init__(self, prefix: str):
        local = _storage.local_path_of(prefix)
        assert local is not None
        # A prefix that is a symlink: the directory it points to is the dataset.
        self.shown = prefix
        self.root = Path(os.path.realpath(local))
        self.staging = self.root.parent / f".{self.root.name}{_STAGING_SUFFIX}"

    def _read_marker(self) -> dict | None:
        try:
            with open(self.root / MARKER) as f:
                return json.load(f)
        except FileNotFoundError:
            return None
        except (OSError, ValueError) as exc:
            raise SinkError(f"cannot read {self.shown}/{MARKER}: {exc}") from exc

    def claim(self, asset: str, layout: dict) -> None:
        if self.root.exists() and not self.root.is_dir():
            raise SinkError(f"the dataset prefix {self.shown}/ is a file, not a directory")
        marker = self._read_marker()
        if marker is None and self.root.is_dir():
            foreign = sorted(e for e in os.listdir(self.root) if not e.startswith("."))
            if MARKER in foreign:
                # Another of the asset's keys claimed the prefix in between.
                marker = self._read_marker()
            elif foreign:
                raise _refuse_foreign(self.shown, foreign[0])
        if marker is not None:
            _check_owner(self.shown, marker, asset)
            if _same_layout(marker, asset, layout):
                return
        self.root.mkdir(parents=True, exist_ok=True)
        self.write_marker(_marker_body(asset, layout))

    def write_marker(self, body: dict) -> None:
        # Written beside the prefix and renamed in: never half written, and never a file in
        # the prefix that is not the marker.
        tmp = self.root.parent / f".{self.root.name}.marker-{os.getpid()}-{uuid.uuid4().hex}"
        try:
            tmp.write_text(json.dumps(body, indent=2) + "\n")
            os.replace(tmp, self.root / MARKER)
        finally:
            tmp.unlink(missing_ok=True)

    def new_stage(self) -> Path:
        self.staging.mkdir(parents=True, exist_ok=True)
        _clean_dead_stages(self.staging)
        stage = self.staging / f"{os.getpid()}-{uuid.uuid4().hex}"
        stage.mkdir()
        return stage

    def drop_stage(self, stage: Path) -> None:
        shutil.rmtree(stage, ignore_errors=True)
        try:
            self.staging.rmdir()
        except OSError:
            pass

    def swap_in(self, stage: Path, rel: str, marker_layout: dict | None) -> None:
        if not rel:
            # An unpartitioned asset: the whole prefix is its one key, marker included.
            shutil.copy2(self.root / MARKER, stage / MARKER)
            dest = self.root
        else:
            dest = self.root / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
        if not dest.exists():
            os.rename(stage, dest)
            return
        if _exchange(stage, dest):
            return  # `stage` now holds the old files; drop_stage removes them
        # No atomic exchange here: move the old directory aside, the new one in, then
        # remove the old one. A reader in between finds the key missing, never mixed.
        aside = self.staging / f"old-{os.getpid()}-{uuid.uuid4().hex}"
        try:
            os.rename(dest, aside)
        except OSError:
            # The prefix itself cannot be renamed (a mount point): replace its entries.
            _replace_entries(stage, dest)
            return
        os.rename(stage, dest)
        shutil.rmtree(aside, ignore_errors=True)

    # Pruning ------------------------------------------------------------------

    def prune(self, asset: str, dims: list[str], keep: set[str], digest: str) -> dict:
        marker = self._read_marker()
        if marker is None:
            return {"removed": 0}
        _check_owner(self.shown, marker, asset)
        removed = 0

        def walk(directory: Path, depth: int, rel: str) -> None:
            nonlocal removed
            for entry in sorted(os.listdir(directory)):
                if entry.startswith(".") or (depth == 0 and entry == MARKER):
                    continue
                path = directory / entry
                sub = f"{rel}/{entry}" if rel else entry
                is_level = path.is_dir() and entry.startswith(f"{dims[depth]}=")
                if is_level and depth + 1 < len(dims):
                    walk(path, depth + 1, sub)
                    try:
                        path.rmdir()  # only when it emptied
                    except OSError:
                        pass
                    continue
                if is_level and sub in keep:
                    continue
                if path.is_dir() and not path.is_symlink():
                    shutil.rmtree(path)
                else:
                    path.unlink()
                removed += 1

        if dims and self.root.is_dir():
            walk(self.root, 0, "")
        self.write_marker({**marker, "keys_digest": digest})
        return {"removed": removed}


def _replace_entries(stage: Path, dest: Path) -> None:
    new = set(os.listdir(stage))
    for name in new:
        target = dest / name
        if target.is_dir() and not target.is_symlink():
            aside = dest / f".{name}.old-{uuid.uuid4().hex}"
            os.rename(target, aside)
            os.rename(stage / name, target)
            shutil.rmtree(aside, ignore_errors=True)
        else:
            os.replace(stage / name, target)
    for name in set(os.listdir(dest)) - new:
        if name.startswith("."):
            continue
        path = dest / name
        if path.is_dir() and not path.is_symlink():
            shutil.rmtree(path)
        else:
            path.unlink()


def _exchange(a: Path, b: Path) -> bool:
    """Swap two directories in one step (Linux `renameat2(RENAME_EXCHANGE)`)."""
    if not sys.platform.startswith("linux"):
        return False
    try:
        import ctypes

        libc = ctypes.CDLL(None, use_errno=True)
        renameat2 = libc.renameat2
    except (OSError, AttributeError):
        return False
    at_fdcwd, rename_exchange = -100, 2
    renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    return renameat2(at_fdcwd, os.fsencode(a), at_fdcwd, os.fsencode(b), rename_exchange) == 0


def _clean_dead_stages(staging: Path) -> None:
    """Remove what writers that are no longer running left in the staging directory."""
    from barca._artifacts import _pid_alive

    for entry in staging.iterdir():
        pid = entry.name.split("-", 2)[1 if entry.name.startswith("old-") else 0]
        if pid.isdigit() and not _pid_alive(int(pid)):
            if entry.is_dir():
                shutil.rmtree(entry, ignore_errors=True)
            else:
                entry.unlink(missing_ok=True)


class _RemoteTarget:
    def __init__(self, prefix: str):
        self.shown = prefix
        self.root = prefix
        self.fs = _storage.get_fs(prefix)

    def _read_marker(self) -> dict | None:
        uri = f"{self.root}/{MARKER}"
        try:
            with self.fs.open(uri, "rb") as f:
                return json.loads(f.read())
        except FileNotFoundError:
            return None
        except ValueError as exc:
            raise SinkError(f"cannot read {self.shown}/{MARKER}: {exc}") from exc

    def _objects(self, under: str) -> list[str]:
        self.fs.invalidate_cache(under)
        try:
            found = self.fs.find(under)
        except FileNotFoundError:
            return []
        # Not the prefix itself, nor the empty "directory" objects some stores list.
        base = self._key(under)
        return sorted(o for o in found if o.rstrip("/") != base and not o.endswith("/"))

    def claim(self, asset: str, layout: dict) -> None:
        marker = self._read_marker()
        if marker is None:
            existing = self._objects(self.root)
            existing = [o for o in existing if not o.rsplit("/", 1)[-1].startswith(".")]
            if any(o.rsplit("/", 1)[-1] == MARKER for o in existing):
                # Another of the asset's keys claimed the prefix in between.
                marker = self._read_marker()
            elif existing:
                raise _refuse_foreign(self.shown, existing[0].rsplit("/", 1)[-1])
        if marker is not None:
            _check_owner(self.shown, marker, asset)
            if _same_layout(marker, asset, layout):
                return
        self.write_marker(_marker_body(asset, layout))

    def write_marker(self, body: dict) -> None:
        # One request (not a multi-part upload): several keys may claim the prefix at once.
        self.fs.pipe(f"{self.root}/{MARKER}", (json.dumps(body, indent=2) + "\n").encode())

    def new_stage(self) -> Path:
        from barca._artifacts import _staging_dir

        stage = _staging_dir() / f"dataset-{uuid.uuid4().hex}"
        stage.mkdir()
        return stage

    def drop_stage(self, stage: Path) -> None:
        shutil.rmtree(stage, ignore_errors=True)

    def swap_in(self, stage: Path, rel: str, marker_layout: dict | None) -> None:
        base = f"{self.root}/{rel}" if rel else self.root
        local, remote = [], []
        for path in sorted(p for p in stage.rglob("*") if p.is_file()):
            local.append(str(path))
            remote.append(f"{base}/{path.relative_to(stage).as_posix()}")
        if local:
            self.fs.put(local, remote)
        new = {self._key(r) for r in remote}
        old = [o for o in self._objects(base) if self._key(o) not in new]
        old = [o for o in old if self._key(o) != self._key(f"{self.root}/{MARKER}")]
        if old:
            self.fs.rm(old)

    def _key(self, uri: str) -> str:
        """An object's path as `find` returns it (without the scheme)."""
        return self.fs._strip_protocol(uri).rstrip("/")

    def prune(self, asset: str, dims: list[str], keep: set[str], digest: str) -> dict:
        marker = self._read_marker()
        if marker is None:
            return {"removed": 0}
        _check_owner(self.shown, marker, asset)
        root = self._key(self.root)
        doomed = []
        keys = set()
        for obj in self._objects(self.root):
            rel = obj[len(root) :].lstrip("/")
            if rel == MARKER:
                continue
            parts = rel.split("/")
            levels = parts[: len(dims)]
            ok = len(parts) > len(dims) and all(
                p.startswith(f"{d}=") for p, d in zip(levels, dims)
            )
            if not ok or "/".join(levels) not in keep:
                doomed.append(obj)
                keys.add("/".join(levels) if ok else rel)
        if doomed:
            self.fs.rm(doomed)
        self.write_marker({**marker, "keys_digest": digest})
        return {"removed": len(keys)}


# ─── Pruning: `python -m barca._sinks prune` ─────────────────────────────────


def prune(request: dict) -> dict:
    """Delete the keys of each dataset that its asset no longer has.

    The request: ``{"datasets": [{"path", "asset", "dims", "keys": [[value, ...], ...],
    "digest"}]}``, keys in ``dims`` order. Answers ``{"datasets": [{"path", "removed"} or
    {"path", "error"}]}``.
    """
    out = []
    for ds in request.get("datasets", []):
        path = ds["path"]
        try:
            dims = list(ds["dims"])
            keep = {key_dir(dims, dict(zip(dims, k))) for k in ds["keys"]}
            target = _target(_prefix(path))
            done = target.prune(ds["asset"], dims, keep, ds["digest"])
            out.append({"path": path, **done})
        except Exception as exc:
            out.append({"path": path, "error": _storage.safe_error(f"{type(exc).__name__}: {exc}")})
    return {"datasets": out}


def main(argv: list[str]) -> int:
    if argv[:1] != ["prune"]:
        print("usage: python -m barca._sinks prune < request.json", file=sys.stderr)
        return 2
    request = json.load(sys.stdin)
    json.dump(prune(request), sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
