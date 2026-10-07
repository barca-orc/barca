---
title: Conditional Execution
description: Barca has no conditional construct. Branch inside the function body, or raise in a gate step to stop what depends on it.
---

Use this when what a step does depends on the data. Barca has no conditional construct: the
graph is read from the source before anything runs and has the same shape on every run. A
condition is an `if` inside a function body, or a step that raises so that the steps after
it do not run.

## Example: branch inside the body

```python
from barca import asset, task


@asset()
def validation() -> dict:
    rows = [{"id": 1, "amount": 9.5}, {"id": 2, "amount": -1.0}]
    bad = [r["id"] for r in rows if r["amount"] < 0]
    return {"passed": not bad, "bad_ids": bad}


@task(inputs={"result": validation})
def publish(result: dict) -> dict:
    if result["passed"]:
        return {"action": "published"}
    return {"action": "held back", "bad_ids": result["bad_ids"]}
```

```bash
barca run publish pipeline.py
```

```
[barca] 2/2 steps | done in 0.0s
Run 51f18c924f70 | ran 'publish' in 0.089s (2 steps, 2 phases)

Value:
{
  "action": "held back",
  "bad_ids": [
    2
  ]
}
```

Both steps run and the run succeeds. The branch taken is visible only in what the task
returns or prints.

## Example: a gate that stops the run

To make a failed check stop everything after it and fail the command, raise in a step and
make the later steps depend on it. The `_` prefix makes the dependency ordering-only
([Ordering-Only Dependencies](/patterns/03-ordering-only-deps/)).

```python
@task(inputs={"result": validation})
def gate(result: dict) -> None:
    if not result["passed"]:
        raise ValueError(f"validation failed for ids {result['bad_ids']}")


@task(inputs={"_gate": gate})
def publish_gated(_gate) -> dict:
    return {"action": "published"}
```

```bash
barca run publish_gated pipeline.py
```

```
[barca] 0/3 steps | failed in 0.0s
[barca] run failed: step 'pipeline.py:gate' failed (exit 1)
Worker failed: ValueError: validation failed for ids [2]
  File ".../pipeline.py", line 21, in gate
    raise ValueError(f"validation failed for ids {result['bad_ids']}")
```

`publish_gated` does not run and the command exits 1.

## Limits

- **A step cannot be skipped from the outside.** Every step in the target's cone either runs,
  is served from cache, or is skipped because a step it depends on failed. A step that decides
  to do nothing still runs and still counts as a success.
- **Decorators take no condition.** There is no `when=` argument. On 0.18.0
  `@task(when=lambda: ...)` is not an error: the keyword is ignored and the task runs.
- **A gate that raises fails the run.** That is the intent of a gate, but it means the exit
  code is 1 and steps downstream are reported as skipped. If "nothing to do" is a normal
  outcome, branch in the body and return a value that says so.
