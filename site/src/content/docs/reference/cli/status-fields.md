---
title: "barca status: Fields and JSON"
description: "What barca status reports for each node beyond its state (last materialization, artifact shape, run timing, trigger), its JSON document, limits and disk usage."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/status-fields.md
---

<!-- Generated from crates/barca-cli/docs/status-fields.md (barca docs status-fields). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

What `barca status` reports for each node beyond its cache state: the last materialization, the
last changed bytes, the artifact's shape, run timing and attempts, and what started a run; the
JSON document; and the fine print on targets, limits, an artifact store and disk usage. Reading
status and finding problems is [barca status](/reference/cli/status/).

## Last materialization

`last_materialization` is the most recent execution recorded in the metadata DB, successful or
failed: `status`, `created_at` (UTC), `elapsed_seconds`, `run_hash`, `artifact`, `format`,
`size_bytes`, `rows`, and `error` for a failure. A cache hit is not an execution, so serving from
cache does not change it. It is `null` when the node never ran.

`rows` is how many rows a successful tabular result holds, counted by the worker when the step
wrote it ("Row counts" below); `null` when it was not counted.

## Row counts

The worker records the rows of a result when that is cheap to know exactly, and never guesses:

| result | `rows` |
|---|---|
| a pandas or polars DataFrame, a pyarrow Table, in any format (parquet, CSV, pickle) | its length |
| a polars LazyFrame or a DuckDB relation written to parquet, and each key of a batched call | the row count in the written file's footer (pyarrow, else duckdb, else polars reads it; only the footer is read) |
| a LazyFrame or relation written to CSV, anything that is not a frame (json, pickle of other values), a failed attempt | `null` |

Results recorded by an older barca, and results a server's ledger received from a client, have
`null`. The count is in `last_materialization.rows` here, in each step of `barca history RUN_ID
--json` (`steps[].rows`), in `barca stats --json` (`recent_runs[].rows`; for a partitioned
node the rows of the run's successful keys added up, `null` unless every one was counted, and
`recent_runs[].empty_keys`), in `barca sql` (`materializations.rows`, `nodes.last_rows`,
`nodes.partitions_empty`), and in the server's routes ([Server Mode: a Shared Ledger and --server: Data health](/reference/server-mode/#data-health)).
What the counts say about a node's data, and how many assets have one at all, is in "Data
signals and coverage" below.
A partitioned node's `partitions.empty` counts its keys whose latest successful result wrote no
rows (absent when none did).
For a partitioned node it is one key's row from the latest run, the first key in sort order
(`partition` names it), so two identical runs report the same key whatever order the keys
finished in.

## Health

Every node has a `health`: one word that says whether something needs doing about it, beside
its cache state and never in place of it. `barca status --json`, the server's `GET /nodes` and
`GET /state` and `barca.nodes` in `barca sql` carry the same word, computed once in barca, and
the web UI shows it. The words, most severe first:

| `health` | Printed as | Meaning |
|---|---|---|
| `failing` | failing | its own last attempt failed |
| `blocked` | blocked | something upstream keeps it from being brought up to date |
| `suspect` | suspect data | its last run succeeded but its recorded row counts look wrong |
| `behind` | behind | out of date against its inputs, partly computed, out of sync, or late |
| `undecided` | undecided | barca cannot decide yet (a cutoff or sensor upstream has not reported) |
| `never_run` | never run | nothing recorded |
| `ok` | ok | fresh, or a task or sensor whose last run succeeded |

`health_reason` names the rule that decided it. The first that applies wins:

