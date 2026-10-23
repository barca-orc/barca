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
inspection routes of the [server-mode RFC](/rfcs/0007-server-mode/) (section 3.4), and its
[ledger](#the-ledger) (#402). The CLI is a client of these routes: `barca get daily_report
--server <url>` runs on the server and prints what a local run prints
([Deploying](/deploying/#running-commands-on-the-server-from-another-machine)), and with a
server configured (`[server] url` or `BARCA_SERVER`) a laptop's `barca get` reads and posts to
the ledger routes over `http://` or `https://`.

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

The flags (`--host`, `--port`, `--watch`, `--no-schedule`, `--timezone`, `--read-only`,
`--drain-timeout`, `--env`) are in
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

With a remote store configured, `barca serve` backs its metadata DB up to the store every
minute it changed. See [Deploying](/deploying/#with-a-remote-store).

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
| `POST` | `/runs` | Start a run of `barca get` or `barca run` on the server. Body: [`RunRequest`](#starting-a-run). `202 {"run_id", "status", "position"}` (the run joins the [run queue](#the-run-queue): `running` when it started at once, or `pending` and its place, 1 = next), or `200` with the `--dry-run` JSON when `dry_run` is set. |
| `GET`  | `/runs/{run_id}` | The run: what started it, its status and, once finished, its [outcome](#outcomes), with its steps and captured lines. Survives a restart. At most 1000 steps (`?limit=N`, `?all=true`; failed steps first), with `steps_page: {total, truncated}`. |
| `GET`  | `/runs/{run_id}/events` | Server-sent events, each with an `id:`: `run_started`, `line`, `log`, `step_finished`, `run_finished`. |
| `DELETE` | `/runs/{run_id}` | Cancel a pending or running run. |
| `GET`  | `/runs/{run_id}/output` | The final artifact's bytes of a successful single-target run. |
| `GET`  | `/runs/{run_id}/logs` | What the run's steps wrote (stdout and stderr), so far while it runs: `{run_id, status, logs}`. `?step=` and `?stream=stdout\|stderr` filter as `barca logs` does. |
| `GET`  | `/runs?limit=100` | Recent durable runs, joined with queued and live server runs. |
| `GET`  | `/logs/{run_id}` | A run's captured output lines, persisted after it finishes. |

<a id="the-ledger"></a>**The ledger.** What a local `barca get` on another machine talks to (`barca docs server-mode`).

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/ledger/lookup` | Body `{"hashes": [{"node_id", "run_hash"}]}`. Answer `{"results": [...]}`, one per pair in order: `{"node_id", "run_hash", "exists": true, "path", "format", "size_bytes", "digest"}` for the latest successful result the ledger holds, or `exists: false`. By run hash only. |
| `POST` | `/ledger/runs` | Body: one finished run, `{"run": {run_id, command, files, target, status, steps_*, started_at, finished_at, elapsed_seconds, host}, "steps": [{node_id, run_hash, status, path, format, size_bytes, digest, ...}]}`. Every successful step must name an object in this server's store with its SHA-256 `digest`; the server checks each (a directory store by digest, an object store by size), then records the run as `triggered_by: client` with `client_run_id` and answers `201 {"run_id", "bytes_changed"}`. A missing, foreign or mismatched object is `409` with the envelope naming the node, and nothing is recorded. A run hash the ledger already holds with the same digest adds nothing; with another digest the ledger keeps its row and lists the step in `bytes_changed`. A repeated post of the same `run.run_id` answers with the run already recorded. `400` without a store, `403` read-only, `503` while shutting down. |

**Inspection.** The body is the CLI command's `--json` output, built by the same function, so a
client prints it as it would print the local command's.

| Method | Path | Same as |
|--------|------|---------|
| `GET`  | `/list` | `barca list --json --all` |
| `GET`  | `/nodes?targets=a,b&sample=N` | `barca status --json --all`, for the server's ledger |
| `GET`  | `/history?limit=N&all=1` | `barca history --json` (`-l N`, `--all`) |
| `GET`  | `/stats/{name}` | `barca stats --json` |
| `GET`  | `/sql/views?files=a.py,b.py` | Which result each `barca sql` view reads in the ledger: `{"views": [{name, node, kind, partitioned, format, files: [{path, partition, content_hash}], stale, run_hash}], "missing": [{name, node, kind, partitioned, command, keys_unknown?}], "duplicates": [{name, ids}]}`, by the function a local `barca sql` uses. The client runs the query; the server runs no SQL. |
| `GET`  | `/materializations?limit=N&offset=M` | Step results, newest first: `{"materializations": [{node_id, partition, run_id, run_hash, status, created_at, elapsed_seconds, cpu_seconds, max_rss_bytes, attempts, format, size_bytes, artifact, content_hash, error_type, error_message}], total, limit, offset}`; `limit` defaults to 1000, at most 10000. `barca sql`'s `barca.materializations`. |
| `GET`  | `/project` | The deployed tree: `{api_version, root, env, deployed_at, commit, files: [{path, sha256}]}` |
| `GET`  | `/health` | Liveness (or `draining`), `api_version`, `env`, the artifact store (`store_uri`), read-only, scheduler, load diagnostics, the cache size against its limit, `started_at` (changes on a restart) and `queue: {paused, queued, running, max_runs}`. |

**The served graph and the UI.**

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/state` | Every node: its `barca status` entry plus typical durations, next run and the run working on it now. |
| `POST` | `/state/reset` | Start the metadata database over, as `barca state reset`: it moves aside to `metadata.db.reset-<UTC>`, nothing is deleted. See [Resetting the database](#resetting-the-database). |
| `GET`  | `/groups` | Organizational groups as `[{id, name, description, members, output}]`; separate from execution edges. |
| `GET`  | `/assets` | Every node as `barca list --json` shows it. |
| `GET`  | `/assets/{name}` | One node's `barca list --json` entry with its `barca stats --json` body and the run working on it now. |
| `GET`  | `/assets/{name}/schema` | Selected node and direct inputs as `NodeStatus[]`, with artifact shapes inspected on demand. |
| `GET`  | `/plan` | Execution plan (phases and streams) as JSON. |
| `GET`  | `/schedule` | Scheduled jobs with next fire time, last run, last skipped tick and pause. |
| `POST` | `/schedule/{id}/pause` | Hold one schedule back, optionally `?until=<RFC 3339>`. |
| `POST` | `/schedule/{id}/resume` | Let a paused schedule fire again. |
| `GET`  | `/queue` | The [run queue](#the-run-queue): `{paused, paused_at, draining, max_runs, running: [...], queued: [...]}`, each run `{run_id, status, position, command, target, triggered_by, schedule_id, queued_at}`. |
| `POST` | `/queue/pause` | Hold every queued run back until resumed; runs in flight go on. Kept across restarts. Answers the queue; `403` read-only. |
| `POST` | `/queue/resume` | Let queued runs start again, in order. Answers the queue; `403` read-only. |
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
hour after it finished, while finished runs hold at most 64 MB of memory; past that the oldest
go first, never the run that finished last) and a lagging subscriber reads on from where it was. A connection that
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
  "outcome": { "status": "success", "run_id": "1b942bb33182", "final_output": {"_barca_artifact": {"path": ".barca/default/artifacts/pipeline.py:daily_report/9f2c.json", "format": "json", "size_bytes": 8}}, "steps": [], "...": "..." },
  "error": null,
  "steps": [{ "node_id": "pipeline.py:daily_report", "status": "success", "elapsed_seconds": 0.4, "error": null }],
  "logs": [{ "node_id": "pipeline.py:daily_report", "seq": 0, "line": "wrote 3 rows", "stream": "stdout", "ts": "2026-10-20T05:00:00.512Z" }]
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

- `success`: the exact `barca get --json` / `barca run --json` stdout body. For `get`,
  `final_output` is the artifact pointer (`{"_barca_artifact": {"path", "format", "size_bytes"}}`,
  a path on the server), and for a partitioned target an object of them keyed by partition key,
  as `barca get --json` prints it (past 1000 keys, the `_barca_partitions` summary that lists
  only the keys the run computed); for `run` it is the value for a JSON artifact and an artifact
  pointer for binary formats. Several targets carry `targets`, in the order given.
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

`GET /runs?status=running` lists only the runs in flight, newest first: this server's pending
and running runs, and the `barca get` / `barca run` commands running on the same `.barca/`
whose process is alive (the check the scheduler uses to skip a tick). Each has `active_steps`,
the nodes it is working on now as `[{node_id, step, partitions}]` (see `running` under
[State](#state)). Any other `status` is a `400`.

Each summary has `id` and `run_id` (the database run id), `handle` (the same id while the
server keeps the run in memory, `null` otherwise), `command`, `files`, `target`, `status`,
`triggered_by`, `scheduled_at`, `schedule_id`, UTC `started_at` and `finished_at`,
`elapsed_seconds`, `steps_total`, `steps_executed`, `steps_cached`, and `error`; with
`?status=running`, also `active_steps`.

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
{ "status": "ok", "version": "0.27.0", "api_version": 1, "env": "default", "store_uri": null,
  "read_only": false, "scheduler": true, "scheduler_enabled": true, "metadata_error": null,
  "load_errors": [], "cache": { "size_bytes": 1830000000, "limit_bytes": 20000000000 } }
```

`status` is `ok`, or `draining` from the first SIGTERM or SIGINT until the server exits (see
[the drain](#stopping-the-drain)); the response is `200` either way.

`api_version` is the major version of this API (`1`); a client refuses another, and additive
fields do not bump it. `env` is the environment the server runs in. `store_uri` is the artifact
store the server writes to (`null` when artifacts stay on its machine), so a client can refuse
a mismatched `[remote]`.

`cache` (from 0.25, experimental) is `.barca/` against its size limit as the server last
measured it, after a run or on its once-a-minute check (see
[Cache size](/reference/config/#cache-size)); `null` before the first measurement, with
`[cache].max_size` off, and on a `--read-only` server, which deletes nothing. A run result
gains `cache: {size_bytes, limit_bytes, evicted_files, evicted_bytes}` when the check after it
found `.barca/` over its limit.

`scheduler` is `true` when this server fires `Schedule(...)` nodes: on by default, `false` with
`--no-schedule` or `--read-only`. `scheduler_enabled` (from 0.28) is the same, and `false` once
the scheduler stopped on an error; `metadata_error` then says why.

`metadata_error` (from 0.28) is why the metadata DB cannot be used, while it cannot, and `null`
otherwise. A writer server sets its database up before it binds, and exits 3 when it cannot,
so on a writer it is set only when the scheduler stopped on a database error later. A
`--read-only` server whose database is unusable (empty, not SQLite, a newer format, a malformed
schema) keeps serving and reports it here; it goes back to `null` once a read succeeds again.

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
fire time in unix seconds for scheduled nodes. `cache` is the `barca status` cache entry: `state` is `cached`, `unsynced`, `stale`, `never_run`, `partial`,
`unknown` or `always_runs`, with a machine-readable `reason` and a `detail` in words (see
[`barca status`](/reference/cli/#status)). `shape` is always `null` here:
reading artifact shapes starts a reader process, too slow for an endpoint the UI polls. The cache
check reads a private copy of the metadata DB, so this endpoint never writes it.

`running` is the run working on the node now, whoever started it, or `null`:

```
"running": {
  "run_id": "0eeee4d342ed",
  "triggered_by": "schedule",          // schedule, api, client, manual (a CLI run), or null
  "started_at": "2026-10-21T10:00:00.000Z",
  "step": "running",                   // or "queued"
  "partitions": { "running": 6, "done": 4, "total": 10 }   // null for an unpartitioned node
}
```

It covers this server's runs (the scheduler's, `POST /runs`, `barca get --server`) and the
`barca get` / `barca run` commands on the same `.barca/` whose process is alive. A server run
covers its targets and the nodes upstream of them; a node is `queued` while the run waits for a
run slot or for the node's upstream steps, and `running` once its step executes, until its
`step_finished`. A CLI run streams nothing to the server: it covers only its targets, as
`running`, until each records a materialization. For a partitioned node, `done` is the keys
with a result (cached, or finished by this run), `total` every key, and `running` the keys not
done yet while the step runs. When several runs work on one node, the one whose step is running
wins, then the oldest. `GET /assets/{name}` has the same `running` field. The UI polls this
endpoint every 2 s while any node is running (10 s otherwise) and shows the runs on every page,
whichever tab or client started them.

### Polling cheaply: what the server keeps

The inspection routes (`/state`, `/nodes`, `/assets`, `/assets/{name}`, `/list`, `/history`,
`/stats/{name}`, `/runs`, `/runs/{run_id}`, the log routes, `/schedule`, `/plan`, `/groups`,
`/project` and `/health`) answer every `GET` with an `ETag`, a digest of the body, and
`Cache-Control: no-cache`. A request whose `If-None-Match` names that `ETag` gets
`304 Not Modified` and no body while the body is the same. A browser does this by itself, so the
UI's polls of an unchanged project transfer nothing.

Between requests the server keeps what the answers are computed from, each with the evidence
that it is still right:

- the graph it decides cache states with, until a pipeline file, a helper module a step reads
  or a path an import was looked up at changes (or `--watch` sees an edit). A graph with keys
  from `partitions(<expression>)` is loaded again at least every 30 seconds, since the
  expression can read anything;
- one private copy of the metadata DB for every request, until the database or its WAL
  changes on disk;
- each `barca status` result (`/state`, `/nodes`, the `running` fields), until the graph or the
  database changes. An answer kept for more than 30 seconds is computed again in the
  background, which bounds how long an artifact deleted by hand, outside barca, still shows as
  `cached`.

A finished run, a run on this project by the CLI, a ledger post or an edit is in the next answer.
A file is trusted to be unchanged only once it has been still for a moment (2 seconds for source
files, a tenth of a second for the database), so a request right after a write computes its
answer afresh. While a client polls `/state` (the UI does every 10 seconds while open), the
server computes it again in the background once a change has settled and no run of the server
is going, so the poll finds it ready.

### Resetting the database

```
POST /state/reset      → { database, moved_to, moved, undo }
```

What the web UI's **Reset database** button calls (on the "Can't load pipeline" panel, and in
the sidebar), behind a dialog that says what happens and how to undo it. It works as `barca state
reset` does: under the database's lock, the metadata database and its `-wal` and `-shm` move to
`metadata.db.reset-<UTC>` (and `<that>-wal`, `<that>-shm`) in the same directory, and an empty
database takes its place, so the next request sees an empty history. Nothing is deleted;
artifacts are left alone. A database that cannot be opened or migrated (the case that makes
`/state` fail with `Database error: ...`) moves as it is. The server keeps no connection open
between requests, so the next one opens the new file; the paused schedules and the finished runs
it remembered are cleared with the database.

```json
{
  "database": ".barca/metadata.db",
  "moved_to": ".barca/metadata.db.reset-2026-10-20T18-02-11.482Z",
  "moved": [".barca/metadata.db.reset-2026-10-20T18-02-11.482Z", ".barca/metadata.db.reset-2026-10-20T18-02-11.482Z-wal"],
  "undo": "barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z"
}
```

`moved_to` and `undo` are `null` when there was no database. Undo with `undo`, run with the
server stopped. `409` while a run is pending or running (this server's, or a local run's on this
machine), naming it, with nothing changed; `403` under `--read-only`; `503` while the server
drains. There is no authentication (`barca serve` has none by design): anyone who can open the UI
can reset.

### Logs

```
GET /runs/{run_id}/logs?step=report&stream=stderr
  → { "run_id": "...", "status": "running", "logs": [{ node_id, seq, line, stream, ts }, ...] }
GET /logs/{run_id}     → { "logs": [{ node_id, seq, line, stream, ts }, ...] }
```

The lines a run's steps wrote: `stream` is `stdout` or `stderr` (Python `logging`, warnings,
`sys.stderr`, and the traceback of each failed attempt), `ts` when the line was written (RFC
3339 UTC with milliseconds; `null` on lines recorded before 0.29.2). `GET /runs/{run_id}/logs`
is what `barca logs` reads: a run in progress answers with its lines so far, an unknown run is
`404`, and a `step` that matches nothing (or two definitions) or a `stream` that is neither is
`400`. The older `GET /logs/{run_id}` answers from the database once the run finished; an
unknown id is an empty list. A run in progress streams its lines as `log` events, each with its
`stream` and `ts`.

### The run queue

Every run the server accepts (`POST /runs`, the older trigger routes, the UI, a `--server`
client, a scheduled tick) is recorded in the metadata DB (`run_queue`) under its run id before
the server answers. Runs start in the order they were accepted, at most `--max-runs` at once
(`[server] max_runs`, default one per core), while the queue is neither paused nor draining; the
others are `pending`. A run leaves the queue once it has recorded how it ended; `DELETE
/runs/{run_id}` takes a queued run off it and records it `cancelled`.

`POST /queue/pause` holds every queued run back; runs in flight finish, new runs and scheduled
ticks still join the queue (one tick per node waits; the next are skipped as `in_flight`). The
pause is kept in `server_state`, so a restart keeps it. `POST /queue/resume` lets them start
again. `barca queue [pause|resume] --server` calls these routes.

When the server starts it resumes the queued runs, in order and under their run ids, before its
scheduler's catch-up (`[barca] resuming N queued runs`). A run the previous server had started
and never finished recording (it was killed) is not started again: it is reported on startup and
recorded as interrupted.

### Stopping: the drain

The first SIGTERM or SIGINT starts a drain. Until the server exits:

- No queued run starts. `POST /runs` and the older `POST /run`, `POST /run/{target}` and
  `POST /get/{target}` still accept runs (`202`, `status: "pending"`): they wait in the queue
  for the next server. `GET /queue` says `draining: true`.
- `POST /state/reset` and `POST /ledger/runs` return `503` with the error envelope (`kind:
  infra`) and a `Retry-After` header: the drain timeout in seconds, at least 1.
- `GET /health` returns `200` with `"status": "draining"`.
- The scheduler stops; a tick that falls in the drain starts no run, and the next server's
  startup catch-up fires the job once.
- Reads, `GET /runs/{run_id}`, `/runs/{run_id}/events`, `DELETE /runs/{run_id}` and their
  older aliases keep working, and the runs in flight finish, artifact uploads included.

The drain lasts until every run in flight has finished (exit 0), for at most `--drain-timeout`
seconds (`[server] drain_timeout` in `barca.toml`, default 30). When the deadline passes first,
the runs left are stopped and put back at the head of the queue: the next server starts them
again from the beginning under the same run ids (at least once: a step that was interrupted runs
again), and the server exits 0. A second SIGTERM or SIGINT cancels them instead, as by `DELETE
/runs/{run_id}`, with `"error": "run cancelled: server shutdown"`, the line `[barca] run
cancelled: server shutdown` in their logs and `barca.reason: server shutdown` on their
`barca.run` span; the server exits 1. The queued runs are kept either way. Open `/events` streams
end once the runs are over, without `run_finished` for a run still queued; the CLI's clients
reconnect to the next server. See [Deploying](/deploying/#restarting-without-losing-work) for
supervisor grace periods.

### Read-only mode

With `--read-only`, every route that runs, cancels, pauses or resets (`POST /runs` other than a
dry run, `DELETE /runs/{run_id}`, the schedule pause and resume, `POST /state/reset`, and the older
trigger routes)
returns `403`, the scheduler does not start, and every read goes through a private copy of the
metadata DB — the database is never opened in place, created or written.

### Assets

```
GET /assets            → [ListNode, ...]
GET /assets/{name}     → { "asset": ListNode, "stats": Stats, "running": NodeRunning | null }
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
GET /plan              → { total_steps, pool_size, capacity, capacity_source, weights, phases: [{ reason: {type, node_id?}, streams: [{ stream_id, steps }] }], warnings: [{ kind, node, param, message }] }
```

`warnings` is always present: the plan-time warnings for every step of the project, `[]` when
there are none (the same items as `result.warnings` above).

## Scheduling

`barca serve` runs a cron scheduler for nodes declared with `freshness=Schedule("...")`. It is
on by default; pass `--no-schedule` to turn it off.

Each second, every job whose cron matches triggers a run through the same run pool as the
`POST` endpoints. The run's targets are the scheduled node and every `Always` node below it; the
walk down goes past a `Manual` node, which is not updated, and stops at a node with its own
`Schedule`. Each target is brought up to date, not forced:

- **The scheduled node.** A sensor is polled; an asset and its upstream cone are checked
  against the cache; a task runs, reusing cached upstream assets as `barca run <task>` does
  (the older `POST /run/{task}` alias recomputes every upstream asset).
- **`Always` assets below** are computed when they have no cached result for their current run
  hash.
- **`Always` tasks below** run when their inputs changed since their last successful run (code,
  an asset input's result, a sensor input's value), when a task they read ran successfully
  since, or when they never ran.
- **`Manual` nodes** are read as they are, never run by a tick. When one has never been
  computed, the nodes that read it are reported as `skipped` steps with reason `blocked` (and
  as `blocked` targets), and the tick is not an error on that account.

A tick with `Always` nodes below its scheduled node is a multi-target run: its outcome in
`GET /runs/{run_id}` has a `targets` entry per target and `steps` for all of them. A failing
node stops only what depends on it; the other targets still run, and the run's `status` is then
`failed`, its error envelope naming the first node that failed. `POST /runs` reads a `Manual`
node as it is, as `barca get` and `barca run` do; the older `POST /get/{node}`,
`POST /run/{task}` and `POST /run` compute `Manual` upstreams as before.

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
  "paused_until": null,
  "failures": 0,
  "max_failures": 5,
  "failing": false
}
```

`next_fire` and `last_fired` are unix epoch seconds. `next_fire` is the next match of the cron
expression in the zone the server evaluates cron in (`--timezone`), so it is when the job will
fire; `next_run` in `GET /state` is computed the same way. `barca list` cannot know a server's
zone and always uses the local time of the machine it runs on. A job the scheduler has not seen before
gets `last_fired` set to the time the server first started with it, so it is not `null` even
though nothing has run. `last_run` is the most recent scheduled `run_id` and `last_status` its state
(`pending`/`running`/`complete`/`failed`/`cancelled`, or `null` if none yet).

A tick is skipped while a run of the same node is pending or running, whether the scheduler, an
HTTP request or a `barca get` / `barca run` on the same project (`.barca/`, its process alive)
started it (a run of every asset and sensor counts for scheduled assets and
sensors, not tasks), and while the job is paused. `last_skipped` is the time of the last skipped
tick (unix epoch seconds), `skip_reason` why (`in_flight` or `paused`) and `skipped_for` the
`run_id` of the run that was in flight (`null` for a pause); they are `null` until a tick is
skipped, and a restart clears them.

`failures` (integer) is the number of consecutive failed ticks of the node, `max_failures`
(integer) its cap from `Schedule(cron, max_failures=N)` (default 5), and `failing` (boolean)
whether the count has reached the cap. While `failing` is `true` the node's ticks are skipped,
and the server logs `[barca] pipeline.py:inbox failing: 3 consecutive failed ticks
(max_failures=3); skipped until an explicit request or a change in its inputs`. The count is
kept in the metadata DB and survives a restart. It resets on a successful tick, and when the
scheduled node, a node that failed, or a direct input of a failed node succeeds in another run
(an explicit request or a change in its inputs). A tick skipped while the job is paused or while
a run of its node is in flight is not a failed tick and is not counted. A failing job that is
also paused reports `skip_reason: "paused"`.

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

`kind` and `code` are those a local command exits with (`usage` 2, `infra` 3). `reason` is
present only on an error a client may handle on its own. `metadata_unusable` (from 0.28) means
the metadata DB is there but this barca cannot use it; `error` names the file and `remediation`
says what to do, instead of a SQL error such as `Parse error: no such column`:

```json
{ "error": "the metadata DB .barca/envs/prod/metadata.db cannot be used: metadata schema version 2 is not supported by barca 0.28.0 (supports version 1 and legacy version 0); use a compatible Barca release. The database was not migrated or reset",
  "code": 3, "kind": "infra", "reason": "metadata_unusable",
  "remediation": "Not a problem in your code. If a newer barca wrote this database, upgrade barca. Otherwise put a backup back with `barca state restore` (it moves .barca/envs/prod/metadata.db aside first), or start an empty history with `barca state reset` (the Reset database button in the web UI of a running `barca serve`), which moves .barca/envs/prod/metadata.db aside and prints how to undo it." }
```

The UI shows the remediation under "Can't load pipeline", above its Reset database button.
 The HTTP status
follows the kind: `usage` is `400` (parse and DAG errors, a target of the wrong kind, a bad
request body), `404` for something that does not exist (an unknown node, run or route), `409`
for a name that matches several nodes or a request that conflicts with a run's state
(cancelling a finished run, the output of a run still going), and `403` for a run, cancel or
pause asked of a `--read-only` server; `infra` is `500`, and `503`, with a `Retry-After`
header, for a reset or a ledger post asked of a server that is draining before it stops. A `405`, for a known path asked with the
wrong method, names the allowed methods in `error` and in the `Allow` header. A failed step is
never an HTTP error: it is a run whose outcome is `failed`. The Python client raises
`BarcaError` carrying the status and the `error` message for each of these.

## Limits

- No authentication and no TLS. The server listens on `127.0.0.1` by default; `--host` can
  make it reachable from other machines. Authenticate at a proxy.
- One server per project. Machines share results by pointing at it ([the ledger](#the-ledger));
  a second server on the same project, or a bucket on its own, shares nothing (RFC-0007).
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
For partitioned nodes this describes the key `barca status` reports (the latest run's first key in sort order), named in
`last_materialization.partition`, rather than a union of every key's schema.
No sample values are returned. Nested JSON fields report their container type (`dict` or `list`); this is not a recursive schema. Pickle types or constructors are identified from opcodes without executing them, and pickle column schemas are unavailable. This endpoint is available in read-only mode;
unknown and ambiguous names return 404 and 409 respectively.
