---
title: Scheduling
description: Use Barca as a simple task scheduler — run a script on a cron schedule with barca serve.
---

Barca can act as a plain **task scheduler**: decorate a function with a cron
expression and leave `barca serve` running. No external cron, no DSL — the
scheduler is built into the server and on by default.

## Run a script every 10 minutes

```python
# job.py
from barca import task, Schedule

@task(freshness=Schedule("*/10 * * * *"))
def refresh():
    # ...do the work: hit an API, rebuild a file, send a report...
    print("ran at", __import__("datetime").datetime.now())
```

```bash
barca serve job.py
```

That's the whole setup. `barca serve` parses the file, finds every scheduled
definition, and fires each one on its cron tick. `refresh` now runs every ten
minutes for as long as the server is up:

```
[barca] serving on http://127.0.0.1:8274  (1 file)
[barca] scheduling 1 asset:
  job.py:refresh — */10 * * * * (next 2026-07-16 12:30:00)
```

`@task` is the right decorator when the point is the side effect — a task always
re-runs when its tick fires. Use `@asset(freshness=Schedule(...))` instead when
the function *produces data* you want kept fresh. A scheduled asset is
recomputed on every tick, with its upstream assets served from cache when they
are fresh; the `Always` assets downstream of it are recomputed in the same run.
A scheduled task also reuses cached upstream assets.

## Cron reference

Barca uses standard **5-field** cron (`minute hour day-of-month month day-of-week`),
evaluated in the machine's local time by default:

| Cron            | Fires                          |
| --------------- | ------------------------------ |
| `*/10 * * * *`  | every 10 minutes               |
| `0 * * * *`     | every hour, on the hour        |
| `0 5 * * *`     | every day at 05:00             |
| `0 9 * * 1`     | 09:00 every Monday             |
| `0 0 1 * *`     | midnight on the 1st each month |

### Sub-minute schedules

For cadences faster than a minute, add a **sixth** leading field for seconds
(`second minute hour day-of-month month day-of-week`). Barca's scheduler
evaluates at **1-second resolution**:

| Cron              | Fires             |
| ----------------- | ----------------- |
| `*/15 * * * * *`  | every 15 seconds  |
| `*/5 * * * * *`   | every 5 seconds   |
| `0 */2 * * * *`   | every 2 minutes, on the minute |

A 5-field expression has its seconds pinned to `0`, so it fires once per matching
minute exactly as before — adding the seconds field is the only thing that opts
into sub-minute firing. One second is the finest granularity.

## Keeping it running

The scheduler only fires while `barca serve` is alive, so run it under a process
supervisor for anything long-lived. A minimal systemd unit:

```ini
# /etc/systemd/system/barca.service
[Service]
ExecStart=/usr/local/bin/barca serve /srv/pipelines/job.py
Restart=always
WorkingDirectory=/srv/pipelines

[Install]
WantedBy=multi-user.target
```

In a container, `barca serve job.py` is a fine foreground entrypoint. Barca
persists each job's last fire time, so a **missed tick during a restart fires
once on catch-up** rather than being lost (see [caveats](#caveats)).

## Inspecting the schedule

Without starting a server, list definitions and their next fire time:

```bash
barca list job.py
```

```
NAME          KIND  FRESHNESS         NEXT FIRE            DEPS
---------------------------------------------------------------
job.py:refresh  task  cron: */10 * * * *  2026-07-16 12:30:00  -
```

While the server is running, `GET /schedule` returns live status (next fire,
last run id, last status) for each job. See the
[Server API](/reference/server-api/#scheduling) for the response shape.

## Caveats

- **Timezone** — cron is local time by default. Pass `--timezone utc` or an IANA
  name (`--timezone America/New_York`) to change it.
- **Catch-up** — if a tick elapsed while the daemon was down, the job fires
  **once** on restart to catch up. Ticks missed during a long outage are not
  replayed one-for-one, and brand-new jobs are anchored to "now" (no
  first-launch stampede).
- **No self-overlap** — if a job's previous run is still going when the next
  tick arrives, that tick is skipped.
- **Disable it** — `barca serve --no-schedule job.py` serves the HTTP API
  without firing anything on a clock.

For the full semantics — how staleness, sensors, and reconciliation interact
with schedules — see [Schedule-Driven Reconciliation](/workflows/05-schedule-driven-reconciliation-and-effects/).
