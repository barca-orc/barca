---
title: "Framework Comparison: Code, Features and Overhead"
description: The same small pipelines written for barca, Dagster, Prefect and Airflow, a feature list, and run times measured on 2026-06-05 with barca 0.1.5.
---

Last measured: 2026-06-05, with barca 0.1.5 and Dagster (version not recorded), Prefect (version not recorded) and Airflow 3.2.2, on an Apple Silicon (M-series) Mac. Not re-run since; the current barca release is 0.18.0. Re-run tracked in [#277](https://github.com/barca-orc/barca/issues/277).

The Dagster and Prefect versions were whatever PyPI served as latest on that date, under Python
3.12; barca ran under Python 3.14. The code samples for the other tools are the benchmark sources
as written then. Statements about Dagster, Prefect and Airflow on this page were recorded on
2026-06-05 and have not been checked against their current documentation, except where a note
says so. Notes dated 2026-10-07 say where barca itself has changed.

## One function that returns a dict

**Barca:**
```python
from barca import asset

@asset()
def single_asset() -> dict:
    return {"status": "ok"}
```

**Dagster:**
```python
from dagster import asset, materialize

@asset
def single_asset():
    return {"status": "ok"}

# To run it:
result = materialize([single_asset])
```

**Prefect:**
```python
from prefect import flow, task

@task
def single_asset():
    return {"status": "ok"}

@flow
def bench_flow():
    return single_asset()
```

**Airflow:**
```python
from datetime import datetime
from airflow.decorators import dag, task

@task
def single_asset():
    return {"status": "ok"}

@dag(dag_id="trivial", start_date=datetime(2024, 1, 1), schedule=None, catchup=False)
def trivial_dag():
    single_asset()

trivial_dag()
```

| Framework | What the sample needs besides the function |
|-----------|--------------------------|
| Barca | the `@asset()` decorator; run with `barca get file.py` |
| Dagster | the `@asset` decorator and a `materialize([...])` call |
| Prefect | the `@task` decorator and a `@flow` function that calls it |
| Airflow | the `@task` decorator, a `@dag(dag_id=..., start_date=..., schedule=..., catchup=...)` function, and a call to it |

Barca's decorators return the function unchanged, so `python file.py` and a direct call to
`single_asset()` run it as ordinary Python (the `barca` package has to be importable). Nothing
is cached or recorded that way.

## Declaring that B depends on A

**Barca** (an `inputs={}` dict on the decorator):
```python
@asset()
def a():
    return {"value": 1}

@asset(inputs={"data": a})
def b(data):
    return {"value": data["value"] + 1}
```

**Dagster** (a parameter named after the upstream asset, or `AssetIn`):
```python
@asset
def a():
    return {"value": 1}

# Option 1: the parameter name matches the asset name
@asset
def b(a):
    return {"value": a["value"] + 1}

# Option 2: explicit, when the parameter name differs
@asset(ins={"data": AssetIn(key="a")})
def b(data):
    return {"value": data["value"] + 1}
```

**Prefect** (calls inside a `@flow`):
```python
@task
def a():
    return {"value": 1}

@task
def b(data):
    return {"value": data["value"] + 1}

@flow
def pipeline():
    result_a = a()
    return b(result_a)  # wired here, not at definition
```

**Airflow** (calls inside a `@dag`):
```python
@task
def a():
    return {"value": 1}

@task
def b(data):
    return {"value": data["value"] + 1}

@dag(dag_id="chain", start_date=datetime(2024, 1, 1), schedule=None, catchup=False)
def pipeline():
    result_a = a()
    b(result_a)

pipeline()
```

| Framework | Where the dependency is written |
|-----------|---------------------------|
| Barca | on the consuming function's decorator (`inputs={}`) |
| Dagster | on the consuming function: its parameter name, or `AssetIn` |
| Prefect | in the body of the `@flow` function |
| Airflow | in the body of the `@dag` function |

Checked 2026-10-07: Dagster's
[passing data between assets](https://dagster.io/docs/guides/build/assets/passing-data-between-assets)
guide still describes option 1, a parameter named after the upstream asset.

## Fan-out and fan-in

Five sources, five transforms, one merge. The merge step in each:

**Barca:**
```python
@asset(inputs={"f0": feat_0, "f1": feat_1, "f2": feat_2, "f3": feat_3, "f4": feat_4})
def merge(f0, f1, f2, f3, f4):
    return {"combined": [x for f in (f0, f1, f2, f3, f4) for x in f["features"]]}
```

**Dagster:**
```python
@asset(ins={"f0": AssetIn(key="feat_0"), "f1": AssetIn(key="feat_1"), ...})
def merge(f0, f1, f2, f3, f4):
    ...
```

**Prefect:**
```python
@flow(task_runner=ConcurrentTaskRunner())
def pipeline():
    s = [src_0(), src_1(), src_2(), src_3(), src_4()]
    p = [prep(s[i]) for i in range(5)]
    f = [feat(p[i]) for i in range(5)]
    m = merge(f[0], f[1], f[2], f[3], f[4])
    ...
```

Checked 2026-10-07: Prefect's current
[task runners](https://docs.prefect.io/v3/concepts/task-runners) page names the default runner
`ThreadPoolTaskRunner` and does not mention `ConcurrentTaskRunner`. It also says a task called
directly, as in this sample, runs in the main thread and blocks until it completes; concurrent
execution needs `.submit()` or `.map()`. The sample above therefore ran its tasks one after
another, which affects the Prefect times at the end of this page.

**Airflow:**
```python
@dag(...)
def deep_diamond_dag():
    s = [src_0(), src_1(), src_2(), src_3(), src_4()]
    p = [prep(s[i]) for i in range(5)]
    f = [feat(p[i]) for i in range(5)]
    m = merge(f[0], f[1], f[2], f[3], f[4])
    t = transform(m)
    output(t)
```

Independent barca steps run in parallel worker processes.

## What barca does when you run it

`barca get file.py` starts the Rust binary. It parses the source without importing it, builds
the dependency graph, starts Python worker processes, and collects the results. `barca plan
file.py` prints the execution plan as JSON without running anything. Each result is a file under
`.barca/artifacts/<node>/<run_hash>.<ext>`, and run history is in `.barca/metadata.db`.

This section used to describe the internals of the other three tools as well. Those descriptions
were written from memory and could not be verified, so they have been removed. One of them, a
48 KB default size limit on Airflow XComs, does not appear in Airflow's current
[XComs](https://airflow.apache.org/docs/apache-airflow/stable/core-concepts/xcoms.html) page.

## Features

The barca column was updated on 2026-10-07 for barca 0.18.0. The other three columns are as
recorded on 2026-06-05 and have not been re-checked; treat them as a starting point and read
each project's documentation.

| Feature | Dagster (2026-06-05) | Prefect (2026-06-05) | Airflow (2026-06-05) | Barca 0.18.0 |
|---------|---------|---------|---------|-------|
| Web UI | Yes (`dagster dev`) | Yes (Prefect Cloud or server) | Yes (webserver) | Yes, served by `barca serve` at `/ui/` ([Server API](/reference/server-api/)) |
| Run history | Event log, asset catalog | Flow run and task run tracking | DagRun and TaskInstance records | Rows in a local database: `barca history`, `barca stats`, `barca status` ([#50]) |
| Retry on failure | Per-op retries | Per-task retries | Retries | `retries=N, retry_backoff=...` on the decorator, linear backoff ([#51]) |
| Alerting | Sensors and hooks | Automations | Email and other notifiers | No ([#52] is open) |
| Scheduling | Cron schedules and sensors | Deployments | Scheduler | Cron in `barca serve`, 5 or 6 fields, 1-second resolution, one catch-up fire after downtime ([Scheduling](/scheduling/), [#54]) |
| Server mode | `dagster dev` | `prefect server` | Webserver and scheduler | `barca serve`: HTTP API, scheduler and web UI; binds 127.0.0.1, no authentication ([#53]) |
| Remote storage | I/O managers | Result storage | XCom backends | Artifacts on S3, S3-compatible stores, GCS or Azure through fsspec, and a shared history database ([Remote storage](/reference/remote-storage/), [#55]) |
| Containers | Kubernetes executor | Docker infrastructure | Celery and Kubernetes executors | No executor of its own; `barca serve` can run as a container's foreground process ([Scheduling](/scheduling/#keeping-it-running)) |
| Multi-user access control | Yes | Yes | Yes | No |
| Backfills | Partitioned backfills | Via deployments | `dags backfill` | `barca get` runs the partition keys that have no cached result. There is no flag to select keys ([#57] is open) |
| Dynamic fan-out | Dynamic partitions | `.map()` | Dynamic task mapping | `partitions([...])`, an expression evaluated at plan time, or `partitions_from(asset)` |
| Task workflows | Jobs and ops | `@task` and `@flow` | `@task` | `@task`, which always runs and may sit anywhere in the graph except upstream of an asset |
| Tracing | OpenTelemetry | OpenTelemetry | StatsD | Datadog traces, one per run with a span per step ([Telemetry](/reference/telemetry/)); OTLP in [#59] is open |
| Data quality checks | Asset checks | Not built in | Not built in | Not built in. A step that raises fails the run and nothing downstream of it runs |
| Integrations | Yes | Yes | Yes (providers) | None |

[#50]: https://github.com/barca-orc/barca/issues/50
[#51]: https://github.com/barca-orc/barca/issues/51
[#52]: https://github.com/barca-orc/barca/issues/52
[#53]: https://github.com/barca-orc/barca/issues/53
[#54]: https://github.com/barca-orc/barca/issues/54
[#55]: https://github.com/barca-orc/barca/issues/55
[#57]: https://github.com/barca-orc/barca/issues/57
[#59]: https://github.com/barca-orc/barca/issues/59

Barca is pre-1.0 and runs on one machine at a time: it has no remote executor, no access
control and no integrations library. If you need those, the other three tools have them.

## Run times, single run, nothing cached

Measured 2026-06-05 with barca 0.1.5.

| Benchmark | Barca | Dagster | Prefect | Airflow |
|-----------|-------|---------|---------|---------|
| Trivial (1 asset) | 25ms | 378ms | 3.8s | 2.2s |
| Chain 100 | 77ms | 887ms | 3.6s | 79.5s |
| Deep diamond (18) | 66ms | 453ms | 3.6s | 15.6s |
| Fan-out 500×50ms | 2.4s | 29.7s | 30.7s | 417s |

How to read these:

- The pipelines do almost no work, so the times are close to each tool's fixed cost per run and
  per step. For a pipeline whose steps take minutes, a difference of a few hundred milliseconds
  is small.
- The benchmark harness was changed after these numbers were taken. `benchmarks/RESULTS.md` in
  the repository records, on 2026-07-16, that worker counts were not matched between tools in
  this run and that most Prefect scripts called tasks directly instead of through `.submit()`,
  so the Prefect numbers here are a worst case on the parallel benchmarks (fan-out, deep
  diamond). It also records why Dagster ran its steps sequentially in process. Later passes
  under the corrected harness are in
  [`benchmarks/RESULTS.md`](https://github.com/barca-orc/barca/blob/main/benchmarks/RESULTS.md);
  they ran on different hardware and are not comparable with this table.
- According to the results file of that week, Airflow ran through `airflow dags test`.

### Partitioned workload: 10 steps × 1000 partitions = 10,000 steps

| Benchmark | Barca | Dagster | Prefect | Airflow |
|-----------|-------|---------|---------|---------|
| 10k partitioned steps | 0.7s | 95s | >9min (killed) | >22min (killed) |
| Pattern used | `partitions()` | `StaticPartitionsDefinition` | `task.map()` | `expand()` + PostgreSQL |

The Prefect and Airflow runs were stopped before they finished. In barca 0.1.5 the plan for this
workload had 10 entries, one per node, and the workers expanded the partitions. In the same
session barca ran 200,000 steps (100,000 partitions × 2 steps) in 14s; the other tools were not
run at that size.

Note, 2026-10-07: partition handling has changed since 0.1.5; barca 0.18.0 caches each partition
key on its own (`barca docs partitions`). The barca times above were not re-measured on 0.18.0.
