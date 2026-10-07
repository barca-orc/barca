---
title: "Framework Comparison: Aesthetics, Transparency, and Overhead"
description: How much does each orchestrator get in the way? Barca vs Dagster, Prefect, and Airflow.
---

How much does each framework get in the way? Evaluated on: **minimal code**, **easy to understand**, **doesn't mask its behavior**.

## The Trivial Case

"Return a dict. That's it."

**Barca** (4 lines):
```python
from barca import asset

@asset()
def single_asset() -> dict:
    return {"status": "ok"}
```

**Dagster** (4 lines of asset code + 8 lines of runner):
```python
from dagster import asset, materialize

@asset
def single_asset():
    return {"status": "ok"}

# To actually run it:
result = materialize([single_asset])
```

**Prefect** (6 lines + runner):
```python
from prefect import flow, task

@task
def single_asset():
    return {"status": "ok"}

@flow
def bench_flow():
    return single_asset()
```

**Airflow** (10 lines):
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

### Verdict

| Framework | Lines for "return a dict" | Ceremony | Runs standalone? |
|-----------|--------------------------|----------|-----------------|
| **Barca** | 4 | `@asset()` decorator only | Yes (`python file.py` works, decorator is a no-op) |
| **Dagster** | 4 + `materialize()` call | Need `materialize([...])` to execute | No (needs dagster runner) |
| **Prefect** | 4 + `@flow` wrapper | Every task needs a wrapping `@flow` | No (needs prefect runner) |
| **Airflow** | 10 | `@dag(dag_id=..., start_date=..., schedule=..., catchup=...)` | No (needs airflow CLI + DB) |

Barca is the only one where the user code runs standalone without the framework installed.

---

## Dependencies: How You Wire Things Together

"Asset B depends on asset A."

**Barca** — explicit `inputs={}` dict:
```python
@asset()
def a():
    return {"value": 1}

@asset(inputs={"data": a})
def b(data):
    return {"value": data["value"] + 1}
```

**Dagster** — parameter name matching + `AssetIn`:
```python
@asset
def a():
    return {"value": 1}

# Option 1: implicit (param name must match asset name)
@asset
def b(a):
    return {"value": a["value"] + 1}

# Option 2: explicit (when param name differs)
@asset(ins={"data": AssetIn(key="a")})
def b(data):
    return {"value": data["value"] + 1}
```

**Prefect** — call-site wiring inside `@flow`:
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

**Airflow** — call-site wiring inside `@dag`:
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

### Verdict

| Framework | Where dependencies declared | Transparent? |
|-----------|---------------------------|-------------|
| **Barca** | At definition (`inputs={}`) | Yes — you see the DAG from decorators alone, no runner code needed |
| **Dagster** | At definition (`AssetIn`) or implicit | Mostly — implicit name matching is magic; explicit `AssetIn` is clear |
| **Prefect** | At call site (inside `@flow`) | Yes — but you must read the flow function to understand the DAG |
| **Airflow** | At call site (inside `@dag`) | Yes — same as Prefect, DAG is in the orchestration function |

Barca's approach means you can look at ANY function in isolation and know its inputs without reading any orchestration code. The DAG is fully declared in decorators.

---

## The Diamond Pattern (Fan-out + Fan-in)

5 parallel sources → 5 parallel transforms → merge → post-process. This is where frameworks diverge most.

**Barca** — just more `inputs={}`:
```python
@asset(inputs={"f0": feat_0, "f1": feat_1, "f2": feat_2, "f3": feat_3, "f4": feat_4})
def merge(f0, f1, f2, f3, f4):
    return {"combined": [x for f in (f0, f1, f2, f3, f4) for x in f["features"]]}
```
No special fan-in syntax. Dependencies are dependencies.

**Dagster** — same `AssetIn` pattern:
```python
@asset(ins={"f0": AssetIn(key="feat_0"), "f1": AssetIn(key="feat_1"), ...})
def merge(f0, f1, f2, f3, f4):
    ...
```
Verbose but explicit. `AssetIn` for every input.

