# Scheduler example

Barca as a plain **task scheduler** — the smallest possible setup. Decorate a
function with a cron `Schedule` and leave `barca serve` running; each job fires
on its own tick. No external cron, no YAML, no daemon service to install.

```bash
barca serve job.py
```

```
[barca] serving on http://127.0.0.1:8274  (1 file)
[barca] scheduling 2 tasks:
  job.py:heartbeat — */15 * * * * * (next 2026-07-19 20:32:45)
  job.py:refresh   — */10 * * * *   (next 2026-07-19 20:40:00)
```

`job.py` defines two jobs:

| Job         | Cron              | Fires            |
| ----------- | ----------------- | ---------------- |
| `refresh`   | `*/10 * * * *`    | every 10 minutes |
| `heartbeat` | `*/15 * * * * *`  | every 15 seconds |

`heartbeat` uses the **6-field** form (a leading seconds field) for sub-minute
scheduling — Barca evaluates the schedule at 1-second resolution. Standard
5-field cron (`minute hour day-of-month month day-of-week`) works too, as
`refresh` shows.

Inspect the schedule without starting a server:

```bash
barca list job.py          # NEXT FIRE (LOCAL TIME) column shows each job's next tick
```

See the [Scheduling guide](https://barca.sh/scheduling/) for timezones, catch-up
on restart, and keeping `barca serve` alive under a process supervisor.
