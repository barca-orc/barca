# barca UI — architecture

The frontend is built on three principles: **model the backend's types faithfully**,
**make illegal states unrepresentable and illegal transitions uncompilable**, and
**keep decisions in pure functions so they can be tested without a browser**.

## Functional core, thin imperative shell

Every meaningful decision is a **pure function** that takes data and returns data — no
React, no `fetch`, no `EventSource`. React components and hooks are a thin shell that
pipes I/O into those functions and maps their output to elements. This is what lets us
test behaviour (including failure behaviour) by calling a function and asserting on its
return value, with no rendering and no mock server.

## Exhaustive matching with ts-pattern

barca's domain is a set of Rust enums (`NodeKind`, `Freshness`, `RunStatus`, `RunEvent`).
We mirror each as a **discriminated union** in `lib/types.ts` and consume it with
`ts-pattern`'s `match(...).exhaustive()`. Exhaustiveness is the point: when the Rust wire
protocol gains a variant, every `.exhaustive()` site that handles it becomes a **compile
error** until updated. A state can never be silently dropped — which is precisely the bug
that once made a failed run render nothing.

> Rule of thumb: if you're branching on a union's tag, use `match().exhaustive()`, not
> `if`/`switch` with a default. The default is where states go to die.

## The three layers

### 1. Business logic — slim

The domain lives in Rust; the frontend's business logic is deliberately thin.

- `lib/api.ts` — typed `fetch` wrappers over the barca-server HTTP API.
- `lib/types.ts` — re-exports the **generated** wire types and adds the few
  frontend-only / ad-hoc-JSON types (`StatusKind`, `LogLine`, `Health`,
  `RunHandle`, `AssetDetail`).
- `lib/generated/` — TypeScript types **generated from Rust** via `ts-rs`. Do
  not hand-edit. Rust is the single source of truth, so the wire contract can't
  drift (it once did — `Freshness` is `{"type":"Always"}`, PascalCase, which a
  hand-written mirror got wrong).

**Regenerating types:** annotate the Rust type with
`#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]`, then run
`pnpm gen:types` (which runs `cargo test --features ts export_bindings` with the
output dir set to `lib/generated/`). The `ts` feature is off by default, so
`ts-rs` stays out of the production binary. CI should run `pnpm gen:types` then
`git diff --exit-code lib/generated` to make drift a failing build.

### 2. Presentation logic — pure & tested

Pure functions that decide *what* to show. These are the unit-test targets.

| Module | Responsibility | Test |
|---|---|---|
| `lib/runStream.ts` | `reduceRunEvent(state, event)` — fold an SSE event stream into UI state | `runStream.test.ts` |
| `lib/runFeedback.ts` | `runFeedback(status, logs, error)` — map a node's status to a feedback descriptor | `runFeedback.test.ts` |
| `lib/status.ts` | `statusMeta`, `freshnessLabel`, `nodeKindLabel` — domain → display tokens | — |
| `lib/graph.ts` | `buildGraph(assets, dir)` — assets → dagre-positioned React Flow nodes/edges | — |
| `lib/pipeline.ts` | derive pipeline/source names from asset ids | — |

These return **descriptors** (plain data / discriminated unions), never JSX. e.g.
`runFeedback` returns `{ kind: 'failed', error, logs }`, not an `<ErrorPanel>`.

### 3. Presentation — thin React

- `components/`, `pages/`, `layouts/` — map descriptors and state to elements. A component
  should mostly be a `match` over a descriptor's `kind`, one arm per variant.
- `hooks/` — thin wrappers that connect I/O to the pure core. `useRunStream` opens the
  `EventSource` and pipes each event through `reduceRunEvent`; all the folding logic lives
  in `lib/runStream.ts`, so the hook itself needs no test.

## Example: the run stream, end to end

```
SSE bytes ──▶ useRunStream (shell: owns EventSource)
                    │  JSON.parse → RunEvent
                    ▼
            reduceRunEvent(state, event)        ← pure, exhaustive, tested
                    │  RunStreamState
                    ▼
            runFeedback(status, logs, error)    ← pure, exhaustive, tested
                    │  RunFeedback descriptor
                    ▼
            NodeInspector renders match(descriptor)  ← thin presentation
```

The two pure steps are covered by `runStream.test.ts` and `runFeedback.test.ts`,
including the worker-failure path (`"No module named 'sklearn'"` → node `failed` + error
surfaced). No browser, no server, no mocks.

## Toolchain

- **pnpm**, **strict TypeScript** (latest stable), **ts-pattern** across the board.
- **ts-rs** generates the wire types from Rust (`pnpm gen:types`) — Rust is the
  single source of truth; the frontend never hand-writes a wire type.
- **vitest** for the pure-logic tests (`pnpm test`).
- **tsgo** (TS 7 native preview) for fast typechecking (`pnpm typecheck`, ~6× faster than
  `tsc`); the production `build` keeps stable `tsc -b` as the authoritative gate.
- Styling via the barca design-system CSS tokens; components reference `var(--…)`, never
  invented colors.
