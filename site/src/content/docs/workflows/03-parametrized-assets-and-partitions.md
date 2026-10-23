---
title: "Workflow: Parametrized Assets and Partitions"
description: One asset definition run once per key, each key cached on its own. What runs when keys, code or a sensor change, how to look at the results, and the limits on 0.18.0.
---

A partitioned asset is one function that barca runs once per key. Each key is a step of its
own: it has its own run hash, its own artifact, and it is cached separately. Keys run in
parallel across worker processes.

Before checking a consumer's cache, barca verifies run hashes for every upstream
partition in the selected expanded plan across all worker chunks. Missing hashes
fail as an infrastructure error before a partial input set can produce a cache
hit. Valid hashes and cache decisions remain independent of worker pool size.

An earlier version of this page was a design document (identity model, proposed
`materialize()` and `list_partitions()` helpers, a `.barcafiles/` layout). This page
describes the shipped workflow. The examples were first verified with 0.18.0;
`barca docs partitions` is the current behavior reference.

## Example

Three assets and a sensor: `prices` runs once per ticker and reads a sensor, `signal` runs
once per ticker on that ticker's price, and `report` collects every `signal` into one list.

```python
# pipeline.py
from pathlib import Path

from barca import asset, collect, partitions, partitions_from, sensor


@sensor()
def feed_version() -> tuple[bool, str]:
    # Stands in for the etag of the price feed.
    return True, Path("feed_version.txt").read_text().strip()


@asset(inputs={"version": feed_version},
       partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def prices(ticker: str, version: str) -> dict:
    return {"ticker": ticker, "close": float(len(ticker) * 100 + ord(ticker[0])), "feed": version}


@asset(partitions={"ticker": partitions_from(prices)})
def signal(ticker: str, prices: dict) -> dict:
    return {"ticker": ticker, "buy": prices["close"] > 470}


@asset(inputs={"signals": collect(signal)})
def report(signals: list[dict]) -> dict:
    return {"tickers": len(signals), "buys": sorted(s["ticker"] for s in signals if s["buy"])}
```

- `partitions([...])` declares the keys. The key is passed as the parameter named in
  `partitions={...}` (`ticker`).
- `partitions_from(prices)` gives `signal` the same keys as `prices`. Each key receives that
  key's output of `prices`, as the parameter named `prices`.
- `collect(signal)` passes every key's output to `report` as one list.
- An unpartitioned input, here the sensor, is passed whole to every key.

```bash
echo v1 > feed_version.txt
barca list pipeline.py
barca get report pipeline.py
```

`barca list` shows one row per asset, not per key:

```
NAME                      KIND    FRESHNESS  DEPS
-------------------------------------------------
pipeline.py:feed_version  sensor  manual     -
pipeline.py:prices        asset   always     pipeline.py:feed_version
pipeline.py:signal        asset   always     pipeline.py:prices
pipeline.py:report        asset   always     pipeline.py:signal
```

The first run executes eight steps: the sensor, three keys of `prices`, three keys of
`signal`, and `report`.

```
[barca] 8/8 steps | done in 0.0s
Run 5251655353a8 | got 'report' in 0.139s (8 steps, 3 phases)

.barca/artifacts/pipeline.py--report/....json
  {
    "buys": [
      "GOOG",
      "MSFT"
    ],
    "tickers": 3
  }
```

The second run executes one step, the sensor. It returned the same value, so every key and
the report are served from cache:

```
[barca] 1/8 steps | done in 0.0s
Run 52517c66a848 | got 'report' in 0.047s (1 step, 3 phases)
```

## Looking at the results

`barca sql` exposes a partitioned asset as one view over every key, with a `partition`
column:

```bash
barca sql "select * from prices order by ticker"
barca sql "select partition, buy from signal where buy"
```

```
partition    ticker  close  feed
ticker=AAPL  AAPL    465.0  v1
ticker=GOOG  GOOG    471.0  v1
ticker=MSFT  MSFT    477.0  v1
```

```
partition    buy
ticker=GOOG  true
ticker=MSFT  true
```

`barca status` shows one row per asset. A partitioned asset is `cached` when every key is,
and `partial` when some are (see "When a key fails" below). With `--json` the node carries
counts: `"partitions": {"cached": 3, "missing": 0, "missing_keys": [], "total": 3}`.

