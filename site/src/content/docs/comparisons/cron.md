---
title: "Barca vs cron / systemd timers"
description: What a scheduled job gets from a crontab line, a systemd timer, and a barca Schedule under barca serve, feature by feature.
---

Last measured: 2026-07-19, with barca 0.7.0 and cron and systemd (versions not recorded), on a machine that was not recorded. Not re-run since. Re-run tracked in [#277](https://github.com/barca-orc/barca/issues/277).

This page contains no timings. It compares what each tool does for one scheduled job. The cron
column describes [cronie's crontab(5)](https://man7.org/linux/man-pages/man5/crontab.5.html) and
the systemd column
[systemd.timer(5)](https://man7.org/linux/man-pages/man5/systemd.timer.5.html); both were checked
against those manual pages on 2026-10-07. Other cron implementations differ.

A crontab line or a systemd timer starts a process on a schedule. If the job should also retry,
record its runs, or depend on another job, you write that around it. Barca's scheduler has those
built in, and in exchange needs a long-running `barca serve` process.

## The same job, three ways

**crontab:**

```text
*/10 * * * * cd /srv/pipelines && /usr/bin/python3 refresh.py >> /var/log/refresh.log 2>&1
```

**systemd timer** (two files):

```ini
# refresh.timer
[Timer]
OnCalendar=*:0/10
Persistent=true

[Install]
WantedBy=timers.target
```

```ini
# refresh.service
[Service]
ExecStart=/usr/bin/python3 /srv/pipelines/refresh.py
WorkingDirectory=/srv/pipelines
```

**Barca** (a decorator on the function, and a server process):

```python
# job.py
from barca import task, Schedule

@task(freshness=Schedule("*/10 * * * *"), retries=3, retry_backoff=1.0)
def refresh() -> None:
    ...  # the work
```

```bash
barca serve job.py
```

`barca list job.py` shows the schedule and its next fire time without starting the server
(output from barca 0.18.0):

```
NAME            KIND  FRESHNESS           NEXT FIRE (LOCAL TIME)  DEPS
----------------------------------------------------------------------
job.py:refresh  task  cron: */10 * * * *  2026-10-07 14:20:00     -
```

## Feature by feature

| | cron (cronie) | systemd timer | Barca |
| --- | --- | --- | --- |
| **Where the schedule lives** | a crontab | a `.timer` unit and a `.service` unit | on the function, in the Python file |
| **Finest schedule** | one minute | `OnCalendar` accepts seconds; `AccuracySec` defaults to 1 minute and has to be lowered | one second (6-field cron) |
| **Retries** | none | `Restart=` on the service unit | `retries=N, retry_backoff=...` on the decorator; the delay grows linearly |
| **Catch-up after downtime** | none | `Persistent=true`: the unit is triggered once if a trigger was missed while the timer was inactive | fires once on restart if a tick was missed while `barca serve` was down |
| **Overlapping runs** | not prevented | a unit that is still active when the timer elapses is not started again | a tick is skipped while the previous run of the same node is still going |
| **Timezone** | system time zone, or `CRON_TZ` per crontab | set in the calendar expression | `--timezone` on `barca serve`: local (default), `utc`, or an IANA name |
| **Run history** | whatever the job logs | the journal (`journalctl`) | rows in `.barca/metadata.db`, shown by `barca history` |
| **Schedule status** | none | `systemctl list-timers` | `barca list`, and `GET /schedule` while the server runs |
| **Dependencies between jobs** | none | `After=` / `Requires=` between units | a scheduled node can take assets, sensors and other tasks as inputs |
| **Process model** | starts a process per tick | starts a process per tick | one long-running `barca serve` process |

The barca column is from the [Scheduling guide](/scheduling/) and
[Server API](/reference/server-api/#scheduling), and `--timezone`, `barca list` and the example
above were run on barca 0.18.0. Catch-up, overlap skipping and retries were not re-tested for
this page.

Three limits on the barca side:

- Schedules fire only while `barca serve` is running. Something has to keep it running; see
  [Keeping it running](/scheduling/#keeping-it-running), which uses a systemd unit.
- Ticks missed during a long outage are not replayed one for one. The job fires once.
- A tick brings a scheduled asset up to date; it does not force it to recompute. An asset that
  fetches outside data in its own body is computed once and then served from cache on every
  tick. Outside data has to come in through a sensor
  ([Sensors](/workflows/06-sensors-and-external-observations/)). A scheduled task runs on every
  tick.

## When to use which

cron or a systemd timer fits when:

- the job is one script with no retries and no dependencies, and its logging is already handled;
- you do not want a long-running process;
- the job is not written in Python.

Barca fits when:

- there are several scheduled jobs and some depend on others or on shared data;
- you want retries, catch-up and overlap skipping without writing them;
- you want a record of what ran, when, and whether it succeeded;
- you want the schedule in the same file as the code it runs.

The cron reference, sub-minute schedules, timezones and running `barca serve` under a supervisor
are in the [Scheduling guide](/scheduling/).
