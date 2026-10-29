"""Batched partitions (RFC-0013): one call of a step function computes several partition keys.

The coordinator leases the keys of one call to one worker together (an ``execute_batch``
message with a ``call``). The worker calls the function once, with:

- each partition parameter as the list of the call's keys, in key order;
- an input from a partitioned upstream (the same dimension) as a dict of key to that key's
  value, or, when annotated ``duckdb.DuckDBPyRelation``, ``polars.LazyFrame`` or
  ``pyarrow.dataset.Dataset``, as one lazy table over every key's artifact with the dimension
  as a column;
- a keyed ``partitions_from`` source's value as a dict of key to value;
- an unpartitioned input as the whole value, once.

It returns a dict of key to value, or a table with the dimension as a column, which is split
into one value per key. Each key is then written and reported on its own, so each keeps its
own artifact, run hash and materialization.

A key of a call with several dimensions is a tuple of its values, in the order of the
dimension names, sorted.
"""

from __future__ import annotations

import os
from contextlib import contextmanager, nullcontext
from pathlib import Path


class Failed:
    """Return ``barca.Failed("reason")`` as one key's value from a batched step to fail that
    key alone: the other keys of the call succeed and are cached."""

    def __init__(self, reason: str = "") -> None:
        self.reason = str(reason)

    def __repr__(self) -> str:
        return f"Failed({self.reason!r})"


class BatchFailed(Exception):
    """A key that a batched call returned ``barca.Failed`` for, or no result for."""


LAZY_TYPES = ("duckdb", "polars_lazy", "pyarrow_dataset")


def key_dims(steps: list[dict]) -> list[str]:
    """The partition dimensions of a call's keys, sorted."""
    return sorted((steps[0].get("direct_kwargs") or {}).keys())


def key_of(step: dict, dims: list[str]):
    """The key a call's function sees for one of its steps: the value, or a tuple of values."""
    kw = step.get("direct_kwargs") or {}
    if len(dims) == 1:
        return kw[dims[0]]
    return tuple(kw[d] for d in dims)


def key_text(key, dims: list[str]) -> str:
    """``retailer=r7`` (or ``month=2026-10,retailer=r7``), for messages."""
    if len(dims) == 1:
        return f"{dims[0]}={key}"
    return ",".join(f"{d}={v}" for d, v in zip(dims, key))


def _norm(key, dims: list[str]):
    """A returned key, matched to the call's keys by its string form (``1`` is ``"1"``)."""
    if len(dims) == 1:
        return str(key)
    if isinstance(key, (tuple, list)) and len(key) == len(dims):
        return tuple(str(v) for v in key)
    if isinstance(key, dict):
        try:
            return tuple(str(key[d]) for d in dims)
        except KeyError:
            return None
    return None


_SUFFIX_FORMATS = {".parquet": "parquet", ".csv": "csv", ".json": "json"}


def _tabular_format(path: str) -> "str | None":
    return _SUFFIX_FORMATS.get(Path(path).suffix)


def _json_records(path: str) -> bool:
    """Whether a JSON artifact holds a list of records (it starts with `[`)."""
    with open(path, "rb") as f:
        head = f.read(64).lstrip()
    return head.startswith(b"[")


def _not_tabular(param: str, frame_type: str, key: str, path: str) -> TypeError:
    annotation = {
        "duckdb": "duckdb.DuckDBPyRelation",
        "polars_lazy": "polars.LazyFrame",
        "pyarrow_dataset": "pyarrow.dataset.Dataset",
    }[frame_type]
    return TypeError(
        f"the input '{param}' of a batched call is annotated {annotation}, a lazy table over "
        f"its keys' artifacts, which needs tabular artifacts (parquet, CSV, or a JSON list of "
        f"records); the artifact of {key} is not ({path}). Annotate it dict[str, T] to "
        f"receive each key's value instead."
    )


