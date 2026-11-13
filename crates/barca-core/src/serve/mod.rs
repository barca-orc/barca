//! What exists only for `barca serve`: the durable run queue, the capacity its runs share
//! (#628), the asset steps they share while in flight (#654) and the server's auth mode.
//! A `barca get` or `barca run` on its own uses none of it: the hot loop reaches what is
//! here only through [`crate::run_host::RunHost`], as [`ServeHost`].

pub mod capacity;
pub mod claims;
mod host;
pub mod run_queue;

pub use crate::config::AuthMode;
pub use host::ServeHost;
