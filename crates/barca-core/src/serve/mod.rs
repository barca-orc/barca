//! What exists only for `barca serve`: the durable run queue, the capacity its runs share
//! (#628), the asset steps they share while in flight (#654) and the server's auth mode.
//! A `barca get` or `barca run` on its own uses none of it.

mod auth;
pub mod capacity;
pub mod claims;
pub mod run_queue;

pub use auth::AuthMode;
