---
title: "Pattern: Asset-to-Task"
description: An asset produces cached data; a task consumes it to perform a side effect.
---

An asset produces cached data. A task consumes that data to perform a side-effect. Use `barca get` for the asset, `barca run` for the task.

## The right way

```python
from barca import asset, task

@asset
def trained_model():
    return train(load_data("training_set.csv"))

@task(inputs={"model": trained_model})
def deploy(model):
    upload_to_s3(model, bucket="models", key="latest.pkl")
    notify_slack("Model deployed")
```

```bash
# Materialize the asset (cached)
barca get trained_model pipeline.py

# Execute the task (always runs)
barca run deploy pipeline.py
```

## Why this works

- **Assets are cached data.** `trained_model` only re-runs when its code or upstream inputs change. Expensive computation is done once and reused.
- **Tasks are side-effects.** `deploy` always re-runs because side-effects (uploading, notifying) are not idempotent by default. Barca treats tasks as "always stale."
- **Clean separation.** The DAG edge from asset to task means the task is guaranteed to see a fully materialized, up-to-date model before it executes.

## Common mistakes

### Making a task an input to an asset

```python
# Wrong -- DAG validation will reject this
@task
def migrate_db():
    run_migrations()

@asset(inputs={"_done": migrate_db})
def user_counts():
    return query("SELECT count(*) FROM users")
```

Tasks always re-run, so any asset downstream of a task would have its cache perpetually invalidated. Barca's DAG validation rejects task-to-asset edges to prevent this. If you need ordering without data, see [Ordering-Only Dependencies](/patterns/03-ordering-only-deps/) and use a task-to-task edge or an asset-to-asset edge instead.

### Using `barca get` on a task

```bash
# Wrong -- tasks are not cacheable artifacts
barca get deploy pipeline.py
# Error: 'deploy' is a task -- use `barca run` instead
```

`barca get` targets assets; naming a task explicitly is rejected with a clear error pointing you to `barca run`. Running `barca get pipeline.py` on a whole file (no target) materializes every asset and sensor and skips tasks, so no side effect fires; stderr names the skipped tasks and the `barca run` command. (Previously a bare `barca get` ran tasks too.) A task runs only when you name it with `barca run`.
