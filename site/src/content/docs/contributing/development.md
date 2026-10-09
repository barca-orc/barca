---
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
uv venv
source .venv/bin/activate
uv pip install maturin
maturin develop --uv --release --extras test   # builds the binary, installs it and the Python package into .venv
cargo test                                # Rust tests
```

`maturin develop` is driven by `[tool.maturin]` in `pyproject.toml`: it builds the `barca-cli`
crate and installs the binary together with `python/barca/`. The `test` extra adds pytest,
pandas, pyarrow, polars, duckdb and the storage clients the Python tests use. Run it again
after changing Rust code.

`cargo build --release` builds only the binary, at `target/release/barca`.

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
├── scripts/                  # check-version-sync.sh, update-cli-snapshots.sh
├── site/                     # this documentation site (Astro Starlight)
├── SKILL.md                  # the agent skill, also `barca docs skill`
├── pyproject.toml            # maturin build configuration and extras
├── prek.toml                 # pre-commit hooks
└── cliff.toml                # git-cliff configuration for release notes
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

It regenerates `crates/barca-cli/snapshots/help/`, the tables in
`crates/barca-cli/docs/contract.md`, `python/tests/snapshots/cli_contract/` and
`site/src/content/docs/reference/cli-contract.md`. Review the diff: it is the contract change.
See [Testing](/contributing/testing/) for the tests that enforce this.

## The documentation site

```bash
cd site
npm install
npm run dev      # local preview
npm run build
```

`reference/cli-contract.md` is generated; edit `crates/barca-cli/docs/contract.md` instead.
`reference/sql.md`, `reference/telemetry.md` and `reference/discovery.md` are copies of the
manual topics of the same name with site front matter, kept in step by hand: a change to one
must be made in both.

## Git workflow

- `main` is the integration branch. One topic branch per issue, opened as a pull request into
  `main`.
- Commit messages use conventional commits (`feat:`, `fix:`, `refactor:`, `polish:`, `doc:`,
  `test:`, `perf:`, `remove:`); release notes are grouped by these types.
- Releases are described in [Releases](/contributing/releases/).
