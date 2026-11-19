//! The compile layer of barca (#905): source in, `Dag` and `ExecutionPlan` out. Parsing (ruff's
//! AST, no imports), the DAG, run-hash cones, targets, partition selection and planning. It
//! depends on `barca-model` only; `barca-service` re-exports it, so its items keep their
//! `barca_service::` paths.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

pub mod dispatch;
#[cfg(test)]
mod layering_tests;
#[doc(hidden)]
pub mod partition_selection;
#[doc(hidden)]
pub mod planner;
pub mod selection;
pub mod targets;
pub mod warnings;
