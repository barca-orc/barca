---
title: "Workflow: Freshness and Schedules"
description: What the freshness argument does in barca 0.18.0. Schedule fires nodes under barca serve; Always and Manual are recorded and shown.
---

Every `@asset`, `@sensor` and `@task` takes a `freshness=` argument: `Always`, `Manual` or
`Schedule("<cron>")`.

An earlier version of this page described a design in which `Always` nodes were kept up to date
automatically, `Manual` nodes were recomputed only on request, and a `Manual` upstream blocked
the nodes below it. That design is not implemented. In barca 0.18.0:

- `Schedule("<cron>")` makes `barca serve` trigger the node on each cron tick. It has no effect
  on `barca get` or `barca run`.
- `Always` and `Manual` are recorded and shown by `barca list`. They do not change what any
  command or the server computes.

Behavior for `Always` and `Manual` under `barca serve` is proposed in RFC-0008
([PR #276](https://github.com/barca-orc/barca/pull/276)). `barca docs scheduling` and
`barca docs assets` describe `Manual` as "only recomputed on an explicit refresh"; the runs below
show that this is not what 0.18.0 does.

Everything on this page was run with barca 0.18.0.

## Example

```python
# pipeline.py
from barca import asset, task, Manual, Schedule


@asset(freshness=Manual)
def source() -> dict:
    return {"rows": 3}


@asset(inputs={"src": source})              # freshness=Always (default)
def report(src: dict) -> dict:
    return {"rows": src["rows"], "ok": True}


@asset(freshness=Schedule("*/5 * * * * *"))  # every 5 seconds under `barca serve`
def prices() -> dict:
    return {"aapl": 1}


@task(inputs={"r": report}, freshness=Schedule("*/10 * * * * *"))
def send_report(r: dict) -> None:
    print(f"sending {r['rows']} rows")


@asset()                                     # Always; nothing scheduled reads it
def standalone() -> dict:
    return {"x": 1}
```

The six-field cron (seconds first) is only there to make the demonstration short. A five-field
expression such as `Schedule("0 5 * * *")` is daily at 05:00.

```bash
barca list pipeline.py
```

```
NAME                     KIND   FRESHNESS             NEXT FIRE            DEPS
-------------------------------------------------------------------------------
pipeline.py:standalone   asset  always                -                    -
pipeline.py:prices       asset  cron: */5 * * * * *   2026-10-07 14:17:25  -
pipeline.py:source       asset  manual                -                    -
pipeline.py:report       asset  always                -                    pipeline.py:source
pipeline.py:send_report  task   cron: */10 * * * * *  2026-10-07 14:17:30  pipeline.py:report
```

## One-shot commands ignore freshness

`barca get` and `barca run` compute whatever the target needs and has no cached result for,
whatever the freshness of each node.

```bash
barca get report pipeline.py     # source and report run
barca get report pipeline.py     # both cached
```

Change `source` to return `{"rows": 4}` and run the same command, with no `--refresh`:

```bash
barca status pipeline.py
barca get report pipeline.py --json
```

```
NAME         KIND   STATE        WHY             LAST RUN                           SHAPE          DEPS
prices       asset  never-run    no_record       -                                  -              -
source       asset  stale        changed         success 2026-10-07 18:12:46 0.00s  dict (1 key)   -
report       asset  stale        upstream_stale  success 2026-10-07 18:12:46 0.00s  dict (2 keys)  source
send_report  task   always-runs  task            -                                  -              report
```

```
[barca] 2/2 steps | done in 0.0s
{"elapsed_seconds":0.044105417,"final_output":{"ok":true,"rows":4}, ... "steps_executed":2,"warnings":[]}
```

The `Manual` asset recomputed because its code changed, and the `Always` asset below it
recomputed with it. `Manual` did not hold either back.

`barca get pipeline.py` with no target computes every asset and sensor, including the scheduled
`prices`, and skips tasks:

```
[barca] skipped 1 task (send_report): `barca get` without a target materializes assets only. Run a task with: barca run send_report pipeline.py
[barca] 1/3 steps | done in 0.0s
```

## What `barca serve` does on a tick

```bash
barca serve pipeline.py --port 28431
```

`source` was edited again (`{"rows": 5}`) before the server started. The server log:

```
[barca] scheduling 2 assets:
  pipeline.py:prices — */5 * * * * * (next 2026-10-07 14:12:55)
  pipeline.py:send_report — */10 * * * * * (next 2026-10-07 14:13:00)
[barca] serving on http://127.0.0.1:28431  (1 file)
[barca] scheduled run pipeline.py:prices → 51e1471d39b8
[barca] step:pipeline.py:prices cached
[barca] scheduled run pipeline.py:prices → 51e03a13d0c8
[barca] scheduled run pipeline.py:send_report → 51e03a42c1e8
[barca] step:pipeline.py:prices cached
[barca] step:pipeline.py:source completed 0.0s (1/3)
[barca] step:pipeline.py:report completed 0.0s (2/3)
sending 5 rows
[barca] step:pipeline.py:send_report completed 0.0s (3/3)
[barca] 3/3 steps | done in 0.0s
...
[barca] scheduled run pipeline.py:send_report → 51e2c66f6c00
[barca] step:pipeline.py:source cached
[barca] step:pipeline.py:report cached
sending 5 rows
[barca] step:pipeline.py:send_report completed 0.0s (1/3)
[barca] 1/3 steps | done in 0.0s
```

What this shows:

- **A scheduled asset is checked on each tick, not recomputed.** `prices` has a cached result for
  its current code, so every tick serves it from cache and its function does not run. An asset
  that fetches outside data in its own body is computed once and never again by its schedule.
  Outside data has to come in through a sensor: see
  [Outside data that changes in place](/workflows/06-sensors-and-external-observations/#outside-data-that-changes-in-place).
- **A scheduled task runs on each tick.** Its upstream assets are computed only when they have
  no cached result, exactly as with `barca run send_report pipeline.py`.
- **`Manual` and `Always` upstreams are treated the same.** The first `send_report` tick
  recomputed the edited `Manual` asset `source` and the `Always` asset `report`.
- **Only scheduled nodes are triggered.** `standalone` is `Always` and nothing scheduled reads
  it. The server never ran it:

```
NAME         KIND   STATE        WHY           LAST RUN                           SHAPE          DEPS
standalone   asset  never-run    no_record     -                                  -              -
prices       asset  cached       materialized  success 2026-10-07 18:12:47 0.00s  dict (1 key)   -
source       asset  cached       materialized  success 2026-10-07 18:13:00 0.01s  dict (1 key)   -
report       asset  cached       materialized  success 2026-10-07 18:13:00 0.00s  dict (2 keys)  source
send_report  task   always-runs  task          success 2026-10-07 18:13:10 0.00s  null           report
```

`GET /schedule` reports each scheduled node with its cron, last run and next fire time:

```json
[{"cron":"*/5 * * * * *","id":"pipeline.py:prices","kind":"asset","last_fired":1791396795,"last_run":"51e5bf2a66c0","last_status":"complete","next_fire":1791396800},
 {"cron":"*/10 * * * * *","id":"pipeline.py:send_report","kind":"task","last_fired":1791396790,"last_run":"51e2c66f6c00","last_status":"complete","next_fire":1791396800}]
```

Each tick is an ordinary run in `barca history` (command `get` for an asset or sensor, `run` for
a task).

## Summary

| Freshness | `barca get` / `barca run` | `barca serve` |
|---|---|---|
| `Always` (default for assets and tasks) | no effect | no effect; computed only when a triggered node needs it |
| `Manual` (default for sensors) | no effect | no effect; computed only when a triggered node needs it |
| `Schedule("<cron>")` | no effect | asset: checked on each tick, computed if no cached result. Task: runs on each tick. Sensor: runs on each tick; its consumers are not triggered. |

## Related pages

- [Scheduling](/scheduling/): cron syntax, time zones, keeping the server running.
- [Sensors](/workflows/06-sensors-and-external-observations/): outside data, and what a
  scheduled sensor does.
- [Server API](/reference/server-api/): `GET /schedule`, `POST /run/<task>`, cancelling a run.
- `barca docs scheduling` in the terminal.
