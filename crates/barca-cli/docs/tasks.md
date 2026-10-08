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

`parallel(partial(f, x), ...)` and `parallel_map(f, items)` run other `@task` functions (the
branches) in parallel worker processes and return their results in argument order.

```python
import datetime
from functools import partial

from barca import ParallelError, parallel, task


@task()
def check(region: str) -> dict:
    if region == "ap":
        raise RuntimeError("region unavailable")
    return {"region": region, "checked": datetime.date(2026, 1, 2), "zones": {"a", "b"}}


@task()
def check_all() -> dict:
    regions = ["us", "eu", "ap"]
    results = parallel(*(partial(check, r) for r in regions))
    ok = [r for r in results if not isinstance(r, ParallelError)]
    return {
        "failed": [reg for reg, r in zip(regions, results) if isinstance(r, ParallelError)],
        "checked": [r["checked"].isoformat() for r in ok],
        "zones": sorted(set().union(*(r["zones"] for r in ok))),
    }
```

```bash
barca run check_all pipeline.py
# final_output: {"checked": ["2026-01-02", "2026-01-02"], "failed": ["ap"], "zones": ["a", "b"]}
```

**What a branch may return.** Anything a step may return. The branch's worker writes the value
as an artifact, in the format barca picks for any step output (json, pickle or parquet by type:
`barca docs types`), and the caller reads it back:

- A set, a `date` or `datetime`, a `Decimal`, `bytes`, a dataclass or an object of your own
  class, a numpy array, and containers of these (the dict above) come back equal and of the
  same type.
- A pandas or polars DataFrame, a polars LazyFrame, a pyarrow Table and a DuckDB relation come
  back as the type the branch returned.
- A value JSON can represent is passed as JSON, as between steps: a tuple comes back as a list
  and a dict's non-string keys as strings (`{1: "a"}` as `{"1": "a"}`).
- `None` comes back as `None`.

**When a branch fails.** A branch that raises comes back as a `ParallelError` in place of its
result (`.error` has the exception type, the message and the traceback); nothing is raised in
the caller, which decides what a failed branch means. A branch that returns a value barca
cannot pass back (an open file, a lambda, a generator: nothing pickle can write) is different:
`parallel()` raises `BranchResultError` in the caller, the calling step fails and the run exits
1, with the branch, the type and the reason in the error:

```
BranchResultError: parallel() branch 1: pipeline.py:handle returned a _io.TextIOWrapper, which
cannot be passed back to the step that called parallel(): TypeError: cannot pickle
'TextIOWrapper' instances.
```

A branch's result is never replaced by `None`. (Until 0.18.1 it was, for every value that is
not JSON: the caller received `None` and the run succeeded.)

**Where it can be called.** In the body of a `@task`, and in the body of a branch (a branch may
fan out again). A call from an `@asset` body runs its branches too, but the asset is cached
like any other: the branches do not run again until the asset does. A fan-out that should
happen on every run belongs in a task.

**Limits.**

- Arguments given to `partial` are sent to the branch as JSON, not as artifacts: pass values
  JSON can represent (a tuple arrives as a list). A set raises `TypeError` in the caller. So
  does a `date` or `datetime`, unless the `fast` extra (orjson) is installed, in which case it
  arrives as its ISO string. Convert such values yourself (`d.isoformat()`) and the branch
  gets the same thing either way.
- Branches are not steps of the plan: they are not cached, not retried and not uploaded to an
  artifact store. Their artifacts are files in the local artifact directory, named after the
  path of the branch's file, its function and a number
  (`..._pipeline.py--<function>__branch_<n>.<ext>`), and are overwritten by later runs.

## When a task fails

A task (or asset) that raises, or calls `sys.exit()` with any code, fails the run: barca prints
the traceback on stderr and exits 1, and nothing downstream runs. To fail on purpose, for example
on a validation error, raise an exception. A `parallel()` branch that raises fails the run only
if the calling step raises; a branch whose return value cannot be passed back fails the calling
step (see above). Exit codes: `barca docs agents`.

See also: `barca docs cache`, `barca docs examples/deploy-task`.
