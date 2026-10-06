---
title: Getting Started
description: Install barca and run your first asset in under a minute.
---

## Prerequisites

- Python >= 3.12
- [uv](https://docs.astral.sh/uv/) package manager

## Install

```bash
uv init --app my-project
cd my-project
uv add barca
```

This installs the `@asset()` decorator and the `barca` CLI into your project's virtualenv.

## Hello world

Create `pipeline.py`:

```python
from barca import asset

@asset()
def hello() -> dict:
    return {"message": "Hello from barca!"}
```

Run it:

```bash
uv run barca get pipeline.py
```

In a terminal you'll see a progress line on stderr, then a short summary and the value:

```
[barca] 1/1 steps | done in 0.0s
Run b47d261f7ec0 | all assets in 0.206s (1 step, 1 phase)

Value:
{
  "message": "Hello from barca!"
}
```

Piped or captured by a program (or with `--json`), stdout is one line of structured JSON instead:

```bash
uv run barca get pipeline.py | cat
```

```
{"elapsed_seconds":0.296,"final_output":{"message":"Hello from barca!"},"phases":1,"run_id":"b1b72bc4c9e3","steps":[...],"steps_executed":1}
```

The `@asset()` decorator itself does nothing at runtime -- it's an identity function. Your code runs exactly the same with or without barca installed.

## Add a dependency

Assets depend on other assets via `inputs=`. Barca resolves the DAG and executes them in the right order.

```python
from barca import asset

@asset()
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]

@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "sum": sum(d["x"] for d in data)}
```

```bash
uv run barca get pipeline.py
```

Barca sees that `summary` depends on `raw_data`, plans them as a single sequential chain, executes `raw_data` first, then passes its output to `summary` as the `data` kwarg.

## See the plan

You can see what barca will do without executing anything:

```bash
uv run barca plan pipeline.py
```

```json
{
  "total_steps": 2,
  "phases": [
    {
      "reason": "Initial",
      "streams": [
        {
          "stream_id": "p0-w0",
          "steps": ["pipeline.py:raw_data", "pipeline.py:summary"]
        }
      ]
    }
  ]
}
```

## Next steps

- [Guide](/guide/) -- full tutorial covering tasks, sensors, partitions, parallel dispatch, freshness markers, and multi-file pipelines
- [Scheduling](/scheduling/) -- run a script on a cron schedule (e.g. every 10 minutes) with `barca serve`
- [Patterns: Asset to Task](/patterns/02-asset-to-task/) -- common patterns and anti-patterns
- [CLI Reference](/reference/cli/) -- all CLI commands
