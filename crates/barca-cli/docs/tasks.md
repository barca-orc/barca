# Tasks and `barca run`

A task is a workflow step that *does* something (deploy, notify, migrate, warm a cache). Tasks
always re-run and are never cached.

```python
from barca import asset, task


@asset()
def report() -> dict:
    return {"rows": 42}


@task(inputs={"data": report})            # asset -> task: receives the report
def send_email(data: dict) -> None:
    print(f"sending report with {data['rows']} rows")


@task()
def migrate() -> None:
    print("migrating")


@task(inputs={"_migrate": migrate})       # leading "_": ordering only, receives None
def notify(_migrate) -> None:
    print("migration done")
```

## Rules

- A task may depend on assets, sensors or other tasks, and may sit anywhere in the graph.
- A task must **not** be an input to an asset or sensor (its output is never cached, so a
  cacheable node downstream of it would be permanently stale).
- An `inputs` key starting with `_` means "run after, but do not load the data": the
  parameter receives `None` and no artifact is deserialized.
- `@task(freshness=Schedule("<cron>"))` runs on a timer under `barca serve`.

## Running tasks

```bash
barca run send_email pipeline.py                      # task runs; upstream assets come from cache
barca run send_email pipeline.py --refresh report     # re-materialize report and what is downstream of it
barca run send_email pipeline.py --refresh report --no-cascade   # re-materialize only report
barca run send_email pipeline.py --refresh-all        # re-materialize every upstream asset
```

`barca run` is cache-aware for upstream assets, exactly like `barca get`; only the task always
executes. A task must be the target: `barca get` on a task is an error, and `barca run` on an
asset is an error.

`barca get pipeline.py` (no target) never runs tasks: it materializes every asset and sensor,
and prints the skipped tasks and the `barca run` command on stderr. A deploy or notify task fires
only when you name it with `barca run`. (Previously a bare `barca get` ran tasks too.)

## Several tasks in one run

Name several tasks as one comma-separated list (no spaces). A validation sweep is the typical
use: each check is its own task, and one invocation runs them all.

```python
from barca import asset, task


@asset()
def registry() -> dict:
    return {"models": ["churn", "ltv"]}


@task(inputs={"reg": registry})
def validate_registry(reg: dict) -> dict:
    assert reg["models"], "registry is empty"
    return {"models": len(reg["models"])}


@task(inputs={"reg": registry})
def validate_names(reg: dict) -> dict:
    return {"lowercase": all(m == m.lower() for m in reg["models"])}
```

```bash
barca run validate_registry,validate_names pipeline.py             # both checks; registry runs once
barca run validate_registry,validate_names pipeline.py --dry-run   # preview the union of both cones
```

- The union of the targets' cones is planned once: `registry` materializes once (3 steps), and a
  second run serves it from cache (2 steps, the tasks).
- Every target runs even if another fails. A failure skips only the steps that depend on it
  (reported with `"status": "skipped"`, reason `upstream_failed`); the exit code is 1 if any
  target failed.
- The JSON output replaces `final_output` with `targets`, keyed by target:

```json
{"run_id": "...", "steps_executed": 3, "steps": [...],
 "targets": {"validate_registry": {"status": "success", "final_output": {"models": 2}},
             "validate_names": {"status": "success", "final_output": {"lowercase": true}}}}
```

  A failed target is `{"status": "failed", "failed_node": "pipeline.py:...", "error": "..."}`,
  where `failed_node` is the target itself or the upstream step that failed. With one target the
  output is unchanged. `--refresh` names may come from any target's cone. `barca get a,b` works
  the same way for assets.

## Fan-out from inside a task

`parallel(partial(f, x), ...)` and `parallel_map(f, items)` run other `@task` functions in
parallel worker processes and return results in argument order. A failed branch comes back as
a `ParallelError` instead of raising. They are recognized inside `@task` bodies only.

## When a task fails

A task (or asset) that raises, or calls `sys.exit()` with any code, fails the run: barca prints
the traceback on stderr and exits 1, and nothing downstream runs. To fail on purpose, for example
on a validation error, raise an exception. A failed `parallel()` branch fails the run only if the
parent task raises. Exit codes: `barca docs agents`.

See also: `barca docs cache`, `barca docs examples/deploy-task`.
