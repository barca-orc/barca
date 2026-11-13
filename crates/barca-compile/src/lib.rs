//! The compile layer of barca (#905): source in, `Dag` and `ExecutionPlan` out. Parsing (ruff's
//! AST, no imports), the DAG, run-hash cones, targets, partition selection and planning. It
//! depends on `barca-model` only; `barca-core` re-exports it, so its items keep their
//! `barca_core::` paths.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

#[doc(hidden)]
pub mod batch_spec;
#[doc(hidden)]
pub mod cone;
pub mod dag;
#[doc(hidden)]
pub mod decorator_args;
#[doc(hidden)]
pub mod definition;
pub mod discover;
#[doc(hidden)]
pub mod env_reads;
pub mod groups;
#[doc(hidden)]
pub mod input_params;
pub mod load;
pub mod parse;
#[doc(hidden)]
pub mod planner;
#[doc(hidden)]
pub mod project_modules;
pub mod selection;
#[doc(hidden)]
pub mod sink_args;
pub mod targets;
#[doc(hidden)]
pub mod unrelated_modules;
#[doc(hidden)]
pub mod unused_inputs;
pub mod warnings;

pub use dag::Dag;
