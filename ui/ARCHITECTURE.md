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

The domain lives in Rust; the frontend's business logic is deliberately thin. It derives
nothing: every module of `lib/` is one of six kinds (fetch, types, display, format, layout,
page), listed in `crates/barca-cli/ui-layering.json` and checked by
`scripts/check-ui-layering.py` on every pull request (README, "The UI only renders").

- `lib/api.ts` — typed request wrappers over the barca-server HTTP API.
- `lib/credentials.ts` — tab-scoped credentials, request scope and the shared fetch transport.
- `lib/eventStream.ts` — incremental fetch-SSE decoding, abort and reconnect lifecycle.
- `context/AuthGate.tsx` — authentication discovery and the Connect screen; project queries
  mount only after the server mode/connection is known.
- `lib/types.ts` — re-exports the **generated** wire types and adds the few
  frontend-only / ad-hoc-JSON types (`StatusKind`, `LogLine`, `Health`,
  `RunHandle`, `AssetDetail`, `NodeStats`, `ErrorEnvelope`, `RunOutcome`,
  `RunViewStatus`, `ScheduleJob`). These mirror JSON the server builds by hand
  (`serde_json::json!`), so they cannot be generated; the enums inside them that are Rust
  enums (`CacheState`, `ErrorKind`) are.
- `lib/generated/` — TypeScript types **generated from Rust** via `ts-rs`. Do
  not hand-edit. Rust is the single source of truth, so the wire contract can't
  drift (it once did — `Freshness` is `{"type":"Always"}`, PascalCase, which a
  hand-written mirror got wrong).

**Regenerating types:** annotate the Rust type with
`#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]`, then run
`pnpm gen:types` (which runs `cargo test --features ts export_bindings` with the
output dir set to `lib/generated/`). The `ts` feature is off by default, so
`ts-rs` stays out of the production binary. CI's merge tier runs it and then
`git diff --exit-code` on `lib/generated`, so drift fails the build.

### 2. Presentation logic — pure & tested

Pure functions that decide *what* to show. These are the unit-test targets.

