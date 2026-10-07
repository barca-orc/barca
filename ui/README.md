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

## Check

```bash
pnpm --dir ui typecheck && pnpm --dir ui lint && pnpm --dir ui test
pnpm --dir ui gen:types             # after changing a Rust type the UI uses
cargo build -p barca && pnpm --dir ui test:e2e   # Playwright; starts barca serve + vite on e2e/fixture
```

## Build into barca

```bash
pnpm --dir ui build                 # → ui/dist
maturin develop --release           # the binary now embeds ui/dist
```

A binary built without `ui/dist` still works; `/ui/` then explains how to build the UI.
