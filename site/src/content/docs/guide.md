---
title: Guide
description: Where results live, how to see what is cached and why, and how sensors, tasks, partitions and projects with several files work.
---

This guide follows [Getting Started](/getting-started/), which installs barca and runs two
assets. The examples here were checked against barca 0.27.0. Each section names the manual topic
(`barca docs <topic>`) that has the full rules.

## 1. Assets and inputs

An asset is a Python function whose result barca caches. It names its inputs with `inputs=`:
each key is a parameter of the function, and each value is the upstream function whose result
it receives.

```python
from barca import asset


@asset()
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]


@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "total": sum(d["x"] for d in data)}
```

```bash
barca get pipeline.py             # every asset in the file; prints the last asset's artifact path
barca get summary pipeline.py     # one asset and what it depends on
```

- The target comes first, then the files.
- An asset runs again when its own code changes or when one of its inputs does. Edit
  `summary` and only `summary` runs. Edit `raw_data` and both run.
- `inputs=` has to be written literally in the decorator, because barca reads it from the
  source without importing the file. Inputs built in a loop or passed through a variable are
  not seen.
- The decorator returns the function unchanged, so `summary` can be imported and called in a
  unit test like any other function.
- Assets that do not depend on each other run in parallel, in separate worker processes.

More: `barca docs assets`.

## 2. Where results live

A run creates `.barca/` in the project root:

```
.barca/metadata.db                          run history and the record of what is cached
.barca/artifacts/<node>/<run_hash>.<ext>    one file per result: .json, .pkl or .parquet
.barca/envs/<env>/                          the same two, for each --env other than the default
```

- **The project root** is the nearest directory at or above the current one that holds a
  `barca.toml` (an empty file is enough). Without one, it is the current directory.
- **Do not commit `.barca/`.** Barca writes a `.gitignore` inside it, so git already ignores
  it and nothing needs adding to your own.
- **Results are cached by run hash.** The run hash covers the function's code, the helpers
  it uses from your project, and the run hashes of its inputs. It does not cover the bytes
  of the output.
- **The format follows the returned value.** A dict, list, string or number is stored as
  json. A DataFrame, Arrow table or DuckDB relation is stored as parquet. Anything else is
  pickled (`barca docs types`).
- **Results are written locally first.** A remote store is this machine's backup and
  read-through cache: set `BARCA_REMOTE_URI` (or `[remote] uri` in `barca.toml`) and every
  result and a backup of the history are uploaded there. See
  [Remote storage](/reference/remote-storage/). Sharing results between machines means a
  server they all point at (`barca docs server-mode`, [Deploying](/deploying/)).
- **`--env <name>`** (or `BARCA_ENV`) keeps a separate cache and history, so dev and prod
  do not share results.