```
NAME          KIND    STATE        WHY           LAST RUN                               SHAPE          DEPS
feed_version  sensor  always-runs  sensor        success 2026-10-21 21:07:49 UTC 0.00s  str            -
prices        asset   cached       materialized  success 2026-10-21 21:07:49 UTC 0.00s  dict (3 keys)  feed_version
signal        asset   cached       materialized  success 2026-10-21 21:07:49 UTC 0.00s  dict (2 keys)  prices
report        asset   cached       materialized  success 2026-10-21 21:07:49 UTC 0.00s  dict (2 keys)  signal
```

SHAPE describes one key's artifact (a dict with three fields), not the number of partitions.

Each key has its own artifact directory, named `<file>--<asset>_<dimension>_<key>`:

```
.barca/artifacts/pipeline.py--prices_ticker_AAPL/607006b4….json
.barca/artifacts/pipeline.py--prices_ticker_GOOG/a713b4f3….json
.barca/artifacts/pipeline.py--prices_ticker_MSFT/e403d2f9….json
.barca/artifacts/pipeline.py--signal_ticker_AAPL/c0260fbd….json
...
.barca/artifacts/pipeline.py--report/a7cdcfcb….json
```

You do not need to read these by hand; `barca sql` reads them.

## A sensor change re-runs every key

Change what the sensor returns and every key of `prices` runs again, then every key of
`signal`, then `report`:

```bash
echo v2 > feed_version.txt
barca get report pipeline.py --agent
```

```
[barca] step:pipeline.py:feed_version completed 0.0s (1/8)
[barca] step:pipeline.py:prices[ticker=AAPL] completed 0.0s (2/8)
[barca] step:pipeline.py:prices[ticker=MSFT] completed 0.0s (3/8)
[barca] step:pipeline.py:prices[ticker=GOOG] completed 0.0s (4/8)
[barca] step:pipeline.py:signal[ticker=AAPL] completed 0.0s (5/8)
[barca] step:pipeline.py:signal[ticker=MSFT] completed 0.0s (6/8)
[barca] step:pipeline.py:signal[ticker=GOOG] completed 0.0s (7/8)
[barca] step:pipeline.py:report completed 0.0s (8/8)
[barca] 8/8 steps | done in 0.0s
```

A sensor's value is one value for the whole asset. There is no per-key sensor: if only one
ticker's data changed, all keys still run. The same is true of any unpartitioned input.

`--dry-run` does not run the sensor. Before the run above it predicted from the sensor's last
recorded value and reported everything as cached:

```
STATUS    WHY                                                                     STEP
will run  sensors always re-run                                                   pipeline.py:feed_version
cached    3 keys; assumes sensor 'feed_version' returns the same value as its last run  pipeline.py:prices
cached    3 keys cached                                                           pipeline.py:signal
cached    -                                                                       pipeline.py:report

1 will run, 7 cached, 0 unknown
```

More on sensors: [Sensors and External Observations](/workflows/06-sensors-and-external-observations/).

## Adding and removing keys

Adding a key runs the new key, and no other key of that asset, however the keys are written.
Removing a key or reordering them runs no key. The keys are not part of the function's
definition, so editing them never changes the run hash of a key that is already cached
(`barca docs cache`, "Which decorator arguments count").

