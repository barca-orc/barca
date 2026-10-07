---
title: Server API
description: The barca serve HTTP/JSON API — endpoints, async runs, scheduling, and the Python client.
---

`barca serve` starts a long-running HTTP server (the `barca-server` crate, built on
[axum](https://github.com/tokio-rs/axum)) that exposes the orchestrator as a JSON API.
It is the foundation for programmatic triggering, scheduling, and a future web UI.

The server reuses `barca-core` directly — no subprocess, no separate daemon. Core commands
are async and run on the server's runtime; runs execute in background tasks, are tracked in
memory, and can be cancelled mid-flight via `DELETE /run/{run_id}`.

## Starting the server

```bash
barca serve pipeline.py                   # serve a DAG, default port 8274
barca serve pipeline.py --port 8400       # custom port
barca serve pipeline.py --watch           # dev mode: re-parse DAG on file change
barca serve pipeline.py --no-schedule     # disable the cron scheduler
barca serve pipeline.py --timezone utc    # evaluate cron in UTC (default: local)
barca serve pipeline.py --read-only       # inspect only: no runs, no scheduler, DB never written
barca serve a.py b.py                      # multiple source files
```

The server binds to `127.0.0.1` (local only). There is no authentication in v1 — do not
expose it to untrusted networks. It also serves the web UI at `/ui/`; see
[Deploying](/deploying/) for running it behind nginx.

`--watch` is a **local development convenience**: it re-parses the DAG when a source file
changes so `/assets` and `/plan` reflect edits without a restart. It is off by default and
has no effect on the production serving path.

## Endpoints (v1)

All API responses are JSON, except the event stream and the UI.

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/health` | Liveness, version, whether the server is read-only, and whether it runs the scheduler. |
| `GET`  | `/state` | Every node: its `barca status` entry plus typical durations and next run. |
| `GET`  | `/assets` | List every node with kind, freshness, and upstream inputs. |
| `GET`  | `/assets/{name}` | One asset's summary joined with timing/cache stats. |
| `GET`  | `/plan` | Execution plan (phases and streams) as JSON. |
| `POST` | `/run` | Get every asset and sensor, like `barca get <files>` with no target; tasks are skipped (use `/run/{target}`). Returns a `run_id` immediately. |
| `POST` | `/run/{target}` | Trigger a task run. Returns a `run_id`. |
| `POST` | `/get/{target}` | Trigger a run scoped to one target asset. Returns a `run_id`. |
| `DELETE` | `/run/{run_id}` | Cancel an in-flight run (workers terminated, status → `cancelled`). |
| `GET`  | `/status/{run_id}` | Poll the status and result of a run. |
| `GET`  | `/schedule` | List scheduled jobs with next fire time and last run status. |
| `GET`  | `/events/{run_id}` | Server-Sent Events: a run's live log lines and step/run lifecycle. |
| `GET`  | `/logs/{run_id}` | A run's captured output lines, persisted after it finishes. |
| `GET`  | `/`, `/ui` | Redirect (relative `Location: ui/`) to the web UI. |
| `GET`  | `/ui/` | The web UI, compiled into the binary. |

### Async runs

Runs are asynchronous. `POST /run` and `POST /get/{target}` return immediately with a
server-side polling handle:

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
    "final_output": { "path": ".barca/artifacts/…", "format": "json", "size_bytes": 8 },
    "steps": [{ "id": "pipeline.py:report", "kind": "asset", "status": "ran", "…": "…" }],
    "warnings": []
  },
  "error": null,
  "started_at": 1780721263.05,
  "finished_at": 1780721263.17
}
```

`result.steps` says what happened to each planned step, as in the CLI's JSON. `result.warnings`
is always an array: the plan-time warnings for the steps the run planned (`[]` when there are
none), each `{ kind, node, param, message }`. The one kind so far is `unused_input`, a step that
declares an input its function never uses; the server also prints each warning once on its
stderr ([CLI contract](/reference/cli-contract/), "Plan warnings").

`status` is one of `pending`, `running`, `complete`, `failed`, `cancelled`. The `handle` is
the server's polling id; `result.run_id` is the persisted database run id (the run is also
written to `.barca/metadata.db`, same as a CLI run).

In-flight run state is held in memory and is not persisted across a server restart. The run
history in the database persists regardless. A background sweep evicts finished runs
(`complete`/`failed`/`cancelled`) from memory once they are more than an hour old (checked every 5
minutes), so `GET /status/{run_id}` for an old run eventually returns `404` even though its row
remains in `barca history`.

### Cancelling a run

```
DELETE /run/{run_id}
```

Cancels a pending or running run: its Python workers are terminated, partial results from
already-completed steps are persisted, and the run's status transitions to `cancelled`
(both in `/status/{run_id}` and in the `runs` history table). The response is
`{ "run_id": "...", "status": "cancelling" }`; poll `/status/{run_id}` to observe the
transition. Cancelling a run that already finished returns `409`. Runs that exceed the
server's 10-minute timeout are stopped the same way and reported as `failed`.

### Health

```
GET /health
```

```json
{ "status": "ok", "version": "0.17.0", "read_only": false, "scheduler": true }
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

`AssetSummary` is `{ id, kind, freshness, inputs }`. `{name}` matches by asset name or full
node id; an unknown name returns `404`. `AssetStats` carries run counts, timing percentiles,
and cache hit rate.

### Plan

```
GET /plan              → { total_steps, phases: [{ reason: {type, node_id?}, streams: [{ stream_id, steps }] }], warnings: [{ kind, node, param, message }] }
```

`warnings` is always present: the plan-time warnings for every step of the project, `[]` when
there are none (the same items as `result.warnings` above).

## Scheduling

`barca serve` runs a **cron scheduler** — the piece that gives
`@asset(freshness=Schedule("..."))` teeth. It is **on by default**; pass `--no-schedule`
to turn it off.

At startup the server enumerates every node whose freshness is `Schedule(cron)`, parses
each cron expression (standard 5-field, or 6-field with a leading seconds field for
sub-minute schedules), and logs the schedule (invalid or empty cron strings are logged and
skipped, not fatal). A background task then wakes at each second boundary and, for every
job whose cron matches the current second, triggers a run through the same run pool as
`POST /run` / `POST /run/{target}`:

- **Assets and sensors** are materialized via the `get` path, cache-aware.
- **Tasks** are executed via the `run` path. A tick reuses cached upstream assets, as
  `barca run <task>` does; `POST /run/{task}` recomputes every upstream asset.

Each scheduled run gets a normal `run_id`, is visible via `GET /status/{run_id}`, and is
persisted to `.barca/metadata.db` (`barca history`), like a manually triggered run.
Inspect the live schedule with `GET /schedule` or, statically, with `barca list <files>`
(scheduled definitions show their next fire time).

Behavior:

- **Timezone.** Cron is evaluated in the machine's local time by default. Pass
  `--timezone utc` or `--timezone America/New_York` (any IANA name) to change it.
- **Catch-up.** The scheduler persists the last fire time of each job. On startup, if a
  scheduled tick elapsed while the daemon was down, the job fires **once** to catch up
  (jobs never seen before are anchored to "now" — no first-launch stampede). Individual
  ticks missed during a long outage are *not* replayed one-for-one.
- **Concurrency.** Independent runs execute in parallel (bounded by a run pool sized to the
  machine's CPUs); their writes to the shared `metadata.db` are serialized by a process-wide
  DB lock. A scheduled job never overlaps *itself*: if its previous run is still
  pending/running when the next tick arrives, that tick is skipped.
- **Reload.** Under `--watch`, editing a source file re-reads the schedule live (within a
  second). Without `--watch` the job set is fixed for the process lifetime.

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

`next_fire`/`last_fired` are unix epoch seconds (`last_fired` is `null` until the first
fire); `last_run` is the most recent scheduled `run_id` and `last_status` its state
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
`get(target=None)` (`barca get [TARGET]`; omit the target to get every asset and sensor, never tasks) and
`run(target)` (`barca run TARGET`). The trigger methods return a `Run` whose `.wait()` blocks
until the run reaches a terminal state. This complements `barca.api` (`barca.get`/`run`/…),
which shells out to the binary for one-shot commands rather than talking to a server.

## Errors

Errors return a JSON body `{ "error": "..." }` with an appropriate status code: `404` for an
unknown asset or run, `400` for parse/DAG errors, `403` for a run or cancel requested of a
`--read-only` server, `409` for conflicts (an ambiguous `{name}` match
in `GET /assets/{name}`, or cancelling a run that already finished), and `500` for execution or
database failures.

## Not in v1

No authentication (put it at a reverse proxy — see [Deploying](/deploying/)), no distributed
execution, and no persistence of the in-memory run queue across restarts.
