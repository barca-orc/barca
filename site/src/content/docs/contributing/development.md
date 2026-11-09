---
pagefind: false
title: Development Setup
description: Build barca from source, run it, and find your way around the repository.
---

Barca is a Rust workspace with a Python package. You need a Rust toolchain (edition 2024),
Python 3.12 or later, and [uv](https://docs.astral.sh/uv/) or pip. The web UI needs Node and
pnpm only if you change it.

## Build

```bash
git clone https://github.com/barca-orc/barca.git
cd barca
scripts/new-worktree.sh my-change            # a worktree under .worktrees/ with its own .venv and target-local/
cd .worktrees/my-change
source .venv/bin/activate
maturin develop --uv --profile fast --extras test   # after an edit: seconds; the first build about 40 s
cargo test                                          # Rust tests
scripts/check-fast.sh                               # CI's always-on gates, before pushing
```

Work happens in a worktree, never in the trunk checkout: `scripts/new-worktree.sh <slug> [type]`
creates `.worktrees/<slug>` on `<type>/<slug>` off `origin/main`, builds into the tree's own
`target-local/` (sharing compiled dependencies through sccache when installed) and sets up
`.venv`; `scripts/remove-worktree.sh <slug>` reaps the tree's processes and removes it when the
work is merged. The rules, with measured build times, are in
[AGENTS.md](https://github.com/barca-orc/barca/blob/main/AGENTS.md); this page does not repeat them.

`maturin develop` is driven by `[tool.maturin]` in `pyproject.toml`: it builds the `barca-cli`
crate and installs the binary together with `python/barca/`. The `test` extra adds pytest,
pandas, pyarrow, polars, duckdb and the storage clients the Python tests use. Run it again
after changing Rust code. The `fast` profile keeps the release `opt-level` and drops fat LTO and
the single codegen unit, so an edit rebuilds in seconds; `--release` is what wheels ship, for
measuring performance or reproducing a wheel.

`cargo build --release` builds only the binary, at `target-local/release/barca` in a worktree.

`scripts/check-fast.sh` runs what CI's always-on gates run (changelog, specs, the prek hooks
including the pinned ruff and ty, clippy with `-D warnings`, the Rust tests, a rebuild of the
venv's barca when Rust changed, and the Python tests picked from the diff) and prints a pass/fail
summary; `--python-all` runs the whole Python suite.

## Run an example

```bash
cd examples/basic_app
../../.venv/bin/barca list
../../.venv/bin/barca get
../../.venv/bin/barca plan
```

`examples/basic_app` has a `barca.toml`, so barca treats it as the project root and finds
`example_project/assets.py` without file arguments.

## Repository layout

```
barca/
├── Cargo.toml                # Rust workspace root
├── crates/
│   ├── barca-core/           # parser, DAG, planning, hashing, coordinator, config
│   ├── barca-cli/            # the `barca` command
│   │   ├── docs/             # the manual (`barca docs`), compiled into the binary
│   │   └── snapshots/help/   # `--help` snapshots checked by the contract test
│   └── barca-server/         # HTTP API and cron scheduler for `barca serve`
├── python/
│   ├── barca/                # decorators, worker, artifact I/O, transfer helper, Python API
│   └── tests/                # pytest suite
├── ui/                       # web UI (React, Vite), embedded in the binary at build time
├── tests/integration/        # shell tests that run the installed CLI
├── examples/                 # example projects
├── benchmarks/               # benchmarks against other orchestrators
├── scripts/                  # check-version-sync.sh, update-cli-snapshots.sh, changelog-section.py
├── site/                     # this documentation site (Astro Starlight)
├── SKILL.md                  # the agent skill, also `barca docs skill`
├── pyproject.toml            # maturin build configuration and extras
└── prek.toml                 # pre-commit hooks
```

[Architecture](/architecture/#layout) lists the modules inside each crate.

## Pre-commit hooks

`prek.toml` configures [prek](https://github.com/j178/prek): whitespace and TOML/YAML checks,
`ruff` and `ruff-format`, `cargo fmt --all --check`, `ty check python/barca/`,
`scripts/check-version-sync.sh` and `uv lock --check`. A pre-push hook runs
`benchmarks/perf/profile.py`.

```bash
prek install
prek run --all-files
```

CI additionally runs `cargo clippy --workspace --all-targets -- -D warnings`.

## Changing the command line or the manual

A change to a command, flag, output key or exit code also changes the `--help` examples, the
manual topic in `crates/barca-cli/docs/`, this site, and the CLI contract snapshots. After such
a change run:

```bash
scripts/update-cli-snapshots.sh
```

It regenerates `crates/barca-cli/snapshots/help/` and `snapshots/examples/`, the tables in
`crates/barca-cli/docs/contract.md` and the README's command table,
`python/tests/snapshots/cli_contract/` and the site pages generated from manual topics
(`SITE_PAGES` in `crates/barca-cli/src/contract.rs`). Review the diff: it is the contract change.
See [Testing](/contributing/testing/) for the tests that enforce this.

## The documentation site

```bash
cd site
npm install
npm run dev      # local preview
npm run build
```

The reference pages that are manual topics (`reference/cli-contract.md`, `agents.md`,
`assets.md`, `cache.md`, `discovery.md`, `doctor.md`, `duckdb.md`, `groups.md`, `logs.md`,
`partitions.md`, `remote-storage.md`, `server-mode.md`, `sinks.md`, `sql.md`, `status.md`,
`tasks.md`, `telemetry.md`, `types.md`; the list is `SITE_PAGES` in
`crates/barca-cli/src/contract.rs`) are generated from `crates/barca-cli/docs/`, each with an
edit link back to its topic.
Edit the manual topic, then regenerate them with
`BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract::` (`scripts/update-cli-snapshots.sh`
does it too). Every other page is written by hand.

## Git workflow

- `main` is the integration branch, always green. One topic branch per issue, in its own
  worktree, opened as a pull request into `main`.
- Pull requests land through the merge queue, squash-merged: `gh pr merge <n> --squash --auto`
  (or "Merge when ready") on a green PR. Do not rebase or update a branch by hand to keep up with
  main; the queue tests each PR against the main it will land on ([Testing](/contributing/testing/)
  describes the tiers).
- Commit messages use conventional commits (`feat:`, `fix:`, `refactor:`, `polish:`, `doc:`,
  `test:`, `perf:`, `remove:`, `chore:`). Release notes are not generated from them: a change adds
  a bullet under `## Pending` in `CHANGELOG.md`.
- Releases are described in [Releases](/contributing/releases/).
