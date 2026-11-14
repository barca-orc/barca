//! The metadata database layer of barca (#905): `.barca/metadata.db`, its schema and
//! migrations, every write to it (`db/`, one file per table family), and the modules that decide
//! about its rows (who owns a running run, what a restored copy may hold, sizes, sink
//! publications, recorded logs, uploads still owed). It depends on `barca-model` only;
//! `barca-core` re-exports each module, so its items keep their `barca_core::` paths.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod db;
#[doc(hidden)]
pub mod metadata_size;
pub mod pending_uploads;
#[doc(hidden)]
pub mod recording;
pub mod run_logs;
#[doc(hidden)]
pub mod run_owner;
#[doc(hidden)]
pub mod sink_publications;
#[doc(hidden)]
pub mod state_validate;
