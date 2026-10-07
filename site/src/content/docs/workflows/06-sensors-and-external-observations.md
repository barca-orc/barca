---
title: "Workflow: Sensors and External Observations"
description: A sensor reports the version of data that lives outside the graph, so the assets that read that data re-run when it changes.
---

A `@sensor` is a function with no inputs that looks at something outside the graph (a file, a
bucket, a table) and returns `(update_detected, value)`. A sensor runs on every `barca get` or
`barca run` that includes it. Its `value` is part of the run hash of every asset that reads it.

Everything on this page was run with barca 0.18.0. The terminal manual covers the same ground in
`barca docs cache` ("External data that changes in place") and `barca docs assets` ("Sensors").

## Outside data that changes in place

An asset that reads a file or a bucket in its own body is computed once and then served from
cache. Its run hash covers its code and its inputs, and neither changes when the file does.

```python
# pipeline.py
import csv
from pathlib import Path

from barca import asset


@asset()
def orders() -> list:
    with Path("orders.csv").open() as f:
        return list(csv.DictReader(f))


@asset(inputs={"orders": orders})
def total(orders: list) -> dict:
    return {"rows": len(orders), "amount": sum(int(r["amount"]) for r in orders)}
```

```bash
printf 'id,amount\n1,10\n2,20\n' > orders.csv
barca get total pipeline.py --agent
printf '3,30\n' >> orders.csv
barca get total pipeline.py --agent
```

The second run does not see the new row:

```
[barca] step:pipeline.py:orders cached
[barca] step:pipeline.py:total cached
{"elapsed_seconds":0.00661275,"final_output":{"amount":30,"rows":2}, ...
```

Without a sensor, the way to pick up the change is `--refresh`, which recomputes the assets you
name and everything downstream of them. It takes one comma-separated list (`--refresh a,b`).

```bash
barca get total pipeline.py --refresh orders --agent
```

```
[barca] step:pipeline.py:orders completed 0.0s (1/2)
[barca] step:pipeline.py:total completed 0.0s (2/2)
[barca] 2/2 steps | done in 0.0s
{"elapsed_seconds":0.100022459,"final_output":{"amount":60,"rows":3}, ...
```

## Converting the asset to a sensor plus an asset

Add a sensor that returns something identifying the current version of the data, and make the
asset that reads the data take the sensor as an input. The asset body does not change.

```python
# pipeline.py
import csv
import hashlib
from pathlib import Path

from barca import asset, sensor


@sensor()
def orders_version() -> tuple[bool, str]:
    # Stands in for a blob's etag or a table's last-modified time.
    return True, hashlib.sha256(Path("orders.csv").read_bytes()).hexdigest()[:12]


@asset(inputs={"version": orders_version})
def orders(version: str) -> list:
    with Path("orders.csv").open() as f:
        return list(csv.DictReader(f))


@asset(inputs={"orders": orders})
def total(orders: list) -> dict:
    return {"rows": len(orders), "amount": sum(int(r["amount"]) for r in orders)}
```

The consumer receives the sensor's `value` only (here a `str`), not the tuple. `orders` does not
use `version` in its body; barca does not warn about an unused input that comes from a sensor.

### First run after the conversion

```bash
barca get total pipeline.py --agent
```

```
[barca] step:pipeline.py:orders_version completed 0.0s (1/3)
[barca] step:pipeline.py:orders completed 0.0s (2/3)
[barca] step:pipeline.py:total completed 0.0s (3/3)
[barca] 3/3 steps | done in 0.0s
{"elapsed_seconds":0.083302625,"final_output":{"amount":60,"rows":3}, ...
```

`orders` and `total` recompute once, although `orders.csv` has not changed since the last run.
Adding the input changed the definition of `orders`, so its run hash is new, and `total` follows.
Expect one recompute of every converted asset and everything downstream of it.

### Unchanged observation

```bash
barca get total pipeline.py --agent
```

```
[barca] step:pipeline.py:orders_version completed 0.1s (1/3)
[barca] step:pipeline.py:orders cached
[barca] step:pipeline.py:total cached
[barca] 1/3 steps | done in 0.1s
```

The sensor runs every time. It returned the same value, so its consumers are served from cache.
`steps_executed` is 1.

### Changed observation

```bash
printf '4,40\n' >> orders.csv
barca get total pipeline.py --agent
```

```
[barca] step:pipeline.py:orders_version completed 0.0s (1/3)
[barca] step:pipeline.py:orders completed 0.0s (2/3)
[barca] step:pipeline.py:total completed 0.0s (3/3)
[barca] 3/3 steps | done in 0.0s
{"elapsed_seconds":0.080977666,"final_output":{"amount":100,"rows":4}, ...
```

In the JSON output the re-run steps carry `"reason": "not_materialized"` with the detail `no
cached result for this code and these inputs`. There is no separate reason for "the sensor
changed".

### Returning to an earlier value

The cache is keyed by the sensor's value, not by time. Put the three-row file back and the result
computed for that value is reused:

```bash
printf 'id,amount\n1,10\n2,20\n3,30\n' > orders.csv
barca get total pipeline.py --agent
```

```
[barca] step:pipeline.py:orders_version completed 0.0s (1/3)
[barca] step:pipeline.py:orders cached
[barca] step:pipeline.py:total cached
[barca] 1/3 steps | done in 0.0s
{"elapsed_seconds":0.157731625,"final_output":{"amount":60,"rows":3}, ...
```

This is correct when the value identifies the data's content or version (a content hash, an
etag). It is wrong for a value that can repeat while the data differs, such as a row count or a
status flag: the asset would be served the result computed the last time the count had that
value.

## Return only what identifies the data

A sensor whose value differs on every run re-runs its consumers on every run. This sensor
includes the time it was checked:

