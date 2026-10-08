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

- **A branch that raises does not raise in the caller.** `try`/`except` around `parallel()`
  does not catch it. Check each result with `isinstance(result, ParallelError)`, and raise
  yourself if a failed branch should fail the run.
- **Arguments must be partials.** A bare function, or a call such as `deploy("us-east-1", 1)`
  (which runs in the caller and passes its return value), fails the step:

  ```
  Worker failed: TypeError: parallel() expects functools.partial objects, got function
  ```

- **Branches are not retried.** A branch runs once, whatever `retries=` its task declares
  (checked on 0.18.0 with a branch declared `retries=2`: one attempt).
- **Branches are not cached.** They are tasks, and tasks always run.
- **Use it in tasks.** A call from an `@asset` body also runs its branches, but the asset is
  then cached like any other asset and the branches do not run again until the asset does. If
  the fan-out should happen on every run, it belongs in a task.
- **A branch may call `parallel()` itself.** Until 0.18.1 that could hang the run.
- **Arguments must be JSON values** (dict, list, str, number, bool, `None`). They are sent to
  the branch as JSON: a tuple arrives as a list, and a set fails the calling step with a
  `TypeError`. So does a `datetime.date` (`TypeError: Object of type date is not JSON
  serializable`), unless the `fast` extra (orjson) is installed, in which case the branch
  receives its ISO string. Convert such values yourself (`d.isoformat()`) and the branch gets
  the same thing either way.

## What a branch may return

Anything a step may return. The branch's worker writes its return value as an artifact, in the
format barca picks for any step output (json, pickle or parquet, by type), and the calling step
reads it back:

| The branch returns | The caller receives |
|---|---|
| a set, a frozenset, a `date` or `datetime`, a `Decimal`, `bytes`, a dataclass, an object of your own class, a numpy array, or a container holding any of these | an equal value of the same type |
| a pandas or polars DataFrame, a polars LazyFrame, a pyarrow Table, a DuckDB relation | a frame of the type the branch returned |
| a value JSON can represent | what JSON gives back, as between steps: a tuple as a list, a dict's non-string keys as strings (`{1: "a"}` as `{"1": "a"}`) |
| `None` | `None` |
| nothing, because it raised | a `ParallelError` |
| a value that cannot be written or read back (an open file, a lambda, a generator) | nothing: `parallel()` raises `BranchResultError`, the calling step fails and the run exits 1 |

The error names the branch, the type and the reason:

```
BranchResultError: parallel() branch 1: pipeline.py:handle returned a _io.TextIOWrapper, which
cannot be passed back to the step that called parallel(): TypeError: cannot pickle
'TextIOWrapper' instances.
```

Until 0.18.1 only JSON values came back. Any other return value (a set, a date, a DataFrame)
reached the caller as `None`, with no error or warning, and the run succeeded.

A large result is not sent between processes: the caller reads it from the branch's artifact
file. Branch artifacts are local files in the artifact directory, named after the branch's
file, its function and a number; later runs overwrite them. They are not uploaded to an
artifact store, with or without one configured.
