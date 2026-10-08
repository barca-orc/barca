---
title: Server API
description: The HTTP and JSON API of barca serve, with its endpoints, asynchronous runs, the scheduler and the Python client.
---

`barca serve` starts an HTTP server that runs assets and tasks on request, fires
`Schedule(...)` nodes on their cron, and serves a web UI at `/ui/`.

The server binds to `127.0.0.1` and has no authentication: anyone who can reach the port can
start and cancel runs. To reach it from another machine, put a reverse proxy that authenticates
in front of it (see [Deploying](/deploying/)).

Runs execute inside the server process, in the background. A run in progress is tracked in
memory and can be cancelled with `DELETE /run/{run_id}`. Finished runs are written to
`.barca/metadata.db`, like runs from the command line.

## Starting the server

```bash
barca serve pipeline.py                   # one file, default port 8274
barca serve --port 8400 --timezone utc    # every file in the project
```

The flags (`--port`, `--watch`, `--no-schedule`, `--timezone`, `--read-only`, `--env`) are in
the [CLI reference](/reference/cli/#serve). On start the server prints its address and the
schedule on stderr:

```
[barca] serving on http://127.0.0.1:8274  (1 file)
[barca] scheduling 1 asset:
  pipeline.py:daily — 0 5 * * * (next 2026-10-08 05:00:00)
```

`--watch` re-parses the DAG when a source file changes, so `/assets` and `/plan` reflect edits
without a restart. Files added after the server started are not picked up until a restart, with
or without `--watch`.

`barca serve` does not support shared history. With a remote store configured it exits 2
unless `state = "off"` is set in `barca.toml` or `BARCA_STATE=off` in the environment. See
[Deploying](/deploying/#with-a-remote-store).

## Endpoints (v1)

All API responses are JSON, except the event stream and the UI.

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/health` | Liveness, version, whether the server is read-only, and whether it runs the scheduler. |
| `GET`  | `/state` | Every node: its `barca status` entry plus typical durations and next run. |
| `GET`  | `/assets` | List every node with kind, freshness, upstream inputs and declared environment variables. |
| `GET`  | `/assets/{name}` | One asset's summary joined with timing/cache stats. |
| `GET`  | `/assets/{name}/schema` | Selected node and direct inputs as `NodeStatus[]`, with artifact shapes inspected on demand. |
| `GET`  | `/plan` | Execution plan (phases and streams) as JSON. |
| `POST` | `/run` | Get every asset and sensor, like `barca get <files>` with no target; tasks are skipped (use `/run/{target}`). Returns a `run_id` immediately. |
| `POST` | `/run/{target}` | Trigger a task run. Every upstream asset is recomputed, as with `barca run <task> --refresh-all`. Returns a `run_id`. |
| `POST` | `/get/{target}` | Trigger a run scoped to one target asset. Returns a `run_id`. |
| `DELETE` | `/run/{run_id}` | Cancel an in-flight run (workers terminated, status → `cancelled`). |
| `GET`  | `/status/{run_id}` | Poll the status and result of a run. |
| `GET`  | `/schedule` | List scheduled jobs with next fire time and last run status. |
| `GET`  | `/events/{run_id}` | Server-Sent Events: a run's live log lines and step/run lifecycle. |
| `GET`  | `/logs/{run_id}` | A run's captured output lines, persisted after it finishes. |
| `GET`  | `/`, `/ui` | Redirect (relative `Location: ui/`) to the web UI. |
| `GET`  | `/ui/` | The web UI, compiled into the binary. |

### Async runs

Runs are asynchronous. `POST /run`, `POST /run/{target}` and `POST /get/{target}` take no
request body and return `200` immediately with a polling handle:

```json
{ "run_id": "1b942bb33182" }
```

Poll `GET /status/{run_id}` until `status` reaches a terminal state (`complete`, `failed`, or
`cancelled`):

```json
{
  "handle": "1b942bb33182",
  "status": "complete",
  "result": {
    "run_id": "1b9422cf12f3",
    "elapsed_seconds": 0.115,
    "steps_executed": 2,
    "phases": 1,
    "final_output": { "path": ".barca/artifacts/…", "format": "json", "size_bytes": 11, "elapsed_seconds": 0.0076 },
    "steps": [{ "id": "pipeline.py:report", "kind": "asset", "status": "ran", "…": "…" }],
    "warnings": []
  },
  "error": null,
  "started_at": 1780721263.05,
  "finished_at": 1780721263.17
}
```

`result.final_output` is always a pointer to the artifact file (`path`, `format`,
`size_bytes`, `elapsed_seconds`), for json results too. The command line prints a json value
inline; the server does not. Read the file at `path`, relative to the project root.

The target name is checked when the run starts, not when it is requested. A `POST` with an
unknown target, or with an asset on `/run/{target}` or a task on `/get/{target}`, still returns
`200` and a `run_id`; the run then has `"status": "failed"`, `"result": null` and the message
in `error`:

```json
{ "handle": "52344f5c6f20", "status": "failed", "result": null,
  "error": "Asset 'nope' not found. Available: pipeline.py:orders, pipeline.py:total",
  "started_at": 1791397140.41, "finished_at": 1791397140.41 }
```

`result.steps` says what happened to each planned step, as in the CLI's JSON, including
`artifact_mismatch: true` on a step whose artifact the store holds with other bytes than were
recorded for it and on the steps that read it ([CLI contract](/reference/cli-contract/), "A
store copy that differs from its recorded hash"). `result.warnings`
is always an array: the plan-time warnings for the steps the run planned (`[]` when there are
none), each `{ kind, node, param, message }`. The one kind so far is `unused_input`, a step that
declares an input its function never uses; the server also prints each warning once on its
stderr ([CLI contract](/reference/cli-contract/), "Plan warnings").

`status` is one of `pending`, `running`, `complete`, `failed`, `cancelled`. The `handle` is
the server's polling id; `result.run_id` is the persisted database run id (the run is also
written to `.barca/metadata.db`, same as a CLI run).

Run state is held in memory and is lost on a server restart; the run history in the database
is not. Finished runs are removed from memory once they are more than an hour old, so
`GET /status/{run_id}` for an old run returns `404` although its row remains in
`barca history`.

### Cancelling a run

```
DELETE /run/{run_id}
```

Cancels a pending or running run. Its Python workers are terminated, the results of steps
that had already finished are kept, and the run's status becomes `cancelled`, both in
`/status/{run_id}` (with `"error": "run cancelled"`) and in `barca history`. The response is
`{ "run_id": "...", "status": "cancelling" }`; poll `/status/{run_id}` to see the change.
Cancelling a run that already finished returns `409`, and an unknown id `404`. A run that
exceeds the server's 10-minute limit is stopped the same way and reported as `failed`.

### Health

```
GET /health
```

```json
{ "status": "ok", "version": "0.18.1", "read_only": false, "scheduler": true }
```

`scheduler` is `true` when this server fires `Schedule(...)` nodes: on by default, `false` with
`--no-schedule` or `--read-only`.

### State

```
GET /state             → [NodeState, ...]   (dependency order)
```

Each `NodeState` is a `barca status --json` node (`id`, `name`, `kind`, `inputs`, `partitioned`,
`cache`, `partitions`, `last_materialization`, `shape`, `env`) plus `durations` — `{ median_seconds,
p95_seconds, samples }` over the last 20 successful runs, or `null` — and `next_run`, the next cron
fire time in unix seconds for scheduled nodes. `cache` is the `barca status` cache entry: `state` is `cached`, `stale`, `never_run`, `partial`,
`unknown` or `always_runs`, with a machine-readable `reason` and a `detail` in words (see
[`barca status`](/reference/cli/#status)). `shape` is always `null` here:
reading artifact shapes starts a reader process, too slow for an endpoint the UI polls. The cache
check reads a private copy of the metadata DB, so this endpoint never writes it.

### Live events and logs

```
GET /events/{run_id}   → text/event-stream of RunEvent JSON
GET /logs/{run_id}     → { "logs": [{ node_id, seq, line }, ...] }
```

Events are `run_started`, `log` (`{ node_id, line }`, one per line a step prints), `step_finished`
(`{ node_id, ok, elapsed_seconds?, error? }`) and `run_finished` (`{ run_id, ok }`). A client that
connects after the run started first receives the events it missed; the stream stays open after
`run_finished` (with a keep-alive every 15s) until the run is evicted. The response carries
`X-Accel-Buffering: no` so nginx passes events through as they happen. `/logs` accepts either the
`run_id` returned by `POST /run` or the run id stored in run history.

### Read-only mode

With `--read-only`, `POST /run`, `POST /run/{target}`, `POST /get/{target}` and
`DELETE /run/{run_id}` return `403`, the scheduler does not start, and `/state`, `/assets/{name}`
and `/logs` read a private copy of the metadata DB — the database is never opened in place,
created or written.

### Assets

```
GET /assets            → [AssetSummary, ...]
GET /assets/{name}     → { "asset": AssetSummary | null, "stats": AssetStats }
```

`AssetSummary` is `{ id, kind, freshness, inputs, env }`. Here `freshness` is an object,
`{"type": "Always"}`, `{"type": "Manual"}` or `{"type": "Schedule", "value": "0 5 * * *"}`,
unlike `barca list --json`, which prints a lowercase string. `{name}` matches by function name
or full node id; an unknown name returns `404`. `AssetStats` is `{ node_id, total_runs,
cache_hit_rate, avg_elapsed_seconds, median_elapsed_seconds, p95_elapsed_seconds,
max_elapsed_seconds, recent_runs }`; the timings are `null` until the node has run.

### Plan

```
GET /plan              → { total_steps, phases: [{ reason: {type, node_id?}, streams: [{ stream_id, steps }] }], warnings: [{ kind, node, param, message }] }
```

`warnings` is always present: the plan-time warnings for every step of the project, `[]` when
there are none (the same items as `result.warnings` above).

## Scheduling

`barca serve` runs a cron scheduler for nodes declared with `freshness=Schedule("...")`. It is
on by default; pass `--no-schedule` to turn it off. `Schedule` is the only freshness value the
server acts on. `Always` and `Manual` are recorded and shown (`barca list`, `/assets`) and
cause no runs.

Each second, every job whose cron matches triggers a run through the same run pool as the
`POST` endpoints:

- **Assets and sensors** go through the `get` path, cache-aware. A sensor's tick runs the
  sensor and does not trigger the assets that read it.
- **Tasks** go through the `run` path. A tick reuses cached upstream assets, as
  `barca run <task>` does; `POST /run/{task}` recomputes every upstream asset.

Each scheduled run gets a `run_id`, is visible via `GET /status/{run_id}`, and is written to
`.barca/metadata.db` (`barca history`), like a run requested over HTTP. Independent runs
execute in parallel, bounded by a run pool sized to the machine's CPUs. Timezone, catch-up
after downtime, skipped overlapping ticks and reloading are described in
[Scheduling](/scheduling/#caveats).

### `GET /schedule`

```
GET /schedule → [ScheduleEntry, ...]
```

Each `ScheduleEntry` is:

```json
{
  "id": "pipeline.py:daily_report",
  "cron": "0 5 * * *",
  "kind": "asset",
  "next_fire": 1780740000,
  "last_fired": 1780653600,
  "last_run": "1b942bb33182",
  "last_status": "complete"
}
```

`next_fire` and `last_fired` are unix epoch seconds. `next_fire` is the next match of the cron
expression in the zone the server evaluates cron in (`--timezone`), so it is when the job will
fire; `next_run` in `GET /state` is computed the same way. `barca list` cannot know a server's
zone and always uses the local time of the machine it runs on. A job the scheduler has not seen before
gets `last_fired` set to the time the server first started with it, so it is not `null` even
though nothing has run. `last_run` is the most recent scheduled `run_id` and `last_status` its state
(`pending`/`running`/`complete`/`failed`/`cancelled`, or `null` if none yet).

## Python client

The `barca.Client` SDK (standard-library only) wraps this API:

```python
from barca import Client

c = Client("http://127.0.0.1:8274")
run = c.get("daily_report")       # POST /get/{target}, returns immediately
result = run.wait(timeout=30)     # poll /status until complete/failed
print(result["status"])

for job in c.schedules():         # GET /schedule
    print(job["id"], job["cron"], job["next_fire"])
```

`Client` methods map to the endpoints above: `health()`, `assets()`, `asset(name)`,
`plan()`, `schedules()`, `status(run_id)`, `cancel(run_id)` (also available as
`Run.cancel()`), plus the two trigger verbs that mirror the CLI —
`get(target=None)` (omit the target to get every asset and sensor, never tasks) and
`run(target)`. The trigger methods return a `Run` with `.status()`, `.cancel()` and
`.wait(timeout=600.0, poll=0.5)`, which blocks until the run is complete, failed or cancelled.
`Client()` defaults to `http://127.0.0.1:8274`. There are no client methods for `/state`,
`/events` or `/logs`. `barca.get`, `barca.run` and the other functions in `barca.api` are
separate: they start the `barca` binary for one command and do not talk to a server.

## Errors

Errors return a JSON body `{ "error": "..." }`: `404` for an unknown name in
`GET /assets/{name}` or an unknown run id, `400` for parse and DAG errors, `403` for a run or cancel requested of a
`--read-only` server, `409` for conflicts (an ambiguous `{name}` match
in `GET /assets/{name}`, or cancelling a run that already finished), and `500` for execution or
database failures.

## Limits

- No authentication and no TLS. The server listens on `127.0.0.1` only; there is no flag to
  change the address.
- No shared history: `BARCA_STATE=off` is required with a remote store.
- Runs in progress are not persisted. After a restart their handles return `404`; finished runs
  remain in `barca history`.
- One machine: steps run in worker processes on the host that runs the server.

### Input and output schemas

`GET /assets/{name}/schema` reads a private metadata snapshot and inspects the
selected node and its direct inputs using the same reader as `barca status`.
It returns `NodeStatus[]`. Each node's `shape` contains `type`, `rows` and
`columns: [{name, type}]` when the artifact supports them. JSON objects report
field names and types, JSON lists also report `item_types`; pickle reports its top-level type without unpickling. Reader failures
appear in `shape.note`. No artifact or a failed latest attempt gives `shape: null`.
For partitioned nodes this describes the latest materialized key, named in
`last_materialization.partition`, rather than a union of every key's schema.
No sample values are returned. Nested JSON fields report their container type (`dict` or `list`); this is not a recursive schema. Pickle types or constructors are identified from opcodes without executing them, and pickle column schemas are unavailable. This endpoint is available in read-only mode;
unknown and ambiguous names return 404 and 409 respectively.
