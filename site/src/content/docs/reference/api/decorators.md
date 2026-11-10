---
title: Decorators API
description: Arguments and behavior of @asset, @sensor, @task, @sink and @unsafe, the freshness markers, partitions, parallel() and the other names barca exports.
---

Everything here is imported from `barca`. The decorators return the function unchanged: the
`barca` binary reads their arguments from the source without importing your module, so write
them literally (a dict literal for `inputs=`, a list of string literals for `env=`), or barca
exits 2 naming the argument ("Values barca reads" below).

| Name | What it is for |
|---|---|
| [`@asset`](#asset) | A function whose result is cached and reused until its code or inputs change |
| [`@sensor`](#sensor) | A function that checks something outside (a file, a table) on every run; what reads it reruns when its value changes |
| [`@task`](#task) | A function that does something (deploy, notify) and runs every time |
| [`@sink`](/reference/api/helpers/#sink) | Also write an asset's result to a path you choose, local or in a bucket |
| [`partitions`, `partitions_from`, `collect`](/reference/api/helpers/#partitions) | Run one function per key, and gather the keys back |
| [`Schedule`, `Manual`, `Always`](#schedule) | When `barca serve` runs an asset (`freshness=`) |
| [`parallel`, `parallel_map`](/reference/api/helpers/#parallel) | Inside a task, run other tasks at the same time |
| [`asset_ref`](/reference/api/helpers/#asset_ref) | Name an input by its id (`path/file.py:function`) instead of importing it |
| [`@unsafe`](#unsafe) | Mark a function that reads globals or does I/O, which silences purity warnings |
| [Other exports](/reference/api/helpers/#other-exports) | `barca.get`, `barca.sql`, `Group`, `duckdb_connection` and the rest |

## Accepted arguments

These are all the arguments the decorators and helpers take. The Python signatures in
`barca/__init__.py` accept exactly these (a test keeps this table, the signatures and the
check in the binary equal), so a type checker and an IDE know them too.

| Call | Positional arguments | Keyword arguments |
|---|---|---|
| `group()` | one | `members`, `output`, `description` |
| `@asset` | none | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env`, `weight`, `deterministic`, `cutoff`, `batch` |
| `@sensor` | none | `name`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env`, `weight` |
| `@task` | none | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env`, `weight`, `batch` |
| `@sink` | one | `serializer`, `partition_by`, `rows_per_file` |
| `partitions()` | one | none |
| `partitions_from()` | one | none |
| `batches()` | one | `strict` |
| `batches_by()` | one | `max_size` |
| `collect()` | one | none |
| `asset_ref()` | one | none |
| `Schedule()` | one | `max_failures`, `timeout_seconds`, `batch` |

Any other argument is an error when barca reads the file, before anything runs (exit 2, `kind`
`usage`). Up to 0.18.1 it was ignored without a message: `@asset(after=other)`,
`@task(when=...)` or a misspelt `input=` planned, ran and did nothing.

```
$ barca list pipeline.py --pretty
Parse error: pipeline.py:report (line 9): `input` is not an argument of @asset. Did you mean `inputs`? @asset accepts: name, inputs, partitions, serializer, freshness, timeout_seconds, retries, retry_backoff, description, tags, env
Rename `input` to `inputs`, or remove it. See `barca docs assets`.
```

- The error names the node, the line, the argument and everything the call accepts. "Did you
  mean" names the closest accepted argument within one edit of what was written (two for a name
  longer than four characters), or two that tie. Mistyped flags, subcommands and targets get the
  same rule.
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
- Qualified and aliased top-level Barca imports are checked. Foreign imports and explicit
  rebinding disable recognition of the affected name. A later star import disables it only
  when the export set is unknown or includes that local name. Namespace reflection retains
  conservative discovery but disables argument checks. Argument values are checked next;
  task-body `parallel()` / `parallel_map()` calls are not checked.

### Values barca reads

Each value has to be written in a form barca reads from the source. Any other value is an
error when barca reads the file (exit 2, `kind` `usage`), naming the argument, the file and
line, and the form to write. Up to 0.33.1 most were read as if the argument were not written:
`inputs=make_inputs()` declared no inputs, `tags=TAGS` no tags, `timeout_seconds=LIMIT` the
default 300, `@sink(EXPORT_PATH)` no sink.

| Argument | Form |
|---|---|
| `name=`, `description=` | a string literal |
| the path of `@sink` | a string literal (`@sink("exports/report.parquet")`) |
| `inputs=` | a dict literal: keys are string literals, each value names a step (`upstream`, `module.upstream`, `collect(upstream)`, `asset_ref("file.py:upstream")`) |
| `partitions=` | a dict literal: keys are string literals, each value is `partitions(<keys>)` or `partitions_from(upstream)`; the keys inside `partitions(...)` may be any expression |
| `serializer=` | a string literal: `"json"`, `"pickle"`, `"parquet"` or `"csv"` (on `@sink` too) |
| `freshness=` | `Always`, `Manual` or `Schedule("<cron>", ...)`, written in the decorator |
| `timeout_seconds=`, `retries=` | a whole-number literal |
| `retry_backoff=` | a number literal (`2` or `2.0`) |
| `tags=` | a dict literal whose keys and values are string literals |
| `env=` | a list of string literals |

`None` reads as "not written" for `name=`, `description=`, `inputs=`, `partitions=`,
`serializer=` and `tags=`. A module constant is an error even when it holds a literal:

```python
from barca import asset

TAGS = {"team": "growth"}

@asset(tags=TAGS)
def report() -> int:
    return 1
```

```
$ barca list pipeline.py --pretty
Parse error: pipeline.py:report (line 5): `tags=` must be a dict literal, got `TAGS`; barca reads decorator arguments from the source without running it, so it cannot see what this is
Write it like `tags={"team": "growth"}`: a dict literal whose keys and values are string literals. See `barca docs assets`.
```

Not checked:

- A call barca cannot identify as its own helper because the name is rebound or comes from
  another library (`partitions = load_partitions`, `cron_lib.Schedule(...)`), or because the
  file reassigns barca's export (`barca.partitions = other`), as a partition dimension or as
  `freshness=`: the value is left unread, as its arguments are. In `inputs=` such a call names
  no step and is an error.

A later `from shared import *` preserves imported Barca names when the project module has
one literal `__all__` list or tuple of strings, with no other references to `__all__`,
that does not export those local names.
Only colliding names lose recognition, including imported aliases and `group`. Barca uses
the same package and sibling import paths as its workers and reads the source without
importing it. Missing, computed, conditional or mutated export sets, installed modules and
namespace reflection remain uncertain; put the Barca import last or use explicit imports.
Star-import dependency hashing remains conservative.

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
    weight: int = 1,
    deterministic: bool = True,
    cutoff: bool = False,
)
```

Declares an asset: a function whose result barca stores and reuses. The function runs again
only when its run hash changes, that is, when its code, an input, a sensor value it reads, its
partition key or a declared environment variable changes.

`inputs` maps a parameter name to an upstream node. Each key must be a parameter of the function,
and each parameter without a default must be an input or a partition dimension (or have
`**kwargs` take it); barca checks both when it reads the file, exit 2 (a `@task`'s parameters may
be left unfilled, for `parallel()` branches). `name` replaces the function name in the
node id. `serializer` forces `"json"`, `"pickle"`, `"parquet"` or `"csv"`. `timeout_seconds` is the
limit for one attempt. `description` and `tags` are metadata.

`retries` is the total number of attempts on failure (1 = no retry). `retry_backoff` is the base
delay in seconds between attempts (delay grows linearly: `retry_backoff * attempt`).

Of these arguments, `inputs`, `serializer` and the dimension names in `partitions` are part of
the run hash, with stacked `@sink` decorators and any decorator that is not barca's: changing
one runs the asset again. `name`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`,
`weight`, `deterministic`, `cutoff`, `description`, `tags` and the partition keys are not, and neither is the formatting of the
decorator: editing them re-runs nothing. [The Run Hash](/reference/hashing/#which-decorator-arguments-count) has the full list with the reason
for each (from 0.19; up to 0.18 any edit to the decorator re-ran the asset).

`env` declares the environment variables the function reads. See
[Declared environment variables](#declared-environment-variables-env) below. `weight` is how
much of the pool's capacity the step takes while it runs; see
[Heavy steps](#heavy-steps-weight) below. `cutoff=True` makes what reads the asset depend on the
bytes it wrote instead of its run hash; see [Early cutoff](#early-cutoff-cutofftrue) below.

`deterministic=False` says the asset may return other bytes for the same code and inputs (a
snapshot of an external system), so barca does not warn `produced different bytes for the same
code and inputs; the function is not deterministic` for it, and does not report
`bytes_changed` or `last_bytes_change`. It changes nothing else: the result is cached under
its run hash as before, and what reads it is not made stale by new bytes alone. Sensors and
`Manual` assets never get the warning.

```python
from barca import asset, Always, Manual, Schedule

@asset()                                    # freshness=Always (default)
def my_asset() -> dict:
    return {"x": 1}

@asset(freshness=Manual)                    # refreshed by hand only; readers get its last result
def pinned_data() -> dict:
    return {"x": 1}

@asset(freshness=Schedule("0 5 * * *"))     # fires daily at 05:00 under `barca serve`
def daily_report() -> dict:
    return {"x": 1}
```

### Freshness

`freshness=` takes `Always` (the default), `Manual` or `Schedule("<cron>")`. `barca list`
shows it (the `freshness` key in its JSON). Freshness is not part of the definition hash.

- `Schedule("<cron>")`: `barca serve` starts a run on each cron tick (see [Schedule](#schedule)).
- `Always`: brought up to date at each tick of a scheduled node above it, in that tick's run.
  A node with no scheduled node above it is never run by a tick.
- `Manual`: refreshed by hand only. A tick never runs it, and `barca get` or `barca run` of
  something below it reads its latest successful result, even when stale, with a note on
  stderr. Only `--refresh <name>` or `--refresh-all` recomputes it, also when it is the
  target of `barca get`; a `Manual` task named as the target of `barca run` runs. A
  `Manual` node that was never computed makes `barca get` of what reads it exit 2. A partitioned
  `Manual` node is not pinned yet, and requests to `barca serve` (`POST /get`, `POST /run`)
  compute `Manual` upstreams as before.

See [Scheduling](/scheduling/#always-and-manual).

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
[Large inputs](/recipes/large-inputs/) and [Output Formats and Types](/reference/types/).

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
own environment (the workers inherit the same environment), or when it is not set there from the
project's `.env`, and folds the name and value into the step's run hash:

- Changing a declared variable re-materializes the asset and everything downstream of it. An
  unset variable is its own value, distinct from an empty string.
- Each step's entry in the JSON result carries `"env": {"API_TOKEN": null, "SOURCE_CSV": "b.csv"}`
  (`null` = unset), and `--agent` step lines end with `env API_TOKEN=<unset> SOURCE_CSV=b.csv`.
- Names ending in `_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD` (any case, or the bare word) are
  hashed but shown as `<redacted>` in every output.
- `barca list` shows declared names in an ENV column, and as `env` in `--json`.
- Nodes that declare no `env` hash exactly as before, so existing caches stay valid.
- A declared variable that is not set in barca's environment is read from the project's `.env`
  (in the project root, next to `barca.toml`), so a pipeline that loads `.env` at import
  (`load_dotenv()`, before a settings class reads the environment) is hashed with the value it
  runs with: editing `SOURCE_CSV=b.csv` in `.env` re-runs `raw` like an `export` does. The
  environment wins over the file, as with `load_dotenv()`'s default. Barca reads the file
  without importing anything, understands `KEY=VALUE` lines, `export `, `#` comments, single and
  double quotes and `${NAME}` / `${NAME:-default}`, and does not set its values for the step:
  a pipeline that uses them loads the file itself. Another dotenv file (`.env.local`, a path
  given to `load_dotenv(...)`) is not read; declare such variables by exporting them.

`env=` is also accepted on `@task` and `@sensor`. Those always run, so there it only records the
values each run used.

**Undeclared reads are reported, not hashed.** Environment variables your code reads without
declaring them are not part of the cache key, so changing one does not invalidate anything.
Barca finds such reads statically, in the function and in the project helpers it reaches
(`os.environ["NAME"]`, `os.environ.get("NAME")`, `os.getenv("NAME")`, pydantic settings
fields), and prints one warning line for the command before anything runs: how many steps
read undeclared variables, and up to five of their names. The JSON result lists them per step as `undeclared_env` (also in `--dry-run` and
`barca status --json`, experimental), and the `warnings` array has one `undeclared_env` item per
variable. [The Cache: Environment variables](/reference/cache/#environment-variables), has the exact rules.

### Heavy steps (`weight=`)

Experimental (0.29). Independent steps run at the same time, one per worker. Under a hard
memory limit, several heavy steps that become ready together can add up past it, and the
kernel kills one (`worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)`,
see [A worker that dies](/reference/cli/exit-codes/#a-worker-that-dies)). `weight=` limits how many of
them run at once without lowering `BARCA_POOL_SIZE` for every step.

```python
from barca import asset

@asset(weight=2)
def embed_a() -> dict:
    return {"rows": 1}

@asset(weight=2)
def embed_b() -> dict:
    return {"rows": 2}

@asset(weight=2)
def embed_c() -> dict:
    return {"rows": 3}
```

- The weights of the steps running at once add up to at most the **capacity**: the pool size,
  unless `[execution] capacity` in `barca.toml` or `BARCA_CAPACITY` sets it (the environment
  variable wins). With a capacity of 4 the three assets above run two at a time.
- The pool size is `BARCA_POOL_SIZE`, else `[execution] pool_size`, else the available cores, or
  the capacity when it is smaller: with `capacity = 1` a run starts one worker.
- `weight` is a whole number of at least 1 (default 1); anything else is a parse error. It is
  accepted on `@asset`, `@task` and `@sensor`, and it is not part of the run hash: outputs and
  cache decisions are the same at any weight or capacity.
- A step whose weight is above the capacity runs alone rather than never, and each command that
  plans it warns on stderr.
- Steps start in the order they become ready: a heavy step waiting for room is not overtaken by
  lighter steps that became ready after it.
- Partition keys take their node's weight. `parallel()` branches take the weight of the step
  that called `parallel()`; that step is not counted while it waits for them.
- The capacity is per run: two runs at the same time (two commands, or `barca serve` running two
  schedules at once) each fill their own. `barca serve --capacity N` (`[server] capacity`) adds
  one capacity that every run of the server shares, admitted first come, first served across
  runs; see [Scheduling](/scheduling/#heavy-rebuilds-beside-a-frequent-job).
- `barca plan` shows `pool_size`, `pool_source`, `capacity` and `weights`, `--dry-run` shows
  `capacity` and each heavy step's `weight`, and `barca doctor` reports the capacity and the
  pool size.

### Early cutoff (`cutoff=True`)

Experimental (0.31). What reads an asset folds in its run hash, so when the asset reruns (its
code, an upstream or a sensor it reads changed) everything below it reruns, even when it wrote
the same bytes. With `cutoff=True`, what reads it folds in the SHA-256 of the bytes it wrote
instead: a rerun that writes the same bytes leaves its consumers cached. A global step that
every partition key reads is where this pays: one rerun with unchanged output no longer reruns
every key.

```python
from pathlib import Path
from barca import asset, partitions, sensor

@sensor()
def rate_feed() -> float:
    return float(Path("rate.txt").read_text())

@asset(inputs={"raw": rate_feed}, cutoff=True)
def rates(raw: float) -> dict:
    return {"usd_eur": round(raw, 1)}

@asset(inputs={"rates": rates}, partitions={"customer": partitions(["acme", "globex"])})
def billed(customer: str, rates: dict) -> dict:
    return {"customer": customer, "eur": 100 * rates["usd_eur"]}
```

- The digest is of the serialized artifact (the `content_hash` every materialization records),
  per key for a partitioned asset; a `collect()` fan-in reads every key's digest by key.
- What reads it still reruns when its own code, its other inputs or its declared env change.
- What reads it is decided after it ran, in a later phase. A dry run or `barca status` cannot
  know the bytes of a cutoff asset that would run: what reads it is `unknown` with reason
  `cutoff_pending`.
- `cutoff` is not part of the asset's own run hash, but turning it on or off reruns what reads
  it once.
- A `--refresh` cascade stops at it: its consumers are decided by the bytes the refresh wrote.
- A dict `partitions_from` source with `cutoff=True` gives each key its own value only, as a
  sensor source does.
- On `@asset` only. A nondeterministic asset writes new bytes every rerun, so cutoff saves
  nothing on it.

[External Data and Early Cutoff: Early cutoff](/reference/external-data/#early-cutoff-assetcutofftrue) has the details: what is hashed, parquet and library
versions, server mode, sinks, and why it is opt-in.

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
    weight: int = 1,
)
```

Declares a sensor: a function that looks at something outside the pipeline (a directory, a
bucket, an API) and returns a value that identifies its current state. A sensor has no inputs
(`sensor '...' cannot have inputs`, exit 2) and runs on every `barca get` or `barca run` whose
cone includes it. See `@asset` above for `env`, `retries` and `retry_backoff`.

`partitions` runs the sensor once per key, with the key passed as the parameter named in the
dict; each key's value is stored on its own. `serializer` forces the format the value is stored
in (`"json"`, `"pickle"`, `"parquet"` or `"csv"`).

```python
from pathlib import Path

from barca import asset, sensor, Schedule


@sensor(freshness=Schedule("*/5 * * * *"))
def inbox_files() -> list[str]:
    files = sorted(str(f) for f in Path("inbox").glob("*.csv"))
    return files


@asset(inputs={"files": inbox_files})
def loaded(files: list[str]) -> dict:
    return {"n": len(files)}
```

**Return value.** Return a version value (mtime, ETag, file list) for content comparison.
Return `True`/`False` for an explicit change signal, or `(changed, value)` to also pass data.
`True` advances freshness even without data; `False` retains the previous observation
and its value. Consumers receive only the optional value. Through 0.32.0 the flag was
ignored: replace `(True, etag)` with `etag` when comparison is intended.

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

**Freshness.** The default is `Manual`. A sensor is never pinned: it is polled whenever a run
needs it, whatever its freshness. `Schedule("<cron>")` makes `barca serve` poll it on that cron;
the same tick brings up to date the `Always` assets and tasks below it, so when its value
changes they are recomputed or fired in that run. `@sensor(freshness=Always)` is accepted
without an error.

See [Sensors and External Observations](/examples/sensors/) for
a walk-through, and [Assets, Sensors and Inputs: Sensors](/reference/assets/#sensors).

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
    weight: int = 1,
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

Run a task with [`barca run`](/reference/cli/run/). Upstream assets are served from cache, as
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
Schedule("0 5 * * *")                   # 5-field cron: daily at 05:00
Schedule("*/15 * * * * *")              # 6-field cron: every 15 seconds
Schedule("*/5 * * * *", max_failures=3) # skip ticks after 3 consecutive failed ones
Schedule("* * * * *", timeout_seconds=50)  # stop a tick's run after 50 seconds
```

A freshness value for `freshness=` on `@asset`, `@sensor` or `@task`. It has an effect only
while `barca serve` is running; `barca get` and `barca run` ignore it. `barca list` shows the
cron and the next fire time.

The cron is passed by position. `max_failures=` is keyword-only and must be a positive integer
literal; the default is 5. It caps the consecutive failed ticks of the node: past it, its ticks
are skipped until an explicit request for it or for a node that failed, or a new result of one
of a failed node's inputs, succeeds. `GET /schedule` reports `failures`, `max_failures` and
`failing`. `timeout_seconds=` is keyword-only, a whole number of seconds (0: no limit): how long
a run started by one of its ticks may take under `barca serve`, over `--run-timeout` and `[server]
run_timeout_seconds` (no limit by default); a run that takes longer is recorded as `error`, `run
timed out after Ns (Schedule(timeout_seconds=) of NODE)`. Neither is part of the run hash. Any
other keyword, `cron=` included, is an error when the file is read (exit 2).

The cron has 5 fields (`minute hour day-of-month month day-of-week`) or 6 with a leading
seconds field. There is no year field. See [Scheduling](/scheduling/) for what a tick does.

The helpers (`partitions`, `collect`, `asset_ref`, `@sink`, `parallel`) and the other names barca
exports are on [Helpers API](/reference/api/helpers/).
