//! The leaf of the analysis layer: the view types every read model shares, and nothing that
//! computes one. A node's row ([`NodeStatus`]) and the result that carries the rows
//! ([`StatusResult`]), the words of its health, its schedule block and refresh path, and an
//! entry of the rollup. The modules that fill them (`schedule_standing`, `refresh_path`,
//! `estimates`, `causes`, `rollup`, `health`, `status`) each name this module and the ones
//! before them, never one after (`docs/engineering/layering.md`); each still exports the
//! types it used to define under its own path.

mod health;
mod node;
mod rollup;
mod schedule;

pub use health::*;
pub use node::*;
pub use rollup::*;
pub use schedule::*;
