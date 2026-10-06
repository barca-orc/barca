# Assets, inputs and freshness

An asset is a function whose output barca caches and tracks.

```python
from barca import asset, Always, Manual, Schedule


@asset()                                   # freshness=Always (default)
def raw() -> dict:
    return {"x": 1}


@asset(inputs={"data": raw}, retries=3, retry_backoff=2.0, timeout_seconds=600)
def clean(data: dict) -> dict:
    return {"x": data["x"] + 1}


@asset(freshness=Manual)                   # only recomputed on explicit refresh
def pinned() -> dict:
    return {"x": 0}


@asset(freshness=Schedule("0 5 * * *"))    # fires daily at 05:00 under `barca serve`
def daily() -> dict:
    return {"x": 2}
```

## Options on `@asset`

| Option | Meaning |
|---|---|
| `inputs={"param": upstream}` | Wire upstream nodes to parameters. The param name must match a function parameter. |
| `name="..."` | Explicit node name (the default is the function name). |
| `freshness=` | `Always` (default), `Manual`, or `Schedule("<cron>")`. |
| `serializer=` | Force `"json"`, `"pickle"` or `"parquet"`. See `barca docs types`. |
| `partitions=` | Fan one asset out over keys. See `barca docs partitions`. |
| `timeout_seconds=` | Per-attempt time limit (default 300). |
| `retries=` | Total attempts on failure; 1 means no retry. |
| `retry_backoff=` | Base delay in seconds; the delay grows linearly with the attempt number. |
| `env=["NAME", ...]` | Environment variables the function reads. Their values are part of the cache key and are reported per step. See below. |
| `description=`, `tags=` | Metadata. |

## Environment variables: `env=`

An asset that reads an environment variable (a source path, a region, a model name) should
declare it. Barca reads the declared variables when it plans the run, folds each name and value
into the run hash, and reports the values each step used.

```python
import os

from barca import asset


@asset(env=["SOURCE_CSV", "API_TOKEN"])
def raw() -> dict:
    return {"source": os.environ.get("SOURCE_CSV", "default.csv")}


@asset(inputs={"data": raw})
def summary(data: dict) -> dict:
    return {"from": data["source"]}
```

```bash
export SOURCE_CSV=a.csv
barca get summary pipeline.py --agent     # raw and summary run
barca get summary pipeline.py --agent     # both cached
export SOURCE_CSV=b.csv
barca get summary pipeline.py --agent     # raw and summary run again
```

- Changing a declared variable re-materializes the asset and everything downstream of it.
  Unset is its own value, distinct from an empty string. Variables that are not declared are
  not part of the cache key.
- The JSON result's `steps` entry for the node carries `"env": {"API_TOKEN": null,
  "SOURCE_CSV": "b.csv"}` (`null` = unset), and `--agent` step lines end with
  `env API_TOKEN=<unset> SOURCE_CSV=b.csv`.
- Names ending in `_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD` (any case, or the bare word) are
  hashed like any other but shown as `<redacted>` in all output.
- `barca list` shows the declared names in an ENV column (`env` in `--json`).
- `env=` must be a literal list of string literals; anything else (a variable, a tuple, a
  computed name) is a parse error. It is also accepted on `@task` and `@sensor`, which always
  run: there it only records the values used.

**Limitation:** barca cannot see environment variables your code reads without declaring them.
Those are not part of the cache key and are not reported, so a changed value does not
invalidate the asset.

## How a node is identified

A node id is `<file>:<function>` (for example `pipeline.py:clean`), or the explicit `name=`.
Targets on the command line can use the bare function name (`barca get clean pipeline.py`).
Several targets are one comma-separated list (`barca get clean,report pipeline.py`): their
upstream cones are planned together, so an asset both need materializes once, and the JSON
output is keyed by target (`barca docs agents`).
With no target, `barca get pipeline.py` materializes every asset and sensor in the file and skips
tasks (previously it ran them too); run a task with `barca run <task> pipeline.py`.
An input defined in another file is imported like any Python name (`from pipelines.sources
import raw`, then `inputs={"r": raw}`); barca resolves the import statically. Use
`asset_ref("other/file.py:raw")` to name one without importing it. See `barca docs discovery`.

## Naming

An asset is an ordinary Python function, so its name is an ordinary Python name. If a file
imports a module (`import carry_forward_registry`) and also defines an asset with the same
name, the `def` rebinds the name and shadows the module for the rest of the file. Give the
import an alias (`import carry_forward_registry as cf_registry`) or rename the asset. Barca does
not warn about this; Python simply uses the later definition.

## Static analysis

Planning never imports your code. The decorators, `inputs=` and `freshness=` must be written
literally enough for barca to read them from the source. Dynamic decorator construction
(building `inputs` in a loop, calling a decorator through a variable) is not visible to the
planner. Mark code barca cannot reason about with `@unsafe` (silences purity warnings only).

## Unused inputs

Every declared data input is loaded in full before the step runs, whether or not the function
uses it. At plan time barca reads the function body and warns when a data input (a parameter
wired by `inputs=` whose name does not start with `_`) is never referenced, or appears only in
a `del` statement:

```
[barca] warning: step `report` declares input `raw` but never uses it; it is still loaded in full before every run. Remove the dependency, or rename the parameter `_raw` if only ordering is needed (ordering-only inputs are not loaded)
```

The warning is printed once per command on stderr (`plan`, `get`, `run`, `get --dry-run`) and
is also a `warnings` entry in the JSON of `barca plan` and `barca get|run --json`:
`{"kind": "unused_input", "node": "report", "param": "raw", "message": "..."}`. The key is
absent when there is nothing to warn about. The fix is to remove the input, or to rename the
parameter `_raw` (and the key in `inputs=`) when you only need the upstream to run first:
ordering-only inputs are not loaded.

The check is conservative, because barca cannot see through a call. Any other mention of the
name counts as a use: passing it to a helper, a nested function, a comprehension, an f-string.
A body that calls `locals()`, `vars()`, `eval()` or `exec()`, and a function with `**kwargs`,
are never flagged. A warning does not change the exit code or the cache.

## Sensors

`@sensor` observes external state and returns `(update_detected: bool, value)`. Sensors have no
inputs and must use `Manual` or `Schedule(...)` freshness, never `Always`.

A sensor always runs, and its `value` is part of the run hash of every asset that reads it: when
the value changes, those assets and everything downstream of them re-run; when it is the same,
they are served from cache. This is how to track external data that changes in place: a sensor
returns a blob's etag and the asset that reads the blob depends on the sensor (`barca docs cache`,
"External data that changes in place"). Return only what identifies the data: a value that
changes on every run, such as a timestamp, re-runs the sensor's consumers every time. The
`update_detected` flag is not used for caching.

`barca get <sensor> pipeline.py` observes one sensor. `barca get pipeline.py` (no target) observes
every sensor, including one nothing depends on: a sensor is something `get` can target, and
observing is read-only. Tasks are the only nodes a bare `get` skips.

See also: `barca docs tasks`, `barca docs cache`, `barca docs scheduling`.
