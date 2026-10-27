---
title: Decorators API
description: Arguments and behavior of @asset, @sensor, @task, @sink and @unsafe, the freshness markers, partitions, parallel() and the other names barca exports.
---

Everything here is imported from `barca`. The decorators return the function unchanged: the
`barca` binary extracts their arguments from source without importing your module. Literal
partition key lists need no resolver import; a nonliteral `partitions(<expression>)` imports
the pipeline module to evaluate its keys. Arguments must therefore be written literally (a dict literal for `inputs=`, a list of
string literals for `env=`; "Values barca reads" below), or barca exits 2; a decorator built in a loop or installed through a runtime assignment is not
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
default 300.

| Argument | Form |
|---|---|
| `name=`, `description=` | a string literal |
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

- The path of `@sink`. A path that is not a string literal (`@sink(EXPORT_PATH)`) declares no
  sink, without a message.
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
decorator: editing them re-runs nothing. `barca docs cache` has the full list with the reason
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
variable. `barca docs cache`, "Environment variables", has the exact rules.

### Heavy steps (`weight=`)

Experimental (0.29). Independent steps run at the same time, one per worker. Under a hard
memory limit, several heavy steps that become ready together can add up past it, and the
kernel kills one (`worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)`,
see [A worker that dies](/reference/cli/#a-worker-that-dies)). `weight=` limits how many of
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

`barca docs cache` ("Early cutoff") has the details: what is hashed, parquet and library
versions, server mode, sinks, and why it is opt-in.

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
$ barca get summary pipeline.py -o value --json
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

On an unpartitioned `upstream` that returns a dict (0.29.6):

- the dict's keys are the keys, and each key is called with its own value, as the parameter
  named after the upstream (or the `inputs=` key that names it); a function without that
  parameter gets the key only;
- each key's run hash folds in its own value (canonical JSON, keys sorted) and the upstream's
  run hash, not the whole dict: when one value changes, only that key runs, and the keys of a
  `partitions_from` chain below it;
- keys must be strings or integers (an integer becomes its string) and values JSON-writable;
  anything else, or a return that is neither a list nor a dict, fails the upstream step with an
  error naming the key or the expected types;
- `--dry-run --json` and `barca status --json` report per key what changed
  (`partitions.changed`, `value ID` for a new value). See
  [Only What Changed, Across Many Partitions](/patterns/09-changed-partitions/).

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
- `barca get sales` on a partitioned asset returns every key: one artifact path per key, and a
  `final_output` object keyed by partition key (`region=emea`); past 1000 keys, only the keys
  the run computed ([many partition keys](/reference/cli/#many-partition-keys)). `--partition region=emea`
  selects keys across the run (upstream partitioned assets too; a `collect()` over them is
  skipped), computes only those and scopes `--refresh sales` to them
  ([CLI reference](/reference/cli/#get)). [`barca sql`](/reference/sql/) has one view with a
  `partition` column holding `region=emea`.
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
    partition_by: list[str] | None = None,
    rows_per_file: int | None = None,
)
```

Stacked under `@asset`, it writes the asset's result to an extra path each time the asset
runs. A cache hit writes the sink only when it holds another run hash's output (below). Paths are local or any fsspec URI (`abfss://`,
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

- The format comes from `serializer=` (`json`, `pickle`, `parquet`, `csv`), else the path's extension
  (`.json`, `.pkl`, `.pickle`, `.parquet`, `.csv`), else the asset's own artifact format.
- A parquet sink needs a DataFrame, Arrow table or DuckDB relation. Anything else is a sink
  failure.
- Writes go to a temporary file first and are then renamed or uploaded, so a crash does not
  leave a partial file at the destination.
- No other node may take a sink as an input.
- A sink that fails fails the step (from 0.30.0): the step's error is a `SinkError` naming the
  sink, the run exits 1 with a `step_failed` envelope, and the step is not recorded. Every sink
  of the step is still attempted. Up to 0.29 a failed sink printed `[barca] SINK FAILED: ...` on
  stderr and the asset succeeded.
- On a partitioned asset each key writes its own file, with the key inserted before the
  extension: `@sink("./exports/sales.json")` on keys `region=emea` and `region=amer` wrote
  `exports/sales_region_emea.json` and `exports/sales_region_amer.json`.
- Barca records the run hash each sink last published, per partition key (from 0.34.0). A cache
  hit whose run hash differs (code A, then B, then A again) writes the sink again from the
  cached artifact without running the step, and prints `[barca] sink out.json: republished the
  cached result of banana` on stderr; one that matches writes nothing. Barca does not check
  what is at the path, and the record is per machine: a remote sink another machine wrote is
  not known here. Up to 0.33.1 a cache hit never wrote its sinks, so after A, B, A the sink
  held B.

### Dataset sinks

A path ending in `/` is a dataset sink: a Hive-partitioned dataset with one directory level
per partition dimension of the asset, in declared order. `partition_by=[...]` adds levels from
columns of the returned table; `rows_per_file=` caps the rows of each parquet file.

```python
import pandas as pd
from barca import asset, partitions, sink

@asset(partitions={"retailer": partitions(["r1", "r2"])})
@sink("s3://bucket/exports/scored/", partition_by=["order_date"])
def scored(retailer: str) -> pd.DataFrame:
    ...
# s3://bucket/exports/scored/retailer=r1/order_date=2026-10-22/part-0.parquet
```

```sql
select * from read_parquet('s3://bucket/exports/scored/**/*.parquet', hive_partitioning = true)
```

- Parquet (zstd) for a table by default; `serializer="csv"` or `"json"` (JSON Lines,
  `part-0.jsonl`) write one file per partition. Partition columns live in the path, not in the
  files.
- DuckDB writes a returned relation (streaming), polars a polars frame, pyarrow a pandas frame
  or an Arrow table; all three produce the same layout and file names.
- A cached key writes nothing unless its directory holds another run hash's output; a key that
  runs is written to staging and swapped in for its directory (one rename on a local disk; on
  an object store the new files are uploaded, then the old ones deleted). A key the asset no longer has is deleted after the run.
- The sink owns its prefix and marks it with `_barca_sink.json`; a prefix that holds files
  barca did not write, or that another asset owns, is refused.
- A write that creates more than 1,000 directories, or at least 100 files averaging under 1 MB,
  prints a warning: partition by a coarser column.
- `partition_by=` and `rows_per_file=` on a file sink (no trailing `/`) are a usage error,
  exit 2.

`barca docs sinks` has the details and how to choose partition columns.

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

## Other exports

| Name | What it is |
|---|---|
| `duckdb_connection()` | The DuckDB connection barca binds `duckdb.DuckDBPyRelation` inputs to, one per worker process. Configure it at import time of your module. See `barca docs types`. |
| `ParallelError` | What `parallel()` returns in place of a branch that raised. |
| `BranchResultError` | Raised by `parallel()` when a branch returned a value that cannot be passed back to the caller. |
| `get`, `run`, `plan`, `history`, `stats` | Python functions that start the `barca` binary and return parsed results. See `barca docs agents`, "Getting values, not pointers". |
| `BarcaError` | Raised by those functions; carries `kind`, `code`, `remediation`, and for a failed step `node`, `traceback`, `artifact_dir`, `phase`. |
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

### CSV outputs

`@asset(serializer="csv")`, `@sensor(serializer="csv")` and `@task(serializer="csv")`
write frames as CSV. `@sink("exports/orders.csv")` infers CSV from the extension.
Pandas, Polars (including LazyFrame), Arrow tables and DuckDB relations are supported.
Writers include column headers and omit the pandas index. Parameter annotations select
pandas (the default), Polars, Arrow or DuckDB readers. Readers infer types; CSV does not
preserve exact dtypes, indexes, null/empty-string distinctions or nested columns.
Unsupported values fail rather than falling back to pickle. Remote lazy CSV inputs are
materialized before exposing their lazy interface. Frames still default to Parquet.
CSV artifacts are also available to `barca sql` and `barca status --sample`.

Task return values appear in `barca run` results as asset values do. Captured stdout is
available through `barca logs RUN_ID --pretty` or `--json`, separately from progress.
JSON run/get output suppresses step prints. In other modes stderr prefixes each line
with its node id; framework agent lines retain `[barca]` prefixes.
