# Barca overview

Barca is an embedded asset orchestrator. You write plain Python functions, decorate them,
and run `barca` anywhere in the project: it finds every file that imports barca
(`barca docs discovery`). A Rust binary parses the source statically (it never imports
your code to plan), builds a DAG, runs only what is stale, and caches every output.

## Mental model

- `@asset` — a cached function. Same code + same inputs → served from cache.
- `@task` — a step that always runs (deploy, notify, migrate). Never cached.
- `@sensor` — observes external state and returns `(changed: bool, value)`.
- Dependencies are declared with `inputs={"param": upstream_fn}`. The parameter receives
  the upstream function's output.
- Every step's output is fully materialized to a file under `.barca/artifacts/` (json,
  pickle, or parquet). That file is the cache checkpoint; nothing lazy crosses a step.

## Smallest working example

```python
from barca import asset


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"nums": numbers})
def total(nums: list) -> dict:
    return {"total": sum(nums)}
```

```bash
barca list pipeline.py           # discover nodes and dependencies
barca get total pipeline.py      # run only what `total` needs; JSON on stdout when piped
barca get total pipeline.py      # second time: everything is a cache hit
```

## Commands

| Command | Purpose |
|---|---|
| `barca get [target] [files...]` | Get asset value(s); cache-aware. No target: every asset and sensor, never tasks. `barca file.py` is shorthand. `a,b` gets several in one run. |
| `barca run task [files...]` | Run a task (always re-runs) and its dependency cone. `a,b` runs several in one run. |
| `barca list [files...]` | List nodes with kind, freshness and dependencies (`--json`, `--limit`/`--all`, `--fields`; `barca docs agents`). |
| `barca status [target] [files...]` | Per node: cache state and why, last run, artifact rows/columns (`--json`, `--limit`/`--all`, `--fields`). |
| `barca sql "<query>" [files...]` | Query cached results with DuckDB; each asset is a view (`barca docs sql`). Experimental. |
| `barca plan [files...]` | Emit the tiered execution plan as JSON. |
| `barca history` / `barca stats` | Past runs; timing and cache statistics (`--json`, `--fields`; history takes `--limit`/`--all`). |
| `barca serve [files...]` | HTTP API and cron scheduler. |

`files...` are optional everywhere: without them barca reads every file in the project that
imports barca; with them (files or directories) it reads only those (`barca docs discovery`).
| `barca docs [topic]` | This manual. |

In a terminal, `get`/`run`/`list`/`history`/`stats` print human-readable output; piped or run
from a program they print JSON. `--json` and `--pretty` (or `BARCA_OUTPUT=json|pretty`) override
that; see `barca docs agents`.

## Topics

- `barca docs discovery` — which files make up a project: the root, walks, `[discovery]`, node ids
- `barca docs assets` — decorators, inputs, freshness, retries
- `barca docs types` — how outputs are stored and read (json, pickle, parquet; pandas, polars, pyarrow, duckdb)
- `barca docs tasks` — tasks and `barca run`
- `barca docs cache` — what is cached, artifacts, `--refresh` / `--refresh-all`, environments
- `barca docs remote` — share one cache across machines (S3, GCS, Azure) with a few environment variables
- `barca docs partitions` — fan-out over keys, fan-in with `collect`
- `barca docs sinks` — export outputs to local or remote paths
- `barca docs scheduling` — freshness, cron schedules, `barca serve`
- `barca docs telemetry` — report runs and steps to Datadog (`BARCA_TELEMETRY=datadog`)
- `barca docs status` — one view of cache state, last run and artifact shape per node
- `barca docs sql` — query cached results with DuckDB while debugging, without writing a step
- `barca docs skill` — the short agent skill (also `SKILL.md` in the repo): start here if you are an AI agent
- `barca docs agents` — output contract, exit codes and workflows for scripts and AI agents
- `barca docs contract` — the CLI contract: every command, flag, environment variable, exit code and
  JSON schema, marked stable or experimental, and the policy for changing them
- `barca docs examples` — runnable example pipelines (`examples/duckdb`, `examples/partitions`, ...)

Run `barca docs` for one-line summaries of every topic.
