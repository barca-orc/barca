---
title: "Barca vs Prefect"
description: Install size, steps to a first result, run output, error output and caching for the same pipeline, measured on 2026-06-10 with barca 0.2.0 and Prefect 3.7.4.
---

Last measured: 2026-06-10, with barca 0.2.0 and Prefect 3.7.4, on macOS (Apple Silicon), Python 3.14, both installed from PyPI with `uv`. Not re-run since. Re-run tracked in [#277](https://github.com/barca-orc/barca/issues/277).

Everything on this page describes those two versions on that date unless a note says otherwise.
Prefect has had releases since, and so has barca: where barca's behavior has changed, a note
dated 2026-10-07 says how.

## Install footprint

| Metric | barca 0.2.0 | Prefect 3.7.4 |
|--------|-------|---------|
| Packages installed | 1 | 104 |
| Venv size | 16 MB | 163 MB |
| Import time | 22ms | 400ms |

The barca wheel declared no dependencies. Installing Prefect installed 104 packages, among them
fastapi, sqlalchemy, pydantic, cryptography, docker, redis, opentelemetry and graphviz.

Note, 2026-10-07: the barca wheel now also embeds the web UI that `barca serve` shows. barca
0.18.0 is still one package with no dependencies; a fresh Python 3.12 virtualenv with only barca
installed is 21 MB (`du -sh`). The Prefect column and the import times were not re-measured.

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
from prefect import flow

@flow
def hello():
    return {"message": "Hello!"}

if __name__ == "__main__":
    print(hello())
```

```bash
python pipeline.py
```

Prefect needs no separate command: the flow runs with `python pipeline.py`. In the test each run
started a temporary HTTP server first, and the run took about 6 seconds in total, of which 2 to 3
seconds passed before any user code executed.

## Running the same pipeline

The pipeline is two steps: one returns three rows, the other counts and sums them.

### Barca 0.2.0: 240ms, 2 output lines

```
[barca] 2/2 steps done in 0.0s
{"elapsed_seconds":0.241,"final_output":{"count":3,"total":6},...}
```

Note, 2026-10-07: barca 0.18.0 prints more keys in this JSON object (`status`, a `steps` array,
`warnings`). The current shape is in the [CLI contract](/reference/cli-contract/).

### Prefect 3.7.4: 5.5 seconds, 7 log lines and the result

```
21:00:00 | INFO | prefect - Starting temporary server on http://127.0.0.1:8966
See https://docs.prefect.io/... for more information on running a dedicated Prefect server.
21:00:03 | INFO | Flow run 'illustrious-moth' - Beginning flow run ...
21:00:03 | INFO | Task run 'raw_data-d46' - Finished in state Completed()
21:00:03 | INFO | Task run 'summary-eed' - Finished in state Completed()
21:00:04 | INFO | Flow run 'illustrious-moth' - Finished in state Completed()
{'count': 3, 'total': 6}
21:00:04 | INFO | prefect - Stopping temporary server on http://127.0.0.1:8966
```

The log shows 3 seconds between "Starting temporary server" and the first task. Each time on
this page is one run, not an average.

## Error output

The same step raising `ValueError("something went wrong")`.

### Barca 0.2.0: 2 lines

```
[barca] 0/1 steps done in 0.0s
Worker failed: something went wrong
```

Note, 2026-10-07: barca 0.18.0 prints more than this: a JSON result with `"status": "failed"`
on stdout, and on stderr a `[barca] run failed: ...` line and one JSON line with the error, a
remediation and the traceback frames from the user's file. See the
[CLI contract](/reference/cli-contract/).

### Prefect 3.7.4: about 75 lines

The error was printed three times:

1. a task-level ERROR with a traceback (`task_engine.py`, `run_context`, `call_task_fn`, ...);
2. a flow-level ERROR with a traceback (`flow_engine.py`, `run_context`, `call_flow_fn`, ...);
3. the unhandled exception with a traceback (`flows.py`, `run_flow`, ...).

Each traceback had more than ten Prefect frames, with the frame from the user's file
(`broken.py:6`) six frames down.

## Caching

Barca caches every asset by its run hash, a hash of the function's code and its inputs, with no
option to set. A second run of the same pipeline on barca 0.2.0 finished in 2ms and reported
`steps_executed: 0`. `barca docs cache` says what the hash covers.

Prefect has `cache_policy=INPUTS`. In the test it did not carry a result from one
`python pipeline.py` run to the next under the temporary server: every run executed every task.
A persistent `prefect server start` instance was not tested.

## Other things observed in the same session

These are notes from using Prefect 3.7.4 on 2026-06-10. They have not been checked against a
later Prefect release.

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
  which took 2 to 3 seconds. A flow computing `1 + 2` took 3.5 seconds.
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
