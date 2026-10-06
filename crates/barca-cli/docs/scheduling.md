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

What a tick does to the node it fires:

- A scheduled **asset** is recomputed, whatever is cached for it. Its upstream assets come from
  cache when they are fresh. Assets downstream of it with `Always` freshness are recomputed in
  the same run, so they never hold a result built from the previous one. A `Manual` or scheduled
  asset, a task or a sensor downstream is left alone, and so is everything behind it.
- A scheduled **task** runs. Its upstream assets come from cache when they are fresh, as with
  `barca run <task>`.
- A scheduled **sensor** is polled. Assets that read it recompute the next time a run reaches
  them, if its output changed (`barca docs cache`).

A tick is skipped while the previous run of the same node is still going.

```bash
barca list pipeline.py                         # shows each schedule and its next fire time
barca serve pipeline.py                        # HTTP API on 127.0.0.1:8274 + scheduler
barca serve pipeline.py --timezone utc         # evaluate cron in UTC (default: local)
barca serve pipeline.py --no-schedule          # API only, no scheduler
barca serve pipeline.py --watch                # dev: re-parse the DAG when files change
```

`serve` binds to `127.0.0.1` with no authentication. Endpoints are documented at
https://barca.sh/reference/server-api/ and `GET /schedule` reports live schedule status.
Full model: https://barca.sh/scheduling/.