| `health_reason` | `health` | When |
|---|---|---|
| `last_attempt_failed` | `failing` | `last_materialization.status` is `failed` |
| `keys_failed` | `failing` | `partitions.failed` is above 0 and the last materialization did not fail |
| `upstream_failing` | `blocked` | a node upstream is failing: an input, or an input of an input that is itself neither failing, `cached` nor `always_runs`. A pinned asset ([barca pin and unpin](/reference/cli/pin/)) is never blocked and a failure does not pass it |
| `cache_blocked` | `blocked` | `cache.state` is `blocked`, or `cache.reason` is `upstream_failed` |
| `empty` | `suspect` | it has an `empty` signal |
| `rows_changed` | `suspect` | it has a `rows_changed` signal |
| `behind_input` | `behind` | it has a `behind_input` signal |
| `schedule_late` | `behind` | its schedule missed a slot more than a minute ago (half the interval for a schedule more frequent than every two minutes) and is not paused |
| `stale`, `partial`, `unsynced` | `behind` | `cache.state` is that state |
| `checked_up_to_date` | `ok` | `cache.state` is `unknown`, and the newest tick that checked the node found it up to date or rebuilt it, with nothing seen to have changed since ("Last check" below) |
| `unknown` | `undecided` | `cache.state` is `unknown` |
| `never_run` | `never_run` | `cache.state` is `never_run` |
| `no_successful_run` | `never_run` | a task, sensor or `always` asset with no recorded run |
| `last_run_succeeded` | `ok` | a task, sensor or `always` asset whose last run did not fail |
| `cached` | `ok` | `cache.state` is `cached` |

A partitioned node has one health. A run working on a node does not change it. A node below
a failure is `blocked` even when its own state is `unknown` (it waits on a cutoff): the walk
passes through such nodes. `barca status` scoped to targets reads the targets' upstream cones,
so a failure above them is seen.

The table prints the words of the nodes it lists under the cache counts, most severe first:
`Health: 1 failing, 1 blocked, 2 ok`. With `--state` or `--problems` the line reads `Health
(nodes listed)`.

```bash
barca status --json | jq -r '.nodes[] | select(.health != "ok") | [.health, .health_reason, .id] | @tsv'
barca sql "select health, count(*) from barca.nodes group by all order by 2 desc"
```

Health is experimental ([CLI Contract](/reference/cli-contract/)): later releases add why a node that is not
fresh is so and when it is expected fresh.

## Last check

`last_check` is what the newest finished tick that had the node as a target did with it
([Scheduling With Cron](/scheduling/)): `{"at": "2026-11-17T15:00:05Z", "outcome": "up_to_date", "schedule":
"pipeline.py:inbox"}`, or `null` when no retained tick did. `at` is when that tick's run ended
(RFC 3339, UTC), `schedule` the scheduled node whose tick it was, and `outcome` one of:

| `outcome` | The tick's run |
|---|---|
| `rebuilt` | executed its step, which succeeded. For a sensor that means it was polled, whatever it returned; for a task, that it fired |
| `up_to_date` | found a cached result for its code and inputs: a cache hit |
| `failed` | executed its step, which failed |
| `not_reached` | ended before it decided the node: a failure upstream, a cancellation, a run that never started |

It is a record, shown whatever its age. Ticks are recorded by a scheduling `barca serve`
(`schedule_ticks` in the metadata database); `barca status` on that machine and a
`--read-only` server read the same record. Each schedule keeps the outcomes of its newest 20
fired ticks, and with several schedules reaching a node the newest tick wins.

**When a check decides health.** `barca status` and `GET /state` predict what recomputing
everything above a node would do, without running anything. Where the answer depends on what
only a run produces, the node's state is `unknown` ([barca status: Cache state](/reference/cli/status/#cache-state)): below
an `@asset(cutoff=True)` that would run, the bytes it will write. A tick does run, and reads a
`Manual` or pinned node above as it is (the prediction recomputes it), so it can find the node
up to date every five minutes while its state stays `unknown`. Such a node is `ok` with
`health_reason: "checked_up_to_date"` when all of this holds:

- its state is `unknown` and no earlier rule applies (it is not failing, blocked, suspect, or
  the scheduled node of a late schedule);
- its last check's `outcome` is `up_to_date` or `rebuilt`: a newer `failed` or `not_reached`
  check is the last check, and decides nothing;
