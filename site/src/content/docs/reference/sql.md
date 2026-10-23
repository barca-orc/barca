---
title: "SQL"
description: "barca sql: query cached results with DuckDB while debugging, without writing a step."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/sql.md
---

<!-- Generated from crates/barca-cli/docs/sql.md (barca docs sql). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

:::note
This page is the manual topic `barca docs sql`: the CLI prints the same text.
:::

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

Only parquet, CSV and json results can be queried: DataFrames, Arrow tables and DuckDB relations are
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

With remote storage ([`barca docs remote`](/reference/remote-storage/)) a result that exists only in the bucket is a view like
any other. When a query names such a view, barca downloads its artifact (for a partitioned asset,
the current keys' artifacts) into `.barca/sql-cache/` and queries the copy. stderr says what was downloaded:

```
barca: fetched 1 remote artifact (2.2 KB) into .barca/sql-cache/
```

- Only the views a query names are downloaded, and whole: DuckDB then reads the local copy, so
  the first query over a large result takes as long as its download. A view counts as named when
  its name appears in the query as a word, so a column or alias spelled like a remote view
  downloads that view too.
- A copy is reused while the object is unchanged. Each query asks the store for the size and
  version of every remote file it reads (one metadata request per file) and downloads again only
  when they differ, for example after `barca get revenue --refresh revenue`.
- The copies mirror the object's URI (`.barca/sql-cache/s3/my-bucket/...`). They count toward the
  cache size limit and are the first thing it evicts, least recently used first (a copy used in
  the last 5 minutes is kept; `barca docs cache`, "Disk use"). Deleting `.barca/sql-cache/` is
  always safe: it holds nothing but copies.
- `show tables` lists remote views without downloading them.
- The download uses the filesystem, credentials and `[remote.storage_options.*]` the steps use.

## barca's own tables

barca's own records are tables in a `barca` schema, so they never clash with a view's name:

| Table | One row per | Columns |
|---|---|---|
| `barca.runs` | run | the keys of `barca history --json` runs: `run_id`, `command`, `files`, `target`, `job`, `status`, `steps_total`, `steps_executed`, `steps_cached`, `attempts`, `started_at`, `finished_at`, `elapsed_seconds`, `triggered_by`, `scheduled_at`, `schedule_id`, `client_run_id`, `failure` (a struct of `node` and `error`, null when no step failed) |
| `barca.materializations` | step result (successful or failed; cache hits are not results) | `node_id` (with `[key]` for a partition), `partition`, `run_id`, `run_hash`, `status`, `created_at`, `elapsed_seconds`, `cpu_seconds`, `max_rss_bytes`, `attempts`, `format`, `size_bytes`, `artifact`, `content_hash`, `error_type`, `error_message` |
| `barca.nodes` | node | `barca status --json`, flattened: `id`, `name`, `kind`, `partitioned`, `inputs`, `env`, `state`, `reason`, `detail`, `run_hash`, `artifact`, `local`, `partitions_total`, `partitions_cached`, `partitions_missing`, and `last_status`, `last_created_at`, `last_elapsed_seconds`, `last_run_hash`, `last_artifact`, `last_format`, `last_size_bytes`, `last_content_hash`, `last_error` from `last_materialization` |

```bash
barca sql "select target, status, elapsed_seconds from barca.runs where status = 'failed' order by started_at desc limit 10"
barca sql "select node_id, avg(elapsed_seconds) as secs from barca.materializations group by 1 order by 2 desc"
barca sql "select name, state, reason from barca.nodes where state <> 'cached'"
barca sql "select r.target, m.node_id, m.size_bytes from barca.runs r join barca.materializations m using (run_id)"
```

- `show tables` lists the views and these tables (`barca.runs`).
- A table is read when the query names it (its name as a word, like a remote view), from this
  machine's metadata DB, every row. Timestamps are text as `barca history --json` prints them
  (UTC, `YYYY-MM-DD HH:MM:SS`; `scheduled_at` is RFC 3339), so they sort; cast them to compare
  with times (`started_at::timestamp > now()::timestamp - interval 1 day`). `files`, `inputs` and
  `env` are lists.
- `barca.nodes` comes from the same prediction as the views (`barca status`); `local` is `null`
  without a server.
- The tables are a read-only copy for the query: writing to them changes nothing.

## With a server

With a server configured (`[server] url` or `BARCA_SERVER`, [`barca docs server-mode`](/reference/server-mode/)), `barca sql`
asks the server which result each view reads and runs the query on this machine:

```
$ barca sql "select * from revenue"
[barca] server http://barca.internal:8274 ([server] url in barca.toml)
region  amount
amer    70.5
emea    12.0
```

- The server resolves the views from its ledger (`GET /sql/views`): which result is current for
  each node, its store path, format and digest, and for a partitioned asset which keys are
  current. The names, the stale results and their note, and the partition rules are those of a
  local `barca sql`; they follow the server's deployed code.
- The artifacts are read from the store this project shares with the server, as above: a remote
  store's are downloaded into `.barca/sql-cache/` when the query names their view. The project's
  `[remote]` must be the server's store (exit 2 otherwise, as for `barca get`).
- barca's tables are the server's: `barca.runs` is the team's history, `barca.materializations`
  every recorded step result (`GET /history`, `GET /materializations`), `barca.nodes` what `barca
  status` shows, with `local` saying whether this machine has a copy. At most 10000 rows of each
  table are read, newest first; stderr says when a table has more (`barca: barca.runs holds the
  newest 10000 of the 12000 rows on the server at ...`).
- A result this machine computed whose post to the server is pending (`unsynced`, shown by
  `barca status`) is not mixed in: the view is the server's. With no result on the server, the
  error says this machine has one and names `--local`; with an older one, stderr says so.
- `--local` queries this machine's own records and artifacts, and touches nothing on the server.
  `--server <url>` resolves against that server for one command; the query still runs here.
- An unreachable server exits 3 with remediation `--local`. Nothing falls back to this machine's
  records.
- The server never runs SQL. DuckDB reads and writes files (`COPY ... TO`, `read_csv('/etc/...')`)
  and the API has no authentication, so a query run on the server would be remote file access to
  it. This is deliberate: every query runs on the machine that typed it.

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

- a node with no result yet (with a server: none in its ledger): names the command that produces
  it (`barca get never`);
  when querying with `--env`, add the same `--env <name>` to that command (the hint
  currently omits it);
- a pickled result: says it cannot be queried;
- an unknown view: lists the views there are;
- a SQL error: DuckDB's message, and the views;
- a remote result whose driver is not installed: names the extra (`pip install 'barca[s3]'`).

A remote result that cannot be downloaded (rejected credentials, the network, an object that is
no longer in the bucket) exits 3 and carries the store's error. A server that cannot be reached exits 3 and names
`--local`.

## What it does not do

- It never executes a pipeline step or records a new run. Source parsing is static,
  but `partitions(<expression>)` may evaluate Python and import the pipeline module
  while loading its key list. Remote artifact copies are written under `.barca/sql-cache/`. The one
  write to the metadata DB is the read time of the results the query names (`last_read_at`),
  which the cache size limit uses to evict results nobody reads first.
- SQL is not restricted to SELECT: an explicit `COPY ... TO 'file.csv'` writes that
  file using DuckDB. Queries run with the process's filesystem permissions.
- Install SQL support in barca's Python environment: `uv add 'barca[sql]'`.
- The query runs in a fresh in-memory DuckDB, not on `barca.duckdb_connection()`: extensions or
  macros your pipeline module sets up at import time are not loaded.

To recompute a result before querying it, use `--refresh` (`barca get revenue --refresh revenue`);
never delete files under `.barca/` (other than `.barca/sql-cache/`).

See also: `barca docs status`, `barca docs types`, `barca docs cache`, [`barca docs server-mode`](/reference/server-mode/).
