//! The engine layer of barca (#905): plan in, results out. The coordinator's ready queue, the
//! worker pool and its I/O loop, the Unix-socket protocol and its wire tables, batched calls,
//! measured cost, dataset sinks and the trace context each step carries. It depends on
//! `barca-model` and `barca-compile` only (no metadata database); `barca-service` re-exports each
//! module, so its items keep their `barca_service::` paths.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod batch;
#[doc(hidden)]
pub mod coordinator;
#[doc(hidden)]
pub mod cost;
#[doc(hidden)]
pub mod dataset_sinks;
pub mod interrupt;
#[doc(hidden)]
pub mod io_loop;
#[doc(hidden)]
pub mod observation;
#[doc(hidden)]
pub mod result_journal;
#[doc(hidden)]
pub mod run_host;
mod step_json;
#[doc(hidden)]
pub mod step_trace;
#[doc(hidden)]
pub mod wire;
