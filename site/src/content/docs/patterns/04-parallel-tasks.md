---
title: "Pattern: Parallel Tasks"
description: Inside a task, parallel() and parallel_map() run other tasks in separate worker processes and return their results in order.
---

Use this when a task decides at run time what work to fan out: one deploy per region, one
upload per file. `parallel()` and `parallel_map()` run other `@task` functions in separate
worker processes and return the results in argument order.

To fan an asset out over a fixed set of keys, with each key cached, use partitions instead:
[Parametrized Assets and Partitions](/workflows/03-parametrized-assets-and-partitions/).

## Example

```python
import time
from functools import partial

from barca import ParallelError, asset, parallel, task


@asset()
def model() -> dict:
    return {"version": 3}


@task()
def deploy(region: str, version: int) -> dict:
    time.sleep(1)
    if region == "ap-southeast-1":
        raise RuntimeError("region unavailable")
    return {"region": region, "version": version}


@task(inputs={"m": model})
def deploy_all(m: dict) -> dict:
    regions = ["us-east-1", "eu-west-1", "ap-southeast-1"]
    results = parallel(*(partial(deploy, r, m["version"]) for r in regions))
    out = {}
    for region, result in zip(regions, results):
        if isinstance(result, ParallelError):
            out[region] = f"failed: {result.error}"
        else:
            out[region] = "ok"
    return out
```

```bash
barca run deploy_all pipeline.py
```

## What barca does

Each argument to `parallel()` is a `functools.partial` around a `@task` function. The
coordinator suspends the calling worker, runs the branches on other workers, and resumes the
caller with the results. The three one-second branches finish together; the failed branch
comes back as a `ParallelError` and the run still succeeds (exit 0), because `deploy_all`
did not raise:

```
[barca] 4/4 steps | done in 4.4s
Run 51e6126313d8 | ran 'deploy_all' in 2.522s (2 steps, 2 phases)

Value:
{
  "ap-southeast-1": "failed: RuntimeError: region unavailable\n  File \".../pipeline.py\", line 17, in deploy\n    raise RuntimeError(\"region unavailable\")",
  "eu-west-1": "ok",
  "us-east-1": "ok"
}
```

`ParallelError.error` is a string: the exception type, its message and the traceback lines.

`parallel_map(fn, items, **kwargs)` is the same as
`parallel(*(partial(fn, item, **kwargs) for item in items))`:

```python
from barca import parallel_map

results = parallel_map(deploy, ["us-east-1", "eu-west-1"], version=m["version"])
```

## Limits

- **A failed branch does not raise in the caller.** `try`/`except` around `parallel()` catches
  nothing. Check each result with `isinstance(result, ParallelError)`, and raise yourself if a
  failed branch should fail the run.
- **Arguments must be partials.** A bare function, or a call such as `deploy("us-east-1", 1)`
  (which runs in the caller and passes its return value), fails the step:

  ```
  Worker failed: TypeError: parallel() expects functools.partial objects, got function
  ```

- **Branches are not retried.** A branch runs once, whatever `retries=` its task declares
  (checked on 0.18.0 with a branch declared `retries=2`: one attempt).
- **Branches are not cached.** They are tasks, and tasks always run.
- **Use it in tasks.** On 0.18.0 a call from an `@asset` body also ran its branches, but the
  asset is then cached like any other asset and the branches do not run again until its code
  changes. If the fan-out should happen on every run, it belongs in a task.
- **Arguments and results must be JSON values** (dict, list, str, number, bool, `None`).
  Observed on 0.18.0: an argument that is not JSON-serializable, such as a `datetime.date`,
  fails the calling step with `TypeError: Object of type date is not JSON serializable`. A
  branch that returns a value that is not JSON-serializable (a set, a date, a DataFrame) does
  not fail: the caller receives `None` in its place. Pass and return plain values, or have the
  branch write its data somewhere and return the location.
