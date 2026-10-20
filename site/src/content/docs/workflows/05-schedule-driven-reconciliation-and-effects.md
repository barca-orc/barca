---
title: "Workflow: Freshness and Schedules"
description: What the freshness argument does. Schedule fires a node under barca serve, Always nodes below it follow its ticks, and a Manual node is refreshed by hand only.
---

Every `@asset`, `@sensor` and `@task` takes a `freshness=` argument: `Always`, `Manual` or
`Schedule("<cron>")`.

- `Schedule("<cron>")` makes `barca serve` start a run on each cron tick. It has no effect on
  `barca get` or `barca run`.
- `Always` (the default for assets and tasks) nodes below a scheduled node are brought up to
  date in that node's ticks.
- `Manual` (the default for sensors) assets and tasks are refreshed by hand only. A tick never
  runs one, and a command that reads one reads its last result.

This is RFC-0008. Before it, `Always` and `Manual` were recorded and shown and changed nothing;
see [Upgrading](#upgrading) for what that means for an existing pipeline.

## Example

```python
# pipeline.py
from pathlib import Path

from barca import Manual, Schedule, asset, sensor, task


@sensor(freshness=Schedule("*/5 * * * *"))   # the only schedule
def prices() -> tuple[bool, str]:
    return True, Path("prices.txt").read_text()


@asset(inputs={"etag": prices}, freshness=Manual)
def baseline(etag: str) -> dict:             # refreshed by hand only
    return {"baseline": etag}


@asset(inputs={"baseline": baseline, "etag": prices})   # Always (the default)
def report(baseline: dict, etag: str) -> dict:
    return {"baseline": baseline["baseline"], "etag": etag}


@task(inputs={"r": report})                  # Always
def send_report(r: dict) -> None:
    print("sending", r)


@task(inputs={"baseline": baseline})         # Always
def publish(baseline: dict) -> None:
    print("publish", baseline["baseline"])


@asset()                                     # Always; nothing scheduled is above it
def standalone() -> dict:
    return {"x": 1}
```

`barca list pipeline.py` shows each node's freshness, and the cron and next fire time of
`prices`.

## What `barca serve` does on a tick

```bash
barca serve pipeline.py
```

Every five minutes the server starts one run for `prices`. Its targets are `prices` and every
`Always` node below it: `report`, `send_report` and `publish`. The walk down goes past
`baseline`, which is `Manual` and is not updated; `publish` is below `prices` only through it.
A node with a `Schedule` of its own would stop the walk and follow its own ticks.

Each target is brought up to date, not forced:

- **`prices` is polled.** When it returns the value it returned last time, `report` has a cached
  result for its run hash and `send_report`'s inputs are the ones it last ran with, so nothing
  else runs.
- **When `prices` returns a new value**, the same run computes `report` once, reading the
  pinned `baseline`, and fires `send_report`, because the result of `report` it reads changed.
- **`publish` does not fire**, because nothing it reads changed: `baseline` was not
  recomputed.
- **`standalone` is never run by a tick.** No scheduled node is above it. A node like it is
  computed by a tick only when a target reads it and it has no cached result.

The cache is keyed by the sensor's value, not by time. A scheduled asset that fetches outside
data in its own body, with no sensor upstream, is computed once and then served from cache on
every tick. Outside data has to come in through a sensor: see
[Outside data that changes in place](/workflows/06-sensors-and-external-observations/#outside-data-that-changes-in-place).

Each tick is an ordinary run in `barca history` and `GET /runs`. Its result lists a step for
each target that was checked, so the `Always` nodes below `prices` appear in the run of
`prices`.

## Refreshing a `Manual` node

`baseline` changes only when it is named:

```bash
barca get baseline pipeline.py                 # recompute it as the target
barca get report pipeline.py --refresh baseline
```

The next tick finds `report` and `publish` out of date against the new `baseline`, computes
`report` and fires `send_report` and `publish`.

Without that, `barca get report pipeline.py` does not recompute `baseline` either, even when
its input `prices` has a new value: it reads the last result of `baseline` and says on stderr
that it is stale:

```
[barca] note: baseline is Manual and stale; barca get baseline pipeline.py --refresh baseline to recompute
```

If `baseline` has never been computed, it cannot be read. `barca get report pipeline.py` then
exits 2 and names the command that computes it. Under a tick, `report`, `send_report` and
`publish` are reported as `skipped` steps with reason `blocked`, and the tick is not an error.

Sensors are not pinned: they are polled whenever a run needs them, whatever their freshness.
A partitioned `Manual` node is not pinned yet; it is computed like an `Always` node. Requests to
the server (`POST /get/{node}`, `POST /run/{task}`, `POST /run`) compute `Manual` upstreams as
before.

## When a tick fails

A node that fails in a tick stops only what depends on it. If `report` raises, `send_report`
does not run, `publish` still does when its inputs changed, and the run's status is `failed`.
`report` is still out of date, so the next tick tries it again.

`Schedule(cron, max_failures=N)` (default 5) caps the consecutive failed ticks of the scheduled
node. Past the cap its ticks are skipped and `GET /schedule` reports `failing: true` with
`failures` and `max_failures`. The count survives a restart; it resets on a successful tick, or
when the scheduled node, a node that failed, or a direct input of a failed node succeeds in
another run, such as `barca get report pipeline.py` after a fix. See
[Scheduling](/scheduling/#failures-and-max_failures).

## Summary

| Freshness | `barca get` / `barca run` | `barca serve` tick |
|---|---|---|
| `Always` (default for assets and tasks) | computed when the target needs it and it has no cached result | below a scheduled node: an asset is computed if it has no cached result; a task runs if its inputs changed since its last successful run. Otherwise not run by a tick. |
| `Manual` (default for sensors) | an asset or task upstream is read as it is, even when stale; recomputed only when named | never run by a tick; nodes below read its last result |
| `Schedule("<cron>")` | no effect | sensor: polled. Asset: checked, computed if no cached result. Task: runs. |

## Upgrading

Two changes are breaking for a pipeline written before RFC-0008:

- An `Always` node below a scheduled node, tasks included, now runs at that node's ticks.
  Before, only the scheduled node ran, and a scheduled sensor did not trigger what read it.
  Mark a node `Manual` to keep it out of the ticks.
- `barca get` and `barca run` no longer recompute a stale `Manual` upstream. Name it with
  `--refresh` to recompute it.

Two separately scheduled nodes that share an upstream still run separately, and if their ticks
coincide both may compute it ([issue #80](https://github.com/barca-orc/barca/issues/80)):
schedule the sensor once and let the `Always` nodes below it follow instead.

## Related pages

- [Scheduling](/scheduling/): cron syntax, time zones, keeping the server running.
- [Sensors](/workflows/06-sensors-and-external-observations/): outside data, and what a
  scheduled sensor does.
- [Server API](/reference/server-api/): `GET /schedule`, `POST /run/<task>`, cancelling a run.
- `barca docs scheduling` in the terminal.
