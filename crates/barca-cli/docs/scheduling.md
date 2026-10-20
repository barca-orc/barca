# Scheduling and `barca serve`

`barca serve` currently runs deployed code on one machine and exposes the HTTP API,
web UI and cron scheduler. Use the HTTP API (`POST /runs`) or `barca.Client` to trigger remote
work. Ordinary `barca get` and `barca.get()` run locally. The server half of RFC-0007's run
contract is implemented (#393); CLI `--server`/`--local`, server URL configuration (#394) and
laptop runs against the server's ledger (#402) are not yet.
Deployment instructions: https://barca.sh/deploying/.

Freshness is recorded on every node. Only `Schedule(...)` starts automatic runs:

| Freshness | Meaning |
|---|---|
| `Always` (default for assets/tasks) | Recorded and displayed; no automatic runs or special cache policy. |
| `Manual` (default for sensors) | Recorded and displayed; no refresh barrier. A triggered command treats it like `Always`. |
| `Schedule("<cron>")` | Fires on a cron schedule while `barca serve` is running. |

Automatic `Always` reconciliation and `Manual` barriers are proposed in RFC-0008
(https://github.com/barca-orc/barca/pull/276); they are not implemented.

```python
from barca import asset, task, sensor, Schedule


@asset(freshness=Schedule("0 5 * * *"))          # 5-field cron: daily at 05:00
def daily_report() -> dict:
    return {"rows": 1}


@task(freshness=Schedule("*/15 * * * * *"))      # 6-field cron: every 15 seconds
def heartbeat() -> None:
    print("tick")
```

- Cron has 5 fields (`minute hour day-of-month month day-of-week`) or 6 with a leading seconds
  field. The scheduler evaluates at 1-second resolution. There is no year field.
- Schedules only fire while `barca serve` is running; `barca get` never fires them.
- `Schedule` takes the cron expression by position. `Schedule(cron="0 5 * * *")` and any other
  keyword are errors when the file is read, exit 2 (`barca docs assets`, "Accepted arguments").

A tick brings the node up to date. It does not force it to recompute:

- A scheduled **asset** is checked on every tick. Sensors upstream of it are polled, and each
  asset in its cone runs only if there is no cached result for its current code and inputs
  (`barca docs cache`). If there is one, the asset is served from cache and its function does
  not run.
- A scheduled **task** runs on every tick. Its upstream assets are checked the same way, as
  with `barca run <task>`.
- A scheduled **sensor** is polled on every tick. Its tick does not trigger consumers;
  schedule or explicitly trigger the downstream asset/task to process its output.

So outside data has to come in through a sensor (`barca docs cache`, "External data that changes
in place"). A scheduled asset that fetches data in its own body, with no sensor upstream, has
nothing on its input side that can change: it is computed once and then served from cache on
every tick, until its code or a declared `env=` variable changes or it is named in `--refresh`.
The same goes for a plain asset that a scheduled task reads.

The cache is keyed by the sensor's value, not by time. A sensor that returns to a value it had
before (a row count, a status flag) brings back the result computed for that value. Return
something that identifies the version of the data, such as an etag or a last-modified time.

A scheduled run is recorded with `triggered_by: "schedule"`, the tick it was started for
(`scheduled_at`) and the schedule (`schedule_id`, the node's id), in `barca history --json`,
`GET /runs/{run_id}` and the `barca.run` span (`barca docs telemetry`).

The older `POST /run/<task>` alias is different from a tick: it recomputes every upstream asset.

A tick is skipped, not queued, while a run of the same node is pending or running, whoever
started it: an earlier tick, `POST /runs` naming the node, or a `POST /runs` of every asset and
sensor (it does not hold back a scheduled task). The skip is a line on stderr naming the run
in flight, `last_skipped`, `skip_reason: "in_flight"` and `skipped_for` in `GET /schedule`
(kept in memory: a restart clears them), and, with telemetry on, a `barca.run` span with status
`skipped` (`barca docs telemetry`).
The next tick after the run ends fires as usual. Only a run of the node itself counts: a run of
a node downstream of it does not hold the tick back, though it computes the same upstream cone.
Two scheduled nodes that share an upstream run separately, and both may compute it.

```text
[barca] scheduled run pipeline.py:orders skipped — run 1b942bb33182 of the same node still in flight
```

## Pausing one schedule

To hold one job back for a maintenance window or a planned manual rebuild, pause it; the rest of
the schedule keeps firing. The id is the node's full id (percent-encode a `/` in it) or its name:

```bash
curl -X POST http://127.0.0.1:8274/schedule/daily_report/pause                              # until resumed
curl -X POST 'http://127.0.0.1:8274/schedule/daily_report/pause?until=2026-10-21T06:00:00Z'  # until a time (RFC 3339)
curl -X POST http://127.0.0.1:8274/schedule/daily_report/resume
```

Each answers the job's `GET /schedule` entry, which reports `paused` and `paused_until` (unix
epoch seconds, `null` for a pause until resumed). While a job is paused its due ticks are
skipped: a line on stderr, `skip_reason: "paused"` in `GET /schedule`, and with telemetry on a
`barca.run` span with status `skipped` and reason `paused`. A pause ends at `until`, or when the
job is resumed; the ticks skipped while it was paused are not caught up.

A pause is kept in the metadata DB (`schedule_state`), so a restart, such as a deploy, does not
resume a job someone paused. A pause on a node that is no longer scheduled is kept and applies
again if the schedule comes back. A `--read-only` server refuses both routes with `403`.

Cron is evaluated in one timezone for the whole server, set with `--timezone`: `local` (the
default: the zone of the machine or container), `utc`, or an IANA name such as
`America/New_York`. `local` and `utc` are accepted in any letter case; IANA names are spelled as
in the tz database. Any other value is a usage error: the server exits 2 and names the value.

`barca list` talks to no server and does not know its `--timezone`. Its next fire times (the
`NEXT FIRE (LOCAL TIME)` column, `next_fire` in JSON) are the next match of the cron expression
in the local time of the machine `list` runs on. For a server started with another zone, ask the
server: `GET /schedule` returns `next_fire` as unix epoch seconds, computed in the server's zone.

```bash
curl -s http://127.0.0.1:8274/schedule         # [{"id": ..., "cron": ..., "next_fire": 1791522000, "last_skipped": null, ...}]
```

```bash
barca list pipeline.py                         # shows each schedule and its next fire time
barca serve pipeline.py                        # HTTP API + scheduler + web UI on 127.0.0.1:8274
barca serve pipeline.py --timezone utc         # evaluate cron in UTC (default: local)
barca serve pipeline.py --no-schedule          # API only, no scheduler
barca serve pipeline.py --watch                # dev: re-parse the DAG when files change
barca serve pipeline.py --read-only            # inspect only: no runs, no scheduler
barca serve pipeline.py --host 0.0.0.0         # listen on every interface (containers, VMs)
```

`--read-only` serves the API without the ability to change anything: run, cancel and pause
endpoints return `403`, the scheduler never starts, and every read of the metadata DB goes through a
private copy, so it is safe to point at a project another process is running.

`serve` binds to `127.0.0.1` by default and has no authentication. `--host 0.0.0.0` (or `::`)
listens on every interface, which a container or VM needs for the port to be reachable from
outside; barca then prints a warning on stderr, because anyone who can reach the port can trigger
runs. Keep it on a private network or behind a proxy that authenticates. Open
`http://127.0.0.1:8274/` for the web UI. Endpoints are documented at
https://barca.sh/reference/server-api/; the normative wire contract is
https://github.com/barca-orc/barca/blob/main/specs/server-api.openapi.yaml. Handler errors are
JSON, while malformed request parameters can produce plain-text extractor errors.
`GET /schedule` reports live schedule status. Behind
nginx or Traefik (any path prefix, live logs included): https://barca.sh/deploying/.
Full model: https://barca.sh/scheduling/.

The UI's Runs view shows persisted CLI, scheduled and HTTP runs, plus queued/live server
runs, with what started each. Select a run to inspect its status, timing, steps, errors and
logs. After triggering a node in the graph, use View run to open its details; even a cache hit
records a new run. The same inspection is available through `GET /runs?limit=100` and
`GET /runs/{run_id}`, which read private DB snapshots. A run the server started keeps its full
outcome in the DB, so `GET /runs/{run_id}` gives the same answer after a restart.

## Calling the server

Deploy the project code and its Python dependencies on the server, keep `.barca/` on
persistent storage, and start `barca serve pipeline.py`. Workers execute on that machine.
From another terminal, trigger a deployed asset or task:

```bash
curl -X POST http://127.0.0.1:8274/runs -d '{"command": "get", "targets": ["daily_report"]}'  # an asset and its dependencies
curl -X POST http://127.0.0.1:8274/runs -d '{"command": "run", "targets": ["publish"]}'       # a task
curl -N http://127.0.0.1:8274/runs/RETURNED_RUN_ID/events   # its progress lines and outcome, as they happen
curl http://127.0.0.1:8274/runs/RETURNED_RUN_ID             # the run and its outcome; also after a restart
curl http://127.0.0.1:8274/runs/RETURNED_RUN_ID/output      # the final artifact's bytes
```

Replace the target names with functions in the deployed pipeline. `POST /runs` takes the flags
of `barca get` and `barca run` as JSON (`targets`, `refresh`, `refresh_all`, `cascade`,
`dry_run`, `agent`) and answers `202` with the run id at once, which is the id
`barca history` shows. The targets are checked first, in the CLI's words: a request a local
command would refuse starts nothing. The event stream sends each stderr line a local run
prints (`line`, tagged `when: agent` or `always`), each line a step prints (`log`), each step's
result (`step_finished`, with `cached: true` for a cache hit), and `run_finished` with the
outcome: the exact `barca get --json` body on success, the failed-run body and the error
envelope on failure. A dropped connection is not resumed; reconnecting replays the run from its
first event. Remote clients use the URL of the private server or its authenticating proxy in
place of the loopback address above. The older `POST /get/<target>`, `POST /run/<target>`,
`POST /run`, `GET /status/<id>` and `GET /events/<id>` routes stay for one more minor release.

The standard-library Python client wraps the same API:

```python
from barca import Client

client = Client("http://127.0.0.1:8274")
run = client.get("daily_report")
result = run.wait(timeout=600)
print(result["status"])
```

With a remote artifact store configured, serving requires local metadata:

```bash
BARCA_STATE=off barca serve pipeline.py  # local ledger; artifact uploads still enabled
```

Other machines do not get cache/history sharing from that artifact store. CLI `--server`,
`[server] url`, `BARCA_SERVER` and local runs posting results to the server's ledger belong
to the part of RFC-0007 not implemented yet. `barca.get()` starts a local CLI process; use
`Client.get()` for an HTTP request.

## Stopping the server

SIGINT (Ctrl-C) and SIGTERM (`kill`, `docker stop`, systemd) stop the server the same way. It
prints `[barca] SIGTERM received: stopping runs and shutting down`, stops accepting
connections, cancels the runs in flight (their workers are stopped and the runs are recorded as
`cancelled` in `barca history`), ends the open event streams and exits 0. That normally
takes less than a second. It is bounded: runs get 10 seconds to stop, and connections still open
after that get 2 more. This also holds when barca is process 1 of a container, so a plain
`docker stop` works.

SIGHUP and SIGQUIT are not handled. Outside a container they end the process at once, and a run
in flight is then reported as `interrupted` (`barca docs cache`); `nohup barca serve` keeps
ignoring SIGHUP. A process that is process 1 of a container never receives them. SIGKILL cannot
be handled by any program.

## Source load failures

A broken source does not disable unrelated schedules. Serve excludes its definitions
and graph-dependent nodes while retaining healthy definitions, even in a file with a
blocked sibling node. Startup logs and the UI report unloaded sources and affected
node IDs; `GET /health` exposes the same `load_errors`. With `--watch`, repaired
configured sources and schedules rejoin automatically. One-shot commands stay strict.