- the check has not expired. It stands for one interval of its schedule after its run ended,
  plus the time that tick took from its slot to its end, plus the grace a late schedule gets
  (a minute; half the interval for a schedule more frequent than every two minutes): by then
  the next tick's check is due. A tick found up to date at 15:00:05 by a five-minute schedule
  stands until 15:06:10. The check of a schedule that is no longer in the project never stands;
- the node's definition (its code, counted decorator arguments and helpers) is the one its
  newest successful result recorded: an edit since is seen;
- no node above it has a materialization recorded after that tick's run ended, other than by
  a tick's run still in flight (whose own check is on its way).

Otherwise the node keeps the health its state gives (`undecided`). A node whose check stands
gets no `behind_input` signal: the tick read its inputs' newest results and found it up to
date. Not seen until the next tick or the expiry: an edit to a node above it (which shows on
that node), and a new value of an environment variable it declares.

The table adds the check to the `WHY` of a node whose state is `unknown`: `cutoff_pending;
checked 3m ago, up to date`. `barca.nodes` has it as `last_check_at` and `last_check_outcome`.

```bash
barca status --json | jq -r '.nodes[] | select(.last_check) | [.id, .last_check.outcome, .last_check.at] | @tsv'
barca sql "select name, health, last_check_outcome, last_check_at from barca.nodes where health_reason = 'checked_up_to_date'"
```

`last_check` and `checked_up_to_date` are experimental with the rest of health.

## Data signals and coverage

`signals` lists what a node's recorded results say about its data, from facts the metadata DB
holds (row counts workers recorded, when each result was written), never by reading an
artifact. Only assets have signals, in this order:

| `kind` | When | Fields |
|---|---|---|
| `empty` | its latest successful result wrote no rows: for a partitioned node, some keys' latest successful results | `empty_keys` of `keys` (the keys whose latest result recorded a count; 1 of 1 without keys), `previous_empty_keys`: how many were empty before its newest run, `null` when no key had a counted result before it |
| `rows_changed` | a latest successful result's row count differs from the one before it, same node and key, by more than half of it (any rows after an empty result count) | `rows` and `previous_rows` of `key` (`null` without keys), the key that moved the most relative to before; `changed_keys`: how many moved |
| `behind_input` | its cache state is `unknown` (nothing else says it is behind, typically a `collect()` fan-in below a pending cutoff), no tick's check of it stands ("Last check" above) and its latest successful result is older than the newest successful result of an asset it reads, from another run | `input` (the input with the newest result), `input_success_at`, `last_success_at` (UTC `YYYY-MM-DD HH:MM:SS`) |

A signal stays until the next successful result of the node (or key) replaces the one it is
about; there is no acknowledgement yet. A `stale`, `partial` or `blocked` node gets no
`behind_input`: its state already says it will run.

