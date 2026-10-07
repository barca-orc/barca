---
title: "Workflow: Tasks and Workflow Management"
description: A task does something (deploy, notify, migrate), always runs, and is run with barca run. A worked example with real output.
---

A `@task` is a step that does something: deploy, notify, migrate. It is never cached. It runs
every time it is in a run, and you run it with `barca run <task> <files>`.

Everything on this page was run with barca 0.18.0. The terminal manual covers tasks in
`barca docs tasks`.

## Assets and tasks

| | `@asset` | `@task` |
|---|---|---|
| Cached | yes | no, always runs |
| May depend on | assets, sensors | assets, sensors, tasks |
| May be an input to | assets, tasks | tasks only |
| Run with | `barca get` | `barca run` |

## Example

```python
# pipeline.py
from barca import asset, task


@asset()
def raw_data() -> list:
    return [3, 1, 2]


@asset(inputs={"data": raw_data})
def trained_model(data: list) -> dict:
    return {"version": 1, "weights": sorted(data)}


@task(inputs={"model": trained_model})
def deploy(model: dict) -> dict:
    print(f"deploying model v{model['version']}")
    return {"endpoint_id": "ep-1"}


@task(inputs={"d": deploy})
def smoke_test(d: dict) -> dict:
    return {"endpoint_id": d["endpoint_id"], "passed": True}


@task(inputs={"result": smoke_test})
def notify(result: dict) -> None:
    print("deploy succeeded" if result["passed"] else "deploy failed")


@task()
def migrate_db() -> None:
    print("migrating")


@task(inputs={"_migrate": migrate_db})
def warm_cache(_migrate) -> None:
    print(f"warming cache; _migrate is {_migrate!r}")
```

Tasks use the same `inputs=` as assets: each key names a parameter of the function.

## Running a task

`barca run` runs the task you name and everything upstream of it. Upstream assets are served
from cache when they have a cached result. Upstream tasks always run.

```bash
barca run notify pipeline.py --agent
```

```
[barca] step:pipeline.py:raw_data completed 0.0s (1/5)
[barca] step:pipeline.py:trained_model completed 0.0s (2/5)
deploying model v1
[barca] step:pipeline.py:deploy completed 0.0s (3/5)
[barca] step:pipeline.py:smoke_test completed 0.0s (4/5)
deploy succeeded
[barca] step:pipeline.py:notify completed 0.0s (5/5)
[barca] 5/5 steps | done in 0.0s
{"elapsed_seconds":0.154927667,"final_output":null,"phases":1,"run_id":"5209eb30e900","status":"success","steps":[...],"steps_executed":5,"warnings":[]}
```

The same command again runs three steps: the two assets are cached and the three tasks run.

```
[barca] step:pipeline.py:raw_data cached
[barca] step:pipeline.py:trained_model cached
deploying model v1
[barca] step:pipeline.py:deploy completed 0.0s (1/5)
[barca] step:pipeline.py:smoke_test completed 0.0s (2/5)
deploy succeeded
[barca] step:pipeline.py:notify completed 0.0s (3/5)
[barca] 3/5 steps | done in 0.0s
```

A task's `print` goes to stderr with the progress lines. stdout is one JSON object.

`barca run smoke_test pipeline.py` stops at `smoke_test`: `deploy` and `smoke_test` run, and
`notify` is not part of the run.

`--dry-run` shows what a command would do without running anything:

```bash
barca run notify pipeline.py --dry-run
```

```
Dry run: barca run notify (nothing executed, nothing written)

STATUS    WHY                  STEP
cached    -                    pipeline.py:raw_data
cached    -                    pipeline.py:trained_model
will run  tasks always re-run  pipeline.py:deploy
will run  tasks always re-run  pipeline.py:smoke_test
will run  tasks always re-run  pipeline.py:notify

3 will run, 2 cached, 0 unknown
```

## Recomputing upstream assets

```bash
barca run deploy pipeline.py --refresh raw_data                # raw_data and everything downstream of it
barca run deploy pipeline.py --refresh raw_data --no-cascade   # raw_data only
barca run deploy pipeline.py --refresh-all                     # every upstream asset
```

`--refresh` takes one comma-separated list (`--refresh raw_data,trained_model`). With
`--refresh raw_data`, three steps run:

```
[barca] step:pipeline.py:raw_data completed 0.0s (1/3)
[barca] step:pipeline.py:trained_model completed 0.0s (2/3)
deploying model v1
[barca] step:pipeline.py:deploy completed 0.0s (3/3)
```

With `--no-cascade`, `trained_model` stays cached and barca says so:

```
[barca] warning: 'trained_model' was served from cache but depends on refreshed 'raw_data', so it does not reflect the refresh. Drop --no-cascade, add it to --refresh (for example --refresh raw_data,trained_model) or use --refresh-all.
```

`--no-cache` still works as a deprecated spelling of `--refresh-all` and prints
`[barca] warning: --no-cache is deprecated and will be removed in a future minor release; use --refresh-all`.

## Ordering without data

An `inputs=` key that starts with `_` means "run after this, and do not load its output". The
parameter receives `None`.

```bash
barca run warm_cache pipeline.py --agent
```

```
migrating
[barca] step:pipeline.py:migrate_db completed 0.0s (1/2)
warming cache; _migrate is None
[barca] step:pipeline.py:warm_cache completed 0.0s (2/2)
[barca] 2/2 steps | done in 0.0s
```

See [Ordering-Only Dependencies](/patterns/03-ordering-only-deps/).

## Several tasks in one run

Name several tasks as one comma-separated list. Their upstreams are planned together, and the
JSON output has a `targets` object in place of `final_output`:

```bash
barca run notify,warm_cache pipeline.py --json
```

```json
{"targets": {"notify": {"final_output": null, "status": "success"},
             "warm_cache": {"final_output": null, "status": "success"}},
 "steps_executed": 5, ...}
```

## Looking at what ran

A task's last successful return value is kept and can be queried, although it is never used as
a cache hit. `barca status pipeline.py` lists every task as `always-runs`.

```
$ barca sql "select * from smoke_test"
endpoint_id  passed
ep-1         true
```

## What is rejected

A task as the target of `barca get`, and an asset as the target of `barca run`, are usage errors
(exit 2):

```
{"code":2,"error":"'deploy' is a task — use `barca run` instead","kind":"usage","remediation":"Run `barca list pipeline.py` to see available assets and tasks."}
{"code":2,"error":"'trained_model' is an asset — use `barca get` instead","kind":"usage","remediation":"Run `barca list pipeline.py` to see available assets and tasks."}
```

`barca get pipeline.py` with no target computes the assets and skips every task, naming them on
stderr. A task as an input to an asset is rejected when the graph is built (exit 2); a sensor
cannot have inputs of any kind, so a task cannot feed one either. See
[Asset-to-Task](/patterns/02-asset-to-task/#limits) for the messages.

## Related pages

- [Error Handling](/patterns/06-error-handling/) and
  [Execution Controls](/workflows/09-execution-controls-and-ad-hoc-params/): `retries=`,
  `retry_backoff=` and `timeout_seconds=` work on tasks as they do on assets.
- [Freshness and Schedules](/workflows/05-schedule-driven-reconciliation-and-effects/):
  `@task(freshness=Schedule("<cron>"))` runs a task on a timer under `barca serve`.
- [Parallel Tasks](/patterns/04-parallel-tasks/): `parallel()` and `parallel_map()` inside a task.
- [`examples/basic_app`](https://github.com/barca-orc/barca/tree/main/examples/basic_app): a
  task that reads an asset, and an ordering-only chain.
