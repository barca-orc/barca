---
title: "Pattern: Asset-to-Task"
description: An asset produces a cached value; a task reads it and does something with it. The task runs every time.
---

Use this when a cached value feeds an action: a deploy, an upload, a notification. The
asset is computed when its code or inputs change. The task runs every time you ask for it.

## Example

```python
from barca import asset, task


@asset()
def model() -> dict:
    return {"version": 3, "auc": 0.91}


@task(inputs={"m": model})
def deploy(m: dict) -> dict:
    print(f"deploying model v{m['version']}")
    return {"deployed": m["version"]}
```

A task is run with `barca run`, target first, then the file:

```bash
barca run deploy pipeline.py
```

## What barca does

First run: two steps, `model` and then `deploy`. The task's `print` goes to stderr.

```
deploying model v3
[barca] 2/2 steps | done in 0.0s
Run 51e3fed40948 | ran 'deploy' in 0.048s (2 steps, 1 phase)

Value:
{
  "deployed": 3
}
```

Second run: one step. `model` is served from cache and `deploy` runs again.

```
deploying model v3
[barca] 1/2 steps | done in 0.0s
Run 51e3fed84870 | ran 'deploy' in 0.045s (1 step, 1 phase)
```

`--dry-run` shows the same decision without running anything:

```bash
barca run deploy pipeline.py --dry-run
```

```
Dry run: barca run deploy (nothing executed, nothing written)

STATUS    WHY                  STEP
cached    -                    pipeline.py:model
will run  tasks always re-run  pipeline.py:deploy

1 will run, 1 cached, 0 unknown
```

To recompute the asset as well, name it: `barca run deploy pipeline.py --refresh model`
(two steps again). `--refresh-all` recomputes every upstream asset.

## Limits

- **`get` is for assets and `run` is for tasks.** `barca get deploy pipeline.py` exits 2 with
  ``'deploy' is a task — use `barca run` instead``, and `barca run model pipeline.py` exits 2
  with ``'model' is an asset — use `barca get` instead``.
- **`barca get pipeline.py` with no target never runs a task.** It materializes the assets and
  sensors and prints the tasks it skipped on stderr:

  ```
  [barca] skipped 1 task (deploy): `barca get` without a target materializes assets only. Run a task with: barca run deploy pipeline.py
  ```

- **A task cannot be an input to an asset or a sensor.** A task's result is never cached, so
  barca rejects the edge when it builds the graph (exit 2):

  ```
  DAG error: task 'bad.py:migrate_db' cannot be an input to asset 'bad.py:user_counts' (tasks are never cached, so this would poison caching)
  ```

  A task may be an input to another task; see
  [Ordering-Only Dependencies](/patterns/03-ordering-only-deps/).
- **A task that fails is not retried unless you ask.** See
  [Error Handling](/patterns/06-error-handling/).

The same material is in the terminal manual: `barca docs tasks`.
