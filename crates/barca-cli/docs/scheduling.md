# Scheduling and `barca serve`

Freshness says when a node should be kept up to date:

| Freshness | Meaning |
|---|---|
| `Always` (default) | Recomputed whenever stale and its upstreams are fresh. |
| `Manual` | Only recomputed on an explicit refresh. A `Manual` upstream blocks `Always` downstream nodes from auto-updating. |
| `Schedule("<cron>")` | Fires on a cron schedule while `barca serve` is running. |

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
- A scheduled **sensor** is polled on every tick.

So outside data has to come in through a sensor (`barca docs cache`, "External data that changes
in place"). A scheduled asset that fetches data in its own body, with no sensor upstream, has
nothing on its input side that can change: it is computed once and then served from cache on
every tick, until its code or a declared `env=` variable changes or it is named in `--refresh`.
The same goes for a plain asset that a scheduled task reads.

The cache is keyed by the sensor's value, not by time. A sensor that returns to a value it had
before (a row count, a status flag) brings back the result computed for that value. Return
something that identifies the version of the data, such as an etag or a last-modified time.

`POST /run/<task>` is different from a tick: it recomputes every upstream asset.

A tick is skipped while the previous run of the same scheduled node is still going. Two
scheduled nodes that share an upstream run separately, and both may compute it.

Cron is evaluated in one timezone for the whole server, set with `--timezone`: `local` (the
default: the zone of the machine or container), `utc`, or an IANA name such as
`America/New_York`. `local` and `utc` are accepted in any letter case; IANA names are spelled as
in the tz database. Any other value is a usage error: the server exits 2 and names the value.

`barca list` talks to no server and does not know its `--timezone`. Its next fire times (the
`NEXT FIRE (LOCAL TIME)` column, `next_fire` in JSON) are the next match of the cron expression
in the local time of the machine `list` runs on. For a server started with another zone, ask the
server: `GET /schedule` returns `next_fire` as unix epoch seconds, computed in the server's zone.

```bash
curl -s http://127.0.0.1:8274/schedule         # [{"id": ..., "cron": ..., "next_fire": 1791522000, ...}]
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
https://barca.sh/reference/server-api/ and `GET /schedule` reports live schedule status. Behind
nginx or Traefik (any path prefix, live logs included): https://barca.sh/deploying/.
Full model: https://barca.sh/scheduling/.

The UI's Runs view shows persisted CLI, scheduled and HTTP runs, plus queued/live server
runs. Select a run to inspect its status, timing, steps, errors and logs. After triggering a
node in the graph, use View run to open its details; even a cache hit records a new run.
The detail URL switches to the durable run ID so it can be reopened after a server restart.
The same inspection is available through `GET /runs?limit=100` and `GET /runs/{id}` and
uses private DB snapshots. Logs and materialized steps survive restart; cached counts
remain, but cached per-step reports and final output require the server's retained result.

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
