---
title: Server API
description: Start, follow and cancel runs on barca serve over HTTP, read their outcomes, and call it from Python. Every route is on Server API Endpoints.
---

`barca serve` starts an HTTP server that runs assets and tasks on request, fires
`Schedule(...)` nodes on their cron, and serves a web UI at `/ui/`.

The server binds to `127.0.0.1`, and authentication is off by default: anyone who can reach the
port can start and cancel runs. To reach it from another machine, turn on [project tokens](/auth/)
(`--auth token`; requests then carry `Authorization: Bearer <token>`), put a reverse proxy that
authenticates in front of it, or both (see [Deploying](/deploying/#authentication)).

Runs execute inside the server process, in the background. `POST /runs` answers at once with the
run's id, which is its database run id (the one `barca history` shows); the run's events stream
from `GET /runs/{run_id}/events`, and `GET /runs/{run_id}` answers from `.barca/metadata.db`
once it has finished, so the answer survives a restart.

## Quick start: a run from a script

```bash
curl -s -X POST localhost:8274/runs -H 'Content-Type: application/json' \
  -d '{"command": "get", "targets": ["customers"]}'
# {"position":null,"run_id":"81c4f8e97588","status":"running"}

curl -s localhost:8274/runs/81c4f8e97588           # status, then its outcome when finished
curl -s localhost:8274/runs/81c4f8e97588/output    # the result's bytes: [0, 1, 2, ...]
curl -sN localhost:8274/runs/81c4f8e97588/events   # or follow it live (server-sent events)
```

With `--auth token`, add `-H "Authorization: Bearer $BARCA_SERVER_TOKEN"`
([Authentication and Tokens](/auth/)). From Python, `barca.get("customers", server=True)` does
the same and returns the value ([Python client](#python-client)).

| I want to… | Read |
|---|---|
| Start a run and the fields it takes | [Starting a run](#starting-a-run) |
| Follow a run live, or reconnect to it | [Following a run](#following-a-run) |
| Know whether it succeeded, and what it returned | [Outcomes](#outcomes) |
| Cancel a run | [Cancelling a run](#cancelling-a-run) |
| Check the server from a load balancer or monitor | [Health](/reference/server-api-endpoints/#health) |
| List, pause or resume schedules | [Scheduling](/reference/server-api-endpoints/#scheduling) |
| See every route | [Endpoints](/reference/server-api-endpoints/#endpoints) |
| Handle errors | [Errors](#errors) |

## Starting the server

```bash
barca serve pipeline.py                   # one file, default port 8274
barca serve pipeline.py --host 0.0.0.0    # every interface (containers, VMs)
barca serve --port 8400 --timezone utc    # every file in the project
```

The flags (`--host`, `--port`, `--watch`, `--no-schedule`, `--timezone`, `--read-only`,
`--drain-timeout`, `--max-runs`, `--capacity`, `--run-timeout`, `--auth`, `--env`) are in
the [CLI reference](/reference/cli/serve/). On start the server prints its environment, the
schedule and its address on stderr:

```
[barca] env default (built-in default); artifact store local (.barca/artifacts); metadata .barca/metadata.db (not created yet)
[barca] scheduling 1 asset:
  pipeline.py:daily — 0 5 * * * (next 2026-11-10 05:00:00)
[barca] serving on http://127.0.0.1:8274  (1 file)
```

The default bind address is `127.0.0.1`. `--host` takes an IP address, including `::` for
IPv6; hostnames are rejected. `0.0.0.0` listens on every IPv4 interface. With authentication
off (the default) anyone who can reach the port can trigger runs; a non-loopback address prints
a startup warning. Turn on [project tokens](/auth/), or
use a private network or an authenticating proxy.

`--watch` re-parses the DAG when a source file changes, so `/assets` and `/plan` reflect edits
without a restart. Files added after the server started are not picked up until a restart, with
or without `--watch`.

With a remote store configured, `barca serve` backs its metadata DB up to the store every
minute it changed. See [Deploying](/deploying/#with-a-remote-store).

## Runs

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
  "batch": ["scored=1000"],     // optional --batch overrides for nodes declaring batch=
  "partition": "region=emea",   // optional --partition, as the flag takes it (a JSON object or array for exact keys)
  "values": false,              // get -o value: final_output holds the values, not artifact pointers
  "allow_paused": false,        // --allow-paused: run a target that `barca pause` paused
  "agent": true                 // which stderr lines the client will print
}
```

A partitioned `run` returns every key's value in `outcome.final_output`, keyed by partition
key, with tables as JSON rows. The same bounded summary as `get` applies above 1000 keys.

Only `command` is required. The fields are the flags of `barca get` and `barca run`
(`--refresh`, `--refresh-all`, `--no-cascade` is `"cascade": false`, `--dry-run`, `--agent`).
The optional `batch` array takes repeatable `--batch` values (`all`, `none`, a positive
integer, or `NODE=VALUE`). It overrides grouping only for nodes that declare `batch=`.
Invalid values or names are `400` before a run starts. A dry run adds `{grouping, keys, calls}`
to each affected step.

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
command line, with `triggered_by: "api"`. A run that exceeds the run timeout (`--run-timeout`
or `[server] run_timeout_seconds`; no limit by default) is stopped and ends with an `error`
outcome.

### Following a run

```
GET /runs/{run_id}/events   → text/event-stream
```

Each event is a server-sent event whose `data:` is JSON with a `type`, and whose `id:` is
`<attempt>:<n>`: the run attempt and the event's number in it, from `1`:

| `type` | Payload | Meaning |
|---|---|---|
| `run_started` | `run_id` | The run left the queue. |
| `line` | `line`, `when` | A stderr line exactly as a local run prints it: `--agent` step lines (`when: "agent"`), warnings, `[barca]` notes and the end-of-run line (`when: "always"`). A client prints the lines its own mode would. |
| `log` | `node_id`, `line` | A line a step printed. |
| `step_finished` | `node_id`, `ok`, `cached?`, `elapsed_seconds?`, `error?`, `import_seconds?` | A step ran, failed, or was served from cache (`cached: true`). |
| `run_finished` | `run_id`, `ok`, `outcome` | The run's [outcome](#outcomes). The stream ends after it. |

A subscriber without `Last-Event-ID` receives the run from its first event whenever it connects,
and every event once, however slowly it reads: the server keeps a run's events in memory while the
run is live (until an hour after it finished) within bounded retention, at most 65,536 events or
16 MiB per attempt and 64 MiB across the server; past a bound the oldest frames go first, and a
lagging subscriber that reaches a gap is closed rather than skipping it. Each event carries an
attempt-scoped `id`. A connection that drops mid-run is resumed by reconnecting with
`Last-Event-ID` set to the last id processed: the stream continues strictly after it (`204` when
that id was the run's last event). A malformed id is `400`, an id from another attempt of the run
(a requeued or recovered attempt) is `409`, and an id whose events have expired is `410`; the
CLI and the UI then fall back to polling `GET /runs/{run_id}` and say the live transcript is
incomplete. Specification: `specs/behavior/server-event-replay.md` (shipped in 0.36.0,
[RFC-0016](/rfcs/0016-server-tokens-and-stream-resume/)). The run goes on whether anyone is
reading. Once the run is no longer kept, the stream is a `404`; `GET /runs/{run_id}` answers from
the database then.

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
Each row has `first_failure`: `{ node_id, error }`, the run's first failed step (with its partition
key) and the first line of its error, or `null` when no step failed. `error` is only a run-level
error, so a run whose steps failed usually has `first_failure` and `error: null`.

`GET /runs?status=running` lists only the runs in flight, newest first: this server's pending
and running runs, and the `barca get` / `barca run` commands running on the same `.barca/`
whose process is alive (the check the scheduler uses to skip a tick). Each has `active_steps`,
the nodes it is working on now as `[{node_id, step, partitions}]` (see `running` under
[State](/reference/server-api-endpoints/#state)). Any other `status` is a `400`.

Each summary has `id` and `run_id` (the database run id), `handle` (the same id while the
server keeps the run in memory, `null` otherwise), `command`, `files`, `target`, `status`,
`triggered_by`, `scheduled_at`, `schedule_id`, UTC `started_at` and `finished_at`,
`elapsed_seconds`, `steps_total`, `steps_executed`, `steps_cached`, `error` and `first_failure`; with
`?status=running`, also `active_steps`.

The UI's **Runs** view lists this history and shows what started each run; each run opens its
status, timing, steps, errors and logs.

These inspection endpoints always read a private DB snapshot. They do not create or migrate
the project's database, change run status, or execute Python, including on a read-only server.

## Python client

`barca.get(..., server=True)` and `barca.run(..., server=True)` (or `server="<url>"`) run a
command on the server through the CLI's `--server` and return the value, as a local call does.
A parquet or pickle result is read from `GET /runs/{run_id}/output`.

`barca.Client` is **deprecated** in favour of them: constructing one emits a
`DeprecationWarning`. There is no scheduled removal; a removal migrates `Client` off the deprecated
routes first and lands as a breaking change in a minor release. The standard-library SDK
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

`Client` still talks to the older routes, retained as deprecated compatibility aliases; its
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

Every route, with health, state, the run queue and scheduling, is on
[Server API Endpoints](/reference/server-api-endpoints/).