Do not delete files under `.barca/` to force a recompute. Use `--refresh` (section 4). Every
file under `.barca/` is listed in [Configuration](/reference/config/#where-things-live).

More: `barca docs cache`, `barca docs remote`.

## 3. Seeing what barca sees

None of these commands runs a step.

```bash
barca list                            # every asset, sensor and task, with its inputs
barca status                          # per node: cached or stale and why, last run, shape of the result
barca get summary --dry-run           # what this exact command would run or serve from cache
barca plan pipeline.py                # the execution plan as JSON: phases, and the steps in each
barca sql "select * from raw_data"    # query a cached result with DuckDB
```

[Getting Started](/getting-started/#change-the-code) shows the output of `status` and
`--dry-run`. `--dry-run` works on `get` and `run` and takes the same flags, so it also
previews a `--refresh`.

`barca sql` runs a DuckDB query over the results already on disk. Every node with a json or
parquet result is a view named after its function:

```
$ barca sql "select count(*) as n, sum(x) as total from raw_data"
n  total
3  6
```

It returns at most 100 rows unless you pass `--limit N` or `--all`, shows the last result even
when the asset is stale (stderr says so). Install SQL support with `uv add 'barca[sql]'`. See [barca sql](/reference/sql/).

`get`, `run`, `list`, `status`, `sql`, `history` and `stats` print a table or summary in a
terminal and JSON when piped or captured; `--json` and `--pretty` override that. Progress and
errors go to stderr. The JSON shapes and exit codes are in the
[CLI reference](/reference/cli/#output-format), and the [agent skill](/reference/agent-skill/)
is a short guide for AI agents.

More: `barca docs status`, `barca docs sql`.

## 4. Sensors: data from outside

An asset that reads a file, a bucket or a table in its own body has the same code and the
same inputs on every run. Barca computes it once and then serves it from cache, even after
the data changes.

The supported pattern is a `@sensor` in front of the asset. A sensor returns
`(update_detected, value)`. It runs every time, and its `value` is part of the run hash of
every asset that reads it.

```python
import hashlib
from pathlib import Path

from barca import asset, sensor


@sensor()
def orders_version() -> tuple[bool, str]:
    # anything that identifies the version of the data: an etag, a last-modified time
    return True, hashlib.md5(Path("orders.csv").read_bytes()).hexdigest()


@asset(inputs={"version": orders_version})
def orders(version: str) -> list:
    return Path("orders.csv").read_text().splitlines()[1:]


@asset(inputs={"rows": orders})
def order_count(rows: list) -> int:
    return len(rows)
```

With an `orders.csv` of a header and two rows:

```bash
barca get order_count pipeline.py    # 3 steps run; the value is 2
barca get order_count pipeline.py    # 1 step runs: the sensor. orders and order_count are cached
# ... a row is appended to orders.csv ...
barca get order_count pipeline.py    # 3 steps run; the value is 3
```

- The asset receives only `value` (here `version: str`), not the tuple. The `bool` is not
  used for caching.
- Return only what identifies the data. A value that changes on every run, such as a
  timestamp, re-runs the sensor's consumers every time.
- A sensor has no inputs.
- The cache is keyed by the value, not by time. If the sensor returns a value it returned
  before, the result computed for that value is served.
- `--dry-run` and `barca status` run nothing, so they assume the sensor returns the value
  it returned last time.

[Sensors and External Observations](/workflows/06-sensors-and-external-observations/) walks
through each of these with output.

### Recomputing by hand: `--refresh`

```bash
barca get order_count pipeline.py --refresh orders                # orders and everything downstream of it
barca get order_count pipeline.py --refresh orders --no-cascade   # only orders; barca warns that order_count is out of date
barca get order_count pipeline.py --refresh-all                   # every asset the target depends on
```

`--refresh` takes one comma-separated list (`--refresh a,b`), and `barca run` takes the same
three flags. Use it also after a change the run hash does not see: an
installed package, a module outside the project root, or an environment variable the
function reads without declaring it in `@asset(env=[...])`.

More: `barca docs cache` has the complete list, under "Not followed".

The run hash covers the function and every project definition it uses, followed through
imports, decorators, defaults and the other places Python can reach a helper. Where barca
cannot tell statically which definition a name means (a module-level `if`, a star import), it
includes more rather than less, so an edit nearby can re-run a step that did not need it. The
exact rules are in `barca docs cache`, "What the definition covers" and "Followed
conservatively".

## 5. Tasks, and `get` versus `run`

A task is a step that does something: deploy, notify, upload. It is never cached.

```python
from barca import asset, task


@asset()
def daily_report() -> dict:
    return {"revenue": 42000, "orders": 150}


@task(inputs={"report": daily_report})
def notify(report: dict) -> None:
    print(f"Daily revenue: {report['revenue']}")
```

```
$ barca run notify pipeline.py
[pipeline.py:notify] Daily revenue: 42000
[barca] 1/2 steps | done in 0.0s
Run 14d08771a6ca | ran 'notify' in 0.167s (1 step, 1 phase)

Value:
null
```

| | `barca get <asset> [files]` | `barca run <task> [files]` |
|---|---|---|
| Target | an asset or a sensor | a task |
| The target itself | runs only if it is not cached | always runs |
| Assets it depends on | run only if not cached | run only if not cached |
| No target | every asset and sensor; tasks are skipped and named on stderr | not allowed |
| Several targets | `barca get a,b` | `barca run a,b` |

- Using the wrong command, or putting a file before the target, exits 2 with a message
  that names the right command.
- A task can depend on assets, sensors and other tasks. An asset or a sensor cannot depend
  on a task.
- An input whose name starts with `_` is for ordering only: the step runs after it, the
  value is not loaded, and the parameter receives `None`.

More: `barca docs tasks`.

## 6. Large inputs

A DataFrame, Arrow table or DuckDB relation is stored as parquet. How a downstream step
reads it depends on the annotation of the parameter that receives it:

| Annotation | What is read |
|---|---|
| none, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table` | the whole file, before the function runs (no annotation means pandas) |
| `duckdb.DuckDBPyRelation`, `pl.LazyFrame` | nothing before the function runs; then the columns the step's query uses |

A declared input is loaded whether or not the function uses it, and barca warns at plan time
about one the function never mentions. `barca get` on an asset stored as parquet prints a
pointer to the file, not the rows; use `barca sql` to look at it.
[Large inputs](/patterns/08-large-inputs/) has a worked example and the limits.

More: `barca docs big-inputs`, `barca docs types`.

## 7. Partitions

Partitions run one asset once per key. The keys run in parallel, and each key is cached on
its own.

```python
from barca import asset, collect, partitions, partitions_from

REGIONS = ["emea", "amer", "apac"]


@asset(partitions={"region": partitions(REGIONS)})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(partitions={"region": partitions_from(sales)})    # same keys as sales
def margin(region: str, sales: dict) -> dict:
    return {"region": region, "margin": sales["revenue"] * 0.2}


@asset(inputs={"all_sales": collect(sales), "margins": collect(margin)})
def total(all_sales: list[dict], margins: list[dict]) -> dict:
    return {"revenue": sum(s["revenue"] for s in all_sales), "margin": sum(m["margin"] for m in margins)}
```

```bash
barca get total pipeline.py     # 7 steps: three keys each of sales and margin, then total
barca get total pipeline.py     # 0 steps
# ... "latam" is added to REGIONS ...
barca get total pipeline.py     # 3 steps: sales and margin for latam, then total
```

- The key is passed as the parameter named in `partitions={...}`, here `region`.
- `partitions_from(sales)` gives another asset the same keys. Each key receives that key's
  result of `sales`, as the parameter named `sales`.
- `collect(sales)` gives a downstream asset every key's result as one list. An
  unpartitioned asset that names a partitioned one in `inputs=` without `collect()` is a
  usage error (exit 2).
- An unpartitioned asset or a sensor in the `inputs=` of a partitioned asset is passed whole
  to every key. Changing it re-runs every key.
- `barca status` shows the asset as one row, and as `partial` when only some keys are cached.
- In `barca sql` a partitioned asset is one view over every key, with a `partition` column:

```
$ barca sql "select * from sales"
partition     region  revenue
region=amer   amer    400
region=apac   apac    400
region=emea   emea    400
region=latam  latam   500
```

Limits:

- **A new key runs alone however the keys are written:** a literal list inside the decorator,
  a module-level constant as above, another expression, or `partitions_from(...)`.
- **`barca get sales` returns every key**: one artifact path per key (`-o value` for the
  values). Past 1000 keys it lists only the keys the run computed and counts the rest
  ([many partition keys](/reference/cli/#many-partition-keys)). `--partition region=emea` computes and returns only that key, and scopes
  `--refresh sales` to it. It narrows upstream partitioned assets too, and skips a `collect()`
  fan-in over them.
- **A removed key's result stays on disk** until the cache size limit evicts it, and its runs stay
  in the history. `barca sql` shows only the current keys: with `latam` removed again, the view
  above has three rows.

[Parametrized Assets and Partitions](/workflows/03-parametrized-assets-and-partitions/) shows
each of these with output. More: `barca docs partitions`.

## 8. Projects with several files

With no file arguments, barca reads every `.py` file under the project root that imports
barca and builds one graph from all of them.

```
my_project/
  barca.toml              empty; marks the project root
  helpers.py              a plain module, no barca import
  pipelines/
    sources.py            @asset rows
    report.py             @asset total, which reads rows
```

```python
# pipelines/sources.py
from barca import asset

from helpers import clean


@asset()
def rows() -> list:
    return clean([1, 0, 2])
```

```python
# pipelines/report.py
from barca import asset

from pipelines.sources import rows


@asset(inputs={"rows": rows})
def total(rows: list) -> int:
    return sum(rows)
```

```bash
barca get total                 # from the root or any directory below it
barca get total pipelines/      # read only the files under pipelines/
```

- An input from another file is imported by its path from the project root
  (`from pipelines.sources import rows`) and then named in `inputs=`. Barca resolves the import
  from the source. A bare `from sources import rows` next to it is a usage error (exit 2) that
  names the qualified import to use, because the same file would have two module names.
- A node's id is `<file>:<function>`, with the file relative to the project root
  (`pipelines/report.py:total`). A bare name such as `total` works as a target when only one
  file defines it.
- Steps run with the project root as their working directory.
- `helpers.py` is never passed on the command line, but it is part of the cache key: an
  asset's run hash covers the definitions it uses from your project's modules. Editing
  `clean`, or anything `clean` calls, re-runs `rows` and what depends on it. Editing another
  function in `helpers.py` re-runs nothing.
- The standard library, installed packages, modules outside the project root
  and imports built at run time are not followed. After changing one of those, use
  `--refresh`.

Which directories and files a walk skips, and the `[discovery]` settings that change it, are
in [Discovery](/reference/discovery/).

More: `barca docs discovery`, `barca docs cache`.

## 9. Schedules

`freshness=Schedule("<cron>")` on an asset, task or sensor makes `barca serve` run it on that
cron schedule. `barca get` and `barca run` do not look at the clock.

```python
from barca import task, Schedule


@task(freshness=Schedule("*/10 * * * *"))      # every 10 minutes
def refresh() -> None:
    print("tick")
```

```bash
barca list job.py                     # shows each schedule and its next fire time
barca serve job.py --timezone utc     # scheduler, HTTP API and web UI on 127.0.0.1:8274
```

A scheduled task runs on every tick. A scheduled asset is brought up to date on every tick,
so it runs only if it is not cached: outside data still has to come in through a sensor
(section 4). The same tick also brings up to date every `Always` node below the scheduled
one (`Always` is the default), so a schedule on a sensor keeps what reads it current. A `Manual`
node is refreshed by hand only: ticks skip it, and what reads it uses its last result.

[Scheduling](/scheduling/) covers what a tick does, `Always` and `Manual`, and failed ticks, and
[Deploying](/deploying/) covers running the server.

## Where to go next

- `barca docs` in the terminal: the manual, by topic. `barca <command> --help` ends with
  examples.
- [CLI reference](/reference/cli/) and [Configuration](/reference/config/).
- [Patterns](/patterns/01-asset-to-asset/) for common shapes and what to avoid.
- `barca history` and `barca stats <asset>` for past runs and timings.
