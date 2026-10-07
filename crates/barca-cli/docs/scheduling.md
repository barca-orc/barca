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

Nodes that are due at the same tick share one run when they have a step in common and that run
would plan nothing ahead of any of them except its own upstream. The step they share is then
computed once. "Due at the same tick" is about the moment, not the cron text: `0 5 * * *` and
`*/5 * * * *` are due together at 05:00. The nodes caught up when the server starts are treated
the same way.

- Shares a run: nodes that read the same upstream side by side (two scheduled tasks reading
  one asset, two scheduled assets reading one sensor), and a node with the nodes downstream of
  it (a scheduled asset and the scheduled tasks that read it).
- Does not share a run, and may compute the common step once each, as before 0.18: nodes at
  different depths below the step they share, and a node that also reads something the others
  do not. Example: `tracked` reads sensor `version`; task `publish` reads `feed` (which reads
  `version`) and `model`. One run would make `publish` wait for `tracked` and `tracked` wait for
  `model`, so each gets its own run and `version` is polled twice per tick. A run executes in
  phases, each waiting for the one before, and barca only shares a run that delays no node.
- Nodes with nothing in common always get their own run.

When a node is kept out of a run for the second reason, `barca serve` says so once on stderr
(`... runs on its own, not in one run with ...: there it would wait for ...`).

Sharing a run does not tie the nodes to each other:

- "Still going" is judged per node. Once a node's own step has ended, its next tick fires, even
  while a slower node it ran with keeps the shared run open, and what that run computed is
  served from cache.
- A node that fails stops only the nodes downstream of it. The run is then `failed`, as
  `barca get a,b` is when one target fails: `GET /status/<run>` lists every node's outcome
  under `result.targets`, and the run is one `failed` row in `barca history`.
- `GET /schedule` reports each node's own `last_status`, and the run it last fired into as
  `last_run` (the same id for nodes that shared a run).
- A shared run is stopped after 10 minutes per node in it (20 minutes for two nodes). So a
  node that hangs is stopped later than the 10 minutes it would get alone, and until then only
  that node's ticks are skipped. `DELETE /run/<run>` cancels the whole run: nodes whose step
  had already ended keep their results.
- The nodes of a shared run share one pool of workers (one per core). Each step waits only for
  its own inputs and a free worker, never for another node's step.

A run stopped by its time limit is `failed` in `GET /status/<run>` and `cancelled` in
`barca history`.

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
