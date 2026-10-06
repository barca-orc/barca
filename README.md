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

Barca is an asset orchestrator that adds almost no overhead to your Python pipelines. You write
plain functions and decorate them; a compiled Rust binary parses the source (it never imports your
code to plan), builds the dependency graph, runs only what is stale, and caches every output.
Python does what it is best at: running your code.

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
$ barca get summary pipeline.py
[barca] 2/2 steps | done in 0.0s
Run 7b2aae64c94d | got 'summary' in 0.076s (2 steps, 1 phase)

Value:
{
  "count": 3,
  "total": 6
}
```

Run it again and both steps come from cache. No config file, no daemon.

## Install

Barca is designed for use with [uv](https://docs.astral.sh/uv/) and needs Python 3.12 or newer:

```bash
uv add barca
```

One wheel gives you the `barca` CLI (a compiled Rust binary), the Python API (`barca.get()`,
`barca.run()`, `barca.plan()`, ...) and the decorator stubs (`@asset`, `@sensor`, `@task`) for IDE
autocomplete and type checking. Extras: `barca[parquet]` (pandas and pyarrow, for DataFrame
outputs), `barca[fast]` (orjson), and `barca[s3]`, `barca[azure]`, `barca[gcs]` or `barca[remote]`
for [shared remote storage](https://barca.sh/reference/remote-storage/).

From source (needs a Rust toolchain and [maturin](https://www.maturin.rs/)):

```bash
git clone https://github.com/barca-orc/barca.git
cd barca
uv sync
maturin develop --release    # builds the binary and installs it into .venv
```

## A first pipeline

Assets are cached functions; `inputs=` wires a parameter to an upstream function. A `@task` always
re-runs (deploys, notifications) and is started with `barca run`. A `@sensor` observes external
state and returns `(changed, value)`.

```python
# pipeline.py
from barca import asset, task

@asset()
def numbers() -> list:
    return [1, 2, 3]

@asset(inputs={"nums": numbers})
def total(nums: list) -> dict:
    return {"total": sum(nums)}

@task(inputs={"t": total})
def publish(t: dict) -> None:
    print(f"publishing {t}")
