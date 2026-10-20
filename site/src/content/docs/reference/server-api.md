---
title: Server API
description: The HTTP and JSON API of barca serve, with its endpoints, asynchronous runs, the scheduler and the Python client.
---

`barca serve` starts an HTTP server that runs assets and tasks on request, fires
`Schedule(...)` nodes on their cron, and serves a web UI at `/ui/`.

The server binds to `127.0.0.1` and has no authentication: anyone who can reach the port can
start and cancel runs. To reach it from another machine, put a reverse proxy that authenticates
in front of it (see [Deploying](/deploying/)).

**Scope:** this is the implemented `barca serve` API, including the run contract and
inspection routes of the [server-mode RFC](/rfcs/0007-server-mode/) (section 3.4). The CLI's
`--server` flag is a client of these routes: `barca get daily_report --server <url>` runs on the
server and prints what a local run prints ([Deploying](/deploying/#running-commands-on-the-server-from-another-machine)).
Local runs against the server's ledger, with its `/ledger/*` routes (#402), are not
implemented yet.

Runs execute inside the server process, in the background. `POST /runs` answers at once with the
run's id, which is its database run id (the one `barca history` shows); the run's events stream
from `GET /runs/{run_id}/events`, and `GET /runs/{run_id}` answers from `.barca/metadata.db`
once it has finished, so the answer survives a restart.

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

## Endpoints

The normative [OpenAPI 3.1 contract](https://github.com/barca-orc/barca/blob/main/specs/server-api.openapi.yaml)
defines the current routes, bodies and statuses and is checked against the real router and
Python Client. This page explains that contract; it does not advertise proposed RFC routes.

Responses are JSON, except the event stream, a run's output and the UI. Every error body is the
CLI's error envelope (see [Errors](#errors)). Axum request-extractor errors (for example, an
invalid `/runs?limit=`) can instead return `400` plain text. GET routes also accept HEAD and
return the corresponding headers without a body.

**Runs.**

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/runs` | Start a run of `barca get` or `barca run` on the server. Body: [`RunRequest`](#starting-a-run). `202 {"run_id"}`, or `200` with the `--dry-run` JSON when `dry_run` is set. |
| `GET`  | `/runs/{run_id}` | The run: what started it, its status and, once finished, its [outcome](#outcomes), with its steps and captured lines. Survives a restart. |
| `GET`  | `/runs/{run_id}/events` | Server-sent events, each with an `id:`: `run_started`, `line`, `log`, `step_finished`, `run_finished`. |
| `DELETE` | `/runs/{run_id}` | Cancel a pending or running run. |
| `GET`  | `/runs/{run_id}/output` | The final artifact's bytes of a successful single-target run. |
| `GET`  | `/runs?limit=100` | Recent durable runs, joined with queued and live server runs. |
| `GET`  | `/logs/{run_id}` | A run's captured output lines, persisted after it finishes. |

**Inspection.** The body is the CLI command's `--json` output, built by the same function, so a
client prints it as it would print the local command's.

| Method | Path | Same as |
|--------|------|---------|
| `GET`  | `/list` | `barca list --json --all` |
| `GET`  | `/nodes?targets=a,b&sample=N` | `barca status --json --all`, for the server's ledger |
| `GET`  | `/history?limit=N&all=1` | `barca history --json` (`-l N`, `--all`) |
| `GET`  | `/stats/{name}` | `barca stats --json` |
| `GET`  | `/project` | The deployed tree: `{api_version, root, env, deployed_at, commit, files: [{path, sha256}]}` |
| `GET`  | `/health` | Liveness, `api_version`, `env`, the artifact store (`store_uri`), read-only, scheduler, load diagnostics. |

**The served graph and the UI.**

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/state` | Every node: its `barca status` entry plus typical durations and next run. |
| `GET`  | `/groups` | Organizational groups as `[{id, name, description, members, output}]`; separate from execution edges. |
| `GET`  | `/assets` | Every node as `barca list --json` shows it. |
| `GET`  | `/assets/{name}` | One node's `barca list --json` entry with its `barca stats --json` body. |
| `GET`  | `/assets/{name}/schema` | Selected node and direct inputs as `NodeStatus[]`, with artifact shapes inspected on demand. |
| `GET`  | `/plan` | Execution plan (phases and streams) as JSON. |
| `GET`  | `/schedule` | Scheduled jobs with next fire time, last run, last skipped tick and pause. |
| `POST` | `/schedule/{id}/pause` | Hold one schedule back, optionally `?until=<RFC 3339>`. |
| `POST` | `/schedule/{id}/resume` | Let a paused schedule fire again. |
| `GET`  | `/`, `/ui` | Redirect (relative `Location: ui/`) to the web UI. |
| `GET`  | `/ui/` | The web UI, compiled into the binary. |

**Older routes, kept as aliases for one minor release.** They run on the same functions as the
routes above, and the `run_id` they answer with is the database run id. Move to the new routes;
the aliases are removed in the cleanup release after this one.

| Method | Path | Use instead |
|--------|------|-------------|
| `POST` | `/run` | `POST /runs` with `{"command": "get"}` |
| `POST` | `/run/{target}` | `POST /runs` with `{"command": "run", "targets": [...], "refresh_all": true}` (the alias recomputes upstream assets) |
| `POST` | `/get/{target}` | `POST /runs` with `{"command": "get", "targets": [...]}` |
| `DELETE` | `/run/{run_id}` | `DELETE /runs/{run_id}` |
| `GET`  | `/status/{run_id}` | `GET /runs/{run_id}` |
| `GET`  | `/events/{run_id}` | `GET /runs/{run_id}/events` |

### Starting a run

```
POST /runs
Content-Type: application/json

{
  "command": "get",             // "get" | "run"
  "targets": ["daily_report"],  // as given on the command line; [] means every asset and sensor
  "files": [],                  // deployed files, as typed relative to the root; the run reads only these. [] means all of them
  "refresh": [], "refresh_all": false, "cascade": true,
  "dry_run": false,
  "agent": true                 // which stderr lines the client will print
}
```

Only `command` is required. The fields are the flags of `barca get` and `barca run`
(`--refresh`, `--refresh-all`, `--no-cascade` is `"cascade": false`, `--dry-run`, `--agent`).
The command runs on the server with its deployed code. A file that is not one of the deployed
files is a `400` naming them.

```json
{ "run_id": "1b942bb33182" }
```

The targets are checked before anything starts, with the function `barca get` and `barca run`
use, in the CLI's words: an unknown name is `404` (with the `barca list` remediation), a name
that matches several nodes `409`, a target of the wrong kind (`get` of a task) `400`, and `run`
without a target `400`. No run is started for a refused request and there is no `run_id`.
`dry_run` answers `200` at once with the `--dry-run --json` body, decided against a private
copy of the metadata DB; nothing starts and nothing is written, also on a `--read-only` server.
A read-only server refuses every other run with `403`.

The check is made against the source as it is now: the server keeps the list of nodes between
requests and reads the files again when one of them has changed size, modification time or
change time. A run that was started can still fail on its target if the source changes between
the check and the run; its outcome is then an `error` with the CLI's envelope.

Every run is one node: it executes in worker processes on the server's machine, bounded by a
run pool sized to the machine's CPUs, and is written to `.barca/metadata.db` like a run from the
command line, with `triggered_by: "api"`. A run that exceeds the server's 10-minute limit is
stopped and ends with an `error` outcome.

### Following a run

```
GET /runs/{run_id}/events   → text/event-stream
```

Each event is a server-sent event whose `data:` is JSON with a `type`, and whose `id:` is its
number in the run, from `0`:

| `type` | Payload | Meaning |
|---|---|---|
| `run_started` | `run_id` | The run left the queue. |
| `line` | `line`, `when` | A stderr line exactly as a local run prints it: `--agent` step lines (`when: "agent"`), warnings, `[barca]` notes and the end-of-run line (`when: "always"`). A client prints the lines its own mode would. |
| `log` | `node_id`, `line` | A line a step printed. |
| `step_finished` | `node_id`, `ok`, `cached?`, `elapsed_seconds?`, `error?`, `import_seconds?` | A step ran, failed, or was served from cache (`cached: true`). |
| `run_finished` | `run_id`, `ok`, `outcome` | The run's [outcome](#outcomes). The stream ends after it. |

A subscriber receives the run from its first event whenever it connects, and every event once,
however slowly it reads: the server keeps the run's events for as long as it keeps the run (an
hour after it finished) and a lagging subscriber reads on from where it was. A connection that
drops mid-run is not resumed: reconnecting replays the run from its first event, and resuming
from `Last-Event-ID` is future work. The run goes on whether anyone is reading. Once the run is no
longer kept, the stream is a `404`; `GET /runs/{run_id}` answers from the database then.

The response carries `X-Accel-Buffering: no` so nginx passes events through as they happen, and a
keep-alive comment every 15 seconds.

### Outcomes

```
GET /runs/{run_id}
```

```json
{
  "run_id": "1b942bb33182",
  "status": "success",
  "command": "get",
  "target": "daily_report",
  "files": ["pipeline.py"],
  "triggered_by": "api",
  "scheduled_at": null,
  "schedule_id": null,
  "started_at": "2026-10-20T05:00:00.000Z",
  "finished_at": "2026-10-20T05:00:01.000Z",
  "elapsed_seconds": 1.2,
  "steps_total": 2, "steps_executed": 1, "steps_cached": 1,
  "outcome": { "status": "success", "run_id": "1b942bb33182", "final_output": {"n": 2}, "steps": [], "...": "..." },
  "error": null,
  "steps": [{ "node_id": "pipeline.py:daily_report", "status": "success", "elapsed_seconds": 0.4, "error": null }],
  "logs": [{ "node_id": "pipeline.py:daily_report", "seq": 0, "line": "wrote 3 rows" }]
}
```

`status` is `pending` or `running` while the run is live, then the outcome's status; a row the
server did not start reads as the database has it (`success`, `failed`, `cancelled`,
`interrupted`). `triggered_by` is `api` for a run started over HTTP, `schedule` for a cron
tick (with the tick in `scheduled_at`, RFC 3339 in UTC, and the scheduled node in
`schedule_id`), `manual` for a command typed on a machine, and `client` for a run a laptop
posted to the ledger (#402); it is `null` for runs recorded before barca recorded it.

`outcome` is `null` while the run runs, and for runs not started by `barca serve`. Otherwise it is
tagged by `status`:

- `success`: the exact `barca get --json` / `barca run --json` stdout body. `final_output` is the
  value for a JSON artifact and an artifact pointer for binary formats, as on the command line.
  Several targets carry `targets`, in the order given.
- `failed`: the CLI's failed-run stdout body (`failed_node`, `error`, `steps`, ...) plus
  `envelope`, the stderr error envelope with the traceback.
- `cancelled`, `error`: `{"status", "envelope"}` only.

The outcome is stored with the run's row (`runs.outcome_json`), so `GET /runs/{run_id}` gives
the same answer after the server restarts. A run cancelled while it was still queued, or refused
by a source change after it was accepted, has a row of its own with its outcome.

```
GET /runs/{run_id}/output
```

The bytes of a successful single-target run's final artifact, with `Content-Type` by format:
`application/json`, `application/vnd.apache.parquet`, `text/csv`, and
`application/octet-stream` for pickle. A run of several targets or of every asset, a failed run
and an unknown run are `404`; a run still going is `409`. It reads the server's local artifact.

### Cancelling a run

```
DELETE /runs/{run_id}
```

Cancels a pending or running run. Its Python workers are terminated, the results of steps
that had already finished are kept, and the run ends `cancelled` (in its events, in
`GET /runs/{run_id}` and in `barca history`). The response is
`{ "run_id": "...", "status": "cancelling" }`. Cancelling a run that already finished returns
`409`, and an unknown id `404`.

### Run history

`GET /runs?limit=100` returns `{ "runs": [...], "total": 12, "truncated": false }`,
newest first. `limit` defaults to 100 and is bounded to 1–1000. It includes runs from the CLI,
HTTP and the scheduler in this environment's local metadata DB, plus queued server runs. There
is no source filter: a historical run remains visible even when its pipeline is no longer served.

Each summary has `id` and `run_id` (the database run id), `handle` (the same id while the
server keeps the run in memory, `null` otherwise), `command`, `files`, `target`, `status`,
`triggered_by`, `scheduled_at`, `schedule_id`, UTC `started_at` and `finished_at`,
`elapsed_seconds`, `steps_total`, `steps_executed`, `steps_cached`, and `error`.

The UI's **Runs** view lists this history and shows what started each run; each run opens its
status, timing, steps, errors and logs.

These inspection endpoints always read a private DB snapshot. They do not create or migrate
the project's database, change run status, or execute Python, including on a read-only server.

### Inspection in the CLI's shapes

`/list`, `/nodes`, `/history` and `/stats/{name}` answer with the `--json` body of
`barca list --all`, `barca status --all`, `barca history` and `barca stats`, byte for byte: the
server builds them with the same functions. A `--server` client applies `--limit` and
`--fields` itself. `/nodes` reads the server's ledger; `root` is the server's project root.
`next_fire` in `/list` is in the server machine's local time, as `barca list` prints it there.

`GET /project` describes the deployed tree: `api_version`, `root`, `env`, `deployed_at` (when the
server started), `commit` (read from `.git` at the root, `null` outside a git checkout) and each
served file with its `sha256`, hashed on each request so an edit shows at once.

### Health

```
GET /health
```

```json
{ "status": "ok", "version": "0.24.2", "api_version": 1, "env": "default", "store_uri": null,
  "read_only": false, "scheduler": true, "load_errors": [] }
```

`api_version` is the major version of this API (`1`); a client refuses another, and additive
fields do not bump it. `env` is the environment the server runs in. `store_uri` is the artifact
store the server writes to (`null` when artifacts stay on its machine), so a client can refuse
a mismatched `[remote]`. `scheduler` is `true` when this server fires `Schedule(...)` nodes: on by default, `false` with
`--no-schedule` or `--read-only`.

`load_errors` is an additive array of `{file, error, affected_nodes}`. `file` names
an unloaded source, `error` describes the failure, and `affected_nodes` names the
actual excluded node IDs (empty if syntax failure prevents identifying definitions).
Healthy source/definitions and their schedules remain available; unrelated nodes
in a valid file with a blocked dependent are retained. `/state` keeps its array
shape and contains only loaded nodes, with no synthetic error rows. These are
source/graph diagnostics; arbitrary Python import/execution failures remain run
errors. `--watch` repairs and removals refresh source diagnostics and scheduling.
A run of every asset refuses an empty loaded asset/sensor set with 400; excluded targets are
404. The UI displays these diagnostics above the normal healthy graph.

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

### Logs

```
GET /logs/{run_id}     → { "logs": [{ node_id, seq, line }, ...] }
```

The lines a run's steps printed, from the database once the run finished; an unknown id is an
empty list. A run in progress streams them as `log` events.

### Read-only mode

With `--read-only`, every route that runs, cancels or pauses (`POST /runs` other than a dry
run, `DELETE /runs/{run_id}`, the schedule pause and resume, and the older trigger routes)
returns `403`, the scheduler does not start, and every read goes through a private copy of the
metadata DB — the database is never opened in place, created or written.

### Assets

```
GET /assets            → [ListNode, ...]
GET /assets/{name}     → { "asset": ListNode, "stats": Stats }
```

`ListNode` is a `barca list --json` node: `{ id, kind, freshness, inputs, env }`, with
`freshness` a lowercase word (`always`, `manual`, `schedule`) and, for a scheduled node, the
cron expression in `schedule` and the next fire time in `next_fire` (local time). `{name}`
matches by function name or full node id; an unknown name returns `404`. `Stats` is the
`barca stats --json` body: `{ id, total_runs, cache_hit_rate, avg_elapsed_seconds,
median_elapsed_seconds, p95_elapsed_seconds, max_elapsed_seconds, recent_runs }`; the timings
are `null` until the node has run.

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
  `barca run <task>` does; the older `POST /run/{task}` alias recomputes every upstream asset.

Each scheduled run gets a `run_id`, is visible via `GET /runs/{run_id}`, and is written to
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
  "last_status": "complete",
  "last_skipped": 1780657200,
  "skipped_for": "7c0d5e2a91f4",
  "skip_reason": "in_flight",
  "paused": false,
  "paused_until": null
}
```

`next_fire` and `last_fired` are unix epoch seconds. `next_fire` is the next match of the cron
expression in the zone the server evaluates cron in (`--timezone`), so it is when the job will
fire; `next_run` in `GET /state` is computed the same way. `barca list` cannot know a server's
zone and always uses the local time of the machine it runs on. A job the scheduler has not seen before
gets `last_fired` set to the time the server first started with it, so it is not `null` even
though nothing has run. `last_run` is the most recent scheduled `run_id` and `last_status` its state
(`pending`/`running`/`complete`/`failed`/`cancelled`, or `null` if none yet).

A tick is skipped while a run of the same node is pending or running, whether the scheduler or
an HTTP request started it (a run of every asset and sensor counts for scheduled assets and
sensors, not tasks), and while the job is paused. `last_skipped` is the time of the last skipped
tick (unix epoch seconds), `skip_reason` why (`in_flight` or `paused`) and `skipped_for` the
`run_id` of the run that was in flight (`null` for a pause); they are `null` until a tick is
skipped, and a restart clears them.

Each scheduled run is recorded with `triggered_by: "schedule"`, the tick in `scheduled_at` and
the job's id in `schedule_id` (`GET /runs/{run_id}`, `barca history --json`).

### Pausing one schedule

```
POST /schedule/{id}/pause                       → the job's ScheduleEntry
POST /schedule/{id}/pause?until=2026-10-21T06:00:00Z
POST /schedule/{id}/resume                      → the job's ScheduleEntry
```

`{id}` is the scheduled node's full id (percent-encode a `/` in it) or its name. A paused job's
due ticks are skipped with `skip_reason: "paused"` (a line on stderr, and a `barca.run` span with
status `skipped` and reason `paused` when telemetry is on) until it is resumed or `until`
passes; `paused` and `paused_until` (unix epoch seconds, `null` for a pause until resumed) report
it. The ticks skipped while paused are not caught up. Resuming a job that is not paused changes
nothing.

A pause is kept in the metadata DB (`schedule_state`), so a restart, such as a deploy, does not
resume it. `until` must be an RFC 3339 time in the future (`400` otherwise); an id that is not a
scheduled node is `404`, a name that matches several `409`, and a read-only server answers `403`.
When the scheduler has not published the job (`--no-schedule`, or still starting) the answer is
`{id, paused, paused_until}`.

## Python client

`barca.get(..., server=True)` and `barca.run(..., server=True)` (or `server="<url>"`) run a
command on the server through the CLI's `--server` and return the value, as a local call does.
A parquet or pickle result is read from `GET /runs/{run_id}/output`.

`barca.Client` is **deprecated** in favour of them: constructing one emits a
`DeprecationWarning`, and it will be removed in a later minor release. The standard-library SDK
wraps this API:

```python
from barca import Client

c = Client("http://127.0.0.1:8274")
run = c.get("daily_report")       # POST /get/{target} (alias), returns immediately
result = run.wait(timeout=30)     # poll /status (alias) until complete/failed
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

`Client` still talks to the older routes, which remain as aliases for one minor release; its
statuses are those of `GET /status/{run_id}` (`complete`, not `success`).

## Errors

Every error body is the CLI's stderr error envelope ([CLI contract](/reference/cli-contract/)):

```json
{ "error": "Asset 'nope' not found. Available: pipeline.py:orders, pipeline.py:total",
  "code": 2, "kind": "usage",
  "remediation": "Run `barca list` to see available assets and tasks." }
```

`kind` and `code` are those a local command exits with (`usage` 2, `infra` 3). The HTTP status
follows the kind: `usage` is `400` (parse and DAG errors, a target of the wrong kind, a bad
request body), `404` for something that does not exist (an unknown node, run or route), `409`
for a name that matches several nodes or a request that conflicts with a run's state
(cancelling a finished run, the output of a run still going), and `403` for a run, cancel or
pause asked of a `--read-only` server; `infra` is `500`. A `405`, for a known path asked with the
wrong method, names the allowed methods in `error` and in the `Allow` header. A failed step is
never an HTTP error: it is a run whose outcome is `failed`. The Python client raises
`BarcaError` carrying the status and the `error` message for each of these.

## Limits

- No authentication and no TLS. The server listens on `127.0.0.1` by default; `--host` can
  make it reachable from other machines. Authenticate at a proxy.
- No shared history: `BARCA_STATE=off` is required with a remote store.
- Runs in progress are not resumed after a restart. A finished run's outcome is kept and
  `GET /runs/{run_id}` answers it; a run still queued at the restart never started.
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
