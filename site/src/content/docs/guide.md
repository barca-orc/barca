---
title: Guide
description: A full tutorial from a single asset to a multi-stage DAG with sensors, tasks, and partitions.
---

This guide walks you through building a real pipeline with barca, from a single function to a multi-stage DAG with sensors, tasks, and partitions.

## Prerequisites

- Python >= 3.12
- barca installed (`pip install barca`)

Verify it works:

```bash
barca --help
```

## 1. Your first asset

An asset is a Python function that returns data. That's it.

Create a file called `pipeline.py`:

```python
from barca import asset

@asset()
def greeting() -> dict:
    return {"message": "Hello from barca!"}
```

Run it:

```bash
barca get pipeline.py
```

You'll see output like:

```json
{"elapsed_seconds":0.039,"final_output":{"message":"Hello from barca!"},"phases":1,"run_id":"b1b1ff29d6cc","steps_executed":1}
```

And on stderr, a one-line progress summary:

```
[barca] 1/1 steps | done in 0.0s
```

**What just happened?**

1. The Rust binary parsed `pipeline.py` using ruff's AST parser (no import, pure text analysis)
2. Found one `@asset()` decorator, extracted the function name and metadata
3. Built a trivial DAG (one node, no edges)
4. Generated an execution plan with one phase
5. Spawned a Python worker, which imported your module and called `greeting()`
6. Collected the return value and persisted it to `.barca/metadata.db`

The `@asset()` decorator itself does nothing at runtime -- it's an identity function. Your code runs exactly the same with or without barca installed.

## 2. Dependencies between assets

Assets can depend on other assets via `inputs=`. Barca resolves the DAG and executes them in the right order.

```python
from barca import asset

@asset()
def raw_data() -> list[dict]:
    return [
        {"name": "Alice", "score": 92},
        {"name": "Bob", "score": 85},
        {"name": "Carol", "score": 97},
    ]

@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    scores = [d["score"] for d in data]
    return {
        "count": len(scores),
        "mean": sum(scores) / len(scores),
        "top": max(data, key=lambda d: d["score"])["name"],
    }
```

```bash
barca get pipeline.py
```

Barca sees that `summary` depends on `raw_data`, so it:

1. Puts both in the same phase, same worker stream — a linear chain has no
   parallelism to gain from a phase split
2. Runs `raw_data` first in that stream's worker
3. Hands `raw_data`'s output straight to `summary` in the same process
4. Runs `summary` with the data injected as the `data` kwarg

You can inspect the plan without running anything:

```bash
barca plan pipeline.py
```

```json
{
  "total_steps": 2,
  "phases": [
    {
      "reason": "Initial",
      "streams": [
        {"stream_id": "p0-w0", "steps": ["pipeline.py:raw_data", "pipeline.py:summary"]}
      ]
    }
  ]
}
```

## 3. Parallel execution

When assets are independent (no edges between them), barca runs them in parallel as separate worker streams within the same phase.

```python
from barca import asset

@asset()
def users() -> list[dict]:
    return [{"id": 1, "name": "Alice"}, {"id": 2, "name": "Bob"}]

@asset()
def products() -> list[dict]:
    return [{"id": 1, "name": "Widget"}, {"id": 2, "name": "Gadget"}]

@asset()
def orders() -> list[dict]:
    return [{"user_id": 1, "product_id": 2, "qty": 3}]

@asset(inputs={"users": users, "products": products, "orders": orders})
def report(users: list[dict], products: list[dict], orders: list[dict]) -> dict:
    return {
        "total_users": len(users),
        "total_products": len(products),
        "total_orders": len(orders),
    }
```

The plan will look like:

```
Phase 1 (Initial): users, products, orders  ← 3 parallel streams
Phase 2 (FanIn):    report                   ← waits for all 3
```

Barca spawns up to `available_parallelism()` workers per phase. On an 8-core machine, all three source assets run concurrently.

## 4. Diamond DAGs

Real pipelines aren't linear chains. They fork and join. Barca handles this naturally.

