//! `barca-run`: the run orchestrator (#1056, C6). It executes a plan and records what happened:
//! `execution` (prepare, decide, dispatch a phase, finalize), `persist` (a run's rows, the
//! minute backup), `incremental` (publication and freshness proofs), `mismatch`, `commands`
//! (the get and run entry points) and `telemetry`.
//!
//! It is the one crate that drives `barca-engine` (the worker loop, which has no database) and
//! writes a run to the metadata database, with the decisions of `barca-decide` and the storage
//! of `barca-store`. It sits beside `barca-analysis` and names nothing of it.

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
