# Changelog

Current release notes are published on [GitHub Releases](https://github.com/barca-orc/barca/releases).
Recent tagged release notes include [v0.21.0](https://github.com/barca-orc/barca/releases/tag/v0.21.0)
(2026-10-09): SQL installation extra, remote-off override, storage startup checks,
non-destructive metadata evolution, transactional terminal outcomes and genuine
Barca decorator aliases. See its upgrade notes for the storage permission and
binding compatibility requirements. The preceding [v0.20.1](https://github.com/barca-orc/barca/releases/tag/v0.20.1)
(2026-10-09) shipped inspectable run history and triggered runs, shared-history cancellation
reconciliation, and verified shared DuckDB worker lifetime. See
[v0.20.0](https://github.com/barca-orc/barca/releases/tag/v0.20.0) for the preceding
upgrade and breaking-change notes. Releases between the historical entries below
and these tags are available on the same releases page.

The older notes below are preserved as historical records. Their former
"Unreleased" section describes work that has already shipped; it is not the
current pending-release queue.

## Historical notes (formerly "Unreleased")

### Breaking changes

- `barca run` is now cache-aware for upstream assets by default (same as `barca get`);
  the task itself still always re-runs. Previously every upstream asset was force-rerun.
  `--burst` is renamed to `--refresh <a,b>`; add `--refresh-all` (alias `--no-cache`) to
  restore the old default. Python API: `barca.run(..., burst=[...])` is now
  `refresh=[...]` / `refresh_all=True`.

### Features

- `--dry-run` on `barca get` and `barca run`: reports, for the exact command and flags, which
  steps are served from cache and which will run and why (`cached` / `run` / `partial` /
  `unknown`), without executing or writing anything. Real runs now report the same per step in a
  `steps` array (and `[barca] step:<id> cached` in `--agent` mode). The dry run and the real run
  share one decision function, and tests check that the predicted `will_run` equals the real
  run's `steps_executed`.
- `barca docs`: a manual compiled into the binary (concepts, output formats, caching, tasks,
  partitions, scheduling, runnable examples, conventions for scripts and AI agents). Every
  command's `--help` now ends with runnable examples, and `list`, `history` and `stats` take
  `--json`. Docs, help examples and JSON output are now part of the feature workflow
  (see CLAUDE.md); tests execute the manual's examples.
- DuckDB: steps returning a `DuckDBPyRelation` or a pyarrow `Table` are written as parquet
  without `serializer="parquet"` (a relation used to crash with "cannot pickle", a Table was
  silently pickled). Barca now owns one DuckDB connection per worker process and binds every
  duckdb-typed input as a view named after its parameter for the step, so SQL by name works
  in helpers without bind code; `barca.duckdb_connection()` exposes the connection for
  one-time configuration (extensions, credentials, settings). Steps that mix relations from
  their own `duckdb.connect()` with inputs still fail, but the failure now carries a `barca:`
  note explaining the conflict and the fix instead of only DuckDB's cryptic message.
- Sub-minute cron scheduling: `Schedule(...)` now accepts a 6-field cron with a
  leading seconds field (e.g. `*/15 * * * * *` — every 15 seconds); the `barca serve`
  scheduler evaluates at 1-second resolution. 5-field crons are unchanged (seconds
  pinned to `0`).
- Built-in cron scheduler in `barca serve`: enforces `@asset`/`@task`/`@sensor`
  `freshness=Schedule(...)`, timezone-aware (`--timezone`), durable catch-up on
  restart, self-overlap skip, and live status via `GET /schedule`; toggle with
  `--no-schedule`.
- Shared remote state and content-addressed artifacts: `barca.toml` config, `--env`
  environment separation, and remote artifact storage (Azure/S3/GCS/R2 via fsspec)
  with the metadata DB pushed/pulled as a blob.
- Minimal standalone scheduler example under `examples/scheduler`.

### Bug Fixes

- `--refresh` is no longer easy to misuse: a name that is not an upstream asset is an error that
  lists the valid names (it used to silently do nothing), `--refresh a b` says to use a comma
  instead of failing with "No such file", and barca warns when a refresh leaves cached assets
  downstream of it stale (run hashes cover upstream hashes, not outputs, so they stay cached).
- A step that runs for 15 s or more is now reported on stderr (`[barca] still running (45s): ...`,
  `BARCA_PROGRESS_SECS` to tune) in every mode, so a slow step no longer looks hung.
- Partitioned assets are now cached per key. Previously every partition re-executed on each
  run (a `TODO` in the coordinator); now unchanged keys are served from cache and only new or
  changed keys run, with the fan-in re-running only when its inputs change.
- Concurrent barca processes in one project no longer fail with `Failed locking file
  '.barca/metadata.db'. File is locked by another process`. Turso opens the DB
  single-process (its multi-process mode is experimental), and `barca get`/`run` held the DB
  open for the whole run. Barca now holds a short cross-process lock (`.barca/metadata.db.lock`)
  only while it reads or writes the DB and releases it while your Python runs, so processes
  queue instead of failing.

### Refactor

- Async-native core: the async runtime is owned by the caller, with cancellable runs.
- Centralized cron parsing behind a single `CronExpr::parse` helper so validation,
  the scheduler, and the `/schedule` handler share one grammar.

### Documentation

- Surface the task-scheduler workflow in the README and docs splash; new
  "Barca vs cron / systemd timers" comparison; sub-minute cron documented across
  the Scheduling guide and reference.
- Real `freshness=` (and other) keyword parameters on the `@asset`/`@sensor`/`@task`
  Python stubs for IDE autocomplete and type checking.

## [0.1.1] - 2026-06-04

### Bug Fixes

- Fix 3 PR review issues: artifact key collision, binary caching, deterministic partition get

### Changes

- Correct __main__.py entry-point description in CLAUDE.md
- Clean up stale files from pre-Rust rewrite

### Polish

- Polish: Result errors, default subcommand, versions, test isolation, README

### Release

- V0.1.1: Engine refactor + clap CLI + Python API
- V0.1.1: Engine refactor + clap CLI + Python API

## [0.1.0] - 2026-06-04

### Bug Fixes

- Fix PyPI license metadata: use SPDX identifier instead of text table
- Fix all 11 PR review issues: CI, correctness, latent bugs, nits
- Fix all 3 staleness gaps: cross-file imports, sensor bypass, partition cascade
- Fix chain caching (ordered persist) + add cached benchmarks
- Fix run_hash consistency: 13/13 cache tests pass
- Fix: user print() statements no longer corrupt worker protocol
- Fix all clippy warnings, tighten idiomatic Rust
- Fix thread-safety in MetadataStore for concurrent per-thread usage (#27)
- Fix: make UI reactive to asset state changes after reset/reindex (#14)
- Fix: support bare @asset decorator (no parentheses) (#12)

### Changes

- File-based artifact persistence: replace JSON-over-stderr with format-aware artifacts
- Require Python >=3.12, build wheels for 3.12/3.13/3.14
- Restore full release workflow from prior working config
- Use PYPI_API_TOKEN secret for PyPI publish (already configured)
- Engine hardening: refactor, protocol, first-class partitions, P0 fixes, CI/CD
- Rewrite gap tests: cross-file, sensor, partition (drop ops concerns)
- Add gap tests documenting known cache/staleness limitations
- Add cache fuzz tests: 100 random DAGs × mutate × verify staleness
- Add dependency cone analysis for staleness detection
- Implement `barca get` with cache-hit detection and staleness tracking
- Add cache/staleness integration tests (TDD: all currently fail)
- Add dagster server-mode partition benchmarks
- Add dagster/prefect for partition benchmarks, run full comparison
- Add partition benchmarks with partition-aligned stream assignment
- Implement dynamic partition eval and partitions_from resolution
- Implement static partition expansion and dynamic partition parsing
- Clean up DagNode duplication, remove dead code, tighten types
- Comprehensive planner tests documenting DAG shape → execution plan mapping
- Make parser pure: return Result, add 10 edge case tests
- Add integration tests for CLI behavior
- Add Airflow benchmarks: trivial, chain_100, deep_diamond, fan_out_500_50ms
- Complete benchmark fairness: server-mode dagster, parallel prefect
- WIP: Add server-mode benchmark scaffolding for dagster
- Address benchmark fairness concerns (ExSidius/barca#35)
- Add ETL pipeline, wide join, and incremental backfill benchmarks
- Add large_payloads, map_reduce, and multi_file_discovery benchmarks
- Add deep_diamond, wide_layers, and mixed_io_cpu benchmarks
- Wire up multi-process dispatch: workers communicate via stdout, Rust owns DB
- Add execution planner: Dag → decompose → Topology → plan → ExecutionPlan
- Add spaceflights, fan_out_500, and fan_out_500_50ms benchmarks
- Add benchmark results README
- Use SmallVec for inputs/sinks/partition_keys (stack allocation for small collections)
- Add comprehensive grammar spec tests, fix int parsing
- Rewrite barca as Rust binary with Python execution runner
- Add AGENTS.md (#33)
- Replace Datastar/Jinja2 UI with React + shadcn/ui (#31)
- Add comprehensive tests for multicore asset execution (#30)
- Add learning path to README, show artifact path after refresh, fix broken examples (#29)
- Replace plain text CLI output with Rich-formatted tables and panels (#28)
- Consolidate three packages into one + GitHub install instructions (#25)
- Update documentation with GitHub installation and CLI options (#24)
- Switch from sqlite3 to libsql as primary DB driver for MVCC concurrency (#23)
- Update documentation: reflect notebook workflow, add MkDocs, improve code docs (#22)
- Add notebook workflow helpers: load_inputs, materialize, read_asset, list_versions (workflow 7) (#21)
- Make sensors first-class nodes with dedicated CLI, API, and display (workflow 6) (#20)
- Add spaceflights benchmark: 10-asset diamond DAG adapted from Kedro (#19)
- Add test coverage roadmap with detailed spec for 100+ new tests (#18)
- Improve dependency tracking with codebase merkle hash, snapshots, and batch worker (#17)
- Add benchmark suite: Barca vs Prefect vs Dagster orchestration performance (#16)

### Documentation

- Document Airflow 3 LocalExecutor limitations
- Document Airflow LocalExecutor limitation: requires PostgreSQL for parallelism
- Docs: fix datastar-reference for RC.8 syntax (#13)
- Docs: fix Quick Start so new users can actually run barca (#11)

### Refactor

- Refactor to align with barca.allium: freshness, sinks, run/dev/prune (#32)

### Release

- Release polish: metadata, changelog, serializer= parsing, __version__

### Removed

- Drop linux-arm64 from release matrix (simsimd NEON cross-compile failure)
- Remove dead code: plan.rs, NodeState, classify_shape, stats
- Remove AssetStatusBadge web component; use data-persist for dark mode (#15)

## [0.0.3] - 2026-03-14

### Changes

- Chore: bump version to 0.0.3
- Chore: use cross for Linux CLI builds in just release

## [0.0.3rc1] - 2026-03-14

### Bug Fixes

- Fix: use command -v to check for cargo-zigbuild
- Fix: use uv tool install ziglang + symlink to zig in setup recipe
- Fix: ignore untracked files in dirty check
- Fix: handle PEP 440 rc versions in just release (convert to semver for Cargo)

### Changes

- Chore: bump version to 0.0.3rc1
- Chore: sync uv.lock
- Ci: replace release workflow with local just release recipe
- Ci: use macos-latest for x86_64 macOS wheel (macos-13 unavailable)
- Chore: bump all crate versions to 0.0.3
- Ci: fix manylinux glibc compatibility for bundled CLI binary (#10)
- Init (#1)
- Initial commit
