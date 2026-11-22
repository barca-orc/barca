//! `barca-decide`: the decisions layer (#1056, C6). The run hash and the cache verdict
//! (`cache`), freshness (`freshness`), what changed since the last success (`changes`), why a
//! step runs (`plan_reasons`), what a run would do (`predict`, the dry run), what it computes
//! again because an artifact is gone (`recover`), its pool size and capacity (`sizing`) and when
//! a schedule fires (`schedule`).
//!
//! A decision is the same whether a run executes it or a read-only command predicts it, so this
//! crate sits below both `barca-run` and `barca-analysis` and names neither, and never
//! `barca-engine`. It takes a graph (`barca_model::dag::Dag`) and loads none: the Python
//! frontend is a dev-dependency, for tests that write a graph as source.

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

pub mod evidence;

pub mod inspection;
