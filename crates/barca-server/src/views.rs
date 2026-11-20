//! Bodies the server answers with that are assembled from several of its parts: a row of
//! `GET /state`, the `running` entry of a node, and the scheduler's published job.

use barca_service::outcome::TriggeredBy;
use serde::Serialize;

/// One row of `GET /state`: the node's `barca status` entry plus what the
/// table needs beyond it — typical durations and the next cron fire time.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeState {
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub status: barca_service::status::NodeStatus,
    /// Typical wall time over the most recent successful materializations.
    pub durations: Option<Durations>,
    /// Next cron fire time (unix epoch seconds) in the server's `--timezone`, if scheduled.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub next_run: Option<i64>,
    /// The most recent cron fire time at or before now (unix epoch seconds) in the server's
    /// `--timezone`, if scheduled: the slot it last had.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub prev_run: Option<i64>,
    /// For a scheduled node that missed a slot: the first slot (unix epoch seconds) after its
    /// schedule's newest fired tick, when that slot is at or before now. `null` while on time,
    /// while paused, and when no tick is recorded. The same value as `late_since` of
    /// `GET /schedule` and of its `schedule` block, which also says whether it is `late`
    /// (`barca_core::schedule_standing::standing`).
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub late_since: Option<i64>,
    /// The run working on this node now, whoever started it (`null` when none is).
    pub running: Option<NodeRunning>,
    /// When its newest successful result was written (UTC `YYYY-MM-DD HH:MM:SS`, like
    /// `last_materialization.created_at`); `null` if it never succeeded.
    pub last_success_at: Option<String>,
    /// The first failed attempt after its last clean run; `null` unless its latest attempts
    /// failed.
    pub failing_since: Option<String>,
    /// How many runs in a row failed it, up to now (`0` when its latest attempt did not fail).
    pub consecutive_failures: u32,
    /// Its newest run as a whole, the run `last_materialization` is from; absent until a run
    /// recorded a row for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_run: Option<barca_service::db::RunTotals>,
}

pub use barca_service::health::Durations;

/// Durable, mutable view of one scheduled job, published by the scheduler and
/// read by `GET /schedule`. Where the schedule stands (its last tick and run, its next slot,
/// whether it is late) is not here: it is computed at request time from the recorded ticks
/// (`barca_service::schedule_standing::standing`), as every other surface computes it.
#[derive(Clone, Debug, Serialize)]
pub struct JobStatus {
    /// Full node id (the run target).
    pub id: String,
    /// The node's cron expression.
    pub cron: String,
    /// Node kind (asset / sensor / task).
    pub kind: barca_service::NodeKind,
    /// Last time a due tick of this job was skipped (unix epoch seconds) because a run of
    /// its node was already in flight. Kept in memory only; a restart clears it.
    pub last_skipped: Option<i64>,
    /// Handle of the in-flight run that caused `last_skipped` (`None` for a paused job).
    pub skipped_for: Option<String>,
    /// Why the last due tick was skipped: `in_flight` or `paused`.
    pub skip_reason: Option<&'static str>,
    /// Consecutive failed ticks (RFC-0008 rule 9); persisted in `schedule_failures`.
    pub failures: u32,
    /// `Schedule(..., max_failures=N)`: at this many consecutive failed ticks the job is
    /// skipped and reported `failing`.
    pub max_failures: u32,
}

/// A run in flight that has a node in it: executing it, about to reach it, or waiting in the
/// queue with it among its nodes (`step`).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeRunning {
    /// The run (`GET /runs/{run_id}`).
    pub run_id: String,
    /// What started the run; `null` for a CLI run recorded by a barca that did not record it.
    pub triggered_by: Option<TriggeredBy>,
    /// When the run started (RFC 3339, UTC).
    pub started_at: String,
    /// `running`: the run is executing it now. `waiting`: the run is executing and will still
    /// reach it. `queued`: the run waits in the queue.
    pub step: RunningStep,
    /// `step` in the words every surface prints: `running`, `in this run`, `in a queued run`.
    pub label: String,
    /// For a partitioned node: how many of its keys are done.
    pub partitions: Option<PartitionProgress>,
}

pub use barca_service::run_list::RunningStep;

pub use barca_service::served::PartitionProgress;
