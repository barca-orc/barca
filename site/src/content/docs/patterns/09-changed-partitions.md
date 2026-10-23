---
title: "Pattern: Only What Changed, Across Many Partitions"
description: One sensor query returns every key and its version; partitions_from over that dict runs only the keys whose version changed, through a chain of partitioned assets.
---

Use this when an asset is split into many keys (thousands to hundreds of thousands of
retailers, customers, devices) and on each poll only a few of them have new data.

A sensor that returns a **dict** of key -> version decides both which keys exist and which
changed. `partitions_from(sensor)` makes one key per dict key; each key receives its own
value and folds only that value into its run hash, so a key whose version did not change is
served from cache, and so is everything `partitions_from` chains below it.

## Example

```python
import duckdb
from barca import Schedule, asset, partitions_from, sensor


@sensor(freshness=Schedule("*/5 * * * *"))
def retailer_versions() -> tuple[bool, dict]:
    rows = duckdb.connect("warehouse.duckdb", read_only=True).execute(
        "select retailer, max(updated_at)::varchar from orders group by retailer"
    ).fetchall()
    return True, dict(rows)                       # {"r000001": "2026-10-22 09:14:03", ...}


@asset(partitions={"retailer": partitions_from(retailer_versions)})
def cleaned(retailer: str, retailer_versions: str) -> dict:
    # retailer_versions is this retailer's value only
    orders = duckdb.connect("warehouse.duckdb", read_only=True).execute(
        "select * from orders where retailer = ?", [retailer]
    ).df()
    return {"retailer": retailer, "rows": len(orders), "as_of": retailer_versions}


@asset(partitions={"retailer": partitions_from(cleaned)})
def scored(retailer: str, cleaned: dict) -> dict:
    return {**cleaned, "score": cleaned["rows"] * 0.1}
```

```bash
barca get scored pipeline.py               # first run: every retailer
barca get scored pipeline.py --dry-run     # which retailers would run, and why
barca serve pipeline.py                    # every 5 minutes: the sensor, then what changed
```

A tick where 1 of 10,000 retailers changed runs three steps (the sensor and the retailer's
key of each asset) and serves the other 19,998 keys from cache; `barca history` records
`STEPS 3, CACHED 19998`. The dry run names the key and the reason:

```
partial   9999 of 10000 keys cached; will run: retailer=r000010 (value pipeline.py:versions changed)  pipeline.py:cleaned
```

## What to know

- The dict's keys are strings or integers (an integer becomes its string). Its values must be
  JSON-writable; a value that is not fails the sensor step with an error naming the key.
- A key's value is hashed as canonical JSON (keys sorted at every level), so the order the
  query returns rows in does not matter.
- Return a version per key (a max timestamp, an etag, a row count), not the data: the dict is
  written once per poll and read by each worker that runs a key.
- A new key runs that key; a removed key runs nothing. A `collect(scored)` downstream runs
  when any key is added, removed or recomputed.
- Editing the sensor's code re-runs every key, as for anything that reads a sensor.
- The alternative, a sensor partitioned by retailer
  (`@sensor(partitions={"retailer": partitions(RETAILERS)})`), runs once per key on every
  tick: 10,002 steps per tick at 10,000 keys instead of 3.
- Measured costs at 10k, 50k and 200k keys are in `benchmarks/RESULTS.md` ("Keyed
  partitions"); the per-tick cost grows with the key count even when one key changed.

The manual has the details: `barca docs partitions`, "Updating only what changed, across many
partitions".
