# Layers inside barca-core (#1056 track C)

Status: guard in place and the cheap cuts made, 2026-11-19. Non-normative. The plan is
[#1056](https://github.com/barca-orc/barca/issues/1056); the crate graph above this is in
[crate-split.md](crate-split.md).

The crates point one way, but inside `barca-core` 38 of the 45 modules formed one dependency
cycle: each could reach every other through `crate::` paths in non-test code. A layer cannot
become a crate while that holds, so the modules get layers first and a test that keeps the
count of wrong-way edges from growing.

## The layers

| Rank | Layer | Modules | Holds |
|---|---|---|---|
| 0 | leaf | `db`, `http`, `results`, `text` | the metadata database (`barca-db`, re-exported), the HTTPS client, the result types a run and a plan return, text every layer formats the same way |
| 1 | storage | `backup`, `gc`, `ledger`, `pauses`, `pins`, `run_queue`, `store_inspect`, `store_sync`, `transfer`, `verified_copies` | what is kept on disk, in the store and in the tables one feature owns |
| 2 | decisions | `cache`, `changes`, `freshness`, `plan_reasons`, `predict`, `recover`, `schedule`, `sizing` | run hash, cache and freshness verdicts, why a step runs, what a run would do (the dry-run prediction), what it computes again because an artifact is gone, its pool size and capacity, when a schedule fires |
| 3 | engine | `commands`, `execution`, `incremental`, `mismatch`, `persist`, `telemetry` | executes a plan and records what happened |
| 3 | analysis | `causes`, `estimates`, `health`, `history_filter`, `key_query`, `liveness`, `partition_grid`, `refresh_path`, `rollup`, `run_explanation`, `run_list`, `schedule_standing`, `status`, `views` | read models over what was recorded |
| 4 | service | `diagnostics`, `envelope`, `outcome`, `queries`, `report`, `serve`, `sql`, `values` | one operation per command, its typed result, its text |

## The rule

A module may name a module of its own layer or of a layer with a lower rank. `engine` and
`analysis` share a rank and never name each other. Modules re-exported from the lower crates
(`crate::config`, `crate::dag`, `crate::time`, ...) are below all of this and are not checked:
Cargo already does.

The rule says nothing about edges inside one layer, so a cycle within a layer passes. The
size of the largest cycle is tracked beside it (below) so that those do not grow unseen.

## Where this differs from the plan's table

The table in #1056 came from a module-level scan. Four placements changed when the items
were read:

- **A leaf layer.** The plan asked for a leaf for formatting helpers. `text` is that leaf;
  `db` and `http` name no module of this crate and sit with it.
- **`results` is a leaf, not service.** It holds the types a run and a plan return
  (`GetResult`, `MultiResult`, `PlanResult`, `AssetSummary`, the re-exported step reports).
  The engine builds them and `schedule` reads `AssetSummary`, so above the engine it was a
  wrong-way edge from six modules. Its one upward import, `gc::CacheReport`, moved into it.
- **`run_queue` is a storage module of its own.** It was `serve::run_queue`, but it names only
  `db`: it is the queue's and `server_state`'s rows. `health`, `liveness` and
  `schedule_standing` read it for `barca status` on a machine that serves nothing.
  `serve::run_queue` still names it for `barca-server`.
- **`http` is listed** (leaf). The plan's table left it out; every module must have a layer.

## The test

`crates/barca-core/src/module_layering_tests.rs`, run by `cargo test -p barca-core`:

1. It lists the modules `lib.rs` declares outside `#[cfg(test)]`. One with no layer in
   `LAYERS` fails, and so does a `LAYERS` entry that `lib.rs` does not declare.
2. It lexes every non-test source file (test module files and `#[cfg(test)]` items are
   skipped, as in the write-boundary test) and reads `crate::m::item`,
   `use crate::m::{a, b}`, `use crate::{m, n::item}` and, where a file imports a module by
   name, its `m::item` paths. A file belongs to the module its path starts with. `super::`
   and `self::` paths stay inside one module and are not read.
3. An edge the rule forbids must be a line of
   `crates/barca-core/src/module_layering_allowlist.txt`, written
   `from -> to: item, item`. A new edge fails; a new item on a listed edge fails; a line or an
   item the source no longer has fails until it is deleted; a line the layers allow fails. So
   the file is exact, and it only shrinks.
4. `LARGEST_CYCLE` is the number of modules in the largest strongly connected component of
   the whole graph. It must equal what the scan finds: lower it when a cut shrinks the cycle,
   cut the new edge when a change grows it.

The fix for a failure is to move the item down a layer or to pass it in as a value. Adding a
line to the allowlist is not.

## Counts

| | wrong-way edges | largest cycle |
|---|---|---|
| v0.45.0, with the table above | 42 | 38 modules |
| after C1 | 16 | 29 modules |
| after C2 and C3 | 13 | 29 modules (see "The order inside analysis") |

Under the plan's original table (`results` in service, no leaf) the same v0.45.0 source has
more wrong-way edges, since every engine module that returns a result names `results`.

## What C1 moved

- `text` (new): `span`, `ago`, `utc_cell` from `report`; `shell_quote` from `envelope`;
  `human` (re-exported from `barca_model::cache_size`, where it already lived; `gc::size` is
  that module); `trace_since` from `status`. `report::utc_cell` and `envelope::shell_quote`
  still name them for the CLI.
- `results`: `CacheReport` from `gc`, `list_assets_from_dag` from `queries`, `KEY_SUMMARY` and
  `key_preview` from `report`. `gc::CacheReport` and `queries::list_assets_from_dag` still
  name them.
- `execution`: `RunOptions` and `RunCommand` from `outcome` (the engine's own input;
  `outcome` re-exports both) and `filter_plan_to_subgraph` from `queries` (its only caller).
- `commands::upload_pending_now` from `transfer`: it wraps `execution::upload_pending`, so it
  was the one reason `transfer` named the engine.
- `db::open_metadata` from `queries` (two calls into `barca-db`; `pauses` needed it), with
  `queries::open_metadata` still naming it. `history_filter::step_matches` from `queries`,
  likewise.
- `Trigger`, `TriggeredBy` and `rfc3339` are imported from `crate::trigger` and `crate::time`
  (barca-model), where they are defined, instead of through `outcome`'s re-exports.
- `status` uses `targets::short_name`; its private copy was the same function.

## What is left, and why

Each line of the allowlist, grouped by the item of the plan that removes it.

- **C4, stores below the engine.** `ledger -> cache`, `pins -> cache`, `store_inspect ->
  cache`, `store_sync -> cache` (the cache reader and its lookup sit in decisions and read
  the stores); `store_sync -> mismatch`, `store_sync -> recover`, `ledger -> persist`.
  `store_inspect -> status` (`NodeStatus`): the store check edits status rows in place, so
  cutting it means returning facts for `status` to apply, not moving a type.
- **C5, rendering.** `execution -> report` (9 items: the progress and end-of-run lines). `run_explanation -> outcome` (`RunView`) waits on the typed views
  of B3.
- **Needs a decision.** `ledger -> envelope` (`ErrorEnvelope`) and `ledger -> outcome`
  (`API_VERSION`): the ledger is a client of the server's HTTP API, so the question is where
  the wire vocabulary lives. `schedule -> queries` (`list_assets`): only
  `schedule::collect_jobs` uses it, and only a server test calls that; delete it or move it.

Inside one layer, so not on the allowlist, but part of the cycle:

- `gc <-> backup`: `gc` asks `backup::owned_local_file` which files are backups, and `backup`
  calls `gc::forget_survey` after a restore. Either the local-file layout or the survey has
  to move, and neither is a plain move.
- `outcome -> report`: `Outcome` renders through `report`. Splitting the run request from the
  rendered outcome is C5. (`outcome` names `serve::ServeHost` only in a doc comment.)
- `liveness` writes `server_state` (`record_serving`, `record_scheduler`, `heartbeat`). Those
  are the scheduler's records of itself, written by `barca-server` through these functions
  and read by `observe` in the same module: the record's format and its reader are one
  module on purpose. With `run_queue` in storage the edge is legal; splitting writer from
  reader is a C3 question.

`short_name` is not one function. `targets::short_name` (barca-compile) drops a partition
key and then the file (`p.py:a[k=1]` gives `a`); `step_trace::short_name` (barca-engine,
re-exported as `telemetry::short_name`) takes a base id and only drops the file, so a keyed
id keeps its key (`a[k=1]`). `report` passes ids that may carry a key to the second one and
prints the result, so merging them would change output. They stay two.

## What C2 moved: decisions out of the engine

A read-only command asks what a run would do and why. That is a decision, and it lived in the
run orchestrator, so `status`, `partition_grid`, `queries` and `diagnostics` named `execution`.

- `predict` (new, decisions): `execution::predict` whole. `explain_dag`,
  `explain_dag_with_keys`, `HeldNodes`, and `filter_plan_to_subgraph`, which only the
  prediction calls.
- `sizing` (new, decisions): `Sizing`, `sizing`, `sizing_for`, `planned_weights` and
  `warn_over_capacity` from `execution`. `barca_core::sizing` (the function) is unchanged.
- `freshness::planned`: `pin_manual` and `Planned` from `execution::prepare`. A run and a
  prediction plan the `Manual` and pinned nodes through it.
- `cache::step_report`: `report_for`, `merge_partition_reports`, `stale_warning` and
  `env_report` from `report`. They turn a `Decision` into a `StepReport`, which is not
  rendering; with them in `report` the prediction would have named the service layer.
  `kind_str` is named where it is defined (`crate::model`).
- `recover` is a decisions module, not an engine one. It names `cache`, `freshness`, `db`,
  `results`, `store_sync` and `transfer`, nothing of the engine, and its rule (a cached step
  whose artifact is gone runs again) is the one a run applies and a dry run predicts.

`execution` calls the same functions when it runs. What the prediction still takes from the
engine is in `barca-engine`, the crate below: `dispatch` (expanding `partitions_from` steps,
the inputs a phase provides) and `run_host::LocalHost` (a run on its own shares no capacity).
Those are plan and host vocabulary rather than execution, and Cargo already orders them; they
would have to move to `barca-compile` or a crate of their own before a `barca-decide` crate
could sit below `barca-engine`.

## The order inside analysis (C3)

The read models imported each other in a cycle. They now have rows, and the test checks them
(`ANALYSIS_ROWS`): an analysis module names the analysis modules of the rows before its own,
never its own row or a later one. No exception is listed.

| Row | Modules |
|---|---|
| 0 | `views`, `history_filter`, `key_query`, `run_explanation` |
| 1 | `schedule_standing`, `partition_grid` |
| 2 | `liveness` |
| 3 | `refresh_path` |
| 4 | `estimates` |
| 5 | `causes` |
| 6 | `rollup` |
| 7 | `health`, `run_list` |
| 8 | `status` |

Three chains share the rows: `views -> schedule_standing -> liveness -> refresh_path ->
estimates -> causes -> rollup -> health -> status`; `run_explanation -> run_list` (which also
reads `rollup`); `key_query -> partition_grid`.

What moved:

- `views` (new): the types every read model shares and nothing that computes one.
  `NodeStatus`, `StatusResult`, `StatusFilter`, `StatusSummary`, `CacheStatus`,
  `PartitionState`, `LastMaterialization` from `status`; `Health`, `HealthReason`,
  `DataSignal`, `Cause`, `NoEstimate`, `RootKind`, `Root`, `DataCoverage`, `LastCheck`,
  `Check`, `Checks`, `last_attempt_failed` and `recorded_at` from `health`; `ScheduleStatus`
  from `status`, `ScheduleState` from `schedule_standing`, `RefreshPath` from `refresh_path`;
  `RollupEntry`, `ActionKind` and the counts an entry sums from `rollup`. Each module still
  exports the types it defined (`health::Health`, `status::NodeStatus`, ...), so no caller
  changed.
- `Evidence` to `estimates`, its first user: it holds `refresh_path::PathEvidence` and
  `estimates::Timing`, so it cannot sit in the leaf without both. `health::Evidence` names it.
- `PlannedRun`, `QueueRun`, `queue_starts` and `typical_run_seconds` to `liveness`, where
  `recorded_timing` reads them; `estimates` still exports them.
- `ScheduleState::of`, `ScheduleState::detail` and `ScheduleStatus::of` stay in (or move to)
  `schedule_standing`, which has the `Standing` they read.
- `rollup::rollup` (the entry point of `barca status --rollup`: status rows, then health, then
  the tree) to `status::rollup`. `barca-cli` calls it there.
- `target_ids` and `target_line` to `run_explanation`; `run_list` still exports them.
- The grid's types (`PartitionGrid`, `GridCounts`, `GridDimension`, `state`) to `key_query`,
  which is queries over a grid and names nothing else; `partition_grid`, which builds one
  from the cache and the database, still exports them.

Where the order differs from the plan's (`... health -> status -> rollup`): `rollup` comes
before `health`. `health::attach`, the one path every host classifies through
(`Inputs::assemble`), also sums the scope's root entry into the result, and `status::status`
calls it. So the rollup's sums are below health, and what tied `rollup` back to `health` and
`status` (the types, `last_attempt_failed`, the `--rollup` entry point) moved instead.

The largest cycle is still 29 modules: `predict` joined it and `key_query` left. Nothing in
it is held together by an edge inside analysis any more. Every path from a lower layer back
up goes through an allowlisted edge (`schedule -> queries`, `ledger -> outcome`,
`execution -> report`, `store_inspect -> status`, `run_explanation -> outcome`, ...), and
with those 13 edges gone the largest cycle would be `cache`, `changes`, `freshness` and
`plan_reasons`, inside decisions. So the number falls with C4, C5 and the two decisions
listed above, not with more moves here.
