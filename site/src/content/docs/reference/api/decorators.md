---
title: Decorators API
description: Reference for @asset, @sink, @sensor, @task, @unsafe, Schedule, partitions, and parallel().
---

Core decorators for defining assets, sensors, tasks, sinks, and related primitives.

## @asset

```python
@asset(
    name: str | None = None,
    inputs: dict[str, AssetRefLike] | None = None,
    partitions: dict[str, PartitionSpecLike] | None = None,
    serializer: SerializerKind | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
)
```

Declares a cacheable, provenance-tracked asset. The default freshness is `Always` — the asset is kept up to date automatically during `barca run`.

`retries` is the total number of attempts on failure (1 = no retry). `retry_backoff` is the base
delay in seconds between attempts (delay grows linearly: `retry_backoff * attempt`).

`env` declares the environment variables the function reads. See
[Declared environment variables](#declared-environment-variables-env) below.

```python
from barca import asset, Always, Manual, Schedule

@asset()                                    # freshness=Always (default)
def my_asset() -> dict:
    return {"x": 1}

@asset(freshness=Manual)                    # only via explicit refresh
def pinned_data() -> dict:
    return {"x": 1}

@asset(freshness=Schedule("0 5 * * *"))     # daily at 05:00
def daily_report() -> dict:
    return {"x": 1}
```

`Manual` freshness blocks downstream `Always` assets from auto-updating — a downstream asset cannot be fresher than its most-upstream `Manual` dependency.

### Type annotations and materialization

Parameter and return type hints are optional and do not change the decorator API. When present,
they tell barca which parquet **reader** or **writer** to use on step boundaries — for example
`orders: pl.DataFrame` deserializes with polars instead of defaulting to pandas.

**Every asset output is still fully materialized** to an artifact file at the end of the step.
Barca does not keep lazy polars `LazyFrame`s or duckdb relations alive across workers; the
artifact on disk is the cache checkpoint. On the reading side, a `pl.LazyFrame` or
`duckdb.DuckDBPyRelation` input reads nothing up front: the step's query decides which columns
and row groups are read, from a remote artifact store too (only those byte ranges are
fetched). If you materialize an asset, you get a durable,
content-addressed file that any downstream step (or machine) can hit.

To run one efficient computation and cache multiple results, define multiple `@asset` functions
that share helpers, or compute everything you need inside a single step before returning.

Supported annotation shapes (statically parsed, no import):

| Annotation | Parquet role |
|---|---|
| *(none)* | pandas reader (default) |
| `pd.DataFrame` / `pandas.DataFrame` | pandas |
| `pl.DataFrame` / `polars.DataFrame` | polars |
| `pl.LazyFrame` | polars, lazy (a `LazyFrame` scanning the parquet file on read; collected on write) |
| `pyarrow.Table` | pyarrow (written with `pyarrow.parquet`) |
| `duckdb.DuckDBPyRelation` | duckdb (relation on read; materialized to parquet on write) |

### Declared environment variables (`env=`)

```python
import os
from barca import asset

@asset(env=["SOURCE_CSV", "API_TOKEN"])
def raw() -> dict:
    return {"source": os.environ.get("SOURCE_CSV", "default.csv")}
```

`env=` must be a literal list of string literals; barca reads it statically (a variable, tuple or
computed name is a parse error). When it plans a run, barca reads each declared variable from its
own environment (the workers inherit the same environment) and folds the name and value into
the step's run hash:

- Changing a declared variable re-materializes the asset and everything downstream of it. An
  unset variable is its own value, distinct from an empty string.
- Each step's entry in the JSON result carries `"env": {"API_TOKEN": null, "SOURCE_CSV": "b.csv"}`
  (`null` = unset), and `--agent` step lines end with `env API_TOKEN=<unset> SOURCE_CSV=b.csv`.
- Names ending in `_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD` (any case, or the bare word) are
  hashed but shown as `<redacted>` in every output.
- `barca list` shows declared names in an ENV column, and as `env` in `--json`.
- Nodes that declare no `env` hash exactly as before, so existing caches stay valid.

`env=` is also accepted on `@task` and `@sensor`. Those always run, so there it only records the
values each run used.

**Limitation:** environment variables your code reads without declaring them are invisible to
barca. They are not part of the cache key, so changing one does not invalidate anything.

## Partitions

```python
partitions(values: list[str | int])          # static partition values
partitions_from(source: AssetLike)            # derive partitions from an upstream asset
collect(source: AssetLike)                    # fan-in: aggregate all partitions of an upstream asset
asset_ref(canonical_name: str)                # reference a node by id without importing it
```

Use `partitions=` on `@asset` to split an asset's work across a set of keys, executed as
independent steps:

```python
from barca import asset, partitions, partitions_from, collect

@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def price(ticker: str) -> dict:
    return fetch_price(ticker)

@asset(partitions={"ticker": partitions_from(price)})   # same partition keys as `price`
def signal(ticker: str, price: dict) -> dict:
    return compute_signal(price)

@asset(inputs={"prices": collect(price)})                # fan-in: all partitions as a list
def summary(prices: list[dict]) -> dict:
    return aggregate(prices)
```

`partitions(...)` accepts a literal list (extracted statically at parse time) or any other Python
expression — e.g. a list comprehension or function call — which is evaluated by the Python runtime
at plan time. `partitions_from(price)` on a partitioned `price` gives the asset the same keys
(under the same dimension name, which must be its only dimension), and calls each key with the key
and that key's output of `price`, as the parameter named after it: `signal(ticker="AAPL",
price=<the AAPL output of price>)`. List the upstream in `inputs=` as well to receive it under
another name (`inputs={"p": price}`). Each consumer key depends only on its own upstream key, so a
new key of `price` runs only that key of `signal`. `partitions_from(tickers)` on an *unpartitioned*
asset that returns a list uses the list's values as keys, known once `tickers` has run; the list is
not passed to the function. `collect(...)`, used inside `inputs=`, aggregates every partition of an
upstream asset into a single list delivered to the parameter. A partitioned asset in an
unpartitioned asset's `inputs=` without `collect()` is a usage error (exit 2) that names both
`collect(price)` and `partitions_from(price)`; up to 0.11 it silently behaved like `collect()`. An
unpartitioned asset in a partitioned asset's `inputs=` is delivered whole to every key; it runs
once, before any key, and its run hash is part of every key's run hash, so changing it (or
`--refresh` on it) re-runs every key.

Cross-file inputs are ordinary imports: `from other_module.assets import raw_data`, then
`inputs={"data": raw_data}`. Barca resolves the import statically to that file's node (see
[Discovery](/reference/discovery/)). `asset_ref("path/to/file.py:function_name")`, used inside
`inputs=`, references a node by its canonical id (root-relative file path + function name, or its
explicit `name=`) without importing it, for example to avoid an import cycle:

```python
from barca import asset, asset_ref

