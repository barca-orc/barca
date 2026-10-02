# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Architecture

Barca is an invisible asset orchestrator — a Rust binary that parses Python source
files, builds a DAG, generates execution plans, and dispatches work to a Python runtime.
Users install a single package (`barca`) that provides both the CLI binary and Python
stubs for decorators.

### Workspace structure

```
Cargo.toml              ← Rust workspace root
crates/
  barca-core/           ← Core library: models, parser, DAG, execution planning, hashing
  barca-cli/            ← CLI binary (the `barca` command)
    docs/               ← The manual: markdown topics embedded in the binary (`barca docs`)
python/barca/
  __init__.py           ← No-op decorator stubs (identity functions)
  _worker.py            ← Batch worker (invoked by Rust via `python -m barca._worker`)
  _artifacts.py         ← Serialization: json, pickle, parquet format detection + I/O
  _transfer.py          ← Artifact transfer helper (local dir ↔ remote store, background)
  __main__.py           ← Entry point for `python -m barca` (delegates to `_worker.main()`)
  py.typed              ← PEP 561 marker
pyproject.toml          ← Maturin build config (binary + Python stubs in one wheel)
```

### How it works

1. **Rust coordinator** (`barca get <file.py>`):
   - Parses Python using ruff's AST (no import, pure static analysis)
   - Builds a petgraph DAG from `@asset`/`@sensor`/`@task` decorators
   - Generates a tiered execution plan (JSON)
   - Persists plan + results to local Turso/libSQL database (`.barca/metadata.db`)
   - Maintains a pool of stateless Python workers and a global ready queue
   - Rust assigns one task at a time to idle workers via Unix domain socket (UDS)

2. **Python worker** (`python -m barca._worker`):
   - Stateless: connects to the coordinator's UDS and receives one task at a time
   - Imports user modules via `importlib.util.spec_from_file_location`
   - Executes the task, serializes results to artifact files (json/pickle/parquet)
   - Reports results back to Rust via the Unix domain socket protocol
   - For `parallel()`: coordinator freezes the caller (SIGSTOP), spawns a temp replacement,
     adds children to the ready queue; on completion kills the temp, resumes the caller (SIGCONT)
   - No DB access — Rust owns all persistence
   - Artifacts are always local; with a remote store, `python -m barca._transfer` (one per run,
     driven by `transfer.rs` over its own UDS) uploads them in the background and fetches
     other machines' cache hits before they are read

3. **Python stubs** (`from barca import asset, ...`):
   - Pure no-ops — decorators return the function unchanged
   - Exist for IDE autocomplete, type checking, and so user code runs standalone

### Dependencies

- **Rust**: ruff_python_parser (AST), petgraph (DAG), turso (DB), serde/serde_json, sha2, toml (barca.toml config)
- **Python**: no runtime dependencies (stdlib only; pyarrow optional for parquet; fsspec + adlfs/s3fs/gcsfs optional for remote artifact storage via the `azure`/`s3`/`gcs`/`remote` extras)
- **Build**: maturin (packages Rust binary + Python stubs into one wheel)

## Commands

```bash
# Build (development)
cargo build --release
maturin develop --release     # installs into .venv

# Run
.venv/bin/barca get <file.py>
.venv/bin/barca plan <file.py>   # emit plan JSON only

# Tests
cargo test

# Benchmarks
benchmarks/trivial/bench.sh 10
benchmarks/chain_100/bench.sh 5   # (coming soon)
```

## Design principles

1. **Invisible** — the orchestrator should add zero perceptible overhead
2. **Static analysis** — never import user code in the planning phase
3. **Rust for planning, Python for execution** — each does what it's best at
4. **Single install** — `uv add barca` gives users everything
5. **Turso for persistence** — Rust owns the DB; Python has no DB access
6. **Artifact-based data passing** — serialized files (json/pickle/parquet) between worker batches
7. **Content-addressed artifacts** — `{artifacts}/{node}/{run_hash}{ext}`; shared remote state pulls/pushes the metadata DB as a blob (see site/src/content/docs/reference/config.mdx and site/src/content/docs/reference/remote-storage.md, published at https://barca.sh/reference/config/ and https://barca.sh/reference/remote-storage/)

## CLI and manual are part of every feature

The CLI must be usable by humans **and** AI agents without leaving the terminal. Any change to a
command, flag, decorator, output format, or caching behavior ships with all of:

1. **`--help` examples** — the subcommand's `after_help` in `crates/barca-cli/src/main.rs`
   (runnable `barca ...` example lines; a new flag gets an example).
2. **The manual** — the matching topic in `crates/barca-cli/docs/*.md`, embedded by
   `crates/barca-cli/src/docs.rs`. New topics go in `TOPICS` and must be linked from
   `overview.md` (or `examples.md`). The files live inside the crate so they ship in the sdist.
3. **Machine-readable output** — results as JSON on stdout (inspection commands take `--json`),
   progress and errors on stderr, exit codes 0 ok / 1 runtime failure / 2 usage error.
4. **Site docs** (`site/src/content/docs/`) and the README CLI table.

CI guards these: `cargo test -p barca` parses every `barca ...` line in `--help` examples and
manual topics against the real CLI, and requires help text on every flag and examples on every
documented command; `python/tests/test_docs_examples.py` executes the manual's example pipelines
and asserts what the text claims. Document behavior you have run, and state known limitations
plainly (e.g. partitioned steps are not cache-checked yet) rather than describing intended
behavior.

## Git workflow

Trunk-based: `main` is the integration branch — always green, but not necessarily
released. Merging to main never publishes anything; only pushing a `v*` tag does.

- **Always use worktrees** for local development work
- **Topic branches**: one per issue, branched off main, PRed straight into main
- **Release**: when ready to ship, cut a short-lived release branch
  `v<major>.<minor>.<patch>` off main (no descriptive suffix) containing only the
  version bump; PR it into main, then tag the merge commit — the tag triggers the
  release workflow (wheels, GitHub Release, PyPI)

## Commit messages

Use [conventional commits](https://www.conventionalcommits.org/): `type: description` or `type(scope): description`.

git-cliff groups commits into changelog sections by type:

| Type       | Changelog group |
|------------|-----------------|
| `feat`     | Features        |
| `fix`      | Bug Fixes       |
| `refactor` | Refactor        |
| `polish`   | Polish          |
| `doc`      | Documentation   |
| `test`     | Testing         |
| `perf`     | Performance     |
| `remove`   | Removed         |

Non-conventional commits appear under "Changes". Co-Authored-By trailers are stripped automatically.

## Versioning

Pre-1.0 — everything is unstable prototype work. Stay on `0.x.y` until the API and CLI
surface are genuinely stable and battle-tested.

- **Minor bump** (`0.x.0`): new capability area, new public surface (e.g. adding the HTTP API), or breaking changes
- **Patch bump** (`0.x.y`): iteration within a capability — bug fixes, polish, performance, refactors

Version must be consistent across `Cargo.toml` (workspace root + all crates) and `pyproject.toml`.
Bump all of them together in a single commit at the start of a release branch.
