---
title: Error Handling
description: Declare retries and a timeout on the decorator. A step that still fails stops the steps that depend on it and the command exits 1.
---

Use `retries=`, `retry_backoff=` and `timeout_seconds=` on `@asset` or `@task` when a step
can fail for reasons that pass: a network call, a rate limit, a lock. The coordinator runs
the retry loop; the function does not need one.

| Option | Meaning | Default |
|---|---|---|
| `retries=` | Total attempts. `retries=3` is one attempt and up to two more. | 1 (no retry) |
| `retry_backoff=` | Base delay in seconds. The delay before attempt N is `retry_backoff * (N - 1)`. | 0 |
| `timeout_seconds=` | Time limit for each attempt. | 300 |

## Example

This asset fails twice and succeeds on the third attempt. It appends a line to a file on
each attempt so the attempts can be seen afterwards.

```python
import os
import time
from pathlib import Path

from barca import asset, task


@asset(retries=3, retry_backoff=1.0)
def flaky() -> dict:
    log = Path("attempts.txt")
    n = len(log.read_text().splitlines()) + 1 if log.exists() else 1
    with log.open("a") as f:
        f.write(f"attempt {n} at {time.time():.1f} pid {os.getpid()}\n")
    if n < 3:
        raise RuntimeError(f"attempt {n} failed")
    return {"attempts": n}


@task(inputs={"data": flaky})
def notify(data: dict) -> dict:
    return {"notified_after_attempts": data["attempts"]}
```

```bash
barca run notify pipeline.py
cat attempts.txt
```

## What barca does

```
[barca] 2/2 steps | done in 0.1s
Run 51ec7879e318 | ran 'notify' in 3.346s (2 steps, 1 phase)

Value:
{
  "notified_after_attempts": 3
}
```

```
attempt 1 at 1791396830.7 pid 17784
attempt 2 at 1791396831.7 pid 17995
attempt 3 at 1791396833.8 pid 18365
```

The second attempt starts one second after the first and the third two seconds after the
second. Each attempt runs in a new worker process (three process ids), so nothing in memory
carries over from a failed attempt. The run reports success and exits 0; the failed attempts
are not shown in the summary.

## When every attempt fails

```python
@asset(retries=2)
def broken() -> dict:
    print("broken runs")
    raise ValueError("always fails")


@task(inputs={"data": broken})
def after_broken(data: dict) -> None:
    print(f"got {data}")
```

```bash
barca run after_broken pipeline.py
```

```
broken runs
broken runs
[barca] 0/2 steps | failed in 0.0s
[barca] run failed: step 'pipeline.py:broken' failed (exit 1)
Worker failed: ValueError: always fails
  File ".../pipeline.py", line 27, in broken
    raise ValueError("always fails")

Fix the error in 'pipeline.py:broken' (see the traceback) and re-run the same command. Steps that succeeded are cached and will not re-run.
```

The command exits 1. `after_broken` does not run. With `--json`, stdout carries the failure
and each step's outcome:

```json
{"status": "failed", "failed_node": "pipeline.py:broken", "error": "ValueError: always fails",
 "steps": [{"id": "pipeline.py:broken", "kind": "asset", "status": "failed", ...},
           {"id": "pipeline.py:after_broken", "kind": "task", "status": "skipped",
            "reason": "upstream_failed", "detail": "a step it depends on failed", ...}],
 "steps_executed": 1, ...}
```

Each step has its own retry setting. A retry of one step never re-runs a step that already
succeeded, and a downstream step's `retries=` does not apply while its upstream is failing,
because the downstream step never starts.

## Timeouts

An attempt that runs longer than `timeout_seconds` fails like any other error and is retried
if attempts remain. With `@asset(timeout_seconds=1)` on a function that sleeps for five
seconds, the command fails after about one second:

```
Worker failed: TimeoutError: Function '<lambda>' exceeded timeout of 1s
```

## Limits

- **A failed attempt may have had effects.** A retry runs the whole function again. Make a
  retried step safe to repeat.
- **An exception the function catches is not a failure.** A body that catches everything and
  returns normally is recorded as a success and is not retried. Let the exception out of the
  function when you want a retry.
- **Barca does not see a loop in the body.** A hand-written retry loop works, but barca sees
  one attempt, and the attempts share one `timeout_seconds`.
- **Branches of `parallel()` are not retried**, whatever their task declares:
  [Parallel Tasks](/patterns/04-parallel-tasks/).
- **A failed asset is not cached.** `barca status` shows it as `never-run` with `failed` under
  WHY, and the next `barca get` tries it again.
- `sys.exit()` in a step, with any code, fails the step (`barca docs tasks`).
