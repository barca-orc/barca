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

A tick is skipped while the previous run of the same scheduled node is still going.

Nodes that are due at the same tick and have a step in common run together, as one run over
the union of their cones, so the step they share is computed once: a sensor or asset upstream of
several of them, or a scheduled asset that a scheduled task reads. "Due at the same tick" is
about the moment, not the cron text: `0 5 * * *` and `*/5 * * * *` are due together at 05:00.
The nodes caught up when the server starts are treated the same way. Nodes with nothing in
common each get their own run, as does a node that a shared run would hold back (it would wait
for a step it does not depend on). Sharing a run does not tie the nodes to each other:

- "Still going" is judged per node. Once a node's own step has ended, its next tick fires, even
  while a slower node it ran with keeps the shared run open, and what that run computed is
  served from cache.
- A node that fails stops only the nodes downstream of it. The run is then `failed`, as
  `barca get a,b` is when one target fails: `GET /status/<run>` lists every node's outcome
  under `result.targets`, and the run is one `failed` row in `barca history`.
- `GET /schedule` reports each node's own `last_status`, and the run it last fired into as
  `last_run` (the same id for nodes that shared a run).
- A run is stopped after 10 minutes per node in it (20 minutes for two nodes), and
  `DELETE /run/<run>` cancels the whole run: nodes whose step had already ended keep their
  results.

In `barca history`, a shared run's `target` is the node ids separated by commas. Its `command`
is `get` when the nodes are all assets and sensors, `run` when they are all tasks, and `serve`
when it has both.

Known limit: runs are not shared when artifacts go to a remote store (`barca docs remote`).
There a step is recorded only when its run ends, so a node in a shared run could not fire again
until the whole run had ended. Such a server starts one run per due node, and an upstream that
two of them share may be computed by both.

```bash
barca list pipeline.py                         # shows each schedule and its next fire time
barca serve pipeline.py                        # HTTP API + scheduler + web UI on 127.0.0.1:8274
barca serve pipeline.py --timezone utc         # evaluate cron in UTC (default: local)
barca serve pipeline.py --no-schedule          # API only, no scheduler
barca serve pipeline.py --watch                # dev: re-parse the DAG when files change
barca serve pipeline.py --read-only            # inspect only: no runs, no scheduler
```

`--read-only` serves the API without the ability to change anything: run and cancel endpoints
return `403`, the scheduler never starts, and every read of the metadata DB goes through a
private copy, so it is safe to point at a project another process is running.

`serve` binds to `127.0.0.1` with no authentication. Open `http://127.0.0.1:8274/` for the web
UI. Endpoints are documented at https://barca.sh/reference/server-api/ and `GET /schedule`
reports live schedule status. Behind nginx (any path prefix, live logs included):
https://barca.sh/deploying/.
Full model: https://barca.sh/scheduling/.
