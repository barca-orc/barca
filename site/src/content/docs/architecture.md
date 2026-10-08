---
title: Architecture
description: The crates, modules, worker protocol and storage that make up barca.
---

Barca is a Rust binary and a Python package shipped in one wheel. The binary parses Python
source without importing it, builds a dependency graph, decides what needs to run, and
dispatches steps to Python worker processes. The Python package holds the decorators and the
worker.

This page describes the current source layout. The reasons behind the main choices are in
[Architecture decisions](/architecture-decisions/).

## Layout

```
crates/
  barca-core/src/             library: no HTTP dependencies
    parse.rs, discover.rs     read decorated functions from source with ruff's parser; find the project's .py files
    load.rs                   from source files to a DAG, with dynamic partitions resolved
    dag.rs, planner.rs        petgraph graph, validation, phases and streams of the plan
    hash.rs, cone.rs, envdeps.rs   run hash: the function, the helper code it reaches, declared env= variables
    cache.rs, recover.rs      cache lookups; recompute a cached step whose artifact file is gone
    coordinator.rs            ready queue, dependencies, retries, parallel groups (no I/O)
    io_loop.rs, protocol.rs   worker pool, leased batches, length-prefixed JSON over a Unix socket
    cost.rs                   measured step cost and batch sizing
    db.rs, config.rs          the metadata database; project root, barca.toml, environment, flags
    transfer.rs, state_*.rs   remote store: artifact transfer, and shared history as one blob
    store_sync.rs             a run's link to the store: upload results, fetch remote inputs, wait at the end
    persist.rs                recording a run: run and step rows, the final write, the shared-history push
    commands.rs               get and run entry points
    targets.rs                target resolution, refresh-name checks and target planning
    execution.rs              prepare, decide, dispatch and finalize a run
    queries.rs, status.rs, sql.rs   read-only commands: plan, history, stats, list; status; sql
    results.rs                what a command returns (serde types shared by the CLI and the server)
    report.rs, report/        shared result formatting, field projection and progress lines
    envelope.rs               error classification, remediation and error envelopes
    schedule.rs               schedule discovery and next-fire descriptions
    helper_proc.rs            Python command construction, serialized spawning and helper lifetime
    telemetry/                run reports; one Datadog trace per run
  barca-server/src/           axum HTTP API, cron scheduler, file watcher, embedded web UI
  barca-cli/                  the `barca` binary; only `serve` calls barca-server
    src/args.rs               clap argument definitions
    src/commands/             one handler module per subcommand
    src/main.rs               argument parsing, project setup and dispatch
    src/input.rs, output.rs   input validation, project roots and output selection
    src/error.rs              clap and stderr adapter to the shared core error envelope
    src/tests.rs, contract.rs CLI behavior, help and contract checks
    docs/                     the manual, compiled into the binary
python/barca/
  __init__.py                 decorators (they return the function unchanged), parallel()
  _worker.py, _runtime.py     the worker (`python -m barca._worker`) and its socket client
  _artifacts.py, _duckdb.py   json, pickle and parquet reading and writing; DuckDB connections
  _storage.py, _transfer.py, _state.py   remote store backends, artifact transfer, shared-history blob
  _inspect.py, _sql.py        artifact shape for `barca status`; the query for `barca sql`
  api.py, client.py           Python API that shells out to the binary; HTTP client for `barca serve`
ui/                           web UI (TypeScript, Vite); its build output is compiled into barca-server
pyproject.toml                maturin build: binary and Python package in one wheel
```

## A run

`barca get total pipeline.py` does this:

1. **Parse.** Each file is parsed with ruff's Python parser. Nothing is imported. The one
   exception is `partitions(<expression>)` with a non-literal expression, which a Python
   subprocess evaluates.
2. **Graph.** `dag.rs` builds a petgraph graph from the decorators and rejects cycles, unknown
   inputs, a sensor with inputs, a task used as an input to an asset or sensor, and two nodes
   with the same id.
3. **Plan.** `planner.rs` splits the part of the graph the target needs into phases. A phase is
   a set of steps whose inputs are available when it starts.
