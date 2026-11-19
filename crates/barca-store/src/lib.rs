//! `barca-store` (#1056, C6).

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod backup;
pub mod envelope;
pub mod gc;
pub mod http;
#[cfg(test)]
mod layering_tests;
pub mod ledger;
pub mod pauses;
pub mod pins;
pub mod results;
pub mod run_queue;
#[doc(hidden)]
pub mod store_inspect;
#[doc(hidden)]
pub mod store_sync;
pub mod text;
pub mod transfer;
#[doc(hidden)]
pub mod verified_copies;
