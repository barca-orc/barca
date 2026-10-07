---
title: "Pattern: Large Inputs"
description: A parquet input with no annotation is read whole with pandas. Annotate it as a DuckDB relation or a polars LazyFrame to read only what the step uses.
---

Use this when a step reads a large upstream table and needs only some of its columns or rows.

A parquet input with no annotation is read with pandas, in full, before your function runs.
Annotate the parameter `duckdb.DuckDBPyRelation` or `pl.LazyFrame` and nothing is read up
front: the step's own query decides which columns and row groups are read. Barca has no
`columns=` or `where=` option on `inputs=`.

| Annotation | What is read |
|---|---|
| none, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table` | the whole parquet file, before the function runs |
| `duckdb.DuckDBPyRelation`, `pl.LazyFrame` | the file's metadata; then the columns the step's query uses, and the row groups its filter cannot rule out |

## Example

```python
import duckdb
import polars as pl
from barca import asset


@asset()
def events() -> duckdb.DuckDBPyRelation:
    return duckdb.sql("""
        select range as id, range % 10 as bucket, 'user-' || (range % 100) as name
        from range(100000)
    """)


@asset(inputs={"events": events})
def eager(events) -> dict:
    # no annotation: the whole file is read with pandas before this runs
    return {"type": type(events).__name__, "rows": len(events), "columns": list(events.columns)}


@asset(inputs={"events": events})
def per_bucket(events: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    # only the `bucket` column is read
    return events.aggregate("bucket, count(*) as n").order("bucket")


@asset(inputs={"events": events})
def bucket_3(events: pl.LazyFrame) -> pl.LazyFrame:
    return events.filter(pl.col("bucket") == 3).select("id", "name")


@asset(inputs={"_events": events})
def after_events(_events) -> dict:
    return {"ran": True, "received": repr(_events)}
```

This needs `duckdb`, `polars` and `barca[parquet]` installed.

```bash
barca get eager,per_bucket,bucket_3,after_events pipeline.py
```

## What barca does

All five steps run. `eager` received a pandas DataFrame of every row and column
(`"rows": 100000`, `"type": "DataFrame"`). `per_bucket` and `bucket_3` return a relation and a
lazy frame. Barca executes them into the step's parquet file when the step ends, and
`barca get` prints a pointer to the file, not the rows:

```
per_bucket: success
{
  "_barca_artifact": {
    "format": "parquet",
    "path": ".barca/artifacts/pipeline.py--per_bucket/1cbb617a...759caa.parquet",
    "size_bytes": 462
  }
}
```

`after_events` ran after `events` and received `None`: a `_` input is never loaded.

To look at the stored results, use `barca sql`:

```
$ barca sql "select * from per_bucket limit 3"
bucket  n
0       10000
1       10000
2       10000
```

`barca status pipeline.py` shows each result's shape (`10 rows x 2 cols` for `per_bucket`), and
`barca stats <asset> pipeline.py` shows how long a step took.

## Going to pandas

If a step needs pandas, take a DuckDB relation, narrow or aggregate it, and convert the
result: `events.filter("bucket = 3").project("id").df()`. `.df()` builds a DataFrame of
whatever the relation selects, so calling it on the unnarrowed input is the full read the
annotation was meant to avoid.

## Limits

- **A declared input is always loaded** unless its name starts with `_`, whether or not the
  function uses it. Barca warns at plan time about a step that never uses one of its inputs;
  the warning is shown in
  [Ordering-Only Dependencies](/patterns/03-ordering-only-deps/#an-unused-input-without-the-prefix-is-loaded-and-reported).
  Remove an unused input, or rename it `_<name>` (the key in `inputs=` and the parameter) when
  it is there for ordering only.
- **Only parquet artifacts can be read lazily.** A json or pickle input is deserialized whole,
  whatever the annotation.
- **Skipping rows needs clustering.** Reading only the needed columns always works. A filter
  skips a row group only when that group's min/max statistics exclude the value, which
  requires the data to be sorted on the filtered column. In the example `bucket` cycles
  through 0 to 9 in every row group, so `bucket_3` skips no row group. Sort the upstream
  step's output on the column downstream steps filter by. Barca does not sort what a step
  writes.
- **Barca does not check that a lazy step narrows its input.** A step that annotates
  `duckdb.DuckDBPyRelation` and calls `.df()` on it reads everything.
- **Annotations are read from the source.** Use the conventional names (`pd`, `pl`, `pyarrow`,
  `duckdb`).
- **Remote artifacts.** A lazy input is read in place, fetching only the byte ranges the query
  touches, when every reader of the artifact in that phase is lazy. An eager input is
  downloaded whole, once. See [Remote storage](/reference/remote-storage/).
- Steps that use DuckDB share one connection per worker process. See
  [Anti-Patterns](/patterns/07-anti-patterns/#fixed-names-on-duckdbs-shared-connection).

The same material is in the terminal manual: `barca docs big-inputs`. Input annotations are
listed in `barca docs types`.