4. **Cache check.** Each asset's run hash is looked up in the metadata database. A hit is
   served from its artifact file. Sensors and tasks always run.
5. **Execute.** Steps that need to run go on a ready queue. A pool of Python workers, one per
   CPU by default (`BARCA_POOL_SIZE` overrides), takes them over a Unix domain socket.
6. **Record.** Each finished step is written to the metadata database as it finishes, and the
   run row is closed at the end. With a remote store, artifacts are uploaded in the background
   by the transfer helper, a step is recorded once its upload is confirmed (when the run
   ends), and the shared history is pushed after that.

Planning happens on every command. There is no stored plan to go stale between commands.

## Workers

- A worker is started with `python -m barca._worker` and connects to the coordinator's Unix
  socket. Python is the `python` or `python3` beside the `barca` executable, otherwise
  `python3` on `PATH`.
- Workers are started as needed, up to the pool size, and stay alive for the whole run.
- Messages are JSON with a 4-byte big-endian length prefix. The coordinator sends one step or
  a batch of steps; the worker answers each with `step_completed` or `step_error`, and also
  sends log lines, heartbeats and `parallel()` requests.
- A worker imports the pipeline file from source and calls the function. It reads inputs from
  artifact files and writes the result to an artifact file. It has no database access.
- Batch sizes come from measured step times, and a step that calls `parallel()` is stopped
  with SIGSTOP while a temporary worker takes its place. Both are explained in
  [Architecture decisions](/architecture-decisions/).
- A failed step is retried up to `retries` attempts in total, after `retry_backoff × attempt`
  seconds, in a new worker process. If a worker dies, the steps it had not started go back on
  the queue.

## The server

`barca serve` runs the same commands as the CLI, in the same process. A run started over HTTP
or by the scheduler runs as a background task; its status and live events are held in memory,
and finished runs are also in the `runs` table. The scheduler fires nodes declared with
`freshness=Schedule(...)`. The server binds `127.0.0.1` and has no authentication. See
[Scheduling](/scheduling/), [Deploying](/deploying/) and the
[Server API](/reference/server-api/).

## Node kinds

| Kind | Decorator | Default freshness | Cached | Can be an input to |
|------|-----------|-------------------|--------|--------------------|
| asset | `@asset()` | `Always` | yes, by run hash | assets and tasks |
| sensor | `@sensor()` | `Manual` | no, runs every time | assets and tasks |
| task | `@task()` | `Always` | no, runs every time | other tasks only |

Only `Schedule` freshness has an effect at run time today; see
[Core constraints](/core-constraints/#freshness-declarations).

## Storage

Everything is under `.barca/` in the project root (under `.barca/envs/<env>/` for an
environment other than `default`).

`.barca/metadata.db` is a Turso (SQLite-compatible) database written only by the Rust binary.
Its tables are `materializations` (one row per step result: node id, run hash, status,
artifact path, timings, attempts, error), `runs` (one row per run), `logs` (captured stdout of
steps), `cost_estimates` (the time estimate per node, used to size batches) and
`schedule_state` (when each scheduled node last fired).

Artifacts are files at `.barca/artifacts/{node}/{run_hash}{ext}`, in json, pickle or parquet.
They are cached by run hash: the hash covers the function's code, the helper code it reaches
and its inputs, not the bytes of the result. A step's result is always written to a file, and
the next step reads that file.

With a remote store configured, artifacts are still written locally first and then uploaded,
and the metadata database is shared as one object in the store (shared history). See
[Remote storage](/reference/remote-storage/).

## Dependencies

- **Rust**: `ruff_python_parser`, `petgraph`, `turso`, `serde`, `sha2`, `tokio`, `croner`,
  `clap`, `toml`; the server adds `axum`, `notify`, `chrono-tz` and `rust-embed`.
- **Python**: 3.12 or later, standard library only. Extras: `parquet`, `fast` (orjson), and
  `s3`, `r2`, `gcs`, `azure`, `remote` (fsspec and the store's client).
- **Build**: maturin packages the binary and the Python package into one wheel. 0.18.0
  publishes wheels for macOS on Apple Silicon and x86-64 Linux with glibc.
