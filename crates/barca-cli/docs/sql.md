# SQL: query cached results with DuckDB

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

With remote storage (`barca docs remote`) a result that exists only in the bucket is a view like
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
  the last 5 minutes is kept; `barca docs disk`). Deleting `.barca/sql-cache/` is
  always safe: it holds nothing but copies.
- `show tables` lists remote views without downloading them.
- The download uses the filesystem, credentials and `[remote.storage_options.*]` the steps use.

## barca's own tables

barca's own records are tables in a `barca` schema, so they never clash with a view's name:

| Table | One row per | Columns |
|---|---|---|
| `barca.runs` | run | the keys of `barca history --json` runs: `run_id`, `command`, `files`, `target`, `job`, `status`, `steps_total`, `steps_executed`, `steps_cached`, `attempts`, `started_at`, `finished_at`, `elapsed_seconds`, `triggered_by`, `scheduled_at`, `schedule_id`, `client_run_id`, `failure` (a struct of `node` and `error`, null when no step failed), `principal`, `trigger_reason`, `cancellation` (a struct of `by`, `principal`, `started`, `waiting_behind`, `same_as`, null unless a server cancelled the run) |
| `barca.materializations` | step result (successful or failed; cache hits are not results) | `node_id` (with `[key]` for a partition), `partition`, `run_id`, `run_hash`, `status`, `created_at`, `elapsed_seconds`, `cpu_seconds`, `max_rss_bytes`, `attempts`, `format`, `size_bytes`, `artifact`, `content_hash`, `error_type`, `error_message`, `rows` (rows of a tabular result, null when not counted) |
| `barca.nodes` | node | `barca status --json`, flattened: `id`, `name`, `kind`, `partitioned`, `inputs`, `env`, `state`, `reason`, `detail`, `health`, `health_reason`, `signals` (the kinds, a list), `measured`, `cause`, `root_id`, `root_kind`, `root_node`, `gap_seconds`, `last_check_at`, `last_check_outcome`, `refresh_schedule`, `next_tick`, `missed_ticks`, `stale_since`, `expected_fresh_at`, `recheck_at`, `no_estimate`, `run_hash`, `artifact`, `local`, `partitions_total`, `partitions_cached`, `partitions_missing`, `partitions_empty`, and `last_status`, `last_created_at`, `last_elapsed_seconds`, `last_run_hash`, `last_artifact`, `last_format`, `last_size_bytes`, `last_rows`, `last_content_hash`, `last_error` from `last_materialization` |
| `barca.schedules` | scheduled node | the `schedule` block of `barca status --json` with what names the node: `id`, `name`, `kind`, `cron`, `interval_seconds`, `targets` (the nodes one tick brings up to date, a list), `next_fire`, `last_fired`, `last_status`, `last_run_id`, `state`, `state_detail`, `late`, `late_since`, `paused`, `paused_until`, `failing`, `failures`, `max_failures`, `skip_reason`, `live`, `typical_queue_wait_seconds`, `typical_run_seconds`, `queue_bound` |
| `barca.ticks` | recorded tick of a schedule (`barca docs scheduling`, "The tick record") | `schedule_id`, `scheduled_at`, `action` (`fired`, `paused`, `skipped`, `failing`), `skipped_for`, `run_id`, `run_status`, `started_at`, `finished_at`, `queue_wait_seconds`, `rebuilt`, `up_to_date`, `failed`, `not_reached` (how many targets ended each way), `targets` (a list of `{node, outcome}`, null past a schedule's newest 20 fired ticks) |
| `barca.queue` | run in a server's run queue (`barca queue`) | `run_id`, `position` (1 = next; null once started), `state` (`queued`, `running`, `draining`), `queued_at`, `started_at`, `estimated_start_at`, `wait_seconds`, `command`, `target`, `triggered_by`, `schedule_id`, `scheduled_at` |

```bash
barca sql "select target, status, elapsed_seconds from barca.runs where status = 'failed' order by started_at desc limit 10"
barca sql "select node_id, avg(elapsed_seconds) as secs from barca.materializations group by 1 order by 2 desc"
barca sql "select name, state, reason from barca.nodes where state <> 'cached'"
barca sql "select health, health_reason, count(*) as nodes from barca.nodes group by all order by nodes desc"
barca sql "select root_kind, root_node, count(*) as held, sum(gap_seconds) as gap from barca.nodes where root_id is not null group by all order by held desc"
barca sql "select sum(gap_seconds) as gap, count(*) filter (where cause is not null and expected_fresh_at is null) as unbounded from barca.nodes"
barca sql "select r.target, m.node_id, m.size_bytes from barca.runs r join barca.materializations m using (run_id)"
barca sql "select node_id, created_at, rows from barca.materializations where rows = 0 order by created_at desc"
```

- `show tables` lists the views and these tables (`barca.runs`).
- A table is read when the query names it (its name as a word, like a remote view), from this
  machine's metadata DB, every row. Timestamps are text as `barca history --json` prints them
  (UTC, `YYYY-MM-DD HH:MM:SS`; `scheduled_at` is RFC 3339), so they sort; cast them to compare
  with times (`started_at::timestamp > now()::timestamp - interval 1 day`). `files`, `inputs` and
  `env` are lists.
- `barca.nodes` comes from the same prediction as the views (`barca status`); `local` is `null`
  without a server. `health`, `health_reason`, `signals` and `measured` are the values `barca
  status --json` prints (`barca docs status-fields`, "Health"); `measured` is `null` for a task
  or sensor. `last_check_at` (RFC 3339, UTC) and `last_check_outcome` are `last_check`, both
  `null` for a node no retained tick checked. `refresh_schedule`, `next_tick` (RFC 3339, UTC)
  and `missed_ticks` are `refresh_path` ("Refresh path"), all `null` for a node no tick will
  refresh. `stale_since`, `expected_fresh_at` and `recheck_at` (RFC 3339, UTC) are the node's
  times ("Times"); without a server only `stale_since` is ever set. `no_estimate` is why a
  node that is not fresh has no `expected_fresh_at`, `null` otherwise. `cause` is why a node
  is not fresh, `root_id`, `root_kind` and `root_node` its `root` (what explains it), all
  `null` for a fresh node, and `gap_seconds` its freshness gap, 0 for a fresh or undecided
  node ("Cause", "Root"): grouping by `root_id` is the root list of `barca status --rollup`.
- `barca.schedules` has one row per node with `freshness=Schedule(...)`, with the values `barca
  status --json` prints in its `schedule` block. `interval_seconds` is how far apart its slots
  are now and `targets` the nodes one of its ticks brings up to date, the scheduled node first.
  On this machine no scheduler runs, so `live` is false and the rest is what the metadata DB
  records of a `barca serve` of this project; from a server they are its scheduler's.
- `barca.ticks` is the tick record: what the scheduler of `barca serve` decided for each due
  tick and how the run it started ended, newest first. It holds at most 1000 ticks and 14 days
  of each schedule. `scheduled_at` is the slot (RFC 3339, UTC), `started_at` and `finished_at`
  the run's times (RFC 3339 with milliseconds). `unnest(targets)` gives one row per target. A
  project no server has scheduled has no ticks.
- `barca.queue` is a server's run queue as `barca queue --json` lists it: the started runs,
  then the waiting ones in order. Only a server has one. Without a server the table is empty
  and stderr says so (`barca: barca.queue is empty: the run queue is a server's ...`); a
  `--read-only` server lists the queue its metadata records, without `started_at`,
  `estimated_start_at` and `wait_seconds`.
- Every time in these tables is text. Cast one to compute with it:
  `now() - stale_since::timestamptz`, `finished_at::timestamptz - started_at::timestamptz`.
- The tables are a read-only copy for the query: writing to them changes nothing.

## Health queries

The tables answer how far a project is from fresh, why, and what holds it (`barca docs
status-fields`: "Cause", "Root", "Times"). Each query below runs as written, on this machine
and on a server (`--server`). The answers shown are over this project, written twenty minutes
before, with `inbox` and `orders` computed once, `clean`, `reviewed`, `ledger` and `totals`
never, and two recorded ticks of `inbox` that each waited 400 seconds for a run slot and
whose run failed before it reached `clean`:

```python
from barca import Always, Manual, Schedule, asset, sensor


@sensor(freshness=Schedule("*/5 * * * *"))
def inbox() -> int:
    return 1


@asset(inputs={"n": inbox}, freshness=Always)
def orders(n: int) -> dict:
    return {"n": n}


@asset(inputs={"o": orders}, freshness=Always)
def clean(o: dict) -> dict:
    return o


@asset(inputs={"c": clean}, freshness=Manual)
def reviewed(c: dict) -> dict:
    return c


@asset(freshness=Always)
def ledger() -> dict:
    return {}


@asset(inputs={"l": ledger}, freshness=Always)
def totals(l: dict) -> dict:
    return l
```

The freshness gap as one number, in node-seconds, with how many nodes are not fresh and how
many of those have no time at which they are expected fresh (the gap of those grows until
someone acts):

```sql
select sum(gap_seconds) as gap_seconds,
       count(*) filter (where cause is not null) as not_fresh,
       count(*) filter (where cause is not null and expected_fresh_at is null) as unbounded
from barca.nodes
```

```
gap_seconds  not_fresh  unbounded
4660         4          4
```

The gap by source file (a node's id starts with its file):

```sql
select split_part(id, ':', 1) as file,
       count(*) filter (where cause is not null) as not_fresh,
       sum(gap_seconds) as gap_seconds
from barca.nodes group by all order by gap_seconds desc
```

```
file         not_fresh  gap_seconds
pipeline.py  4          4660
```

The top roots: what explains the nodes that are not fresh, by how many each holds and their
gap. It is the root list of `barca status --rollup`:

```sql
select root_kind, root_node, count(*) as nodes, sum(gap_seconds) as gap_seconds
from barca.nodes where root_id is not null
group by all order by nodes desc, gap_seconds desc, root_node limit 10
```

```
root_kind    root_node             nodes  gap_seconds
no_schedule  pipeline.py:ledger    2      2330
never_run    pipeline.py:clean     1      1165
no_schedule  pipeline.py:reviewed  1      1165
```

Nodes nothing scheduled will refresh, oldest first:

```sql
select name, health, root_kind, root_node, stale_since
from barca.nodes where cause = 'no_refresh_path' order by stale_since, name
```

```
name      health     root_kind    root_node             stale_since
ledger    never_run  no_schedule  pipeline.py:ledger    2026-11-17T20:16:35Z
reviewed  never_run  no_schedule  pipeline.py:reviewed  2026-11-17T20:16:35Z
totals    never_run  no_schedule  pipeline.py:ledger    2026-11-17T20:16:35Z
```

Overdue nodes (ticks reached them and did not leave them fresh), with how many ticks missed
them and what the last one did:

```sql
select name, missed_ticks, refresh_schedule, last_check_outcome, last_check_at
from barca.nodes where cause = 'overdue' order by missed_ticks desc, name
```

```
name   missed_ticks  refresh_schedule   last_check_outcome  last_check_at
clean  2             pipeline.py:inbox  not_reached         2026-11-17T20:31:42Z
```

Schedules whose ticks wait for a run slot longer than they are apart (queue-bound), with
their typical wait: more run slots (`barca serve --max-runs`) or schedules spaced further
apart clear it.

```sql
select id, interval_seconds, typical_queue_wait_seconds, typical_run_seconds
from barca.schedules where queue_bound order by typical_queue_wait_seconds desc
```

```
id                 interval_seconds  typical_queue_wait_seconds  typical_run_seconds
pipeline.py:inbox  300               400
```

`typical_run_seconds` is null until a tick's run has succeeded.

What the ticks did with one node over time (the newest 20 fired ticks of a schedule keep
their targets). The quoted node id is a value. With a server, the query runs through
`POST /sql/query` and needs no shared artifact store.

```sql
select t.scheduled_at, target.outcome, t.run_status, t.queue_wait_seconds
from barca.ticks t, unnest(t.targets) as u(target)
where target.node = 'pipeline.py:clean' order by t.scheduled_at desc
```

```
scheduled_at          outcome      run_status  queue_wait_seconds
2026-11-17T20:25:00Z  not_reached  failed      400.0
2026-11-17T20:20:00Z  not_reached  failed      400.0
```

Runs per hour by what started them (`schedule`, `api`, `client`, `manual`):

```sql
select date_trunc('hour', started_at::timestamp) as hour, triggered_by, count(*) as runs
from barca.runs group by all order by hour desc, runs desc
```

```
hour                 triggered_by  runs
2026-11-17T20:00:00  manual        1
```

`barca.nodes` has no group column: the gap by group is the tree of `barca status --rollup`
(`GET /state/rollup`), where every level is the sum of its children.

## With a server

With a server configured (`[server] url` or `BARCA_SERVER`, `barca docs server-mode`), `barca sql`
asks the server which result each view reads and runs the query on this machine (a query over
barca's tables alone is answered by the server: "Queries the server answers" below):

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
  newest 10000 of the 12000 rows on the server at ...`). `barca.schedules`, `barca.ticks` and
  `barca.queue` are the server's too, with `live` true for the schedules its scheduler fires.
  For a query that runs here (one that also names a result view) their rows are read through
  the server's query route; a server that does not answer it (below) leaves them empty, and
  stderr says so.
- A result this machine computed whose post to the server is pending (`unsynced`, shown by
  `barca status`) is not mixed in: the view is the server's. With no result on the server, the
  error says this machine has one and names `--local`; with an older one, stderr says so.
- `--local` queries this machine's own records and artifacts, and touches nothing on the server.
  `--server <url>` resolves against that server for one command; a query over a result view
  still runs here.
- An unreachable server exits 3 with remediation `--local`. Nothing falls back to this machine's
  records.
- The server never runs SQL over results or files. DuckDB reads and writes files (`COPY ... TO`,
  `read_csv('/etc/...')`) and the API is unauthenticated by default, so a query over results run
  on the server would be remote file access to it. This is deliberate: a query that names a
  result view runs on the machine that typed it.

### Queries the server answers

A query that names only barca's tables (`barca.runs`, `barca.materializations`, `barca.nodes`,
`barca.schedules`, `barca.ticks`, `barca.queue`) needs no result and no file, so the server
answers it (`POST /sql/query`), and this machine needs no DuckDB for it:

```
$ barca sql --server http://barca.internal:8274 "select status, count(*) as runs from barca.runs group by all"
[barca] server http://barca.internal:8274 (--server)
status   runs
success  412
failed   3
```

The web UI asks through the same route. The output is the same document; what differs from a
query run here:

- The server loads up to 100000 rows of each table, newest first (a query run here reads at
  most 10000 of each from a server); stderr says when a table has more.
- A query has 5 seconds, waiting for the server's one query helper and loading the tables
  included. Past it the query is cancelled and the command exits 2: narrow the query.
- An answer holds at most 10000 rows and 4MB of them. `--all`, or a `--limit` above that, runs
  the query here; rows past 4MB are refused with exit 2 (select fewer or narrower columns,
  or aggregate). A query that needs more than the 1GB of memory the server gives a query is
  exit 2 too.
- The server admits four queries at once, one running and three waiting. One more exits 3
  (`429`): run it again in a moment.
- A query that calls `query(...)` or `query_table(...)`, which read a table by a name built at
  run time, has all of barca's tables loaded for it.
- The query also runs here when files are typed after it (the server's `barca.nodes` is every
  deployed file's), when it names a result view as well, and, with a configured server (no
  `--server`), when it names `barca.nodes`, whose `local` and `unsynced` describe this machine.
- A server from before the route, or one whose Python environment has no DuckDB (`uv add
  'barca[sql]'` there), does not answer: the query runs here, as it did before.

What the server accepts is one `SELECT` over those tables. Its database holds nothing else and
has DuckDB's external access switched off, so these are refused with exit 2, whatever the
server's files and credentials are:

- a result view (`select * from revenue`), or any relation that is not one of barca's tables;
- a file or a URL: `read_parquet`, `read_csv`, `read_json`, `read_text`, `glob`, a quoted path;
- any statement that is not a `SELECT`: `COPY`, `ATTACH`, `INSTALL`, `LOAD`, `PRAGMA`, `SET`,
  `CREATE`, `INSERT`, `UPDATE`, `DELETE`;
- more than one statement;
- a write inside a `SELECT` (`with gone as (delete from barca.runs returning *) select ...`):
  the query runs in a read-only transaction.

The tables are barca's records as every read route shows them. `barca.runs` and
`barca.materializations` carry the error text of failed steps (`error`, `error_message`,
`failure`), which holds whatever the pipeline's code put in an exception, and artifact paths:
whoever may send a query may read those, as with `GET /history` and `GET /runs/{run_id}`. Use
`--auth token` where that matters. An answer can also show DuckDB's version and platform.

`barca docs server-mode` ("The server's HTTP API") gives the route's request, answer and status
codes.

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

Python callers can use `barca.sql("select * from orders", "pipeline.py")`.
It returns a dictionary with `columns`, `rows` (row dictionaries), `total`,
`truncated`, and an optional `hint`. Pass `limit=20` or `all=True` to control
the row count, `env="dev"` to select an environment, and `server=True` or
`server="https://..."` to query a server. It queries cached results without
executing assets and raises `barca.BarcaError` on failure.

See also: `barca docs status`, `barca docs types`, `barca docs cache`, `barca docs server-mode`.
