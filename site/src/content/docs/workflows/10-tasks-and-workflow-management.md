---
title: "Workflow: Tasks and Workflow Management"
description: Tasks are the counterpart to assets — side effects that always re-run.
---

Tasks are the counterpart to assets. Assets produce cached, provenance-tracked data. Tasks perform side-effects that always re-run.

## Asset vs Task

| | `@asset` | `@task` |
|---|----------|---------|
| Cached? | Yes | No -- always re-runs |
| Position in DAG | Anywhere | Anywhere |
| May depend on | Assets, sensors | Assets, sensors, tasks |
| May be input to | Assets, sensors, tasks | Tasks only |
| Run with | `barca get` | `barca run` |

The hard rule: **a task cannot be an input to an asset or sensor.** A task always re-runs, so feeding its output into a cacheable node would keep that node perpetually stale. DAG validation rejects this with a clear error.

## Commands

Use `barca get` for assets and `barca run` for tasks. Using the wrong command gives a clear error telling you which one to use.

```bash
# Materialize an asset (cache-aware).
barca get trained_model pipeline.py

# Run a task (always executes).
barca run deploy pipeline.py
```

`barca run` executes the targeted task and everything upstream of it:

- Upstream assets are materialized (cache-aware by default).
- Upstream tasks always re-run.
- Sensors always re-run.

### Selective re-runs with `--refresh`

By default, `barca run` uses cached upstream assets when they are fresh. Use `--refresh` to force re-materialize specific assets and everything downstream of them (or `--refresh-all` for all of them):

```bash
# Re-train the model and rebuild what depends on it; use cached data for everything else.
barca run deploy pipeline.py --refresh trained_model

# Force-rerun multiple assets (and their downstream).
barca run deploy pipeline.py --refresh raw_data,trained_model

# Re-run only raw_data; assets built from it stay cached (barca warns).
barca run deploy pipeline.py --refresh raw_data --no-cascade
```

`--refresh` previously did not cascade; `--no-cascade` keeps that behavior.

## Declaring tasks

```python
from barca import asset, task

@asset()
def trained_model() -> dict:
    return train(data)

@task(inputs={"model": trained_model})
def deploy(model: dict) -> dict:
    endpoint = upload_to_endpoint(model)
    return {"endpoint_id": endpoint.id}

@task(inputs={"d": deploy})
def notify(d: dict) -> None:
    send_slack(f"deployed to {d['endpoint_id']}")
```

Tasks use the same `inputs=` syntax as assets. The parameter name in the dict must match a parameter in the function signature.

## Ordering-only dependencies

When a task needs another node to run first but does not consume its output, use the `_` prefix convention:

```python
@task()
def migrate_db() -> None:
    run_migrations()

@task(inputs={"_migrate": migrate_db})
def warm_cache(_migrate: None) -> None:
    refresh_all_caches()

@task(inputs={"_warm": warm_cache})
def notify(_warm: None) -> None:
    send_slack("migration and cache warm complete")
```

The `_` prefix signals that the parameter exists only for ordering. The value is passed but typically ignored. This creates a clear sequential chain: `migrate_db` then `warm_cache` then `notify`.

## Wiring assets into tasks

Assets flow into tasks through `inputs=`, the same way assets flow into other assets:

```python
@asset()
def raw_data() -> list:
    return fetch_from_api()

@asset(inputs={"data": raw_data})
def trained_model(data: list) -> dict:
    return fit(data)

@task(inputs={"model": trained_model})
def deploy(model: dict) -> dict:
    return upload(model)
```

When you run `barca run deploy pipeline.py`, barca materializes `raw_data` and `trained_model` (using cache when fresh), then executes `deploy` with the model output.

## Task composition

Tasks compose through declared dependencies. Each task declares its inputs, and barca builds the execution plan:

```python
@task(inputs={"model": trained_model})
def deploy(model: dict) -> dict:
    return upload(model)

@task(inputs={"d": deploy})
def smoke_test(d: dict) -> dict:
    return run_tests(d["endpoint_id"])

@task(inputs={"result": smoke_test})
def notify(result: dict) -> None:
    if result["passed"]:
        send_slack("deploy succeeded")
    else:
        send_slack("deploy failed -- rolling back")
```

Running `barca run notify pipeline.py` executes the full chain: materialize assets, deploy, smoke test, notify.

Running `barca run smoke_test pipeline.py` runs only up to the smoke test -- `notify` is not in scope.

## Retries

Retries are declared per-task via decorator kwargs. Rust owns the retry loop.

```python
@task(retries=3, retry_backoff=1.0, inputs={"model": trained_model})
def deploy(model: dict) -> dict:
    return upload(model)
```

- `retries` -- total attempts (1 = no retry, 3 = up to 3 attempts). Default is 1.
- `retry_backoff` -- base delay in seconds. Delay before attempt N is `retry_backoff * N`.

Each retry is a fresh worker invocation. Retries do not cascade -- a parent task's retries do not re-run its children, and a child's retries do not trigger its parent.

See [Error Handling](/patterns/06-error-handling/) for details on failure propagation and common mistakes.

## What tasks cannot do

- **Cannot be inputs to assets.** An asset depending on a task would be perpetually stale. DAG validation rejects this.
- **Cannot be inputs to sensors.** Sensors cannot have inputs at all, from any node kind -- they're source nodes only.
- **Cannot use `barca get`.** Running `barca get` on a task name gives an error directing you to use `barca run`.

## Worked examples

- [`examples/basic_app`](https://github.com/barca-orc/barca/tree/main/examples/basic_app) --
  a task consuming an asset (`log_summary`) and an ordering-only chain
  (`migrate` then `warm_cache` then `notify`).
