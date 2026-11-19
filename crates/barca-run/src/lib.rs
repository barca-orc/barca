//! `barca-run` (#1056, C6).

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod commands;
#[doc(hidden)]
pub mod execution;
#[doc(hidden)]
pub mod incremental;
#[cfg(test)]
mod layering_tests;
#[doc(hidden)]
pub mod mismatch;
#[doc(hidden)]
pub mod persist;
pub mod telemetry;
