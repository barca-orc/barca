//! `barca-analysis` (#1056, C6).

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
