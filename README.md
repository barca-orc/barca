<p align="center">
  <h1 align="center">barca</h1>
  <p align="center"><strong>The invisible asset orchestrator.</strong><br/>Rust plans it. Python runs it. You just write functions.</p>
</p>

<p align="center">
  <a href="https://pypi.org/project/barca/"><img alt="PyPI" src="https://img.shields.io/pypi/v/barca?style=flat-square&color=3572A5" /></a>
  <img alt="Python" src="https://img.shields.io/badge/python-%E2%89%A53.12-3572A5?style=flat-square" />
  <img alt="Rust" src="https://img.shields.io/badge/rust-2024_edition-dea584?style=flat-square" />
  <a href="https://github.com/barca-orc/barca/blob/main/LICENSE"><img alt="License" src="https://img.shields.io/github/license/barca-orc/barca?style=flat-square" /></a>
</p>

---

Barca is an asset orchestrator that adds **zero perceptible overhead** to your Python pipelines. A compiled Rust binary handles parsing, DAG construction, and execution planning. Python does what it's best at: running your code.

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
{"elapsed_seconds":0.27,"final_output":{"count":3,"total":6},"phases":1,"run_id":"...","steps_executed":2}
```

No config files. No YAML. No daemon. Just functions and a fast binary.

## Install

Barca is designed for use with [uv](https://docs.astral.sh/uv/):

```bash
uv add barca
```

This gives you:
- The `barca` CLI binary (compiled Rust)
- Python API: `barca.get()`, `barca.plan()`
- Decorator stubs for `@asset`, `@sensor`, `@task` (IDE autocomplete + type checking)

For optional parquet (DataFrame) support:

```bash
uv add 'barca[parquet]'
```

All in one wheel, built with [maturin](https://www.maturin.rs/). Requires Python >= 3.12.

### From source

```bash
git clone https://github.com/barca-orc/barca.git
cd barca
uv sync
cargo build --release
maturin develop --release    # installs into current .venv
```

## Quick start

```python
# assets.py
from barca import asset

@asset()
def hello() -> dict:
    return {"message": "Hello from barca!"}
```

```bash
barca get assets.py
```

That's it. Barca parses your Python source with [ruff](https://github.com/astral-sh/ruff)'s AST parser (no import, pure static analysis), builds a dependency graph, generates a phased execution plan, spawns Python workers, and persists results to a local SQLite database -- all in under 40ms for a trivial asset.

## How it works

```
                    ┌─────────────────────────────────────┐
                    │          barca get pipeline.py       │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │         Rust binary (barca)          │
                    │                                      │
                    │  1. Parse Python source (ruff AST)   │
                    │  2. Build DAG (petgraph)              │
                    │  3. Generate execution plan           │
                    │  4. Initialize DB (.barca/metadata.db)│
                    │  5. Spawn Python workers per phase    │
                    │  6. Collect outputs, persist to DB    │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │      Python worker (per phase)       │
                    │                                      │
                    │  - Loads modules via importlib        │
                    │  - Executes steps in tier order       │
                    │  - LRU cache for in-process results   │
                    │  - Emits JSON lines to stdout         │
                    └─────────────────────────────────────┘
```

**Key design decisions:**

- **Static analysis only** -- Rust never imports your Python code. It parses source text and extracts decorator metadata from the AST.
- **Phased execution** -- The planner decomposes the DAG into sequential phases. Within each phase, independent streams run in parallel workers.
- **No framework lock-in** -- Decorators are identity functions. Your code runs standalone without barca installed.
- **Single binary** -- One `pip install` gives you everything. No JVM, no Docker, no scheduler service.

## Decorators

```python
from barca import asset, sensor, task, sink, unsafe
from barca import Always, Manual, Schedule
from barca import partitions, partitions_from, collect, asset_ref
```

### `@asset`

Cached computation node. The workhorse.

```python
@asset()
def prices() -> dict:
    return {"AAPL": 150, "MSFT": 380}

@asset(inputs={"data": prices})
def report(data: dict) -> str:
    return f"Tracked {len(data)} tickers"
```

### `@sensor`

Observes external state. Returns `(update_detected, output)`.

```python
@sensor()
def inbox_files() -> tuple[bool, list[str]]:
    files = list(Path("inbox").glob("*.csv"))
    return bool(files), [str(f) for f in files]
