# Large inputs: load only what a step needs

An input that is not annotated is read with pandas, in full, before your function runs. For a
large upstream that is the single most expensive thing a step can do. Barca has no
`columns=` / `where=` option on `inputs=`: the step's own query already says which columns and
rows it needs, so **lazy input types are how you load a subset**.

## The rule

For a large input, annotate the parameter `duckdb.DuckDBPyRelation` or `pl.LazyFrame`. Filter,
project and aggregate before you materialize anything.

| Annotation | What is read |
|---|---|
| none, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table` | the whole file, up front |
| `duckdb.DuckDBPyRelation`, `pl.LazyFrame` | nothing up front; only the columns (and row groups) the step's query touches, when it runs |

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
def per_bucket(events: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    # aggregate before anything is materialized: only `bucket` is read
    return events.aggregate("bucket, count(*) as n").order("bucket")


@asset(inputs={"events": events})
def bucket_3(events: pl.LazyFrame) -> pl.LazyFrame:
    # polars: filter and project on the lazy frame; the step's result is what is written
    return events.filter(pl.col("bucket") == 3).select("id", "name")


@asset(inputs={"events": events})
def first_ids(events: duckdb.DuckDBPyRelation) -> dict:
    # the pandas path: narrow with the relation first, convert only the small result
    small = events.filter("bucket = 3").order("id").limit(5).project("id")
    return {"ids": small.df()["id"].tolist()}


@asset(inputs={"_events": events})
def after_events(_events) -> dict:
    # ordering only: runs after `events`, never loads it
    return {"ran": True}
```

```bash
barca get per_bucket pipeline.py
barca get first_ids pipeline.py
barca get after_events pipeline.py
```

`first_ids` returns `{"ids": [3, 13, 23, 33, 43]}`; `per_bucket` and `bucket_3` are written as
parquet, so `barca get` prints an `_barca_artifact` pointer for them (`barca docs types`).

## The pandas path

If you need pandas, take a DuckDB relation, narrow or aggregate it, then convert the result.
Do not convert the whole input.

- `Relation.df()` is slow on string columns. On one 22M-row table with string columns,
  converting it with `.df()` took 88 s, against 3.9 s for `pd.read_parquet` of the same file.
  Narrowed to 3 columns and a third of the rows, `.df()` still took 3.9 s. These are single
  measurements on one machine; yours will differ, but the shape holds: `.df()` costs far more
  than the read itself.
- Prefer `.arrow()` or `.pl()` for anything large, and use `.df()` only on small results
  (an aggregate, a `limit`).
- Even better, stay in the relation or the lazy frame and return that: it is executed straight
  into the step's parquet file and nothing is held in memory.

## Row skipping needs clustering

Reading only the columns a query uses always works. Skipping *rows* by a filter only works when
the data is clustered on the filtered column: parquet keeps min/max statistics per row group,
and a filter can skip a row group only when its range excludes the value. If the upstream is
unsorted on that column, a selective filter still reads every row group of the columns it
touches. Sort the upstream step's output on the column you filter by (`order by`) when
downstream steps filter on it.

## Unused inputs

Every declared data input is loaded before the step runs, used or not. Barca warns at plan time
when a step never uses one (`barca docs assets`, "Unused inputs"). Remove it, or rename the
parameter `_<name>` (and the key in `inputs=`) when you only need the ordering: ordering-only
inputs are never loaded, as `after_events` above shows.

## Remote artifacts

With a remote artifact store, a lazy input is read in place: only the byte ranges the query
touches are fetched, nothing is downloaded. Eager inputs (no annotation, `pd.DataFrame`,
`pl.DataFrame`, `pyarrow.Table`) are downloaded whole, once, into `.barca/artifacts/`. This
applies when every step in the phase that reads the result is lazy; see `barca docs remote`.

See also: `barca docs types`, `barca docs examples/duckdb`, `barca docs remote`.
