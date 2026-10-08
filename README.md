<h1 align="center">barca</h1>

<p align="center">Runs Python functions as a dependency graph and caches their results.</p>

<p align="center">
  <a href="https://pypi.org/project/barca/"><img alt="PyPI" src="https://img.shields.io/pypi/v/barca?style=flat-square&color=3572A5" /></a>
  <img alt="Python" src="https://img.shields.io/badge/python-%E2%89%A53.12-3572A5?style=flat-square" />
  <img alt="Rust" src="https://img.shields.io/badge/rust-2024_edition-dea584?style=flat-square" />
  <a href="https://github.com/barca-orc/barca/blob/main/LICENSE"><img alt="License" src="https://img.shields.io/github/license/barca-orc/barca?style=flat-square" /></a>
</p>

---

Barca runs Python functions as a dependency graph and caches their results.

You mark functions with `@asset`, `@sensor` or `@task` and declare their inputs. The
`barca` binary, written in Rust, reads the source without importing it, works out what
needs to run, and runs it in Python worker processes. Results are stored as files under
`.barca/`. An asset runs again only when its code or its inputs change; sensors and tasks
run every time.

There is no server to run and no configuration file to write. `barca serve` adds
schedules, an HTTP API and a web UI when you want them.

```python
# pipeline.py
from barca import asset


@asset()
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]


@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "total": sum(d["x"] for d in data)}
```

```
$ barca get pipeline.py
[barca] 2/2 steps | done in 0.0s
Run 51fabebab1c0 | all assets in 0.135s (2 steps, 1 phase)

Value:
{
  "count": 3,
  "total": 6
}

$ barca get pipeline.py
Run 51fa8fe09d90 | all assets in 0.005s (0 steps, 1 phase)
...
```

The second run executes 0 steps: both results come from the cache. Output shown in this
file is from barca 0.18.0.

## Install

```bash
uv add barca          # or: pip install barca
```

