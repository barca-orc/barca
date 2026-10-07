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
the function *produces data* you want kept fresh. A scheduled asset is checked
on every tick: the sensors upstream of it are polled, and it runs only if there
is no cached result for its current code and inputs. A scheduled task's upstream
assets are checked the same way. Bring outside data in through a `@sensor` that
returns something identifying the version of the data (an etag, a last-modified
time); an asset that fetches data in its own body with no sensor upstream is
computed once and then served from cache until its code changes.

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
- **Shared upstream, one run** — jobs due at the same tick that have a step in
  common (a sensor or asset upstream of several of them, or a scheduled asset that
  a scheduled task reads) run together as one run over the union of their cones, so
  that step is computed once. "The same tick" is the moment, not the cron text:
  `0 5 * * *` and `*/5 * * * *` are due together at 05:00. Jobs caught up at startup
  are treated the same way. Jobs with nothing in common each get their own run, and
  so does a job that a shared run would make wait for a step it does not depend on.
  With a remote artifact store, runs are not shared: each due job gets its own run,
  and an upstream two of them share may be computed by both.
- **No self-overlap, per job** — if a job's previous run is still going when the
  next tick arrives, that tick is skipped. "Still going" is the job's own step: once
  it has ended, the job's next tick fires even while a slower job it ran with keeps
  the shared run open.
- **A failure stays local** — a job that fails stops only the jobs downstream of it.
  The shared run is then `failed`, like `barca get a,b` when one target fails, and
  `GET /status/{run_id}` lists every job's outcome under `result.targets`.
  `GET /schedule` reports each job's own status.
- **Time limit** — a run is stopped after 10 minutes per job in it, so a run shared
  by three jobs has 30 minutes. Cancelling a shared run (`DELETE /run/{run_id}`)
  cancels all of it; jobs whose step had already ended keep their results.
- **Disable it** — `barca serve --no-schedule job.py` serves the HTTP API
  without firing anything on a clock.

For the full semantics — how staleness, sensors, and reconciliation interact
with schedules — see [Schedule-Driven Reconciliation](/workflows/05-schedule-driven-reconciliation-and-effects/).
