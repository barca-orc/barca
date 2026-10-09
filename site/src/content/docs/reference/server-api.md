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
barca serve pipeline.py --host 0.0.0.0    # every interface (containers, VMs)
barca serve --port 8400 --timezone utc    # every file in the project
```

The flags (`--host`, `--port`, `--watch`, `--no-schedule`, `--timezone`, `--read-only`, `--env`) are in
the [CLI reference](/reference/cli/#serve). On start the server prints its address and the
schedule on stderr:

```
[barca] serving on http://127.0.0.1:8274  (1 file)
[barca] scheduling 1 asset:
  pipeline.py:daily — 0 5 * * * (next 2026-10-08 05:00:00)
```

The default bind address is `127.0.0.1`. `--host` takes an IP address, including `::` for
IPv6; hostnames are rejected. `0.0.0.0` listens on every IPv4 interface. There is no
authentication, so anyone who can reach the port can trigger runs. A non-loopback address
prints a startup warning; use a private network or an authenticating proxy.

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
| `GET`  | `/runs?limit=100` | Recent durable runs, joined with queued and live server runs. |
| `GET`  | `/runs/{id}` | Inspect a durable run or live polling handle, with steps and captured logs. |
| `GET`  | `/schedule` | List scheduled jobs with next fire time and last run status. |
| `GET`  | `/events/{run_id}` | Server-Sent Events: a run's live log lines and step/run lifecycle. |
| `GET`  | `/logs/{run_id}` | A run's captured output lines, persisted after it finishes. |
| `GET`  | `/`, `/ui` | Redirect (relative `Location: ui/`) to the web UI. |
| `GET`  | `/ui/` | The web UI, compiled into the binary. |

### Run history and inspection

The UI's **Runs** view lists this history and opens each run's status, timing, steps,
errors and logs. After starting a node from the graph, **View run** opens its live details.
The detail URL switches to the durable run ID once assigned, so it can be bookmarked
and reopened after a server restart.

`GET /runs?limit=100` returns `{ "runs": [...], "total": 12, "truncated": false }`,
newest first. `limit` defaults to 100 and is bounded to 1–1000. It includes runs from the CLI,
HTTP triggers and the scheduler in this environment's local metadata DB, plus queued server
runs that have not yet received a durable ID. There is no source filter: a historical run
remains visible even when its pipeline is no longer served.

Each summary has `id`, `run_id` (the durable DB ID, or `null` while queued), `handle`
(the server polling handle, or `null` after restart/eviction), `command`, `files`, `target`,
`status`, UTC `started_at` and `finished_at`, `elapsed_seconds`, `steps_total`,
`steps_executed`, `steps_cached`, and `error`. Durable statuses are `running`, `success`,
`failed`, `cancelled` and `interrupted`; an accepted run not yet persisted can be `pending`.
A failure before the engine records a run remains visible only in this server's memory.

`GET /runs/{id}` accepts either ID while the server retains the handle and returns
`{ "run": {...}, "steps": [...], "logs": [...], "result": null }`. A step has `node_id`,
`status`, `elapsed_seconds` and `error`; a log line has `node_id`, `seq` and `line`.
Logs and materialized steps survive a restart under the durable ID. While running, captured
live logs and completed steps are included. `result` is the completed engine result while
its server handle is retained. Its step reports supplement cached steps; cached inputs
have no new materialization row, so those per-step reports and final output are unavailable
after a server restart, although durable run-level cached counts remain. An unknown ID
returns JSON `404`.

These inspection endpoints always read a private DB snapshot. They do not create or migrate
the project's database, change run status, or execute Python, including on a read-only server.
Existing `/status/{handle}`, cancellation and live SSE endpoints keep their handle contract.

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

`POST /run/{target}` and `POST /get/{target}` check the target before they start a run, with
the check `barca run` and `barca get` make (one function decides for both; a wrong-kind answer
names the other endpoint where the command line names the other command). The check is made
against the source as it is now: the server keeps the list of nodes between requests and reads
the files again when one of them has changed size, modification time or change time (the
last moves even when an edit puts the old modification time back). When the target cannot run, no
run is started, there is no `run_id`, and the response is an error with a JSON body
`{ "error": "..." }`:

| Request | Status | `error` |
|---|---|---|
| a name that matches no node | `404` | `Asset 'nope' not found. Available: pipeline.py:orders, pipeline.py:total` |
| a name that matches several nodes (the same function name in two files) | `409` | ``'orders' matches more than one node: a.py:orders, b.py:orders. Name one by its full id, e.g. `a.py:orders` `` |
| `POST /get/{target}` naming a task | `400` | `'publish' is a task: use POST /run/publish` |
| `POST /run/{target}` naming an asset | `400` | `'orders' is an asset: use POST /get/orders` |
| source that does not parse, or a DAG that cannot be built | `400` | the parse or DAG error |

`POST /run/{target}` accepts a task or a sensor and `POST /get/{target}` an asset or a sensor,
as on the command line. A target is a function name, a full node id (`pipeline.py:orders`) or a
path-suffixed id. An id with a directory in it works with its `/` as it is or percent-encoded
(`sub/pipeline.py:orders`, `sub%2Fpipeline.py:orders`). A trigger with no target
(`POST /get/`), like any path that is not an endpoint, is a `404` with an `error` body.

The set of files is the one the server started with, as for a run: a pipeline file added
under a served directory or the project is not read until a restart (its nodes are `404`), and
after one is deleted every trigger is a `400` naming the missing file. Without the check both
were a `200` whose run failed with the same message.

A run that was started can still fail on its target if the source changes between the check
and the run. It then has `"status": "failed"`, `"result": null` and the message in `error`,
like any other failed run:

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
{ "status": "ok", "version": "0.20.1", "read_only": false, "scheduler": true }
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
`GET /assets/{name}`, an unknown target in `POST /run/{target}` or `POST /get/{target}`, or an
unknown run id; `400` for parse and DAG errors and for a target of the wrong kind for the
endpoint; `403` for a run or cancel requested of a `--read-only` server; `409` for conflicts (a
name that matches several nodes in `GET /assets/{name}` or in a trigger, or cancelling a run
that already finished); `405`, with the allowed methods in `error` and in the `Allow`
header, for a known path asked with the wrong method; and `500` for execution or database
failures. The Python client raises
`BarcaError` carrying the status and the message for each of these.

## Limits

- No authentication and no TLS. The server listens on `127.0.0.1` by default; `--host` can
  make it reachable from other machines. Authenticate at a proxy.
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
