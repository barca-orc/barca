# Sinks

`@sink` writes an asset's output to an extra location whenever the asset materializes, for
example to export a file for another system. It is stacked on top of `@asset`.

```python
import pandas as pd
from barca import asset, sink


@asset()
@sink("./exports/orders.parquet")
@sink("s3://my-bucket/exports/orders.parquet")
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2], "amount": [9.5, 20.0]})


@asset()
@sink("./exports/summary.json")
def summary() -> dict:
    return {"orders": 2}
```

- Paths are local or any fsspec URI (`abfss://`, `s3://`, `gs://`). Remote schemes need the
  matching extra (`pip install "barca[s3]"`, `[azure]`, `[gcs]`, or `[remote]`).
- The format comes from `serializer=` (`json`, `pickle`, `parquet`, `csv`), else the path extension
  (`.json`, `.pkl`/`.pickle`, `.parquet`, `.csv`), else the parent asset's artifact format.
- A CSV sink writes a header with no pandas index. CSV readers infer types; see `barca docs types`.
- A parquet or CSV sink needs a DataFrame, Arrow table or DuckDB relation. Anything else is a sink
  failure: barca never writes pickle bytes under a `.parquet` or `.csv` name.
- Several `@sink` decorators may be stacked on one asset.
- A sink is written when its asset runs. Adding a `@sink` or editing one (its path or
  `serializer=`) changes the asset's run hash, so the next run executes the asset and writes
  the sink (`barca docs cache`). A cache hit writes a sink only when the sink holds another run
  hash's output ("Cache hits" below).
- What `@sink` takes is in the table in `barca docs assets`, "Accepted arguments". A path
  passed as `path=`, no path, a second positional argument (`@sink("out.txt", "json")`) or any
  other keyword is an error when the file is read, exit 2. Up to 0.18.1 `@sink(path=...)`
  declared no sink and `@sink("out.txt", "json")` ignored the format.
- The path is a string literal, read from the source. A constant, an f-string or a
  concatenation (`@sink(EXPORT_PATH)`, `@sink(f"exports/{name}.parquet")`) is an error when the
  file is read, exit 2 (`barca docs assets`, "Values barca reads"). Up to 0.33.1 such a sink was
  dropped without a message and never written.
- Writes are staged and finalized atomically, so a crash never leaves a partial file.
- A sink path is yours: barca writes the file there and changes nothing else. If the path is a
  symlink, the file it points to is written and the link stays. If a directory is at the path
  (or the link leads to one), the sink fails and the directory is left as it is.
- Sinks are leaf nodes: no other node may take a sink as an input.
- **A sink that fails fails its step**, for every sink: the step's error is a `SinkError`
  naming each sink that failed (`SinkError: sink ./exports/out.parquet: ValueError: ...`), the
  run exits 1 with a `step_failed` envelope (`barca docs contract`), and the step is not
  recorded, so the next run tries it again. Every sink of the step is still attempted. Up to
  0.29 a failed sink only printed `[barca] SINK FAILED: ...` on stderr and the asset succeeded.
- For partitioned assets each partition writes its own file with the key inserted before the
  extension: `out.parquet` becomes `out_region_emea.parquet`. For a dataset that tools can read
  as one partitioned table, end the path with `/` (below).

## Cache hits

Barca records which run hash each sink last published: per sink, per partition key of a
partitioned asset. When the asset is served from cache:

- If the sink holds the cached run hash's output, nothing is written.
- If it holds another one, the sink is written again from the cached artifact, and the step
  does not run. Code A, then B, then A again: the third run is a cache hit on A, and the sink,
  which held B, holds A again. Each sink that was written prints a line on stderr:

  ```
  [barca] sink out.json: republished the cached result of orders
  [barca] sink exports/scored/: republished 1 cached key of scored
  ```

- The cached artifact is read back as the asset's annotated return type (pandas when it has
  none), so a sink written this way is written by that type's writer. With an artifact store,
  an artifact this machine lacks is fetched first; one that is nowhere is computed again, which
  writes the sink.
