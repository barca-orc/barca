//! `barca-python`: the Python frontend (#1056, D2 and C6). Project files in, a graph out: find
//! the files (`discover`), read decorated functions from source with ruff's parser and no
//! import (`parse`, `decorators`, `decorator_args`, `arg_values`, `sink_args`, `batch_spec`,
//! `input_params`, `groups`), compute each definition's fingerprint (`definition`, `cone`,
//! `project_modules`), resolve references and hand the result to `Dag::from_parts` (`link`,
//! `node_refs`), and drive all of it, with dynamic partition keys evaluated (`load`).
//!
//! It names `barca-model` and no other barca crate: the graph (`barca_model::dag`) and its node
//! types (`barca_model::ir`) are there, and planning (`barca-compile`) is beside this crate and
//! reads the graph, never the frontend. A second frontend would be a crate like this one
//! (`docs/engineering/ir.md`). Its modules are in rows (`layering_tests.rs`).

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

#[doc(hidden)]
pub mod arg_values;
#[doc(hidden)]
pub mod batch_spec;
#[doc(hidden)]
pub mod cone;
#[doc(hidden)]
pub mod decorator_args;
#[doc(hidden)]
pub mod decorators;
#[doc(hidden)]
pub mod definition;
pub mod discover;
#[doc(hidden)]
pub mod env_reads;
#[doc(hidden)]
pub mod extracted;
#[doc(hidden)]
pub mod file_names;
pub mod groups;
#[doc(hidden)]
pub mod input_params;
#[doc(hidden)]
pub mod link;
pub mod load;
#[doc(hidden)]
pub mod node_refs;
pub mod parse;
#[doc(hidden)]
pub mod project_modules;
#[doc(hidden)]
pub mod sink_args;
#[doc(hidden)]
pub mod syntax_problem;
#[doc(hidden)]
pub mod unrelated_modules;
#[doc(hidden)]
pub mod unused_inputs;

#[cfg(test)]
mod layering_tests;
