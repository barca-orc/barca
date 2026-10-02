pub mod cache;
pub mod commands;
pub mod cone;
pub mod config;
pub mod coordinator;
pub mod cost;
pub mod dag;
pub mod db;
pub mod dispatch;
pub mod hash;
pub mod io_loop;
pub mod model;
pub mod parse;
pub mod planner;
pub mod protocol;
pub mod state_sync;
pub mod transfer;

pub use dag::Dag;
pub use model::*;
pub use planner::{ExecutionPlan, ResourceConfig, expand_partition_combos};
/// Re-exported so callers (CLI, server) share one token type without depending
/// on tokio-util directly.
pub use tokio_util::sync::CancellationToken;

/// Top-level error type for barca engine operations.
#[derive(Debug, thiserror::Error)]
pub enum BarcaError {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("Asset '{0}' not found. Available: {1}")]
    AssetNotFound(String, String),

    #[error("DAG error: {0}")]
    Dag(#[from] dag::DagError),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Worker failed: {0}")]
    WorkerFailed(String),

    #[error("run cancelled")]
    Cancelled,

    #[error("Database error: {0}")]
    Db(String),

    #[error("{0}")]
    Other(String),
}
