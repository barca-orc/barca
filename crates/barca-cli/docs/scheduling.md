# Scheduling and `barca serve`

`barca serve` currently runs deployed code on one machine and exposes the HTTP API,
web UI and cron scheduler. From another machine, `barca get daily_report --server <url>` (and
`run`, `status`, `list`, `history`, `stats`) runs a command there and prints it here as a local
run would (`barca docs server-mode`); the HTTP API (`POST /runs`) is underneath. Without
`--server`, `barca get` and `barca.get()` run locally, and with a server configured they read
and post to its ledger (`--local` leaves it untouched).
Deployment instructions: https://barca.sh/deploying/.

Freshness says when each node is updated. Under `barca serve` (RFC-0008):

| Freshness | Meaning |
|---|---|
| `Schedule("<cron>")` | Fires on a cron schedule while `barca serve` is running. |
| `Always` (default for assets/tasks) | Brought up to date at each tick of a scheduled node above it, in that tick's run. |
| `Manual` (default for sensors) | Refreshed by hand only: never by a tick, and never by a request for something below it, which reads its current result. A sensor is the exception: every run that needs its value polls it, whatever its freshness. |

```python
from pathlib import Path

from barca import asset, sensor, task, Manual, Schedule


@sensor(freshness=Schedule("*/5 * * * *"))      # the only schedule
def inbox() -> tuple[bool, str]:
    return True, Path("inbox.txt").read_text()


@asset(inputs={"files": inbox})                  # Always (the default)
def orders(files: str) -> dict:
    return {"files": files}


@asset(inputs={"orders": orders})                # Always
def hourly_summary(orders: dict) -> dict:
    return {"summary": orders["files"]}


@task(inputs={"orders": orders})                 # Always
def send_report(orders: dict) -> None:
    print("report", orders["files"])


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
- `Schedule` takes the cron expression by position and one keyword, `max_failures` (below).
  `Schedule(cron="0 5 * * *")` and any other keyword are errors when the file is read, exit 2
  (`barca docs assets`, "Accepted arguments").

### What a tick does

A tick of a scheduled node is one run with several targets: the scheduled node, then every
`Always` node below it. The walk down from the scheduled node goes past a `Manual` node (which
is not updated) and stops at a node with a `Schedule` of its own (which follows its own ticks).
Nodes with no scheduled node above them are never touched by a tick. A tick brings each target
up to date; it does not force it:

- The scheduled node: a **sensor** is polled; an **asset** and its cone are checked, each asset
  running only if there is no cached result for its current code and inputs (`barca docs cache`);
  a **task** runs.
- An `Always` **asset** below it is up to date when it has a cached result for its current run
  hash; otherwise it is computed. A sensor that returns to an earlier value brings back the
  result computed then.
- An `Always` **task** below it fires when its last successful run used other versions of its
  inputs than the current ones (other code, another result of an asset it reads, another value
  of a sensor it reads), when a task it reads has run successfully since, or when it has never
  run. Otherwise it is up to date and does not run.

In the pipeline above, every five minutes `inbox` is polled. When it returns the same value
nothing else runs. When it returns a new one, the same run computes `orders` once, then
`hourly_summary` and `send_report`. Upstream inputs that no scheduled node is above (a plain
asset a target reads) are computed as part of the run when they have no cached result.

The cache is keyed by the sensor's value, not by time. A sensor that returns to a value it had
before (a row count, a status flag) brings back the result computed for that value. Return
something that identifies the version of the data, such as an etag or a last-modified time. A
scheduled asset that fetches data in its own body, with no sensor upstream, has nothing on its
input side that can change: it is computed once and then served from cache on every tick, until
its code or a declared `env=` variable changes or it is named in `--refresh`.

With `--watch`, an edit to an `Always` node changes its run hash; the next tick of a scheduled
node above it recomputes it. Nothing happens between ticks.

A tick's run is recorded with `triggered_by: "schedule"`, the tick it was started for
(`scheduled_at`) and the schedule (`schedule_id`, the scheduled node's id), in
`barca history --json`, `GET /runs/{run_id}` and the `barca.run` span (`barca docs telemetry`),
whose resource names every target of the tick, sorted (`get hourly_summary,inbox,orders,send_report`).
Its `job` is the scheduled node.

### `Manual` pins itself

A `Manual` asset or task is never run by a tick, and a `barca get` or `barca run` of something
below it does not recompute it: whatever reads it reads its latest result, even when its inputs
or code have changed since. Nodes below it are hashed against that result (its run hash and its
bytes), so they are not recomputed because of it until it changes, and are recomputed once it
has a new result: a refresh by hand that gives new bytes makes them stale even when its code
and inputs did not change. Only naming it in `--refresh` gets it recomputed:
`barca get baseline pipeline.py --refresh baseline` (or `--refresh-all`). That holds when it is
the target too: `barca get baseline pipeline.py` returns its current result, with the stale
note, and exits 2 when it was never computed. A `Manual` task named as the target of
`barca run` runs. The next tick then finds what reads it out of date and updates it.

```python
from pathlib import Path

