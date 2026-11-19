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
| 0 | leaf | `db`, `envelope`, `http`, `results`, `text` | the metadata database (`barca-db`, re-exported), the error envelope and the API version a client and the server share, the HTTPS client, the result types a run and a plan return, text every layer formats the same way |
| 1 | storage | `backup`, `gc`, `ledger`, `pauses`, `pins`, `run_queue`, `store_inspect`, `store_sync`, `transfer`, `verified_copies` | what is kept on disk, in the store and in the tables one feature owns |
| 2 | decisions | `cache`, `changes`, `freshness`, `plan_reasons`, `schedule` | run hash, cache and freshness verdicts, why a step runs, when a schedule fires |
| 3 | engine | `commands`, `execution`, `incremental`, `mismatch`, `persist`, `recover`, `telemetry` | executes a plan and records what happened |
| 3 | analysis | `causes`, `estimates`, `health`, `history_filter`, `key_query`, `liveness`, `partition_grid`, `refresh_path`, `rollup`, `run_explanation`, `run_list`, `schedule_standing`, `status` | read models over what was recorded |
| 4 | service | `diagnostics`, `outcome`, `queries`, `report`, `serve`, `sql`, `values` | one operation per command, its typed result, its text |

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
| after C4 (stores) | 6 | 19 modules |

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

- **C2, decisions out of `execution`.** `status -> execution` and `partition_grid ->
  execution` (`explain_dag_with_keys`, `HeldNodes`).
- **C5, rendering.** `execution -> report` (13 items: the progress and end-of-run lines) and
  `incremental -> report`. `run_explanation -> outcome` (`RunView`) waits on the typed views
  of B3.
- **Needs a decision.** `schedule -> queries` (`list_assets`): only
  `schedule::collect_jobs` uses it, and only a server test calls that; delete it or move it.

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
