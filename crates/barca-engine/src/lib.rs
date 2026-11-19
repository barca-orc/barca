//! The engine layer of barca (#905): plan in, results out. The coordinator's ready queue, the
//! worker pool and its I/O loop, the wire tables, batched calls, measured cost, dataset sinks
//! and the trace context each step carries. It depends on `barca-model`, `barca-compile` (the
//! plan), `barca-proc` (processes and the socket protocol) and, for `batch` alone,
//! `barca-python` (a batched step's labels are evaluated by the frontend); never on the
//! metadata database. `barca-run` drives it.

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
