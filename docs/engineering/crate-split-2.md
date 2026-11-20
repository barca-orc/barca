# Crate split, part two (#1056, C6)

Status: done, 2026-11-19. Non-normative record of how `barca-core` (67k lines after
[#905](crate-split.md), 55 modules in five layers) was dissolved into crates that match its
layers, with no change to commands, flags, JSON keys, routes, decorator arguments, output, the
`.barca/` layout, what is written to the database or the store, generated TypeScript, CLI
snapshots or the OpenAPI document. No crate named `core` remains. The plan is
[#1056](https://github.com/barca-orc/barca/issues/1056) (item C6, with the owner's decision of
2026-11-19 in its comments); the layers and the cuts that made them separable are in
[layering.md](layering.md); the crate map is in [CLAUDE.md](../../CLAUDE.md).

## The crates

Lines are of `src/`, tests included.

| Crate | Lines | Holds |
|---|---|---|
| `barca-model` | 9,335 | shared vocabulary: values, ids, hashes, time, `term`, the config; the graph (`ir` node types, the `dag` container, `names`, the `--batch` value); `source_scan`, the lexer and module-graph scan the layering tests share |
| `barca-python` | 15,648 | the Python frontend: `discover`, `parse` and the decorator readers, `definition`, `cone`, `project_modules`, `link`, `load`, `extracted` |
| `barca-compile` | 5,473 | planning: `targets`, `partition_selection`, `selection`, `planner`, `dispatch` (partition expansion, a phase's provided inputs), `warnings` |
| `barca-db` | 18,857 | the metadata database, unchanged but for `open::open_metadata` |
| `barca-proc` | 2,313 | `helper_proc` (start and stop a child process) and `protocol` (framed JSON over a Unix socket, the hello, worker and transfer messages, the socket path) |
| `barca-engine` | 8,488 | the per-step scheduler and worker loop: `coordinator`, `io_loop`, `wire`, `batch`, `cost`, `dataset_sinks`, `step_json`. No database |
| `barca-store` | 13,331 | storage: `transfer`, `store_sync`, `verified_copies`, `store_inspect`, `gc`, `backup`, `ledger`, `pins`, `pauses`, `run_queue`; the leaf types `results`, `envelope`, `text`, `http` |
| `barca-decide` | 7,385 | decisions: `cache`, `freshness`, `changes`, `plan_reasons`, `predict`, `recover`, `sizing`, `schedule` |
| `barca-run` | 12,802 | the run orchestrator: `execution`, `persist`, `incremental`, `mismatch`, `commands`, `telemetry` |
| `barca-analysis` | 18,502 | read models: `views`, `status`, `health`, `causes`, `estimates`, `refresh_path`, `liveness`, `schedule_standing`, `rollup`, `run_list`, `run_explanation`, `partition_grid`, `key_query`, `history_filter` |
| `barca-service` | 14,489 | one operation per command: `queries`, `loaded`, `served`, `report`, `outcome`, `sql`, `values`, `diagnostics`, `serve`, the `db` facade; the re-exports the hosts name |

## The graph

Each crate may depend on the crates listed for it, and on nothing else of the workspace.
`crates/barca-service/src/layering_tests.rs` holds this table against the manifests,
dev-dependencies included.

| Crate | Depends on | In tests only |
|---|---|---|
| `barca-model` | | |
| `barca-python` | model | |
| `barca-compile` | model | python |
| `barca-db` | model | |
| `barca-proc` | model | |
| `barca-engine` | model, compile, proc, python | |
| `barca-store` | model, compile, db, proc, python | |
| `barca-decide` | model, compile, db, store | python |
| `barca-analysis` | model, compile, db, store, decide | python |
| `barca-run` | model, compile, db, proc, engine, store, decide, python | |
| `barca-service` | all of the above | |
| `barca-server` | service | |
| `barca-cli` | service, server | |

```text
barca-model
├── barca-python          barca-compile          barca-db          barca-proc
│        └──────────┐          │    └──────────┐     │    ┌──────────┘ │
│              barca-engine ◀──┘               barca-store ◀───────────┘
│                    │                              │
│                    │                         barca-decide
│                    │                          │         │
│                    └──────────▶ barca-run ◀───┘    barca-analysis
│                                     └──────┐    ┌───────┘
└───────────────────────────────────────▶ barca-service ◀── barca-server, barca-cli
```

What the graph says:

- **`barca-engine` has no database.** Its manifest names neither `barca-db` nor libsql, as
  before. `barca-run` is the one crate that both drives it and records a run.
- **`barca-store` and `barca-decide` do not name the engine.** What they took from it is below
  it now: `helper_proc` and `protocol` in `barca-proc`, `dispatch` in `barca-compile`.
- **`barca-run` and `barca-analysis` are side by side** and neither names the other: a run
  records, a read model reads.
- **The hosts import one crate.** `barca_core::x` in `barca-cli` and `barca-server` is
  `barca_service::x`. `barca-service` alone re-exports lower crates' modules; a module of
  `barca-run` names `barca_decide::cache`, not a re-export.
- **libsql** is linked by `barca-db`, `barca-store` (gc, backup and the ledger read through a
  connection) and `barca-run` (proofs and step rows inside `barca-db`'s transaction);
  `barca-decide` and `barca-service` link it as a dev-dependency, for tests that write rows no
  owner writes. `barca-engine`, `barca-proc`, `barca-python`, `barca-compile`, `barca-analysis`
  and the hosts do not.

## Stages

One commit each on `refactor/crate-split`; after each the workspace built, clippy was clean
with `-D warnings` and `cargo test --workspace` passed. Files were moved with `git mv`; what
changed inside them is `crate::` paths and visibility.

1. The last wrong-way module edge and what lower layers took from the engine: `barca-proc`,
   `dispatch` to `barca-compile`, the run view's conversion beside its type.
2. `barca-store`. 3. `barca-decide`. 4. `barca-analysis`. 5. `barca-run`.
6. `barca-service`, the hosts repointed, `crates/barca-core` gone.
7. `barca-python`; the graph to `barca-model`; `barca-compile` is planning.

## Decisions that differ from the plan

- **A `barca-proc` crate.** `helper_proc` and the protocol's framing and transfer messages are
  used by the engine (workers) and by the storage layer (the transfer and state helpers), and
  neither may name the other. They name only `barca-model` and tokio. A crate of their own
  keeps tokio's `process` and `io-util` out of `barca-model` and a protocol edit from
  rebuilding the database crate. `step_json` (the frame a worker is handed, built from the
  coordinator's item) stays in the engine, as its own module.
- **`dispatch` is `barca-compile`'s, whole.** `predict` and `recover` use four of its
  functions; the module names only the plan and the model, so it moved as one file beside
  the planner.
- **The leaf modules are in `barca-store`.** `results` and `envelope` name the graph's and the
  plan's types, and `http` the HTTPS client, so they cannot be `barca-model`'s; the storage
  layer uses all four (`gc` returns a `CacheReport`, the ledger reads error envelopes over
  HTTP), so they sit in its first two rows. `barca-service` re-exports them for the hosts.
- **The graph container is `barca-model`'s, and planning stays `barca-compile`.** The plan
  suggested moving the planning modules into `barca-decide`. `barca-engine` reads the plan and
  must not reach the database, which `barca-decide` does, so planning stays below the engine.
  The `Dag` container, `names` and the `--batch` value moved to `barca-model` (which now links
  petgraph), so that the frontend and planning are both above it and neither names the other.
  `partition_selection` stays with planning: only the planner and `selection` use it.
- **`Dag::build`, `isolate`, `isolate_with` are functions** of `barca_python::link` (an
  inherent impl cannot cross a crate), and `BatchOverride::check` is defined with its type in
  `barca-model` (it was in `targets` only to keep the frontend's row off the graph's).
- **`open_metadata` is `barca_db::open::open_metadata`.** `pauses` (storage) needs it; it was
  defined in the facade the hosts see.
- **`barca_service::loaded`.** The commands that load the project and then ask a read model or
  a decision (`status`, `status_with_membership`, `rollup`, `grid_for_target`,
  `key_for_target`, `describe_schedule`) moved up from `status`, `partition_grid` and
  `schedule`, unchanged, so that `barca-analysis` and `barca-decide` take a graph and load
  none. `barca-cli` calls them at `barca_service::loaded::`.
- **The module scan is `barca_model::source_scan`.** A crate's `cfg(test)` code does not reach
  another crate's tests, and the layering test of every crate needs the same lexer and module
  graph. It is a `#[doc(hidden)]` module of `barca-model` (test support: nothing in a build
  calls it, and the release binary does not grow by it), beside its test-hooks module.
- **Three doc comments still say `barca_core::`** (`served/queue.rs`, twice, and
  `barca-server/src/views.rs`), and three still link `crate::` paths that are another crate's
  now (`results.rs`, `views/schedule.rs`, `run_list.rs`). ts-rs copies them into
  `ui/src/lib/generated/`, which had to come out byte-identical. Follow-up below.

## What still loads the graph from a lower crate

`barca-decide`, `barca-analysis` and `barca-compile` use the frontend in tests only (a test
writes a graph as Python source). Three crates below the service layer still depend on it:

| Crate | Where | What it does | To cut it |
|---|---|---|---|
| `barca-run` | `execution/prepare.rs` (`build_dag`, when the request carries no graph) | a run started from files loads them | every caller passes a graph (`ExecuteRequest.dag` is an `Option` today; the CLI's run path would load in `barca-service`) |
| `barca-store` | `gc/mod.rs` (`manual_nodes`: `discover::discover`, `parse::extract_nodes`) | the cache limit reads which nodes are `freshness=Manual`, statically, from the project's files | pass those node ids in from the caller, which has the graph. `gc/` was being changed on another branch while this one moved it, so its body was left as it was |
| `barca-engine` | `batch.rs` (`barca_python::batch_spec::evaluate_labels`, called by `annotate`, the dry run's account of batching, and re-exported for the run) | a batched step's `batches_by` labels are evaluated in a Python subprocess by the frontend's evaluator | pass the evaluator to `annotate` as a function, or move `annotate` to `barca-decide` beside `predict`, its one caller; the run names `barca_python::batch_spec::evaluate_labels` itself. Tried and reverted here: it is one call, not only a re-export |

So `barca-decide` and `barca-analysis` reach the frontend through `barca-store` in Cargo's
graph (an edit to the parser rebuilds them), though neither names it.

## Tests that moved, and hooks

- **Two tests printed a read model through the renderer above it.** The status table's test
  is `barca-service`'s `report::status_table_tests` (its two fixture helpers copied). The audit
  shape's rollup (698 nodes) is computed by `barca-analysis`'s
  `on_the_audit_shape_actionable_roots_lead`, which records it without node entries in
  `crates/barca-analysis/testdata/audit_rollup.json` (`BARCA_UPDATE_SNAPSHOTS=1` writes it);
  `report::audit_rollup_tests` in `barca-service` reads the record and asserts the same text
  as before. The text prints no node's own row, and a scratch test compared the full and the
  recorded rollup's text, byte for byte, before the node entries were dropped.
- **One frontend test planned.** `undeclared_env_reads_are_found_through_helpers_and_summarized_once`
  moved from `load` to `barca-compile`'s `warnings` tests, where the planner is.
- **Always-compiled test hooks**, all `#[doc(hidden)]`: `protocol::write_message` and
  `read_message` (the engine's tests frame messages with them), `ledger::CacheReader::open`
  (the decisions' tests), and the macro `store_check_fix!` (exported so the run orchestrator's
  last line can `concat!` it). Visibility was otherwise widened only where a cross-crate use
  needs it: `pub(crate)` became `pub` on those items, in modules that are `#[doc(hidden)]`
  where they were private.
- **Guards.** The manifest check, the write boundary, the step-status literals and the helper
  launch scan are in `barca-service`, above every crate they read, and read every library
  crate. `every_child_process_is_started_through_this_module` moved with `helper_proc` to
  `barca-proc`. The module layering test of `barca-core` became one test per crate on
  `source_scan::module_graph`: rows with no exception in `barca-store`, `barca-analysis`,
  `barca-compile` and `barca-python`; the size of the largest module cycle, which may only
  shrink, in `barca-decide` (4: `cache`, `changes`, `freshness`, `plan_reasons`), `barca-run`
  (2: `persist`, `telemetry`) and `barca-service` (1, which is none). Its allowlist of
  wrong-way edges and the list of engine uses were empty and are gone: Cargo checks both now.
  Dropped with them: the two unit tests of the layer table's `allowed` function and the unit
  tests of `barca-compile`'s own scanner, which the shared one replaced.

## Measured

Before is `integrate/modularity` at 78faf8fc, after is this branch on top of it (f2c6197e, before
the later commits of `integrate/modularity` were merged in), on one machine (32
cores, Linux) on 2026-11-19 while other builds ran on it (load 4 to 15), so times are
indicative and sizes and counts are exact.

**Release binary and budgets** (`cargo build --release -p barca`, fat LTO; `scripts/perf-gate.py`
on the release wheel, `TRIVIAL_RUNS=40`, three runs each):

| | Before | After |
|---|---|---|
| release binary, bytes | 20,344,112 | 20,580,880 (+236,768, +1.2%) |
| trivial run, wall median, ms | 54.1, 55.9, 51.0 | 47.1, 49.7, 48.6 |
| trivial run, coordinator CPU median, ms (budget 50) | 9.7, 9.8, 9.9 | 9.6, 10.0, 9.8 |
| trivial run, coordinator peak, KiB (budget 19,531) | 15,876, 15,860, 15,860 | 15,988, 16,024, 15,936 |
| incremental, coordinator CPU max, ms (budget 40) | 35.9, 18.4, 19.3 | 17.7, 16.6, 17.3 |
| incremental, coordinator peak, KiB (budget 19,531) | 16,212, 16,264, 16,132 | 16,660, 16,164, 16,604 |
| history, plan-reasons median, ms (budget 25) | 2.1, 1.6, 1.6 | 2.0, 1.9, 1.6 |

Every budget is met in every run. The binary is 1.2% larger and the coordinator's peak about
100 to 150 KiB higher (0.7%); the peak has 3.5 MiB of room under its budget on this machine.
No code was added: the growth is what seven more crate boundaries cost under `opt-level = "s"`
with fat LTO (each crate instantiates the generic code it uses, and items that were
crate-private are exported), and it was not chased further. #905 measured +0.3% for four
boundaries.

**Builds** (dev profile, a target directory of its own; "touch" is `touch` on one file, then
the same command; the count is workspace crates recompiled):

| | Before | After |
|---|---|---|
| clean `cargo build -p barca`, s | 40.4 (7 crates) | 35.8 (13 crates) |
| clean `cargo test --workspace --no-run` after it, s | 38.7 | 36.9 |

| One file touched | Crates rebuilt, before | after | `cargo build`, s | `cargo test --no-run`, s | Rust tests the merge tier selects, before | after |
|---|---|---|---|---|---|---|
| a read model (`health.rs`) | 3 | 4 | 5.5 → 2.3 | 19.7 → 5.4 | 938 | 641 |
| the run orchestrator (`execution.rs`) | 3 | 4 | 3.0 → 2.2 | 5.9 → 4.4 | 938 | 601 |
| the service layer (`report.rs`) | 3 | 3 | 3.0 → 1.8 | 12.1 → 15.5 | 938 | 509 |
| a decision (`cache.rs`) | 3 | 6 | 3.2 → 2.3 | 5.7 → 14.2 | 938 | 812 |
| storage (`transfer.rs`) | 3 | 7 | 3.1 → 2.6 | 13.5 → 5.7 | 938 | 938 |
| the engine (`coordinator.rs`) | 4 | 5 | 5.3 → 2.1 | 5.9 → 5.1 | 1,080 | 696 |
| the database (`db/runs.rs`) | 4 | 8 | 3.5 → 3.0 | 8.3 → 5.7 | 1,077 | 1,077 |
| the planner (`planner.rs`) | 5 | 9 | 3.6 → 2.8 | 11.4 → 12.2 | 1,385 | 1,110 |
| the parser (`parse.rs`) | 5 | 9 (10 with tests) | 3.5 → 2.9 | 13.2 → 13.9 | 1,385 | 1,349 |
| the model (`model.rs`) | 7 | 13 | 3.8 → 3.4 | 6.4 → 12.5 | 1,626 | 1,626 |

More crates are recompiled by one edit, and each is smaller: before, any edit above the
engine recompiled all 67k lines of `barca-core` and relinked its one test binary of 617
tests. The times of a touch are dominated by linking and by the machine's load and do not
separate cleanly; the count of tests is the plan's point. An edit to a read model, to the run
orchestrator or to the service layer no longer selects the other two layers' tests or the
decisions' and the stores'; an edit to the engine no longer selects the read models'. The
workspace has 1,626 Rust tests before and after (tests moved crates; the dropped guards' unit
tests and the added per-crate ones cancel): `barca` 107, `barca-analysis` 132, `barca-compile`
77, `barca-db` 139, `barca-decide` 79, `barca-engine` 95, `barca-model` 103, `barca-proc` 36,
`barca-python` 239, `barca-run` 92, `barca-server` 213, `barca-service` 188, `barca-store`
126. Before: `barca` 107, `barca-compile` 305, `barca-core` 617, `barca-db` 139, `barca-engine`
143, `barca-model` 102, `barca-server` 213.

## Follow-ups

- **Cut the three frontend dependencies** in the table above, `barca-store`'s first: it is
  what puts the parser under `barca-decide` and `barca-analysis` in Cargo's graph.
- **The six doc comments** that name `barca_core::` or a `crate::` path of another crate:
  correct them in a change that also regenerates `ui/src/lib/generated/` (comment lines only).
- **Order `barca-decide`'s and `barca-run`'s modules** (their cycles of 4 and 2) and turn the
  two ratchets into rows.
- **`barca-service` still names lower crates through its own re-exports** (`crate::cache` for
  `barca_decide::cache`): its files were moved with their paths as they were. Naming the crate
  that owns each item would let the re-exports shrink to what the hosts use.
- **Broken intra-doc links.** As after #905: no CI job runs `cargo doc`.
- **`barca-analysis` is the largest crate above the database** (18,502 lines) and `status`
  its largest module; a split of the views from the classification is the next cut if its
  rebuild time matters.
