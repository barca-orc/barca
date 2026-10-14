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

Every node with a result on disk is a view named after its declared `name=`, or its
function when no explicit name is set:

- an asset or sensor at its cached result. If its code or inputs changed since it ran, the view
  still shows its last result, and stderr says it is stale and how to refresh it;
- a task at its last successful result;
- a partitioned asset as one view over its current keys' latest successful results,
  with a `partition` column
  (`week=w1`).

When two nodes share a name, both views are named by their full id instead, which you
quote in SQL (`select * from "pipelines/a.py:orders"`); stderr lists them. Pass files or
directories after the query to limit the views to those files
(`barca sql "select * from orders" pipeline.py`).

Only parquet and json results can be queried: DataFrames, Arrow tables and DuckDB relations are
stored as parquet; a dict or a list of dicts as json. A JSON list of scalars is
a view with one column named `json` (for example, `[1, 2]` becomes two rows). A pickled result (any other Python object)
is not a view.

Partition membership comes from the same full cache-aware prediction as a dry run,
including cached keys, all worker chunks, and derived keys whose source is cached.
Removing a key excludes its rows from the current view; its materialization and
artifact remain in history. Adding a key does not invent rows before it runs.
Current keys can still show stale successful values until refreshed.

If `partitions_from(...)` needs a source to run before its current keys are known,
SQL leaves that view unavailable and explains which source must materialize.
It does not guess membership from old results; unrelated views remain queryable.
An asset with zero current keys has no partition result view, because SQL cannot
infer a schema from absent current results.

## Remote storage

With optimistic shared state, SQL first synchronizes metadata into the selected
environment's local `.barca/` history, just like status. It does not create a new run.

With remote storage (`barca docs remote`) a result that exists only in the bucket is a view like
any other. When a query names such a view, barca downloads its artifact (for a partitioned asset,
the current keys' artifacts) into `.barca/sql-cache/` and queries the copy. stderr says what was downloaded:

```
barca: fetched 1 remote artifact (2.1 KB) into .barca/sql-cache/
```

- Only the views a query names are downloaded, and whole: DuckDB then reads the local copy, so
  the first query over a large result takes as long as its download. A view counts as named when
  its name appears in the query as a word, so a column or alias spelled like a remote view
  downloads that view too.
- A copy is reused while the object is unchanged. Each query asks the store for the size and
  version of every remote file it reads (one metadata request per file) and downloads again only
  when they differ, for example after `barca get revenue --refresh revenue`.
- The copies mirror the object's URI (`.barca/sql-cache/s3/my-bucket/...`) and are never removed
  by barca. Deleting `.barca/sql-cache/` is always safe: it holds nothing but copies.
- `show tables` lists remote views without downloading them.
- The download uses the filesystem, credentials and `[remote.storage_options.*]` the steps use.

## Output

In a terminal the result is a table; piped, or with `--json`, it is one JSON document:

```
{"columns": ["region", "amount"], "rows": [{"region": "amer", "amount": 70.5}, ...],
 "total": 3, "truncated": false}
```

At most 100 rows are returned by default. `--limit N` and `--all` change that; when rows are cut
off, `truncated` is true, `total` counts every row, and `hint` says how to see more.

## Errors

These exit 2, with the fix as the remediation:

- a node with no result yet: names the command that produces it (`barca get never`);
  when querying with `--env`, add the same `--env <name>` to that command (the hint
  currently omits it);
- a pickled result: says it cannot be queried;
- an unknown view: lists the views there are;
- a SQL error: DuckDB's message, and the views;
- a remote result whose driver is not installed: names the extra (`pip install 'barca[s3]'`).

A remote result that cannot be downloaded (rejected credentials, the network, an object that is
no longer in the bucket) exits 3 and carries the store's error.

## What it does not do

- It never executes a pipeline step or records a new run. Source parsing is static,
  but `partitions(<expression>)` may evaluate Python and import the pipeline module
  while loading its key list. With optimistic shared state, local history may change
  when synchronized; remote artifact copies are written under `.barca/sql-cache/`.
- SQL is not restricted to SELECT: an explicit `COPY ... TO 'file.csv'` writes that
  file using DuckDB. Queries run with the process's filesystem permissions.
- Install SQL support in barca's Python environment: `uv add 'barca[sql]'`.
- The query runs in a fresh in-memory DuckDB, not on `barca.duckdb_connection()`: extensions or
  macros your pipeline module sets up at import time are not loaded.

To recompute a result before querying it, use `--refresh` (`barca get revenue --refresh revenue`);
never delete files under `.barca/` (other than `.barca/sql-cache/`).
