---
title: "Pattern: Asset-to-Asset"
description: Chain assets with inputs=. Each step is cached and runs again only when its code or an upstream changes.
---

Use this when one computed value feeds another and you want each step cached on its own.
An asset names its upstream assets in `inputs=`; barca runs them in order and passes each
result to the parameter of the same name.

## Example

```python
from barca import asset


@asset()
def raw_orders() -> list:
    return [{"id": 1, "amount": 120.0}, {"id": 2, "amount": None}, {"id": 3, "amount": 45.5}]


@asset(inputs={"orders": raw_orders})
def cleaned(orders: list) -> list:
    return [o for o in orders if o["amount"] is not None]


@asset(inputs={"orders": cleaned})
def summary(orders: list) -> dict:
    return {"count": len(orders), "total": sum(o["amount"] for o in orders)}
```

Save it as `pipeline.py` and ask for the last asset. The target comes first, then the file:

```bash
barca get summary pipeline.py
```

## What barca does

The first run executes all three steps. Progress goes to stderr and the result to stdout
(a summary in a terminal, one JSON object when piped or with `--json`):

```
[barca] 3/3 steps | done in 0.0s
Run 51dc7bf7a540 | got 'summary' in 0.058s (3 steps, 1 phase)

Value:
{
  "count": 2,
  "total": 165.5
}
```

The second run executes nothing. Every step has a run hash, computed from its code and its
inputs' run hashes, and a result already stored under that hash is reused:

```
Run 51dc748e4ce8 | got 'summary' in 0.003s (0 steps, 1 phase)
```

Edit the body of `summary` and only `summary` runs again (`1/3 steps`). Edit `raw_orders` and
all three run, because each downstream hash includes its upstream's.

To see the state of each step without running anything, and to look at a stored result:

```bash
barca status pipeline.py
barca sql "select * from cleaned"
```

```
NAME        KIND   STATE   WHY           LAST RUN                           SHAPE            DEPS
raw_orders  asset  cached  materialized  success 2026-10-07 18:12:41 0.00s  3 rows x 2 cols  -
cleaned     asset  cached  materialized  success 2026-10-07 18:12:41 0.00s  2 rows x 2 cols  raw_orders
summary     asset  cached  materialized  success 2026-10-07 18:12:41 0.00s  dict (2 keys)    cleaned

3 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 0 always run
```

```
id  amount
1   120.0
3   45.5
```

Results are files under `.barca/artifacts/<node>/<run_hash>.json` (or `.parquet`, `.pkl`).
`barca sql` and `barca status` read them for you. Git already ignores `.barca/`.

## Limits

- **The run hash covers code and inputs, not the outside world.** An asset that reads a file,
  a bucket or a database in its own body is computed once and then served from cache, whatever
  happens to that data. Put a sensor in front of it:
  [Sensors and outside data](/workflows/06-sensors-and-external-observations/).
- **A dependency must be declared in `inputs=`.** Calling another asset's function from the
  body is an ordinary Python call. It works, but barca does not list it as a dependency
  (`barca list` shows `-` under DEPS), does not cache the called function's result on its own,
  and runs it inside the caller every time the caller runs. Editing the called function still
  re-runs the caller, because functions a step uses are part of its hash.
- **Every result is written to a file between steps.** Return data, not handles (an open
  connection or a generator cannot be stored). Formats: `barca docs types`.
- **Do not edit an input in place.** A step receives its own copy of a cached value, with some
  gaps; see [Anti-Patterns](/patterns/07-anti-patterns/#mutating-asset-inputs-in-place).
- A large parquet input is read whole unless its parameter is annotated as lazy:
  [Large Inputs](/patterns/08-large-inputs/).