from barca import asset, sensor, task, Manual, Schedule


@sensor(freshness=Schedule("*/5 * * * *"))
def prices() -> tuple[bool, str]:
    return True, Path("prices.txt").read_text()


@asset(inputs={"etag": prices}, freshness=Manual)
def baseline(etag: str) -> dict:                 # only an explicit request updates it
    return {"baseline": etag}


@asset(inputs={"baseline": baseline, "etag": prices})
def report(baseline: dict, etag: str) -> dict:   # recomputed when prices changes
    return {"baseline": baseline["baseline"], "etag": etag}


@task(inputs={"baseline": baseline})
def publish(baseline: dict) -> None:             # fires only when baseline is refreshed
    print("publish", baseline["baseline"])
```

When `prices` changes, `report` is recomputed reading the pinned `baseline`, and `publish` does
not fire. `barca get report` prints a note when it reads a `Manual` node that is stale:

```text
[barca] note: baseline is Manual and stale; barca get baseline pipeline.py --refresh baseline to recompute
```

A `Manual` node that has never been computed cannot be read. `barca get report` is then a usage
error (exit 2) whose remediation is that command; under a tick, the nodes that read it are
reported `blocked` in the run (a `skipped` step with reason `blocked`) and the tick is not an
error. Sensors are polled whenever a run needs them, whatever their freshness. A partitioned
`Manual` node is not pinned yet: it is computed like an `Always` node.

`POST /runs` (and `--server`) runs what `barca get` or `barca run` would run, so a `Manual` node
is read as it is there too. The older aliases `POST /get/<node>`, `POST /run/<task>` and
`POST /run` compute what they are asked for as before, `Manual` upstreams included;
`POST /run/<task>` recomputes every upstream asset.

### Failures and `max_failures`

A node that fails in a tick's run stops only what depends on it; the other targets still run.
It is still out of date, so the next tick tries it again. `Schedule(cron, max_failures=N)`
(default 5) caps consecutive failed ticks of a scheduled node: past it the node's ticks are
skipped and `GET /schedule` reports `failing: true` with `failures` and `max_failures`. The count
is kept in the metadata DB across restarts. It resets on a successful tick, and when something
the tick failed on succeeds in another run: an explicit request for the scheduled node or for a
node that failed (`barca get`, `barca run`, `POST /runs`), or a new result of one of that node's
inputs. Ticks skipped while the job is paused, or while a run of its node is in flight, are not
failures and are not counted.

```text
@sensor(freshness=Schedule("*/5 * * * *", max_failures=3))
```

```text
[barca] pipeline.py:inbox failing: 3 consecutive failed ticks (max_failures=3); skipped until an explicit request or a change in its inputs
```

A tick is skipped, not queued, while a run of the same node is pending or running, whoever
started it: an earlier tick, `POST /runs` naming the node, or a `POST /runs` of every asset and
sensor (it does not hold back a scheduled task). A `barca get` or `barca run` started from a
shell on the same project (the same `.barca/`) counts the same way: `barca get <node>`,
`barca run <task>`, or a `barca get` of every asset and sensor. The server finds it in the
metadata database as a `running` run whose process is alive; a `running` row left by a run that
was killed does not hold a tick back, nor does one whose process cannot be checked from here
(another machine's run in a restored backup). The skip is a line on stderr naming the run
in flight, `last_skipped`, `skip_reason: "in_flight"` and `skipped_for` in `GET /schedule`
(kept in memory: a restart clears them), and, with telemetry on, a `barca.run` span with status
`skipped` (`barca docs telemetry`).
The next tick after the run ends fires as usual. Only a run of the node itself counts: a run of
a node downstream of it does not hold the tick back, though it computes the same upstream cone.
Two scheduled nodes that share an upstream run separately, and if their ticks coincide both may
compute it: schedule the sensor once and let the `Always` nodes below it follow instead.

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
curl -s http://127.0.0.1:8274/schedule         # [{"id": ..., "next_fire": 1791522000, "failures": 0, "failing": false, ...}]
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

`--read-only` serves the API without the ability to change anything: run, cancel, pause and reset
endpoints return `403`, the scheduler never starts, and every read of the metadata DB goes through a
private copy, so it is safe to point at a project another process is running.

### The metadata DB at startup

`serve` names the database it uses on its first line, after the environment and the artifact
store, with the format it is in:

```
[barca] env prod (--env); artifact store local (.barca/envs/prod/artifacts); metadata .barca/envs/prod/metadata.db (db format 1)
```

A writer server (not `--read-only`) sets that database up before it binds the port, migrating an
older one in place (with no database yet the line says `(not created yet)`, and the first run or
the scheduler creates it). When it cannot, it exits 3 instead of serving with its scheduler off, naming
the file and what to do:

```
the metadata DB .barca/envs/prod/metadata.db cannot be used: it does not start with a SQLite header

