---
title: Architecture
description: The crates, modules, worker protocol and storage that make up barca.
---

Barca is a Rust binary and a Python package shipped in one wheel. The binary reads Python
source statically (a nonliteral `partitions(...)` or `batches(...)` expression and a
`batches_by` function are evaluated in a resolver subprocess that imports the module), builds a
dependency graph, decides what needs to run, and
dispatches steps to Python worker processes. The Python package holds the decorators and the
worker.

This page describes the current source layout. The reasons behind the main choices are in
[Architecture decisions](/architecture-decisions/).

## Layout

```text wrap=false
crates/
  barca-core/src/             library: no HTTP dependencies
    parse.rs, discover.rs     read decorated functions from source with ruff's parser; find the project's .py files
    load.rs                   from source files to a DAG, with dynamic partitions resolved
    dag.rs, planner.rs        petgraph graph, validation, phases and streams of the plan
    hash.rs, cone.rs, envdeps.rs   run hash: the function, the helper code it reaches, declared env= variables
    definition.rs             which decorator arguments are part of the hash, and their canonical form
    cache.rs, recover.rs      cache lookups; recompute a cached step whose artifact file is gone
    coordinator.rs            ready queue, dependencies, retries, parallel groups (no I/O)
    io_loop.rs, protocol.rs   worker pool, leased batches, length-prefixed JSON over a Unix socket
    cost.rs                   measured step cost and batch sizing
    db/, config.rs            the metadata database; project root, barca.toml, environment, flags
    run_owner.rs              conservative process-liveness evidence for interrupted runs
    transfer.rs, backup.rs    remote store: artifact transfer, and backups of the metadata DB
    store_sync.rs             a run's link to the store: upload results, fetch remote inputs, wait at the end
    persist.rs                recording a run: run and step rows, the final write, the minute backup
    commands.rs               get and run entry points
    targets.rs                target resolution, refresh-name checks and target planning
    execution.rs              prepare, decide, dispatch and finalize a run
    queries.rs, status.rs, sql.rs   read-only commands: plan, history, stats, list; status; sql
    results.rs                what a command returns (serde types shared by the CLI and the server)
    report.rs, report/        shared result formatting, field projection and progress lines
    envelope.rs               error classification, remediation and error envelopes
    schedule.rs               schedule discovery and next-fire descriptions
    decorator_args.rs         accepted decorator/helper arguments and static validation
    term.rs                   ordered output and child streams that tolerate closed pipes
    helper_proc.rs            Python command construction, serialized spawning and helper lifetime
    telemetry/                run reports; Datadog traces, one per step and one per run
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
  _artifacts.py, _duckdb.py   json, pickle, parquet and CSV reading and writing; DuckDB connections
  _storage.py, _transfer.py, _state.py   remote store backends, artifact transfer, metadata backups
  _inspect.py, _sql.py        artifact shape for `barca status`; the query for `barca sql`
  api.py, client.py           Python API that shells out to the binary; HTTP client for `barca serve`
ui/                           web UI (TypeScript, Vite); its build output is compiled into barca-server
pyproject.toml                maturin build: binary and Python package in one wheel
```

## A run

`barca get total pipeline.py` does this:

