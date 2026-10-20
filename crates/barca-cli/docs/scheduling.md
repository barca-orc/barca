# Scheduling and `barca serve`

`barca serve` currently runs deployed code on one machine and exposes the HTTP API,
web UI and cron scheduler. Use the HTTP API or `barca.Client` to trigger remote work.
Ordinary `barca get` and `barca.get()` run locally. The accepted server-ledger design
(RFC-0007), including CLI `--server`/`--local`, server URL configuration and laptop
runs against the server's ledger, is not implemented in v0.24.0 (#393, #394, #402).
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

`POST /run/<task>` is different from a tick: it recomputes every upstream asset.

A tick is skipped, not queued, while a run of the same node is pending or running, whoever
started it: an earlier tick, `POST /get/<node>`, `POST /run/<task>`, or `POST /run` (every asset
and sensor; it does not hold back a scheduled task). The skip is a line on stderr naming the run
in flight, `last_skipped` and `skipped_for` in `GET /schedule` (kept in memory: a restart clears
them), and, with telemetry on, a `barca.run` span with status `skipped` (`barca docs telemetry`).
The next tick after the run ends fires as usual. Only a run of the node itself counts: a run of
a node downstream of it does not hold the tick back, though it computes the same upstream cone.
Two scheduled nodes that share an upstream run separately, and both may compute it.

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

With a remote store configured, `barca serve` uploads artifacts and backs its metadata DB up
every minute it changed (`barca docs remote`, "Backups and restore"); `barca state restore`
refuses to run while it is up:

```bash
barca serve pipeline.py                  # local ledger; artifact uploads and backups enabled
```

Other machines do not get cache or history sharing from that store. CLI `--server`,
`[server] url`, `BARCA_SERVER` and local runs posting results to the server's ledger belong
to the unimplemented RFC-0007 design. `barca.get()` starts a local CLI process; use
`Client.get()` for the HTTP request above.

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

- refuses new runs: `POST /run`, `POST /run/{target}` and `POST /get/{target}` answer `503`
  with a `Retry-After` header of the drain timeout in seconds (at least 1);
- stops the scheduler (`[barca] scheduler stopped: the server is draining`). A tick that falls
  in the drain starts nothing; the next server's startup catch-up fires the job once;
- reports `"status": "draining"` on `GET /health` (still `200`);
- keeps answering reads, `/status`, `/events` and `DELETE /run/{id}`;
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
