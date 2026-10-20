# Scheduling and `barca serve`

`barca serve` currently runs deployed code on one machine and exposes the HTTP API,
web UI and cron scheduler. Use the HTTP API or `barca.Client` to trigger remote work.
Ordinary `barca get` and `barca.get()` run locally. The accepted server-ledger design
(RFC-0007), including CLI `--server`/`--local`, server URL configuration and laptop
runs against the server's ledger, is not implemented in v0.24.0 (#393, #394, #402).
Deployment instructions: https://barca.sh/deploying/.

Freshness says when each node is updated. Under `barca serve` (RFC-0008):

| Freshness | Meaning |
|---|---|
| `Schedule("<cron>")` | Fires on a cron schedule while `barca serve` is running. |
| `Always` (default for assets/tasks) | Brought up to date at each tick of a scheduled node above it, in that tick's run. |
| `Manual` (default for sensors) | Refreshed by hand only: never by a tick, and never by a request for something below it, which reads its current result. |

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

### `Manual` pins itself

A `Manual` asset or task is never run by a tick, and a `barca get` or `barca run` of something
below it does not recompute it: whatever reads it reads its latest result, even when its inputs
or code have changed since. Nodes below it are hashed against that result, so they are not
recomputed because of it until it changes. Only naming it in `--refresh` gets it recomputed:
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

`POST /get/<node>`, `POST /run/<task>` and `POST /run` compute what they are asked for as before,
`Manual` upstreams included. `POST /run/<task>` recomputes every upstream asset.

### Failures and `max_failures`

A node that fails in a tick's run stops only what depends on it; the other targets still run.
It is still out of date, so the next tick tries it again. `Schedule(cron, max_failures=N)`
(default 5) caps consecutive failed ticks of a scheduled node: past it the node's ticks are
skipped and `GET /schedule` reports `failing: true` with `failures` and `max_failures`. The count
is kept in the metadata DB across restarts. It resets on a successful tick, and when something
the tick failed on succeeds in another run: an explicit request for the scheduled node or for a
node that failed (`barca get`, `barca run`, `POST /get`, `POST /run`), or a new result of one of
that node's inputs.

```text
@sensor(freshness=Schedule("*/5 * * * *", max_failures=3))
```

```text
[barca] pipeline.py:inbox failing: 3 consecutive failed ticks (max_failures=3); skipped until an explicit request or a change in its inputs
```

A tick is skipped, not queued, while a run of the same node is pending or running, whoever
started it: an earlier tick, `POST /get/<node>`, `POST /run/<task>`, or `POST /run` (every asset
and sensor; it does not hold back a scheduled task). The skip is a line on stderr naming the run
in flight, `last_skipped` and `skipped_for` in `GET /schedule` (kept in memory: a restart clears
them), and, with telemetry on, a `barca.run` span with status `skipped` (`barca docs telemetry`).
The next tick after the run ends fires as usual. Only a run of the node itself counts: a run of
a node downstream of it does not hold the tick back, though it computes the same upstream cone.
Two scheduled nodes that share an upstream run separately, and if their ticks coincide both may
compute it: schedule the sensor once and let the `Always` nodes below it follow instead.

```text
[barca] scheduled run pipeline.py:orders skipped — run 1b942bb33182 of the same node still in flight
```

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

`--read-only` serves the API without the ability to change anything: run and cancel endpoints
return `403`, the scheduler never starts, and every read of the metadata DB goes through a
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
runs. Select a run to inspect its status, timing, steps, errors and logs. After triggering a
node in the graph, use View run to open its details; even a cache hit records a new run.
The detail URL switches to the durable run ID so it can be reopened after a server restart.
The same inspection is available through `GET /runs?limit=100` and `GET /runs/{id}` and
uses private DB snapshots. Logs and materialized steps survive restart; cached counts
remain, but cached per-step reports and final output require the server's retained result.

## Calling the server

Deploy the project code and its Python dependencies on the server, keep `.barca/` on
persistent storage, and start `barca serve pipeline.py`. Workers execute on that machine.
From another terminal, trigger a deployed asset or task:

```bash
curl -X POST http://127.0.0.1:8274/get/daily_report  # an asset and its dependencies
curl -X POST http://127.0.0.1:8274/run/publish      # a task; refreshes its upstream assets
curl http://127.0.0.1:8274/status/RETURNED_RUN_ID   # use run_id from the POST response
```

Replace the target names with functions in the deployed pipeline. The POST returns a
polling handle immediately; poll until `status` is `complete`, `failed` or `cancelled`.
The completed result contains an artifact pointer whose path is relative to the server's
project root. HTTP does not return that file's bytes. Remote clients use the URL of the
private server or its authenticating proxy in place of the loopback address above.

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
to the unimplemented RFC-0007 design. `barca.get()` starts a local CLI process; use
`Client.get()` for the HTTP request above.

## Stopping the server

SIGINT (Ctrl-C) and SIGTERM (`kill`, `docker stop`, systemd) stop the server the same way. It
prints `[barca] SIGTERM received: stopping runs and shutting down`, stops accepting
connections, cancels the runs in flight (their workers are stopped and the runs are recorded as
`cancelled` in `barca history`), ends the open `/events` streams and exits 0. That normally
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
