//! A run as the server holds it: what was asked for ([`RunSpec`]), and its live record while
//! it waits, runs and for a while after ([`RunState`]).

use barca_service::CancellationToken;
use barca_service::cache::CachePolicy;
use barca_service::outcome::{RunCommand, Trigger};
use barca_service::results::GetResult;
use serde::Serialize;

/// Lifecycle of an async run tracked by the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Accepted, not yet started.
    Pending,
    /// Executing in a background task.
    Running,
    /// Finished successfully.
    Complete,
    /// Finished with an error.
    Failed,
    /// Stopped mid-flight via `DELETE /runs/{id}` or server shutdown.
    Cancelled,
}

/// In-memory record of a single run, kept while it runs and for a while after. `handle` is
/// the database run id, allocated when the run is accepted.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunState {
    /// The run id (see `/runs/{run_id}`); the older `/status/{run_id}` calls it the handle.
    pub handle: String,
    /// The same id, once the run has written its database row.
    #[serde(skip)]
    pub db_run_id: Option<String>,
    #[serde(skip)]
    pub command: String,
    /// The targets as given, comma-separated (`None`: every asset and sensor).
    #[serde(skip)]
    pub target: Option<String>,
    /// The full node ids the targets resolved to when the run was accepted; empty for a run of
    /// every asset and sensor. The scheduler matches a due job against them, so a run counts
    /// as in flight for its node however the target was spelled.
    #[serde(skip)]
    pub nodes: Vec<String>,
    /// What started the run.
    #[serde(skip)]
    pub trigger: barca_service::outcome::Trigger,
    /// Where the request asked to join the queue.
    #[serde(skip)]
    pub queue: barca_service::outcome::QueuePlace,
    /// The run was asked to compute again what is cached (`--refresh`, `--refresh-all`): a
    /// cached node of it is not one it will serve from cache.
    #[serde(skip)]
    pub refreshes: bool,
    /// The files the request named (`[]`: the deployed set).
    #[serde(skip)]
    pub files: Vec<String>,
    pub status: RunStatus,
    /// Populated when `status == Complete` for a single-target run. Carries the DB run id,
    /// timing, output.
    pub result: Option<GetResult>,
    /// Populated when the run did not succeed.
    pub error: Option<String>,
    /// Unix epoch seconds when the run was accepted.
    pub started_at: f64,
    /// Unix epoch seconds when the run finished (success or failure).
    pub finished_at: Option<f64>,
    /// The run's full result once it finished.
    #[serde(skip)]
    pub outcome: Option<barca_service::outcome::Outcome>,
    /// Cancels this run's execution future. `DELETE /runs/{id}` triggers it;
    /// the run task observes it, terminates workers, and marks the run
    /// cancelled. Not part of the JSON status payload.
    #[serde(skip)]
    pub cancel: CancellationToken,
    /// `DELETE /runs/{id}` asked for it: a stopping server does not put it back in the queue.
    #[serde(skip)]
    pub cancel_requested: bool,
    /// Who cancelled it and whether it had started (#925), recorded with its outcome.
    #[serde(skip)]
    pub cancellation: Option<barca_service::run_explain::Cancellation>,
}

/// A run to start: what to run, how, and what asked for it.
pub(crate) struct RunSpec {
    pub command: RunCommand,
    /// As given (`[]`: every asset and sensor).
    pub targets: Vec<String>,
    /// The full node ids the targets resolved to.
    pub nodes: Vec<String>,
    /// As given in the request (`[]`: the deployed set).
    pub files: Vec<String>,
    pub policy: CachePolicy,
    pub trigger: Trigger,
    /// `get -o value`: the outcome's `final_output` holds values, not pointers.
    pub values: bool,
    /// `get --partition`.
    pub partition: Option<barca_service::selection::PartitionSelection>,
    /// `--batch` (RFC-0013), each as the flag takes it.
    pub batch: Vec<String>,
    /// The freshness rules (RFC-0008): a `Manual` node read as it is (`POST /runs`, as the CLI
    /// does), and whether this is a scheduled tick.
    pub rules: barca_service::freshness::RunRules,
    /// `Schedule(..., timeout_seconds=N)` of the tick that started it: how long the run may
    /// take (0: no limit). `None`: the server's run timeout (`runs::run_timeout`).
    pub timeout: Option<u64>,
    /// How many times a server killed during its drain left this run unfinished, and the next
    /// server put it back in the queue (`runs::resume_at_startup`).
    pub kill_requeues: u32,
    /// `queue` of `POST /runs`: `last` orders the run with scheduled work.
    pub queue: barca_service::outcome::QueuePlace,
}

impl RunSpec {
    /// A run started over HTTP.
    pub fn api(command: RunCommand, targets: Vec<String>, nodes: Vec<String>) -> Self {
        Self {
            command,
            targets,
            nodes,
            files: Vec::new(),
            policy: CachePolicy::CacheAware,
            trigger: Trigger::api(),
            values: false,
            partition: None,
            batch: Vec::new(),
            // The older trigger routes compute what they are asked for, as before RFC-0008.
            rules: barca_service::freshness::RunRules::default(),
            timeout: None,
            kill_requeues: 0,
            queue: Default::default(),
        }
    }
}