@asset(inputs={"data": asset_ref("other_module/assets.py:raw_data")})
def process(data: dict) -> dict:
    return data
```

## @sink

```python
@sink(
    path: str,
    serializer: str | None = None,
)
```

Stacked on an `@asset` to write the asset's output to a path when it materialises. Paths are fsspec-compatible (local, `abfss://`, `s3://`, `gs://`, etc. — remote schemes need the matching extra, see [Remote storage](/reference/remote-storage/)). Multiple `@sink` decorators may be stacked on the same asset.

```python
from barca import asset, sink, Always

@asset(freshness=Always)
@sink('./output.json')
@sink('abfss://exports@myacct.dfs.core.windows.net/output.parquet', serializer='parquet')
def banana() -> dict:
    return {'a': 1}
```

The serialization format for each sink is chosen by precedence: the `serializer=` kwarg (`json`, `pickle`, `parquet`) → the sink path's extension (`.json`, `.pkl`, `.pickle`, `.parquet`) → the parent asset's artifact format. Writes are staged through a local temp file and uploaded/renamed atomically, so a crash never leaves a partial file at the destination.

Sinks are leaf nodes — no other asset may list a sink as an input. A sink failure does not fail the parent asset, but is surfaced prominently in logs (`[barca] SINK FAILED: ...`).

For partitioned assets, each partition writes its own sink file with the partition key injected before the extension: `@sink('out.parquet')` on partitions `ticker=AAPL, ticker=MSFT` produces `out_ticker_AAPL.parquet` and `out_ticker_MSFT.parquet`.

## @sensor

```python
@sensor(
    name: str | None = None,
    freshness: Manual | Schedule = Manual,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
)
```

Declares an external-state observer. Sensors must use `Manual` or `Schedule` freshness — `Always` is not valid for sensors (polling frequency must be declared explicitly). See `@asset` above for `env`, `retries` and `retry_backoff` semantics.

Sensors return `(update_detected: bool, output)` tuples. The worker unpacks the tuple: a downstream asset receives `output` only. `update_detected` is not used for caching.