```python
from barca import asset

@asset()
def raw_sales() -> list[dict]:
    return [{"product": "A", "amount": 100}, {"product": "B", "amount": 200}]

@asset()
def raw_inventory() -> list[dict]:
    return [{"product": "A", "stock": 50}, {"product": "B", "stock": 10}]

@asset(inputs={"sales": raw_sales})
def clean_sales(sales: list[dict]) -> list[dict]:
    return [s for s in sales if s["amount"] > 0]

@asset(inputs={"inventory": raw_inventory})
def clean_inventory(inventory: list[dict]) -> list[dict]:
    return [i for i in inventory if i["stock"] > 0]

@asset(inputs={"sales": clean_sales, "inventory": clean_inventory})
def dashboard(sales: list[dict], inventory: list[dict]) -> dict:
    return {
        "total_revenue": sum(s["amount"] for s in sales),
        "low_stock": [i["product"] for i in inventory if i["stock"] < 20],
    }
```

The execution plan:

```
Phase 1 (Initial): raw_sales → clean_sales        ← one stream, one worker, chained
                    raw_inventory → clean_inventory ← parallel stream, chained
Phase 2 (FanIn):    dashboard                       ← waits for both chains
```

Each source-and-its-transform pair is a linear chain, so barca runs it as one
worker stream rather than splitting it across phases; the two chains still run
in parallel with each other. `dashboard` depends on both chains' outputs, so it
gets its own fan-in phase.

## 5. Sensors

Sensors observe external state. They're source nodes in the DAG that return `(update_detected, output)`.

```python
from barca import asset, sensor

@sensor()
def check_inbox() -> tuple[bool, list[str]]:
    from pathlib import Path
    files = list(Path("inbox").glob("*.csv"))
    return bool(files), [str(f) for f in files]

@asset(inputs={"files": check_inbox})
def process_inbox(files: list[str]) -> dict:
    return {"processed": len(files), "files": files}
```

Sensors are never cached -- they always re-run. The worker unpacks the `(update_detected, output)` tuple automatically, so a downstream asset's kwarg receives just `output` (as in `files: list[str]` above), not the tuple.

A sensor's returned value is part of the run hash of every asset that reads it: when the value
changes, those assets (and everything downstream of them) re-run; when it is the same, they are
served from cache. That makes a sensor the way to track external data that changes in place, for
example a sensor that returns a blob's etag in front of the asset that reads the blob. Return only
what identifies the data: a value that changes on every run (a timestamp) re-runs the sensor's
consumers every time. `--dry-run` and `barca status` assume a sensor returns its last recorded
value, and report its consumers as `unknown` before it has ever run. See `barca docs cache`,
"External data that changes in place". Here, `process_inbox` re-runs when the list of files changes and is cached otherwise.

## 6. Tasks

Tasks handle side effects -- deploying, notifying, writing to external systems. They always re-run and are never cached.

```python
from barca import asset, task

@asset()
def daily_report() -> dict:
    return {"revenue": 42000, "orders": 150}

@task(inputs={"report": daily_report})
def send_slack_notification(report: dict) -> None:
    # In production, this would call Slack's API
    print(f"Daily revenue: ${report['revenue']:,}")

@task(inputs={"report": daily_report})
def write_to_s3(report: dict) -> None:
    # In production, this would upload to S3
    print(f"Uploading report with {report['orders']} orders")
```

