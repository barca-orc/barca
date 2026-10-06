"""Basic barca example — demonstrates every major feature.

Showcases:

- Bare ``@asset`` (default ``freshness=Always()``)
- ``@asset(inputs=...)`` with upstream dependencies
- ``freshness=Manual()`` for on-demand assets
- ``freshness=Schedule("*/5 * * * *")`` for cron-driven assets
- ``@asset @sink(path)`` for writing outputs to files via fsspec
- ``@sensor(freshness=Schedule(...))`` for observing external state
- ``@task`` for workflow-management steps that always re-run (never cached)
- ``@task(inputs=...)`` for a task that consumes an upstream asset
- ``@task(inputs={"_dep": dep})`` for ordering-only task chains (no data passed)
- ``@asset(partitions=...)`` with static partitions
- ``partitions_from(upstream)`` — a downstream asset reuses an upstream's partition keys
- ``collect(asset)`` for aggregating all partitions of an upstream
"""

import time

from barca import (
    Always,
    Manual,
    Schedule,
    asset,
    collect,
    partitions,
    partitions_from,
    sensor,
    sink,
    task,
)

# ---------------------------------------------------------------------------
# Workflow 1: Single assets, no inputs
# ---------------------------------------------------------------------------


@asset
def bare_asset() -> dict:
    """Bare @asset — default freshness is Always."""
    return {"bare": True}


@asset()
def hello_world() -> dict:
    return {"message": "Hello from barca!"}


@asset()
def greeting() -> str:
    return "Hello from Barca!"


@asset(freshness=Manual())
def manual_only() -> dict:
    """Never fired by the scheduler. Recompute it with
    `barca get manual_only example_project/assets.py --refresh manual_only`."""
    return {"manual": True, "ran_at": time.time()}


@asset(freshness=Schedule("0 */6 * * *"))
def six_hourly() -> dict:
    """Runs every 6 hours while `barca serve` is running (the scheduler fires it)."""
    return {"schedule": "6h", "ts": time.time()}


# ---------------------------------------------------------------------------
# Workflow 2: Asset with upstream inputs
# ---------------------------------------------------------------------------


@asset()
def fruit() -> str:
    return "banana"


@asset(inputs={"fruit": fruit})
def uppercased(fruit: str) -> str:
    return fruit.upper()


# ---------------------------------------------------------------------------
# Workflow 3: Sinks — stack @sink decorators on an @asset
# ---------------------------------------------------------------------------


@asset()
@sink("tmp/greeting.json", serializer="json")
@sink("tmp/greeting.pkl", serializer="pickle")
def greeting_for_world() -> dict:
    """Every materialisation writes to both sinks via fsspec."""
    return {"hi": "world", "lang": "en"}


# ---------------------------------------------------------------------------
# Workflow 4: Partitioned assets, partitions_from and collect
# ---------------------------------------------------------------------------


@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def fetch_prices(ticker: str) -> dict:
    return {"ticker": ticker, "price": len(ticker) * 100}


# This downstream reuses fetch_prices' 3 partition keys — runs 1:1, and each key
# receives that key's fetch_prices output as the parameter named after it.
@asset(partitions={"ticker": partitions_from(fetch_prices)})
def normalised_price(ticker: str, fetch_prices: dict) -> dict:
    return {"ticker": ticker, "normalized": fetch_prices["price"] / 100.0}


# This downstream uses collect() to consume ALL partitions at once.
@asset(inputs={"prices": collect(fetch_prices)})
def price_summary(prices: list[dict]) -> dict:
    """``prices`` is a list with one entry per partition of ``fetch_prices``."""
    total = sum(p["price"] for p in prices)
    return {"tickers": sorted(p["ticker"] for p in prices), "total": total}


# ---------------------------------------------------------------------------
# Workflow 5: Large partition set
# ---------------------------------------------------------------------------


@asset(partitions={"key": partitions([f"p{i:05d}" for i in range(10000)])})
def wide_asset(key: str) -> dict:
    return {"key": key, "index": int(key[1:])}


# ---------------------------------------------------------------------------
# Workflow 6: Sensors and tasks
# ---------------------------------------------------------------------------


@sensor(freshness=Schedule("*/5 * * * *"))
def heartbeat_sensor():
    """Fires every 5 minutes; always reports an update."""
    return (True, {"ts": time.time(), "healthy": True})


@asset(inputs={"tick": heartbeat_sensor}, freshness=Always())
def last_heartbeat_seen(tick):
    """A consumer receives the sensor's output (the second element of its tuple).

    The output is part of this asset's cache key, so a new ``ts`` re-runs it.
    """
    return {"healthy": tick["healthy"], "last_ts": tick["ts"]}


# A task that consumes an upstream *asset*. Tasks always re-run and are never
# cached — perfect for "do something with the result" side effects.
@task(inputs={"summary": price_summary})
def log_summary(summary):
    """Runs as a side effect whenever we run it; consumes the price_summary asset."""
    print(f"[barca task] price summary: {summary}")


# ---------------------------------------------------------------------------
# Workflow 7: Ordering-only task chain (no data passed) via `_` prefix
#
#   migrate → warm_cache → notify
#
# Run the whole chain with:  barca run notify example_project/assets.py
# ---------------------------------------------------------------------------


@task()
def migrate():
    """Run a database migration."""
    print("[barca task] running migration")


@task(inputs={"_migrate": migrate})
def warm_cache(_migrate):
    """Warm caches — only after the migration has run."""
    print("[barca task] warming cache")


@task(inputs={"_warm_cache": warm_cache})
def notify(_warm_cache):
    """Notify the team — only after the cache is warm."""
    print("[barca task] migration + cache warm complete")
