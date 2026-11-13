---
pagefind: false
title: Testing
description: The test suites in the barca repository, how to run each, and what CI runs.
---

## The suites

| Suite | Where | Run with |
|---|---|---|
| Rust tests | inline `#[test]` modules in `crates/*/src/`, plus `crates/barca-core/tests/` and `crates/barca-server/tests/` | `cargo test` |
| Python tests | `python/tests/` | `pytest python/tests -q` |
| Storage backend tests | part of `python/tests/`, against local emulators | `pytest python/tests -q` with the emulators running |
| Shell integration tests | `tests/integration/*.sh` | `bash tests/integration/<script>.sh` |
| Web UI tests | `ui/` | `pnpm --dir ui test` |
| Manual and contract tests | `cargo test -p barca`, `python/tests/test_docs_examples.py`, `python/tests/test_cli_contract.py` | see below |

The Python and shell tests run the installed `barca` binary, so build it first
(`maturin develop --uv --release --extras test`, see [Development Setup](/contributing/development/)).

## Rust

```bash
cargo test                # every crate
cargo test -p barca       # the CLI crate, including the manual and contract checks
```

Most Rust tests are inline in the module they test (parser, DAG, hashing, cache decisions,
coordinator, scheduler, config). The separate test files are:

| File | What it covers |
|---|---|
| `crates/barca-core/tests/grammar_spec.rs` | Parsing of decorator syntax |
| `crates/barca-core/tests/socket_stress.rs` | The worker socket protocol under load |
| `crates/barca-core/tests/unused_input_repo_sweep.rs` | The unused-input warning against the repository's own examples and docs, and that none of them passes a decorator an argument barca does not define |
| `crates/barca-server/tests/api.rs` | The HTTP endpoints of `barca serve` |

## Python

```bash
pytest python/tests -q
pytest python/tests/test_sql.py -q        # one file
```

Most tests in `python/tests/` create a temporary project with decorated functions, run the
real `barca` binary on it, and check the output, exit code and files.

Each test has a timeout (pytest-timeout, from the `test` extra): 120 s by default (`timeout` in
`[tool.pytest.ini_options]` in `pyproject.toml`), 300 s in the files whose tests are known to
take longer (`test_minute_checkpoints.py`, `test_remote_faults.py`, `test_simulation.py`, with a
module-level `pytest.mark.timeout(300)`). A test past its timeout fails with the stacks of its
threads, so a hang costs minutes instead of the CI job's own limit. A test that needs optional packages (pandas, pyarrow, duckdb,
jsonschema) skips without them through `pytest.importorskip`, so the suite collects without
the `test` extra.

barca prints through `barca_core::outln!` and `errln!`, which do not panic when the reader of
stdout or stderr has gone; clippy denies `println!` and `eprintln!` in the three crates.
Workers do not hold barca's stderr when it is a pipe: they write to a pipe barca reads and
forwards (`crates/barca-model/src/term.rs`), so a step's child processes cannot meet a closed
pipe either. `python/tests/test_closed_pipe.py` starts barca with a pipe whose read end is
already closed, so the first write fails every time and no test waits for a reader to exit.

Tests that must act while a helper process is in the middle of something (a Ctrl-C during an
upload, in `test_remote_cancel.py`) do not sleep and hope: `python/tests/hold/` is a
`sitecustomize` shim that pauses a barca helper at a named point until the test releases it
(`BARCA_TEST_HOLD=<point>:<dir>`).

## Storage backend tests and emulators

Tests of remote storage and metadata backups (`test_state_backends.py`, `test_remote_faults.py`,
`test_remote_env_config.py`, `test_remote_inspect.py` and others) run against local emulators
and need no cloud account:

| Store | Emulator | Variable | Default |
|---|---|---|---|
| S3 and R2 | MinIO | `BARCA_TEST_S3_ENDPOINT` | `http://localhost:9100` |
| GCS | fake-gcs-server | `BARCA_TEST_GCS_ENDPOINT` | `http://localhost:9200` |
| Azure | Azurite | `BARCA_TEST_AZURITE_HOST` | `127.0.0.1:9210` |

