//! `barca-analysis`: the analysis layer (#1056, C6). Read models over what was recorded: a
//! node's cache state (`status`), its health, cause and root (`health`, `causes`), when it is
//! fresh again (`estimates`, `refresh_path`, `liveness`, `schedule_standing`), the sums of
//! `barca status --rollup` (`rollup`), runs in words (`run_list`, `run_explanation`), a
//! partitioned node's keys (`partition_grid`, `key_query`), and the types they share (`views`).
//!
//! A read model reads and starts nothing: this crate sits above `barca-decide`, beside
//! `barca-run`, and names neither `barca-run` nor `barca-engine`. It takes a graph
//! (`barca_model::dag::Dag`) and loads none; the commands that start from files are
//! `barca_service::loaded`. Its modules are in rows (`layering_tests.rs`).

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod causes;
pub mod estimates;
pub mod health;
pub mod history_filter;
pub mod key_query;
#[cfg(test)]
mod layering_tests;
pub mod liveness;
pub mod partition_grid;
pub mod refresh_path;
pub mod rollup;
pub mod run_explanation;
pub mod run_list;
pub mod schedule_standing;
pub mod status;
#[doc(hidden)]
pub mod views;
