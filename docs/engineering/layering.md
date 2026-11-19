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
| 2 | decisions | `cache`, `changes`, `freshness`, `plan_reasons`, `schedule` | run hash, cache and freshness verdicts, why a step runs, when a schedule fires |
| 3 | engine | `commands`, `execution`, `incremental`, `mismatch`, `persist`, `recover`, `telemetry` | executes a plan and records what happened |
| 3 | analysis | `causes`, `estimates`, `health`, `history_filter`, `key_query`, `liveness`, `partition_grid`, `refresh_path`, `rollup`, `run_explanation`, `run_list`, `schedule_standing`, `status` | read models over what was recorded |
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
- **C4, stores below the engine.** `ledger -> cache`, `pins -> cache`, `store_inspect ->
  cache`, `store_sync -> cache` (the cache reader and its lookup sit in decisions and read
  the stores); `store_sync -> mismatch`, `store_sync -> recover`, `ledger -> persist`.
  `store_inspect -> status` (`NodeStatus`): the store check edits status rows in place, so
  cutting it means returning facts for `status` to apply, not moving a type.
- **C5, rendering.** `execution -> report` (13 items: the progress and end-of-run lines) and
  `incremental -> report`. `run_explanation -> outcome` (`RunView`) waits on the typed views
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