```

A sensor always runs, and its returned value is part of the run hash of every asset that reads
it: a new value re-runs those assets and everything downstream, the same value serves them from
cache. Use one in front of data that changes in place (return the blob's etag); see
`barca docs cache`.

### `@task`

Workflow-management step — deploys, notifications, migrations, cache warming.
Always re-runs (never cached). May appear anywhere in the graph and may depend
on assets, sensors, or other tasks, but must **not** be an input to an asset or
sensor (that would poison caching). Run a task with `barca run <task>`.

```python
# A task consuming an upstream asset (asset → task).
@task(inputs={"report": report})
def publish(report: str) -> None:
    print(f"Publishing: {report}")

```

### Declared environment variables

Assets that read environment variables declare them, so a changed value invalidates the cache:

```python
@asset(env=["SOURCE_CSV", "API_TOKEN"])
def raw() -> dict:
    return load(os.environ["SOURCE_CSV"])
```

The values are folded into the run hash at plan time (unset is its own value) and reported per
step in `--agent` lines and the JSON result. Names ending in `_TOKEN`, `_SECRET`, `_KEY` or
`_PASSWORD` are hashed but redacted. Undeclared variables are not tracked.

### `@sink`

Declares a sink output target (stacks on `@asset`; file writing coming soon).

```python
@asset()
@sink("output/data.json", serializer="json")
def my_data() -> dict:
    return {"rows": 42}
```

### Freshness markers

| Marker | Behavior |
|--------|----------|
| `Always` | Auto-materializes whenever stale (default for `@asset` and `@task`) |
| `Manual` | Only runs on explicit refresh |
| `Schedule("0 5 * * *")` | Cron expression (5-field, or 6-field for sub-minute) |

### Partitions

Fan a single asset definition into N independent materializations:

```python
@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def prices(ticker: str) -> dict:
    return {"ticker": ticker, "price": get_price(ticker)}
```

| Function | Purpose |
|----------|---------|
| `partitions(values)` | Static list of partition keys |
| `partitions_from(source)` | Same keys as `source`; each key also receives that key's output of a partitioned `source` |
| `collect(asset_fn)` | Aggregate all partitions of an upstream |
| `asset_ref(ref_string)` | Canonical asset reference |

## CLI

```
barca get [target] [file.py|dir/ ...]      Get asset(s) — cache-aware; `a,b` gets several
barca run <task> [file.py|dir/ ...]        Run a task (always re-runs) and its cone; `a,b` runs
                                           several
  get/run: --refresh a,b                   re-run a, b and their downstream (--no-cascade: only a, b)
  get/run: --refresh-all                   re-run every asset in the cone
barca plan [file.py ...]                   Emit execution plan as JSON (experimental)
barca list [file.py|dir/ ...] [--json|--pretty] [--limit N]  List all definitions with deps and env
barca status [target] [file.py ...] [--json|--pretty]  Cache state, last run, artifact rows/columns per
                                           node; `a,b` shows several cones
barca sql "<query>" [file.py ...] [--json] Query cached results with DuckDB (experimental)
barca history [--limit N] [--json|--pretty] Show recent run history
barca stats <target> [file.py ...] [--json|--pretty]  Timing/cache stats for an asset
barca serve [file.py ...] [--port N]       Run the HTTP API server + cron scheduler
barca docs [topic] [--all] [--json]        Built-in manual: concepts, formats, examples
barca --help                               Show help (every command ends with examples)
```

Files are optional everywhere: without them barca reads every `.py` file under the project root
(the nearest `barca.toml`) that imports barca. See `barca docs discovery`.

`barca docs` is the manual, compiled into the binary: topics for types and output formats,
caching, tasks, partitions, scheduling, runnable examples, and conventions for scripts and AI
agents (`barca docs agents`). AI agents: load [`SKILL.md`](SKILL.md) (also `barca docs skill`), a
~1500-token [Agent Skill](https://barca.sh/reference/agent-skill/) with the commands, argument
order, exit codes and guardrails. Results go to stdout: human-readable in a terminal, JSON when piped
or captured (`--json` / `--pretty` or `BARCA_OUTPUT=json|pretty` override); progress and errors go
to stderr. In JSON mode an error is one JSON line on stderr (`{"error", "code", "kind",
"remediation"}`, plus `node`, `traceback` and `artifact_dir` when a step failed). Exit codes: `0`
ok, `1` step failed, `2` usage error, `3` barca/infra failure, `130` cancelled.

List output is bounded: `list` shows 100 nodes and `history` 10 runs unless you pass `--limit N`
or `--all`, and their JSON reports `truncated` and `total`. `--fields a,b` keeps only those keys
on each item of any JSON output (`barca list pipeline.py --fields id,inputs`).

Shorthand: `barca pipeline.py` works as `barca get pipeline.py` (all assets and sensors; tasks are skipped, use `barca run`).

The CLI surface is written down as a contract: every command, flag, environment variable, exit
code, JSON output schema and `--agent` line, each marked stable or experimental, in
[`crates/barca-cli/docs/contract.md`](crates/barca-cli/docs/contract.md) (also `barca docs
contract`, and [online](https://barca.sh/reference/cli-contract/)). Snapshot tests fail CI on any
change to it that the contract does not reflect. Before 1.0 a breaking change ships in a minor
release with a "Breaking" line in the release notes; from 1.0 changes are additive only.

## Scheduling

Barca doubles as a plain **task scheduler**. Decorate a function with a cron
`Schedule` and leave `barca serve` running — each job fires on its own tick. No
external cron, no YAML, no daemon service to install.

```python
# job.py
from barca import task, Schedule