<figure class="diagram">
<svg class="dg-wide" viewBox="0 0 750 230" role="img" aria-label="A run: parse, graph, plan, cache check; a hit reuses the artifact, a miss executes on a Python worker and is recorded"><defs><marker id="arr-run" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" class="dg-head"/></marker></defs><rect class="dg-box " x="10" y="20" width="130" height="56" rx="6"/><text x="75.0" y="45.0" text-anchor="middle">pipeline.py</text><text class="dg-sub" x="75.0" y="62.0" text-anchor="middle">your code</text><rect class="dg-box " x="160" y="20" width="130" height="56" rx="6"/><text x="225.0" y="45.0" text-anchor="middle">1. Parse</text><text class="dg-sub" x="225.0" y="62.0" text-anchor="middle">no import</text><rect class="dg-box " x="310" y="20" width="130" height="56" rx="6"/><text x="375.0" y="45.0" text-anchor="middle">2. Graph</text><text class="dg-sub" x="375.0" y="62.0" text-anchor="middle">check inputs</text><rect class="dg-box " x="460" y="20" width="130" height="56" rx="6"/><text x="525.0" y="45.0" text-anchor="middle">3. Plan</text><text class="dg-sub" x="525.0" y="62.0" text-anchor="middle">order into phases</text><rect class="dg-box dg-accent" x="610" y="20" width="130" height="56" rx="6"/><text x="675.0" y="45.0" text-anchor="middle">4. Cache check</text><text class="dg-sub" x="675.0" y="62.0" text-anchor="middle">look up run hash</text><path class="dg-edge " d="M140,48 L156,48" marker-end="url(#arr-run)"/><path class="dg-edge " d="M290,48 L306,48" marker-end="url(#arr-run)"/><path class="dg-edge " d="M440,48 L456,48" marker-end="url(#arr-run)"/><path class="dg-edge " d="M590,48 L606,48" marker-end="url(#arr-run)"/><rect class="dg-box dg-ok" x="610" y="160" width="130" height="56" rx="6"/><text x="675.0" y="185.0" text-anchor="middle">Cache hit</text><text class="dg-sub" x="675.0" y="202.0" text-anchor="middle">artifact reused</text><rect class="dg-box dg-run" x="330" y="160" width="220" height="56" rx="6"/><text x="440.0" y="185.0" text-anchor="middle">5. Execute</text><text class="dg-sub" x="440.0" y="202.0" text-anchor="middle">ready queue → Python workers</text><rect class="dg-box " x="60" y="160" width="210" height="56" rx="6"/><text x="165.0" y="185.0" text-anchor="middle">6. Record</text><text class="dg-sub" x="165.0" y="202.0" text-anchor="middle">metadata.db + .barca/artifacts/</text><path class="dg-edge " d="M675,76 L675,156" marker-end="url(#arr-run)"/><text class="dg-label" x="686" y="122" text-anchor="start">hit</text><path class="dg-edge " d="M640,76 C640,120 500,120 480,156" marker-end="url(#arr-run)"/><text class="dg-label" x="560" y="112" text-anchor="middle">miss</text><path class="dg-edge " d="M330,188 L274,188" marker-end="url(#arr-run)"/></svg>
<svg class="dg-tall" viewBox="0 0 340 552" role="img" aria-label="A run, top to bottom: parse, graph, plan, cache check; a hit reuses the artifact, a miss executes on a Python worker and is recorded"><defs><marker id="arr-run-t" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" class="dg-head"/></marker></defs><rect class="dg-box " x="70" y="10" width="200" height="52" rx="6"/><text x="170.0" y="33.0" text-anchor="middle">pipeline.py</text><text class="dg-sub" x="170.0" y="50.0" text-anchor="middle">your code</text><path class="dg-edge " d="M170,62 L170,78" marker-end="url(#arr-run-t)"/><rect class="dg-box " x="70" y="82" width="200" height="52" rx="6"/><text x="170.0" y="105.0" text-anchor="middle">1. Parse</text><text class="dg-sub" x="170.0" y="122.0" text-anchor="middle">no import</text><path class="dg-edge " d="M170,134 L170,150" marker-end="url(#arr-run-t)"/><rect class="dg-box " x="70" y="154" width="200" height="52" rx="6"/><text x="170.0" y="177.0" text-anchor="middle">2. Graph</text><text class="dg-sub" x="170.0" y="194.0" text-anchor="middle">check inputs</text><path class="dg-edge " d="M170,206 L170,222" marker-end="url(#arr-run-t)"/><rect class="dg-box " x="70" y="226" width="200" height="52" rx="6"/><text x="170.0" y="249.0" text-anchor="middle">3. Plan</text><text class="dg-sub" x="170.0" y="266.0" text-anchor="middle">order into phases</text><path class="dg-edge " d="M170,278 L170,294" marker-end="url(#arr-run-t)"/><rect class="dg-box dg-accent" x="70" y="298" width="200" height="52" rx="6"/><text x="170.0" y="321.0" text-anchor="middle">4. Cache check</text><text class="dg-sub" x="170.0" y="338.0" text-anchor="middle">look up run hash</text><rect class="dg-box dg-ok" x="190" y="400" width="140" height="52" rx="6"/><text x="260.0" y="423.0" text-anchor="middle">Cache hit</text><text class="dg-sub" x="260.0" y="440.0" text-anchor="middle">artifact reused</text><rect class="dg-box dg-run" x="10" y="400" width="160" height="52" rx="6"/><text x="90.0" y="423.0" text-anchor="middle">5. Execute</text><text class="dg-sub" x="90.0" y="440.0" text-anchor="middle">Python workers</text><rect class="dg-box " x="10" y="490" width="160" height="52" rx="6"/><text x="90.0" y="513.0" text-anchor="middle">6. Record</text><text class="dg-sub" x="90.0" y="530.0" text-anchor="middle">metadata.db + files</text><path class="dg-edge " d="M220,350 C220,375 260,380 260,396" marker-end="url(#arr-run-t)"/><text class="dg-label" x="268" y="384" text-anchor="start">hit</text><path class="dg-edge " d="M120,350 C120,375 90,380 90,396" marker-end="url(#arr-run-t)"/><text class="dg-label" x="82" y="384" text-anchor="end">miss</text><path class="dg-edge " d="M90,452 L90,486" marker-end="url(#arr-run-t)"/></svg>
<figcaption>What <code>barca get</code> does. Steps 1–4 happen in the Rust binary in milliseconds; only steps that miss the cache reach a Python worker.</figcaption>
</figure>

1. **Parse.** Each file is parsed with ruff's Python parser. Nothing is imported. The
   exceptions are a non-literal `partitions(...)` or `batches(...)` expression and a `batches_by`
   function, which a resolver subprocess evaluates.
2. **Graph.** `dag.rs` builds a petgraph graph from the decorators and rejects cycles, unknown
   inputs, a sensor with inputs, a task used as an input to an asset or sensor, and two nodes
   with the same id.
3. **Plan.** `planner.rs` splits the part of the graph the target needs into phases. A phase is
   a set of steps whose inputs are available when it starts.
4. **Cache check.** Each asset's run hash is looked up in the metadata database. A hit is
   served from its artifact file. Sensors and tasks always run.
5. **Execute.** Steps that need to run go on a ready queue. A pool of Python workers, one per
   CPU by default (`BARCA_POOL_SIZE` or `[execution] pool_size` overrides, capped at the
   capacity when `weight=` steps set a smaller one), takes them over a Unix domain socket.
6. **Record.** Each finished step is written to the metadata database as it finishes, and the
   run row is closed at the end. With a remote store, artifacts are uploaded in the background
   by the transfer helper, a step is recorded once its upload is confirmed (when the run
   ends), and the metadata database is backed up to the store after that.

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
`freshness=Schedule(...)`. The server binds `127.0.0.1`; authentication is off unless you turn on [project tokens](/auth/). See
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

`.barca/metadata.db` is a SQLite-compatible libSQL database written only by the Rust binary.
Its tables are `materializations` (one row per step result: node id, run hash, status,
artifact path, timings, attempts, error), `runs` (one row per run), `logs` (captured stdout of
steps), `cost_estimates` (the time estimate per node, used to size batches) and
`schedule_state` (when each scheduled node last fired, and whether it is paused) and
`schedule_failures` (the consecutive failed ticks of a scheduled node) and `sink_publications`
(the run hash each `@sink` last wrote) and `asset_pins` (assets held at an earlier
materialization by `barca pin`), and for `barca serve`
`run_queue` (the runs it accepted and has not finished, in order) and `server_state` (whether
that queue is paused).

Artifacts are files at `.barca/artifacts/{node}/{run_hash}{ext}`, in json, pickle, parquet or CSV.
They are cached by run hash: the hash covers the function's code, the helper code it reaches
and its inputs, not the bytes of the result. A step's result is always written to a file, and
the next step reads that file.

With a remote store configured, artifacts are still written locally first and then uploaded,
and the metadata database is backed up to the store, five copies kept; `barca state restore`
puts one back. See [Remote storage](/reference/remote-storage/).

## Dependencies

- **Rust**: `ruff_python_parser`, `petgraph`, `libsql`, `serde`, `sha2`, `tokio`, `croner`,
  `clap`, `toml`; the server adds `axum`, `notify`, `chrono-tz` and `rust-embed`.
- **Python**: 3.12 or later, standard library only. Extras: `parquet`, `fast` (orjson), and
  `s3`, `r2`, `gcs`, `azure`, `remote` (fsspec and the store's client).
- **Build**: maturin packages the binary and the Python package into one wheel. Since 0.22.0
  wheels are published for macOS on Apple Silicon and for Linux on x86-64 and arm64, glibc and
  musl.
