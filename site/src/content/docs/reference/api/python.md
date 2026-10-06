---
title: Python API
description: barca.get, run, plan, history and stats - call the CLI from Python and get values back.
---

`import barca` also gives you functions that call the `barca` binary and return Python values.
They need the binary on `PATH` (a `uv add barca` install provides it) and raise
`barca.BarcaError` when a step fails or barca cannot run.

```python
import barca

# Every asset and sensor in a file; returns the last asset's value. Tasks are not run.
value = barca.get("pipeline.py")                  # {"count": 3, "total": 6}

# One asset (cache-aware): target first, then files
value = barca.get("summary", "pipeline.py")       # {"count": 3, "total": 6}

# A task and its dependency cone; the task always re-runs
barca.run("publish", "pipeline.py")               # the task's return value (None here)

plan = barca.plan("pipeline.py")                  # dict: total_steps, phases
print(plan["total_steps"])                        # 2

barca.history(limit=5)                            # list of run dicts, newest first
barca.stats("summary", "pipeline.py")             # dict: cache_hit_rate, p95_elapsed_seconds, ...
```

`get` and `run` take the same refresh controls as the CLI:

| Argument | CLI flag |
|---|---|
| `refresh=["a", "b"]` | `--refresh a,b` |
| `cascade=False` | `--no-cascade` |
| `refresh_all=True` | `--refresh-all` |

`get(..., no_cache=True)` is the deprecated spelling of `refresh_all=True` and emits a
`DeprecationWarning`.

Values of every format come back loaded: json as dicts and lists, parquet as DataFrames,
pickles unpickled. The CLI's JSON holds only a pointer for parquet and pickle outputs; the
functions read the artifact for you.

`barca.Client` is a separate client for a running [`barca serve`](/reference/server-api/).