**Prefect** — call-site wiring:
```python
@flow(task_runner=ConcurrentTaskRunner())
def pipeline():
    s = [src_0(), src_1(), src_2(), src_3(), src_4()]
    p = [prep(s[i]) for i in range(5)]
    f = [feat(p[i]) for i in range(5)]
    m = merge(f[0], f[1], f[2], f[3], f[4])
    ...
```
The parallelism is visible but you need `ConcurrentTaskRunner()` to actually run in parallel. Without it, everything is sequential.

**Airflow** — same call-site pattern:
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
Clean! Airflow's `@task` decorator with the TaskFlow API is actually pleasant. But you pay for it with `@dag(dag_id=..., start_date=..., schedule=..., catchup=...)` on every DAG.

---

## What Each Framework Hides From You

| Framework | What's hidden | How it surprises you |
|-----------|--------------|---------------------|
| **Barca** | Worker process spawning, artifact serialization | Almost nothing — it's a binary that runs your code. `.barca/` appears but it's just a cache. |
| **Dagster** | I/O managers, run storage, event log, asset catalog | A lot. Default I/O manager pickles everything. Logs go to a structured event store. `materialize()` does way more than it looks. |
| **Prefect** | Task state machine, result persistence, API server, flow run tracking | Prefect tracks every task state transition (Pending→Running→Completed). By default it phones home to Prefect Cloud or needs a local server. `ConcurrentTaskRunner` vs default is a silent behavior change. |
| **Airflow** | Scheduler, executor, metadata DB, XCom serialization, DAG parsing interval | The most hidden behavior. Your DAG file is parsed every 30s by the scheduler. XCom (data passing between tasks) has a 48KB default limit. The executor (Sequential/Local/Celery/Kubernetes) fundamentally changes behavior with no code change. |

---

## Boilerplate per Asset

How much framework code do you write per asset function?

| Framework | Decorator | Extra imports | Config objects | Orchestration wrapper |
|-----------|-----------|--------------|----------------|----------------------|
| **Barca** | `@asset()` | `from barca import asset` | None | None |
| **Dagster** | `@asset` | `from dagster import asset, AssetIn` | `AssetIn(key=...)` per input | `materialize([...])` |
| **Prefect** | `@task` | `from prefect import task, flow` | `ConcurrentTaskRunner()` for parallelism | `@flow` wrapper function |
| **Airflow** | `@task` | `from airflow.decorators import dag, task` | `@dag(dag_id=..., start_date=..., schedule=..., catchup=...)` | `@dag` wrapper function + `dag()` call |

---

## Execution Model Transparency

"What actually happens when I run this?"

**Barca**: `barca get file.py` → Rust binary parses source (no import), builds DAG, spawns Python workers, collects results. You can see exactly what happened: `barca plan file.py` shows the execution plan as JSON. Artifacts are plain files in `.barca/artifacts/`. No hidden state machines.

**Dagster**: `materialize([assets])` → loads assets into a "repository", builds a job, creates a "run", executes steps through an I/O manager that pickles results, logs events to a structured store. `dagster dev` launches a full web UI. Much of this is invisible from the code.

**Prefect**: `flow()` → creates a "flow run", each task becomes a "task run" with state transitions tracked by Prefect's API. Default behavior phones home to Prefect Cloud. Local mode uses SQLite. Result persistence is configurable but defaults are opaque.

**Airflow**: `airflow dags test` → parses all DAG files, initializes metadata DB, creates DagRun + TaskInstance records, passes data via XCom (stored in DB, 48KB default limit), logs to filesystem. Production mode requires scheduler + executor + message broker + DB.

---

## What You Give Up With Barca

Minimalism is a tradeoff. Here's what the other frameworks have that barca doesn't:

