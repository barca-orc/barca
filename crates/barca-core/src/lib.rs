//! `barca-core`: the one crate `barca-cli` and `barca-server` import (#905). It wires the
//! engine (`barca-engine`) to the metadata database (`barca-db`) over the compiled DAG
//! (`barca-compile`) and the shared types (`barca-model`), and owns cache policy, gc, backups,
//! transfer, the ledger, status, reports and queries. Its `pub` surface is what those two
//! callers (and this crate's own integration tests) use, and nothing else:
//!
//! - **Commands and their results** (this crate): `commands` (run a pipeline), `queries`
//!   (plan, history, logs, stats, `open_metadata`), `status`, `report`, `results`, `values`,
//!   `sql`, `gc`, `backup`, `transfer`, `ledger`, `freshness`, `pins`, `pauses`, `schedule`,
//!   `partition_grid`, `history_filter`, `diagnostics`, `envelope`, `outcome`, `cache`, `http`,
//!   `telemetry`, `text` (sizes, spans and times as text), `run_queue` (the server's queue
//!   rows) and `serve` (what exists only for `barca serve`: capacity, claims,
//!   `record_unstarted_run`); `sizing`, the pool size and capacity of a run.
//! - **Metadata reads and the server's own writes**: `db`, a listed subset of `barca_db::db`
//!   (see that module). An operation of several database calls is a core function above, not a
//!   sequence in a caller.
//! - **Shared types** (`barca-model`): `config`, `model` and the root types `BarcaError`,
//!   `CancellationToken`, `CronExpr`, `FailedStep`, `Freshness`, `LogStream`, `NodeKind`,
//!   `RunEvent`, `StepId`, `release_free_memory`; `artifact_format`, `cache_state`, `hash`,
//!   `jsonlog`, `lines`, `step_status`, `suggest`, `time`, `term` with `outln!`/`errln!`, and
//!   `test_hooks`.
//! - **Source to DAG** (`barca-compile`): `dag`, `discover`, `groups`, `load`, `parse`,
//!   `selection`, `targets`, `warnings`.
//! - **Engine pieces callers name** (`barca-engine`): `batch`, `dispatch`, `helper_proc`,
//!   `interrupt`; and `run_logs` (`barca-db`), the spool `barca logs -f` follows.
//!
//! Everything else of the lower crates is re-exported `pub(crate)` or not at all: callers do
//! not depend on `barca-model`, `barca-compile`, `barca-db` or `barca-engine` directly.
//!
//! The modules declared here are layered (`docs/engineering/layering.md`, #1056): leaf, storage,
//! decisions, engine beside analysis, service. `module_layering_tests.rs` checks every
//! `crate::<module>` path against that order; a new module gets a layer there.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

pub mod commands;
pub mod db;
pub mod diagnostics;
pub(crate) mod execution;
mod incremental;
mod mismatch;
pub mod outcome;
mod persist;
pub mod queries;
pub mod report;
pub mod serve;
pub mod served;
pub mod sql;
pub mod telemetry;
pub mod values;

// The model layer lives in `barca-model` (#905 step 4); its modules and root items keep their
// `barca_core::` paths.
#[doc(hidden)]
pub use barca_model::artifact_format;
pub use barca_model::cache_state;
pub use barca_model::config;
pub(crate) use barca_model::envdeps;
pub use barca_model::hash;
pub use barca_model::jsonlog;
pub(crate) use barca_model::keyed;
pub(crate) use barca_model::layout;
pub use barca_model::lines;
pub use barca_model::model;
pub(crate) use barca_model::model::*;
pub use barca_model::model::{CronExpr, Freshness, NodeKind, StepId};
pub(crate) use barca_model::node_estimate;
pub(crate) use barca_model::process;
pub use barca_model::run_explain;
use barca_model::source_observation;
#[cfg(test)]
use barca_model::source_scan;
pub use barca_model::step_status;
pub use barca_model::suggest;
pub use barca_model::term;
pub use barca_model::test_hooks;
pub use barca_model::time;
pub(crate) use barca_model::trace_ids;
pub(crate) use barca_model::trigger;
pub use barca_model::{
    BarcaError, CancellationToken, FailedStep, LogStream, RunEvent, errln, outln,
    release_free_memory,
};
pub(crate) use barca_model::{PartialRun, run_line, safe_node_id};

// The compile layer lives in `barca-compile` (#905 step 4); the modules callers of `barca-core`
// name keep their `barca_core::` paths.
pub use barca_compile::dag;
pub use barca_compile::discover;
pub use barca_compile::dispatch;
pub use barca_compile::groups;
pub use barca_compile::load;
pub use barca_compile::parse;
pub(crate) use barca_compile::planner;
pub use barca_compile::selection;
pub use barca_compile::targets;
pub use barca_compile::warnings;

// The metadata database layer lives in `barca-db` (#905 step 4); its modules keep their
// `barca_core::` paths.
pub(crate) use barca_db::pending_uploads;
pub use barca_db::run_logs;
use barca_db::run_owner;
use barca_db::sink_publications;
pub(crate) use barca_db::state_validate;

// The engine layer lives in `barca-engine` (#905 step 4); its modules keep their `barca_core::`
// paths.
pub use barca_engine::batch;
pub(crate) use barca_engine::coordinator;
pub(crate) use barca_engine::cost;
use barca_engine::dataset_sinks;
pub use barca_engine::interrupt;
pub(crate) use barca_engine::io_loop;
use barca_engine::observation;
pub(crate) use barca_engine::run_host;
pub(crate) use barca_engine::step_trace;

// The process layer lives in `barca-proc` (#1056, C6).
pub use barca_proc::helper_proc;
pub(crate) use barca_proc::protocol;

pub use sizing::sizing;

#[cfg(test)]
mod helper_launch_tests;
#[cfg(test)]
mod ir_roundtrip_tests;
#[cfg(test)]
mod layering_tests;
#[cfg(test)]
mod module_layering_tests;
#[cfg(test)]
mod selection_tests;
#[cfg(test)]
mod step_status_literals_tests;
#[cfg(test)]
mod ui_fixture_tests;
#[cfg(test)]
mod write_boundary_tests;

// The store layer lives in `barca-store` (#1056, C6).
pub use barca_store::backup;
pub use barca_store::envelope;
pub use barca_store::gc;
pub use barca_store::http;
pub use barca_store::ledger;
pub use barca_store::pauses;
pub use barca_store::pins;
pub use barca_store::results;
pub use barca_store::run_queue;
pub(crate) use barca_store::store_inspect;
use barca_store::store_sync;
pub use barca_store::text;
pub use barca_store::transfer;

// The decide layer lives in `barca-decide` (#1056, C6).
pub use barca_decide::cache;
pub(crate) use barca_decide::changes;
pub use barca_decide::freshness;
pub(crate) use barca_decide::plan_reasons;
pub(crate) use barca_decide::predict;
use barca_decide::recover;
pub use barca_decide::schedule;
pub(crate) use barca_decide::sizing;

// The analysis layer lives in `barca-analysis` (#1056, C6).
pub use barca_analysis::causes;
pub use barca_analysis::estimates;
pub use barca_analysis::health;
pub use barca_analysis::history_filter;
pub use barca_analysis::key_query;
pub use barca_analysis::liveness;
pub use barca_analysis::partition_grid;
pub use barca_analysis::refresh_path;
pub use barca_analysis::rollup;
pub use barca_analysis::run_explanation;
pub use barca_analysis::run_list;
pub use barca_analysis::schedule_standing;
pub use barca_analysis::status;
pub(crate) use barca_analysis::views;