def lazy_table(param: str, frame_type: str, dims: list[str], keys: list, paths: list[str], texts):
    """One lazy table over ``paths`` (one artifact per key) with the column ``dim`` holding
    each row's key. Nothing is loaded until the table is queried."""
    formats = set()
    for i, (k, p) in enumerate(zip(texts, paths)):
        fmt = _tabular_format(p)
        if fmt is None or (frame_type == "pyarrow_dataset" and fmt == "json"):
            raise _not_tabular(param, frame_type, k, p)
        if fmt == "json" and i == 0 and not _json_records(p):
            raise _not_tabular(param, frame_type, k, p)
        formats.add(fmt)
    if len(formats) > 1:
        raise TypeError(
            f"the input '{param}' of a batched call mixes artifact formats "
            f"({', '.join(sorted(formats))}); a lazy table reads one format"
        )
    fmt = formats.pop() if formats else "parquet"
    columns = {d: [str(k if len(dims) == 1 else k[i]) for k in keys] for i, d in enumerate(dims)}
    if frame_type == "duckdb":
        return _duckdb_table(param, fmt, columns, paths)
    if frame_type == "polars_lazy":
        return _polars_table(fmt, columns, paths)
    return _arrow_dataset(fmt, columns, paths)


# Views bound for the lazy duckdb inputs of the call in progress; dropped after it is written.
_bound: list[str] = []


def _duckdb_table(param: str, fmt: str, dimensions: dict, paths: list[str]):
    import pyarrow as pa

    from barca import _duckdb

    con = _duckdb.connection()
    mapping = f"__barca_keys_{param}"
    con.register(mapping, pa.table({"__barca_file": paths, **dimensions}))
    _bound.append(mapping)
    q = _duckdb._quote
    if fmt == "parquet":
        reader = con.read_parquet(paths, filename=True, union_by_name=True)
    elif fmt == "csv":
        reader = con.read_csv(paths, filename=True, union_by_name=True, header=True)
    else:
        reader = con.read_json(paths, format="array", filename=True)
    source = f"__barca_files_{param}"
    reader.create_view(source, replace=True)
    _bound.append(source)
    columns = [c for c in reader.columns if c != "filename"]
    keep = ", ".join(f"f.{q(c)}" for c in columns if c not in dimensions)
    select = f"{keep}, " if keep else ""
    return con.sql(
        f"SELECT {select}{', '.join(f'm.{q(d)}' for d in dimensions)} "
        f"FROM {q(source)} f JOIN {q(mapping)} m "
        f"ON f.filename = m.__barca_file"
    )


def _polars_table(fmt: str, dimensions: dict, paths: list[str]):
    import polars as pl

    mapping = pl.LazyFrame({"__barca_file": paths, **dimensions})
    if fmt == "parquet":
        lf = pl.scan_parquet(paths, include_file_paths="__barca_file")
    elif fmt == "csv":
        lf = pl.scan_csv(paths, include_file_paths="__barca_file")
    else:
        frames = []
        for p in paths:
            frames.append(pl.read_json(p).with_columns(pl.lit(p).alias("__barca_file")))
        lf = pl.concat(frames, how="diagonal_relaxed").lazy()
    present = lf.collect_schema().names()
    lf = lf.drop([d for d in dimensions if d in present])
    return lf.join(mapping, on="__barca_file", how="inner").drop("__barca_file")


def _arrow_dataset(fmt: str, dimensions: dict, paths: list[str]):
    import pyarrow as pa
    import pyarrow.dataset as ds
    import pyarrow.fs as pafs

    file_format = ds.ParquetFileFormat() if fmt == "parquet" else ds.CsvFileFormat()
    filesystem = pafs.LocalFileSystem()
    if paths:
        schema = ds.dataset(paths[0], format=file_format, filesystem=filesystem).schema
    else:
        schema = pa.schema([])
    for dim in dimensions:
        if dim not in schema.names:
            schema = schema.append(pa.field(dim, pa.string()))
    partitions = []
    for i in range(len(paths)):
        expression = ds.scalar(True)
        for dim, values in dimensions.items():
            expression = expression & (ds.field(dim) == pa.scalar(values[i]))
        partitions.append(expression)
    return ds.FileSystemDataset.from_paths(
        [str(Path(p).resolve()) for p in paths],
        schema=schema,
        format=file_format,
        filesystem=filesystem,
        partitions=partitions,
    )