| Feature | Dagster | Prefect | Airflow | Barca | Roadmap |
|---------|---------|---------|---------|-------|---------|
| **Web UI / dashboard** | Yes (dagster dev) | Yes (Prefect Cloud / server) | Yes (webserver) | No | `barca serve` ships an HTTP API only, no UI planned |
| **Run history / lineage** | Full event log, asset catalog | Flow run tracking, task states | DagRun/TaskInstance records | Basic: rows in SQLite, no UI | Planned — just more DB rows ([#50]) |
| **Retry on failure** | Built-in per-op retries | Built-in per-task retries | Built-in retries + SLAs | Yes — `@asset(retries=N, retry_backoff=...)`, linear backoff, applied by the Rust coordinator | Shipped ([#51]) |
| **Alerting / notifications** | Sensors + hooks | Automations, Slack/email | Email, Slack, PagerDuty | No | Planned — Slack + Resend hooks via `barca.toml` ([#52]) |
| **Scheduling** | Built-in cron + sensors | Built-in via deployments | Core feature (scheduler daemon) | Cron enforcement in `barca serve`: 5-field + 6-field sub-minute (1s resolution), timezone-aware, durable catch-up, parallel runs, `GET /schedule` (see [vs. cron](/comparisons/cron/)) | Shipped ([#54]); per-tick replay of long outages not supported |
| **Server mode** | Built-in (dagster dev) | Built-in (prefect server) | Built-in (webserver + scheduler) | Yes — `barca serve` (HTTP API + cron scheduler; binds 127.0.0.1, no auth) | Shipped ([#53]) |
| **Remote storage** | Pluggable I/O managers (S3, GCS, etc.) | Result storage backends | XCom + external storage hooks | Yes — Azure ADLS Gen2, S3, GCS, and Cloudflare R2 via fsspec scheme dispatch, plus a shared metadata DB pulled/pushed as a blob | Shipped ([#55]); pluggable DB engine beyond Turso/libSQL still planned ([#56]) |
| **Docker / containers** | Supported via Kubernetes executor | Supported via Docker infra | Celery/Kubernetes executors | Not built-in | Artifact storage is already pluggable; trivial once the DB backend is too ([#56]) |
| **Multi-user / team** | Workspace permissions, code locations | Workspace RBAC, service accounts | DAG-level permissions, RBAC | Single-user only | Not planned — deliberate decision for simplicity |
| **Backfills** | Built-in partitioned backfills | Via deployments | `dags backfill` (v2) | Supported — `barca get` re-runs subgraphs; needs partition filter on CLI | CLI flag: `--partition region=us` ([#57]) |
| **Dynamic pipelines** | Dynamic partitions, graph DSL | Dynamic tasks via `.map()` | Dynamic task mapping | Supported — static, dynamic (eval at plan time), derived (`partitions_from`) | — |
| **Task-based workflows** | Jobs + ops (separate from assets) | `@task` + `@flow` (native) | `@task` (native) | `@task` for side-effects; can appear anywhere in graph | Implemented in v0.2.0 |
| **APM / observability** | Via OpenTelemetry | Via OpenTelemetry | Via StatsD / Prometheus | Not built-in | Planned — Datadog (P1) + Sentry (P2) ([#59]) |
| **Data quality / expectations** | Asset checks, freshness policies | Not built-in (use Great Expectations) | Not built-in | Not built-in — use pydantic/pandera/asserts in your functions; failures block downstream naturally | Syntactic sugar at best; not urgent |
| **Plugin ecosystem** | Large (200+ integrations) | Growing (collections) | Massive (providers) | None | Hooks system ([#52]) is the starting point |

[#50]: https://github.com/ExSidius/barca/issues/50
[#51]: https://github.com/ExSidius/barca/issues/51
[#52]: https://github.com/ExSidius/barca/issues/52
[#53]: https://github.com/ExSidius/barca/issues/53
[#54]: https://github.com/ExSidius/barca/issues/54
[#55]: https://github.com/ExSidius/barca/issues/55
[#56]: https://github.com/ExSidius/barca/issues/56
[#57]: https://github.com/ExSidius/barca/issues/57
[#58]: https://github.com/ExSidius/barca/issues/58
[#59]: https://github.com/ExSidius/barca/issues/59

Barca is fast and minimal **because** it doesn't do most of this yet. But the roadmap is deliberate: each feature is designed to add capability without adding framework complexity. Run history is just more DB rows. Retries are extra attempts the Rust coordinator dispatches to workers, not a new service. Scheduling is a cron check in the server. None of these require new services, config languages, or architectural overhead.

The bet is that for many workloads — especially agent-driven pipelines, local data processing, and development iteration — you want to start minimal and add what you need, rather than pay for everything upfront.

### Where the other frameworks genuinely shine

**Dagster** is the most thoughtful about data assets as first-class citizens. Its I/O manager abstraction means you can swap storage backends without changing business logic. Asset lineage and the software-defined asset model are genuinely good ideas that barca's `@asset` decorator is inspired by. If you need a production data platform with a team, Dagster is the right choice.

**Prefect** is the most Pythonic. The `@task` / `@flow` model feels natural. `ConcurrentTaskRunner` and `.map()` for dynamic parallelism are elegant. If you want an orchestrator that feels like writing normal Python with superpowers, Prefect is excellent.

**Airflow** has the largest ecosystem and the most battle-tested production deployment story. If you need 50 different provider integrations, a scheduler that runs 24/7, and an operations team that already knows Airflow, nothing else compares. The TaskFlow API in Airflow 2+ is a genuine improvement over the old operator model.

### Barca's honest positioning

Barca is not a replacement for any of these in production data platform scenarios. It's for a different use case: **you want to write Python functions, have them run fast, cache correctly, and get out of the way.** No server, no config, no framework to learn. The cost is that you're on your own for everything beyond execution and caching.

---

## Summary Scorecard

| Criteria | Barca | Dagster | Prefect | Airflow |
|----------|-------|---------|---------|---------|
| **Minimal code** | Best | Good | OK | Verbose |
| **Dependency clarity** | Best (decorators only) | Good (AssetIn is explicit) | OK (read flow function) | OK (read dag function) |
| **Behavior transparency** | Best (binary + files) | Mixed (I/O managers hidden) | Mixed (state machine hidden) | Low (scheduler/executor hidden) |
| **Runs standalone** | Yes | No | No | No |
| **Feature richness** | Low | High | High | Highest |
| **Production readiness** | Early (pre-1.0) | Mature | Mature | Very mature |
| **Team / multi-user** | No | Yes | Yes | Yes |
| **Remote execution** | No | Yes | Yes | Yes |

---

## Performance (Single Run, No Caching)

| Benchmark | Barca | Dagster | Prefect | Airflow |
|-----------|-------|---------|---------|---------|
| Trivial (1 asset) | **25ms** | 378ms (15x) | 3.8s (153x) | 2.2s (87x) |
| Chain 100 | **77ms** | 887ms (12x) | 3.6s (46x) | 79.5s (1,033x) |
| Deep diamond (18) | **66ms** | 453ms (7x) | 3.6s (54x) | 15.6s (237x) |
| Fan-out 500×50ms | **2.4s** | 29.7s (12x) | 30.7s (13x) | 417s (171x) |

The speed gap is real but context matters. In a 10-minute ETL pipeline, 400ms of framework overhead (Dagster) is noise. The gap matters most for: fast iteration loops, agent-driven pipelines that run many small DAGs, and workloads where framework overhead dominates actual compute.

### Partitioned Workloads (10 steps × 1000 partitions = 10,000 steps)

Each framework uses its idiomatic partition/map pattern — no strawmen.

| Benchmark | Barca | Dagster | Prefect | Airflow |
|-----------|-------|---------|---------|---------|
| 10k partitioned steps | **0.7s** | 95s (136x) | >9min (killed) | >22min (killed) |
| Pattern used | `partitions()` | `StaticPartitionsDefinition` | `task.map()` | `expand()` + PostgreSQL |

Barca's late partition expansion creates 10 StreamSteps (one per node), not 10,000. Workers expand partitions internally. The other frameworks create per-partition objects in their registries/DBs, which doesn't scale.

At 200k steps (100k partitions × 2 steps), barca completes in 14s. The other frameworks are not viable at this scale.
