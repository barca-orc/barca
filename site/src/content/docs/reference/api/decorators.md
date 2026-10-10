---
title: Decorators API
description: Arguments and behavior of @asset, @sensor, @task, @sink and @unsafe, the freshness markers, partitions, parallel() and the other names barca exports.
---

Everything here is imported from `barca`. The decorators return the function unchanged: the
`barca` binary extracts their arguments from source without importing your module. Literal
partition key lists need no resolver import; a nonliteral `partitions(<expression>)` imports
the pipeline module to evaluate its keys. Arguments must therefore be written literally (a dict literal for `inputs=`, a list of
string literals for `env=`); a decorator built in a loop or installed through a runtime assignment is not
seen. Qualified names and imported aliases are supported as described below. The signatures below are those of `python/barca/__init__.py`; unless a version is named,
the behavior was checked by running 0.18.0.

## Qualified and aliased imports

Barca recognizes top-level imports such as `import barca`, `import barca as b`
and `from barca import asset as a`, for decorators and their helpers:

```python
import barca as b

@b.asset(partitions={"key": b.partitions(["a", "b"])})
def rows(key):
    return {"key": key}
```

Planning reads these bindings statically and never imports your module. A name
reassigned or imported from another library does not define a Barca node; local
parameters or assignments also shadow task-body helpers such as `parallel`.
Dynamic alias assignments and conditional imports are not resolved. Explicit writes to a Barca module export
conservatively disable recognition of that export through every module alias and
direct import, including earlier copies; unaffected exports remain available.

If a foreign decorator previously became a node merely because it was named
`asset`, `sensor` or `task`, import the actual Barca decorator instead. To keep a
foreign wrapper on a node, stack it with a genuine Barca decorator (qualified or
aliased imports work). Bare names with no competing binding retain legacy
source-snippet recognition. Namespace reflection retains conservative discovery,
but disables provenance-based validation and uses conservative hashing.

## Accepted arguments

These are all the arguments the decorators and helpers take. The Python signatures in
`barca/__init__.py` accept exactly these (a test keeps this table, the signatures and the
check in the binary equal), so a type checker and an IDE know them too.