@task(freshness=Schedule("*/10 * * * *"))
def refresh() -> None:
    ...  # hit an API, rebuild a file, send a report — a task always re-runs

@task(freshness=Schedule("*/15 * * * * *"))   # 6-field cron → every 15 seconds
def heartbeat() -> None:
    ...
```

```bash
barca serve job.py
```

Standard 5-field cron (`minute hour day-of-month month day-of-week`) and a 6-field
form with a leading **seconds** field are both supported — the scheduler evaluates
at 1-second resolution. It's timezone-aware, fires once on restart to catch up a
missed tick, skips a tick if the previous run is still going, and exposes live
status at `GET /schedule`. Inspect the schedule any time with `barca list job.py`.

See the [Scheduling guide](https://barca.sh/scheduling/) for the full story
(timezones, catch-up, keeping it alive under systemd). A minimal example lives in
[`examples/scheduler`](examples/scheduler).

## Server

`barca serve` starts a long-running HTTP server that exposes the orchestrator as a
JSON API — for triggering runs programmatically and polling status — and serves the
web UI at `http://127.0.0.1:8274/ui/`. It also runs the built-in cron scheduler above.
It binds to `127.0.0.1` by default (local only, no auth); to run it behind nginx, see
[Deploying](https://barca.sh/deploying/).

```bash
barca serve pipeline.py --port 8274      # default port 8274
barca serve pipeline.py --watch          # dev mode: re-parse DAG on file change
barca serve pipeline.py --no-schedule    # HTTP API only, don't fire scheduled jobs
barca serve pipeline.py --timezone utc   # evaluate cron in UTC (default: local)
barca serve pipeline.py --read-only      # inspect only: no runs, no scheduler, DB never written
```

Runs are async: `POST` returns a `run_id` immediately, then you poll `/status/{run_id}`.

```bash
curl localhost:8274/health                       # {"status":"ok","version":"0.16.0"}
curl localhost:8274/assets                       # list assets + deps
curl localhost:8274/plan                          # execution plan JSON
curl -XPOST localhost:8274/run                    # → {"run_id":"…"}; poll /status/<id>
curl -XPOST localhost:8274/get/summary            # run a single target
curl localhost:8274/status/<run_id>               # poll run status + result
curl -XDELETE localhost:8274/run/<run_id>         # cancel an in-flight run
```

See the [Server API reference](https://barca.sh/reference/server-api/) for the full endpoint reference.

## Python API

```python
import barca

# Get all assets in a file (returns the last asset's value; tasks are not run)
value = barca.get("pipeline.py")
print(value)  # {"count": 3, "total": 6}

# Get a specific asset's value (cache-aware)
value = barca.get("summary", "pipeline.py")
print(value)  # {"count": 3, "total": 6}

# Inspect the execution plan
plan = barca.plan("pipeline.py")
print(plan["total_steps"])  # 2
```

All output formats work transparently: dicts, lists, sets, DataFrames, and arbitrary Python objects are serialized as JSON, pickle, or parquet and deserialized automatically. Every asset is **fully materialized** to an artifact file at step boundaries — that persistence is the cache checkpoint. Type annotations on parameters (e.g. `pl.DataFrame`) select the parquet reader; they do not skip materialization.

### `barca plan` -- inspect without running

```bash
$ barca plan pipeline.py
{
  "total_steps": 2,
  "phases": [
    {
      "reason": "Initial",
      "streams": [
        {
          "stream_id": "p0-w0",
          "steps": ["pipeline.py:raw_data", "pipeline.py:summary"]
        }
      ]
    }
  ]
}
```

### `barca get` -- execute and get results

Parses source, builds DAG, spawns workers, collects outputs, persists to `.barca/metadata.db`. With a target, only the target's subgraph runs. Without a target, every asset and sensor runs and tasks are skipped (previously tasks ran too); stderr names the skipped tasks. Run tasks with `barca run`.

Output is a JSON summary:

```json
{
  "status": "success",
  "run_id": "...",
  "elapsed_seconds": 0.27,
  "steps_executed": 2,
  "phases": 1,
  "final_output": {"count": 3, "total": 6}
}
```

Use `--refresh-all` to skip cache lookups and execute everything fresh (`--refresh a,b` for
chosen assets and what is downstream of them).

Diagnostics go to stderr:

```
[barca] 2/2 steps | done in 0.0s
```

## Benchmarks

All benchmarks measured with [hyperfine](https://github.com/sharkdp/hyperfine) (3 warmup runs, 10 measured runs) on the same machine. Barca is compared against Dagster and Prefect running equivalent pipelines.

### Trivial (1 asset, zero work)

Measures pure framework overhead -- how long it takes to do nothing.

| Framework | Mean | Relative |
|-----------|------|----------|
| **barca** | **38.0 ms** | **1.00x** |
| dagster | 538.1 ms | 14.2x |
| prefect | 3977.7 ms | 104.7x |

Barca's total overhead (parse + plan + spawn + persist) is **38ms**. Dagster needs ~0.5s. Prefect needs ~4s.

### Benchmark suite

The `benchmarks/` directory covers a wide range of DAG topologies and workloads —
overhead/scaling, DAG shapes, real workloads (ETL, ML pipelines), partitioned
runs, and dynamic dispatch/resilience — each with equivalent Dagster and Prefect
implementations for apples-to-apples comparison. See
[`benchmarks/README.md`](benchmarks/README.md) for the full topology tables and
[`benchmarks/RESULTS.md`](benchmarks/RESULTS.md) for current results.

Run any benchmark:

```bash
cd benchmarks/trivial
./bench.sh 10    # 10 measured runs
```

## Architecture

```
Cargo.toml                  Rust workspace root
crates/
  barca-core/               Engine: parser, DAG, planner, dispatch, DB, cache
  barca-cli/                Thin CLI shell (clap → barca-core)
python/barca/
  __init__.py               Decorator stubs + API exports
  api.py                    Python API (get/plan via subprocess)
  _worker.py                Execution worker (invoked by Rust binary)
  _artifacts.py             Artifact serialization (json/pickle/parquet)
  py.typed                  PEP 561 marker
pyproject.toml              Maturin build config
```

### Tech stack

| Layer | Technology |
|-------|-----------|
| Parser | [ruff](https://github.com/astral-sh/ruff) Python AST (static, no import) |
| DAG | [petgraph](https://github.com/petgraph/petgraph) |
| Database | [Turso/libSQL](https://turso.tech/) (local SQLite) |
| Serialization | [serde](https://serde.rs/) + serde_json |
| Hashing | SHA-256 (content-addressed artifacts) |
| Build | [maturin](https://www.maturin.rs/) (Rust binary + Python stubs in one wheel) |
| Python runtime | Python >= 3.12 |

### Node kinds

| Kind | Decorator | Cached | Can be input to |
|------|-----------|--------|-------------|
| **asset** | `@asset()` | Yes | assets, sensors, tasks |
| **sensor** | `@sensor()` | No | assets, sensors, tasks |
| **task** | `@task()` | No | tasks only (not assets/sensors) |

## Development

```bash
git clone https://github.com/barca-orc/barca.git
cd barca

# Build
cargo build --release
maturin develop --release

# Test
cargo test

# Run
barca get examples/basic_app/example_project/assets.py
barca plan examples/basic_app/example_project/assets.py
```

## Project status

Barca is in active development. The core pipeline (parse -> DAG -> plan -> execute -> persist) is working and benchmarked. See the [guide](https://barca.sh/guide/) for a walkthrough.

## License

[MIT](./LICENSE)