- A sink that cannot be written this way fails the step like a sink that fails when the step
  runs: `SinkError: sink out.json: ...`, exit 1, a `step_failed` envelope. Nothing is recorded
  as published, so the next run tries again.
- Barca does not look at what is at the path: a sink file deleted or edited by hand is not
  written again while its run hash stays the same. `--refresh` naming the asset runs it, which
  writes the sink.
- The record is per machine. A sink on this machine's disk counts as published only by runs on
  this machine; a `s3://`, `gs://` or `abfss://` sink written by another machine (another
  laptop, a server) is not known here, so a run here may write it again, and a run here that
  matches its own record does not see what another machine wrote there since.
- A metadata DB from before 0.34.0 has no record. The asset's last run that wrote sinks stands
  in for it, so upgrading writes nothing that is current.

## Dataset sinks

A sink path ending in `/` is a **dataset sink**: a Hive-partitioned dataset with one directory
level per partition dimension of the asset, in the order `partitions=` declares them. Nothing
is repeated in the sink:

```python
import pandas as pd
from barca import asset, partitions, sink


@asset(partitions={"retailer": partitions(["r1", "r2"])})
@sink("./exports/scored/", partition_by=["order_date"])
def scored(retailer: str) -> pd.DataFrame:
    return pd.DataFrame({
        "order_date": ["2026-10-21", "2026-10-22", "2026-10-22"],
        "amount": [1.0, 2.0, 3.0],
    })
```

```
exports/scored/_barca_sink.json
exports/scored/retailer=r1/order_date=2026-10-21/part-0.parquet
exports/scored/retailer=r1/order_date=2026-10-22/part-0.parquet
exports/scored/retailer=r2/order_date=2026-10-21/part-0.parquet
exports/scored/retailer=r2/order_date=2026-10-22/part-0.parquet
```

Read it as one table, with the partition values recovered from the path:

```sql
select retailer, order_date, sum(amount)
from read_parquet('exports/scored/**/*.parquet', hive_partitioning = true)
group by all
```

- `partition_by=["col", ...]` adds levels from columns of the returned table, after the asset's
  own dimensions. On an unpartitioned asset it is the only partitioning
  (`orders/order_date=2026-10-22/part-0.parquet`); without it an unpartitioned asset writes
  `orders/part-0.parquet`.
- `rows_per_file=N` caps the rows of each parquet file (`part-0`, `part-1`, ...).
- The format is parquet (zstd) for a table, unless `serializer="csv"` or `serializer="json"`.
  csv and json write one file per partition; `rows_per_file=` applies to parquet only. json is
  written as JSON Lines (`part-0.jsonl`, one object per row; read it with
  `read_json('exports/scored/**/*.jsonl', hive_partitioning = true)`). A value that is not a
  table is written as one JSON document, `part-0.jsonl`, and takes neither `partition_by=` nor
  `rows_per_file=`.
- Partition columns live in the path and are left out of the files, as Hive does. A column of
  the returned table named like one of the asset's dimensions is left out too: the path carries
  the key.
- Values are encoded as pyarrow encodes them: `a b` is `a%20b`, `x/y` is `x%2Fy`, and a null or
  empty value is `__HIVE_DEFAULT_PARTITION__`. DuckDB and pyarrow decode them.

**Who writes.** The writer of the returned value writes the files, and every writer produces
the same layout and file names:

| The asset returns | Writer |
|---|---|
| a DuckDB relation (including one from `barca.duckdb_connection()`) | DuckDB (`COPY ... PARTITION_BY`), streaming: nothing is loaded into Python. With `rows_per_file=` or json, DuckDB's COPY cannot do it, so the relation is streamed in Arrow batches to pyarrow's writer. |
| a polars `DataFrame` or `LazyFrame` | polars (`sink_parquet` / `sink_csv` with `pl.PartitionBy`; a polars without `PartitionBy`, and json, go through Arrow) |
| a pandas `DataFrame` or Arrow table | `pyarrow.dataset.write_dataset`, Hive flavor |