| Call | Positional arguments | Keyword arguments |
|---|---|---|
| `@asset` | none | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env` |
| `@sensor` | none | `name`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env` |
| `@task` | none | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env` |
| `@sink` | one | `serializer` |
| `partitions()` | one | none |
| `partitions_from()` | one | none |
| `collect()` | one | none |
| `asset_ref()` | one | none |
| `Schedule()` | one | none |

Any other argument is an error when barca reads the file, before anything runs (exit 2, `kind`
`usage`). Up to 0.18.1 it was ignored without a message: `@asset(after=other)`,
`@task(when=...)` or a misspelt `input=` planned, ran and did nothing.

```
$ barca list pipeline.py --pretty
Parse error: pipeline.py:report (line 9): `input` is not an argument of @asset. Did you mean `inputs`? @asset accepts: name, inputs, partitions, serializer, freshness, timeout_seconds, retries, retry_backoff, description, tags, env
Rename `input` to `inputs`, or remove it. See `barca docs assets`.
```

- The error names the node, the line, the argument and everything the call accepts. "Did you
  mean" appears only when exactly one accepted argument is one edit away from what was written
  (two for a name longer than four characters).
- Every argument in the table has an effect on each decorator it is listed for, and only
  arguments that had none are rejected. On `@task` and `@sensor`, `partitions=` runs the
  function once per key and `serializer=` forces the stored format, as on `@asset`.
- The helpers and `@sink` take exactly one argument, by position: `partitions(values=[...])`,
  `collect(asset_fn=x)`, `Schedule(cron="...")` and `@sink(path="...")` are errors that say to
  pass the value as the first argument; `@sink()`, `partitions(a, b)` and
  `@sink("out.txt", "json")` are errors too.
- `@asset(**options)`, `@asset(*args)` and `@asset("x")` are errors: barca reads the arguments
  from the source and cannot see what a `**` or `*` holds.
- The check is made per file, like the check for a syntax error. Every command that reads the
  file exits 2 and runs nothing, whatever its target; with no file arguments that is every
  pipeline file of the project. `barca serve` starts and answers `400` on the routes that read
  the pipeline. Only the first such argument in a file is reported.
- Importing the module (`python pipeline.py`) raises
  `TypeError: asset() got an unexpected keyword argument 'after'`.
- Not checked: a decorator or helper that is not positively barca's. A name is checked only
  when `from barca import <name>` (or `from barca import *`) stands at the top level of the
  module and nothing else binds the name at module level (an assignment such as
  `task = app.task`, a `def` or `class`, another import, also inside `try:` or `if`, a loop or
  `with` target, `global`, a later `from other import *`). Celery's or Prefect's
  `@task(bind=True)` is left alone. Also not checked: the values of the arguments (an `inputs=`
  that is not a dict literal is still read as "no inputs"), and `parallel()` and
  `parallel_map()`.
- `import barca as b` with `@b.asset(...)` and `from barca import asset as a` with `@a(...)`
  are not read as nodes at all, so nothing is checked there and `barca list` shows nothing.

## @asset

```python
@asset(
    name: str | None = None,
    inputs: dict[str, AssetRefLike] | None = None,
    partitions: dict[str, PartitionSpecLike] | None = None,
    serializer: SerializerKind | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
)
```

Declares an asset: a function whose result barca stores and reuses. The function runs again
only when its run hash changes, that is, when its code, an input, a sensor value it reads, its
partition key or a declared environment variable changes.

`inputs` maps a parameter name to an upstream node. `name` replaces the function name in the
node id. `serializer` forces `"json"`, `"pickle"` or `"parquet"`. `timeout_seconds` is the
limit for one attempt. `description` and `tags` are metadata.

`retries` is the total number of attempts on failure (1 = no retry). `retry_backoff` is the base
delay in seconds between attempts (delay grows linearly: `retry_backoff * attempt`).

Of these arguments, `inputs`, `serializer` and the dimension names in `partitions` are part of
the run hash, with stacked `@sink` decorators and any decorator that is not barca's: changing
one runs the asset again. `name`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`,
`description`, `tags` and the partition keys are not, and neither is the formatting of the
decorator: editing them re-runs nothing. `barca docs cache` has the full list with the reason
for each (from 0.19; up to 0.18 any edit to the decorator re-ran the asset).