Python 3.12 or later. The wheel contains the `barca` binary, the decorators and a Python
API; wheels are published for macOS on Apple Silicon and x86-64 Linux with glibc. [uv](https://docs.astral.sh/uv/) is recommended, not required: barca runs your steps
with the Python of the environment it is installed in, or `python3` on `PATH`.

Optional extras: `barca[parquet]` (pyarrow, for pandas DataFrames), and `barca[s3]`,
`barca[gcs]`, `barca[azure]` or `barca[remote]` for a remote store. `barca sql` needs
`duckdb` installed in the same environment.

## Where results live

```
.barca/metadata.db                          run history and the record of what is cached
.barca/artifacts/<node>/<run_hash>.<ext>    one file per result: .json, .pkl or .parquet
.barca/envs/<env>/                          the same two, for each --env other than the default
```

- `.barca/` is created in the project root: the nearest directory at or above the current
  one that holds a `barca.toml` (an empty file is enough), else the current directory.
- Do not commit `.barca/`. Barca writes a `.gitignore` inside it, so git already ignores
  it and nothing needs adding to your own.
- Results are cached by run hash, which covers the function's code and its inputs, not
  the bytes of the output.
- Results are always written locally first. A remote store is an optional shared copy:
  set `BARCA_REMOTE_URI=s3://my-bucket/barca/my-project` (or `gs://`, `abfs://`) and other
  machines using the same location get cache hits for what this one computed.
- `--env <name>` (or `BARCA_ENV`) keeps a separate cache and history, so dev and prod do
  not share results.

```bash
barca get summary pipeline.py --env prod
barca status pipeline.py --env prod
```

More: `barca docs cache`, `barca docs remote`.

## Seeing what barca sees

Ask the CLI instead of reading source files or opening files under `.barca/`. None of
these commands runs a step.

```bash
barca list                                # every asset, sensor and task, with its inputs
barca status                              # per node: cached or stale and why, last run, rows and columns
barca get summary --dry-run               # what this exact command would run or serve from cache
barca plan                                # the execution plan as JSON
barca sql "select * from raw_data"        # query a cached result with DuckDB
```

After changing `summary` in the example above:

```
$ barca status
NAME      KIND   STATE   WHY           LAST RUN                           SHAPE           DEPS
raw_data  asset  cached  materialized  success 2026-10-07 18:14:53 0.00s  3 rows x 1 col  -
summary   asset  stale   changed       success 2026-10-07 18:14:53 0.00s  dict (2 keys)   raw_data

1 cached, 1 stale, 0 never run, 0 partial, 0 unknown, 0 always run
```

`barca sql` makes every node with a json or parquet result a view named after its
function, so you can look at data without writing a step or a script:

```
$ barca sql "select count(*) as n, sum(x) as total from raw_data"
n  total
3  6

$ barca sql "describe raw_data"
column_name  column_type  null  key  default  extra
x            BIGINT       YES
```

At most 100 rows are returned unless you pass `--limit N` or `--all`. Pickled results
cannot be queried.

**Scripts and AI agents.** `get`, `run` and the inspection commands print their result on
stdout: a table or summary in a terminal, JSON when piped or captured (`--json` and
`--pretty` override). Progress and errors go to stderr; in JSON mode an error is one JSON
line with `error`, `code`, `kind` and `remediation`. Exit codes: `0` ok, `1` a step
failed, `2` usage error, `3` barca or infrastructure failure, `130` cancelled.

```
$ barca sql "select * from raw_data where x > 1" --json
{"columns": ["x"], "rows": [{"x": 2}, {"x": 3}], "total": 2, "truncated": false}
```

(Printed indented.) An agent should load
[`SKILL.md`](SKILL.md), also printed by `barca docs skill`. `barca docs agents` is the full
reference, and `barca docs contract` lists every command, flag, exit code and JSON schema,
each marked stable or experimental.

More: `barca docs status`, `barca docs sql`.

## Data from outside comes in through a sensor

An asset that reads a file, a bucket or a table in its own body has the same code and the
same inputs on every run, so it is computed once and then served from cache, even after
the data changes. Put a `@sensor` in front of it that returns something identifying the
current version of the data, and make the asset take the sensor as an input:

```python
import hashlib
from pathlib import Path

from barca import asset, sensor


@sensor()
def orders_version() -> tuple[bool, str]:
    # anything that identifies the version: an etag, a last-modified time
    return True, hashlib.md5(Path("orders.csv").read_bytes()).hexdigest()


@asset(inputs={"version": orders_version})
def orders(version: str) -> list:
    return Path("orders.csv").read_text().splitlines()[1:]


@asset(inputs={"rows": orders})
def order_count(rows: list) -> int:
    return len(rows)
```

```bash
barca get order_count pipeline.py    # 3 steps run
barca get order_count pipeline.py    # 1 step runs: the sensor. orders and order_count are cached
# ... a row is appended to orders.csv ...
barca get order_count pipeline.py    # 3 steps run: the sensor returned a new value
```

A sensor runs every time, and the value it returns is part of the run hash of the assets
that read it. Return only what identifies the data: a value that changes on every run,
such as a timestamp, re-runs those assets every time.

To recompute by hand, name the assets:

```bash
barca get order_count pipeline.py --refresh orders                # orders and everything downstream of it
barca get order_count pipeline.py --refresh orders --no-cascade   # only orders; barca warns that order_count is out of date
barca get order_count pipeline.py --refresh-all                   # every asset the target depends on
```

`--refresh` takes one comma-separated list (`--refresh a,b`). `--no-cache` is a deprecated
spelling of `--refresh-all`. Do not delete files under `.barca/` to force a recompute.

More: `barca docs cache`, which also lists what the run hash does not see (star imports,
installed packages, environment variables not declared with `env=`).

## Large inputs

A DataFrame, Arrow table or DuckDB relation is stored as parquet. The annotation on the
consuming parameter decides how it is read back:

| Annotation | What is read |
|---|---|
| none, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table` | the whole file, before the function runs (no annotation means pandas) |
| `duckdb.DuckDBPyRelation`, `pl.LazyFrame` | nothing up front; then only the columns, and where the file's statistics allow the row groups, that the step's query uses |

```python
import duckdb
from barca import asset


@asset()
def events() -> duckdb.DuckDBPyRelation:
    return duckdb.sql("select range as id, range % 10 as bucket from range(100000)")


@asset(inputs={"events": events})
def per_bucket(events: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    # only the `bucket` column is read
    return events.aggregate("bucket, count(*) as n").order("bucket")
```

A declared input is always loaded, whether or not the function uses it. Barca warns when
it plans a step that never uses one:

```
[barca] warning: pipeline.py:report never uses its input `events`. It is still loaded in full each time the step runs, and it counts toward the step's cache key. Use it, remove it from inputs=, or rename the parameter `_events` if it is there for ordering only (a `_` input is not loaded and never flagged)
```

json and pickle inputs are always read whole.

More: `barca docs big-inputs`, `barca docs types`.

## Commands

The target comes first, then files or directories. Both are optional: with no files,
barca reads every `.py` file under the project root that imports barca
(`barca docs discovery`).

```bash
barca get summary pipeline.py     # an asset and what it depends on
barca get pipeline.py             # every asset and sensor in the file; tasks are skipped
barca run publish pipeline.py     # a task and what it depends on
```

`get` is for assets and uses the cache. `run` is for tasks: the task always runs, and the
assets it depends on come from the cache as with `get`. Using the wrong one is a usage
error (exit 2) that names the right one. `barca pipeline.py` is short for
`barca get pipeline.py`.

| Command | What it does |
|---|---|
| `barca get [target] [files...]` | Get one asset, several (`a,b`), or with no target every asset and sensor. Runs only what is not cached. |
| `barca run <task> [files...]` | Run a task, or several (`a,b`), and what they depend on. The task always runs. |
| &nbsp;&nbsp;`get` and `run` flags | `--refresh a,b`, `--no-cascade`, `--refresh-all`, `--dry-run`, `--env <name>`, `--agent` (plain progress lines), `--json`, `--pretty`, `--fields` |
| `barca list [files...]` | Every definition with its kind, freshness, inputs and declared `env`. `--json`, `--pretty`, `--limit N`, `--all`, `--fields` |
| `barca status [target] [files...]` | Per node: cache state and why, last run, artifact rows and columns. `--sample N`, `--json`, `--pretty`, `--limit N`, `--all`, `--fields`, `--env` |
| `barca sql "<query>" [files...]` | Query cached results with DuckDB (experimental). `--json`, `--pretty`, `--limit N`, `--all`, `--env` |
| `barca plan [files...]` | The execution plan as JSON (experimental). No flags. |
| `barca history` | Recent runs. `--limit N` (default 10), `--all`, `--json`, `--pretty`, `--fields`, `--env` |
| `barca stats <target> [files...]` | Timing and cache statistics for one asset. `--json`, `--pretty`, `--fields`, `--env` |
| `barca serve [files...]` | HTTP API, cron scheduler and web UI. `--port N`, `--watch`, `--no-schedule`, `--timezone`, `--read-only`, `--env` |
| `barca docs [topic]` | The manual, compiled into the binary. `--all`, `--json`, `--fields` |
| `barca version` | Print the version (also `barca --version`). |

`--fields a,b` keeps only those keys on each item of the JSON output. Every
`barca <command> --help` ends with runnable examples.

## Partitions

Partitions run one asset once per key. Each key is cached on its own.

```python
from barca import asset, collect, partitions

REGIONS = ["emea", "amer", "apac"]


@asset(partitions={"region": partitions(REGIONS)})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(inputs={"all_sales": collect(sales)})    # every key's result, as one list
def total(all_sales: list[dict]) -> dict:
    return {"regions": len(all_sales), "revenue": sum(s["revenue"] for s in all_sales)}
```

`barca get total pipeline.py` runs 4 steps, then 0 on a second run. After `"latam"` is
added to `REGIONS` it runs 2: `sales` for `latam`, then `total`. On 0.18.0 that holds when
the keys are in a module-level constant, as above; adding a key to a literal list written
inside the decorator ran every key again. No single key can be targeted or refreshed, and
`barca get sales` returns one key's value. In `barca sql` a partitioned asset is one view
with a `partition` column (`region=emea`).

More: `barca docs partitions`.

## Schedules and the server

`barca serve` runs a cron scheduler, an HTTP API and a web UI. It binds to `127.0.0.1`
with no authentication.

```python
# job.py
from barca import task, Schedule


@task(freshness=Schedule("*/10 * * * *"))    # every 10 minutes; a 6-field cron starts with seconds
def refresh() -> None:
    print("refreshing")
```

```bash
barca list job.py                        # shows each schedule and its next fire time
barca serve job.py                       # port 8274; the web UI is at /ui/
barca serve job.py --timezone utc        # evaluate cron in UTC (default: local time)
```

Schedules fire only while `barca serve` is running; `barca get` and `barca run` do not
look at the clock. A scheduled task runs on every tick. A scheduled asset is brought up to
date on every tick, which means it runs only if it is not cached: data from outside still
has to come in through a sensor. A scheduled sensor's tick runs the sensor and does not
trigger the assets that read it. The other freshness values, `Always` (the default) and
`Manual`, are recorded and shown by `barca list` and have no effect at run time today.

Runs started over HTTP are asynchronous: `POST` returns a `run_id`, and you poll
`/status/<run_id>`.

```bash
curl localhost:8274/health                  # {"read_only":false,"scheduler":true,"status":"ok","version":"0.18.1"}
curl localhost:8274/schedule                # each schedule: last and next fire, last status
curl -XPOST localhost:8274/run              # every asset and sensor (tasks are skipped) -> {"run_id":"..."}
curl -XPOST localhost:8274/get/summary      # one asset and what it depends on
curl -XPOST localhost:8274/run/publish      # one task; this recomputes every asset it depends on
curl localhost:8274/status/<run_id>         # {"status":"complete","result":{...}}
curl -XDELETE localhost:8274/run/<run_id>   # cancel a run in flight (409 if it already finished)
```

More: `barca docs scheduling`, the [server API reference](https://barca.sh/reference/server-api/),
and [Deploying](https://barca.sh/deploying/) for running it behind nginx.

## Python API

```python
import barca

barca.get("pipeline.py")              # every asset; returns the last asset's value: {"count": 3, "total": 6}
barca.get("summary", "pipeline.py")   # one asset, cache-aware: {"count": 3, "total": 6}
barca.plan("pipeline.py")["total_steps"]   # 2
```

The decorators return the function unchanged, so decorated functions can be called and
unit tested as ordinary Python.

## Datadog job traces

Set `BARCA_TELEMETRY=datadog` to report job runs and steps to a Datadog Agent.
Install `barca[datadog]` in your job environment to add named Python executions and
nested library traces in the same job trace. In the `<DD_SERVICE>-python` service,
select `barca.execute` to see jobs by their canonical names. See `barca docs telemetry`
for setup, tags, and delivery limits.

## The manual

`barca docs` lists the topics and `barca docs <topic>` prints one. Start with `overview`.
The site, [barca.sh](https://barca.sh), has the same material plus a guide and patterns.

## Development

```bash
git clone https://github.com/barca-orc/barca.git
cd barca
uv venv
uv pip install maturin
maturin develop --release --extras test   # builds the binary and installs it into .venv
cargo test
```

See [Architecture](https://barca.sh/architecture/) and
[Contributing](https://barca.sh/contributing/development/). `benchmarks/` holds the same
pipelines written for barca, Dagster and Prefect; [`benchmarks/RESULTS.md`](benchmarks/RESULTS.md)
records measurements with their date and machine. Barca is pre-1.0: a breaking change to
the CLI ships in a minor release with a "Breaking" line in the release notes.

## License

[MIT](./LICENSE)
