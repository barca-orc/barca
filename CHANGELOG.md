# Changelog

All notable changes to this project will be documented in this file.

Generated from the conventional commit history with `git cliff --config cliff.toml`, one
section per release tag; the same text is the body of each GitHub Release. Commits since the
latest tag are not listed until the next release.

## [0.15.0] - 2026-10-05

### Bug Fixes

- A store copy that differs from its recorded hash warns instead of failing the run (#241)

### Features

- Check a local copy of a stored artifact against its recorded hash (#240)
- Local-first artifacts with background transfer to the remote store (#145)

### Release

- V0.15.0 (#242)

## [0.14.0] - 2026-10-05

### Bug Fixes

- Lazy inputs read remote parquet in place instead of downloading it (#238)
- Pl.LazyFrame inputs arrive as a LazyFrame scan, not a loaded DataFrame (#237)

### Release

- V0.14.0 (#239)

## [0.13.4] - 2026-10-05

### Features

- Read remote artifact shapes; barca sql queries remote results (#219)

### Polish

- Print a repeated library warning once per run, then a count (#218)

### Release

- V0.13.4 (#222)

## [0.13.3] - 2026-10-05

### Bug Fixes

- Keep staging files until they are read; one staging dir per worker (#216)

### Release

- V0.13.3 (#217)

## [0.13.2] - 2026-10-04

### Bug Fixes

- Sink bugs found by executing the manual; clippy in CI; small cleanups (#210)

### Release

- V0.13.2 (#211)

## [0.13.1] - 2026-10-04

### Bug Fixes

- Configure remote storage with environment variables alone (#207)

### Release

- V0.13.1 (#209)

## [0.13.0] - 2026-10-04

### Features

- Barca sql queries cached results with DuckDB (experimental) (#206)
- Cross-file inputs through ordinary imports; never guess a name (#205)
- Read every file in the project; file arguments are optional (#204)
- Run from the project root, the nearest barca.toml above the cwd (#203)

### Release

- V0.13.0 (#208)

## [0.12.0] - 2026-10-03

### Bug Fixes

- A target name selects exactly that node, never a suffix match (#192)
- Partitions_from passes each key and its upstream output (#191)
- Resolve CLI surface inconsistencies before 1.0 (#186)

### Changes

- Run test_run_refresh.sh; guard that CI runs every integration script (#147)

### Features

- A sensor's output invalidates the assets that read it (#188)

### Release

- V0.12.0 (#193)

## [0.11.0] - 2026-10-02

### Bug Fixes

- Hash helper modules with a bare filename and for module-attribute calls (#182)
- Never run a stale .pyc; validate cached bytecode by source hash (#179)
- Bare barca get materializes assets and sensors, never tasks (#177)
- Pass unpartitioned inputs to partitioned assets; hash them first (#175)

### Documentation

- CLI contract (barca docs contract) with help and JSON schema snapshots (#181)

### Release

- V0.11.0 (#184)

## [0.10.0] - 2026-10-02

### Bug Fixes

- A step's print() output goes to stderr, not stdout (#172)
- Failed runs still print a JSON result line (#163)
- Usage errors state the fix and point at barca list (#161)
- A step that calls sys.exit() or raises KeyboardInterrupt must fail (#160)

### Documentation

- Ship SKILL.md for agents, embedded as barca docs skill (#173)

### Features

- Barca status — cache state, last run and artifact shape per node (#169)
- --refresh cascades downstream by default; add --no-cascade (#168)
- Several targets per run: barca run a,b / barca get a,b (#167)
- Declared env dependencies with @asset(env=[...]) (#166)
- Bounded list output with --limit, --all and --fields (#165)
- TTY-aware output defaults across all commands (#164)
- Structured error envelope on stderr; exit codes 1/2/3/130 (#162)

### Release

- V0.10.0 (#174)

## [0.9.0] - 2026-10-02

### Bug Fixes

- Make --refresh unambiguous for agents; report long-running steps (#143)
- Concurrent barca processes queue on the metadata DB instead of failing (#136)
- Parallel() children no longer push the step counter past the total (#139)
- Write duckdb relations and pyarrow Tables as parquet (#133)

### Changes

- Drop the macOS x86_64 (Intel) wheel build (#131)
- Point repo links at barca-orc/barca; drop the dead CI badge (#138)
- Drop sccache; it broke rust-cache on Linux (#132)
- Run release builds on Depot runners and fix cold caches (#130)

### Features

- --dry-run for get and run; real runs report what each step did (#144)
- Cache partitioned steps per key (#140)
- One shared connection per worker, inputs bound as views, barca.duckdb_connection() (#135)
- Built-in manual (barca docs), --help examples, --json on list/history/stats (#134)
- Respect type annotations for parquet frame loaders (#126)

### Release

- V0.9.0 (#142)

## [0.8.0] - 2026-10-01

### Bug Fixes

- Pin bitnamilegacy/minio for the backends job (#127)
- Point CI and license badges to ExSidius/barca (#120)
- Resolve relative imports against parent package, not submodule's own name (#118)
- Deliver full partition list to collect() fan-in consumers (#114)
- Reject non-5-field and malformed cron in Schedule(...) (#115)

### Changes

- Run CI on Depot CI (#128)
- Rename docs site package/worker from barca-docs to barca (#119)
- Add Cloudflare Workers configuration (#96)

### Features

- Make barca run cache-aware by default; rename --burst to --refresh (#125)
- Sub-minute cron scheduling + surface the task-scheduler workflow (#121)

### Release

- V0.8.0 (#129)

### Testing

- Add scheduler_overhead benchmark + real-hardware results (#123)

## [0.7.0] - 2026-07-17

### Bug Fixes

- Give Prefect real concurrency on benchmarks with parallel branches (#111)
- Install pandas/pyarrow for etl_duckdb_dataframes smoke test
- Address Indent review findings on PR #94
- Correct per-step timing to include artifact serialization
- Fix dependency wiring for partitioned steps across streams
- Fix run.sh path bugs and cache/collision bugs found rerunning suite

### Changes

- Standardize benchmark CPU/RAM fairness, fix partition-wiring regression, fix etl_duckdb serialization loss (#94)

### Documentation

- RFC process + initial baseline RFCs on the docs site (#99)
- Add Scheduling guide surfacing the cron task-scheduler use case (#110)
- Fix stale benchmark counts and README benchmark suite table
- Fix drift between docs site and implementation (reference + comparisons) (#98)
- Document the etl_duckdb serialization investigation and fix
- Correct the noise-vs-real diagnosis, add peak memory table
- Record full 18-benchmark standardized re-run with variance
- Update git workflow to trunk-based development model (#91)

### Features

- Add Astro + Starlight documentation site (#95)
- Add etl_duckdb_dataframes, a DataFrame/parquet variant
- Opt-in whole-process-tree peak memory measurement
- Add bench.sh to the remaining script-mode benchmarks
- Adaptive pull-queue executor with measured-cost batch sizing (#92)
- Allow BARCA_POOL_SIZE env override for worker pool size

### Performance

- Fix worker-pool shutdown tax + Docker harness for reproducible benchmarking (#112)
- Use pickle serializer for etl_duckdb's heaviest payloads

### Polish

- Standardize CPU pinning and worker counts across frameworks

### Refactor

- Async-native core — runtime owned by the caller, cancellable runs (#88)
- Switch ruff crates from git pin to crates.io =0.0.4 (#89)

### Release

- V0.7.0 (#113)

### Testing

- Wire benchmark examples and partition correctness into CI

## [0.6.1] - 2026-07-12

### Bug Fixes

- Exit worker on SIGTERM via os._exit, not sys.exit

### Changes

- Run the full Python suite against the backend emulators

### Release

- Release: v0.6.1

## [0.6.0] - 2026-07-12

### Bug Fixes

- Correct GCS and Azure shared-state conflict handling

### Changes

- Scope backends job to state and conformance tests

### Features

- First-class S3, GCS, and Cloudflare R2 object stores

### Release

- Release: v0.6.0

## [0.5.0] - 2026-07-11

### Documentation

- Doc: config reference, shared-state guide, 0.5.0 scope

### Features

- Shared remote state, content-addressed artifacts, barca.toml, --env

### Release

- Release: v0.5.0

### Testing

- Make shared-state e2e exec-count check portable to BSD wc

## [0.4.0] - 2026-07-11

### Bug Fixes

- Surface error types/tracebacks, real retry backoff, fresh worker per attempt

### Documentation

- Doc: remote storage guide and @sink serializer docs
- Mark 0.3.0 as current release, 0.2.0 as shipped

### Features

- Remote artifact backends (ADLS-first) and @sink execution

### Release

- V0.4.0

## [0.3.0] - 2026-07-10

### Features

- Cron scheduling in barca serve + Python server client

### Release

- V0.3.0

## [0.2.1] - 2026-06-11

### Bug Fixes

- Auto-create .barca/.gitignore on init

### Changes

- Bump version to 0.2.1, add version-sync pre-commit hook

### Documentation

- Add dagster and prefect comparison docs
- Fix README inaccuracies from new-user walkthrough (#71)

### Features

- Add barca list command for definition discovery

### Polish

- Address PR review feedback

## [0.2.0] - 2026-06-11

### Bug Fixes

- ParallelError serialization and serializer defaulting
- Plumb actual error messages to ParallelError, add SIGTERM handler
- Propagate node kind to workers, fix sensor unpacking and CI
- Address 36 PR review comments
- Version bump to 0.2.0, address pre-merge audit findings
- Unique socket paths per dispatch (prevent path collisions)
- Address final 6 PR #69 review comments
- Unique artifact paths for parallel branches
- Address 16 remaining PR #69 review comments
- Gitignore example uv.lock files
- Update example pyproject.toml to use PyPI barca by default
- Remove stale rust-rewrite branch from CI triggers
- Address 4 PR review comments on retry/scheduler
- Address 2 PR review comments on task decorator

### Changes

- UDS coordinator with SIGSTOP/SIGCONT parallel dispatch
- Address 8 PR review comments on barca-server
- Add barca serve: long-running HTTP API server (#53)
- Add Rust-owned retries with error/attempt tracking and anti-pileup scheduling
- Add first-class @task node, remove @effect (issue #58)

### Documentation

- Deepen user API decisions with concrete alternatives
- Add user API decisions document
- Add Allium spec for user-facing API contract
- Consolidate docs — delete 7 stale files, slim getting-started
- Move Docker benchmarks to v0.3.0 scope
- Release roadmap, Schedule caveat, final cleanup
- Clean up stale documentation for v0.2.0
- Add architecture decisions document (ADR) for v0.2.0
- Update documentation for v0.2.0 architecture
- Add versioning policy to CLAUDE.md
- Add git workflow, conventional commits, and CI branch patterns

### Features

- UDS coordinator with SIGSTOP/SIGCONT parallel dispatch
- Add async I/O loop (io_loop.rs)
- Add queue-based Coordinator (coordinator.rs)
- Wire Unix socket protocol into dispatch.rs (production path)
- Add Python socket runtime (_runtime.py)
- Add executor module (executor.rs)
- Add socket protocol module (protocol.rs)
- Add pure WorkPlan layer (work_plan.rs)
- Add parallel_tasks benchmarks + update patterns doc
- Parallel() stage 3 — sub-worker dispatch
- Parallel() stage 2 — coroutine protocol plumbing
- Parallel() stage 1 — model, parser, and Python stubs

### Performance

- Async tokio sockets for parallel dispatch (true concurrency)
- Rewrite parallel() dispatch to use batched execution model

### Polish

- Fix pull→push terminology across docs

### Refactor

- Enforce get/run semantics, remove after=, add patterns docs

### Removed

- Drop all PYTHON_GIL references

### Testing

- Add socket stress tests (isolate accept pattern)
- Add 6 invariant-based parallel tests
- Add 27 runtime protocol tests + verify all existing tests pass
- Add 10 parallel() integration tests
- Add parallel() end-to-end test example

## [0.1.5] - 2026-06-05

### Changes

- Address 3 PR review comments: macOS CPU parsing, Arc for StepId, surface benchmark errors
- Update framework comparison with partitioned benchmark results
- Add partitioned_10k benchmark: Docker-based fair comparison across frameworks
- Late partition expansion: workers expand partitions, planner emits compact descriptions
- Bump plan_2002 threshold to 500ms (10k partition expansion is legitimately ~200ms)
- Add performance profiler with regression thresholds (pre-push hook)
- Fix O(n²) parse bug: cache module definitions for cone hashing (54x speedup)
- Add timeseries_1000 benchmark: 2002 assets, barca vs dagster vs prefect
- Strip+LTO (17→13MB binary), Arc<str> clones, drop file_sources
- Refactor StreamStep/StepId to Arc<str> for zero-cost clones
- Result error handling + performance benchmarking infrastructure
- Refactor unwrap/expect to proper Result propagation, bump to v0.1.5 (#62)

## [0.1.4] - 2026-06-05

### Changes

- Address 4 PR review comments: dead branch, empty stderr, NULL elapsed, issue #63
- Rust code quality: zero clippy warnings, type alias for callback
- Clean up: remove dead dispatch_plan, fix clone warnings, remove unused import
- Fixed-width ETA in progress bar: no layout shifts ever
- Progress bar info hierarchy: ETA left, bar center, step name right
- Fix progress bar layout shift: use wide_msg + finish_and_clear
- Streaming per-step progress via mpsc channels
- Indicatif progress bar + --agent flag for structured output
- Add median/max/p95 to stats, fix Python parser, add timing distribution tests
- Add subdirectory file path tests for get API
- Unify `run` and `get` into a single `get` command
- Run history, per-asset timing, progress bar, barca history/stats commands (#50)
- Address PR review: relative imports, multi-hop re-exports, package precedence
- Cross-file cone analysis for subdirectory imports and __init__.py re-exports

## [0.1.3] - 2026-06-05

### Changes

- Update framework comparison: add task workflows, APM, partition filter links
- Data quality: not a gap — users have pydantic/pandera/asserts, failures block downstream
- Fix framework comparison: backfills and dynamic partitions are not gaps
- Update framework comparison with roadmap links and decisions
- Framework comparison: add honest tradeoffs section
- Add framework comparison doc: aesthetics, transparency, overhead
- Add reliability tests: partial results, timeout, fan-in cache
- Fix Airflow benchmarks: use dags test (not backfill) with LocalExecutor
- Partial result persistence, fan-in cache fix, timeout enforcement, Airflow benchmarks

## [0.1.2] - 2026-06-05

### Changes

- Remove unreachable frozen runpy check (already covered by frozen prefix)
- Address 4 PR review comments: traceback filtering, path matching, -O safety, shorthand flags
- Add ty type checker to prek, fix type narrowing in _worker.py
- Output modes, clean errors, version subcommand, partition first-not-last
- Include LICENSE in sdist for PyPI compliance

## [0.1.1] - 2026-06-04

### Changes

- Fix CI: clear stale wheels before maturin build
- Fix release workflow: checkout before download-artifact
- Address inline review comments: sentinel envelope, version check, run() sort
- Add git-cliff for automated changelog generation
- Fix 3 PR review issues: artifact key collision, binary caching, deterministic partition get
- Correct __main__.py entry-point description in CLAUDE.md
- Clean up stale files from pre-Rust rewrite
- Result errors, default subcommand, versions, test isolation, README
- Engine refactor + clap CLI + Python API
- Engine refactor + clap CLI + Python API

## [0.1.0] - 2026-06-04

### Bug Fixes

- Make UI reactive to asset state changes after reset/reindex (#14)
- Support bare @asset decorator (no parentheses) (#12)

### Changes

- Fix PyPI license metadata: use SPDX identifier instead of text table
- Drop linux-arm64 from release matrix (simsimd NEON cross-compile failure)
- Release polish: metadata, changelog, serializer= parsing, __version__
- Fix all 11 PR review issues: CI, correctness, latent bugs, nits
- File-based artifact persistence: replace JSON-over-stderr with format-aware artifacts
- Require Python >=3.12, build wheels for 3.12/3.13/3.14
- Restore full release workflow from prior working config
- Use PYPI_API_TOKEN secret for PyPI publish (already configured)
- Engine hardening: refactor, protocol, first-class partitions, P0 fixes, CI/CD
- Fix all 3 staleness gaps: cross-file imports, sensor bypass, partition cascade
- Rewrite gap tests: cross-file, sensor, partition (drop ops concerns)
- Add gap tests documenting known cache/staleness limitations
- Add cache fuzz tests: 100 random DAGs × mutate × verify staleness
- Fix chain caching (ordered persist) + add cached benchmarks
- Fix run_hash consistency: 13/13 cache tests pass
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
- Remove dead code: plan.rs, NodeState, classify_shape, stats
- Make parser pure: return Result, add 10 edge case tests
- Add integration tests for CLI behavior
- User print() statements no longer corrupt worker protocol
- Document Airflow 3 LocalExecutor limitations
- Document Airflow LocalExecutor limitation: requires PostgreSQL for parallelism
- Add Airflow benchmarks: trivial, chain_100, deep_diamond, fan_out_500_50ms
- Complete benchmark fairness: server-mode dagster, parallel prefect
- Add server-mode benchmark scaffolding for dagster
- Address benchmark fairness concerns (ExSidius/barca#35)
- Add ETL pipeline, wide join, and incremental backfill benchmarks
- Add large_payloads, map_reduce, and multi_file_discovery benchmarks
- Add deep_diamond, wide_layers, and mixed_io_cpu benchmarks
- Wire up multi-process dispatch: workers communicate via stdout, Rust owns DB
- Add execution planner: Dag → decompose → Topology → plan → ExecutionPlan
- Add spaceflights, fan_out_500, and fan_out_500_50ms benchmarks
- Add benchmark results README
- Use SmallVec for inputs/sinks/partition_keys (stack allocation for small collections)
- Fix all clippy warnings, tighten idiomatic Rust
- Add comprehensive grammar spec tests, fix int parsing
- Rewrite barca as Rust binary with Python execution runner
- Add AGENTS.md (#33)
- Refactor to align with barca.allium: freshness, sinks, run/dev/prune (#32)
- Replace Datastar/Jinja2 UI with React + shadcn/ui (#31)
- Add comprehensive tests for multicore asset execution (#30)
- Add learning path to README, show artifact path after refresh, fix broken examples (#29)
- Replace plain text CLI output with Rich-formatted tables and panels (#28)
- Fix thread-safety in MetadataStore for concurrent per-thread usage (#27)
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
- Remove AssetStatusBadge web component; use data-persist for dark mode (#15)

### Documentation

- Fix datastar-reference for RC.8 syntax (#13)
- Fix Quick Start so new users can actually run barca (#11)

## [0.0.3] - 2026-03-14

### Changes

- Bump version to 0.0.3
- Use cross for Linux CLI builds in just release

## [0.0.3rc1] - 2026-03-14

### Bug Fixes

- Use command -v to check for cargo-zigbuild
- Use uv tool install ziglang + symlink to zig in setup recipe
- Ignore untracked files in dirty check
- Handle PEP 440 rc versions in just release (convert to semver for Cargo)

### Changes

- Bump version to 0.0.3rc1
- Sync uv.lock
- Replace release workflow with local just release recipe
- Use macos-latest for x86_64 macOS wheel (macos-13 unavailable)
- Bump all crate versions to 0.0.3
- Fix manylinux glibc compatibility for bundled CLI binary (#10)
- Init (#1)
- Initial commit
