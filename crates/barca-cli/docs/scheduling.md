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
def inbox() -> str:
    return Path("inbox.txt").read_text()


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
def prices() -> str:
    return Path("prices.txt").read_text()


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

From the CLI, `barca status --problems` lists a failing (or paused) schedule with the error of
its last run, and `schedule` in `barca status --json` has the same `failures`, `max_failures`,
`failing`, `paused` and `next_fire` as `GET /schedule`; `barca doctor` with a server fails its
`server_scheduler` check while one is failing (`barca docs status`, "Schedules").

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

What a run keeps is bounded by what it computed, not by how many keys it served from cache
(0.30.2, #623):

- Its outcome lists at most 1000 keys of a partitioned target; past that, the keys it
  computed and a count of the others (`barca docs partitions`, "Many keys").
- `GET /runs/{run_id}` lists at most 1000 of its recorded steps (`?limit=N` for another
  number, `?all=true` for every one; failed steps are kept first), with `steps_page: {total,
  truncated}`; `barca history RUN_ID` takes `--limit N` and `--all` the same way. A run records
  a step per key it computed, never per cached key.
- The server keeps a finished run's outcome and event stream in memory for an hour, and at most
  64 MB of them: past that the oldest finished runs are dropped first (their record stays in the
  DB, where `GET /runs/{run_id}` reads it; `GET /runs/{run_id}/events` is then `404`). The run
  that finished last is always kept. After each run the server hands the memory the run freed
  back to the system, so its resident size returns to its idle size between ticks.

## Calling the server

Deploy the project code and its Python dependencies on the server, keep `.barca/` on
persistent storage, and start `barca serve pipeline.py`. Workers execute on that machine.
From another terminal, trigger a deployed asset or task:

```bash
curl -X POST http://127.0.0.1:8274/runs -d '{"command": "get", "targets": ["daily_report"]}'  # an asset and its dependencies
curl -X POST http://127.0.0.1:8274/runs -d '{"command": "run", "targets": ["publish"]}'       # a task
curl -N http://127.0.0.1:8274/runs/RETURNED_RUN_ID/events   # its progress lines and outcome, as they happen
curl http://127.0.0.1:8274/runs/RETURNED_RUN_ID/logs        # what its steps wrote so far (`barca logs`)
curl http://127.0.0.1:8274/runs/RETURNED_RUN_ID             # the run and its outcome; also after a restart
curl http://127.0.0.1:8274/runs/RETURNED_RUN_ID/output      # the final artifact's bytes
```

Replace the target names with functions in the deployed pipeline. `POST /runs` takes the flags
of `barca get` and `barca run` as JSON (`targets`, `refresh`, `refresh_all`, `cascade`,
`dry_run`, `agent`) and answers `202` with the run id at once, which is the id
`barca history` shows, and the run's place in the run queue ("The run queue" below). The targets are checked first, in the CLI's words: a request a local
command would refuse starts nothing. The event stream sends each stderr line a local run
prints (`line`, tagged `when: agent` or `always`), each line a step writes (`log`, with its
`stream`, `stdout` or `stderr`, and `ts`, when it was written), each step's
result (`step_finished`, with `cached: true` for a cache hit), and `run_finished` with the
outcome: the exact `barca get --json` body on success, the failed-run body and the error
envelope on failure. Reconnecting replays the run from its first event (the CLI's clients skip
what they already printed, and ride through a restart: "Clients ride through a restart" below). Remote clients use the URL of the private server or its authenticating proxy in
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

## The run queue

Every run the server accepts joins its run queue: `POST /runs` and the older trigger routes, a
`--server` client, the web UI, and each scheduled tick. The run is recorded in the metadata DB
(`run_queue`) under its run id before the server answers, so a restart does not lose it. Runs
start in the order they were accepted, at most `--max-runs` at once (`[server] max_runs` in
`barca.toml`, default one per core); the others wait with status `pending`. `POST /runs` answers
`202 {"run_id", "status", "position"}`: `running` when it started at once, or `pending` and its
place in the queue (1 = next).

```bash
barca serve pipeline.py --max-runs 2           # two runs at a time; the rest wait, in order
barca queue --server                           # the configured server's queue: running and queued runs
barca queue --server http://127.0.0.1:8274 --json   # {paused, paused_at, draining, max_runs, running, queued}
barca queue pause --server                     # nothing new starts; runs in flight go on
barca queue resume --server                    # queued runs start again, in order
```

`barca queue` reads the server named by `--server`, `BARCA_SERVER` or `[server] url`; without one
it is a usage error (exit 2). The same is `GET /queue`, `POST /queue/pause` and
`POST /queue/resume`, and the web UI's Runs page shows the queue with a Pause / Resume button.
`GET /health` has `queue: {paused, queued, running, max_runs}`.

While the queue is paused, runs in flight finish, new runs are still accepted and wait, and a
scheduled tick joins the queue too. The rule for a tick whose node already has a run pending or
running holds: one tick waits, the ticks after it are skipped (`skip_reason: "in_flight"`), so a
long pause leaves one run per scheduled node, not one per tick. The pause is kept in the metadata
DB (`server_state`), like a schedule's pause: a server restarted while paused stays paused and
prints `[barca] the run queue is paused (since ...)`. A `--read-only` server refuses pause and
resume with `403` (`barca queue pause` exits 2); its `GET /queue` shows what the metadata DB
records. `DELETE /runs/{id}` on a queued run takes it off the queue and records it `cancelled`.
`barca state reset` (and `POST /state/reset`, refused while a run is queued) clears the pause
with the rest of the database.

When the server starts it resumes the runs it finds queued, in their order and under their run
ids, before its scheduler catches up, and prints `[barca] resuming 3 queued runs`. A client that
holds a run id sees the run go `pending` → `running` → `success` across the restart. A run the
previous server had started and never finished recording is not started again when the server
died on its own (SIGKILL without a stop signal first, such as the OOM killer, or a crash): it is
reported once on startup and recorded as interrupted (`error` when it never wrote a row), because
a run that kills its server would otherwise kill every server after it.

A run that was in flight when the server was told to stop is different. The first SIGTERM or
SIGINT marks every run in flight `draining` in the queue before anything else; if whatever sent
the signal kills the server before the drain ends (a supervisor whose grace period is too short),
the next server finds the run still `draining`, prints `[barca] run RUN_ID was in flight when the
last server was killed during its drain (SIGKILL after SIGTERM: the stop grace period ran out);
it is put back in the queue and starts again`, and starts it again first, under the same id, as
if the drain timeout had passed. This happens once per run: a run that is in flight through two
killed drains in a row is recorded as interrupted, so a run that keeps its server from stopping
cannot loop.

## Stopping the server

SIGINT (Ctrl-C) and SIGTERM (`kill`, `docker stop`, systemd) stop the server the same way: the
first one starts a drain, which loses nothing the server accepted.

```bash
barca serve pipeline.py                      # on SIGTERM, runs in flight get 30 seconds
barca serve pipeline.py --drain-timeout 300  # give them 5 minutes
barca serve pipeline.py --drain-timeout 0    # stop them at once and requeue them
```

```toml
# barca.toml (the flags win)
[server]
drain_timeout = 300
max_runs = 4
```

During the drain the server prints `[barca] SIGTERM received: draining — the queue is paused
and new runs are queued for the next start; waiting up to 30s for 1 run in flight` and:

- starts no queued run, and keeps accepting new ones: `POST /runs` (and the older routes)
  answer as usual, with `status: "pending"`, and the run waits in the queue for the next server
  (`draining: true` in `GET /queue`);
- stops the scheduler (`[barca] scheduler stopped: the server is draining`). A tick that falls
  in the drain starts nothing; the next server's startup catch-up fires the job once;
- reports `"status": "draining"` on `GET /health` (still `200`);
- keeps answering reads, `GET /runs/{id}`, its `/events` and `DELETE /runs/{id}` (and their
  older aliases); `POST /state/reset` and ledger posts answer `503` with `Retry-After`;
- waits for the runs in flight, their artifact uploads included, to finish and be recorded.

When they have all finished it prints `[barca] drained: every run finished; shutting down` (and
how many runs wait in the queue), ends the open `/events` streams and exits 0. With nothing in
flight that takes well under a second.

When the drain timeout passes before the runs in flight have finished, they are stopped and put
back at the head of the queue: `[barca] the drain timeout (30s) passed: stopping 1 run and
putting it back in the queue`. Their workers are stopped, the steps that finished are kept (and
are cache hits on the next attempt), and the stopped attempt is not recorded as a run that ended:
its row and lines are replaced by the next attempt's. The server exits 0, and the next server
starts them again from the beginning, first, under the same run ids.

A second SIGTERM or SIGINT during the drain cancels the runs in flight instead: each run is
recorded as `cancelled` in `barca history`, with `[barca] run cancelled: server shutdown` in its
logs (`barca logs RUN_ID`) and `barca.reason: server shutdown` on its `barca.run` telemetry span.
The server exits 1, after an error naming how many runs were cancelled, so a supervisor or
deploy script can tell a forced stop from a clean one. The queued runs are kept either way.
Stopped runs get 10 seconds to stop their workers and record themselves, and connections still
open after that get 2 more (shorter with a drain timeout under 10 seconds: below).

### At least once

A run the drain timeout requeued runs again from its start. Assets that finished are cache hits
then, but a step that was interrupted runs again, and a task runs again from its beginning
(including what it did before it was stopped, such as an email or an insert). So a run the server
accepted is executed at least once, not exactly once. A task with side effects should be safe to
repeat (an upsert, a key that makes a second send a no-op), or give the drain long enough for it
to finish (`--drain-timeout`), or cancel it with a second signal when a repeat would be worse
than no run.

SIGHUP and SIGQUIT are not handled. Outside a container they end the process at once, and a run
in flight is then reported as `interrupted` (`barca docs cache`); `nohup barca serve` keeps
ignoring SIGHUP. A process that is process 1 of a container never receives them. SIGKILL cannot
be handled by any program: the queued runs survive it, the runs in flight are interrupted.

## Restarting without losing work

A deploy, a reboot or a config change restarts the server. Nothing it accepted is lost:

```bash
barca queue pause --server       # optional: let nothing new start before the restart
systemctl restart barca          # or: docker restart barca
barca queue --server             # after: the queue resumed (still paused if you paused it)
barca queue resume --server      # if you paused it
```

1. Pausing first is optional. It lets the runs in flight finish while nothing new starts, so the
   stop that follows waits only for them.
2. The supervisor sends SIGTERM: the server drains (above), the runs in flight finish or, at the
   drain timeout, go back to the queue, and it exits 0. Clients following a run reconnect.
3. The new server prints `[barca] resuming N queued runs` and starts them in order.

Give the supervisor a stop grace period of at least the drain timeout plus what the server takes
after it. Otherwise it kills the server mid-drain; the runs in flight are then put back in the
queue by the next server, once (above), but they lose the time the drain gave them. After the
drain the server takes:

| Part | Drain timeout 10 s or more | Drain timeout under 10 s |
|---|---|---|
| Stopped runs terminate their workers and record themselves | 10 s | half the drain timeout, at least 2 s |
| Open connections finish | 2 s | 1 s |
| The last metadata backup (only with a store) | 31 s | the drain timeout, at least 5 s, plus 1 s |
| The telemetry flush (only with `BARCA_TELEMETRY`) | 3 s | 3 s |

So 42 seconds with the default drain timeout of 30 and no store, 73 with a store; 9 seconds with
`drain_timeout = 5`, which fits Docker's default of 10; 3 with `drain_timeout = 0`. Under a
supervisor, `barca serve` prints the number for its configuration when it starts: as process 1
of a container, under systemd (`INVOCATION_ID` is set) and under Kubernetes
(`KUBERNETES_SERVICE_HOST` is set):

```text
[barca] stop grace: the container runtime must allow at least 9s between SIGTERM and SIGKILL (drain timeout 5s + 4s for stopping runs and closing connections); set docker stop -t or stop_grace_period (Docker's default is 10s)
```

With the default 30-second drain timeout, 45 seconds or more:

| Supervisor | Setting | Default | With `drain_timeout = 30` |
|---|---|---|---|
| systemd | `TimeoutStopSec=` | 90 s | 90 s is enough |
| Docker | `docker stop -t`, `stop_grace_period` in Compose | 10 s | `stop_grace_period: 45s` |
| Kubernetes | `terminationGracePeriodSeconds` | 30 s | `45` |

This also holds when barca is process 1 of a container, so a plain `docker stop` drains. Keep
`.barca/` on persistent storage: the queue is in its metadata DB.

### Clients ride through a restart

A `--server` client following a run (`barca get ... --server`, `barca run ... --server`) and
`barca logs -f --server` keep following it when the server goes away: they print `[barca] lost
the event stream of run RUN_ID (...); waiting up to 60s for the server to come back`, ask the
server again every half second, and once it answers follow the same run id there (`[barca] the
server restarted; following run RUN_ID there`). A run the server stopped and requeued is shown
again from its new start; a run that ended meanwhile is read from `GET /runs/{id}`. When the
server does not come back within the window, the command ends as before: exit 3, naming the run
and how to read it later. `BARCA_SERVER_RECONNECT` sets the window in seconds (0: do not wait).

```bash
BARCA_SERVER_RECONNECT=300 barca run publish --server   # wait up to 5 minutes for a restart
BARCA_SERVER_RECONNECT=0 barca logs -f --server         # give up at once, as before 0.29.4
```

## Containers with a memory limit

Each step runs in its own worker process, and `serve` runs independent steps at the same time,
up to the pool size. Under a hard memory limit (`mem_limit` in Compose, a pod's memory limit),
heavy steps that become ready together (a cold cache after a deploy, say) can add up past it:
the kernel kills one, and the step fails with
`worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)`. The same step
succeeds alone.

Give the heavy steps a `weight=` (experimental, `barca docs assets`, "Heavy steps"): the steps
running at once add up to at most the capacity, the pool size unless `[execution] capacity` in
barca.toml or `BARCA_CAPACITY` sets it. With

```toml
# barca.toml
[execution]
capacity = 4
```

and `@asset(weight=2)` on the heavy assets, at most two of them run at once, and light steps
still use what is left. `barca doctor` reports the capacity in effect and the pool size. The
pool is the cores, or the capacity when it is smaller (`[execution] pool_size` or
`BARCA_POOL_SIZE` set it outright), so `capacity = 1` starts one worker, not one per core. That
capacity is per run: two schedules that fire together run with a capacity each. To limit the
server as a whole, give it a capacity of its own (below).

## A capacity shared by every run

Experimental. `barca serve --capacity N` (or `[server] capacity = N` in barca.toml; the flag wins)
admits the `weight=` of the steps of every run on the server against one budget. A step starts
only when it fits its own run's capacity and the server's. Use it instead of `--max-runs 1` to
keep a light, frequent job running while a long heavy rebuild holds the rest:

```toml
# barca.toml
[server]
capacity = 4      # the steps of every run together weigh at most 4
```

```python
from barca import Schedule, asset


@asset(freshness=Schedule("* * * * *"))   # the one-minute sync: weight 1
def sync() -> dict:
    return {"rows": 1}


@asset(weight=3)                          # the rebuild's heavy steps: 3 of the 4
def rebuild_customer() -> dict:
    return {"rows": 2}
```

```bash
barca serve pipeline.py --capacity 4           # the same, from the command line
barca queue --server --json                    # capacity: {total, in_use, runs, waiting}
```

While a rebuild step of weight 3 runs, the sync's weight-1 steps run beside it, one at a time;
a second weight-3 step waits. How a step is admitted:

- **First come, first served across runs, with the head reserved.** A run asks for the step at
  the head of its own ready queue once that step fits the run's own capacity. If nothing waits
  and it fits what is left, it starts. Otherwise the run waits in line, one place per run. Only
  the run at the front of the line can be admitted, when its step fits; nothing behind it goes
  first, even a lighter step that would fit. So a heavy step is never starved by a stream of
  light ones: the weight that comes free is kept for it until it fits, and the light steps that
  asked after it wait that long (in the example, at most one heavy step's length when a heavy
  step waits). Steps within one run keep the order of their run's queue, as before.
- **No deadlock.** Only running steps hold weight, and they finish, fail or are cancelled. A
  step heavier than the server's capacity counts as all of it: it runs alone on the server, with
  `[barca] warning: 'x' has weight=8, more than the capacity 4 (--capacity); it runs alone` when
  its run is planned. A step that calls `parallel()` gives its weight to its branches while it
  waits for them and takes it back when it resumes, even if that briefly goes over.
- **Per-run capacity.** `[execution] capacity` still limits each run. The pool of each run is
  the cores, bounded by the smaller of the two capacities. Runs keep their own worker pools; a
  run waiting for the server's capacity holds no worker for the step it waits with.
- **`--max-runs`** still limits how many runs execute at once; a run waiting for capacity holds
  its run slot. With a server capacity, `--max-runs` can be larger than you would otherwise set
  it: the capacity, not the number of runs, bounds the load.
- **The run queue** is unchanged: runs leave it as before, and the capacity is held in memory, so
  a restart starts with nothing in use.

`GET /queue` (and `barca queue --server --json`) has `capacity: {total, source, in_use, runs,
waiting}`: the weight in use, the weight each run holds, and the steps waiting in admission
order with their run, weight and `waiting_seconds`; `null` without a server capacity. `GET
/health` has `queue.capacity: {total, in_use, waiting}`. The web UI's Runs page shows `weight 3
of 4 in use · 1 step waiting` and the waiting steps.

## Run timeout

A run under `barca serve` has no time limit by default. `--run-timeout SECONDS` (or `[server]
run_timeout_seconds` in barca.toml; the flag wins) stops a run that takes longer, and
`Schedule(cron, timeout_seconds=N)` sets it for one schedule's ticks, over both. 0 means no
limit at any of them. The time counts from when the run leaves the queue.

```bash
barca serve pipeline.py --run-timeout 3600     # stop any run after an hour
barca serve pipeline.py --run-timeout 0        # no limit (the default)
```

```python
@asset(freshness=Schedule("*/5 * * * *", timeout_seconds=240))   # a tick's run may take 4 minutes
def sync() -> dict:
    return {"rows": 1}
```

A run that times out is stopped like a cancelled one (its workers are terminated; steps that
finished stay cached) and is recorded as `error` in `barca history`, with the reason in its
outcome: `run timed out after 240s (Schedule(timeout_seconds=) of sync)`, naming the setting
that applied. Before 0.31.0 every run under `barca serve` stopped at 600 seconds, a limit that was
not configurable; `--run-timeout 600` restores it.

## Source load failures

A broken source does not disable unrelated schedules. Serve excludes its definitions
and graph-dependent nodes while retaining healthy definitions, even in a file with a
blocked sibling node. Startup logs and the UI report unloaded sources and affected
node IDs; `GET /health` exposes the same `load_errors`. With `--watch`, repaired
configured sources and schedules rejoin automatically. One-shot commands stay strict.

See also: `barca docs server-mode`, `barca docs telemetry`, `barca docs assets`.
