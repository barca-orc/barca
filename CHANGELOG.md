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

## Pending

### Performance

- Cold runs of large partitioned pipelines scale linearly with the number of keys again: a
  three-level chain of 20,000 keys runs cold in about 9.5 s instead of about 245 s, with a
  0.55 GB coordinator instead of 5.3 GB (#345). Each key of a step used to wait on every key of
  the previous step in its stream through its own edge; one shared gate per step now keeps the
  same ordering. Dispatch also finds an upstream's output by id instead of scanning every
  output, and the end-of-run check no longer scans every item on each event.

### Features

- `barca serve` runs the server half of the `--server` run contract (RFC-0007 §3.4, #393).
  `POST /runs` takes the flags of `barca get` / `barca run` as JSON and answers with the run's
  database id at once (`dry_run` answers with the `--dry-run` JSON); targets are checked first,
  in the CLI's words. `GET /runs/{run_id}/events` streams every stderr line a local run prints
  (`line`, tagged `when: agent | always`), step output (`log`), step results (`step_finished`,
  with `cached: true` for cache hits) and `run_finished` with the outcome, each event numbered
  (`id:`); a slow or reconnecting reader receives every event once, and the stream ends after
  `run_finished`. `GET /runs/{run_id}` answers with the outcome (the exact `--json` body, or the
  failed-run body and error envelope) from the metadata DB, so it survives a restart;
  `DELETE /runs/{run_id}` cancels; `GET /runs/{run_id}/output` serves the final artifact.
  `GET /list`, `/nodes`, `/history` and `/stats/{name}` return the CLI's `--json` bodies byte
  for byte, `GET /project` describes the deployed tree, and `GET /health` adds `api_version`,
  `env` and `store_uri`. Runs record `triggered_by` (`manual`, `api`, `schedule`, `client`) and,
  for scheduled runs, `scheduled_at` and `schedule_id`, in `barca history --json`, `GET /runs`
  and the `barca.run` span (`barca.trigger`, `barca.scheduled_at`, `barca.schedule_id`, #434).
  The web UI uses the new routes and shows what started each run.
- `--server [<url>]` runs `get`, `run`, `status`, `list`, `history` and `stats` on a
  `barca serve` and prints the answer as the local command prints it: the same stdout JSON, the
  same stderr lines, the same exit codes, rendered by the same functions (#394, experimental).
  With no URL it uses `BARCA_SERVER`, then `[server] url` in `barca.toml`, and says so once on
  stderr; nothing configured is a usage error. `--local` ignores a configured server (and is a
  usage error with `--server`); `--server` on `serve`, `plan`, `sql`, `doctor`, `logs`, `docs`
  and `version` is a usage error. A run streams its progress and step output, and Ctrl-C cancels
  it on the server (a second Ctrl-C exits 130 at once). A server that cannot be reached, is not
  barca or speaks another API version is exit 3 naming its URL; a stream that drops mid-run is
  exit 3 naming the run id. A named file that differs from the deployed one prints a note.
  `barca.get(..., server=True)` and `barca.run(..., server=True)` do the same from Python.
  `barca.Client` is deprecated in favour of them and warns when constructed. The binary now
  carries an HTTPS client (`reqwest` with `rustls`); `POST /runs` with `files` now reads only
  those files, as the CLI does. See `barca docs server-mode`.
- `POST /schedule/{id}/pause` (optionally `?until=<RFC 3339>`) and `POST /schedule/{id}/resume`
  hold one schedule back without `--no-schedule` (#435). `GET /schedule` reports `paused`,
  `paused_until` and `skip_reason`; a paused job's ticks are skipped with reason `paused` (also
  on the telemetry span) and are not caught up. Pauses are kept in `schedule_state`, so a restart
  does not resume a paused job.
- **Breaking (HTTP API):** every error body is the CLI error envelope (`error`, `code`, `kind`,
  `remediation`, ...) instead of `{"error"}` alone (`error` is still there). `GET /assets` and
  `GET /assets/{name}` return the `barca list --json` node shape (`freshness` a lowercase word,
  cron in `schedule`) and the `barca stats --json` body (`id` instead of `node_id`).
  `GET /runs/{id}` returns the flat run view with its `outcome` instead of `{run, steps, logs,
  result}`. Run ids returned by the trigger routes are the database run ids. `POST /run`,
  `POST /run/{target}`, `POST /get/{target}`, `DELETE /run/{run_id}`, `GET /status/{run_id}` and
  `GET /events/{run_id}` stay as aliases for one minor release; use `POST /runs`,
  `DELETE /runs/{run_id}`, `GET /runs/{run_id}` and `GET /runs/{run_id}/events`. Event streams
  now end after `run_finished` and carry an `id:` per event.

## 0.24.2 (2026-10-20)

### Bug Fixes

- `barca serve`: a cron tick is skipped while any run of the same node is pending or running,
  including one started by `POST /get`, `POST /run/{task}` or `POST /run`, not only one the
  scheduler started (#432). The skip is logged with the run in flight, reported as
  `last_skipped` and `skipped_for` in `GET /schedule`, and sent to telemetry as a `barca.run`
  span with status `skipped` and `barca.reason: in_flight`.

- Refresh dependency lockfiles (`uv.lock`, `ui/`, `site/`, the dagster benchmark) to clear the open critical and high Dependabot alerts: PyJWT 2.15.1, aiohttp 3.14.5, cryptography 50.0.2, urllib3 2.8.0, oauthlib 4.0.0, astro 7.4.1, wrangler 4.151.3, react-router 7.18.4 and vitest 4.1.12, plus patched transitives.

- Failed `barca.step` telemetry spans now report their duration, from the first attempt to the
  permanent failure (retries included; a timeout reports about its timeout), instead of a
  zero-length span at the end of the run.

- A worker that dies while running a step (#433) now names how it ended instead of a bare
  `worker disconnected`: `worker for <node> exited with SIGKILL (possibly the OOM killer)`,
  another signal by name, or `exited with code N`, in the step error, the run JSON and the
  failure summary. `get`, `run` and `serve` print `[barca] env <env> (<source>); artifact store
  <store>` once on stderr at startup when `default_env` from `barca.toml` or an artifact store
  is in effect.

- Follow a project module reached through another module's `import` (`from wrapper import
  helpers` or `wrapper.helpers.f()` where `wrapper.py` does `import helpers`), and follow
  import chains at any depth instead of stopping six project modules deep; modules that
  import each other terminate. Editing such a helper now invalidates its consumers. Existing
  affected consumers may recompute once after upgrading.

## 0.24.0

### Bug Fixes

- Follow project helper references in exception handler types, including exception tuples
  and `except*`. Editing those helpers invalidates their consumers. Existing affected
  consumers may recompute once after upgrading.

- Follow project helper references inside set literals. Editing those helpers now invalidates
  their cached consumers. Existing affected consumers may recompute once after upgrading.

- Follow project helper calls in assignment receivers and indices, including annotated, unpacked, loop, context-manager and comprehension targets. Editing such a helper now invalidates affected cached assets.

- Follow project helper references in `for`/`while` `else:` blocks so edits invalidate affected cached assets.

- Follow project helper values, classes and mapping keys in `match` patterns so edits invalidate affected cached assets.

- Follow project helper references in ordinary function/class decorators, including nested definitions and decorator-factory arguments.

- Retain project-helper provenance for unpacked and augmented module constants, including aliases captured before later overwrites. Consumers conservatively hash the module source and static references.

- Track project star imports in ordinary helper modules conservatively, including functions/classes, captured constants, forwarded exports, multiple/conditional imports and transitive stars.
- Follow project helper references in function and lambda default arguments, including
  nested functions and keyword-only defaults. Editing those helpers now invalidates their
  consumers. Existing affected consumers may recompute once after upgrading.

- Retain project-helper provenance for names, functions and classes bound inside module-level `for`, `while`, `with` and `match` blocks. Consumers conservatively hash module source and static references.

### Features

- Add `barca doctor` prerequisite checks, credential identity diagnostics and a documented local-only debugging profile.
- Run JSON reports executed-step durations and attempts, with slow module imports
  reported separately. History JSON totals recorded materialization attempts; stats
  retains per-materialization attempts. Timeout errors identify the step id.
- **Breaking:** agent completion and permanent failure lines append `attempts=N` after
  retries. Allow this optional suffix when parsing those lines, or read the structured
  `steps[].attempts` field instead.

### Removed

- **Breaking:** removed `barca get|run --no-cache` and `barca.get(no_cache=...)`.
  Use `--refresh-all` and `refresh_all=True`, respectively. The CLI alias is now an
  unknown-flag usage error (exit 2); Python rejects the removed keyword.

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
