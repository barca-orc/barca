---
title: "Barca vs Prefect"
description: Install size, import time, steps to a first result, run time, run output, error output and caching for the same pipeline, measured on 2026-10-21 with barca 0.28.0 and Prefect 3.9.0.
---

Last measured: 2026-10-21, with barca 0.28.0 and Prefect 3.9.0, both from PyPI with `uv`,
Python 3.13, on Linux (AMD Ryzen AI MAX+ 395). Setup and commands are in
[`benchmarks/RESULTS.md`](https://github.com/barca-orc/barca/blob/main/benchmarks/RESULTS.md).
It replaces the first comparison, made on 2026-06-10 with barca 0.2.0 and Prefect 3.7.4 on
macOS.

## Install footprint

| Metric | barca 0.28.0 | Prefect 3.9.0 |
|--------|-------|---------|
| Packages installed | 1 | 103 |
| Venv size (`du -sm`) | 31 MB | 147 MB |
| Lockfile entries | 2 | 106 |
| Import time (`python -c "import X"`) | 42 ms | 502 ms |

Each column is `uv pip install X` into an empty venv. Import times are medians of 20 runs;
`python -c pass` took 13 ms. The barca wheel contains the Rust binary, the web UI that
`barca serve` shows and the Python decorator stubs, and declares no dependencies.

## Steps to first output

### Barca

```bash
uv add barca
# write pipeline.py
barca get pipeline.py
```

### Prefect

```python
# pipeline.py
from prefect import flow, task

@task
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]

@task
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "total": sum(d["x"] for d in data)}

@flow
def pipeline():
    return summary(raw_data())

if __name__ == "__main__":
    print(pipeline())
```

```bash
python pipeline.py
```

Prefect needs no separate command: the flow runs with `python pipeline.py`. Each run starts a
temporary local server first; in the logged run below, 1.6 seconds passed between "Starting
temporary server" and the start of the flow.

## Running the same pipeline

The pipeline is two steps: one returns three rows, the other counts and sums them.

| Metric | barca 0.28.0 | Prefect 3.9.0 |
|--------|-------|---------|
| Command | `barca get pipeline.py` | `python pipeline.py` |
| Time, nothing cached | 73 ms (67–76) | 4.59 s (4.37–4.91) |
| Output | 1 JSON line on stdout, 2 progress lines on stderr | the result on stdout, 7 log lines on stderr |
| Error output | 1 JSON line on stdout, 4 lines on stderr | 111 lines on stderr |

Times are medians of 10 runs, the two tools taking turns run by run; the range is the fastest
and slowest run. barca's `.barca/` was deleted before each run.

### Barca 0.28.0

```
[barca] importing pipeline.py
[barca] 2/2 steps | done in 0.0s
{"elapsed_seconds":0.0609,"final_output":{"_barca_artifact":{"format":"json","path":".barca/artifacts/pipeline.py--summary/2d10c4….json","size_bytes":24}},"phases":1,"run_id":"…","status":"success","steps":[…],"steps_executed":2,"warnings":[]}
```

`barca get pipeline.py -o value` prints the value itself. The JSON shape is in the
[CLI contract](/reference/cli-contract/).

### Prefect 3.9.0

```
03:53:34.483 | INFO    | prefect - Starting temporary server on http://127.0.0.1:8235
See https://docs.prefect.io/v3/concepts/server#how-to-guides for more information on running a dedicated Prefect server.
03:53:36.095 | INFO    | Flow run 'fantastic-clam' - Beginning flow run 'fantastic-clam' for flow 'pipeline'
03:53:36.109 | INFO    | Task run 'raw_data-c2b' - Finished in state Completed()
03:53:36.111 | INFO    | Task run 'summary-72d' - Finished in state Completed()
03:53:37.106 | INFO    | Flow run 'fantastic-clam' - Finished in state Completed()
{'count': 3, 'total': 6}
03:53:37.121 | INFO    | prefect - Stopping temporary server on http://127.0.0.1:8235
```

## Error output

The same step raising `ValueError("something went wrong")`.

### Barca 0.28.0

Exit code 1. On stdout, one JSON object with `"status": "failed"`, `"error": "ValueError:
something went wrong"` and `"failed_node"`. On stderr, a progress line, one JSON line with the
error, a remediation and the traceback frames from the user's file, and a `[barca] run failed:
...` line. See the [CLI contract](/reference/cli-contract/).

### Prefect 3.9.0: 111 lines

Exit code 1. The error was printed three times:

1. a task-level ERROR with a traceback (`task_engine.py`, `call_task_fn`, ...);
2. a flow-level ERROR with a traceback;
3. the unhandled exception with a traceback, starting at `broken.py`, line 15.

The frame from the user's file (`broken.py`, line 6) comes after the Prefect frames in each.

## Caching

Barca caches every asset by its run hash, a hash of the function's code and its inputs, with no
option to set. A second run of the same pipeline on barca 0.28.0 took 13 ms (median of 10) and
ran no step. `barca docs cache` says what the hash covers.

Prefect has `cache_policy=INPUTS`. With both tasks set to it and two `python pipeline.py` runs
on Prefect 3.9.0 under the temporary server, the second run reported `summary` as
`Cached(type=COMPLETED)` and ran `raw_data`, which takes no inputs, again. On 2026-06-10, with
Prefect 3.7.4, neither task was cached on the second run.

## Other things observed on 2026-06-10

These are notes from using Prefect 3.7.4 on 2026-06-10. They have not been checked against a
later Prefect release, except where a note dated 2026-10-21 says so.

- `process_customer.map(customer_ids)` fans one task out over a list in one call. barca's
  equivalents are `parallel_map` inside a task and `partitions` on an asset
  ([Parallel tasks](/patterns/04-parallel-tasks/)).
- Flow runs get generated names such as "tricky-kiwi". barca run ids are hexadecimal strings.
- `prefect flow-run ls` listed past runs with their status across separate invocations.
  barca's equivalent is `barca history`.
- `main.serve(name="my-deployment", cron="0 8 * * *")` turned a flow into a scheduled
  long-running process. barca's equivalent is `Schedule("0 8 * * *")` on the function and
  `barca serve` ([Scheduling](/scheduling/)).
- Every `python file.py` run and every `prefect` CLI command started a temporary HTTP server,
  which took 2 to 3 seconds. A flow computing `1 + 2` took 3.5 seconds. Note, 2026-10-21: with
  Prefect 3.9.0 on the Linux machine, the two-task pipeline above took 4.6 seconds and the
  server start 1.6 seconds of it.
- `prefect --help` listed 31 subcommands. The names recorded were: api, artifact, automation,
  block, cloud, concurrency-limit, config, dashboard, deploy, deployment, dev, events,
  experimental, flow, flow-run, global-concurrency-limit, init, plugins, profile, sdk, server,
  shell, task, task-run, transfer, variable, version, work-pool, work-queue, worker.
- `prefect init --help` answered "Unknown option: --help. Did you mean --field?".
- Every run printed the server start and stop lines, the flow run line and one line per task.
  No option to suppress them was found during the test.
- `@flow` and `@task` wrap the function, and the original is reached through `.fn`. barca's
  decorators return the function unchanged, so a decorated function can be called as plain
  Python; barca then does no caching and records nothing.

## Why barca has no `python file.py` mode

In barca the Rust binary does the parsing, planning, caching and persistence, so a pipeline is
run through it: `barca get` on the command line, or `barca.get(...)` from Python, which starts
the same binary. Running `python pipeline.py` executes the functions as ordinary Python with
none of that.