| Module | Responsibility | Test |
|---|---|---|
| `lib/runStream.ts` | `reduceRunEvent(state, event)` — fold an SSE event stream into UI state | `runStream.test.ts` |
| `lib/runs.ts` | `runStatus`, `runIsLive`, `triggeredByLabel`, `parseOutcome` — a run's status, trigger and outcome → display | `runs.test.ts` |
| `lib/queue.ts` | `queueState`, `queueControl`, `queueRows` (running runs first, then the waiting ones, each with its elapsed time), `queueSummary`, `healthQueueLabel` — `GET /queue` → the Runs page's queue strip and list; `queueStatus`, `queueStatusLabel` — the queue in brief that every page shows while runs wait (`components/QueueStatus.tsx`) | `queue.test.ts` |
| `lib/schedules.ts` | `scheduleRows(assets, states, now, jobs)` — scheduled nodes with next run, last run, pause; `lateness` — a schedule that missed its slot (`late_since` past the grace), "late by 12m" | `schedules.test.ts` |
| `lib/dataSignals.ts` | `signalLine(signal)` — a data signal of a status row (`empty`, `rows_changed`, `behind_input`) in one line; `dataSignalSets` — the words of the signals and lateness the server reports; `dataCoverage`, `coverageLine` — "7 of 556 measured" from the rows' `measured` | `dataSignals.test.ts` |
| `lib/healthDisplay.ts` | `healthDisplay`, `HEALTH_ORDER`, `healthFamily`, `worstHealth` — the word, tone and glyph of the server's `health` (classified in `barca-core`, `crates/barca-core/src/health.rs`); `checkOutcomeLabel` — the word of a `last_check.outcome`, which `components/assets/LastCheckLine.tsx` puts in a node's panel ("checked 3m ago, up to date"); `components/assets/RefreshPathLine.tsx` shows the server's `refresh_path` there, one field at a time ("the schedule of inbox · next tick in 3m", or "no refresh path"); nothing here decides a health or a path | `healthTree.test.ts`, `LastCheckLine.test.tsx`, `RefreshPathLine.test.tsx` |
| `lib/runFeedback.ts` | `runFeedback(status, logs, error)` — map a node's status to a feedback descriptor | `runFeedback.test.ts` |
| `lib/cacheStateDisplay.ts` | the one table from `CacheState` to display: each of the CLI's eight words with its tone and rank; `lastAttemptFailed` — the fact shown beside a state; `shownCacheState` — a node that always runs shows its last run (`succeeded`, or `never run`) | `cacheStateDisplay.test.ts` |
| `lib/status.ts` | `statusMeta` — the run lifecycle's one table (`StatusKind` → tone and word); `freshnessLabel`, `nodeKindLabel` | `status.test.ts` |
| `lib/tone.ts` | `Tone`, `toneMeta` — the status hues both tables map onto; colour is never the only carrier | — |
| `lib/graphState.ts` | `graphState(node)` — a node's box: its cache state (word, tone, detail, failed last attempt), the run on it, or no state yet | `graphState.test.ts` |
| `lib/graphView.ts` | `fitViewport`, `fitTarget`, `revealViewport`, `neighborInDirection`, `waitsOn` — the viewport a fit sets (never below the zoom where a card's smallest label is 11px: what fits there around the selected, failed, running node or the start, the rest a pan away), the pan that keeps a selection clear of an opening panel, where the keyboard goes, what a never-run node waits on | `graphView.test.ts` |
| `lib/groupedPage.ts` | the grouped page's derivation: params, scope, entries, search hits, boxes, URL changes, row navigation, header counts (`scopeCountsLabel`), rows held in place under the pointer (`holdOrder`, `hooks/useHeldOrder.ts`); the graph draws the same entries as the list at every level, All pipelines included | `groupedPage.test.ts` |
| `lib/knownStates.ts` | `knownStates(current, loaded)` — while a scope's state loads, each node's newest row already loaded (`useKnownStates`): a navigation keeps the last known state, "Checking…" only for a node never loaded | `knownStates.test.ts` |
| `lib/graph.ts` | `buildGraph(assets, dir)` — assets → dagre-positioned React Flow nodes/edges; `layoutInput` / `graphAt` split it around the layout; `impliedEdges` — in a graph of groups, the edges another path already shows (left out of the layout, drawn for the selection) | `graph.test.ts` |
| `lib/graphLayout.ts` | `layout(input)` — dagre per connected part (by dependency depth), parts packed in rows, and each edge's route (`routeEdges`: straight through a rank between its boxes, curving only between ranks, so no edge crosses a card; drawn by `routePath` in `components/graph/RoutedEdge.tsx`), no React: what `graphLayout.worker.ts` runs off the main thread | `graph.test.ts` |
| `lib/pipeline.ts` | pipeline/source names from asset ids; `pipelineSummaries` (a file's sidebar count, and its state from its sums in the rollup's `files`), `fileShown` (those sums as a glyph and a word, for the sidebar and the table's file header) | `pipeline.test.ts` |
| `lib/health.ts` | `healthCounts` (a scope by state word, failed last attempts beside them), `failures`, `failedUpstream`, `consumersOf`, `scopeAge`, `ageCell`, `streakLabel` — what is broken in a scope, why, and how late; a failing streak is always "N failed runs in a row", a partitioned node's success is "any key"'s, a group's "oldest member"'s | `health.test.ts` |
| `lib/time.ts` | `parseTime`, `formatAgo`, `formatRelative` (`3h ago`, `in 5m`), `formatInstant` — the one time format, drawn by `Age` (relative, absolute on hover) | `time.test.ts` |
| `lib/sidebar.ts` | `updateSidebar`, `fileGroups`, `listedPipelines` — the sidebar's collapse and width (dragged, by keyboard, reset), the files it lists and the tree of groups under a file | `sidebar.test.ts` |
| `lib/documentTitle.ts` | `documentTitle` — the tab's title from the route: what is open, the section, the app | `documentTitle.test.ts` |
| `lib/writeAccess.ts` | `writeAccess`, `writeControl` — whether every write action is drawn: hidden on a read-only server, disabled until `/health` answers | `writeAccess.test.ts` |
| `lib/names.ts` | `splitMiddle` — a long name's cuttable head and its kept tail (`MiddleName`) | `names.test.ts` |
| `lib/palette.ts` | `paletteIndex`, `searchPalette`, `updatePalette` — the command palette (⌘K): each result with its state, failing first among equal matches | `palette.test.ts` |
| `lib/runTarget.ts` | `runTarget`, `runButton` — what a page toolbar's Run button acts on and how it says so | `runTarget.test.ts` |
| `lib/lineage.ts` | `lineage(id, graph, depth)` — what a graph too big to draw shows instead | `lineage.test.ts` |
| `lib/partitionGrid.ts` | `gridOf`, `filterCells`, `issueGroups`, `heatmap`, `filterSelection`, `issueSelection` — every key of a partitioned node (`GET /assets/{id}/partitions/grid`) decoded and filtered, its problems grouped with where they are, binned into the overview, and a filter or a problem turned into a run's `partition` | `partitionGrid.test.ts` |
| `lib/keysPage.ts` | `keysParams`, `keysSearch`, `keysHref` — the key explorer's URL (node, open key, filters) | `keysPage.test.ts` |
| `lib/runsPage.ts` / `lib/tokensPage.ts` | page programs (`update`), `visibleRuns`, `runActivity`; `runsCountLabel` (a filtered count over the runs loaded says it is partial), `runStepRows` (a run's Steps as rows: a node's keys one row with their count, listed when opened) | `*.test.ts` |

These return **descriptors** (plain data / discriminated unions), never JSX. e.g.
`runFeedback` returns `{ kind: 'failed', error, logs }`, not an `<ErrorPanel>`.

### 3. Presentation — thin React

- `components/`, `pages/`, `layouts/` — map descriptors and state to elements. A component
  should mostly be a `match` over a descriptor's `kind`, one arm per variant.
- `hooks/` — thin wrappers that connect I/O to the pure core. `useWriteControl` is the one place a write button learns whether to draw itself (`lib/writeAccess.ts`); `useNow` is one shared 15 s clock for an `Age` with no fetch time of its own; `useDocumentTitle` keeps the tab's title. `useRunStream` opens the
  `EventSource` and pipes each event through `reduceRunEvent`; all the folding logic lives
  in `lib/runStream.ts`, so the hook itself needs no test.

## Model, update, view: pages with state of their own

A page that remembers more than its URL and its server data is a **program**
(`lib/program.ts`): a `Model`, a `Msg` union of everything that can happen to it, an `Effect`
union of everything it asks the outside world to do, and a pure, exhaustively matched
`update(model, msg) → [model, effects]`. Its view is a component of the model and of its server
data, which only dispatches messages; `hooks/useProgram.ts` runs the program and hands each
effect to the page's interpreter, which does the I/O and dispatches the reply as a message.

Server data reaches a view as a `Remote<T>` (`lib/remote.ts`): `loading`, `failed`, or `ready`
with the error of a failed refresh. TanStack Query still fetches, caches and polls; `remote()`
folds its flags into the union at the edge.

| Page | Program | View | Container |
|---|---|---|---|
| Tokens | `lib/tokensPage.ts` | `pages/TokensView.tsx` | `pages/TokensPage.tsx` |
| Runs | `lib/runsPage.ts` | `pages/RunsView.tsx` | `pages/RunsPage.tsx` |
| Failures view | `lib/failuresView.ts` | `pages/FailuresView.tsx` | `pages/GroupedPipelinePage.tsx` |
| Key explorer | `lib/keysPage.ts` + `lib/partitionGrid.ts` (pure derivation; the URL is the model) | `components/partitions/*` | `pages/KeysPage.tsx` |
| Grouped pipeline | `lib/groupedPage.ts` (pure derivation; the URL is the model) | `pages/GroupedView.tsx` | `pages/GroupedPipelinePage.tsx` |

Any screen of such a page is a value: construct a model and a `Remote`, render the view. The kit
page does that for each state, and `replay(update, model, msgs)` tests a sequence of messages
without a browser. A new page with local state follows the same split; the URL stays the source
of truth for what is selected (selecting is a `navigate` effect, the page reads the URL back).

## The kit: every state, from fixtures

`/ui/#/kit` (development only, outside the auth gate so it needs no server) renders each
component and each program's view in each of its states. The states come from `kit/fixtures.ts`,
whose tables are `satisfies Record<Union, …>`: a variant added to a union, a generated wire type
included, is a compile error there until it has a fixture, and `kit/fixtures.test.ts` checks
each fixture is the state its key names. A view too big to show side by side is shown one state
at a time with chips; the messages a click sends are listed under it.

## Large projects: what is broken, and why

`lib/health.ts` answers the questions a project of thousands of nodes raises, over `/state` rows
and the `/assets` graph: a scope's counts by state word (`healthCounts`, drawn by `HealthBar`:
one segment per cache state present, and the failed last attempts counted beside them), its
failures with their error headline and how many nodes downstream wait on each (`failures`), and
the failed nodes upstream of a stale one (`failedUpstream`). The grouped page shows the scope's
health beside its breadcrumb, a health bar per group row, and a Failures view of every failure
in scope, exactly as `GET /state` reports it (nothing is muted on the client); the node panel
leads with the error, or with what a stale node waits on.

A group or pipeline asks `GET /state` for its scope by group id or file (`?targets=group:…`,
`?files=…`), which the server expands: never a list of node ids, which a server refuses past a
few hundred. A graph above `GRAPH_LIMIT` nodes (`lib/lineage.ts`) draws the selected node's
lineage, two steps up and down, instead of laying out a graph nobody can read.

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
2. **Primitives** (`components/`): `Button`, `IconButton`, `Age`/`LastSuccess`, `Tag`, `Chip`/`ChipGroup`, `Select`,
   `SearchInput`, `StatusGlyph`/`StatusBadge`, `StatusDot` (the connection only), `DagNode`, `HealthBar`, `Skeleton`, `ConnectionBadge`, `ErrorLine`,
   `MiddleName`, and `SidePanel` with `Section` and `KeyValue`. Their CSS is in
   `styles/components.css`, by class and `data-*` attribute (`data-tone`, `data-size`); an inline
   `style` is only for a value computed at render (a width, a position, a bar's share).
3. **Pages** (`pages/`, `layouts/`): compose primitives; `styles/shell.css` holds page layout
   only, every page's. No page imports a stylesheet of its own.

Rules:

- A page does not write a `<button>`, `<select>` or `<input>` for something a primitive covers,
  and does not restyle a primitive. If it needs a variant, the primitive gets it.
- A second use of a pattern is the cue to make the primitive; the first use stays in its page.
- A new or changed primitive is added to the kit page (`pages/KitPage.tsx`, at `/ui/#/kit`
  under `pnpm dev`) in every state it has. The kit page is not in the built UI.
- The palette is true neutral greys (hairline borders of low-alpha white on dark), one quiet
  indigo accent (`--accent`: focus rings and the pinned state), and the state hues. Colour is
  status, drawn only as a `StatusGlyph` (`lib/glyph.ts`): a ~14px shape in its state's hue, its
  word beside it in the text colour (`StatusBadge`). Rows, banners, chips, node cards and error
  text stay neutral; a failing row is found by its glyph and the default sort, not a tint. Each
  state has its own glyph, shape and hue (`--state-*`): `failing` a red filled cross, `running`
  a blue turning arc, `fresh` a green filled check, `stale` amber half-filled, `blocked` an
  orange barred ring, `partial` violet three-quarters, `unsynced` teal arrow, `pinned` indigo,
  and the quiet states (never run dashed, unknown, paused, queued, cancelled, skipped) grey. A
  node that always runs (a task, a sensor) has no cache to describe: it shows its last run,
  `succeeded` with the fresh check (the one state that shares a glyph), `failing`, or `never run`;
  that it always runs is said in its tooltip. A `Tone` (`lib/tone.ts`) is the separate question of urgency, which logic reads
  (what needs attention, what sorts first); states may share a tone, never a glyph. A node's
  cache state is one of the CLI's eight words, mapped once in `lib/cacheStateDisplay.ts`; a run's
  status once in `lib/status.ts`. `--signal` (primary actions, links, selection) is neutral;
  `tokens.test.ts` keeps it off the status hues and holds every text colour and state hue at
  4.5:1 on every background of both themes.
- `styles/tokens.test.ts` fails on a raw color or an off-scale font size outside
  `styles/tokens/`. Add a token instead of an exception.

Not primitives yet, by that second-use rule: the sortable table, the run list, the log viewer's
frame. They are the next candidates when a second view needs them.

## Large projects

A project can have thousands of nodes; a view must not render all of them at once.

- **Long tables render the rows in view.** `hooks/useTableWindow.ts` (TanStack Virtual) gives a
  table body the rows near the viewport; `TableSpacer` holds the height of the rest, so the
  sticky header and the scrollbar behave as before. Keyboard navigation goes through
  `focusWindowRow`, which scrolls to a row that is not rendered yet. A table short enough to fit
  renders no spacer, so it has only its own rows. The assets and the groups tables use it.
- **Run details render what is in view.** `LogViewer` renders only the log lines in view
  (TanStack Virtual, each line measured, since a long line wraps), and a run's Steps are rows
  from `runStepRows` in a box of their own, rendered the same way: a node's keys are one row with
  their count until it is opened. A failed run of 5,000 keys and 30,000 log lines renders about
  1,700 elements, not 150,000.
- **Big graphs are laid out in a worker.** `hooks/useGraphLayout.ts` lays out a graph of 300
  nodes or more in `lib/graphLayout.worker.ts` and shows "Laying out N nodes…" in the canvas
  until it arrives; a smaller one is laid out in place, with its data. A poll that returns the
  same structure keeps its layout. The minimap is left out above 400 nodes: it redraws every
  node on each pan.
- Measured on 1,000 partitioned nodes with the CPU slowed four times: a poll of the assets
  table during a run has no long task, and the remaining cost of a 1,000-node graph is the
  browser laying out its nodes when it first shows them.

## Layout stability

A page must not move when its data arrives. The rules:

- **Reserve the space.** Content that loads later gets a `Skeleton` sized like what will
  replace it, or decide from data already on screen whether the slot will exist (the node
  panel's duration histogram is drawn only when the node's `durations.samples` from `/state`
  is two or more, so it never appears or vanishes when the history loads). Never render
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

- **pnpm**, **strict TypeScript** (latest stable), **ts-pattern** across the board. Styling is
  the design-system CSS alone (`styles/`): no utility framework, no CSS-in-TS.
- **ts-rs** generates the wire types from Rust (`pnpm gen:types`) — Rust is the
  single source of truth; the frontend never hand-writes a wire type.
- **vitest** for the pure-logic tests (`pnpm test`).
- **tsc** is the one type checker: `pnpm typecheck` runs `tsc -b`, the same check `build`
  runs before Vite (the project builds in a few seconds, so a second, faster checker would only
  be a second opinion that can disagree).
- Styling via the barca design-system CSS tokens; components reference `var(--…)`, never
  invented colors.

A group card (`groupHealth`) leads with its most severe fact in words (failing beats running beats
stale, the order of `healthParts`), the other parts beside it as glyph and count; the graph's
legend lists every state on its cards, each part of a group card included.

Graph state presentation lives in `src/lib/graphState.ts`. A resting node shows its
`cache.state` from `/state` as its glyph and the CLI's word; a failed last attempt shows as
`failing` (the red cross glyph) in its place, and the state stays in the tooltip and the panel. A
run working on the node overlays that (the running glyph turns, the card gets the running
ring); completion refreshes it. Selection uses a
separate outline. The graph displays every asset and dependency in the selected pipeline. No
nodes are grouped or collapsed automatically.

## End-to-end suites

Three Playwright configs, each starting `barca serve` on its own fixture project and the Vite
dev server in front of it (`pnpm test:e2e` runs them in turn, as CI does): `e2e/` on the flat
project (`e2e/fixture`), `e2e-groups/` on a project declared in groups (`e2e-groups/fixture`,
a failing check among cached models), and `e2e-auth/` in token mode. The groups, states, runs
and failures are the server's; a spec stubs a route only to make the server say something the
fixture cannot (a state request that fails, a read-only health).
