---
title: "Workflow: Freshness-Driven Run and Tasks"
description: How Barca decides when assets, sensors, and tasks are eligible to run.
---

This document specifies how Barca decides when assets, sensors, and tasks are eligible to run.

The goal is to keep the orchestration model small:

- assets define the graph
- sensors bring external state into the graph
- `barca serve` continuously maintains each asset at its declared freshness level; `barca get`/`barca run` apply freshness for a single one-shot pass
- no pipeline DSL is required

This workflow assumes the Barca core constraints documented in [Core Constraints](/core-constraints/).

## Summary

Every asset, sensor, and task declares a `freshness` value that controls when Barca keeps it up to date.

```python
@asset()                                    # freshness=Always (default)
@asset(freshness=Manual)
@asset(freshness=Schedule("0 5 * * *"))

@sensor(freshness=Manual)                   # Always is not valid for sensors
@sensor(freshness=Schedule("*/5 * * * *"))

@task()                                     # freshness=Always (default)
@task(freshness=Manual)
@task(freshness=Schedule("0 6 * * *"))
```

Where:

- `freshness` controls how eagerly Barca keeps a stale node up to date
- staleness is computed from provenance
- `barca run` discovers stale nodes and materialises them when eligible

## Freshness kinds

### `Always`

The default for `@asset` and `@task`. Barca keeps this asset fresh automatically — any upstream change cascades through and re-materialises it during `barca run`.

**`Manual` freshness blocks downstream**: if any transitive upstream asset has `Manual` freshness, downstream `Always` assets cannot auto-update. They remain stale until the `Manual` upstream is explicitly refreshed.

### `Manual`

Barca never auto-updates this asset, even when stale. Only refreshed via explicit request. Useful for source data or pinned inputs that should not change without deliberate action.

### `Schedule("cron_expr")`

Barca brings this asset up to date on each cron tick: it runs only if there is no cached result for its current code and inputs (sensors upstream are polled first). Acceptable staleness between ticks.

```python
from barca import asset, Schedule

@asset(freshness=Schedule("0 5 * * *"))
def prices() -> dict[str, str]:
    return {"ok": "yes"}

@asset(inputs={"prices": prices}, freshness=Schedule("0 6 * * *"))
def daily_report(prices: dict[str, str]) -> dict[str, str]:
    return {"report": prices["ok"]}
```

Behavior:

- at 5:00, `prices` becomes eligible
- after `prices` updates, `daily_report` becomes stale
- before 6:00, `daily_report` is `stale_waiting_for_schedule`
- at 6:00, if upstream is settled, `daily_report` becomes `runnable_stale`

## Decorator APIs

```python
@asset(
    name: str | None = None,
    inputs: dict[str, AssetRefLike] | None = None,
    partitions: dict[str, PartitionSpecLike] | None = None,
    serializer: SerializerKind | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    description: str | None = None,
    tags: dict[str, str] | None = None,
)
```

```python
@sensor(
    name: str | None = None,
    freshness: Manual | Schedule = Manual,
    timeout_seconds: int = 300,
    description: str | None = None,
    tags: dict[str, str] | None = None,
)
```

```python
@task(
    name: str | None = None,
    inputs: dict[str, NodeRefLike] | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
)
```

## Why `Always` is the default

`Always` is the right default because most assets in a pipeline should stay up to date automatically. Users opt into `Manual` or `Schedule` when they need to control when updates happen. `Manual` assets are passive by design.

## Timeout policy

Assets, sensors, and tasks all support:

```python
timeout_seconds: int = 300
```

This timeout applies per attempt. If an attempt exceeds the timeout, Barca terminates the worker and marks the attempt as timed out.

## barca serve model

`barca get`/`barca run` are one-shot commands: each invocation parses the DAG, plans the subgraph for the target, and executes it once before exiting. Continuous freshness enforcement is `barca serve`'s job.

`barca serve` runs a long-running process that serves the DAG over HTTP (`GET /assets`, `GET /plan`, `POST /run`, ...) and drives a background cron scheduler. At startup the scheduler enumerates every node whose `freshness` is `Schedule(cron)`. On each live cron match (evaluated in the configured `--timezone`, local by default) it triggers that node through the `get` path (assets and sensors) or the `run` path (tasks), reusing cached upstream assets:

