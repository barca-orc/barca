---
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
(`maturin develop --release --extras test`, see [Development Setup](/contributing/development/)).

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
| `crates/barca-core/tests/unused_input_repo_sweep.rs` | The unused-input warning against the repository's own examples and docs |
| `crates/barca-server/tests/api.rs` | The HTTP endpoints of `barca serve` |

## Python

```bash
pytest python/tests -q
pytest python/tests/test_sql.py -q        # one file
```

Most tests in `python/tests/` create a temporary project with decorated functions, run the
real `barca` binary on it, and check the output, exit code and files.

## Storage backend tests and emulators

Tests of remote storage and shared history (`test_state_backends.py`, `test_remote_faults.py`,
`test_remote_env_config.py`, `test_remote_inspect.py` and others) run against local emulators
and need no cloud account:

| Store | Emulator | Variable | Default |
|---|---|---|---|
| S3 and R2 | MinIO | `BARCA_TEST_S3_ENDPOINT` | `http://localhost:9100` |
| GCS | fake-gcs-server | `BARCA_TEST_GCS_ENDPOINT` | `http://localhost:9200` |
| Azure | Azurite | `BARCA_TEST_AZURITE_HOST` | `127.0.0.1:9210` |

`BARCA_TEST_S3_KEY` and `BARCA_TEST_S3_SECRET` default to `minioadmin`.

Without the variables set, a test whose emulator is not reachable at the default address is
skipped, so the suite passes on a machine with no emulators. In `test_state_backends.py`, when a
variable is set and its emulator is not reachable, the test fails instead of skipping. CI sets
all three, so a broken emulator cannot make the backend job pass by skipping.

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
specified key does not exist", which makes barca's first upload of the shared history fail with
exit 3. Use the pinned image.

## Shell integration tests

```bash
bash tests/integration/test_cli.sh
```

| Script | What it covers |
|---|---|
| `test_cli.sh` | Commands, flags and output |
| `test_cache.sh`, `test_cache_gaps.sh`, `test_cache_fuzz.sh` | Cache hits and misses after code and input changes |
| `test_env.sh` | `--env` separation |
| `test_remote_state.sh` | Shared history between two project directories |
| `test_partitions.sh` | Partitioned assets |
| `test_run_refresh.sh` | `barca run` with `--refresh` |
| `test_benchmark_examples.sh` | The benchmark pipelines run as smoke tests; expects the binary at `.venv/bin/barca` |
| `test_reverse_proxy.sh` | `barca serve` behind nginx: path prefix, UI, live events |

`python/tests/test_ci_coverage.py` fails if a script in `tests/integration/` is not run by the
CI workflow.

## Web UI

```bash
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui typecheck
pnpm --dir ui lint
pnpm --dir ui test       # vitest
pnpm --dir ui build
```

## Manual, help and contract tests

These keep the documentation and the command line in agreement.

- `cargo test -p barca` parses every `barca ...` line in the `--help` examples and in the
  manual topics against the real argument parser, and requires help text on every flag and
  examples on every documented command.
- `cargo test -p barca contract::` compares each command's `--help` with
  `crates/barca-cli/snapshots/help/`, checks the generated tables in
  `crates/barca-cli/docs/contract.md`, and checks that
  `site/src/content/docs/reference/cli-contract.md` is that topic with site front matter.
- `python/tests/test_cli_contract.py` compares the JSON output schemas and the error envelope
  with `python/tests/snapshots/cli_contract/`.
- `python/tests/test_docs_examples.py` runs the example pipelines in the manual and checks
  what the text says about them.

When one fails after a deliberate change, run `scripts/update-cli-snapshots.sh` and review the
diff. The script builds barca, so it takes as long as a release build.

No test compares the other site pages with the manual. `reference/sql.md`,
`reference/telemetry.md` and `reference/discovery.md` are hand-kept copies of manual topics.

## What CI runs

`.depot/workflows/ci.yml` runs on every pull request into `main`, in two jobs:

- **test**: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test`, then builds
  a wheel with `maturin build --release`, installs it, and runs the shell integration tests.
- **backends**: starts the three emulators, checks and builds the web UI (typecheck, lint,
  test, build), builds a wheel and installs it with the `test` extra, runs the whole Python
  suite with the three `BARCA_TEST_*` endpoints set, then runs `test_reverse_proxy.sh`.

## Reference for test authors

**Materialization status.** A row in the `materializations` table is `success` or `failed`.
Only `success` rows are cache hits. A run under `barca serve` has its own status: `pending`,
`running`, `complete`, `failed` or `cancelled`. A run in `barca history` is `running`,
`success`, `failed`, `cancelled` or `interrupted`.

**Run hash.** `python/tests/test_run_hash_golden.py` pins run hashes from a released version,
so a change that would recompute unchanged projects on upgrade fails a test. What the hash
covers is in `barca docs cache`.
