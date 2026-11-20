# Layers inside barca-core (#1056 track C)

Status: done, 2026-11-19: every layer below is a crate of its own, and `barca-core` is gone
([crate-split-2.md](crate-split-2.md) has the crates, their graph and the tests that replaced
the one described here). This page is the record of how the modules were layered first; where
it says `barca-core`, read the crate that holds the module now. Non-normative. The plan is
[#1056](https://github.com/barca-orc/barca/issues/1056); the crate graph above this is in
[crate-split.md](crate-split.md).

The crates point one way, but inside `barca-core` 38 of the 45 modules formed one dependency
cycle: each could reach every other through `crate::` paths in non-test code. A layer cannot
become a crate while that holds, so the modules get layers first and a test that keeps the
count of wrong-way edges from growing.

## The layers

| Rank | Layer | Modules | Holds |
|---|---|---|---|
| 0 | leaf | `db`, `envelope`, `http`, `results`, `text` | the metadata database (`barca-db`, re-exported), the error envelope and the API version a client and the server share, the HTTPS client, the result types a run and a plan return, text every layer formats the same way |
| 1 | storage | `backup`, `gc`, `ledger`, `pauses`, `pins`, `run_queue`, `store_inspect`, `store_sync`, `transfer`, `verified_copies` | what is kept on disk, in the store and in the tables one feature owns |
| 2 | decisions | `cache`, `changes`, `freshness`, `plan_reasons`, `predict`, `recover`, `schedule`, `sizing` | run hash, cache and freshness verdicts, why a step runs, what a run would do (the dry-run prediction), what it computes again because an artifact is gone, its pool size and capacity, when a schedule fires |
| 3 | engine | `commands`, `execution`, `incremental`, `mismatch`, `persist`, `telemetry` | executes a plan and records what happened |
| 3 | analysis | `causes`, `estimates`, `health`, `history_filter`, `key_query`, `liveness`, `partition_grid`, `refresh_path`, `rollup`, `run_explanation`, `run_list`, `schedule_standing`, `status`, `views` | read models over what was recorded |
| 4 | service | `diagnostics`, `outcome`, `queries`, `report`, `serve`, `served`, `sql`, `values` | one operation per command, its typed result, its text |

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
- **`envelope` is a leaf, not service** (C4). It names no module of this crate but `text`: it is
  the error a command ends with, as data, and the wire form of that error. The ledger client
  (storage) reads it from the server's answers, so in service it was a wrong-way edge.
  `API_VERSION` moved into it from `outcome` for the same reason; `outcome::API_VERSION` still
  names it for the CLI and the server.

## The order inside storage

The storage layer has rows of its own (`STORAGE_ROWS` in the test, checked by
`storage_modules_name_only_the_storage_rows_above_their_own`). A storage module names only
storage modules of the rows above its own, so the layer has no cycle and no allowlist.

| Row | Modules | Names, of storage |
|---|---|---|
| 0 | `pauses`, `run_queue`, `transfer`, `verified_copies` | nothing |
| 1 | `backup`, `store_sync` | `transfer`; `store_sync` also `verified_copies` |
| 2 | `gc`, `store_inspect` | `backup`, `transfer` |
| 3 | `ledger` | `store_inspect`, `transfer` |
| 4 | `pins` | `ledger` (the cache reader), `store_inspect` |

## The test

`crates/barca-service/src/module_layering_tests.rs`, run by `cargo test -p barca-core`:

1. It lists the modules `lib.rs` declares outside `#[cfg(test)]`. One with no layer in
   `LAYERS` fails, and so does a `LAYERS` entry that `lib.rs` does not declare.
2. It lexes every non-test source file (test module files and `#[cfg(test)]` items are
   skipped, as in the write-boundary test) and reads `crate::m::item`,
   `use crate::m::{a, b}`, `use crate::{m, n::item}` and, where a file imports a module by
   name, its `m::item` paths. A file belongs to the module its path starts with. `super::`
   and `self::` paths stay inside one module and are not read.
3. An edge the rule forbids must be a line of
   `crates/barca-service/src/module_layering_allowlist.txt`, written
   `from -> to: item, item`. A new edge fails; a new item on a listed edge fails; a line or an
   item the source no longer has fails until it is deleted; a line the layers allow fails. So
   the file is exact, and it only shrinks.
4. `LARGEST_CYCLE` is the number of modules in the largest strongly connected component of
   the whole graph. It must equal what the scan finds: lower it when a cut shrinks the cycle,
   cut the new edge when a change grows it.

The fix for a failure is to move the item down a layer or to pass it in as a value. Adding a
line to the allowlist is not.

## The typed views (B2, B3)

`served` is a service module: the structs a host answers with and the functions that compute
them, one definition per view. It imports analysis types (`refresh_path::ScheduleInputs`,
`liveness::RecordedTiming`, `run_list::RunNow`) and moves none of them.

| View | Computed by | A server adds |
|---|---|---|
| one run (`RunView`) | `served::run_view`, then `RunDraft::plan` over the graph the host has | its run in memory (`LiveRun`) and the events it streamed |
| the run list (`RunList`) | `served::run_list` over `queries::history_at`; `served::in_flight` for the runs in flight | its runs in memory; which nodes each run in flight works on |
| the queue (`QueueView`) | `served::recorded_queue` for the queue the database records; the live queue is the server's own | each row's decoded description (`RecordedRun`) |
| a schedule (`ScheduleJob`) | `served::schedule_jobs` over `refresh_path::schedule_views` | its scheduler's last skipped tick (`JobSkips`) |
| pauses, pins | `Pauses::of`, `Pins` over `pauses::list` and `pins::list` | nothing |
| a server's health, `GET /project`, `GET /auth/status` | filled by the host; nothing is computed | every field |

`barca history RUN_ID` calls `run_view` with no overlay, so a recorded run is the same run
from either host (`python/tests/test_runs_parity.py`). `barca history` and `GET /history` print
`history_at`'s rows as they are; `GET /runs` puts the overlay on top of the same rows.

`views/wire_tests.rs` compares each struct that replaced a `json!` body with that body, byte
for byte: `serde_json` writes a `json!` object's keys sorted, so those structs declare their
fields in sorted order.

Where two shapes of one concept still differ, on purpose (typed as served, not unified):

- A schedule's times are unix seconds in `ScheduleJob` (`GET /schedule`) and RFC 3339 in
  `status::ScheduleStatus` (the `schedule` block of `GET /state`); `ScheduleJob` also has
  `last_run`, the older name of `last_run_id`.
- A run in a list is a `RunSummary` in `GET /runs` (`id`, `first_failure`, times in RFC 3339)
  and a `db::RunRecord` in `barca history --json` and `GET /history` (`run_id`, `failure`,
  times as stored).
- One run is flat in `GET /runs/{run_id}` (`RunView`) and `{run, steps, steps_page,
  explanation}` in `barca history RUN_ID --json` (`queries::RunDetail`).

## Counts

| | wrong-way edges | largest cycle |
|---|---|---|
| v0.45.0, with the table above | 42 | 38 modules |
| after C1 | 16 | 29 modules |
| after C4 (stores) | 6 | 19 modules |
| after C2 and C3 | 3 | 16 modules |
| after C5 (rendering) | 1 | 5 modules |

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

- **B3, the typed views.** `run_explanation -> outcome` (`RunView`) is the one line left. It
  closes the last cycle (`outcome`, `queries`, `report`, `run_explanation`, `run_list`); with
  it gone no group of modules reaches itself.

Inside one layer, so not on the allowlist, but part of the cycle:

- `outcome -> report`: `Outcome` renders through `report`. Splitting the run request from the
  rendered outcome is C5. (`outcome` names `serve::ServeHost` only in a doc comment.)
- `liveness` writes `server_state` (`record_serving`, `record_scheduler`, `heartbeat`). Those
  are the scheduler's records of itself, written by `barca-server` through these functions
  and read by `observe` in the same module: the record's format and its reader are one
  module on purpose. With `run_queue` in storage the edge is legal; splitting writer from
  reader is a C3 question.

## What C4 moved

No storage module names a higher layer, and none is in the cycle any more.

- **The cache reader is the ledger's.** `CacheReader` (this machine's history, or the answers
  a server's ledger gave) and `lookup_cached` moved from `cache` to `ledger::reader`. The
  reader holds a `Ledger`, and the server answers `POST /ledger/lookup` with the same lookup,
  so `cache -> ledger -> cache` was a cycle of two; `cache::{CacheReader, lookup_cached}`
  still name them for the layers above. `pins` reads through `ledger::CacheReader`.
- **`ledger` off `persist` and `cache::checks_bytes`.** `record_post` is `barca-db`'s
  (`recording`); `persist` only re-exported it. `checks_bytes` reads a node's declaration and
  nothing else, so it is `DagNode::checks_bytes` in `barca-model`, and `cache::checks_bytes`
  calls it.
- **`store_inspect` answers with facts.** `check_store_copies` and `read_shapes`, which choose
  the results to ask about (through the cache reader) and write the answer into `NodeStatus`
  rows, are `status::store_check` (a file of `status`, the code unchanged). `store_inspect`
  keeps the questions and their answers: `inspect_copies`, `classify`, `stat_objects`,
  `artifact_shapes`, `run_json_helper`. With no reader in it, it sits below `ledger`, which
  uses its `stat_objects`.
- **`store_sync` owns what it used from above.** `on_disk` (from `recover`), the text of a
  mismatched copy (`mismatch::describe`, now `store_sync::describe_mismatch`) and
  `BLOCKED_ARTIFACT_PATH` (from `cache`). `recover::on_disk` and
  `cache::BLOCKED_ARTIFACT_PATH` still name them.
- **`gc` names `backup`, not the reverse.** `backup` owns the layout of the backup
  directories, and `gc`, which accounts for everything under `.barca/`, asks it
  (`owned_local_file`). The file `gc` keeps its last walk in is named in `barca-model`'s
  `layout` (`GC_STATE_FILE`, `forget_gc_survey`), which `backup` calls after it replaces the
  database; `gc::forget_survey` is gone.

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

The largest cycle is 16 modules, down from 19: `partition_grid` left it, and `cache`,
`changes`, `freshness`, `plan_reasons`, `predict` and `recover` are not in it. No edge inside
analysis holds it together. Eight read models are still in it because of the three
allowlisted edges: `schedule -> queries` takes `liveness`, and so everything above it, back
up to the service layer, `execution -> report` does the same for the engine, and
`run_explanation -> outcome` for the run views. With those three gone no cycle crosses a
layer. So the number falls with C5 and the decision on `schedule::collect_jobs`, not with
more moves here.

## What C5 moved: rendering depends on values

`report` is the text and JSON renderer of the service layer. Nothing below the service layer
names it, and it names no function of `outcome`, `sql` or `values`.

- **A run's progress lines are the run's.** `execution::progress` (new, a file of `execution`):
  `fmt_eta`, `reconcile_total`, `env_suffix`, `RunOutcome`, `batch_line`, `end_of_run_line`,
  `cached_step_line` and `failed_step_line` from `report`. They are what the orchestrator
  prints while it executes (the bar, the `--agent` lines, the last line), not a view of a
  result, and no other module called them. The alternative, typed events that the service
  layer renders, would put a channel or a callback between a step finishing and its line on
  the hot path for no second consumer; the functions already return text and the caller emits
  it, so the move changes no byte and no timing.
- **`cache::step_report::computed_by_another_run`** from `report`: it rewrites the
  `StepReport` of a step another run of the server computed, beside `report_for`, which made
  it.
- **The JSON form of a result's output is the result's.** `results`: `artifact_metadata`,
  `pointer_output`, `keyed_output` (with its hint), `parse_key_summary` and the helper they
  share, from `report`; `first_multi_failure` (the error a multi-target run ends with) from
  `report::commands`; `SqlResult` from `sql`. They decide which keys a result lists, whether it
  is truncated and what a run's failure is, and the server stores `pointer_output` in a run's
  record. `report` and `sql` still export the names the CLI and the server use.
- **Reading an artifact is `values`.** `read_task_output` and `read_final_output` (a task's JSON
  results read from disk) from `report`. `values` no longer names `report`.
- `report` names `crate::trigger::TriggeredBy` where it is defined, not through `outcome`.
- `schedule::collect_jobs` is gone: only a test of `barca-server` called it, and that test now
  calls `queries::list_assets` and `schedule::jobs_from_summaries` itself.

What stays in `report` is formatting: tables and their column widths, the JSON envelope of a
page (`Page`, `--fields` projection), key order, hints and notes, and the rows of a batched
call in `barca history RUN_ID` (`history_rows` sums a call's keys for one table row; the JSON
lists every key). Two things in it are not formatting and wait on the typed views, which own
the files they would move to: `render_run_detail` calls `run_explanation::explain` with the
clock (the explanation belongs in `queries::RunDetail`), and `ListNode`, `HistoryItem` and
`HistoryExtras::of_server_runs` are wire types of `barca list` and `barca history`.

## What the lower layers take from barca-engine

The module scan reads `crate::<module>` paths of this crate. `crate::dispatch`,
`crate::helper_proc` and the other modules re-exported from `barca-engine` are below it for
Cargo, but a crate made of the leaf, storage, decisions or analysis layer must not depend on
the engine. `the_layers_below_the_engine_name_barca_engine_only_where_listed` reads those paths
too, and `crates/barca-service/src/engine_uses_allowlist.txt` is what is left (it only shrinks).
15 module pairs before, 7 now; the analysis layer names none.

Cut, because the item was a value:

- `OutputRef` and `ErrorPhase` were already `barca_model::step_result`'s; `envelope`,
  `results`, `ledger`, `cache`, `freshness`, `predict` and `recover` name them there
  (`crate::step_result`) instead of through `dispatch`.
- `ProvidedInput` moved to `barca_model::step_result` (`dispatch` re-exports it); `store_sync`
  and `recover` name it there.
- `sizing_for` takes the shared capacity as a value (`Option<(usize, &'static str)>`) instead
  of a `&dyn RunHost`: the run reads it from its host, a prediction passes `None`. So `sizing`
  and `predict` name no `run_host`.

Left, because it is behaviour:

| Layer | Module | Takes | To cut it |
|---|---|---|---|
| storage | `backup`, `pins`, `store_inspect`, `transfer` | `helper_proc` (start a `python -m barca._x` helper, its lifeline, its exit as an error) | `helper_proc` names only `barca-model` and tokio: it moves below the engine as it is (into `barca-model` beside `process` and `spawn_lock`, or a crate of its own) |
| storage | `transfer` | `protocol` (`FrameReader`, `encode_message`, `accept_hello`, `socket_path`, `PROTOCOL_VERSION`, `TransferRequest`, `TransferReply`) | the framing and the transfer messages are shared with the worker protocol; they move below with `helper_proc`, or `barca-store` depends on `barca-engine` |
| decisions | `predict` | `dispatch::{partition_sources, expand_pending_partitions_filtered, selected_source, build_provided_inputs}` | `dispatch` names only `barca-compile` and `barca-model` (it expands a planned phase); it moves to `barca-compile` beside `planner`, which `barca-decide` takes anyway |
| decisions | `recover` | `dispatch::build_provided_inputs` | the same |
