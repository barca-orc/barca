// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

pub mod backup;
pub mod batch;
pub mod cache;
pub(crate) mod changes;
pub mod commands;
pub(crate) mod cone;
pub(crate) mod coordinator;
pub(crate) mod cost;
pub mod dag;
mod dataset_sinks;
pub mod db;
pub(crate) mod decorator_args;
pub(crate) mod definition;
pub mod diagnostics;
pub mod discover;
pub mod dispatch;
pub(crate) mod env_reads;
pub mod envelope;
pub(crate) mod execution;
pub mod freshness;
pub mod gc;
pub mod groups;
pub mod helper_proc;
pub mod history_filter;
pub mod http;
mod incremental;
mod input_params;
pub mod interrupt;
pub(crate) mod io_loop;
pub mod ledger;
pub mod load;
mod metadata_size;
mod mismatch;
pub mod outcome;
pub mod parse;
pub mod partition_grid;
pub mod pauses;
pub mod pending_uploads;
mod persist;
pub mod pins;
pub(crate) mod planner;
pub(crate) mod project_modules;
pub(crate) mod protocol;
pub mod queries;
mod recover;
pub mod report;
mod result_journal;
pub mod results;
pub(crate) mod run_host;
pub mod run_logs;
mod run_owner;
pub mod schedule;
pub mod selection;
pub mod serve;
mod sink_args;
mod sink_publications;
#[cfg(test)]
mod source_scan;
pub mod sql;
pub(crate) mod state_validate;
pub mod status;
pub(crate) mod step_trace;
pub mod store_inspect;
mod store_sync;
pub mod targets;
pub mod telemetry;
pub mod transfer;
pub(crate) mod unrelated_modules;
pub(crate) mod unused_inputs;
pub mod values;
pub mod warnings;

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

pub use dag::Dag;
pub use execution::sizing;

#[cfg(test)]
mod layering_tests;
#[cfg(test)]
mod step_status_literals_tests;

mod wire;

mod observation;
