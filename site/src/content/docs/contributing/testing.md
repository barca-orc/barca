---
title: Testing
description: Test suites, fixture patterns, and state/output reference.
---

## Overview

Barca's test suite spans three layers:

- **Rust unit + integration tests** — in `crates/barca-core/tests/` and
  `crates/barca-server/tests/`, plus inline `#[test]` modules throughout `crates/*/src/`.
  Run with `cargo test`.
- **Python tests** — in `python/tests/`, covering the worker, runtime, and storage
  backends. Run with `uv run pytest python/tests -q`.
- **Shell integration tests** — in `tests/integration/`, run against the built wheel in CI.

```bash
cargo test
uv run pytest python/tests -q
```

## Test suites

### Rust (`crates/*/tests/`, `cargo test`)

| Suite | File | What it covers |
|-------|------|---------------|
| Grammar spec | `crates/barca-core/tests/grammar_spec.rs` | Parsing edge cases for decorator syntax |
| Socket stress | `crates/barca-core/tests/socket_stress.rs` | UDS coordination protocol under load |
| Server API | `crates/barca-server/tests/api.rs` | HTTP endpoints exposed by `barca serve` |

Most Rust coverage lives as `#[test]` functions inline in each module (parser, DAG,
hashing, cache lookup, coordinator dispatch, scheduler, config, etc.) rather than in
separate test files.

### Python (`python/tests/`, `pytest`)

| File | What it covers |
|------|---------------|
| `test_worker_artifacts.py` | Worker-side artifact writing |
| `test_artifacts.py` | Serialization: json/pickle/parquet format detection + I/O |
| `test_state_backends.py` | Shared remote state conformance (pull/push/conflict) |
| `test_storage.py` | Local + remote (fsspec) artifact storage dispatch |
| `test_state.py` | Local state handling |
| `test_runtime.py` | Worker runtime / UDS client behavior |
| `test_parallel.py` | `parallel()` / `parallel_map()` |
| `test_timing.py` | Timing / elapsed-time reporting |
| `test_client.py` | `barca.Client` (HTTP API client) |
| `test_history.py` | `barca.history()` |
| `test_api.py` | `barca.get()` / `barca.plan()` / `barca.run()` / `barca.stats()` |
| `test_errors.py` | `BarcaError` surfacing |
| `test_reliability.py` | Retry / timeout behavior |
| `test_cross_file.py` | Cross-file dependency resolution |
| `test_helper_tracking_194.py` | Helper edits re-run the step: classes, aliases, in-function imports, modules as values, and which file a module name means in each project layout |
| `test_run_hash_golden.py` | Run hashes pinned from a released version: an upgrade must not recompute unchanged projects |
| `test_adaptive_executor.py` | Worker pool sizing |
| `test_artifact_lru.py` | The worker's in-memory artifact cache: isolation of cached values per type, size limits |
| `test_retries.py` | `retries=` / `retry_backoff=` |

### Shell integration (`tests/integration/`, CI only)

`test_cli.sh`, `test_cache.sh`, `test_cache_gaps.sh`, `test_cache_fuzz.sh`, `test_env.sh`,
`test_remote_state.sh` — exercised in `.depot/workflows/ci.yml` against a `maturin build
--release` wheel. A separate CI job (`backends`) runs the full Python suite, including the
state-backend conformance tests, against local object-store emulators (MinIO, fake-gcs-server,
Azurite) — no cloud credentials required. Tests that need an emulator skip when it is not
running, unless its `BARCA_TEST_*` variable is set (`BARCA_TEST_S3_ENDPOINT`,
`BARCA_TEST_GCS_ENDPOINT`, `BARCA_TEST_AZURITE_HOST`): the `backends` job sets all three, so
there an unreachable emulator fails the test instead (`python/tests/emulators.py`).

Tests that must act while a helper process is in the middle of something (a Ctrl-C during an
upload) do not sleep and hope: `python/tests/hold/` is a `sitecustomize` shim that pauses a
barca helper at a named point until the test releases it (`BARCA_TEST_HOLD=<point>:<dir>`).

## Test patterns

### Fixture projects

Python tests create temporary project directories with decorated functions and a
`barca.toml`, then shell out to the built `barca` binary (via `barca.api`) to exercise
real CLI behavior end-to-end.

## State & output reference

### Materialization status values

Each row in the `materializations` table is `success` or `failed` (a cache lookup filters
on `status = 'success'`; failed rows are never served from cache). Server-mode runs
(`barca serve`) track a separate, richer run status: `pending` → `running` → `complete` |
`failed` | `cancelled`.

### Artifact path

An artifact's path is derived from the step's `run_hash` when one is available. It names the
computation, not the bytes: a refresh, or a recompute of a missing artifact, overwrites the file.

```
{artifact_dir}/{safe_node_id}/{run_hash}{ext}
```

e.g. `.barca/artifacts/pipeline.py--summary/3f9a....json`. Without a `run_hash` (older
coordinators, `parallel()` children, batch mode), artifacts fall back to a legacy
node-id-keyed layout: `{artifact_dir}/{safe_node_id}{ext}`. Under the coordinator
`artifact_dir` is always the local artifact directory — a remote store is synced by the
transfer helper (`barca._transfer`, `crates/barca-core/src/transfer.rs`) — but batch mode
and direct `BARCA_ARTIFACT_URI` use still accept a remote URI.

### Hash identity

`definition_hash` (SHA-256) is derived from: protocol version + function source +
dependency cone source (helper/constant deps) + decorator metadata (freshness,
partitions, inputs). Changing any input produces a new hash.

`run_hash` (SHA-256) is derived from the definition hash + partition key (if any) +
sorted upstream materialization IDs + ad-hoc params. A materialization is a cache hit
when its `run_hash` matches an existing successful row.

### API shapes

See the [Server API](/reference/server-api/) reference.

`AssetSummary` includes `kind` (`"asset"`, `"sensor"`, `"task"`), `freshness`, and
`inputs`.
