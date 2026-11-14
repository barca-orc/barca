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
//!   `telemetry`, and `serve` (what exists only for `barca serve`: run queue, capacity, claims,
//!   `record_unstarted_run`); `sizing` from the run orchestrator.
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

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

pub mod backup;
pub mod cache;
pub(crate) mod changes;
pub mod commands;
pub mod db;
pub mod diagnostics;
pub mod envelope;
pub(crate) mod execution;
pub mod freshness;
pub mod gc;
pub mod history_filter;
pub mod http;
mod incremental;
pub mod ledger;
mod mismatch;
pub mod outcome;
pub mod partition_grid;
pub mod pauses;
mod persist;
pub mod pins;
pub mod queries;
mod recover;
pub mod report;
pub mod results;
pub mod schedule;
pub mod serve;
#[cfg(test)]
mod source_scan;
pub mod sql;
pub mod status;
pub(crate) mod store_inspect;
mod store_sync;
pub mod telemetry;
pub mod transfer;
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
pub(crate) use barca_model::ledger_post;
pub use barca_model::lines;
pub use barca_model::model;
pub(crate) use barca_model::model::*;
pub use barca_model::model::{CronExpr, Freshness, NodeKind, StepId};
pub(crate) use barca_model::node_estimate;
pub(crate) use barca_model::process;
pub(crate) use barca_model::reports;
use barca_model::source_observation;
pub use barca_model::step_status;
pub use barca_model::suggest;
pub use barca_model::term;
pub use barca_model::test_hooks;
pub use barca_model::time;
pub(crate) use barca_model::trace_ids;
pub(crate) use barca_model::trigger;
pub(crate) use barca_model::uri;
pub use barca_model::{
    BarcaError, CancellationToken, FailedStep, LogStream, RunEvent, errln, outln,
    release_free_memory,
};
pub(crate) use barca_model::{PartialRun, run_line, safe_node_id};

// The compile layer lives in `barca-compile` (#905 step 4); the modules callers of `barca-core`
// name keep their `barca_core::` paths.
pub use barca_compile::dag;
pub use barca_compile::discover;
pub use barca_compile::groups;
pub use barca_compile::load;
pub use barca_compile::parse;
pub(crate) use barca_compile::planner;
pub use barca_compile::selection;
pub use barca_compile::targets;
pub use barca_compile::warnings;

// The metadata database layer lives in `barca-db` (#905 step 4); its modules keep their
// `barca_core::` paths.
use barca_db::metadata_size;
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
pub use barca_engine::dispatch;
pub use barca_engine::helper_proc;
pub use barca_engine::interrupt;
pub(crate) use barca_engine::io_loop;
use barca_engine::observation;
pub(crate) use barca_engine::protocol;
pub(crate) use barca_engine::run_host;
pub(crate) use barca_engine::step_trace;

pub use execution::sizing;

#[cfg(test)]
mod helper_launch_tests;
#[cfg(test)]
mod layering_tests;
#[cfg(test)]
mod selection_tests;
#[cfg(test)]
mod step_status_literals_tests;
#[cfg(test)]
mod write_boundary_tests;
