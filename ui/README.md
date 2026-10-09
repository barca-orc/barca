# barca web UI

React + TypeScript app served by `barca serve` at `/ui/`. Release wheels compile the
built files (`ui/dist`) into the `barca` binary — see `crates/barca-server/src/ui.rs`.
How it's structured and tested: [ARCHITECTURE.md](./ARCHITECTURE.md).

## Develop

```bash
barca serve --watch                 # in a project: the API on 127.0.0.1:8274
pnpm --dir ui install
pnpm --dir ui dev                   # http://localhost:5173/ui/ — proxies the API to :8274
```

Vite serves the app at `/ui/` and proxies every other path to `barca serve`, mirroring
production, where the UI lives at `<prefix>/ui/` and finds the API at `<prefix>/`.

For a populated demo with synthetic commerce data, run from the repository root:

```bash
cargo build -p barca
python3 ui/demo/seed.py
cd ui/demo
PYTHONPATH=../../python ../../target/debug/barca serve --watch --no-schedule
```

Then run `pnpm --dir ui dev` from the repository root. The demo includes 20 nodes,
six schedules, sample order/customer/inventory schemas, successful and failed run
history, and cached, stale, never-run and unknown asset states. Tasks only print
demo output; no external services are contacted. Re-run the seed script to add
fresh history. The List/Graph toggle keeps the pipeline and selected node in the
URL, and existing `/graph` bookmarks redirect to the Assets graph view.

For a larger modeling pipeline, also run `python3 ui/demo/seed_modeling.py`
from the repository root. [demo/modeling.py](./demo/modeling.py) defines **152
nodes**: 76 assets and one validation task per asset. Names expose the hierarchy:
shared `data__` preparation, five `cv__fold_XX__` branches, `cv__summary__`,
`train__`, `test__`, and `release__`. Each fold separately caches its row splits,
preprocessing, features, initialization, fitting, model, predictions, and metrics.
Preprocessing is fitted on training rows only; the test set stays outside cross
validation. The seed script verifies a second identical get executes zero steps
and runs all 76 validators, leaving branch-specific runs in the history. Select
`modeling.py` in the sidebar to explore its List and Graph views.

The modeling UI demonstrates Python-declared organizational groups. Its 152 nodes appear as five
top-level groups; double-click or press Enter to open nested groups, and use the
breadcrumb to move up. List/Graph keeps the current group and selection in the
URL. Group health includes every descendant, including validation tasks; the
output's cache state is shown separately. “Preview failed check” simulates a
fold 3 validation failure entirely in the browser, and “All nodes” restores the
original flat view. Definitions live in `demo/modeling.py` using `group(...)`; the static parser
serves metadata through `/groups`. Any pipeline with groups uses this view.
`barca list --groups` exposes the same hierarchy in the terminal. Execution and
cache behavior stay unchanged.

## Check

```bash
pnpm --dir ui typecheck && pnpm --dir ui lint && pnpm --dir ui test
pnpm --dir ui gen:types             # after changing a Rust type the UI uses
cargo build -p barca && pnpm --dir ui test:e2e   # Playwright; starts barca serve + vite on e2e/fixture
                                                  # BARCA_BIN=<path> uses another barca binary
pnpm --dir ui dev                   # then /ui/#/kit lists every component in each state
```

## Build into barca

```bash
pnpm --dir ui build                 # → ui/dist
maturin develop --release           # the binary now embeds ui/dist
```

A binary built without `ui/dist` still works; `/ui/` then explains how to build the UI.
