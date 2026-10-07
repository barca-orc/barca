---
title: "Pattern: Large Inputs"
description: Annotate a large input as lazy so a step loads only the columns and rows it needs.
---

A parquet input with no annotation is read with pandas, in full, before your function runs (json
and pickle inputs are always read whole). For a large
upstream, annotate the parameter `duckdb.DuckDBPyRelation` or `pl.LazyFrame` instead. The step's
own query then decides what is read: barca has no `columns=` / `where=` option on `inputs=`
because the query already says it.

```python
import duckdb
from barca import asset


@asset()
def events() -> duckdb.DuckDBPyRelation:
    return duckdb.sql("select range as id, range % 10 as bucket from range(100000)")


@asset(inputs={"events": events})
def per_bucket(events: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    # aggregate before anything is materialized: only `bucket` is read
    return events.aggregate("bucket, count(*) as n").order("bucket")
```

## Rules of thumb

- **Lazy input, then filter, project and aggregate.** Return the relation or lazy frame; it is
  executed straight into the step's parquet file.
- **Going to pandas?** Narrow or aggregate the relation first and convert only the small
  result. `Relation.df()` builds a pandas DataFrame of whatever the relation selects, so on the
  unnarrowed input it is the full read you were avoiding.
- **Skipping rows needs clustering.** Reading only the needed columns always works. A filter
  skips row groups only when the data is clustered on the filtered column; sort the upstream's
  output on the column downstream steps filter by.
- **Drop unused inputs.** An input that is not `_`-prefixed is loaded before the step runs.
  Barca warns at plan time when one is never used; remove it, or rename it `_<name>` for
  ordering only
  ([Ordering-Only Deps](/patterns/03-ordering-only-deps/)).
- **Remote artifacts.** A lazy input is read in place (only the byte ranges the query touches
  are fetched) when every reader of the artifact in that phase is lazy; an eager input is
  downloaded whole, once. See
  [Remote storage](/reference/remote-storage/).

The same text is in the terminal manual: `barca docs big-inputs`. Input annotations are listed in
`barca docs types`.