```python
import time

from barca import asset, sensor


@sensor()
def checked() -> tuple[bool, dict]:
    return False, {"etag": "abc", "checked_at": time.time()}


@asset(inputs={"c": checked})
def consumer(c: dict) -> dict:
    return {"etag": c["etag"]}
```

Two runs in a row, with the etag unchanged, each print:

```
[barca] step:pipeline.py:checked completed 0.0s (1/2)
[barca] step:pipeline.py:consumer completed 0.0s (2/2)
[barca] 2/2 steps | done in 0.0s
```

Return the etag alone. The example also shows that the first element of the tuple is not used
for caching: the sensor returns `False` and its consumer re-runs anyway.

## What `--dry-run` and `barca status` show

Neither executes anything, so neither runs the sensor. They predict from the sensor's last
recorded value.

After `orders.csv` changes, and before any run, both still report the consumers as cached:

```bash
barca get total pipeline.py --dry-run
```

```
Dry run: barca get total (nothing executed, nothing written)

STATUS    WHY                                                                     STEP
will run  sensors always re-run                                                   pipeline.py:orders_version
cached    assumes sensor 'orders_version' returns the same value as its last run  pipeline.py:orders
cached    -                                                                       pipeline.py:total

1 will run, 2 cached, 0 unknown
```

The real run that follows executes all three steps. A dry run that includes a sensor is a
prediction, and the `WHY` column says what it assumed.

Running the sensor on its own records a new value without computing anything else. After that,
`barca status` and `--dry-run` report the consumers as stale:

```bash
barca get orders_version pipeline.py
barca status pipeline.py
```

```
NAME            KIND    STATE        WHY             LAST RUN                           SHAPE            DEPS
orders_version  sensor  always-runs  sensor          success 2026-10-07 18:13:43 0.01s  str              -
orders          asset   stale        changed         success 2026-10-07 18:13:29 0.00s  4 rows x 2 cols  orders_version
total           asset   stale        upstream_stale  success 2026-10-07 18:13:29 0.00s  dict (2 keys)    orders

0 cached, 2 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

A sensor that has never run has no recorded value. Its consumers, and everything downstream of
them, are `unknown`. This is what both commands show straight after the conversion above:

```
STATUS    WHY                                                                     STEP
will run  sensors always re-run                                                   pipeline.py:orders_version
unknown   reads sensor 'orders_version', which has no recorded output: its value is not known until it runs  pipeline.py:orders
unknown   depends on 'orders', whose inputs include a sensor with no recorded output  pipeline.py:total

1 will run, 0 cached, 2 unknown
```

In JSON the reason is `"sensor_output_unknown"`.

## Looking at the results

`barca sql` queries cached results by function name. A sensor's last value is a view too; a
sensor that returns a string or a number has one column, named `json`. With the three-row file
back in place, as above:

```
$ barca sql "select * from total"
rows  amount
3     60

$ barca sql "select * from orders_version"
json
aac441b2f011
```

`barca sql` shows the result that matches the sensor's current value. `barca status` reports
both assets as `cached` at this point, but its LAST RUN and SHAPE columns describe the most
recent materialization, which here is the four-row one (`4 rows x 2 cols` for `orders`).

## A scheduled sensor under `barca serve`

`@sensor(freshness=Schedule("<cron>"))` makes `barca serve` run the sensor on each tick. The tick
runs the sensor and records its value. It does not run the assets that read the sensor.

```python
@sensor(freshness=Schedule("*/5 * * * * *"))      # every 5 seconds, for the demonstration
def orders_version() -> tuple[bool, str]:
    return True, hashlib.sha256(Path("orders.csv").read_bytes()).hexdigest()[:12]
```

With `orders` and `total` unscheduled, the server log shows one step per tick, before and after
`orders.csv` changes:

```
[barca] scheduling 1 asset:
  pipeline.py:orders_version — */5 * * * * * (next 2026-10-07 14:14:00)
[barca] scheduled run pipeline.py:orders_version → 51ee21bc84b0
[barca] step:pipeline.py:orders_version completed 0.0s (1/1)
[barca] 1/1 steps | done in 0.0s
[barca] scheduled run pipeline.py:orders_version → 51f1182a1560
[barca] step:pipeline.py:orders_version completed 0.0s (1/1)
[barca] 1/1 steps | done in 0.0s
```

Afterwards `barca status` reports `orders` and `total` as `stale`; nothing recomputed them.

To have the server recompute when the data changes, put the schedule on the asset or task at the
end of the chain. Each tick of a scheduled asset runs the sensors upstream of it and then
computes only what has no cached result. [Scheduling](/scheduling/#example-one-scheduled-task-below-a-sensor)
shows the log of such a server.

Triggering downstream nodes from a sensor tick is proposed in RFC-0008
([PR #276](https://github.com/barca-orc/barca/pull/276)) and is not implemented.

## Rules and limits

- A sensor cannot have inputs. `@sensor(inputs={...})` is a usage error (exit 2):
  `DAG error: sensor 'pipeline.py:s' cannot have inputs`.
- Assets and tasks may read sensors. A task cannot be an input to a sensor or an asset.
- A sensor's default freshness is `Manual`. barca 0.18.0 accepts `@sensor(freshness=Always)`
  without an error. Only `Schedule(...)` changes what happens, and only under `barca serve`.
- A sensor is never served from cache. Keep it cheap: it runs on every command that includes it.
- Only assets that read the sensor directly have its value in their run hash. Assets further
  downstream change because their upstream's run hash changed.
- A partitioned asset that reads a sensor re-runs every key when the value changes
  (`barca docs cache`).
- One value is kept per sensor: each run overwrites the previous one. `barca history` lists the
  runs, not the values observed.
