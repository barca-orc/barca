---
title: "Workflow: Execution Controls and Ad Hoc Params"
description: Timeouts, retries and cancellation as they work in barca 0.29.1. Ad hoc runtime parameters are not implemented.
---

This page covers timeouts, retries and cancellation. Everything shown was run with barca 0.29.1.

**Ad hoc runtime parameters are not implemented.** An earlier version of this page proposed a
`--param x=7` flag. No such flag exists: `barca get flaky pipeline.py --param x=7` exits 2 with
`unexpected argument '--param' found`. To run one function over a set of values, use
[partitions](/workflows/03-parametrized-assets-and-partitions/). To make a value from outside
part of the cache key, declare an environment variable with `env=[...]` (`barca docs assets`).

## Example

```python
# pipeline.py
import time
from pathlib import Path

from barca import asset


@asset(retries=3, retry_backoff=1.0)
def flaky() -> dict:
    marker = Path("attempts.txt")
    n = int(marker.read_text()) + 1 if marker.exists() else 1
    marker.write_text(str(n))
    if n < 3:
        raise RuntimeError(f"attempt {n} failed")
    return {"attempts": n}


@asset(timeout_seconds=2)
def slow() -> dict:
    time.sleep(30)
    return {"done": True}


@asset(inputs={"s": slow})
def after_slow(s: dict) -> dict:
    return s


@asset()
def long_running() -> dict:
    time.sleep(60)
    return {"done": True}
```

## Retries

`retries=` is the total number of attempts; the default is 1, which means no retry.
`retry_backoff=` is a base delay in seconds; the default is 0. After the Nth failed attempt
barca waits `N × retry_backoff` seconds before the next one. Both are accepted on `@asset`,
`@task` and `@sensor`.

```bash
barca get flaky pipeline.py --agent -o value --json
```

```
[barca] step:pipeline.py:flaky completed 0.0s (1/1) attempts=3
[barca] 1/1 steps | done in 3.1s
{"elapsed_seconds":3.057922537,"final_output":{"attempts":3}, ... "status":"success","steps":[{"attempts":3, ... "status":"ran"}],"steps_executed":1,"warnings":[]}
```

The function ran three times and the third attempt succeeded. The `--agent` line and the
step's `attempts` say how many attempts were made; the step counts once, and its time is the
successful attempt's. With `retries=4`, the measured gaps between the starts of attempts were 1.1 s,
2.2 s and 3.2 s for `retry_backoff=1.0`, and about 0.1 s each for `retry_backoff=0`.

When every attempt fails, the step fails with the last error and the run exits 1:

```
[barca] step:pipeline.py:flaky failed: RuntimeError: attempt 3 failed attempts=3
[barca] 0/1 steps | failed in 3.1s
```

A retry re-runs one step. It does not re-run the step's upstream, and a downstream step's
retries do not re-run this one. See [Error Handling](/patterns/06-error-handling/).

## Timeouts

`timeout_seconds=` is a limit per attempt. The default is 300.

```bash
barca get after_slow pipeline.py --agent
```

```
[barca] step:pipeline.py:slow failed: TimeoutError: Step 'pipeline.py:slow' exceeded timeout of 2s
[barca] 0/2 steps | failed in 2.0s
{"elapsed_seconds":2.048452779,"error":"TimeoutError: Step 'pipeline.py:slow' exceeded timeout of 2s","failed_node":"pipeline.py:slow", ... "status":"failed","steps":[{... "id":"pipeline.py:slow", ... "status":"failed"},{"detail":"a step it depends on failed","id":"pipeline.py:after_slow","kind":"asset","reason":"upstream_failed", ... "status":"skipped"}],"steps_executed":1,"warnings":[]}
{"artifact_dir":".barca/artifacts/pipeline.py--slow","code":1,"error":"step 'pipeline.py:slow' failed: TimeoutError: Step 'pipeline.py:slow' exceeded timeout of 2s","kind":"step_failed", ...}
[barca] run failed: step 'pipeline.py:slow' failed: TimeoutError: Step 'pipeline.py:slow' exceeded timeout of 2s; 1 downstream steps skipped (exit 1)
```

A timeout is a step failure: exit code 1, the step is `failed`, and steps that depend on it are
`skipped` with reason `upstream_failed`. The error and `failed_node` name the step, and the
remediation names `timeout_seconds=`.

## Cancellation

Ctrl-C (SIGINT) cancels a run, and so does SIGTERM. Barca stops its workers and records the run as `cancelled`.

```bash
barca get long_running pipeline.py --agent     # then Ctrl-C after two seconds
```

```
[barca] 0/1 steps | cancelled after 2.0s
{"code":130,"error":"run cancelled","kind":"cancelled","remediation":"Re-run the same command; steps that finished before the cancel are cached and will not re-run."}
```

The exit code is 130. The interrupted step leaves no result: steps that finished before the
cancel stay cached, and the next run computes the rest.

Under `barca serve`, `DELETE /run/{run_id}` cancels a run the same way
([Server API](/reference/server-api/)).

## Seeing what happened

```bash
barca history
barca status pipeline.py
```

```
RUN_ID         CMD     TARGET                   STATUS      STEPS CACHED   TIME STARTED
------------------------------------------------------------------------------------------------------
d870c314e71c   get     long_running             cancelled       1      0   2.0s 2026-10-21 21:07:56 UTC
d8738262e1cc   get     after_slow               failed          1      0   2.0s 2026-10-21 21:07:54 UTC
d8772824e362   get     flaky                    success         1      0   3.1s 2026-10-21 21:07:50 UTC
```

```
NAME          KIND   STATE      WHY           LAST RUN                               SHAPE         DEPS
long_running  asset  never-run  no_record     -                                      -             -
slow          asset  never-run  failed        failed 2026-10-21 21:07:56 UTC         -             -
after_slow    asset  never-run  no_record     -                                      -             slow
flaky         asset  cached     materialized  success 2026-10-21 21:07:54 UTC 0.00s  dict (1 key)  -

1 cached, 0 stale, 3 never run, 0 partial, 0 unknown, 0 always run
```

Run statuses in `barca history` are `success`, `failed`, `cancelled`, `running` and
`interrupted` (the process was killed; `barca docs cache`). There is no separate `timed_out`
status. The cancelled run above is listed with 1 step although its step did not finish.

## Limits

- Retry attempts are not visible in the output or in `barca history`.
- There is no terminal UI for watching or cancelling a run. `barca status` from a second
  terminal shows the steps a running command has finished; the web UI and the cancel endpoint
  need `barca serve`.
