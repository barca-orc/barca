// Everything barca prints goes through `term` (`outln!` / `errln!`), which cannot panic.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

// Unit tests of a large module live beside it: `#[cfg(test)] mod tests;` in `<name>.rs` (or
// `<name>/mod.rs`) points at `<name>/tests.rs`, which starts with `use super::*;`.

#[doc(hidden)]
pub mod artifact_format;
pub mod backup;
pub mod batch;
pub mod cache;
pub mod cache_state;
pub(crate) mod changes;
pub mod commands;
pub(crate) mod cone;
pub mod config;
pub(crate) mod coordinator;
pub(crate) mod cost;
pub mod dag;
mod dataset_sinks;
pub mod db;
pub(crate) mod decorator_args;
pub(crate) mod definition;
pub mod diagnostics;
pub mod discover;
pub mod dispatch;
pub(crate) mod env_reads;
pub(crate) mod envdeps;
pub mod envelope;
pub(crate) mod events;
pub(crate) mod execution;
pub mod freshness;
pub mod gc;
pub mod groups;
pub mod hash;
pub mod helper_proc;
pub mod history_filter;
pub mod http;
mod incremental;
mod input_params;
pub mod interrupt;
pub(crate) mod io_loop;
pub mod jsonlog;
pub(crate) mod keyed;
pub mod layout;
pub mod ledger;
pub mod lines;
pub mod load;
mod metadata_size;
mod mismatch;
pub mod model;
pub mod outcome;
pub mod parse;
pub mod pending_uploads;
mod persist;
pub mod pins;
pub(crate) mod planner;
pub(crate) mod project_modules;
pub(crate) mod protocol;
pub mod queries;
mod recover;
pub mod report;
mod result_journal;
pub mod results;
pub(crate) mod run_host;
pub mod run_logs;
mod run_owner;
pub mod schedule;
pub mod selection;
pub mod serve;
mod sink_args;
mod sink_publications;
#[cfg(test)]
mod source_scan;
pub mod sql;
pub(crate) mod state_validate;
pub mod status;
pub mod step_status;
pub mod store_inspect;
mod store_sync;
pub mod suggest;
pub mod targets;
pub mod telemetry;
pub mod term;
pub mod test_hooks;
pub mod time;
pub mod transfer;
pub(crate) mod unrelated_modules;
pub(crate) mod unused_inputs;
pub mod values;
pub mod warnings;

pub use dag::Dag;
pub use events::{LogStream, RunEvent};
pub use execution::sizing;
pub use model::*;
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

    /// A target name that several nodes have (the same function name in two files). Like
    /// [`BarcaError::AssetNotFound`], a usage error whose remediation is `barca list`.
    #[error("{0}")]
    AmbiguousTarget(String),

    #[error("DAG error: {0}")]
    Dag(#[from] dag::DagError),

    #[error("Parse error: {0}")]
    Parse(String),

    /// A user step failed permanently (after its retries).
    #[error("Step '{}' failed: {}", .0.node, .0.message)]
    WorkerFailed(Box<FailedStep>),

    #[error("run cancelled")]
    Cancelled,

    #[error("Database error: {0}")]
    Db(String),

    /// The metadata DB at `path` is there but this barca cannot use it: it is a directory, not
    /// a SQLite file, written by a newer release, or its schema is malformed (an empty file is
    /// initialized instead, [`db::init_db_as`]). Infra (exit 3);
    /// its envelope's `reason` is `metadata_unusable`. A lock or I/O failure stays [`Db`].
    ///
    /// [`Db`]: BarcaError::Db
    #[error("{}", metadata_unusable(path, why))]
    MetadataUnusable { path: String, why: String },

    /// The caller's mistake: the command or its configuration is wrong (task/asset misuse,
    /// unknown `--refresh` name, invalid config). Re-running the same invocation cannot
    /// succeed; the CLI exits 2.
    #[error("{0}")]
    Usage(String),

    /// Anything else: barca or its environment failed (remote state, worker spawn, I/O).
    #[error("{0}")]
    Other(String),
}

fn metadata_unusable(path: &str, why: &str) -> String {
    if path.is_empty() {
        format!("the metadata DB cannot be used: {why}")
    } else {
        format!("the metadata DB {path} cannot be used: {why}")
    }
}

/// A step that failed permanently, as reported by its worker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedStep {
    /// Node id of the failing step (`file.py:name`, with a partition suffix when partitioned).
    pub node: String,
    /// What the worker reported: `ExcType: message`, then the traceback frames (the user's
    /// code's, or every frame for a failure in barca's own code: see `phase`).
    pub message: String,
    /// Where the step failed, when its worker said so (#664). `None` for a worker that died
    /// without reporting, or a step barca failed without running it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<dispatch::ErrorPhase>,
    /// Where this step's artifacts are stored (`{artifact_root}/{safe node id}`): a local
    /// directory or a remote URI. It may not exist if the step never succeeded.
    pub artifact_dir: Option<String>,
    /// What the run did before it stopped, so callers can still report it (`None` when the
    /// failure is not from a full run, e.g. in tests).
    pub run: Option<Box<PartialRun>>,
}

/// A run that stopped because a user step failed: what was reached before it stopped.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PartialRun {
    pub run_id: String,
    pub elapsed_seconds: f64,
    pub steps_executed: usize,
    pub phases: usize,
    /// What happened to each step that was reached; the failed step has status `failed`.
    pub steps: Vec<results::StepReport>,
    /// Plan-time warnings for the steps this run planned (the same list a successful run has).
    pub warnings: Vec<warnings::PlanWarning>,
    /// A partitioned target's keys that succeeded before the run stopped (failed keys are
    /// absent); `None` for an unpartitioned target.
    #[serde(default, skip)]
    pub partitions: Option<Vec<(String, dispatch::OutputRef)>>,
    /// See [`results::GetResult::key_summary`].
    #[serde(default, skip)]
    pub key_summary: Option<results::KeySummary>,
}

impl FailedStep {
    /// The exception line(s) without the traceback, e.g. `ZeroDivisionError: division by zero`.
    pub fn summary(&self) -> &str {
        self.split().0
    }

    /// The traceback frames, when the worker reported any.
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
mod failed_step_tests;

mod wire;

/// Hand the heap memory that is free back to the operating system. A run of many partition keys
/// allocates and frees hundreds of megabytes; glibc keeps freed memory in each thread's arena,
/// so a long-lived `barca serve` grows by the largest run each arena has served unless it is
/// returned. `barca serve` calls this when a run ends (#623). A no-op off glibc.
pub fn release_free_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only walks the allocator's own free lists.
    unsafe {
        libc::malloc_trim(0);
    }
}

mod observation;

mod source_observation;
