//! `barca-compile`: planning (#905, #1056). A graph in (`barca_model::dag`), an `ExecutionPlan`
//! out: the targets a command names (`targets`), `--partition` (`partition_selection`,
//! `selection`), the planner (`planner`), what a planned phase expands to when it runs
//! (`dispatch`: `partitions_from` keys, the inputs a phase provides) and plan-time warnings
//! (`warnings`). It depends on `barca-model` only and reads nothing of Python's: the frontend is
//! `barca-python`, a dev-dependency for tests that write a graph as source.

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
