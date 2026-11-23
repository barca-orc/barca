//! `barca-service`: the one crate `barca-cli` and `barca-server` import (#905, #1056). It is the
//! service layer, one operation per command with its typed result and its text, on top of the
//! layers that were modules of `barca-core` and are crates now:
//!
//! - **This crate**: `queries` (plan, history, logs, stats), `served` (the typed views a host
//!   answers with), `report` (text and JSON rendering), `outcome` (what a run request ends
//!   with), `sql`, `values`, `diagnostics`, `serve` (what exists only for `barca serve`:
//!   capacity, claims, `record_unstarted_run`), and `db`, a listed subset of `barca_db::db` (see
//!   that module). An operation of several database calls is a function here or below, not a
//!   sequence in a caller.
//! - **The run orchestrator** (`barca-run`): `commands` (run a pipeline), `telemetry`.
//! - **Read models** (`barca-analysis`): `status`, `health`, `causes`, `estimates`, `rollup`,
//!   `refresh_path`, `liveness`, `schedule_standing`, `run_list`, `run_explanation`,
//!   `partition_grid`, `key_query`, `history_filter`.
//! - **Decisions** (`barca-decide`): `cache`, `freshness`, `schedule`, and `sizing`, the pool
//!   size and capacity of a run.
//! - **Storage** (`barca-store`): `gc`, `backup`, `transfer`, `ledger`, `pins`, `pauses`,
//!   `run_queue` (the server's queue rows), and the leaf types `results`, `envelope`, `text`
//!   (sizes, spans and times as text) and `http`.
//! - **Shared types** (`barca-model`): `config`, `model` and the root types `BarcaError`,
//!   `CancellationToken`, `CronExpr`, `FailedStep`, `Freshness`, `LogStream`, `NodeKind`,
//!   `RunEvent`, `StepId`, `release_free_memory`; `artifact_format`, `cache_state`, `hash`,
//!   `jsonlog`, `lines`, `run_explain`, `step_status`, `suggest`, `time`, `term` with
//!   `outln!`/`errln!`, and `test_hooks`.
//! - **Source to DAG and plan** (`barca-compile`): `dag`, `discover`, `dispatch`, `groups`,
//!   `load`, `parse`, `selection`, `targets`, `warnings`.
//! - **Engine and process pieces callers name**: `batch`, `interrupt` (`barca-engine`),
//!   `helper_proc` (`barca-proc`); and `run_logs` (`barca-db`), the spool `barca logs -f`
//!   follows.
//!
//! Everything else of the lower crates is re-exported `pub(crate)` or not at all: the hosts do
//! not depend on a lower crate directly (`layering_tests.rs` checks the manifests). The lower
//! crates do not re-export each other: a module of `barca-run` names `barca_decide::cache`.
//! Only this crate keeps the old paths (`crate::cache`, `barca_service::cache`), for its own
//! modules and for the hosts.
//!
//! `docs/engineering/crate-split-2.md` has the crates, their order and what each may use.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

pub mod cached;
pub mod db;
pub mod diagnostics;
pub mod loaded;
pub mod outcome;
pub mod queries;
pub mod report;
pub mod serve;
pub mod served;
pub mod sql;
pub mod values;

// The model layer (`barca-model`): its modules and root items under this crate's paths.
#[cfg(test)]
pub(crate) use barca_model::PartialRun;
#[doc(hidden)]
pub use barca_model::artifact_format;
pub use barca_model::cache_state;
pub use barca_model::config;
pub use barca_model::hash;
pub use barca_model::jsonlog;
pub use barca_model::lines;
pub use barca_model::model;
pub use barca_model::model::{CronExpr, Freshness, NodeKind, StepId};
pub use barca_model::process;
pub use barca_model::run_explain;
#[cfg(test)]
use barca_model::source_scan;
pub use barca_model::step_status;
pub use barca_model::suggest;
pub use barca_model::term;
pub use barca_model::test_hooks;
pub use barca_model::time;
pub(crate) use barca_model::trigger;
pub use barca_model::{
    BarcaError, CancellationToken, FailedStep, LogStream, RunEvent, errln, outln,
    release_free_memory,
};

// The compile layer (`barca-compile`).
pub use barca_compile::dispatch;
pub(crate) use barca_compile::planner;
pub use barca_compile::selection;
pub use barca_compile::targets;
pub use barca_compile::warnings;
pub use barca_model::dag;
pub use barca_python::discover;
pub use barca_python::groups;
pub use barca_python::load;
pub use barca_python::parse;

// The metadata database (`barca-db`); `db` above is the listed subset of `barca_db::db`.
pub use barca_db::run_logs;
pub(crate) use barca_db::state_validate;

// The engine (`barca-engine`) and the process layer (`barca-proc`).
pub use barca_engine::batch;
pub use barca_engine::interrupt;
pub(crate) use barca_engine::run_host;
pub use barca_proc::helper_proc;

// The storage layer (`barca-store`).
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
pub use barca_store::text;
pub use barca_store::transfer;

// The decisions layer (`barca-decide`).
pub use barca_decide::cache;
pub(crate) use barca_decide::changes;
pub use barca_decide::freshness;
pub(crate) use barca_decide::predict;
pub use barca_decide::schedule;
pub(crate) use barca_decide::sizing;
pub use sizing::sizing;

// The analysis layer (`barca-analysis`).
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

// The run orchestrator (`barca-run`).
pub use barca_run::commands;
pub(crate) use barca_run::execution;
pub use barca_run::telemetry;

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
