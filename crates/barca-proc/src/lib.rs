//! `barca-proc`: the process layer (#1056, C6). How barca starts and stops a child process
//! (`helper_proc`: the Python helpers and the workers, one spawn at a time, shielded from the
//! terminal's Ctrl-C, on a lifeline) and the length-prefixed JSON frames a child and the
//! coordinator exchange over a Unix socket (`protocol`: framing, the hello, the worker's and
//! the transfer helper's messages, the socket path).
//!
//! It names `barca-model` and nothing else of barca, so the engine (`barca-engine`, which
//! starts workers) and the storage layer (`barca-store`, which starts the transfer and state
//! helpers) both sit above it and neither names the other.

// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod helper_proc;
#[doc(hidden)]
pub mod protocol;
