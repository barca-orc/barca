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
  barca-server/         ← `barca serve`: axum HTTP API, cron scheduler, file watcher, embedded UI
ui/                     ← The web UI (TypeScript, Vite); its build output is compiled into barca-server
specs/                  ← Behavioral/interface contracts, design models and unapproved proposals (README.md is the index)
docs/engineering/       ← Non-normative architecture plans, rationale and discussion handoff
DIRECTION.md            ← What barca is, standing non-goals, what is being worked on now
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
   - Persists plan + results to local SQLite-compatible libSQL database (`.barca/metadata.db`)
   - Maintains a pool of stateless Python workers and a global ready queue
   - Rust assigns one task at a time to idle workers via Unix domain socket (UDS)

2. **Python worker** (`python -m barca._worker`):
   - Stateless: connects to the coordinator's UDS and receives one task at a time
   - Imports user modules via `importlib.util.spec_from_file_location`
   - Executes the task, serializes results to artifact files (json/pickle/parquet)
   - Reports results back to Rust via the Unix domain socket protocol
   - For `parallel()`: coordinator freezes the caller (SIGSTOP), spawns a replacement,
     adds children to the ready queue; on completion resumes the caller (SIGCONT) and stops
     the next worker that has nothing leased
   - No DB access — Rust owns all persistence
   - Artifacts are always local; with a remote store, `python -m barca._transfer` (one per run,
     driven by `transfer.rs` over its own UDS) uploads them in the background and fetches
     cache hits this machine lacks (evicted locally, or recorded by a server's ledger) before
     they are read. The store is this machine's backup and read-through cache; sharing means a
     server (`barca docs server-mode`)

3. **Python stubs** (`from barca import asset, ...`):
   - Pure no-ops — decorators return the function unchanged
   - Exist for IDE autocomplete, type checking, and so user code runs standalone

### Dependencies

- **Rust**: ruff_python_parser (AST), petgraph (DAG), libsql (local DB), serde/serde_json, sha2, toml (barca.toml config)
- **Python**: no runtime dependencies (stdlib only; pyarrow optional for parquet; fsspec + adlfs/s3fs/gcsfs optional for remote artifact storage via the `azure`/`s3`/`gcs`/`remote` extras)
- **Build**: maturin (packages Rust binary + Python stubs into one wheel)

## Commands

```bash
# Build (development)
cargo build --release
maturin develop --uv --release --extras test  # active .venv; builds CLI + test dependencies

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

[DIRECTION.md](DIRECTION.md) states what barca is, the standing non-goals and the current
priorities; the pinned Direction issue on GitHub mirrors it. Check a new feature against it before
filing an issue.

1. **Invisible** — the orchestrator should add zero perceptible overhead
2. **Static analysis** — extract source without imports. The resolver evaluates nonliteral
   partition key expressions and computed batch groups in an importable project subprocess
   (RFC-0013); literal keys and groups are read statically. Evaluate `batches_by` only for
   keys that need to run. Keep planning imports within these documented resolver operations.
3. **Rust for planning, Python for execution** — each does what it's best at
4. **Single install** — `uv add barca` gives users everything
5. **SQLite for persistence** — Rust owns the DB through local-only libSQL; Python has no DB access
6. **Artifact-based data passing** — serialized files (json/pickle/parquet) between worker batches
7. **Run-hash-addressed artifacts** — `{artifacts}/{node}/{run_hash}{ext}`; with a store, the metadata DB is backed up to it (`barca state restore` puts a copy back) (see site/src/content/docs/reference/config.mdx and site/src/content/docs/reference/remote-storage.md, published at https://barca.sh/reference/config/ and https://barca.sh/reference/remote-storage/)

## CLI and manual are part of every feature

The CLI must be usable by humans **and** AI agents without leaving the terminal. Any change to a
command, flag, decorator, output format, or caching behavior ships with all of:

1. **`--help` examples** — the subcommand's `after_help` in `crates/barca-cli/src/args.rs`
   (runnable `barca ...` example lines; a new flag gets an example).
2. **The manual** — the matching topic in `crates/barca-cli/docs/*.md`, embedded by
   `crates/barca-cli/src/docs.rs`. New topics go in `TOPICS` and must be linked from
   `overview.md` (or `examples.md`). The files live inside the crate so they ship in the sdist.
3. **Machine-readable output** — results as JSON on stdout (inspection commands take `--json`),
   progress and errors on stderr, exit codes 0 ok / 1 a step failed / 2 usage error / 3 barca infra failure / 130 cancelled
   (see `barca docs contract`).
4. **Site docs** (`site/src/content/docs/`) and the README CLI table.
5. **The CLI contract** — `crates/barca-cli/docs/contract.md` (`barca docs contract`) and its
   snapshots. A change to the surface (a command, flag, environment variable, exit code, JSON
   output key or type, the stderr error envelope, an `--agent` line) updates, in the same PR, the
   snapshots (`scripts/update-cli-snapshots.sh`: `--help` in `crates/barca-cli/snapshots/help/`,
   JSON schemas in `python/tests/snapshots/cli_contract/`, and the generated tables in
   `contract.md`) and the hand-written parts of `contract.md` (stable vs experimental; a new
   experimental flag or command also goes in `EXPERIMENTAL` in `crates/barca-cli/src/contract.rs`).
   Review the snapshot diff: it is the contract change.
   - **Pre-1.0**: breaking changes are allowed, with a minor bump and a "Breaking" line in the
     release notes naming the change and its replacement.
   - **From 1.0**: changes are additive only (new commands, flags, keys, enum values). A
     deprecated flag keeps working for at least one minor release and prints a warning on stderr
     naming its replacement.

CI guards these: `cargo test -p barca` parses every `barca ...` line in `--help` examples and
manual topics against the real CLI, and requires help text on every flag and examples on every
documented command; `python/tests/test_docs_examples.py` executes the manual's example pipelines
and asserts what the text claims. `cargo test -p barca` (`crates/barca-cli/src/contract.rs`) and
`python/tests/test_cli_contract.py` fail on any surface change that the snapshots and
`contract.md` do not reflect. Document behavior you have run, and state known limitations
plainly (e.g. `--partition` skips a `collect()` fan-in over the selected keys instead of
computing it over some keys) rather than describing intended behavior.

## Output and decorator arguments

`barca-core/src/decorator_args.rs` is the shared list of accepted decorator/helper arguments.
Its tests keep Python stubs and documentation tables in sync. Use `outln!` / `errln!` for
lines, or `term::stdout_str` / `stderr_str` for rendered chunks; direct print macros are denied
by Clippy. Use `term::child_output()` for uncaptured child streams and `term::exit` to drain
child output before exiting.

## Git workflow

Trunk-based: `main` is the integration branch — always green, but not necessarily
released. Merging to main never publishes anything; only pushing a `v*` tag does.

- **Always use worktrees** for local development work — see [AGENTS.md](AGENTS.md):
  `scripts/new-worktree.sh <slug> [type]` creates `.worktrees/<slug>` on `<type>/<slug>` off
  origin/main, builds into the tree's own `target-local/` (never the trunk `target/` shared
  between trees) with sccache sharing compiled dependencies when installed, and sets up
  `.venv`; `scripts/remove-worktree.sh <slug>` reaps the tree's processes, deletes
  `target-local/` and removes the tree and its branch when the task is done (orphan trees eat
  disk)
- **Topic branches**: one per issue, branched off main, PRed straight into main
- **Merging**: the `protect-main` ruleset allows **squash merges only**, requires `CI / test`
  and `CI / backends` on the PR's head, and requires the branch to be up to date with main.
  Merge one PR at a time: each merge makes the others behind, and updating them all at once
  restarts every CI run. To ship several PRs as one release, combine them on one branch, run
  CI once, and squash-merge it with a body that lists each PR and the issues it closes.
- **Release**: `scripts/release.sh X.Y.Z`, from the trunk on an up-to-date main
  (`--dry-run` does everything locally and stops before the push). It refuses a version not
  greater than the latest tag, an empty `## Pending`, or (pre-1.0) a patch bump when Pending
  has `### Breaking`; cuts `release/vX.Y.Z` off main in `.worktrees/release-vX.Y.Z` with only
  the version bump (crates/*/Cargo.toml, the barca entries in Cargo.lock and uv.lock,
  pyproject.toml, `barca.__version__`) and `## Pending` renamed to `## X.Y.Z (YYYY-MM-DD)`
  under a new empty `## Pending`; commits `chore(release): vX.Y.Z`, pushes, opens the PR and
  enables squash auto-merge with `--delete-branch`. Tidy the Pending notes on main before
  running it. When the PR merges, `.github/workflows/release-tag.yml` tags the merge commit
  `vX.Y.Z` (checking it matches pyproject.toml) and dispatches the release workflow on the
  tag (wheels, GitHub Release, PyPI); a tag pushed with the workflow token does not trigger
  `on: push: tags`, so it starts `release.yml` by `workflow_dispatch` on the tag ref, which
  runs it exactly as a pushed tag would. If that workflow fails, tag by hand:
  `git tag -a vX.Y.Z -m vX.Y.Z <merge sha> && git push origin vX.Y.Z` (the tag-push trigger
  still works; the tagging workflow skips a tag that already exists). The `release/` prefix
  keeps branch and tag names from colliding, so `git push origin v<x.y.z>` and
  `git log v<x.y.z>..` are unambiguous. The same workflow then deploys barca.sh from the tag
  (`deploy-site`, using the `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` secrets);
  nothing waits on it, so a failed deploy never blocks a release. PyPI's simple index is
  cached by its CDN for up to 10 minutes, so a new version can be uploaded but not yet
  installable; the workflow's post-publish check waits for it.

