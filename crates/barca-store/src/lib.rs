//! `barca-store`: the storage layer (#1056, C6). What is kept on disk, in the artifact store and
//! in the tables one feature owns: artifact transfer (`transfer`, `store_sync`,
//! `verified_copies`), what the store and this machine hold at a path (`store_inspect`), the
//! cache limit and what `.barca/` may drop (`gc`), metadata backups (`backup`), a server's
//! ledger and the reader a cache decision asks (`ledger`), pinned inputs (`pins`), pauses
//! (`pauses`) and the run queue's rows (`run_queue`).
//!
//! With them, the leaf types every layer above shares: the result types a run and a plan return
//! (`results`), the error envelope and the API version (`envelope`), text formatted one way
//! (`text`) and the HTTPS client (`http`). They are here and not in `barca-model` because they
//! name the graph's and the plan's types and the HTTP client.
//!
//! It sits above `barca-db` and `barca-proc` and never names `barca-engine`: a helper is started
//! through `barca_proc::helper_proc`. Its modules are in rows (`layering_tests.rs`).

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