An empty `signals` does not mean the data is fine. A signal can only be raised for a result
whose rows were counted ("Row counts" above), so each asset says whether it was: `measured` is
`true` when its latest successful result has a recorded row count (for a partitioned asset,
any key's), `false` when it has none, and `null` for a task or sensor. An asset with
`measured: false` is not measured, never ok for data.

`data_coverage`, beside `summary`, counts this for every node in scope (a filter does not
change it): `assets`, how many of them are `measured`, and how many of those are `suspect`
(an `empty` or `rows_changed` signal). The table prints it as its last line but one, `Data: 7
of 556 assets measured`, with `, 2 suspect` when some are, so "no suspect data" is never said
without what it is out of.

```bash
barca status --json | jq '.data_coverage'
barca status --json | jq -r '.nodes[] | select(.signals != []) | [.id, (.signals | map(.kind) | join(","))] | @tsv'
barca sql "select count(*) filter (where measured) as measured, count(*) as assets from barca.nodes where kind = 'asset'"
```

## Last changed bytes

`last_bytes_change` is the most recent recompute of the node that produced different bytes for
the same code and inputs (the same run hash): `{node_id, run_hash, content_hash, created_at}`,
with the exact node id (a partition key included). It is absent when that never happened. The
table prints one line per such node under the rows:
`total: different bytes for the same code and inputs at 2026-10-02 11:56:43 UTC (pipeline.py:total)`.
It means the function is not deterministic ([The Cache](/reference/cache/)).

## Artifact shape

`shape` describes the artifact of `last_materialization` when that run succeeded (`null`
otherwise). It is read from the file alone by a small Python helper; your modules are never
imported.

| format | shape |
|---|---|
| parquet | `type: "table"`, `rows`, `columns: [{name, type}]` (arrow types) from the file footer |
| json list | `type: "list"`, `rows`; for a list of objects also `columns` with the JSON types seen (`str \| null`) |
| json object | `type: "dict"`, `keys` (first 100; `key_count` when there are more) |
| other json | `type` (`int`, `str`, ...) |
| pickle | `type` only, e.g. `myproject.Model`, read from the pickle opcodes without unpickling |

`--sample N` adds `sample`: the first N rows (parquet, json list), the first N entries (json
object), or the value (other json). It is off by default to keep output small. Pickles are never
sampled: loading one would import and run code.

When the shape cannot be read, `shape` has a `note` instead of `rows`/`columns`: parquet without
pyarrow installed (`pip install 'barca[parquet]'`), a file that no longer exists, or a remote
artifact that is too large or cannot be reached (below).

### Remote artifacts

With remote storage ([Remote Storage](/reference/remote-storage/)) the shape is read from the bucket, through the same
filesystem, credentials and `[remote.storage_options.*]` the steps use. There is no flag for it:

```bash
barca status total --json --sample 2    # rows, columns and 2 sample rows, read from the bucket
```

- parquet: `rows` and `columns` come from the file footer by ranged requests; the object is not
  downloaded. `--sample N` also reads the first row group, not the whole file. Measured on a
  160 MB file of 20 row groups in S3-compatible storage: 64 KB read for the shape, 8 to 9 MB with
  `--sample 5`.
- json and pickle have to be downloaded to be described, so only objects up to 16 MB are. A
  larger one has `"note": "remote json artifact too large to inspect: 25.1 MB (limit 16 MB)"`.
  `barca sql` can still query a large json result.

Each node with a result costs one or two requests, and up to 8 artifacts are read at a time.
`barca status` with no target reads every node; name a target to read only its upstream cone.

A store that cannot be read never fails the command. A missing driver
(`pip install 'barca[s3]'`), rejected credentials or a network error is the `note` of each
shape, the rest of the status is complete, and the exit code is 0. After the first such failure
the artifacts not yet read report it without another attempt, so an unreachable bucket costs one
wait for the driver's retries (about 10 seconds for an S3 endpoint that refuses connections),
not one per node.

## Partitioned assets

A partitioned asset is one node with a `partitions` summary: `total`, `cached`, `missing` and up
to 20 `missing_keys`, and `empty` when some keys' latest successful result wrote no rows (a
recorded count of 0; keys no longer in the key set count while their result is recorded). Its state is `cached` when every key is, `partial` when some are, and
`stale`/`never_run` when none are. `last_materialization` is one key of the latest run (the first in
sort order), named in its `partition` field (for example `k=a`), and `shape` describes that one key's artifact.

## JSON

```json
{
  "target": "total",
  "targets": ["total"],
  "nodes": [
    {
      "id": "pipeline.py:orders",
      "name": "orders",
      "kind": "asset",
      "inputs": [],
      "partitioned": false,
      "cache": {"state": "cached", "reason": "materialized", "detail": "...", "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet"},
      "health": "ok",
      "health_reason": "cached",
      "signals": [],
      "measured": true,
      "last_check": null,
      "last_materialization": {"status": "success", "created_at": "2026-10-02 11:56:43", "elapsed_seconds": 0.54, "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet", "format": "parquet", "size_bytes": 2182, "rows": 3},
      "shape": {"type": "table", "rows": 3, "columns": [{"name": "id", "type": "int64"}, {"name": "amount", "type": "double"}]}
    }
  ],
  "summary": {"cached": 2, "unsynced": 0, "stale": 0, "never_run": 0, "partial": 0, "blocked": 0, "unknown": 0, "always_runs": 0},
  "data_coverage": {"assets": 2, "measured": 1, "suspect": 0}
}
```

