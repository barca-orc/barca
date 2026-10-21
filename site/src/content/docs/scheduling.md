---
title: Scheduling
description: Run assets, tasks and sensors on a cron schedule with barca serve, what a tick does, how Always and Manual nodes below a schedule behave, and what happens when ticks fail.
---

A node declared with `freshness=Schedule("<cron>")` runs on that cron schedule while
`barca serve` is running. The scheduler is part of the server and is on unless you pass
`--no-schedule`. `barca get` and `barca run` never fire schedules.

## Run a function every 10 minutes

```python
# job.py
from datetime import datetime

from barca import Schedule, task


@task(freshness=Schedule("*/10 * * * *"))
def refresh() -> None:
    # do the work: call an API, rebuild a file, send a report
    print("ran at", datetime.now())
```

```bash
barca serve job.py
```

```
[barca] scheduling 1 task:
  job.py:refresh — */10 * * * * (next 2026-10-20 22:50:00)
[barca] serving on http://127.0.0.1:8274  (1 file)
```

`refresh` now runs every ten minutes for as long as the server is up.

## What a tick does

A tick of a scheduled node is one run with several targets: the scheduled node, then every
`Always` node below it. The walk down from the scheduled node goes past a `Manual` node, which
is not updated, and stops at a node with a `Schedule` of its own, which follows its own ticks.
Nodes with no scheduled node above them are never touched by a tick; they are computed only when
a target needs them as an input and they have no cached result.

A tick brings each target up to date. It does not force anything to recompute.

| Node in the tick | On each tick |
|---|---|
| the scheduled sensor | The sensor is polled and its value is recorded. |
| the scheduled asset | Sensors upstream of it are polled. Each asset upstream of it, and the asset itself, runs only if there is no cached result for its current code and inputs. |
| the scheduled task | The task runs. Its upstream sensors and assets are handled as above. |
| an `Always` asset below | Computed if it has no cached result for its current run hash; otherwise up to date. |
| an `Always` task below | Runs if its last successful run used other versions of its inputs (other code, another result of an asset it reads, another value of a sensor it reads), if a task it reads has run successfully since, or if it has never run. Otherwise it does not run. |

So a schedule on a sensor is enough to keep everything below it current: when the sensor returns
the same value nothing else runs, and when it returns a new one the same run computes the assets
that read it and fires the tasks below them.