`BARCA_TEST_S3_KEY` and `BARCA_TEST_S3_SECRET` default to `minioadmin`.

Without the variables set, a test whose emulator is not reachable at the default address is
skipped, so the suite passes on a machine with no emulators. When a variable is set and its
emulator is not reachable, every test that needs it fails instead of skipping
(`python/tests/emulators.py`; `test_emulators.py` tests that rule). CI sets all three, so a
broken emulator cannot make the backend job pass by skipping.

CI starts the emulators with Docker, at pinned versions:

```bash
docker run -d --name minio -p 9100:9000 \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  bitnamilegacy/minio:2025.5.24
docker run -d --name fake-gcs -p 9200:4443 \
  fsouza/fake-gcs-server:1.56.1 -scheme http -port 4443 \
  -public-host localhost:9200 -external-url http://localhost:9200
docker run -d --name azurite -p 9210:10000 \
  mcr.microsoft.com/azure-storage/azurite:3.37.0 \
  azurite-blob --blobHost 0.0.0.0 --skipApiVersionCheck
```

The MinIO version matters. A MinIO build from September 2025
(`RELEASE.2025-09-06T17-38-46Z`) answers a create-only upload of a new object with "The
specified key does not exist" (it broke the create-only upload of the shared history that
barca 0.25 and earlier made). Use the pinned image.

## Shell integration tests

```bash
bash tests/integration/test_cli.sh
```

| Script | What it covers |
|---|---|
| `test_cli.sh` | Commands, flags and output |
| `test_cache.sh`, `test_cache_gaps.sh`, `test_cache_fuzz.sh` | Cache hits and misses after code and input changes |
| `test_env.sh` | `--env` separation |
| `test_remote_state.sh` | A store as backup and read-through cache: uploads, the metadata backup, fetches without a local copy, `barca state restore` |
| `test_partitions.sh` | Partitioned assets |
| `test_run_refresh.sh` | `barca run` with `--refresh` |
| `test_benchmark_examples.sh` | The benchmark pipelines run as smoke tests; expects the binary at `.venv/bin/barca` |
| `test_reverse_proxy.sh` | `barca serve` behind nginx: path prefix, UI, live events |

`python/tests/test_ci_coverage.py` fails if a script in `tests/integration/` is not run by the
CI workflow.

## Simulation

`tests/simulation/sim.py` compresses long use of a real deployment: a writer `barca serve` with a
scheduled task beside a `--read-only` server on the same `.barca`. A seeded generator edits the
source, returns to old versions, deletes artifacts, runs `barca gc` and a local `barca get`
alongside the server, and sends SIGKILL or SIGTERM to the server, SIGKILL to a worker. After each
operation it checks that results match the source, both servers are healthy with no panic, and,
periodically, that the database is intact and the newest runs and each node's latest success still
exist. Once traffic ends it checks that the database, its WAL and `.barca` settle within their
limits while the scheduler keeps running.

```bash
tests/simulation/sim.py --seed 7 --seconds 120
tests/simulation/sim.py --seeds 20 --seconds 60
tests/simulation/sim.py --seed 7 --seconds 300 --limit-mb 13000 --seed-db-mb 40000 --keep /data/sim
```

`--seed-db-mb` starts from a database in the old format, for an upgrade under traffic, and
`--from-db` from a copy of a real one. Keep a large one off `/tmp`. `--without gc,kill_server`
leaves operations out (without `gc`, only automatic trimming shrinks the database), and
`--tick-sleep 3` keeps a run alive nearly all the time. A failure keeps its directory (`trace.jsonl`, the logs) and prints the seed. **The
seed fixes the sequence of operations, not the timing**, so a kill can land elsewhere on replay,
and a failure that depends on timing may need several tries. It is not deterministic simulation:
it explores real timing and does not control the scheduler, the clock or the disk.

