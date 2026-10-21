# barca status

One read-only view of every node: what it is, whether it is cached and why, when it last ran,
and what its artifact looks like. It answers in one call what would otherwise take `barca list`,
`--dry-run`, `barca history` and a script that opens the artifact.

```python
import pandas as pd
from barca import asset, task


@asset()
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2, 3], "amount": [9.5, 3.0, 12.25]})


@asset(inputs={"df": orders})
def total(df: pd.DataFrame) -> dict:
    return {"total": float(df["amount"].sum())}


@task(inputs={"t": total})
def notify(t: dict) -> None:
    print(t)
```

```bash
barca get total pipeline.py
barca status pipeline.py --pretty               # table (the default in a terminal)
barca status total pipeline.py                  # only `total` and its upstream cone
barca status total,notify pipeline.py           # several targets: the union of their cones
barca status pipeline.py --json                 # one JSON document (the default when piped)
barca status pipeline.py --fields id,cache      # JSON with only these keys per node
barca status pipeline.py --json --sample 2      # plus up to 2 sample rows per json/parquet artifact
```

```
NAME    KIND   STATE        WHY           LAST RUN                               SHAPE            DEPS
orders  asset  cached       materialized  success 2026-10-02 11:56:43 UTC 0.54s  3 rows x 2 cols  -
total   asset  cached       materialized  success 2026-10-02 11:56:43 UTC 0.00s  dict (1 key)     orders
notify  task   always-runs  task          -                                      -                total

2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

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
whose copies do not agree as `unsynced` (below). Nothing is downloaded or uploaded.

Status reads the metadata DB as it is at that moment, and a run records each step as it
finishes. So while a `barca get` is running, status in another terminal already shows the steps
it has finished as `cached` (a partitioned asset as `partial`, with counts); see `barca docs
cache`, "While a run is going, and after one is killed".

Like `list`, status returns every node in JSON unless `--limit N` is explicit. The human
table shows at most 100 nodes unless you pass `--limit N` or `--all`. The
`summary` still counts every node, and the JSON adds `total` and `truncated` (with a `hint` when
truncated). Each node also lists `env`, the environment variables it declares with `env=[...]`
(`barca docs assets`), and, when its code reads variables it does not declare, `undeclared_env`
(experimental; `barca docs cache`, "Environment variables"). `--fields` keeps only the named keys
on each node.

## Cache state

`cache.state` is the same decision `barca get --dry-run` makes (both call one function), so the
two cannot disagree. In JSON the states are snake_case, spelled exactly like the `summary` keys
(`never_run`, `always_runs`); the table prints them as `never-run` and `always-runs`.

| state | meaning | `reason` |
|---|---|---|
| `cached` | a successful result matches this code and these inputs; `get` serves it | `materialized` |
| `unsynced` | cached, but not where it should be: with an artifact store, this machine's copy, the store's object and the recorded digest do not all agree (`barca docs remote`, "`cached` and `unsynced` with a store"), and `get` on it repairs that without running a step; with a configured server, the server has no result but this machine computed one whose post to the server's ledger is pending (`barca docs server-mode`) | `local_only`, `store_only`, `mismatch`, `not_in_ledger` (the table prints `local only`, `store only`, `mismatch`, `not in ledger`) |
| `stale` | it ran before, but `get` would run it again | `changed`, `upstream_stale`, `failed`, `artifact_missing`, `evicted` |
| `never_run` | no successful materialization is recorded | `no_record`, `failed` |
| `partial` | a partitioned asset with some keys cached | `partitions_missing` |
| `unknown` | dynamic partitions (`partitions_from`) whose source has not run yet, or an asset reading a sensor with no recorded output (and what depends on either) | `partitions_unknown`, `sensor_output_unknown` |
| `always_runs` | tasks and sensors are never cached | `task`, `sensor` |

- `changed`: the run hash differs from the last successful materialization. `cache.changed` lists
  what differs, and `detail` and the table's WHY column name the first and count the rest
  (`changed: helper pipelines/common.py:bucket_ltv (+1 more)`):

  | entry | what changed |
  |---|---|
  | `code` | the function, from `def` on |
  | `decorator argument NAME` | an argument of `@asset` / `@sensor` / `@task` that counts (`serializer`, `inputs`, ...; `barca docs cache`) |
  | `decorator NAME` | another decorator that counts (`@sink`, `@functools.lru_cache`, ...) |
  | `helper FILE:NAME` | a definition the function uses, in its own file or a project module (`pipelines/common.py:bucket_ltv`); `helper FILE` for a module it uses whole |
  | `input ID` | an upstream's run hash: its code or its own inputs changed |
  | `sensor ID` | a sensor it reads returned a new value (or its code changed) |
  | `env NAME` | a declared environment variable's value |
  | `kind` | the node became an asset, task or sensor |

  An entry appears when it was added or removed too (a helper the function now uses). Each
  materialization records a short digest of each of these; a result recorded by barca before
  0.29.0 has none, and the node is then a plain `changed` with no `cache.changed`. A `stale`
  node with reason `upstream_stale` carries `cache.changed` as well when it can be told.
- A node that reads a `@sensor` is judged by the sensor's last recorded output: status runs
  nothing, so it assumes the sensor returns the same value next time, and `detail` says so. If
  the sensor recorded a new value since the node last ran (for example a scheduled sensor), the
  node shows as `stale` (`barca docs cache`, "External data that changes in place").
- `upstream_stale`: an input is itself not cached, so this node's inputs will change when it runs.
  `detail` names the input.
- `failed`: the last attempt at exactly this code and these inputs raised; `detail` carries the
  message.
- `artifact_missing`: the result is recorded and its run hash is unchanged, but the artifact file
  is gone and a run over these nodes would have to read it (a step that runs takes it as an
  input, or it is the output the run returns: a target, or with no target the last asset), so
  it would be computed again. A missing
  artifact that nothing would read does not make a node stale: it stays `cached`, and
  `cache.artifact` then names a file that is not there (`barca docs cache`, "A cached result
  whose artifact is missing").
- `evicted`: the same, because the cache size limit deleted the artifact file (`barca docs cache`,
  "Disk use"). A result the artifact store holds is not stale: a run downloads it again.

- `local_only`, `store_only`, `mismatch`: only with an artifact store. `local_only`: the result
  is on this machine and not in the store, or its upload is pending (`detail` then carries the
  upload's last error); the next `get` or `run` uploads it. `store_only`: in the store, not on
  this machine; `get` fetches it. `mismatch`: both copies are there and differ in size (or in
  bytes, for a store that is a plain directory), or this machine's copy is not the one
  recorded; `get` keeps the recorded copy and uploads it again, with the changed-bytes warning.
  A result in neither place is `stale` with `artifact_missing`. Object stores give no checksum,
  so a same-size object with other bytes is not noticed there. A partitioned asset is
  `unsynced` (`local_only`) only when one of its keys has a pending upload. A store that cannot
  be asked leaves the state as it is, with a note in `detail`. The summary line counts these
  nodes (`1 cached, 1 unsynced, 0 stale, ...`) and `summary.unsynced` is always present.

`cache.run_hash` is the cache key for the current code and inputs; `cache.artifact` is the file a
`get` would serve (only when `cached` or `unsynced`).

### With a server configured

With `BARCA_SERVER` or `[server] url` set, `barca status` (without `--server` or `--local`) is
answered by the server's ledger for its deployed code, after `[barca] server <url> (<source>)`
on stderr (`barca docs server-mode`). Each node gets `local`: `true` when this machine has a copy
of the result the server would serve, which is what `barca sql` can query here. A node the
server has no result for, but whose result this machine computed and has not posted yet (the
post was refused or the server could not be reached), is `unsynced` with reason `not_in_ledger`
and `local: true`; the next `get` or `run` against the server posts it. `summary.unsynced`
counts unsynced nodes. A partitioned node's `local` is `false` (it has no single artifact).
`barca status --local` shows this machine's own state.

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
`total: different bytes for the same code and inputs at 2026-10-02 11:56:43 (pipeline.py:total)`.
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
  "summary": {"cached": 2, "unsynced": 0, "stale": 0, "never_run": 0, "partial": 0, "unknown": 0, "always_runs": 0}
}
```

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
attempts per recorded materialization and its stored final-attempt duration.
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

See also: `barca docs cache`, `barca docs logs`, `barca docs sql`, `barca docs agents`.