```

```bash
barca list                          # every node barca found, with its dependencies
barca get total                     # run only what `total` needs; a second run is all cache hits
barca get total --dry-run           # what would run and why, without running or writing anything
barca get total --refresh numbers   # re-run `numbers` and everything downstream of it
barca run publish                   # run a task (always re-runs) and its dependency cone
barca status                        # cache state, last run and artifact shape per node
barca sql "select * from numbers"   # query cached results with DuckDB (experimental)
```

Files are optional everywhere: without them barca reads every `.py` file under the project root
(the nearest directory with a `barca.toml`, else the current one) that imports barca; name files
or directories to narrow it (`barca get total pipeline.py`). `barca pipeline.py` is shorthand for
`barca get pipeline.py`. State lives under `.barca/` (metadata database and artifacts).

## CLI

| Command | What it does |
|---|---|
| `barca get [target[,target...]] [file.py\|dir/ ...]` | Get asset values, cache-aware. No target: every asset and sensor, never tasks. `--refresh a,b [--no-cascade]`, `--refresh-all`, `--dry-run`, `--agent`, `--fields`, `--env`. |
| `barca run <task[,task...]> [file.py\|dir/ ...]` | Run tasks (always re-run) and their dependency cone. Same refresh flags as `get`. |
| `barca list [file.py\|dir/ ...]` | List nodes with kind, freshness and dependencies. `--json`, `--pretty`, `--limit N`, `--all`, `--fields`. |
| `barca status [target[,target...]] [file.py\|dir/ ...]` | Per node: cache state and why, last run, artifact rows and columns. `--json`, `--sample N`, `--limit N`, `--all`, `--env`. |
| `barca sql "<query>" [file.py\|dir/ ...]` | Query cached results with DuckDB, one view per node (experimental). `--json`, `--limit N`, `--all`, `--env`. |
| `barca plan [file.py ...]` | Print the execution plan as JSON (experimental). |
| `barca history` | Recent runs. `--limit N`, `--all`, `--json`, `--pretty`, `--env`. |
| `barca stats <target> [file.py ...]` | Timing and cache statistics for one asset. `--json`, `--pretty`, `--env`. |
| `barca serve [file.py\|dir/ ...]` | HTTP API, the cron scheduler and the web UI at `/ui/` (experimental). `--port N`, `--watch`, `--no-schedule`, `--read-only`, `--timezone TZ`, `--env`. |
| `barca docs [topic]` | The manual, compiled into the binary. `--all`, `--json`. |
| `barca version` | Print the version. |

Every command ends its `--help` with runnable examples.

**The manual.** `barca docs` is the manual: concepts, output formats, caching, tasks, partitions,
scheduling, remote storage and runnable examples. It ships inside the binary, so it works offline
and always matches the installed version; the same material is on the [docs site](https://barca.sh/).
AI agents: load [`SKILL.md`](SKILL.md) (also `barca docs skill`), a short
[Agent Skill](https://barca.sh/reference/agent-skill/) with the commands, argument order, exit
codes and guardrails; `barca docs agents` has the full contract.

**Output.** Results go to stdout: human-readable in a terminal, one line of JSON when piped or
captured (`--json` / `--pretty` or `BARCA_OUTPUT=json|pretty` override). Progress and errors go to
stderr; in JSON mode an error is one JSON line on stderr (`{"error", "code", "kind",
"remediation"}`, plus `node`, `traceback` and `artifact_dir` when a step failed). Exit codes: `0`
ok, `1` a step failed, `2` usage error, `3` barca/infra failure, `130` cancelled. `list` shows 100
nodes and `history` 10 runs unless you pass `--limit N` or `--all`.

**The contract.** Every command, flag, environment variable, exit code, JSON schema and `--agent`
line is written down, each marked stable or experimental, in
[`crates/barca-cli/docs/contract.md`](crates/barca-cli/docs/contract.md) (also `barca docs
contract`, and [online](https://barca.sh/reference/cli-contract/)). Tests fail on any change to the
CLI that the contract does not reflect. Before 1.0 a breaking change ships in a minor release with
a "Breaking" line in the release notes.

## Concepts

| Kind | Decorator | Cached | Can be an input to |
|---|---|---|---|
| asset | `@asset()` | yes | assets, sensors, tasks |
| sensor | `@sensor()` | no: always runs, and its value is part of the run hash of the assets that read it | assets, sensors, tasks |
| task | `@task()` | no: always runs | tasks only |

Other pieces: `@sink` (also write an output to another path), `partitions`, `partitions_from` and
`collect` (fan out over keys, fan in), `Always` / `Manual` / `Schedule("<cron>")` freshness, and
`env=[...]` to declare environment variables that are part of the cache key. Every output is
fully materialized to an artifact file (json, pickle or parquet) under `.barca/artifacts/`; that
file is the cache checkpoint. Decorators are identity functions, so your code also runs without
barca. Details: the [decorators reference](https://barca.sh/reference/api/decorators/), the
[scheduling guide](https://barca.sh/scheduling/), `barca docs telemetry` (report runs and steps to
Datadog with `BARCA_TELEMETRY=datadog`) and the
[server API](https://barca.sh/reference/server-api/).

## Python API

```python
import barca

value = barca.get("summary", "pipeline.py")   # cache-aware; returns the loaded value
barca.run("publish", "pipeline.py")           # run a task
plan = barca.plan("pipeline.py")              # execution plan as a dict
```

See the [Python API reference](https://barca.sh/reference/api/python/).

## Benchmarks

Measured with [hyperfine](https://github.com/sharkdp/hyperfine) against Dagster and Prefect running
equivalent pipelines. A trivial pipeline (one asset, zero work) measures pure framework overhead:

| Framework | Mean | Relative |
|---|---|---|
| **barca** | **37.7 ms** | **1.00x** |
| dagster | 521.6 ms | 13.8x |
| prefect | 4.108 s | 109x |

That run was in a shared container: read the ratios, not the milliseconds. The full suite (DAG
shapes, ETL, partitions, retries) and its caveats are in
[`benchmarks/README.md`](benchmarks/README.md) and [`benchmarks/RESULTS.md`](benchmarks/RESULTS.md);
rerun one with `benchmarks/trivial/bench.sh 10`.

## Development

```bash
cargo build --release
maturin develop --release
cargo test                        # Rust tests, including the CLI contract and help examples
python -m pytest python/tests     # Python tests (needs the `test` extra)
```

The repository layout, design principles and the rules for changing the CLI are in
[`CLAUDE.md`](CLAUDE.md) and the [contributing pages](https://barca.sh/contributing/development/).

## License

[MIT](./LICENSE)