A scheduled node adds `"schedule": {"cron": "0 6 * * *", "next_fire": "2026-10-23T06:00:00Z",
"last_fired": null, "last_status": null, "last_run_id": null, "late": false, "late_since": null,
"failures": 0, "max_failures": 5, "failing": false,
"paused": false, "paused_until": null, "skip_reason": null, "live": false}`, and `--state` or
`--problems` add `"filter"` beside `summary`. `health`, `health_reason`, `signals`, `measured`,
`last_check` and `data_coverage` are in "Health", "Last check" and "Data signals and coverage"
above; here `total` is a dict, which has no row count, so one of the two assets is measured.

Keys inside a `sample` row are printed in alphabetical order; `columns` keeps the file's column
order. `--env <name>` reads another environment's state ([The Cache](/reference/cache/)).

An explicit `@asset(name="orders_clean")` uses `orders_clean` in status and as its SQL
view name, matching `list`, `get` and `stats`. Inspection reads the existing artifact
without importing or rerunning its producer.

## Run timing and attempts

`get --json` and `run --json` add `elapsed_seconds` and `attempts` to executed steps,
including failed steps. Cached steps and dry runs omit them. Elapsed time sums active
worker time across attempts: module import, input loading, the call and artifact
serialization; retry backoff is excluded. After a worker disconnect, elapsed time falls
back to the coordinator's observed in-flight time. For a partitioned step, elapsed time
sums its executed keys (and can exceed run wall time); attempts is the maximum for any
one key, not the number of keys.

```bash
barca get total pipeline.py --json --fields id,status,elapsed_seconds,attempts,import_seconds
barca history --json --fields run_id,status,attempts
barca stats total pipeline.py --json
```

Under `--agent` (and in every run `barca serve` starts), a worker prints
`[barca] importing <module>` on stderr before it imports a module; without `--agent` it prints
nothing of its own, and the import time is in the run JSON. Imports longer than one second add `import_seconds` to the step report, summed across
attempts and executed keys. This time is included in `elapsed_seconds`; it is not an
extra charge. A reused module adds no import time. Framework import diagnostics remain
visible under `--json`; task prints remain accessible through `barca logs`.

`--agent` completion and final failure lines append `attempts=N` when N exceeds one.
Completion lines retain their existing final-attempt duration. `history --json` adds
`attempts`, the total attempts in recorded materialization rows (zero for an all-cached
run; upload-failure rows from earlier versions retain their transfer-attempt counts). `stats` shows
attempts per recorded materialization and its stored final-attempt duration. For a partitioned
node (`barca stats scored`) an execution is a run: `total_runs` counts runs, `elapsed_seconds` and
`cpu_seconds` sum its keys, and each `recent_runs` entry adds `keys`, `failed_keys` and
`empty_keys` (keys that wrote no rows).
Timeout errors name the step id, including its partition key.

Older read-only snapshots without attempt metadata report a null history attempt total.

## What started a run

`barca history --json` rows carry `triggered_by`: `manual` for a command typed on a machine
(`barca get`, `barca run`), `api` for a run started over HTTP on `barca serve`, `schedule` for a
cron tick, and `client` for a run a laptop posted to a server's ledger. A scheduled run also
carries the tick it was started for in `scheduled_at` (RFC 3339, UTC) and the scheduled node in
`schedule_id`; both are `null` otherwise. Runs recorded before barca recorded this have
`triggered_by: null`.

```bash
barca history --json --fields run_id,triggered_by,scheduled_at,schedule_id
```

## Details