**How a key is written.**

- *Only when the key runs, or its run hash changed.* A cached key writes nothing, unless its
  directory holds another run hash's output ("Cache hits" above). A key whose inputs changed
  rewrites its own directory and no other: one sensor key per retailer re-runs, and so
  rewrites, only the retailers whose version changed.
- *Staged, then swapped.* The writer writes the key into a staging directory next to the
  prefix (`.<name>.barca-staging/`); for `s3://`, `gs://` and `abfs://` the staging is local
  (`.barca/staging/`) and barca uploads with the store's fsspec credentials. On a local disk
  the key's directory is then swapped in with one rename (an atomic exchange of the two
  directories on Linux; elsewhere the old directory is renamed aside first, so for that
  instant the key is missing, never mixed), and the old files are deleted. A crash before the
  swap leaves the old key as it was; the next write removes what the crashed one staged. On an
  object store, which has no renames, the new files are uploaded first (each object is
  replaced whole) and the key's old files that the new write does not have are deleted after:
  a reader listing the key during the upload can see some new files and some old ones.
- *The dataset mirrors the asset.* When a key disappears from the asset (a value removed from
  `partitions([...])`, or a `partitions_from` source that no longer lists it), the run deletes
  its directory after the steps have run: `[barca] dataset sink exports/scored/: deleted 1 key
  its asset no longer has`. An unpartitioned asset's dataset is replaced whole, so a
  `partition_by` value that is gone disappears with the write. A run narrowed with
  `--partition` does not know every key and deletes nothing. If the deletion fails, the run
  prints a `[barca] warning:` line and still succeeds: its steps did.
- *The sink owns its prefix.* The first write puts `_barca_sink.json` at the prefix: the asset
  that owns it, the dimensions, `partition_by` and the format. Barca refuses to write into a
  prefix that holds files without that marker (`the dataset prefix exports/scored/ holds files
  barca did not write`), or whose marker names another asset: two assets cannot share a
  prefix. Point the sink at a new prefix, or empty the old one. Files whose names start with
  `.` are not counted.
- Barca does not use DuckDB's `OVERWRITE`, which clears the whole target first.

**Size warnings.** A write that creates more than 1,000 directories, or at least 100 files
averaging under 1 MB, prints a warning, since many small files are slow to read:

```
[barca] warning: sink out/by_id/: one write created 1001 directories (partition_by=[id]). ...
```

The asset's own cached parquet copy is still written alongside the dataset, so a large output is
written twice. Writing straight to the store (DuckDB `httpfs`, without local staging) is not
done.

## How to choose partition columns

Partition by what readers filter on. A reader that asks for `order_date = '2026-10-22'` opens
only that directory; a column nobody filters on only adds directories.

- **Coarse to fine.** List the columns from the one with the fewest values to the most:
  `partition_by=["region", "order_date"]`, not the other way round. The asset's own dimensions
  come first, so they should be the coarsest split (a retailer, a tenant), with a finer
  `partition_by` below.
- **Cardinality.** Each distinct value is a directory, and each combination of values is a
  file. A column with thousands of values (an id, a timestamp) makes thousands of tiny files;
  partition by a coarser derived column instead (the date of a timestamp, the month of a date,
  a bucket of an id) and keep the fine column in the files.
- **File size.** Aim for files of tens to hundreds of MB. Fewer, larger files read faster than
  many small ones; the size warning fires below about 1 MB. Raise `rows_per_file=` (or drop a
  level) if files are small; set it if one partition would be one huge file.
- **Path types.** Values travel as directory names, so readers infer their type: `2026-10-22`
  reads back as a date and `7` as an integer in DuckDB. Use strings, integers and dates;
  floats and timestamps make awkward, lossy names. A null becomes
  `__HIVE_DEFAULT_PARTITION__`.

See also: `barca docs types`, `barca docs partitions`.
