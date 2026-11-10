# barca status: fields, JSON and fine print

What `barca status` reports for each node beyond its cache state: the last materialization, the
last changed bytes, the artifact's shape, run timing and attempts, and what started a run; the
JSON document; and the fine print on targets, limits, an artifact store and disk usage. Reading
status and finding problems is `barca docs status`.

## Last materialization

`last_materialization` is the most recent execution recorded in the metadata DB, successful or
failed: `status`, `created_at` (UTC), `elapsed_seconds`, `run_hash`, `artifact`, `format`,
`size_bytes`, and `error` for a failure. A cache hit is not an execution, so serving from cache
does not change it. It is `null` when the node never ran.
For a partitioned node it is one key's row from the latest run, the first key in sort order
(`partition` names it), so two identical runs report the same key whatever order the keys
finished in.

## Last changed bytes

`last_bytes_change` is the most recent recompute of the node that produced different bytes for
the same code and inputs (the same run hash): `{node_id, run_hash, content_hash, created_at}`,
with the exact node id (a partition key included). It is absent when that never happened. The
table prints one line per such node under the rows:
`total: different bytes for the same code and inputs at 2026-10-02 11:56:43 UTC (pipeline.py:total)`.
It means the function is not deterministic (`barca docs cache`).

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

With remote storage (`barca docs remote`) the shape is read from the bucket, through the same
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
to 20 `missing_keys`. Its state is `cached` when every key is, `partial` when some are, and
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
      "last_materialization": {"status": "success", "created_at": "2026-10-02 11:56:43", "elapsed_seconds": 0.54, "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet", "format": "parquet", "size_bytes": 2182},
      "shape": {"type": "table", "rows": 3, "columns": [{"name": "id", "type": "int64"}, {"name": "amount", "type": "double"}]}
    }
  ],
  "summary": {"cached": 2, "unsynced": 0, "stale": 0, "never_run": 0, "partial": 0, "blocked": 0, "unknown": 0, "always_runs": 0}
}
```

A scheduled node adds `"schedule": {"cron": "0 6 * * *", "next_fire": "2026-10-23T06:00:00Z",
"last_fired": null, "last_status": null, "failures": 0, "max_failures": 5, "failing": false,
"paused": false, "paused_until": null, "skip_reason": null, "live": false}`, and `--state` or
`--problems` add `"filter"` beside `summary`.

Keys inside a `sample` row are printed in alphabetical order; `columns` keeps the file's column
order. `--env <name>` reads another environment's state (`barca docs cache`).

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
`cpu_seconds` sum its keys, and each `recent_runs` entry adds `keys` and `failed_keys`.
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

With an artifact store (`barca docs remote`), status also asks the store about every cached
unpartitioned node, with one `python -m barca._state stat` for all of them, and reports a node
whose copies do not agree as `unsynced` (`barca docs status`, "Cache state"). Nothing is downloaded or uploaded.

Status reads the metadata DB as it is at that moment, and a run records each step as it
finishes. So while a `barca get` is running, status in another terminal already shows the steps
it has finished as `cached` (a partitioned asset as `partial`, with counts); see `barca docs
layout`, "While a run is going, and after one is killed".

Like `list`, status returns every node in JSON unless `--limit N` is explicit. The human
table shows at most 100 nodes unless you pass `--limit N` or `--all`. The
`summary` still counts every node, and the JSON adds `total` and `truncated` (with a `hint` when
truncated). Each node also lists `env`, the environment variables it declares with `env=[...]`
(`barca docs assets`), and, when its code reads variables it does not declare, `undeclared_env`
(experimental; `barca docs cache`, "Environment variables"). `--fields` keeps only the named keys
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

See also: `barca docs status`, `barca docs cache`, `barca docs remote`, `barca docs disk`.