- scheduled assets/sensors go through the `get` path — cache-aware, so unrelated upstreams are reused
- scheduled tasks go through the `run` path — always re-run
- `Always` and `Manual` nodes are not independently polled; they materialise as a side effect of being upstream of whatever is triggered (a scheduled node, or an explicit `POST /run`/`POST /get/{target}` call)

If a job's previous run is still in flight when its next tick arrives, that tick is skipped for that job only — other jobs' ticks are unaffected. On startup, a job fires once immediately if a tick elapsed while the server was down (catch-up); last-fired times are persisted so catch-up survives restarts.

## State model

Recommended internal states:

- `fresh`
- `stale_waiting_for_schedule`
- `stale_waiting_for_upstream`
- `runnable_stale`
- `running`
- `failed`
- `historical`

## Sensors

Sensors are first-class ingress nodes for uncontrolled external state.

```python
from barca import sensor, asset, Schedule

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox_files() -> tuple[bool, list[str]]:
    return True, ["a.csv", "b.csv"]

@asset(inputs={"paths": inbox_files})
def ingest_paths(paths: tuple[bool, list[str]]) -> list[dict[str, str]]:
    ...
```

The full `(update_detected, output)` tuple is passed as input to downstream assets — downstream receives the complete tuple, not just the output payload.

Sensors return `(update_detected: bool, output)`. When `update_detected=True`, downstream assets become stale. When `update_detected=False`, downstream assets do not become stale (no meaningful change was detected).

## Tasks

Tasks handle side effects — sending email, writing to a database, calling an external API. Use `@task` for side-effect operations. Use `@sink` for writing asset outputs to file paths.

```python
from barca import asset, task, sensor, Schedule

@sensor(freshness=Schedule("0 5 * * *"))
def upstream_db_rows() -> tuple[bool, list[dict[str, str]]]:
    ...

@asset(inputs={"rows": upstream_db_rows})
def report_rows(rows: tuple[bool, list[dict[str, str]]]) -> list[dict[str, str]]:
    _, data = rows
    return data

@task(inputs={"rows": report_rows}, freshness=Schedule("0 6 * * *"))
def send_report(rows: list[dict[str, str]]) -> None:
    ...
```

Tasks must not be an input to an asset or sensor (a task always re-runs, so feeding its output into a cacheable node would keep that node perpetually stale).

## Sinks

`@sink` is a decorator stacked on `@asset` for writing outputs to file paths. Paths are fsspec-compatible (local, `s3://`, `gs://`, etc.).

```python
from barca import asset, sink, Always

@asset(freshness=Always)
@sink('./report.json')
@sink('s3://my-bucket/report.json')
def report() -> dict:
    return {"rows": 42}
```

When the parent asset materialises, all attached sinks write its output to their declared paths. Sink failures are non-blocking (leaf nodes) but surface prominently as failures in `barca list` and job logs.

## Partitioned assets and freshness

Freshness applies to partitioned assets per partition:

- staleness is determined per partition
- eligibility is determined per partition
- `barca run` may enqueue many runnable partitions at once

```python
@asset(
    partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])},
    freshness=Schedule("0 5 * * *"),
)
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker}
```

At 5:00, each stale partition becomes independently eligible.

## Acceptance criteria

- An asset with `freshness=Manual` is only refreshed when explicitly requested or when required by a targeted downstream refresh.
- An asset with `freshness=Always` becomes runnable immediately when stale and upstream-ready.
- A sensor with `freshness=Schedule(...)` records observations on its cron cadence.
- An asset with `freshness=Schedule(...)` becomes runnable only when its cron window opens.
- A downstream asset can be stale at 5:10 and still not runnable until its 6:00 schedule.
- Assets and tasks may consume sensors as inputs (receiving the full tuple).
- Tasks use the same `freshness` primitive as assets.
- Tasks record successful executions against current upstream provenance.
- Partitioned assets evaluate staleness and eligibility per partition.
- `Manual` freshness blocks downstream `Always` assets from auto-updating.
- `Always` is not valid for sensors.
