---
title: "Framework Comparison: Code, Features and Overhead"
description: The same small pipelines written for barca, Dagster, Prefect and Airflow, a feature list, and run times measured on 2026-10-21 with barca 0.28.0, Dagster 1.13.26, Prefect 3.9.0 and Airflow 3.3.2.
---

Run times last measured: 2026-10-21, with barca 0.28.0, Dagster 1.13.26, Prefect 3.9.0 and
Airflow 3.3.2 under Python 3.13, on one Linux machine (AMD Ryzen AI MAX+ 395). The setup,
commands, spread and load are in
[`benchmarks/RESULTS.md`](https://github.com/barca-orc/barca/blob/main/benchmarks/RESULTS.md).

The code samples for the other tools are the benchmark sources. The feature descriptions of
Dagster, Prefect and Airflow were recorded on 2026-06-05 and have not been checked against their
current documentation, except where a note says so. Notes dated 2026-10-07 say where barca
itself has changed.

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
@flow(task_runner=ConcurrentTaskRunner(max_workers=BENCH_WORKERS))
def deep_diamond_flow():
    s0, s1, s2, s3, s4 = src_0.submit(), src_1.submit(), src_2.submit(), src_3.submit(), src_4.submit()
    p0, p1, p2, p3, p4 = prep_0.submit(s0), prep_1.submit(s1), ...
    f0, f1, f2, f3, f4 = feat_0.submit(p0), feat_1.submit(p1), ...
    merged = merge.submit(f0, f1, f2, f3, f4)
    ...
```

Checked 2026-10-07: Prefect's current
[task runners](https://docs.prefect.io/v3/concepts/task-runners) page names the default runner
`ThreadPoolTaskRunner` and does not mention `ConcurrentTaskRunner`. Checked 2026-10-21: in
Prefect 3.9.0, `ConcurrentTaskRunner` imports as `ThreadPoolTaskRunner`. A task called directly
runs in the main thread and blocks until it completes; concurrent execution needs `.submit()` or
`.map()`, which the benchmark source above uses. The June version of this sample called the
tasks directly, so the June Prefect times ran them one after another.

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

The barca column describes barca 0.27.0 (updated 2026-10-20). The other three columns are as
recorded on 2026-06-05 and have not been re-checked; treat them as a starting point and read
each project's documentation.

| Feature | Dagster (2026-06-05) | Prefect (2026-06-05) | Airflow (2026-06-05) | Barca 0.27.0 |
|---------|---------|---------|---------|-------|
| Web UI | Yes (`dagster dev`) | Yes (Prefect Cloud or server) | Yes (webserver) | Yes, served by `barca serve` at `/ui/` ([Server API](/reference/server-api/)) |
| Run history | Event log, asset catalog | Flow run and task run tracking | DagRun and TaskInstance records | Rows in a local database: `barca history`, `barca stats`, `barca status` ([#50]) |
| Retry on failure | Per-op retries | Per-task retries | Retries | `retries=N, retry_backoff=...` on the decorator, linear backoff ([#51]) |
| Alerting | Sensors and hooks | Automations | Email and other notifiers | Slack: a post when a job starts failing, recovers, or is still failing a day later ([Telemetry](/reference/telemetry/), [#52]) |
| Scheduling | Cron schedules and sensors | Deployments | Scheduler | Cron in `barca serve`, 5 or 6 fields, 1-second resolution, one catch-up fire after downtime ([Scheduling](/scheduling/), [#54]) |
| Server mode | `dagster dev` | `prefect server` | Webserver and scheduler | `barca serve`: HTTP API, scheduler and web UI; binds 127.0.0.1 by default (`--host` to change), no authentication. Other machines point at it as the team's ledger ([Deploying](/deploying/), [#53]) |
| Remote storage | I/O managers | Result storage | XCom backends | Artifacts on S3, S3-compatible stores, GCS or Azure through fsspec, and backups of the run history database ([Remote storage](/reference/remote-storage/), [#55]) |
| Containers | Kubernetes executor | Docker infrastructure | Celery and Kubernetes executors | No executor of its own; `barca serve` can run as a container's foreground process ([Scheduling](/scheduling/#keeping-it-running)) |
| Multi-user access control | Yes | Yes | Yes | No |
| Backfills | Partitioned backfills | Via deployments | `dags backfill` | `barca get` runs the partition keys that have no cached result; `--partition region=emea` selects keys ([#57]) |
| Dynamic fan-out | Dynamic partitions | `.map()` | Dynamic task mapping | `partitions([...])`, an expression evaluated at plan time, or `partitions_from(asset)` |
| Task workflows | Jobs and ops | `@task` and `@flow` | `@task` | `@task`, which always runs and may sit anywhere in the graph except upstream of an asset |
| Tracing | OpenTelemetry | OpenTelemetry | StatsD | Datadog traces, one per run with a span per step ([Telemetry](/reference/telemetry/)); OTLP is parked ([#59]) |
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

## Run times, nothing cached

Measured 2026-10-21. Medians of 10 runs (fan-out 5; Airflow 5 on chain 100 and 3 on fan-out),
the frameworks taking turns run by run. The range is the fastest and slowest run.

| Benchmark | Barca 0.28.0 | Dagster 1.13.26 | Prefect 3.9.0 | Airflow 3.3.2 |
|-----------|-------|---------|---------|---------|
| Trivial (1 asset) | 64 ms (61–74) | 1.07 s (0.90–1.26) | 5.09 s (4.81–5.34) | 5.05 s (4.80–5.84) |
| Chain 100 | 112 ms (103–124) | 1.88 s (1.57–2.09) | 5.16 s (4.80–7.78) | 10.3 s (9.7–12.9) |
| Deep diamond (18) | 310 ms (265–342) | 1.19 s (1.05–1.31) | 5.13 s (4.68–5.70) | 6.45 s (5.97–7.35) |
| Fan-out 500×50ms | 2.24 s (2.22–2.44) | 30.2 s (29.7–30.8) | 6.53 s (6.19–10.03) | 54.6 s (53.5–56.1) |

How to read these:

- The pipelines do almost no work, so the times are close to each tool's fixed cost per run and
  per step. For a pipeline whose steps take minutes, a difference of a few hundred milliseconds
  is small.
- Each run is a new process: `barca get file.py --refresh-all`, a script calling Dagster's
  `materialize()`, a script calling a Prefect `@flow`, and `airflow dags test`. The Airflow time
  leaves out `airflow db migrate`, which runs first.
- barca and Prefect ran up to 16 steps at once (16 worker processes, a 16-thread task runner).
  Dagster's `materialize()` and Airflow's `dags test` run one step at a time, which is why
  their fan-out times are close to 500 × 50 ms plus overhead.
- barca starts its worker processes one at a time, each after the previous one has connected
  (30 to 40 ms each on this machine). That is most of barca's time on deep diamond and about a
  quarter of it on fan-out.
- The machine was shared with other work during the run; the trivial barca time was 64 ms in
  this pass and 79 to 99 ms in later passes on the same build.

### Partitioned workload: 10 steps × 1000 partitions = 10,000 steps

| | Barca 0.28.0 | Dagster 1.13.26 | Prefect 3.9.0 | Airflow 3.3.2 |
|-----------|-------|---------|---------|---------|
| Time | 2.27 s (2.14–2.40), 5 runs | 141 s (137–144), 3 runs | stopped at 10 min, 1 run | stopped at 10 min, 1 run |
| Done when stopped | | | 3,050 of 10,000 task runs | 1,000 of 10,000 task instances |
| Pattern used | `partitions()` | `StaticPartitionsDefinition`, one `materialize()` per key | `task.map()` | `expand()`, `dags test`, PostgreSQL |

The Prefect and Airflow runs were killed after 10 minutes. barca records a result for each of
the 10,000 partition steps, so a later run reruns only keys whose code or inputs changed. In
June, barca 0.1.5 ran this workload in 0.7 s without recording per-key results; on the machine
used for the table above, 0.1.5 takes 0.42 s.
