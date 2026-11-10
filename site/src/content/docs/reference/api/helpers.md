---
title: Helpers API
description: partitions, partitions_from, collect, asset_ref, @sink, parallel and parallel_map, qualified imports, and the other names barca exports.
---

The helpers that go with the decorators on [Decorators API](/reference/api/decorators/): fanning
an asset out over keys and collecting them, naming an input by id, writing a result to an extra
path, running tasks in parallel, and every other name `barca` exports.

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
  [Only What Changed, Across Many Partitions](/recipes/changed-partitions/).

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
  the run computed ([many partition keys](/reference/cli/get/#many-partition-keys)). `--partition region=emea`
  selects keys across the run (upstream partitioned assets too; a `collect()` over them is
  skipped), computes only those and scopes `--refresh sales` to them
  ([CLI reference](/reference/cli/get/)). [`barca sql`](/reference/cli/sql/) has one view with a
  `partition` column holding `region=emea`.
- `barca status` shows a partitioned asset as one node with `partitions: {total, cached,
  missing, missing_keys}`.

See [Partitions](/reference/partitions/).

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
one asset. The path is a string literal: a constant, an f-string or a concatenation
(`@sink(EXPORT_PATH)`) is an error when the file is read, exit 2 ([Decorators API: Values barca reads](/reference/api/decorators/#values-barca-reads)).
Up to 0.33.1 such a sink was dropped without a message and never written.

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

[Sinks](/reference/sinks/) has the details and how to choose partition columns.

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
belongs in a task. See [Parallel Tasks](/recipes/parallel-tasks/).

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

Literal partition key lists need no import; a nonliteral `partitions(<expression>)` imports
the pipeline module in a subprocess to evaluate its keys. A decorator built in a loop or
installed through a runtime assignment is not seen. The signatures here are those of
`python/barca/__init__.py`; unless a version is named, the behavior was checked by running
0.18.0.

## Other exports

| Name | What it is |
|---|---|
| `duckdb_connection()` | The DuckDB connection barca binds `duckdb.DuckDBPyRelation` inputs to, one per worker process. Configure it at import time of your module. See [Output Formats and Types](/reference/types/). |
| `ParallelError` | What `parallel()` returns in place of a branch that raised. |
| `BranchResultError` | Raised by `parallel()` when a branch returned a value that cannot be passed back to the caller. |
| `get`, `run`, `plan`, `history`, `stats` | Python functions that start the `barca` binary and return parsed results. See [Scripts and AI Agents: Getting values, not pointers](/reference/agents/#getting-values-not-pointers). |
| `BarcaError` | Raised by those functions; carries `kind`, `code`, `remediation`, and for a failed step `node`, `traceback`, `artifact_dir`, `phase`. |
| `Client`, `Run` | HTTP client for `barca serve`. See [Server API](/reference/server-api/#python-client). |
| `sql()` | Query cached results with DuckDB from Python, as `barca sql` does. See [SQL](/reference/cli/sql/). |
| `signed_url()` | A time-limited HTTPS link to an object in S3, GCS or Azure. See [Remote Storage](/reference/remote-storage/#sharing-a-result-signed-urls). |
| `batches()`, `batches_by()`, `Failed` | Grouping for `batch=`, and the value that fails one key of a batched call alone. See [Batched Partitions](/reference/batched-partitions/). |
| `group()`, `Group` | Organize nodes in the UI and CLI without changing execution or caching. See [Groups](/reference/groups/). |

DuckDB steps share one default connection per worker process and execute sequentially.
Import-time setup remains available to later steps. User-created objects and session settings
persist, including after failures; Barca cleans up its bound input views after materialization.
Avoid project catalog names that collide with input parameters: existing tables are skipped
during binding, and existing views can be replaced and then dropped. For naming and cleanup
patterns, see [Anti-Patterns](/recipes/anti-patterns/#fixed-names-on-duckdbs-shared-connection).

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

### Refreshing a partition set from Python

```python
import barca

result = barca.get(
    "final", "pipeline.py",
    refresh=["source[account=A]", "source[account=C]"],
)
```

`barca.run` accepts the same `refresh` list. A keyed selector names one dimension and value;
list entries select a set. These refreshes retain the target's full partition domain, reuse
fresh siblings and let complete `collect()` publishers read refreshed and cached results.
Cascades respect cutoff bytes and `cascade=False`. `partition=` still narrows the domain.
