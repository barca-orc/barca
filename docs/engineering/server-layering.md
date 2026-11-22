# Layers inside `barca-server` (#1056, E1)

Status: E1 done, 2026-11-19. Non-normative. The plan is
[#1056](https://github.com/barca-orc/barca/issues/1056); the check is
`crates/barca-server/src/layering_tests.rs` (`cargo test -p barca-server layering`), which is
the authority when this page and the code disagree.

Cargo checks nothing between the modules of one crate, so the test does. Every module has a
row; non-test code may name another module (`crate::<module>`) only when that module is in a row
above its own. A row never uses itself or a row below, so the modules cannot form a dependency
cycle. `lib.rs` wires the modules (`serve`, the drain) and is not one of them.

| Row | Layer | Modules | Holds |
|---|---|---|---|
| 0 | leaf | `auth`, `config`, `error`, `progress`, `project_socket`, `resolve`, `respond`, `run_model`, `snapshot`, `ui`, `views` | the token store; `ServeConfig` and its zone; `ApiError` and the event cursor's errors; what a run's events say of its steps; the project socket; which node a name names; bodies built from the CLI's renderers; `RunSpec`, `RunState`, `RunStatus`; the metadata DB as a read reads it; the embedded UI; `NodeState`, `NodeRunning`, `JobStatus` |
| 1 | part | `auth_http`, `host_guard`, `queue`, `replay`, `sources` | `Access` and the auth routes; loopback Host validation; the `Queue` and what is recorded of a queued run; `ReplayHub` and a run's event channel; the loaded graph (`Sources`) |
| 2 | part | `read_cache` | `Reads`: the config, the sources and what the read routes keep between requests |
| 3 | part | `sql_helper` | `QueryHelper`: the DuckDB helper process and its protocol |
| 4 | state | `state` | `AppState`, which composes the parts, with `Runs`, `Scheduling` and `Lifecycle` |
| 5 | view | `guards`, `running` | the read-only and draining refusals; the runs in flight and each node's `running` entry |
| 6 | view | `schedule_view` | where every schedule stands: recorded ticks, pauses, the scheduler's published jobs |
| 7 | view | `queue_view` | the queue as the routes answer it, its estimates, `queue` of `/health` |
| 8 | view | `health_view` | health assembly, the rows of `GET /state`, `node_status`, the warm-up task |
| 9 | runs | `runs` | admission, dispatch, a run's life, its projection and its routes |
| 10 | scheduler | `scheduler` | the cron scheduler |
| 11 | routes | `asset_pauses`, `assets`, `fallback`, `gc`, `inspect`, `ledger`, `pins`, `queue_routes`, `reset`, `schedule_routes`, `sql_query`, `state_routes`, `telemetry`, `watch` | one route family each, and the background tasks |
| 12 | router | `routes` | the router |

The rule the order encodes: a stateful part (rows 1 to 3) owns a struct and knows nothing of
`AppState`; `state` composes the parts; what needs several parts at once is a function in a
view row or above. A second test says so in words (`no_stateful_part_is_above_the_state_that_composes_it`),
so that lifting a part above `state` is a decision and not a reordering.

## What an edge is

A `crate::<module>` path (or `super::<module>` in a module's own file) outside comments, string
literals and `#[cfg(test)]` items. Files named `tests.rs` or `*_tests.rs` are test code. The
scan is a copy of the one in `crates/barca-compile/src/layering_tests.rs`. Test modules name
whatever they need: several build an `AppState` to test a part.

## Sub-states

`AppState` is seven fields, each a struct of shared handles (cloning it clones the same `Arc`s
it did as 26 flat fields):

| Field | Type | Holds |
|---|---|---|
| `config` | `Arc<ServeConfig>` | what the server was started with |
| `reads` | `Reads` (`read_cache`) | the same config, `Sources` (the loaded graph, load errors, the watch generation), the `ReadCache`, the metadata error for `/health` |
| `runs` | `Runs` | the run registry, event channels, the replay hub, run slots, the interactive slot, shared capacity, shared steps, the `Queue`, the run tasks |
| `scheduling` | `Scheduling` | the scheduler's published jobs, the schedule pauses, whether the scheduler stopped |
| `lifecycle` | `Lifecycle` | the shutdown and draining tokens, the cancel reason, when the server started |
| `auth` | `Arc<Access>` | the token store |
| `sql` | `Arc<QueryHelper>` | the query helper |

A handler takes the parts it uses through axum's `FromRef` (`State<Reads>`,
`State<Lifecycle>`, `State<Runs>`, `State<Arc<Access>>`), so a route that only reads the graph
or the metadata DB cannot reach the run registry:

| Takes | Handlers |
|---|---|
| `Reads` | `/list`, `/groups`, `/assets`, `/assets/{name}/schema`, `/stats/{name}`, `/sql/views`, `/materializations`, the five `/assets/{name}/partitions*` routes, `/logs/{run_id}`, `/pins`, `/assets/{name}/versions`, the pin routes, `/pauses*`, `/gc`, `/ledger/lookup` |
| `Reads` and `Runs` | `/plan` (the shared capacity sizes the plan) |
| `Reads` and `Lifecycle` | `/project` (when the server started), `/ledger/runs` (refused while draining) |
| `Arc<Access>` | the `/auth/*` routes and the `authorize` middleware |
| `AppState` | `/health`, `/state`, `/state/rollup`, `/nodes`, `/history`, `/assets/{name}` (each reports what runs now), `/schedule*`, `/queue*`, `/state/reset`, `/sql/query`, the run routes |

## Before and after E1

| | Before | After |
|---|---|---|
| Modules | 24 | 40 |
| Largest dependency cycle | 13 modules: `auth_http`, `error`, `handlers`, `inspect`, `pause`, `queue`, `read_cache`, `replay`, `running`, `runs`, `scheduler`, `sql_query`, `state` | none |
| Edges that break the order | not defined (no order) | 0: `ALLOWED` is empty |
| `AppState` fields | 26 | 7 |
| Handlers taking `State<AppState>` | all | 25 of 56 |

The cuts, none of which changes a route's status, body, headers or error text:

| Edge cut | How |
|---|---|
| `error` -> `replay` | `CursorError` to `error` |
| `replay` -> `running`, `state` -> `running` | `Progress` to `progress`; `NodeRunning` to `views` |
| `state` -> `read_cache`, `queue`, `auth_http`, `sql_query` and back | each part's methods on its own struct: `impl AppState` in `read_cache` is `impl Reads`; the loader is `Sources`; the auth handlers take `Arc<Access>`; the helper process is `sql_helper`, with the tables and the route in `sql_query` |
| seven modules -> `handlers::{SnapshotDb, snapshot_db, snapshot_of}` | `snapshot`, and `Reads::snapshot_db` |
| six modules -> `runs::refuse_if_read_only`, `refuse_if_draining` | `guards` |
| `inspect` -> `runs::{json_text, stdout_of}` | `respond` |
| three modules -> `scheduler::zone_of` | `config` |
| `state` -> `read_cache::SOURCES_SETTLED` | `sources` |
| `queue` <-> `runs` | `queue` keeps the `Queue` and what is recorded; admission, dispatch and the resume at startup are `runs::admission`; the view and its routes are `queue_view` and `queue_routes` |
| `handlers` (five things) | `snapshot`, `health_view`, `schedule_view`, `state_routes`, `assets`, `fallback`; `GET /logs/{run_id}` beside the other history reads in `inspect` |
| `pause` (state and routes) | the pauses a server holds to `schedule_view`; the routes, with `GET /schedule`, to `schedule_routes` |

Name resolution by "the full id, or what follows the file" was written out in four places. The
matching is now `resolve::{names, matching}`; each route keeps its own words for no match and
for several, because the contract and the tests pin different texts per route.

## What is left

- The mutating routes that need only the config and the metadata DB (`/pins`, `/pauses`,
  `/gc`) take `Reads`, whose name says less than what they do. A `Project` sub-state under
  `Reads` would name it better; nothing depends on it.
- `runs` is one module of five files and the only place that needs every part. Its handlers
  all take `AppState`.
- `Scheduling` has no `FromRef`: nothing takes it alone.
- The integration tests (`tests/*.rs`) still create `crates/barca-server/.barca` (ignored by
  git): `ensure_env_dirs` creates `.barca` in the working directory, which Cargo sets to the
  crate. The unit tests moved to a directory under the build directory
  (`test_support::leave_source_tree`). Moving the integration tests means editing the
  conformance tests or making `ensure_env_dirs` take a root.
