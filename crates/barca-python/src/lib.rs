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