Outside data must come in through a sensor. The cache is keyed by the sensor's value, not by
time: return something that identifies the version of the data, such as an etag or a
last-modified time. A scheduled asset that fetches data in its own body, with no sensor
upstream, is computed once and then served from cache on every tick
([Sensors](/guide/#4-sensors-data-from-outside)).

The older `POST /run/{task}` alias on the server is different from a tick: it recomputes every
upstream asset. `POST /runs` with `{"command": "run"}` behaves like a tick.

## `Always` and `Manual`

`Always` (the default for assets and tasks) and `Manual` (the default for sensors) say how a
node follows the schedules above it.

| Freshness | Meaning |
|---|---|
| `Schedule("<cron>")` | Fires on a cron schedule while `barca serve` is running. |
| `Always` | Brought up to date at each tick of a scheduled node above it, in that tick's run. |
| `Manual` | Refreshed by hand only: never by a tick, and never by a request for something below it, which reads its current result. A sensor is the exception: every run that needs its value polls it, whatever its freshness. |

```python
# pipeline.py
from pathlib import Path

from barca import Manual, Schedule, asset, sensor, task


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

When `prices` changes, the tick computes `report` reading the pinned `baseline`, and `publish`
does not fire.

### `Manual` pins itself

A `Manual` asset or task is never run by a tick, and `barca get` or `barca run` of something
below it does not recompute it: whatever reads it reads its latest successful result, even when
its inputs or code have changed since. Nodes below it are hashed against that result (its run
hash and its bytes), so it does not make them stale until it changes, and once it has a new
result they are stale relative to it: a refresh by hand that gives new bytes for the same code
and inputs (a reference table that reads external data) makes `barca status` report them stale,
and the next `barca get` or tick recomputes them. A refresh that gives the same bytes changes
nothing below it. When it is stale, `barca get report` says so on stderr:

```
[barca] note: baseline is Manual and stale; barca get baseline pipeline.py --refresh baseline to recompute
```

Only naming it in `--refresh` recomputes it: `--refresh baseline` or `--refresh-all`, also when
it is the target (`barca get baseline` alone returns its current result, with the note when it
is stale). A `Manual` task named as the target of `barca run` runs. The next tick then finds what reads it out of date and updates it.

- A `Manual` node that has never been computed cannot be read. `barca get report` is then a
  usage error (exit 2) whose remediation is that command. Under a tick, the nodes that read it
  are reported as `skipped` steps with reason `blocked`, and the tick is not an error.
- Sensors are polled whenever a run needs them, whatever their freshness.
- A partitioned `Manual` node is not pinned yet: it is computed like an `Always` node.
- `POST /get/{node}`, `POST /run/{task}` and `POST /run` compute `Manual` upstreams as before.

### Upgrading

Two changes are breaking:

- An `Always` node below a scheduled node, tasks included, now runs at that node's ticks. Before,
  only the scheduled node ran. Mark a node `Manual` to keep it out of the ticks.
- `barca get` and `barca run` no longer recompute a stale `Manual` upstream; they read its last
  result. Name it with `--refresh` to recompute it.

## Failures and `max_failures`

A node that fails in a tick's run stops only what depends on it; the other targets still run.
The run's status is then `failed`. The failed node is still out of date, so the next tick tries
it again.

`Schedule(cron, max_failures=N)` caps the consecutive failed ticks of a scheduled node. The
default is 5; the value must be a positive integer literal, passed by keyword. Past the cap the
node's ticks are skipped, the server logs

```
[barca] pipeline.py:inbox failing: 3 consecutive failed ticks (max_failures=3); skipped until an explicit request or a change in its inputs
```

and `GET /schedule` reports `failing: true` with `failures` and `max_failures`. The count is kept
in the metadata DB, so it survives a restart. It resets on a successful tick, and when something
the tick failed on succeeds in another run: an explicit request for the scheduled node or for a
node that failed (`barca get`, `barca run`, `POST /get`, `POST /run`), or a new result of one of
that node's direct inputs.

```python
@sensor(freshness=Schedule("*/5 * * * *", max_failures=3))
def inbox() -> tuple[bool, str]:
    ...
```

`max_failures`, like the rest of the freshness, is not part of the definition hash: changing it
does not invalidate a cached result.

## Example: one scheduled task below a sensor

```python
# pipeline.py
import time

from barca import Schedule, asset, sensor, task


@sensor()
def inbox() -> tuple[bool, dict]:
    # Return something that identifies the version of the outside data.
    return True, {"version": int(time.time()) // 20}


@asset(inputs={"src": inbox})
def orders(src: dict) -> dict:
    print("computing orders for version", src["version"])
    return {"rows": 3}


@task(inputs={"o": orders}, freshness=Schedule("*/10 * * * * *"))
def publish(o: dict) -> None:
    print("publishing", o)
```

The sensor's value changes every 20 seconds and the schedule fires every 10. In the first
tick below `orders` is computed. In the second the sensor's value is unchanged, so `orders`
is a cache hit and only the sensor and the task run:

```
[barca] scheduled run pipeline.py:publish → 14ca5c7b3768
[barca] importing /srv/project/pipeline.py
[barca] step:pipeline.py:inbox completed 0.0s (1/3)
[barca] step:pipeline.py:orders completed 0.0s (2/3)
[barca] step:pipeline.py:publish completed 0.0s (3/3)
[barca] 3/3 steps | done in 0.0s
[barca] scheduled run pipeline.py:publish → 14cd98ec4433
[barca] importing /srv/project/pipeline.py
[barca] step:pipeline.py:inbox completed 0.0s (1/3)
[barca] step:pipeline.py:orders cached
[barca] step:pipeline.py:publish completed 0.0s (2/3)
[barca] 2/3 steps | done in 0.0s
```

What the steps print is recorded with the run, not written to the server's stderr. `barca logs`
reads it back by run id:

```
$ barca logs 14ca5c7b3768 --pretty
computing orders for version 89627531
publishing {'rows': 3}
```

### Two scheduled nodes that share an upstream

Each scheduled node gets its own run. Two scheduled nodes that are due at the same tick and
share an upstream run separately, and both may compute the shared upstream; the sensor also
runs twice, so the two runs can see different data
([issue #80](https://github.com/barca-orc/barca/issues/80)). Schedule the sensor once instead
and leave the nodes below it `Always`: one tick then updates all of them in one run.

## Cron reference

A schedule has 5 fields (`minute hour day-of-month month day-of-week`) or 6 with a leading
seconds field. There is no year field.

| Cron            | Fires                          |
| --------------- | ------------------------------ |
| `*/10 * * * *`  | every 10 minutes               |
| `0 * * * *`     | every hour, on the hour        |
| `0 5 * * *`     | every day at 05:00             |
| `0 9 * * 1`     | 09:00 every Monday             |
| `0 0 1 * *`     | midnight on the 1st each month |
| `*/15 * * * * *` | every 15 seconds              |
| `0 */2 * * * *` | every 2 minutes, on the minute |

The scheduler checks once a second, so one second is the shortest interval. A 5-field
expression fires at second 0 of each matching minute.

## Keeping it running

Schedules fire only while `barca serve` is running, so run it under a supervisor. A minimal
systemd unit:

```ini
# /etc/systemd/system/barca.service
[Service]
ExecStart=/usr/local/bin/barca serve /srv/pipelines/job.py --timezone utc
Restart=always
WorkingDirectory=/srv/pipelines

[Install]
WantedBy=multi-user.target
```

`systemctl stop` sends SIGTERM. Barca then drains: it refuses new runs and stops firing
schedules, lets the runs in flight finish for up to `--drain-timeout` seconds (default 30;
`[server] drain_timeout` in `barca.toml`) and exits 0. Runs still going at the deadline, or at
a second SIGTERM, are cancelled with reason `server shutdown`, and the server exits 1. systemd
waits 90 seconds (`TimeoutStopSec`) before it kills the service; keep that longer than the
drain timeout. For a container, see [Deploying](/deploying/#in-a-container).

## Inspecting the schedule

`barca list job.py` shows each schedule and its next fire time without a server. It evaluates
the cron expression in the local time of the machine it runs on and says so in the column
header, `NEXT FIRE (LOCAL TIME)`. It does not know what `--timezone` a server was started with:
for `0 5 * * *` it shows 05:00 local, while a server running with `--timezone utc` fires at
05:00 UTC.

While the server is running, `GET /schedule` returns each job's next fire time, last fire time,
last run id, last status and consecutive failed ticks
([Server API](/reference/server-api/#scheduling)), and the web UI at
`/ui/` shows the next scheduled run of each node. Both are computed in the server's
`--timezone`, so they are the times the job will fire at.

## Caveats

- **Timezone.** Cron is evaluated in the machine's local time by default. Pass
  `--timezone utc` or an IANA name (`--timezone America/New_York`). `local` and `utc` are
  accepted in any letter case; IANA names are spelled as in the tz database. Any other value is
  a usage error: the server exits 2 and does not start.
- **Catch-up.** If a tick passed while the server was down, the job fires once at startup
  (`[barca] catch-up run job.py:refresh → ...`). Ticks missed during a long outage are not
  replayed one for one. A job seen for the first time waits for its next tick.
- **No overlap.** If a run of the job's node is pending or running when a tick arrives, that
  tick is skipped, not queued, whoever started the run: an earlier tick, `POST /runs` naming
  the node or getting every asset and sensor (so not a scheduled task), or a `barca get` /
  `barca run` of the node from a shell on the same project (the same `.barca/`) whose process
  is still alive. A `running` row left by a killed CLI run does not block. The server logs
  `[barca] scheduled run job.py:refresh skipped — run 1b942bb33182 of the same node still in
  flight`, `GET /schedule` reports it as `last_skipped`, `skip_reason: "in_flight"` and
  `skipped_for` (in memory only), and telemetry sends a `barca.run` span with status `skipped`.
  A run of another node, even one downstream whose cone includes this one, does not hold the
  tick back.
- **Pausing one job.** `POST /schedule/{id}/pause` (optionally `?until=<RFC 3339>`) holds one
  job back and `POST /schedule/{id}/resume` lets it fire again; its ticks meanwhile are skipped
  with `skip_reason: "paused"`, and are not caught up. The pause is kept in the metadata DB, so
  a restart does not resume it. See the [server API](/reference/server-api/#scheduling).
- **What started a run.** A scheduled run is recorded with `triggered_by: "schedule"`, the tick
  (`scheduled_at`) and the schedule (`schedule_id`), in `barca history --json`,
  `GET /runs/{run_id}` and the `barca.run` span.
- **New files and edits.** A run reads the source again each time, so an edit to a function
  takes effect at the next tick. A changed cron expression or a new scheduled node needs a
  restart, or `--watch`. A file added after startup needs a restart either way.
- **Remote stores.** With a remote store configured, `barca serve` uploads artifacts and backs
  its metadata DB up every minute it changed; see
  [Deploying](/deploying/#with-a-remote-store).

[Freshness and Schedules](/workflows/05-schedule-driven-reconciliation-and-effects/) walks
through a pipeline with scheduled, `Always` and `Manual` nodes.
