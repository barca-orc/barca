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

## Design core

The look of the UI is defined in three places, in this order:

1. **Tokens** (`styles/tokens/`): colors, type scale, spacing, radius, elevation, motion, and
   the light theme. The only place a color or a size is written as a value.
2. **Primitives** (`components/`): `Button`, `IconButton`, `Tag`, `Chip`/`ChipGroup`, `Select`,
   `SearchInput`, `StatusDot`/`StatusBadge`, `Skeleton`, `ConnectionBadge`, and `SidePanel` with
   `Section` and `KeyValue`. Their CSS is inline (tokens as `var(--…)`) or, where it needs
   pseudo-classes, in `styles/components.css`.
3. **Pages** (`pages/`, `layouts/`): compose primitives; `styles/shell.css` holds page layout
   only.

Rules:

- A page does not write a `<button>`, `<select>` or `<input>` for something a primitive covers,
  and does not restyle a primitive. If it needs a variant, the primitive gets it.
- A second use of a pattern is the cue to make the primitive; the first use stays in its page.
- A new or changed primitive is added to the kit page (`pages/KitPage.tsx`, at `/ui/#/kit`
  under `pnpm dev`) in every state it has. The kit page is not in the built UI.
- `styles/tokens.test.ts` fails on a raw color or an off-scale font size outside
  `styles/tokens/`. Add a token instead of an exception.

Not primitives yet, by that second-use rule: the sortable table, the run list, the log viewer's
frame. They are the next candidates when a second view needs them.

## Layout stability

A page must not move when its data arrives. The rules:

- **Reserve the space.** Content that loads later gets a `Skeleton` sized like what will
  replace it, or a fixed-size slot that is always rendered (the node panel's duration
  histogram shows "needs two or more successful runs" in the same box). Never render
  `{data && <Thing/>}` where `Thing` changes the height of what follows it.
- **Late things go last.** A control that appears after data loads (a filter chip) sits after
  the controls that are always there, so it pushes nothing that was already placed.
- **Three states, not two.** "Not loaded yet" is its own state (`lib/connection.ts`:
  connecting / online / offline). Showing the failure state until the data arrives is wrong
  and shifts when the truth lands.
- **Fixed-width digits.** `font-variant-numeric: tabular-nums` is on `body`; numbers that
  update in place keep their width.
- **Scrollbars take their space always** (`scrollbar-gutter: stable` on the scroll areas).

`e2e/layout-shift.spec.ts` enforces this: every flow runs with the API held back 800ms (so the
loading state is visible and the data lands after the browser's 500ms input window) and
fails over a small shift budget, naming the elements that moved. A new page or panel gets a
flow there. Run it with `pnpm test:e2e`; `e2e/helpers/layoutShift.ts` has the helpers.

Known gap: the self-hosted fonts load with `font-display: swap`, which reflows text once when
they arrive. The tests do not exercise that (fonts are local and fast).

## Toolchain

- **pnpm**, **strict TypeScript** (latest stable), **ts-pattern** across the board.
- **ts-rs** generates the wire types from Rust (`pnpm gen:types`) — Rust is the
  single source of truth; the frontend never hand-writes a wire type.
- **vitest** for the pure-logic tests (`pnpm test`).
- **tsgo** (TS 7 native preview) for fast typechecking (`pnpm typecheck`, ~6× faster than
  `tsc`); the production `build` keeps stable `tsc -b` as the authoritative gate.
- Styling via the barca design-system CSS tokens; components reference `var(--…)`, never
  invented colors.

Graph state presentation lives in `src/lib/graphState.ts`. Resting node
colors come from `/state`: cached green, stale/partial yellow, never-run/unknown/always-run
neutral, and the latest failed attempt red even if an older artifact exists. Active
run events overlay that state; completion refreshes it. Selection uses a separate outline.
The graph displays every asset and dependency in the selected pipeline. No nodes
are grouped or collapsed automatically.
