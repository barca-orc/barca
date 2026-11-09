---
title: "barca sql"
description: "Query cached results with DuckDB (experimental)."
---

Query cached results with DuckDB. Every asset, sensor and task with a result on disk is a view
named after its declared `name=`, or its function when unset; a partitioned asset is one
view with a `partition` column. No step
runs, your code is not imported, and no run is recorded. Only parquet, CSV and json results are
views; a pickled result cannot be queried. Install SQL support with `uv add 'barca[sql]'`. With a remote store the
artifacts of the views a query names are downloaded into `.barca/sql-cache/` and reused.
barca's own records are tables in the `barca` schema: `barca.runs`, `barca.materializations` and
`barca.nodes`. With a server configured, the views and those tables come from the server's
ledger and the query runs on your machine; the server never runs SQL. Experimental.

```
$ barca sql "select region, sum(amount) as amount from orders group by 1 order by 1"
region  amount
amer    70.5
emea    10.0
```

| Flag | Default | Meaning |
|---|---|---|
| `-l`, `--limit <N>` | `100` | Maximum number of rows returned. |
| `--all` | off | Return every row. Cannot be combined with `--limit`. |
| `--json`, `--pretty` | by terminal | Force JSON or the table. |
| `--env <name>` | `default` | Query a named environment's results. |
| `--server [<url>]` | the configured server | Resolve the views from that server's ledger; the query runs here. |
| `--local` | off | Query this machine's own results and records, ignoring a configured server. |

See [barca sql](/reference/sql/) for views, errors and limits.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