Both tasks run in the same phase (they're independent of each other) after `daily_report` completes. Use `barca run` to execute tasks.

`barca run` always re-runs the task, but serves upstream assets from cache when they are fresh (same as `barca get`) -- so `daily_report` is reused on `barca run send_slack_notification pipeline.py` if it's already materialized. Pass `--refresh report_name_a,report_name_b` to force re-materialize specific upstream assets and everything downstream of them (add `--no-cascade` to re-materialize only the named assets), or `--refresh-all` to refresh the whole upstream cone. `barca get` takes the same `--refresh`, `--no-cascade` and `--refresh-all`.

## 7. Partitions

Partitions fan a single asset definition into N independent runs, one per partition key.

```python
from barca import asset, partitions, collect

@asset(partitions={"region": partitions(["us-east", "us-west", "eu-west"])})
def regional_sales(region: str) -> dict:
    # In production, this would query a database filtered by region
    return {"region": region, "total": hash(region) % 10000}

@asset(inputs={"sales": collect(regional_sales)})
def global_summary(sales: list[dict]) -> dict:
    total = sum(v["total"] for v in sales)
    return {"regions": len(sales), "global_total": total}
```

- `regional_sales` runs 3 times, once per region
- `collect(regional_sales)` aggregates all partition outputs into a single list
- `global_summary` receives all three results at once
- An unpartitioned asset passed in `inputs=` to a partitioned asset reaches every key unchanged;
  it runs once, before any key, and changing it (or `--refresh` on it) re-runs every key
- `partitions_from(regional_sales)` gives another asset the same keys; each key receives that
  key's `regional_sales` output as the parameter `regional_sales`
- An unpartitioned asset cannot read a partitioned one without `collect()`: plain
  `inputs={"sales": regional_sales}` is a usage error (exit 2)

## 8. Multi-file pipelines

Barca can parse multiple Python files. Assets can reference functions across files as long as all files are passed to the CLI.

```
my_project/
  sources.py      # @asset defs for raw data
  transforms.py   # @asset defs that depend on sources
  tasks.py        # @task defs
```

```bash
barca get my_project/sources.py my_project/transforms.py my_project/tasks.py
```

Barca merges all discovered nodes into a single DAG and plans execution across the full graph.
With no target, `barca get` materializes the assets (and sensors) from all three files and skips
the tasks in `tasks.py`; run a task by name with `barca run <task> <files>`.

### Helper modules and the cache

Plain helper modules don't need to be passed on the command line. An asset's run hash covers the
helpers it uses from `.py` files in the pipeline file's directory and its subdirectories, so
editing one re-runs exactly the assets that use it. Both import styles are followed, and both hash
only the definitions the asset uses, not the whole module:

```python
import helpers                    # helpers.clean(...)
import utils.text as t            # t.normalize(...)
from helpers import clean         # clean(...)
```

Editing `clean` (or anything it calls) re-runs the assets that call it; editing another function
in `helpers.py` re-runs nothing. The pipeline path can be spelled any way (`pipeline.py`,
`./pipeline.py`, an absolute path, or `my_project/pipeline.py` from the parent directory): all
compute the same run hash. Not followed yet: class bodies, imports inside a function body, a
module used as a value (`getattr(helpers, name)`), and modules above the pipeline's directory.
Standard-library and installed packages are never hashed. In those cases recompute with
`barca get <asset> pipeline.py --refresh-all` (or `--refresh <asset>` on `get` or `run`).
See `barca docs cache`.

## 9. Freshness markers

Control when assets should re-run:

```python
import time

from barca import asset, Always, Manual, Schedule

@asset(freshness=Always())
def always_fresh() -> dict:
    """Re-runs on every reconcile cycle."""
    return {"ts": time.time()}

@asset(freshness=Manual())
def on_demand() -> dict:
    """Only runs when explicitly triggered."""
    return {"manual": True}

@asset(freshness=Schedule("0 5 * * *"))
def daily_at_5am() -> dict:
    """Eligible for execution at 5 AM daily."""
    return {"scheduled": True}
```

:::note
`Schedule` freshness is enforced by the long-running server. Run
`barca serve pipeline.py` and the scheduler fires each scheduled asset on its cron tick
(evaluated in local time by default; `--timezone` to change). Cron is standard 5-field,
plus a 6-field form with a leading seconds field (`*/15 * * * * *` — every 15s) for
sub-minute schedules. Last-fire times are persisted, so a job whose tick passed while the
server was down fires once to catch up on restart. Inspect the schedule with
`barca list pipeline.py` (scheduled definitions show their next fire time) or
`GET /schedule`, and disable it with `--no-schedule`. A one-shot `barca get`/`barca run`
does *not* consult the clock — it materializes on demand.
:::

If your goal is simply **running tasks on a timer** (rather than keeping data assets
fresh), see the dedicated [Scheduling](/scheduling/) guide — it covers the minimal
`@task(freshness=Schedule(...))` + `barca serve` setup end to end.

## 10. Inspecting plans

`barca plan` is your debugging tool. It shows you exactly what barca will do without executing anything.

```bash
# See the plan as formatted JSON
barca plan pipeline.py | python -m json.tool

# Count total steps
barca plan pipeline.py | python -c "import json,sys; print(json.load(sys.stdin)['total_steps'])"
```

The plan shows:
- **Phases**: groups of work that execute sequentially
- **Streams**: parallel workers within a phase
- **Steps**: individual asset functions within a stream
- **Reason**: why a phase boundary exists: `{"type": "initial"}` for the first phase, or `{"type": "fan_in", "node_id": ...}` when a step needs outputs from multiple prior streams

## Putting it together

Here's a complete pipeline that uses everything:

```python
# pipeline.py
from barca import asset, sensor, task, partitions, partitions_from, collect

TABLES = ["users", "events", "purchases"]

# Sensor: poll for new data
@sensor()
def check_data_lake() -> tuple[bool, dict]:
    # Check if new parquet files landed
    return True, {"path": "s3://bucket/raw/", "files": 3}

# Source assets (parallel)
@asset(partitions={"table": partitions(TABLES)})
def extract(table: str) -> dict:
    return {"table": table, "rows": 1000}

# Transform (runs per partition — partitions_from(extract) reuses extract's keys,
# and each transform(table=X) receives extract's output for table=X as `extract`)
@asset(partitions={"table": partitions_from(extract)})
def transform(table: str, extract: dict) -> dict:
    return {"table": extract["table"], "clean_rows": extract["rows"] - 10}

# Aggregate all partitions
@asset(inputs={"tables": collect(transform)})
def merge(tables: list[dict]) -> dict:
    total = sum(v["clean_rows"] for v in tables)
    return {"total_rows": total, "tables": len(tables)}

# Sensor-driven asset
@asset(inputs={"lake_status": check_data_lake, "data": merge})
def report(lake_status: dict, data: dict) -> dict:
    # The worker already unpacked check_data_lake's (update_detected, output)
    # tuple, so lake_status here is just the sensor's output dict.
    return {"source": lake_status["path"], **data}

# Side-effect task
@task(inputs={"report": report})
def notify(report: dict) -> None:
    print(f"Pipeline complete: {report['total_rows']} rows from {report['tables']} tables")
```

```bash
barca plan pipeline.py   # inspect the execution plan
barca get pipeline.py    # run it
```

## Tips

- **Decorators are no-ops.** Your code works without barca installed. `from barca import asset` imports an identity function. This means you can unit test your functions normally.

- **Outputs are fully materialized between steps.** Workers never pass in-memory lazy handles across process boundaries — every asset writes a complete artifact file (json, pickle, or parquet). That is intentional: materialization *is* the cache checkpoint. Downstream steps read the file back; parameter type annotations (e.g. `orders: pl.DataFrame`) only choose *how* parquet is decoded, not whether it is persisted. By default barca uses JSON for dicts, lists, strings, numbers, and booleans. Two escape hatches beyond that: `@asset(serializer="pickle")` for large or non-JSON-serializable plain-Python payloads (faster than JSON for large list-of-dict structures too — see `benchmarks/RESULTS.md`'s `etl_duckdb` notes), or return a pandas/polars DataFrame and barca automatically serializes it as parquet — no `serializer=` needed, and it's the fastest option for tabular data (vectorized columnar (de)serialization instead of row-by-row). If one efficient computation should produce several cacheable outputs, use multiple `@asset` definitions (sharing helpers) rather than trying to keep a lazy graph alive between steps.

- **Barca runs the code you saved.** Your pipeline files and the modules they import from the same directory tree are checked against a hash of their source, not `__pycache__` timestamps, so a same-size edit within one second (or under a tool that pins mtimes, such as Nix, Bazel or `touch -t`) never runs stale bytecode. Installed packages import as usual.

- **Use `barca plan` liberally.** It's free (no execution) and shows you exactly how barca decomposes your DAG.

- **Check `.barca/metadata.db`.** It's a SQLite database. You can query it directly:
  ```bash
  sqlite3 .barca/metadata.db "SELECT node_id, status, created_at FROM materializations ORDER BY created_at DESC LIMIT 10"
  ```

- **Stderr is for diagnostics.** Barca prints timing and topology info to stderr. Stdout is reserved for the result: a human summary in a terminal, structured JSON when piped (or with `--json`). Pipe stdout to `jq` for clean formatting:
  ```bash
  barca get pipeline.py 2>/dev/null | jq .
  ```
