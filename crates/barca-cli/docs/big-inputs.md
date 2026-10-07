# Large inputs: load only what a step needs

A parquet input that is not annotated is read with pandas, in full, before your function runs.
For a large upstream that can be the most expensive thing a step does. Barca has no
`columns=` / `where=` option on `inputs=`: the step's own query already says which columns and
rows it needs, so **lazy input types are how you load a subset**.

## The rule

For a large parquet input, annotate the parameter `duckdb.DuckDBPyRelation` or `pl.LazyFrame`.
Filter, project and aggregate before you materialize anything.

| Annotation | What is read |
|---|---|
| none, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table` | the whole parquet file, before the function runs |
| `duckdb.DuckDBPyRelation`, `pl.LazyFrame` | no rows before the function runs (opening it reads only the file's metadata); then the columns the step's query uses, and only the row groups its filter cannot rule out (see "Row skipping needs clustering") |

This is about parquet artifacts (what a step returning a DataFrame, Table, LazyFrame or
relation writes). A json or pickle input is always deserialized whole, whatever the annotation
(`barca docs types`).

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

If you need pandas, take a DuckDB relation, narrow or aggregate it, then convert the result, as
`first_ids` does: its `.df()` call builds a pandas DataFrame of five rows and one column, and the
`name` column is never read. `.df()` builds a pandas DataFrame of whatever the relation selects,
so calling it on the unnarrowed input loads every row and column into memory, which is the full
read the lazy annotation was meant to avoid.

Better still, stay in the relation or the lazy frame and return that: it is executed straight
into the step's parquet file (`barca docs types`).

This topic gives no timings: they depend on the data and the machine. Time your own step with
`barca stats <asset>`.

## Row skipping needs clustering

Reading only the columns a query uses always works. Skipping *rows* by a filter only works when
the data is clustered on the filtered column: parquet keeps min/max statistics per row group,
and a filter can skip a row group only when its range excludes the value. If the upstream is
unsorted on that column, a selective filter still reads every row group of the columns it
touches. Sort the upstream step's output on the column you filter by (`order by`) when
downstream steps filter on it.

In the example above, `bucket` cycles through 0 to 9 in every row group, so `bucket_3` and
`first_ids` skip no row group; they save the columns they do not use, not the rows.

## Unused inputs

An input that is not `_`-prefixed is loaded before the step runs, used or not. Barca warns at
plan time when a step never uses one (`barca docs assets`, "Unused inputs"). Remove it, or
rename the parameter `_<name>` (and the key in `inputs=`) when you only need the ordering: a `_`
input is never loaded and the parameter is `None`, as `after_events` above shows.

## Remote artifacts

With a remote artifact store, a lazy input is read in place: only the byte ranges the query
touches are fetched, and the object is not downloaded. An eager input (no annotation,
`pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table`) is downloaded whole, once, into
`.barca/artifacts/`, and later runs on the same machine reuse that copy. The in-place read
applies when every step in the phase that reads the artifact is lazy: one eager reader in the
phase downloads it for all of them (`barca docs remote`).

## Limitations

- Nothing here applies to json or pickle artifacts: they are read whole.
- Barca does not check that a lazy step narrows its input. A step that annotates
  `duckdb.DuckDBPyRelation` and then calls `.df()` on it reads everything.
- Row groups are skipped by the parquet reader (DuckDB or polars), from the file's statistics.
  Barca does not sort or cluster what a step writes; the order is whatever the step returns.

See also: `barca docs types`, `barca docs examples/duckdb`, `barca docs remote`.