A sensor's returned value is part of the run hash of every asset that reads it: when the value
changes, those assets (and everything downstream of them) re-run; when it is the same, they are
served from cache. That makes a sensor the way to track external data that changes in place, for
example a sensor that returns a blob's etag in front of the asset that reads the blob. Return only
what identifies the data: a value that changes on every run (a timestamp) re-runs the sensor's
consumers every time. `--dry-run` and `barca status` assume a sensor returns its last recorded
value, and report its consumers as `unknown` before it has ever run. See `barca docs cache`,
"External data that changes in place".

```python
from barca import sensor, Schedule

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox_files() -> tuple[bool, list[str]]:
    files = list(Path("inbox").glob("*.csv"))
    return len(files) > 0, [str(f) for f in files]
```

Sensors are source nodes only — they have no upstream inputs.

## @task

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
    env: list[str] | None = None,
)
```

Declares a **task** — a workflow-management step such as a deploy, notification,
migration, or cache warm. Tasks always re-run and are never cached, so they're
the right home for "do something" operations that don't produce cacheable data.

- They may appear **anywhere** in the graph (not just at the leaves).
- They may depend on assets, sensors, or other tasks (via `inputs=`).
- For ordering-only dependencies (no data needed), use the `_` prefix convention:
  `inputs={"_dep": some_node}`. The `_` prefix tells barca to skip artifact
  deserialization — the parameter receives `None`.
- They must **not** be an input to an asset or sensor (a task always re-runs, so
  feeding its output into a cacheable node would keep that node perpetually
  stale).

Run a task with [`barca run`](/reference/cli/). Upstream assets are served from cache by
default (like `barca get`); `--refresh a,b` re-materializes the named assets and everything
downstream of them (`--no-cascade` limits it to the named assets), and `--refresh-all` (alias
`--no-cache`) re-materializes all of them.

```python
from barca import asset, task

@asset()
def report() -> dict:
    return {"rows": 42}

# Asset -> task: a task consuming an upstream asset.
@task(inputs={"data": report})
def send_email(data: dict) -> None:
    print(f"Sending report: {data}")

# Ordering-only: migrate runs first, notify doesn't need its data.
@task()
def migrate() -> None:
    run_migration()

@task(inputs={"_migrate": migrate})
def notify(_migrate) -> None:
    send_slack("migration done")
```

## parallel

```python
parallel(*callables) -> list
parallel_map(fn, items, **kwargs) -> list
```

Fan out work from inside a `@task` body across worker processes. `parallel()` takes
`functools.partial`-wrapped calls to other `@task`-decorated functions and returns their results
(or `ParallelError` objects for failed branches) in argument order. `parallel_map(fn, items)` is
sugar for `parallel(*(partial(fn, item) for item in items))`.

```python
from functools import partial
from barca import task, parallel, parallel_map

@task()
def deploy_us(model) -> str: ...

@task()
def deploy_eu(model) -> str: ...

@task()
def deploy_all(model) -> None:
    results = parallel(partial(deploy_us, model), partial(deploy_eu, model))
    # or: results = parallel_map(deploy, ["us", "eu"])
```

When running inside a barca worker, `parallel()` dispatches each branch to a separate worker
process via the coordinator (the calling worker is frozen for the duration and resumed on
completion); called standalone (outside a worker), it runs the callables sequentially. Barca's
static analysis recognizes `partial(fn, ...)` arguments (including inside a starred generator or
list comprehension, e.g. `parallel(*(partial(deploy, r) for r in regions))`) to build the
dependency graph; fully dynamic call sets (e.g. `parallel(*work_items)`) are supported at runtime
but can't be resolved statically. `parallel()`/`parallel_map()` calls are only recognized inside
`@task` bodies, not `@asset` bodies.

A failed branch is returned as a `ParallelError` (with `.error` holding the message) rather than
raising — inspect each result to detect failures.

## @unsafe

```python
@unsafe
def my_asset() -> str:
    return global_config["value"]
```

Marks a function as unsafe — it references globals, performs I/O, or otherwise cannot be tracked by AST analysis. `@unsafe` silences purity warnings; caching behaviour is unchanged. Barca makes no correctness guarantee for unsafe assets.

## Schedule

```python
Schedule("0 5 * * *")       # 5-field cron — daily at 05:00
Schedule("*/15 * * * * *")  # 6-field cron — every 15 seconds
```

Constructs a schedule freshness value. Use inside `freshness=` on any decorator.

Accepts standard **5-field** cron (`minute hour day-of-month month day-of-week`) and a
**6-field** form with a leading seconds field for sub-minute schedules. The scheduler
(`barca serve`) evaluates at 1-second resolution; a 5-field expression has its seconds
pinned to `0`, so it fires once per matching minute. The year field is not supported. See
the [Scheduling guide](/scheduling/) for the full model.