def unbind() -> None:
    """Drop the views the call's lazy duckdb inputs bound (idempotent, never raises)."""
    if not _bound:
        return
    try:
        from barca import _duckdb

        con = _duckdb.connection()
    except Exception:
        _bound.clear()
        return
    for name in reversed(_bound):
        for kind in ("view", "table"):
            try:
                con.execute(f"drop {kind} if exists {name}")
            except Exception:
                pass
        try:
            con.unregister(name)
        except Exception:
            pass
    _bound.clear()


# ─── Outputs ────────────────────────────────────────────────────────────────


def _frame_kind(value) -> "str | None":
    from barca._artifacts import _frame_kind as kind

    return kind(value)


def split(result, keys: list, dims: list[str], staging: Path) -> dict:
    """What a batched call returned, as {normalized key: value}. A missing key is absent; an
    extra key, or a result that is neither a dict nor a table with the dimension columns, is a
    ``TypeError``/``ValueError`` for the whole call.

    A DuckDB relation (and a polars or Arrow table) is written by DuckDB with
    ``COPY ... PARTITION_BY`` into ``staging``: such a key's value is ``("parquet", [files])``.
    """
    wanted = {_norm(k, dims): k for k in keys}
    if isinstance(result, dict):
        out = {}
        for k, v in result.items():
            n = _norm(k, dims)
            if n not in wanted:
                raise ValueError(
                    f"the batched call returned a result for {key_text(k, dims)}, which is not "
                    f"one of its keys"
                )
            out[n] = v
        return out
    kind = _frame_kind(result)
    if kind is None:
        raise TypeError(
            f"a batched step returns a dict of key to value, or a table with the column"
            f"{'s' if len(dims) > 1 else ''} {', '.join(dims)}; it returned "
            f"{type(result).__name__}"
        )
    columns = _columns(result, kind)
    missing = [d for d in dims if d not in columns]
    if missing:
        raise ValueError(
            f"the table a batched step returns is split into keys by the column"
            f"{'s' if len(dims) > 1 else ''} {', '.join(dims)}, and it has no column "
            f"{', '.join(missing)} (its columns: {', '.join(columns)})"
        )
    if kind == "pandas":
        return _split_pandas(result, dims, wanted)
    return _split_duckdb(result, kind, dims, wanted, staging)


def _columns(value, kind: str) -> list[str]:
    if kind == "pandas":
        return [str(c) for c in value.columns]
    if kind == "polars":
        if type(value).__name__ == "LazyFrame":
            return list(value.collect_schema().names())
        return list(value.columns)
    if kind == "pyarrow":
        return list(value.schema.names)
    return list(value.columns)


def _extra(value, dims):
    shown = value if len(dims) > 1 else value[0]
    return ValueError(
        f"the batched call returned rows for {key_text(shown, dims)}, which is not one of its keys"
    )


def _split_pandas(df, dims: list[str], wanted: dict) -> dict:
    out = {}
    by = dims[0] if len(dims) == 1 else dims
    for value, part in df.groupby(by, sort=False, dropna=False):
        values = value if isinstance(value, tuple) else (value,)
        n = _norm(values[0] if len(dims) == 1 else values, dims)
        if n not in wanted:
            raise _extra(values, dims)
        out[n] = part.reset_index(drop=True)
    for n in wanted:
        if n not in out:
            out[n] = df.iloc[0:0].reset_index(drop=True)
    return out


def _native_order(dtype) -> bool:
    """Whether DuckDB 2's partition writer can sort a column of this type (it crashes on
    BIT and BIGNUM, also nested in a list, struct, map or union)."""
    if dtype.id in ("bit", "bignum", "varint"):
        return False
    if dtype.id in ("list", "array", "struct", "map", "union"):
        return all(_native_order(child) for _, child in dtype.children if hasattr(child, "id"))
    return True


