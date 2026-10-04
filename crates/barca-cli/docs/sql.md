# barca sql: query cached results

`barca sql` runs a DuckDB query over the results barca already has on disk. Use it to look at
data while debugging, for example to see which keys a validation counted as failing, without
writing a probe step or a script.

```bash
barca sql "select * from revenue"
barca sql "select ca, ppg, week, residual from reconcile_failures where abs(residual) > 0.01"
barca sql "select o.region, count(*) from orders o join revenue r using (region) group by 1"
```

## Views

Every node with a result on disk is a view named after its function:

- an asset or sensor at its cached result. If its code or inputs changed since it ran, the view
  still shows its last result, and stderr says it is stale and how to refresh it;
- a task at its last successful result;
- a partitioned asset as one view over every key's latest result, with a `partition` column
  (`week=w1`).

When two nodes share a function name, both views are named by their full id instead, which you
quote in SQL (`select * from "pipelines/a.py:orders"`); stderr lists them. Pass files or
directories after the query to limit the views to those files
(`barca sql "select * from orders" pipeline.py`).

Only parquet and json results can be queried: DataFrames, Arrow tables and DuckDB relations are
stored as parquet; a dict or a list of dicts as json. A pickled result (any other Python object)
is not a view.

## Output

In a terminal the result is a table; piped, or with `--json`, it is one JSON document:

```
{"columns": ["region", "amount"], "rows": [{"region": "amer", "amount": 70.5}, ...],
 "total": 3, "truncated": false}
```

At most 100 rows are returned by default. `--limit N` and `--all` change that; when rows are cut
off, `truncated` is true, `total` counts every row, and `hint` says how to see more.

## Errors

All exit 2, with the fix as the remediation:

- a node with no result yet: names the command that produces it (`barca get never`);
- a pickled result: says it cannot be queried;
- an unknown view: lists the views there are;
- a SQL error: DuckDB's message, and the views.

## What it does not do

- It never runs a step, never imports your code, and records nothing: `barca history` is
  unchanged, and nothing is written under `.barca/`.
- It reads local artifacts only. With remote storage, a result whose artifact is only in the
  bucket is not a view yet.
- It needs `duckdb` in the Python environment barca uses (`pip install duckdb`).
- The query runs in a fresh in-memory DuckDB, not on `barca.duckdb_connection()`: extensions or
  macros your pipeline module sets up at import time are not loaded.

To recompute a result before querying it, use `--refresh` (`barca get revenue --refresh revenue`);
never delete files under `.barca/`.
