// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

pub mod backup;
pub mod batch;
pub mod cache;
pub(crate) mod changes;
pub mod commands;
pub(crate) mod coordinator;
pub(crate) mod cost;
mod dataset_sinks;
pub mod diagnostics;
pub mod dispatch;
pub mod envelope;
pub(crate) mod execution;
pub mod freshness;
pub mod gc;
pub mod helper_proc;
pub mod history_filter;
pub mod http;
mod incremental;
pub mod interrupt;
pub(crate) mod io_loop;
pub mod ledger;
mod mismatch;
pub mod outcome;
pub mod partition_grid;
pub mod pauses;
mod persist;
pub mod pins;
pub(crate) mod protocol;
pub mod queries;
mod recover;
pub mod report;
mod result_journal;
pub mod results;
pub(crate) mod run_host;
pub mod schedule;
pub mod serve;
#[cfg(test)]
mod source_scan;
pub mod sql;
pub mod status;
pub(crate) mod step_trace;
pub mod store_inspect;
mod store_sync;
pub mod telemetry;
pub mod transfer;
pub mod values;

// The model layer lives in `barca-model` (#905 step 4); its modules and root items keep their
// `barca_core::` paths.
#[doc(hidden)]
pub use barca_model::artifact_format;
pub use barca_model::cache_size;
pub use barca_model::cache_state;
pub use barca_model::config;
pub use barca_model::dag_error;
pub(crate) use barca_model::envdeps;
pub(crate) use barca_model::events;
pub use barca_model::hash;
pub use barca_model::jsonlog;
pub(crate) use barca_model::keyed;
pub use barca_model::layout;
pub(crate) use barca_model::ledger_post;
pub use barca_model::lines;
pub use barca_model::model;
pub use barca_model::model::*;
pub(crate) use barca_model::node_estimate;
pub(crate) use barca_model::process;
pub use barca_model::reports;
use barca_model::source_observation;
use barca_model::spawn_lock;
pub use barca_model::step_result;
pub use barca_model::step_status;
pub use barca_model::suggest;
pub use barca_model::term;
pub use barca_model::test_hooks;
pub use barca_model::time;
pub(crate) use barca_model::trace_ids;
pub(crate) use barca_model::trigger;
pub(crate) use barca_model::uri;
pub use barca_model::{
    BarcaError, CancellationToken, FailedStep, LogStream, PartialRun, RunEvent, errln, outln,
    release_free_memory, run_line, safe_node_id,
};

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
pub use barca_db::db;
use barca_db::metadata_size;
pub use barca_db::pending_uploads;
pub use barca_db::run_logs;
use barca_db::run_owner;
use barca_db::sink_publications;
pub(crate) use barca_db::state_validate;

pub use dag::Dag;
pub use execution::sizing;

#[cfg(test)]
mod layering_tests;
#[cfg(test)]
mod selection_tests;
#[cfg(test)]
mod step_status_literals_tests;
#[cfg(test)]
mod write_boundary_tests;

mod wire;

mod observation;
