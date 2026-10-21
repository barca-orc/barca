# Barca overview

Barca is an embedded asset orchestrator. You write plain Python functions, decorate them,
and run `barca` anywhere in the project: it finds every file that imports barca
(`barca docs discovery`). A Rust binary parses the source statically, builds a DAG,
runs only what is stale, and caches every output. `partitions(<expression>)` evaluates
Python to resolve its keys and can import the pipeline module during loading.

## Mental model

- `@asset` — a cached function. Same code + same inputs → served from cache.
- `@task` — a step that always runs (deploy, notify, migrate). Never cached.
- `@sensor` — observes external state and returns `(changed: bool, value)`.
- Dependencies are declared with `inputs={"param": upstream_fn}`. The parameter receives
  the upstream function's output.
- Every step's output is fully materialized to a file under `.barca/artifacts/` (json,
  pickle, parquet or CSV). That file is the cache checkpoint; nothing lazy crosses a step.

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
| `barca get [target] [files...]` | Get assets and print their artifact paths; cache-aware, runs only what is stale. No target: every asset and sensor, never tasks. `a,b` gets several in one run. |
| `barca run task [files...]` | Run a task (always re-runs) and its dependency cone. `a,b` runs several in one run. |
| `barca list [files...]` | List nodes with kind, freshness and dependencies (`--json`, `--limit`/`--all`, `--fields`; `barca docs agents`). |
| `barca status [target] [files...]` | Per node: cache state and why, last run, artifact rows/columns (`barca docs status`). |
| `barca logs [RUN_ID]` | A run's recorded step stdout, without progress messages; the most recent run without an id (`barca docs logs`). |
| `barca history` / `barca stats` | Past runs; timing and cache statistics (`--json`, `--fields`; history takes `--limit`/`--all`). |
| `barca sql "<query>" [files...]` | Query cached results with DuckDB; each asset is a view (`barca docs sql`). Experimental. |
| `barca doctor [files...]` | Check project, Python, cache, metadata and stores without running steps (`barca docs doctor`). |
| `barca plan [files...]` | Emit the tiered execution plan as JSON. |
| `barca gc [--dry-run] [--max-size SIZE]` | Evict cached results to keep `.barca/` under `[cache] max_size` (default 10% of the filesystem; barca also does this after every `get` and `run`) (`barca docs cache`, "Disk use"). |
| `barca state backups` / `barca state restore` | List the metadata backups, or put one back in place of the local database (`barca docs remote`). |
| `barca serve [files...]` | HTTP API, cron scheduler and the web UI (`/ui/`). `--server` on get/run/status/list/history/stats runs them there (`barca docs server-mode`). |
| `barca docs [topic]` | This manual. |
| `barca version` | Print the installed version (also `barca --version`). |

`files...` are optional everywhere: without them barca reads every file in the project that
imports barca; with them (files or directories) it reads only those (`barca docs discovery`).

In a terminal, `get`/`run`/`list`/`history`/`stats` print human-readable output; piped or run
from a program they print JSON. `--json` and `--pretty` (or `BARCA_OUTPUT=json|pretty`) override
that; see `barca docs agents`.

## Topics

Getting work done:

- `barca docs discovery` — which files make up a project: the root, walks, `[discovery]`, node ids
- `barca docs assets` — decorators, inputs, freshness, retries
- `barca docs tasks` — tasks and `barca run`
- `barca docs partitions` — fan-out over keys, fan-in with `collect`
- `barca docs cache` — what is cached, artifacts, `--refresh` / `--refresh-all`, environments
- `barca docs types` — how outputs are stored and read (json, pickle, parquet, CSV; pandas, polars, pyarrow, duckdb)
- `barca docs big-inputs` — large inputs: lazy types load only the columns and rows a step needs
- `barca docs sinks` — export outputs to local or remote paths
- `barca docs status` — one view of cache state, last run and artifact shape per node
- `barca docs logs` — a run's recorded step stdout, apart from progress and results
- `barca docs sql` — query cached results with DuckDB while debugging, without writing a step
- `barca docs doctor` — prerequisite checks without running steps, and a copyable local-only debugging profile
- `barca docs groups` — organizational groups and nesting in the UI and CLI
- `barca docs scheduling` — freshness, cron schedules, `barca serve`, HTTP/Python client setup
- `barca docs server-mode` — one `barca serve` as the team's ledger: local runs read cache hits from it and post what they compute; `--server` runs `get`, `run`, `status`, `list`, `history`, `stats` there; `--local` leaves it out
- `barca docs remote` — this machine's backup and read-through cache (S3, GCS, Azure), set with a few environment variables; sharing means a server (`barca docs server-mode`)
- `barca docs telemetry` — report runs and steps to Datadog (`BARCA_TELEMETRY=datadog`), post
  to Slack when a job starts failing or recovers (`BARCA_SLACK_WEBHOOK_URL`), and JSON log lines
  from `barca serve` (`BARCA_LOG=json`)

Examples and reference:

- `barca docs examples` — runnable example pipelines (`examples/duckdb`, `examples/partitions`, ...)
- `barca docs agents` — output contract, exit codes and workflows for scripts and AI agents
- `barca docs skill` — the short agent skill (also `SKILL.md` in the repo): start here if you are an AI agent
- `barca docs contract` — the CLI contract: every command, flag, environment variable, exit code and
  JSON schema, marked stable or experimental, and the policy for changing them

Run `barca docs` for one-line summaries of every topic.

See also: `barca docs examples`, `barca docs skill`, `barca docs contract`.