`--sabotage` breaks what an invariant watches. `python/tests/test_simulation.py` uses it to prove
each one can fail, and runs a short seed, in CI's full tier. `.github/examples/simulation.yml`
runs the long version weekly, with `--strict-reads`: a read the `--read-only` server lost to the
writer fails it. Those reads (`database disk image is malformed`) came from the server's own check
of the database file's header, which closed a descriptor of the file and so dropped the process's
SQLite locks on it; `looking_at_the_database_file_keeps_this_process_s_locks` in
`crates/barca-db/src/db/snapshots.rs` covers it. `BARCA_TEST_CACHE_CHECKPOINT_SECS` shortens the server's
one-minute size check for it; long waits (`STUCK_FOR`, `SURVEY_EVERY`) are skipped by rewriting
`gc-state.json`.

## Web UI

```bash
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui typecheck
pnpm --dir ui lint
pnpm --dir ui test       # vitest
pnpm --dir ui build
pnpm --dir ui gen:types  # regenerate ui/src/lib/generated from Rust; CI fails on a difference
pnpm --dir ui test:e2e   # Playwright: ui/e2e, then ui/e2e-auth
```

The Playwright suites start `barca serve` on their fixture project and the Vite dev server in
front of it. They run `target-local/debug/barca` or `target/debug/barca`, or the binary in
`BARCA_BIN` (CI sets it to the wheel's). Install the browser once with
`pnpm --dir ui exec playwright install chromium`.

In CI the Playwright step does not gate the full tier yet (`continue-on-error`). Seven specs in
`ui/e2e` fail on `main`, listed in [#807](https://github.com/barca-orc/barca/issues/807), and
`ui/e2e-auth` runs only after `ui/e2e` passes. The step's result and the uploaded traces
(`playwright-results`) still show every failure. It gates again when #807 closes.

## Manual, help and contract tests

These keep the documentation and the command line in agreement.

- `cargo test -p barca` parses every `barca ...` line in the `--help` examples and in the
  manual topics against the real argument parser, and requires help text on every flag and
  examples on every documented command.
- `cargo test -p barca contract::` compares each command's `--help` with
  `crates/barca-cli/snapshots/help/`, checks the generated tables in
  `crates/barca-cli/docs/contract.md`, and checks that each site reference page generated from a
  manual topic (`SITE_PAGES` in `crates/barca-cli/src/contract.rs`: `reference/cli-contract`,
  `sql`, `remote-storage`, ...) is what its topic produces and is in the sidebar.
- `python/tests/test_cli_contract.py` compares the JSON output schemas and the error envelope
  with `python/tests/snapshots/cli_contract/`.
- `python/tests/test_docs_examples.py` runs the example pipelines in the manual and checks
  what the text says about them.
- `cargo test -p barca-compile decorator_args` compares the arguments each decorator and helper
  accepts (`SIGNATURES` in `crates/barca-compile/src/decorator_args.rs`) with the signatures in
  `python/barca/__init__.py` and with the "Accepted arguments" tables in `barca docs assets`
  and on the decorators page. Adding an argument means changing all four.

When one fails after a deliberate change, run `scripts/update-cli-snapshots.sh` and review the
diff. The script builds barca, so it takes as long as a release build.

The other site pages are written by hand, and no test compares them with the manual.

## What CI runs

`.depot/examples/ci.yml` runs on every pull request into `main` and on every merge group in
the merge queue. It does not run on push to `main`: `main` only moves to a merge group's
commit, which the queue has just tested. The required checks are `CI / test` and
`CI / backends`. Each is a job that passes when the jobs it needs succeeded.

UI-only diffs (`ui/**`, plus optional `CHANGELOG.md`) run the UI tier on both pull requests and merge groups: repository checks, one wheel build for the real-server browser tests, and UI typecheck, lint, unit and Playwright tests. Clippy, Rust tests, Python suites, proxy tests and performance tests are skipped. Mixed changes retain the tiers below. Releases and `ci-full` override UI scoping; missing diff history retains the broader gates.

Site-only diffs (`site/**`, plus optional `CHANGELOG.md` and `.github/workflows/site.yml`) build no wheel and run no clippy, Python suites or performance tests. On a pull request (the `site` tier) they run the repository checks, which include building the site; in a merge group (`site-merge`) they also run the Rust tests, which read site pages (the sidebar, the pages generated from manual topics, the decorator-argument table and the repository-wide unused-input sweep). A diff that touches `site/src/content/docs/recipes/ordering-only-deps.md`, which a Python test executes, runs the normal tiers. Releases and `ci-full` override site scoping too.

Other changes run in three tiers. A pull request runs the fast tier, so it can enter the queue about a
minute after a push. A merge group runs the merge tier, which gates the merge, in about two
minutes. A release runs the full tier twice: as the `release/v*` pull request's own run, and
again in that pull request's merge group (recognized by its head commit, `chore(release): ...`).
A pull request labelled `ci-full` runs the full tier too, on its next push.

| Job | Check | Fast (pull request) | Merge (merge group) | Full (release, `ci-full`) |
| --- | --- | --- | --- | --- |
| **checks**: the CHANGELOG check, the specification checks, the prek hooks of `prek.toml` (`uvx prek run --all-files`: ruff and ruff format, ty on `python/barca/`, `cargo fmt --check`, the version sync, `uv lock --check`, whitespace and YAML/TOML checks) and the TLA+ models (the jar is cached, its SHA-256 checked each run) | test | yes | yes | yes |
| **clippy**: `cargo clippy --workspace --all-targets -- -D warnings` | test | yes | yes | yes |
| **changes**: the tier and its optional parts; every test file is in exactly one shard (`scripts/ci-shards.py --check`) and every file in `scripts/fast-tests.txt` exists | backends | yes | yes | yes |
| **rust-tests**: `cargo nextest run --workspace`, each test in its own process | test | | yes | yes |
| **rust-tests**, generated types: `ui/scripts/regen-types.sh` (what `pnpm gen:types` runs), then `git diff --exit-code` on `ui/src/lib/generated`, so the UI's wire types cannot drift from Rust | test | | yes | yes |
| **build**: the wheel, once, with the `fast` Cargo profile (no whole-program LTO, incremental); `ui/dist` is restored from a cache keyed on `ui/`, or built | both | | yes | yes |
| **python (fast)**: the quick subset in `scripts/fast-tests.txt`, about 20 s of tests | backends | | yes | |
| **perf**: the performance budgets, `scripts/perf-gate.py` against that wheel (see below) | test | | yes | yes |
| **integration**: the shell integration tests and the benchmark examples, against that wheel | test | | | yes |
| **ui**: the web UI's typecheck, lint and unit tests, then the Playwright suites in `ui/e2e` and `ui/e2e-auth` against that wheel (not gating yet, see below) | backends | | | yes |
| **proxy**: `test_reverse_proxy.sh` and `test_server_mode.sh` | backends | | | yes |
| **python (1/3 .. 3/3, duckdb 1.x)**: the whole Python suite in shards, with the emulators, the duckdb 1.x rerun and the minute checkpoint test | backends | | | yes |
| **durations**: merges the shards' JUnit reports into `scripts/ci-test-durations.json` and uploads it as the `ci-test-durations` artifact (not gating; see below) | | | | yes |

The trade: a merge to `main` is checked by the Rust tests and the quick Python subset only, so
a failure the full tier finds on a release pull request is in one of the pull requests merged
since the previous release. Fix it there (or revert) and push to the release branch. Release
wheels are built by the release workflow with `--release`, not by CI.

No crate has a runnable doctest, so `cargo test --doc` does not run.

**perf** runs `scripts/perf-gate.py` on a 4-CPU sandbox against the wheel. It checks the
budgets in `specs/behavior/performance.md` ("Gates"): the one-asset pipeline of
`benchmarks/trivial` (`barca get --refresh-all`, 15 runs after three warm-ups) for median wall
time, median coordinator CPU and peak coordinator memory, and
`benchmarks/incremental_dag/bench.py` at 5,000 keys for the coordinator's memory and CPU on
warm one-key and unchanged runs, with that harness's own work-count assertions. A check over
budget is measured once more from scratch and fails the job only if it fails again. Run it
locally with `BARCA_BIN=.venv/bin/barca python3 scripts/perf-gate.py`; on a loaded machine
the wall-time check can fail without a regression.

**python** installs the wheel with the `test` extra (with uv) and runs the suite split by file
into shards (`PYTHON_SHARDS` in the workflow), each on an 8-CPU sandbox with one pytest worker
per CPU (`-n "$(nproc)" --dist loadscope`). Tests are grouped by class or module to reuse
fixtures; the slowest 30 tests of each shard are printed in the log, and failures and each
file's duration are written to the check's summary. API tests use a separate temporary
working directory per test, keeping `.barca` databases and artifacts isolated from other
workers.

Three parts run in the full tier only (`scripts/ci-changes.py --all` turns them on; the
script's `--base` mode, which picks parts from the changed paths, is kept for local use):

- **Object-store emulators.** Each shard starts MinIO, fake-gcs-server and Azurite and sets the
  three `BARCA_TEST_*` endpoints, so the backend conformance and transfer fault tests run.
  Otherwise those tests skip.
- **duckdb 1.x.** An extra `python (duckdb 1.x)` job runs every test file that mentions duckdb
  again with the newest duckdb 1.x.
- **The minute checkpoint.** `python/tests/test_minute_checkpoints.py` waits about two minutes
  on the real one-minute metadata backup timer. Outside the full tier the shards set
  `BARCA_TEST_MINUTE_CHECKPOINT=0` and it skips. Locally, with the variable unset, it runs. The
  checkpoint's logic is also covered in seconds by a Rust unit test in
  `crates/barca-core/src/persist.rs` on a short interval.

`scripts/ci-test-durations.json` holds each test file's duration, with and without the
emulators, and the shard split balances on the map that matches the run (counting the minute
checkpoint test only when it runs). A shard's estimated length is its tests spread over its
workers (counted as six of the eight, as measured), but no less than its longest file, which
one worker runs. A file the map does not list is weighted by its size.

The file is refreshed by the release process, not by hand. Each full-tier shard uploads its
JUnit report (`junit-<shard>`), and the `durations` job merges them into the committed file
with `scripts/ci-shards.py --update-durations <report> --emulators` (the full tier runs with
the emulators, so it refreshes the `with_emulators` map), checks the split and uploads the
result as the `ci-test-durations` artifact, kept 90 days. `scripts/release.sh` downloads the
newest one (`gh api`), merges it with `scripts/ci-shards.py --merge-durations` (entries for
files that no longer exist are dropped) and commits it with the version bump, so each release
carries the previous release's measurements; without an artifact, or offline, it warns and
keeps the committed file (`--durations <file>` merges a given copy, `--no-durations` skips
it). The job is not gating: a failed shard still leaves a report, and nothing here can block a
release. To refresh a map by hand, run the suite with `--junitxml` and pass the report to
`scripts/ci-shards.py --update-durations` (add `--emulators` for a run with them).

Cargo builds in the runner's local `target/` directory, incrementally (`CARGO_INCREMENTAL=1`).
Each Rust job (clippy, rust-tests, build) restores that directory before the build from a
cache entry keyed on the job, the rustc version, the `Cargo.lock` hash and the branch (a merge
group starts from `main`'s latest), and saves a new entry after it under a key unique to the
run (the composite actions `.depot/actions/rust-cache-restore` and `rust-cache-save`; the
jobs that test against the wheel set up Python, uv and the wheel with
`.depot/actions/setup-wheel`). A merge group saves under `main`, since its commit becomes `main`; concurrent merge groups
write separate entries, and the next restore takes the newest.
Building directly on Depot's shared cache disk caused truncated object files and invalid Rust
metadata, including after target directories were separated by PR.

## Reference for test authors

**Materialization status.** A row in the `materializations` table is `success` or `failed`.
Only `success` rows are cache hits. A run under `barca serve` has its own status: `pending`,
`running`, `complete`, `failed` or `cancelled`. A run in `barca history` is `running`,
`success`, `failed`, `cancelled` or `interrupted`.

**Run hash.** `python/tests/test_run_hash_golden.py` pins run hashes from a released version,
so a change that would recompute unchanged projects on upgrade fails a test. What the hash
covers is in `barca docs cache`.
