---
title: "Pattern: Large Inputs"
description: Annotate a large input as lazy so a step loads only the columns and rows it needs.
---

An input with no annotation is read with pandas, in full, before your function runs. For a large
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
  result. `Relation.df()` is slow on string columns: on one 22M-row table it took 88 s, against
  3.9 s for `pd.read_parquet`, and 3.9 s even after narrowing to 3 columns and a third of the
  rows (single measurements, one machine). Prefer `.arrow()` or `.pl()` for anything large.
- **Skipping rows needs clustering.** Reading only the needed columns always works. A filter
  skips row groups only when the data is clustered on the filtered column.
- **Drop unused inputs.** Every data input is loaded before the step runs. Barca warns at plan
  time when one is never used; remove it, or rename it `_<name>` for ordering only
  ([Ordering-Only Deps](/patterns/03-ordering-only-deps/)).
- **Remote artifacts.** A lazy input is read in place (only the byte ranges the query touches
  are fetched); an eager input is downloaded whole, once. See
  [Remote storage](/reference/remote-storage/).

The same text is in the terminal manual: `barca docs big-inputs`. Input annotations are listed in
`barca docs types`.
