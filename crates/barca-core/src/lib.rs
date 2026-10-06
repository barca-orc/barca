pub mod cache;
pub mod commands;
pub mod cone;
pub mod config;
pub mod coordinator;
pub mod cost;
pub mod dag;
pub mod db;
pub mod discover;
pub mod dispatch;
pub mod envdeps;
pub mod hash;
pub mod io_loop;
pub mod model;
pub mod parse;
pub mod planner;
pub mod protocol;
pub mod sql;
pub mod state_sync;
pub mod status;
pub mod telemetry;
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

    /// A user step failed permanently (after its retries).
    #[error("Worker failed: {}", .0.message)]
    WorkerFailed(Box<FailedStep>),

    #[error("run cancelled")]
    Cancelled,

    #[error("Database error: {0}")]
    Db(String),

    /// The caller's mistake: the command or its configuration is wrong (task/asset misuse,
    /// unknown `--refresh` name, invalid config). Re-running the same invocation cannot
    /// succeed; the CLI exits 2.
    #[error("{0}")]
    Usage(String),

    /// Anything else: barca or its environment failed (remote state, worker spawn, I/O).
    #[error("{0}")]
    Other(String),
}

/// A step that failed permanently, as reported by its worker.
#[derive(Debug, Clone)]
pub struct FailedStep {
    /// Node id of the failing step (`file.py:name`, with a partition suffix when partitioned).
    pub node: String,
    /// What the worker reported: `ExcType: message`, then the user-code traceback frames.
    pub message: String,
    /// Where this step's artifacts are stored (`{artifact_root}/{safe node id}`): a local
    /// directory or a remote URI. It may not exist if the step never succeeded.
    pub artifact_dir: Option<String>,
    /// What the run did before it stopped, so callers can still report it (`None` when the
    /// failure is not from a full run, e.g. in tests).
    pub run: Option<Box<PartialRun>>,
}

/// A run that stopped because a user step failed: what was reached before it stopped.
#[derive(Debug, Clone)]
pub struct PartialRun {
    pub run_id: String,
    pub elapsed_seconds: f64,
    pub steps_executed: usize,
    pub phases: usize,
    /// What happened to each step that was reached; the failed step has status `failed`.
    pub steps: Vec<commands::StepReport>,
}

impl FailedStep {
    /// The exception line(s) without the traceback, e.g. `ZeroDivisionError: division by zero`.
    pub fn summary(&self) -> &str {
        self.split().0
    }

    /// The user-code traceback frames, when the worker reported any.
    pub fn traceback(&self) -> Option<&str> {
        self.split().1
    }

    /// The worker formats a failure as the exception line(s) followed by
    /// `traceback.format_list` frames, each of which starts with `  File "`.
    fn split(&self) -> (&str, Option<&str>) {
        let msg = self.message.as_str();
        let mut offset = 0;
        for line in msg.split_inclusive('\n') {
            if line.starts_with("  File \"") {
                let tb = msg[offset..].trim_end();
                return (msg[..offset].trim_end(), Some(tb));
            }
            offset += line.len();
        }
        (msg.trim_end(), None)
    }
}

/// `node_id` as a single path segment — mirrors `safe_node_id` in python/barca/_artifacts.py,
/// which names each node's artifact directory.
pub fn safe_node_id(node_id: &str) -> String {
    node_id
        .replace('/', "__")
        .replace(':', "--")
        .replace('[', "_")
        .replace(']', "")
        .replace(['=', ',', ' '], "_")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod failed_step_tests {
    use super::*;

    fn step(message: &str) -> FailedStep {
        FailedStep {
            node: "p.py:oops".into(),
            message: message.into(),
            artifact_dir: None,
            run: None,
        }
    }

    #[test]
    fn splits_exception_line_from_traceback_frames() {
        let s = step(
            "ZeroDivisionError: division by zero\n  File \"p.py\", line 5, in oops\n    return 1 / 0\n",
        );
        assert_eq!(s.summary(), "ZeroDivisionError: division by zero");
        assert_eq!(
            s.traceback(),
            Some("  File \"p.py\", line 5, in oops\n    return 1 / 0")
        );
    }

    #[test]
    fn multi_line_message_without_traceback() {
        let s = step("worker disconnected");
        assert_eq!(s.summary(), "worker disconnected");
        assert_eq!(s.traceback(), None);
        let s = step("ValueError: bad\nsecond line");
        assert_eq!(s.summary(), "ValueError: bad\nsecond line");
    }

    #[test]
    fn safe_node_id_matches_the_python_artifact_layout() {
        assert_eq!(safe_node_id("pipeline.py:fetch"), "pipeline.py--fetch");
        assert_eq!(
            safe_node_id("src/p.py:f[region=eu,day=1]"),
            "src__p.py--f_region_eu_day_1"
        );
    }
}