## Boundary contracts

[specs/README.md](specs/README.md) inventories native boundary specifications and
conformance checks. A public boundary change must update its specification, executable
conformance and explanatory documentation in the same PR. Approved but unimplemented behavior stays explicitly labeled as intent; unapproved
alternatives live only in `specs/proposals/`. Behavioral owners follow the section structure
in the index; plans and rationale live in `docs/engineering/`. Do not describe RFC routes or
schemas as shipped. Existing
CLI contract/snapshot ownership remains in place.

## Commit messages

Use [conventional commits](https://www.conventionalcommits.org/): `type: description` or `type(scope): description`.

Types: `feat`, `fix`, `refactor`, `polish`, `doc`, `test`, `perf`, `remove`, `chore`.

Release notes are not generated from commits: `CHANGELOG.md` is their source. A change adds one
bullet under `## Pending` (`### Breaking`, `### Features`, `### Bug Fixes`, `### Performance`,
`### Removed` or `### Documentation`), and the release workflow publishes the version's
`## <version> (<date>)` section as the GitHub release body. CI (`scripts/check-changelog.py`)
fails a change that edits a released section (one whose `v<version>` tag exists), which catches a
stale branch's bullets merging into it; to fix released notes on purpose, add a
`Changelog-Fix: <reason>` line to a commit message.

## Versioning

Pre-1.0 — everything is unstable prototype work. Stay on `0.x.y` until the API and CLI
surface are genuinely stable and battle-tested.

- **Minor bump** (`0.x.0`): new capability area, new public surface (e.g. adding the HTTP API), or breaking changes
- **Patch bump** (`0.x.y`): iteration within a capability — bug fixes, polish, performance, refactors

Version must be consistent across `Cargo.toml` (workspace root + all crates) and `pyproject.toml`.
Bump all of them together in a single commit at the start of a release branch.