Not a problem in your code. If a newer barca wrote this database, upgrade barca. Otherwise put a backup back with `barca state restore` (it moves .barca/envs/prod/metadata.db aside first), or start an empty history with `barca state reset` (the Reset database button in the web UI of a running `barca serve`), which moves .barca/envs/prod/metadata.db aside and prints how to undo it.
```

A database barca cannot use is one that is not a SQLite file, was written in a newer format, or
has a malformed schema. An empty file (0 bytes) holds no history, so it is not one: a `touch`, a
backup tool or a script that opened the path before barca created the database leaves one, and a
writer initializes it as a new database, with one line on stderr naming it (`[barca] note: the
metadata DB .barca/metadata.db was empty (0 bytes); initialized it as a new database`). `get` and
`run` do the same. A lock another process holds is waited for (up to 60
seconds) before the port is bound; a failure to open or write the file is exit 3 too. `get` and
`run` give the same error, exit 3, with `reason` `metadata_unusable` in the JSON envelope.

A `--read-only` server never writes the file. It reads an empty one as no history yet (its
startup line says `(not created yet)`, after a note) and leaves it for a writer to initialize.
For a file it cannot use it keeps serving: its startup line says
`(unusable)`, a warning follows, `GET /health`
reports the error in `metadata_error`, and every route that reads history answers `500` with the
error envelope, `reason` `metadata_unusable` and the remediation. `metadata_error` goes back to
`null` once a read succeeds again (after the writer migrated the file, or it was restored).

`GET /health` also has `scheduler_enabled`: `false` with `--no-schedule` or `--read-only`, and
once the scheduler stopped on an error (it then says why in `metadata_error`).

With `--env <name>` the database is `.barca/envs/<name>/metadata.db`. If `.barca/metadata.db` (the
default environment's) is also there but empty or not a database, often left by a container that
started once without `--env`, `serve` warns once, naming both files. A valid one is the default
environment's history and is left alone.

```bash
barca serve pipeline.py --env prod             # the startup line names .barca/envs/prod/metadata.db
curl -s http://127.0.0.1:8274/health           # {..., "scheduler_enabled": true, "metadata_error": null}
```

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

The UI shows what is running now on every page, whoever started it (the scheduler, another
client, a `barca get` in a shell on the same `.barca/`): a running node pulses on the graph and
the pipeline page, the assets list shows a Running badge with the elapsed time (and keys
done/total for a partitioned node) linking to the run, each pipeline and group counts its
running nodes, and the header shows how many runs are in progress. It reads `GET /state`,
whose `running` field names the run working on each node, every 2 s while anything runs.
`GET /runs?status=running` lists the runs in flight with the nodes each is working on.

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

From Python, `barca.get("daily_report", server="http://127.0.0.1:8274")` (or `server=True`
with `BARCA_SERVER` or `[server] url` set) runs it on the server and returns its value.
`barca.Client`, the older standard-library HTTP client, is deprecated: it warns when
constructed and will be removed in a later minor release.

With a remote store configured, `barca serve` uploads artifacts and backs its metadata DB up
every minute it changed (`barca docs remote`, "Backups and restore"); `barca state restore`
and `barca state reset` refuse to run while it is up. To start its metadata DB over (one it
cannot read, say), use the web UI's **Reset database** button or `POST /state/reset`: it moves the
database aside to `metadata.db.reset-<UTC>` and starts empty, refused with `409` while a run is
pending or running:

```bash
barca serve pipeline.py                  # local ledger; artifact uploads and backups enabled
```

Other machines share the server's results by pointing `[server] url` (or `BARCA_SERVER`) at
it: their local runs read cache hits from the server's ledger and post what they compute, and
`--server` runs a command on the server instead (`barca docs server-mode`).

## Stopping the server

SIGINT (Ctrl-C) and SIGTERM (`kill`, `docker stop`, systemd) stop the server the same way: the
first one starts a drain.

```bash
barca serve pipeline.py                      # on SIGTERM, runs in flight get 30 seconds
barca serve pipeline.py --drain-timeout 300  # give them 5 minutes
barca serve pipeline.py --drain-timeout 0    # cancel them at once
```

```toml
# barca.toml (the flag wins)
[server]
drain_timeout = 300
```

During the drain the server prints `[barca] SIGTERM received: draining — new runs are refused;
waiting up to 30s for 1 run in flight (send SIGTERM again to stop now)` and:

- refuses new runs: `POST /runs` (a `dry_run` is still answered) and the older `POST /run`,
  `POST /run/{target}` and `POST /get/{target}` answer `503`
  with a `Retry-After` header of the drain timeout in seconds (at least 1);
- stops the scheduler (`[barca] scheduler stopped: the server is draining`). A tick that falls
  in the drain starts nothing; the next server's startup catch-up fires the job once;
- reports `"status": "draining"` on `GET /health` (still `200`);
- keeps answering reads, `GET /runs/{id}`, its `/events` and `DELETE /runs/{id}` (and their
  older aliases);
- waits for the runs in flight, their artifact uploads included, to finish and be recorded.

When they have all finished it prints `[barca] drained: every run finished; shutting down`,
ends the open `/events` streams and exits 0. With nothing in flight that takes well under a
second.

When the drain timeout passes, or a second SIGTERM or SIGINT arrives, before the runs have
finished, the runs still going are cancelled: their workers are stopped, the steps that finished are kept, and each run
is recorded as `cancelled` in `barca history`, with `[barca] run cancelled: server shutdown`
in its logs (`barca logs RUN_ID`) and `barca.reason: server shutdown` on its `barca.run`
telemetry span. The server exits 1, after an error naming how many runs were cancelled, so a
supervisor or deploy script can tell a forced stop from a clean drain. The cancelled runs get 10
seconds to stop, and connections still open after that get 2 more.

A supervisor kills the server when its own grace period runs out, so make that longer than the
drain timeout: Docker's default is 10 seconds (`docker stop -t`, `stop_grace_period` in
Compose), Kubernetes' 30 (`terminationGracePeriodSeconds`), systemd's 90 (`TimeoutStopSec`).
This also holds when barca is process 1 of a container, so a plain `docker stop` drains.

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

See also: `barca docs server-mode`, `barca docs telemetry`, `barca docs assets`.