`env` declares the environment variables the function reads. See
[Declared environment variables](#declared-environment-variables-env) below.

```python
from barca import asset, Always, Manual, Schedule

@asset()                                    # freshness=Always (default)
def my_asset() -> dict:
    return {"x": 1}

@asset(freshness=Manual)                    # recorded; no effect on a run today
def pinned_data() -> dict:
    return {"x": 1}

@asset(freshness=Schedule("0 5 * * *"))     # fires daily at 05:00 under `barca serve`
def daily_report() -> dict:
    return {"x": 1}
```

### Freshness

`freshness=` takes `Always` (the default), `Manual` or `Schedule("<cron>")`. `Schedule` is the
only value with an effect at run time: `barca serve` runs the node on each cron tick (see
[Schedule](#schedule)). `Always` and `Manual` are recorded and shown (`barca list`, the
`freshness` key in its JSON) and do nothing else today: `barca get` and `barca run` treat an
`Always` asset and a `Manual` asset the same way. What they should do under `barca serve` is
proposed in RFC-0008 ([PR #276](https://github.com/barca-orc/barca/pull/276)).

### Type annotations

Parameter and return annotations are optional. For a parquet result they choose the reader or
writer; they are read from the source, so use the conventional names.

| Annotation | Parquet role |
|---|---|
| *(none)* | pandas reader (default) |
| `pd.DataFrame` / `pandas.DataFrame` | pandas |
| `pl.DataFrame` / `polars.DataFrame` | polars |
| `pl.LazyFrame` | polars, lazy (a `LazyFrame` scanning the parquet file on read; collected on write) |
| `pyarrow.Table` | pyarrow |
| `duckdb.DuckDBPyRelation` | duckdb (relation on read; written to parquet when the step ends) |

Every result is written in full to an artifact file when the step ends; nothing lazy is passed
between steps. A `pl.LazyFrame` or `duckdb.DuckDBPyRelation` input reads nothing up front, and
the step's query decides which columns and row groups are read. See
[Large inputs](/patterns/08-large-inputs/) and `barca docs types`.

### Declared environment variables (`env=`)

```python
import os
from barca import asset

@asset(env=["SOURCE_CSV", "API_TOKEN"])
def raw() -> dict:
    return {"source": os.environ.get("SOURCE_CSV", "default.csv")}
```

`env=` must be a literal list of string literals; barca reads it statically (a variable, tuple or
computed name is a parse error). When it plans a run, barca reads each declared variable from its
own environment (the workers inherit the same environment) and folds the name and value into
the step's run hash:

- Changing a declared variable re-materializes the asset and everything downstream of it. An
  unset variable is its own value, distinct from an empty string.
- Each step's entry in the JSON result carries `"env": {"API_TOKEN": null, "SOURCE_CSV": "b.csv"}`
  (`null` = unset), and `--agent` step lines end with `env API_TOKEN=<unset> SOURCE_CSV=b.csv`.
- Names ending in `_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD` (any case, or the bare word) are
  hashed but shown as `<redacted>` in every output.
- `barca list` shows declared names in an ENV column, and as `env` in `--json`.
- Nodes that declare no `env` hash exactly as before, so existing caches stay valid.

`env=` is also accepted on `@task` and `@sensor`. Those always run, so there it only records the
values each run used.

**Limitation:** environment variables your code reads without declaring them are not seen by
barca. They are not part of the cache key, so changing one does not invalidate anything.

## Partitions

```python
partitions(values)            # declare the keys of one dimension
partitions_from(upstream)     # take the keys from an upstream asset
collect(upstream)             # fan-in: every key of an upstream asset as one list
```

`partitions=` on `@asset` splits an asset into one step per key. The keys run in parallel and
each key is cached on its own.

```python
from barca import asset, partitions, partitions_from, collect


@asset(partitions={"region": partitions(["emea", "amer", "apac"])})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(partitions={"region": partitions_from(sales)})   # same keys as `sales`
def margin(region: str, sales: dict) -> dict:
    return {"region": region, "margin": sales["revenue"] * 0.2}


@asset(inputs={"all_sales": collect(sales)})            # fan-in: every key as one list
def summary(all_sales: list[dict]) -> dict:
    return {"total": sum(s["revenue"] for s in all_sales)}
```

```
$ barca get summary pipeline.py --json
[barca] 4/4 steps | done in 0.2s
{"elapsed_seconds":0.578354167,"final_output":{"total":1200},"phases":2, ...}
```

### `partitions(values)`

- The key is passed to the function as the parameter named in `partitions={...}` (`region`
  above).
- A literal list is read from the source. Any other expression (a module constant, a list
  comprehension, a function call) is evaluated by Python when the run is planned.
- A step's entry in the JSON result reports the keys: `"partitions": {"total": 3, "cached": 2,
  "will_run": 1, "will_run_keys": ["k=LATAM"]}`.
- Adding a key runs only the new key, for the asset and for an asset derived from it with
  `partitions_from`; removing or reordering keys runs no key. This is the same for a literal
  list, a module constant, a comprehension, a function call and `partitions_from(...)`. (Up to
  0.18, an edit to a literal list inside the decorator ran every key again.)

### `partitions_from(upstream)`

On a partitioned `upstream`:

- the asset gets the same keys, and each key is called with the key and that key's result of
  `upstream`, passed as the parameter named after the upstream function:
  `margin(region="emea", sales=<the emea result of sales>)`;
- to receive it under another name, list the upstream in `inputs=` as well:
  `@asset(inputs={"s": sales}, partitions={"region": partitions_from(sales)})` calls
  `margin(region, s)`;
- the dimension must keep the upstream's name, the upstream must have one dimension, and
  `partitions_from(upstream)` must be the asset's only dimension. Anything else exits 2 when the
  DAG is built, for example
  `partitions_from(sales) is declared under dimension 'area', but 'sales' is partitioned by 'region'`;
- each key depends on its own key of the upstream only.

On an unpartitioned `upstream` that returns a list:

- the list's values are the keys, and the list itself is not passed to the function;
- the keys are known only after `upstream` has run. Until then `--dry-run` and `barca status`
  report the asset as `unknown` (reason `partitions_unknown`);
- when the list grows, only the new keys run.

### `collect(upstream)`

Used inside `inputs=`, it passes every key's result of a partitioned `upstream` as one list.
The fan-in runs in its own phase after every key has finished, and runs again when its set of
inputs changes.

A partitioned asset in an unpartitioned asset's `inputs=` without `collect()` exits 2 and
names both fixes:

```
DAG error: input 'all_sales' on 'pipeline.py:summary' reads partitioned asset 'pipeline.py:sales', but 'pipeline.py:summary' is not partitioned
Use `inputs={"all_sales": collect(sales)}` to receive every partition of 'sales' as one list, or `partitions={"<key>": partitions_from(sales)}` to run once per partition of 'sales' with that partition's output.
```

Up to 0.11 this passed the list without an error.

### Other rules and limits

- An unpartitioned asset in a partitioned asset's `inputs=` is passed whole to every key. It
  runs once, before any key, and its run hash is part of every key's run hash, so changing it,
  or `--refresh` on it, re-runs every key.
- Artifacts are stored per key: `.barca/artifacts/pipeline.py--sales_region_emea/<run_hash>.json`.
- `barca get sales` on a partitioned asset prints one key's result as `final_output`, not all
  of them, and no single key can be targeted or refreshed. To read every key, use a `collect`
  asset or [`barca sql`](/reference/sql/) (one view with a `partition` column holding
  `region=emea`).
- `barca status` shows a partitioned asset as one node with `partitions: {total, cached,
  missing, missing_keys}`.

See `barca docs partitions`.

## asset_ref

Cross-file inputs are ordinary imports: `from other_module.assets import raw_data`, then
`inputs={"data": raw_data}`. Barca resolves the import statically to that file's node (see
[Discovery](/reference/discovery/)). `asset_ref("path/to/file.py:function_name")`, used inside
`inputs=`, references a node by its canonical id (root-relative file path + function name, or its
explicit `name=`) without importing it, for example to avoid an import cycle. Its signature is
`asset_ref(ref_string: str) -> str`:

```python
from barca import asset, asset_ref

@asset(inputs={"data": asset_ref("other_module/assets.py:raw_data")})
def process(data: dict) -> dict:
    return data
```

## @sink

```python
@sink(
    path: str,
    /,
    *,
    serializer: str | None = None,
)
```

Stacked under `@asset`, it writes the asset's result to an extra path each time the asset
runs. A cache hit does not write the sink again. Paths are local or any fsspec URI (`abfss://`,
`s3://`, `gs://`); remote schemes need the matching extra (see
[Remote storage](/reference/remote-storage/)). Several `@sink` decorators may be stacked on
one asset.

```python
from barca import asset, sink, Always

@asset(freshness=Always)
@sink('./output.json')
@sink('abfss://exports@myacct.dfs.core.windows.net/output.parquet', serializer='parquet')
def banana() -> dict:
    return {'a': 1}
```

- The format comes from `serializer=` (`json`, `pickle`, `parquet`), else the path's extension
  (`.json`, `.pkl`, `.pickle`, `.parquet`), else the asset's own artifact format.
- A parquet sink needs a DataFrame, Arrow table or DuckDB relation. Anything else is a sink
  failure.
- Writes go to a temporary file first and are then renamed or uploaded, so a crash does not
  leave a partial file at the destination.
- No other node may take a sink as an input.
- A failing sink does not fail the asset and does not change the exit code. It is reported on
  stderr as `[barca] SINK FAILED: ...`; check stderr in automation.
- On a partitioned asset each key writes its own file, with the key inserted before the
  extension: `@sink("./exports/sales.json")` on keys `region=emea` and `region=amer` wrote
  `exports/sales_region_emea.json` and `exports/sales_region_amer.json`.

See `barca docs sinks`.

## @sensor

```python
@sensor(
    name: str | None = None,
    partitions: dict[str, PartitionSpecLike] | None = None,
    serializer: SerializerKind | None = None,
    freshness: Freshness = Manual,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
)
```

Declares a sensor: a function that looks at something outside the pipeline (a directory, a
bucket, an API) and returns a value that identifies its current state. A sensor has no inputs
(`sensor '...' cannot have inputs`, exit 2) and runs on every `barca get` or `barca run` whose
cone includes it. See `@asset` above for `env`, `retries` and `retry_backoff`.

`partitions` runs the sensor once per key, with the key passed as the parameter named in the
dict; each key's value is stored on its own. `serializer` forces the format the value is stored
in (`"json"`, `"pickle"` or `"parquet"`).

```python
from pathlib import Path

from barca import asset, sensor, Schedule


@sensor(freshness=Schedule("*/5 * * * *"))
def inbox_files() -> tuple[bool, list[str]]:
    files = sorted(str(f) for f in Path("inbox").glob("*.csv"))
    return len(files) > 0, files


@asset(inputs={"files": inbox_files})
def loaded(files: list[str]) -> dict:
    return {"n": len(files)}
```

**Return value.** Return a tuple `(update_detected, value)`. The asset that reads the sensor
receives `value` only. `update_detected` is not used for caching. A sensor that returns
something other than a two-item tuple has the whole return value passed on as `value`
(observed on 0.18.0; the tuple is the documented form).

**Caching.** The sensor's `value` is part of the run hash of every asset that reads it: with
the same files `loaded` is served from cache, and after a file is added `loaded` and
everything below it run again. This is the supported way to bring outside data into a
pipeline; an asset that reads a file or a bucket in its own body is computed once and then
served from cache. Return only what identifies the data (file names, an etag). A value that
changes on every run, such as a timestamp, re-runs every consumer every time.

**Previews.** `--dry-run` and `barca status` do not run the sensor. They assume it returns its
last recorded value and say so in `detail`; before the sensor has ever run, its consumers are
`unknown` (reason `sensor_output_unknown`).

**Running one.** `barca get inbox_files pipeline.py` runs one sensor and prints its value.
`barca get pipeline.py` with no target runs every sensor, including one nothing depends on.

**Freshness.** The default is `Manual`. `Schedule("<cron>")` makes `barca serve` run the
sensor on that cron; the tick records the sensor's value and does not trigger the assets that
read it. 0.18.0 accepts `@sensor(freshness=Always)` without an error.

See [Sensors and External Observations](/workflows/06-sensors-and-external-observations/) for
a walk-through, and `barca docs assets` ("Sensors").

## @task

```python
@task(
    name: str | None = None,
    inputs: dict[str, NodeRefLike] | None = None,
    partitions: dict[str, PartitionSpecLike] | None = None,
    serializer: SerializerKind | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
)
```

Declares a task: a step that does something (a deploy, a notification, a migration) and is
not cached. A task runs every time it is in a run's cone.

- They may appear anywhere in the graph, not only at the leaves.
- `partitions` runs the task once per key, with the key passed as the parameter named in the
  dict. `serializer` forces the format the return value is stored in (`"json"`, `"pickle"` or
  `"parquet"`).
- They may depend on assets, sensors, or other tasks (via `inputs=`).
- For ordering-only dependencies (no data needed), use the `_` prefix convention:
  `inputs={"_dep": some_node}`. The `_` prefix tells barca to skip artifact
  deserialization — the parameter receives `None`.
- They must not be an input to an asset or sensor. This exits 2:
  `task 'pipeline.py:t1' cannot be an input to asset 'pipeline.py:d'`.

Run a task with [`barca run`](/reference/cli/#run). Upstream assets are served from cache, as
with `barca get`. `barca get` on a task exits 2 and names `barca run`. A task with
`freshness=Schedule("<cron>")` runs on that cron under `barca serve`.

```python
from barca import asset, task

@asset()
def report() -> dict:
    return {"rows": 42}

# Asset -> task: a task consuming an upstream asset.
@task(inputs={"data": report})
def send_email(data: dict) -> None:
    print(f"Sending report: {data}")

# Ordering-only: migrate runs first, notify doesn't need its data.
@task()
def migrate() -> None:
    run_migration()

@task(inputs={"_migrate": migrate})
def notify(_migrate) -> None:
    send_slack("migration done")
```

## parallel

```python
parallel(*callables) -> list
parallel_map(fn, items, **kwargs) -> list
```

Fan out work from inside a `@task` body across worker processes. `parallel()` takes
`functools.partial`-wrapped calls to other `@task`-decorated functions and returns their results
(or `ParallelError` objects for failed branches) in argument order. `parallel_map(fn, items)` is
sugar for `parallel(*(partial(fn, item) for item in items))`.

```python
from functools import partial
from barca import task, parallel, parallel_map

@task()
def deploy_us(model) -> str: ...

@task()
def deploy_eu(model) -> str: ...

@task()
def deploy_all(model) -> None:
    results = parallel(partial(deploy_us, model), partial(deploy_eu, model))
    # or: results = parallel_map(deploy, ["us", "eu"])
```

Inside a barca worker, `parallel()` runs each branch in a separate worker process (the calling
worker is stopped until they finish). Called outside a worker, it runs the callables one after
another. Arguments must be JSON values. A branch may return anything a step may return: its
value is written as an artifact (json, pickle or parquet, by type) and read back by the caller,
so a set, a date or a DataFrame arrives as it was returned, and a JSON value as JSON gives it
back (a tuple as a list).

A branch that raises is returned as a `ParallelError`, whose `.error` holds the message and
traceback, and nothing is raised: the calling task succeeds and the run exits 0 unless your code
inspects the results and raises. A branch whose return value cannot be passed back (an open
file, a lambda) makes `parallel()` raise `BranchResultError`, which fails the calling step.
Branches are not retried and not cached. A call from an `@asset` body also runs its branches,
but the asset is then cached like any other, so a fan-out that should happen on every run
belongs in a task. See [Parallel Tasks](/patterns/04-parallel-tasks/).

## @unsafe

```python
@unsafe
def my_asset() -> str:
    return global_config["value"]
```

Marks a function whose behavior barca cannot work out from its source, because it reads
globals or does I/O. `@unsafe` silences purity warnings only. Caching is unchanged: the asset is
still served from cache while its run hash matches.

## Schedule

```python
Schedule("0 5 * * *")       # 5-field cron: daily at 05:00
Schedule("*/15 * * * * *")  # 6-field cron: every 15 seconds
```

A freshness value for `freshness=` on `@asset`, `@sensor` or `@task`. It has an effect only
while `barca serve` is running; `barca get` and `barca run` ignore it. `barca list` shows the
cron and the next fire time.

The cron has 5 fields (`minute hour day-of-month month day-of-week`) or 6 with a leading
seconds field. There is no year field. See [Scheduling](/scheduling/) for what a tick does.

## Other exports

| Name | What it is |
|---|---|
| `duckdb_connection()` | The DuckDB connection barca binds `duckdb.DuckDBPyRelation` inputs to, one per worker process. Configure it at import time of your module. See `barca docs types`. |
| `ParallelError` | What `parallel()` returns in place of a branch that raised. |
| `BranchResultError` | Raised by `parallel()` when a branch returned a value that cannot be passed back to the caller. |
| `get`, `run`, `plan`, `history`, `stats` | Python functions that start the `barca` binary and return parsed results. See `barca docs agents`, "Getting values, not pointers". |
| `BarcaError` | Raised by those functions; carries `kind`, `code`, `remediation`, and for a failed step `node`, `traceback`, `artifact_dir`. |
| `Client`, `Run` | HTTP client for `barca serve`. See [Server API](/reference/server-api/#python-client). |

DuckDB steps share one default connection per worker process and execute sequentially.
Import-time setup remains available to later steps. User-created objects and session settings
persist, including after failures; Barca cleans up its bound input views after materialization.
Avoid project catalog names that collide with input parameters: existing tables are skipped
during binding, and existing views can be replaced and then dropped. For naming and cleanup
patterns, see [Anti-Patterns](/patterns/07-anti-patterns/#fixed-names-on-duckdbs-shared-connection).

Explicit references to `globals`, `locals`, `vars`, `exec`, `eval` or `__builtins__`
make imported barca names uncertain, including imported aliases and qualified or shadowed
references. Argument validation then follows Python at runtime, and nodes with an uncertain
decorator conservatively hash their entire module and statically tracked dependencies.
These checks cover explicit syntax; they do not resolve arbitrary reflective indirection,
runtime side effects or dynamically imported dependencies.
