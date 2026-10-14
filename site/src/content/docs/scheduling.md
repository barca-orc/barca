---
title: Scheduling
description: Run assets, tasks and sensors on a cron schedule with barca serve, what a tick does, and the limits of scheduling in 0.18.0.
---

A node declared with `freshness=Schedule("<cron>")` runs on that cron schedule while
`barca serve` is running. The scheduler is part of the server and is on unless you pass
`--no-schedule`. `barca get` and `barca run` never fire schedules.

Output on this page is from barca 0.18.0.

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
[barca] serving on http://127.0.0.1:8274  (1 file)
[barca] scheduling 1 task:
  job.py:refresh — */10 * * * * (next 2026-10-07 14:30:00)
```

`refresh` now runs every ten minutes for as long as the server is up.

## What a tick does

A tick starts one run for the scheduled node, the same run `barca get <asset>` or
`barca run <task>` would start. It brings the node up to date. It does not force anything to
recompute.

| Scheduled node | On each tick |
|---|---|
| asset | Sensors upstream of it run. Each asset upstream of it, and the asset itself, runs only if there is no cached result for its current code and inputs. |
| task | The task runs. Its upstream sensors and assets are handled as above. |
| sensor | The sensor runs and its value is recorded. The assets that read it are not triggered. |

So outside data must come in through a sensor: a scheduled asset that fetches data in its own
body is computed once and then served from cache on every tick
([Sensors](/guide/#4-sensors-data-from-outside)). And the schedule belongs on the node whose
result you want, not on the sensor: the tick of an asset or task at the end of the pipeline
runs the sensors above it and computes whatever their values made stale.

`POST /run/{task}` on the server is different from a tick: it recomputes every upstream asset.

## `Always` and `Manual`

Only `Schedule` causes runs. `Always` (the default for assets and tasks) and `Manual` (the
default for sensors) are recorded and shown by `barca list`, and nothing acts on them. What
they should do in `barca serve` is proposed in RFC-0008
([PR #276](https://github.com/barca-orc/barca/pull/276)).

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
[barca] scheduled run pipeline.py:publish → 52f9c9c52e70
[barca] step:pipeline.py:inbox completed 0.0s (1/3)
computing orders for version 89569899
[barca] step:pipeline.py:orders completed 0.0s (2/3)
publishing {'rows': 3}
[barca] step:pipeline.py:publish completed 0.0s (3/3)
[barca] 3/3 steps | done in 0.0s
[barca] scheduled run pipeline.py:publish → 52fa627180a8
[barca] step:pipeline.py:inbox completed 0.0s (1/3)
[barca] step:pipeline.py:orders cached
publishing {'rows': 3}
[barca] step:pipeline.py:publish completed 0.0s (2/3)
[barca] 2/3 steps | done in 0.0s
```

### Two scheduled nodes that share an upstream

Each scheduled node gets its own run. Two scheduled nodes that are due at the same tick and
share an upstream run separately, and both may compute the shared upstream; the sensor also
runs twice, so the two runs can see different data. This is a known limitation
([issue #253](https://github.com/barca-orc/barca/issues/253)). Until it is fixed, give nodes
that share an upstream one scheduled node below them, as above, or schedules that do not
coincide.

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

`systemctl stop` sends SIGTERM. Barca then cancels the runs in flight, records them as
`cancelled` and exits 0, normally in less than a second. For a container, see
[Deploying](/deploying/#in-a-container).

## Inspecting the schedule

`barca list job.py` shows each schedule and its next fire time without a server. It evaluates
the cron expression in the local time of the machine it runs on and says so in the column
header, `NEXT FIRE (LOCAL TIME)`. It does not know what `--timezone` a server was started with:
for `0 5 * * *` it shows 05:00 local, while a server running with `--timezone utc` fires at
05:00 UTC.

While the server is running, `GET /schedule` returns each job's next fire time, last fire time,
last run id and last status ([Server API](/reference/server-api/#scheduling)), and the web UI at
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
- **No overlap with itself.** If a job's previous run is still going when its next tick
  arrives, that tick is skipped, not queued.
- **New files and edits.** A run reads the source again each time, so an edit to a function
  takes effect at the next tick. A changed cron expression or a new scheduled node needs a
  restart, or `--watch`. A file added after startup needs a restart either way.
- **Remote stores.** `barca serve` does not support shared history. With a remote store
  configured, set `BARCA_STATE=off`; see
  [Deploying](/deploying/#with-a-remote-store).

[Freshness and Schedules](/workflows/05-schedule-driven-reconciliation-and-effects/) shows
ticks of scheduled, `Always` and `Manual` nodes side by side.
