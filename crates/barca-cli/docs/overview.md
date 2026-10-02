# Barca overview

Barca is an embedded asset orchestrator. You write plain Python functions, decorate them,
and run `barca` on the file. A Rust binary parses the source statically (it never imports
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
barca get total pipeline.py      # run only what `total` needs; prints JSON on stdout
barca get total pipeline.py      # second time: everything is a cache hit
```

## Commands

| Command | Purpose |
|---|---|
| `barca get [target] files...` | Get asset value(s); cache-aware. `barca file.py` is shorthand. |
| `barca run task files...` | Run a task (always re-runs) and its dependency cone. |
| `barca list files...` | List nodes with kind, freshness and dependencies (`--json`). |
| `barca plan files...` | Emit the tiered execution plan as JSON. |
| `barca history` / `barca stats` | Past runs; timing and cache statistics (`--json`). |
| `barca serve files...` | HTTP API and cron scheduler. |
| `barca docs [topic]` | This manual. |

## Topics

- `barca docs assets` — decorators, inputs, freshness, retries
- `barca docs types` — how outputs are stored and read (json, pickle, parquet; pandas, polars, pyarrow, duckdb)
- `barca docs tasks` — tasks and `barca run`
- `barca docs cache` — what is cached, artifacts, `--no-cache` / `--refresh`, environments
- `barca docs remote` — share artifacts and state across machines (S3, Azure, GCS)
- `barca docs partitions` — fan-out over keys, fan-in with `collect`
- `barca docs sinks` — export outputs to local or remote paths
- `barca docs scheduling` — freshness, cron schedules, `barca serve`
- `barca docs agents` — output contract, exit codes and workflows for scripts and AI agents
- `barca docs examples` — runnable example pipelines (`examples/duckdb`, `examples/partitions`, ...)

Run `barca docs` for one-line summaries of every topic.