@contextmanager
def _without_statistics_propagation(con):
    """DuckDB 2's Parquet writer marks a column REQUIRED when plan statistics prove it has no
    nulls, and a key's statistics differ between its single-file and multi-file writes. The
    same rows must give the same bytes, so no write of the split sees them."""
    before = con.execute("SELECT current_setting('disabled_optimizers')").fetchall()[0][0]
    disabled = [item for item in before.split(",") if item]
    if "statistics_propagation" in disabled:
        yield
        return
    con.execute("SET disabled_optimizers = ?", [",".join([*disabled, "statistics_propagation"])])
    try:
        yield
    finally:
        con.execute("SET disabled_optimizers = ?", [before])


def _split_duckdb(value, kind: str, dims: list[str], wanted: dict, staging: Path) -> dict:
    """Stream a table into one parquet file per key with DuckDB's ``COPY ... PARTITION_BY``."""
    import shutil
    from urllib.parse import unquote

    import duckdb

    from barca import _duckdb

    con = _duckdb.connection()
    q = _duckdb._quote
    name = "__barca_batch_result"
    if kind == "duckdb":
        rel = value
    else:
        if kind == "polars" and type(value).__name__ == "LazyFrame":
            value = value.collect()
        rel = con.from_arrow(value.to_arrow() if kind == "polars" else value)
    rel.create_view(name, replace=True)
    try:
        shutil.rmtree(staging, ignore_errors=True)
        staging.mkdir(parents=True, exist_ok=True)
        dim_list = ", ".join(q(d) for d in dims)
        # Equal rows must give equal bytes whatever order the producer emitted them in, so
        # each key is sorted by every column. DuckDB 2's partition writer sorts each file it
        # writes (ORDER_BY); a key that spans several files, DuckDB 1.x and the types the
        # writer cannot sort are read back and rewritten sorted. Feeding the writer rows
        # grouped by key keeps a key in one file, so most keys skip that second pass.
        native = int(duckdb.__version__.split(".")[0]) >= 2 and all(map(_native_order, rel.types))
        order_by = f", ORDER_BY ({', '.join(q(c) for c in rel.columns)})" if native else ""
        with _without_statistics_propagation(con) if native else nullcontext():
            con.execute(
                f"COPY (SELECT * FROM {q(name)} ORDER BY {dim_list}) TO '{staging}' "
                f"(FORMAT parquet, PARTITION_BY ({dim_list}), WRITE_PARTITION_COLUMNS true, "
                f"OVERWRITE_OR_IGNORE true{order_by})"
            )
            empty = None
            out: dict = {}
            for root, _dirs, files in os.walk(staging):
                parquet = sorted(f for f in files if f.endswith(".parquet"))
                if not parquet:
                    continue
                rel_dir = Path(root).relative_to(staging)
                values = {}
                for part in rel_dir.parts:
                    d, _, v = part.partition("=")
                    values[unquote(d)] = unquote(v)
                if len(dims) == 1:
                    n = values.get(dims[0])
                    if n is None:
                        raise ValueError(
                            f"the batched call returned rows with no {dims[0]} (null); every row "
                            f"needs a key"
                        )
                else:
                    n = tuple(values.get(d) for d in dims)
                if n not in wanted:
                    raise _extra(n if len(dims) > 1 else (n,), dims)
                paths = [str(Path(root) / f) for f in parquet]
                if not native or len(paths) > 1:
                    ordered = str(Path(root) / "__ordered.parquet")
                    con.read_parquet(paths, hive_partitioning=False).order("ALL").write_parquet(
                        ordered
                    )
                    for path in paths:
                        os.remove(path)
                    paths = [ordered]
                out[n] = ("parquet", paths)
            if len(out) < len(wanted):
                # A key with no rows: an empty table with the schema.
                empty = staging / "__empty.parquet"
                con.execute(f"COPY (SELECT * FROM {q(name)} LIMIT 0) TO '{empty}' (FORMAT parquet)")
                for n in wanted:
                    out.setdefault(n, ("parquet", [str(empty)]))
            return out
    finally:
        try:
            con.execute(f"drop view if exists {q(name)}")
        except Exception:
            pass