This section was run with 0.19. Up to 0.18 it held only when the keys were written outside
the decorator: adding a key to a literal list inside it re-ran every key of the asset and
everything downstream, because the decorator's text was part of every key's hash
([#283](https://github.com/barca-orc/barca/issues/283)).

### A literal list in the decorator

Add `"NVDA"` to the list in the example above:

```python
partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG", "NVDA"])}
```

```
$ barca get report pipeline.py --dry-run --pretty
...
STATUS    WHY                                        STEP
will run  sensors always re-run                      pipeline.py:feed_version
partial   3 of 4 keys cached; will run: ticker=NVDA  pipeline.py:prices
partial   3 of 4 keys cached; will run: ticker=NVDA  pipeline.py:signal
will run  changed: input pipeline.py:signal          pipeline.py:report

4 will run, 6 cached, 0 unknown
```

```
$ barca get report pipeline.py --agent
[barca] step:pipeline.py:feed_version completed 0.0s (1/10)
[barca] step:pipeline.py:prices[ticker=NVDA] completed 0.0s (2/10)
[barca] step:pipeline.py:signal[ticker=NVDA] completed 0.0s (3/10)
[barca] step:pipeline.py:report completed 0.0s (4/10)
[barca] 4/10 steps | done in 0.0s
```

The sensor always runs. `prices` and `signal` ran for `NVDA` only, and `report` ran because
the list it collects has one more entry. After removing `"MSFT"` from the list, the sensor
and `report` ran (`2/8 steps`) and no key did. Adding `description="Closing prices"` to the
same decorator ran only the sensor (`1/8 steps`).

### A module-level constant

```python
REGIONS = ["emea", "amer", "apac"]


@asset(partitions={"region": partitions(REGIONS)})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 10}


@asset(inputs={"all_sales": collect(sales)})
def summary(all_sales: list[dict]) -> dict:
    return {"regions": len(all_sales), "total": sum(s["revenue"] for s in all_sales)}
```

After adding `"latam"` to `REGIONS`:

```
[barca] step:pipeline.py:sales[region=latam] completed 0.0s (1/5)
[barca] step:pipeline.py:summary completed 0.0s (2/5)
[barca] 2/5 steps | done in 0.0s
```

After removing `"amer"` from `REGIONS`, only `summary` ran (`1/4 steps`).

Any expression that is not a literal list (a name, a comprehension, a function call) is
evaluated by Python when barca plans the run.

### Keys from an upstream asset

`partitions_from(upstream)` on an unpartitioned asset that returns a list uses the list's
values as keys. Here the list comes from a file, through a sensor:

```python
@sensor()
def region_list() -> tuple[bool, list]:
    return True, Path("regions.txt").read_text().split()


@asset(inputs={"names": region_list})
def regions(names: list) -> list:
    return names


@asset(partitions={"region": partitions_from(regions)})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 10}
```

After appending `latam` to `regions.txt`:

```
[barca] step:pipeline.py:region_list completed 0.0s (1/4)
[barca] step:pipeline.py:regions completed 0.0s (2/4)
[barca] step:pipeline.py:sales[region=latam] completed 0.0s (3/7)
[barca] step:pipeline.py:summary completed 0.0s (4/7)
[barca] 4/7 steps | done in 0.0s
```

After removing `amer` from the file, `regions` and `summary` ran and no key of `sales` did.

When the source returns a **dict** instead of a list, its keys are the keys and each key is
called with its own value; a key runs again only when its value changes. One sensor query
that returns `{retailer: max(updated_at)}` then runs only the retailers with new data, out of
any number: see [Only What Changed, Across Many Partitions](/patterns/09-changed-partitions/).

The keys are known only after `regions` has run. Before its first run, `--dry-run` and
`barca status` report `sales` and everything below it as `unknown` ("partition keys come from
the output of 'regions', which is not available until it runs"). The list itself is not
passed to `sales`.

### A removed key stays in `barca sql`

A removed key's artifact is not deleted, and the `barca sql` view still includes it. After
removing `amer` (in both of the last two examples):

```
$ barca sql "select * from sales"
partition     region  revenue
region=amer   amer    40
region=apac   apac    40
region=emea   emea    40
region=latam  latam   50
```

`summary`, which uses `collect(sales)`, received the three current keys
(`{'regions': 3, 'total': 130}`). To query only current keys, filter on `partition` yourself
or query an asset built with `collect`.

## Selecting keys with `--partition`

A key cannot be named as a target or in `--refresh`; select keys with `--partition` instead.
The selection applies to the whole run, upstream and downstream:

```
$ barca get 'sales[region=emea]' pipeline.py
Asset 'sales[region=emea]' not found. Available: pipeline.py:sales, pipeline.py:summary

$ barca get summary pipeline.py --refresh 'sales[region=emea]'
--refresh: no upstream asset named 'sales[region=emea]' in the cone of 'summary'.
```

- `barca get sales pipeline.py` brings every key up to date.
- `--refresh sales` recomputes every key, and everything downstream; with
  `--partition region=emea` on a `sales` target, only that key.
- `barca get sales pipeline.py` returns every key: one artifact path per key, and in JSON a
  `final_output` object keyed by `region=...` (past 1000 keys, only the keys the run
  computed, with a count of the rest). The Python call `barca.get("sales", "pipeline.py")`
  returns a dict of key to value. `--partition region=emea` (or `partition="region=emea"` in
  Python) computes and returns only that key, and scopes `--refresh sales` to it.
- `barca get margin pipeline.py --partition region=emea` computes only the emea key of `sales`
  too, and `barca get pipeline.py --partition region=emea` (no target) every asset for emea.
  `summary`, a `collect(sales)` fan-in, is skipped with a note on stderr: over one key it would be
  a different result. Run without `--partition` to update it. A key computed under a selection
  is the key a full run computes, so the next full run serves it from cache.

## When a key fails

Keys are independent. With one key raising, the others complete and are cached, the fan-in
does not run, and the command exits 1:

```
[barca] step:pipeline.py:sales[region=emea] completed 0.0s (1/4)
[barca] step:pipeline.py:sales[region=apac] completed 0.0s (2/4)
[barca] step:pipeline.py:sales[region=amer] failed: RuntimeError: amer feed not ready
[barca] 2/4 steps | failed in 0.1s
```

```
$ barca status pipeline.py
NAME     KIND   STATE      WHY                                     LAST RUN                        SHAPE  DEPS
sales    asset  partial    2 of 3 keys cached; partitions_missing  failed 2026-10-21 21:07:49 UTC  -      -
summary  asset  never-run  no_record                               -                               -      sales
```

The next run executes only the failed key and the fan-in (`2/4 steps`).

## With a remote store

With `BARCA_REMOTE_URI` set, every key's result is uploaded to the store, one directory per
key, laid out as it is locally. The store is this machine's backup and read-through cache: with
the local copies of both keys of a `sales` deleted, the next `barca get sales` ran no step and
printed `[barca] fetched 2 cached artifacts`. Another machine reuses those keys only through a
server they both point at (`barca docs server-mode`).

## A few thousand keys

Measured once with barca 0.18.0 on an Apple M4 Max (16 cores) that was busy with other work
(load average about 9), with `KEYS = [f"k{i:04d}" for i in range(2000)]`, a function that
returns a small dict, and a `collect` fan-in. Wall-clock time of the whole command:

| Command | Steps run | Time |
|---|---|---|
| `barca get total pipeline.py`, first run | 2,001 | 1.9 s |
| `barca get total pipeline.py`, second run | 0 | 0.14 s |
| `barca status pipeline.py` | - | 0.13 s |
| `barca sql "select count(*), sum(n) from item"` | - | 0.8 s |
| `barca get total pipeline.py` after adding one key | 2 | 0.34 s |

`.barca/` was 14 MB afterwards. These are times for trivial steps; they show what barca adds
per key, not what your functions cost.

## Limits

- **A key is selected with `--partition`, never named** (above): `sales[region=emea]` is not a
  target, and `--refresh` of an upstream asset addresses all its keys.
- **Up to 0.18, editing a literal key list re-ran every key** (above). From 0.19 a new key
  runs alone wherever the keys are written, and editing `description=`, `tags=` or `retries=`
  on a partitioned asset re-runs nothing. Changing `serializer=`, `inputs=`, a `@sink` or the
  dimension name re-runs every key.
- **Any change to the function's code, or to an unpartitioned input or sensor it reads,
  re-runs every key.**
- **A removed key's results remain** on disk (until the cache size limit evicts them) and in the
  `barca sql` view.
- **`partitions_from(upstream)` on a partitioned upstream** must be the asset's only dimension
  and must keep the upstream's dimension name; anything else is a usage error (exit 2).
- **A partitioned asset in a plain `inputs=`** of an unpartitioned asset, without `collect()`,
  is a usage error (exit 2).
- **`--partition` skips fan-ins.** A `collect()` over a narrowed asset, and what is below it, is
  not run under a selection (reason `partition_selection`); a partitioned asset on another
  dimension keeps every key. Up to 0.29.2 `--partition` narrowed only the target.
- With `@sink("out.parquet")`, each key writes its own file; with a path ending in `/`,
  `@sink("out/")` writes one Hive-partitioned dataset with a `retailer=r1/` directory per key:
  `barca docs sinks`.
- To fan work out at run time from inside a task, without caching, see
  [Parallel Tasks](/patterns/04-parallel-tasks/).

Mixing `partitions()` and `partitions_from()` dimensions on one asset is unsupported
and returns exit 2 before execution. Use only derived dimensions or declare every
dimension explicitly with `partitions([...])`. `barca plan` uses the execution pool
size (available cores or `BARCA_POOL_SIZE`). Warnings sort by node, parameter and kind;
partition key previews list the first twenty keys in lexical order.
