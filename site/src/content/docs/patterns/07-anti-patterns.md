---
title: Anti-Patterns
description: Common mistakes to avoid when using barca, why they fail, and what to do instead.
---

Things to avoid when using barca. Each section describes a mistake, why it fails, and what to do instead.

## Task as input to an asset

```python
from barca import asset, task

@task()
def deploy_model() -> dict:
    return upload(model)

# Wrong -- DAG validation rejects this.
@asset(inputs={"d": deploy_model})
def deployment_report(d: dict) -> dict:
    return {"status": d["status"]}
```

**Why it fails.** Tasks always re-run. An asset depending on a task would be perpetually stale -- every plan would need to re-materialize the asset because its input is never cached. Barca rejects this edge during DAG validation with a clear error.

**What to do instead.** If the upstream produces cacheable data, make it an `@asset`. If it is a side-effect, keep it as a `@task` but do not feed its output into an asset. Wire the asset's data needs through other assets.

## Calling asset functions directly

```python
@asset()
def trained_model() -> dict:
    return train(data)

# Bypasses barca entirely.
result = trained_model()
```

**Why it is a problem.** The function works -- `@asset` is a no-op decorator, so calling it directly returns the value. But you get no caching, no hashing, no provenance tracking, and no artifact storage. The call is invisible to barca.

**What to do instead.** Use `barca get` from the CLI or the Python API to materialize the asset through barca. This gives you caching and tracking.

```bash
barca get trained_model pipeline.py
```

## Mutating asset inputs in place

```python
@asset(inputs={"data": raw_data})
def processed(data: dict) -> dict:
    data["new_field"] = compute()  # mutating the input
    return data
```

**Why it is a problem.** Each worker process deserializes its own copy from the artifact file, so mutating it never corrupts the artifact on disk. A worker also keeps small artifacts (up to 8 MB on disk) in memory for the steps it runs next, and each step gets its own copy of the cached value, not the object another step produced or received, so an in-place edit normally stays inside the step that made it. That copy has gaps. A pandas DataFrame nested inside a dict or list, or a pandas Series, shares the lists and dicts held in its object columns with the cached value, and a polars DataFrame built over a numpy array still follows that array. Mutating one of those in place can change what a later step in the same worker receives. Even where the copy is complete, the code is misleading: it looks like it modifies shared state, so readers will assume `raw_data` is affected.

**What to do instead.** Create a new value and return it:

```python
@asset(inputs={"data": raw_data})
def processed(data: dict) -> dict:
    return {**data, "new_field": compute()}
```

## Calling the barca CLI from inside a worker

```python
import subprocess

@task()
def orchestrate() -> None:
    subprocess.run(["barca", "get", "some_asset", "pipeline.py"])
```

**Why it fails.** This spawns a second barca runtime with its own database connection. The two runtimes do not coordinate -- you get race conditions on the metadata database and duplicated work. The inner run also does not appear in the outer run's execution plan.

**What to do instead.** Structure your work as separate tasks wired through `inputs=`. If you need to trigger a barca run from external code, do it outside the worker process (e.g., from a CI script or a cron job).

## Sharing external state between parallel tasks

```python
@task()
def write_to_s3() -> None:
    s3.put_object(Bucket="shared", Key="output.json", Body=data)

@task()
def read_from_s3() -> dict:
    return s3.get_object(Bucket="shared", Key="output.json")
```

**Why it is a problem.** Barca parallelizes independent tasks across worker processes. If two tasks read and write the same external resource without a declared dependency, barca cannot protect against race conditions. The execution order is non-deterministic.

**What to do instead.** Either:

1. Wire the dependency explicitly so barca orders them:

```python
@task()
def write_to_s3() -> dict:
    s3.put_object(Bucket="shared", Key="output.json", Body=data)
    return {"key": "output.json"}

@task(inputs={"_write": write_to_s3})
def read_from_s3(_write: dict) -> dict:
    return s3.get_object(Bucket="shared", Key="output.json")
```

2. Combine them into one task if the work is small enough:

```python
@task()
def write_and_read_s3() -> dict:
    s3.put_object(Bucket="shared", Key="output.json", Body=data)
    return s3.get_object(Bucket="shared", Key="output.json")
```
