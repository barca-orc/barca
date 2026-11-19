//! `barca-decide` (#1056, C6).

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod cache;
#[doc(hidden)]
pub mod changes;
pub mod freshness;
#[cfg(test)]
mod layering_tests;
#[doc(hidden)]
pub mod plan_reasons;
#[doc(hidden)]
pub mod predict;
#[doc(hidden)]
pub mod recover;
pub mod schedule;
#[doc(hidden)]
pub mod sizing;