Status executes no pipeline step and records no new run, and it does not create `.barca/`.
Source parsing is static, but `partitions(<expression>)` may evaluate Python and import its
module while resolving keys. Times in the table (`LAST RUN`, like `STARTED` in `barca history`
and `CREATED` in `barca stats`) are UTC and say so; the JSON has the same UTC values. Like every inspection command, the result goes to stdout (a table in a terminal, JSON
when piped; `--json` / `--pretty` override) and errors to stderr; an unknown target is a usage
error (exit 2) that ends with ``Run `barca list <files>` to see available assets and tasks.``,
the same remediation as `get` and `run`. A target is one name or several, comma-separated with no
spaces (`a,b`), parsed exactly as `get` and `run` parse them; the JSON names them in `targets`
(and the single one in `target`).

With an artifact store ([Remote Storage](/reference/remote-storage/)), status also asks the store about every cached
unpartitioned node, with one `python -m barca._state stat` for all of them, and reports a node
whose copies do not agree as `unsynced` ([barca status: Cache state](/reference/cli/status/#cache-state)). Nothing is downloaded or uploaded.

Status reads the metadata DB as it is at that moment, and a run records each step as it
finishes. So while a `barca get` is running, status in another terminal already shows the steps
it has finished as `cached` (a partitioned asset as `partial`, with counts); see `barca docs
layout`, "While a run is going, and after one is killed".

Like `list`, status returns every node in JSON unless `--limit N` is explicit. The human
table shows at most 100 nodes unless you pass `--limit N` or `--all`. The
`summary` still counts every node, and the JSON adds `total` and `truncated` (with a `hint` when
truncated). Each node also lists `env`, the environment variables it declares with `env=[...]`
([Assets, Sensors and Inputs](/reference/assets/)), and, when its code reads variables it does not declare, `undeclared_env`
(experimental; [The Cache: Environment variables](/reference/cache/#environment-variables)). `--fields` keeps only the named keys
on each node.

## Disk usage and inspection cost

The experimental `disk_usage` summary reports project-wide storage across all environments,
even when node selection or filters narrow the graph: `budget_bytes`, `backup_bytes`,
physical `size_bytes` and `limit_bytes`. Recognized local metadata backups are outside the
cache budget and included in the physical total. Human output shows the same distinction.
Budgeted bytes use the last maintained GC measurement; status does not walk artifacts or
refresh that measurement. Backup directories are checked directly with a bounded survey
(at most 4096 directory entries across environments and backup files). Unmeasured counts,
including an incomplete backup survey, are `null` and displayed as unmeasured. Use `barca
doctor` or `barca gc --dry-run` for a full measurement. Server status reports server storage.
The UI's `/state` node array does no additional disk-accounting work.

Status inspection reuses bounded evidence instead of re-reading unchanged artifact bytes.
Local file identities include device, inode, size, mtime and ctime; changed or missing local
copies and directory-store objects are checked again immediately. Cloud availability and
remote shape observations can be up to five minutes old; cache hits do not extend that
window. Local materialization changes, confirmed transfers, credential changes and Python
import-environment changes invalidate the affected evidence. Inspection prefers an already
verified local copy for shape. This is availability
and shape evidence, not a new freshness proof. Required reads, transfers and server ledger
publication keep their direct verification paths.

Local inspection evidence is reusable only after the file's change timestamp is at least
one second old. Recently changed files are read directly without retaining evidence, so
inode reuse within a filesystem timestamp tick cannot preserve an old observation.

The disposable inspection cache is limited to 8 MiB and 8192 entries per environment, with
at most 64 KiB per value. Corruption, eviction or an unwritable cache causes another check;
a busy cache-write lock is skipped. Cold inspection still hashes selected local copies and
plain-directory store objects once, so its cost grows with their bytes. These limits do not
resolve the broader cold/per-key planning and inspection gaps (#655 and #656).

See also: [barca status](/reference/cli/status/), [The Cache](/reference/cache/), [Remote Storage](/reference/remote-storage/), [Disk Use](/reference/disk/).

---

*The same text is in your terminal: `barca docs status-fields`.*
