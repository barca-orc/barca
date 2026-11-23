//! The run queue of a server: `GET /queue` ([`QueueView`]), the same rows as `barca.queue`,
//! and the queue a metadata database records ([`recorded`]), which is what a server that runs
//! nothing itself (`--read-only`) answers with.

use crate::liveness::RecordedTiming;
use crate::outcome::QueuePlace;
use crate::run_explain::WaitingFor;
use crate::run_queue::QueueState;
use crate::serve::capacity::CapacitySnapshot;
use crate::time::rfc3339_millis as timestamp;
use crate::trigger::{Trigger, TriggeredBy};
use serde::{Deserialize, Serialize};

/// The words of [`QueueEntry::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum QueueEntryStatus {
    /// Accepted, waiting for a run slot.
    Pending,
    Running,
}

/// One run of `GET /queue`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct QueueEntry {
    pub control: crate::outcome::RunControl,
    pub run_id: String,
    /// `pending` (waiting to start) or `running`.
    #[cfg_attr(feature = "ts", ts(as = "QueueEntryStatus"))]
    pub status: String,
    /// Its place among the waiting runs, 1 = next; `null` for a running run.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub position: Option<usize>,
    pub command: String,
    /// The targets as given, comma-separated; `null` for every asset.
    pub target: Option<String>,
    /// `target` in a few words, as a row shows it (`barca_core::run_list::target_line`):
    /// `all assets`, `billing.py:orders +21 more`.
    #[serde(default)]
    pub target_line: String,
    pub triggered_by: TriggeredBy,
    pub schedule_id: Option<String>,
    /// When the server accepted it.
    pub queued_at: String,
    /// When it started executing (RFC 3339, UTC); `null` while it waits, and from a
    /// `--read-only` server (the recorded queue does not say).
    pub started_at: Option<String>,
    /// For a waiting run: when it is expected to start (RFC 3339, UTC), an estimate rounded
    /// up to 5 seconds: when a run slot frees, from the typical run time of the runs ahead
    /// of it over `max_runs` slots. `null` for a running run, while the queue is paused or
    /// draining, and from a `--read-only` server.
    pub estimated_start_at: Option<String>,
    /// How long it waits for a run slot, whole seconds: from `queued_at` to `started_at` for
    /// a running run, to `estimated_start_at` for a waiting one. `null` when neither is known.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub wait_seconds: Option<u64>,
    /// Where `run_queue` has it: `queued`, `running`, or `draining` (in flight when its server
    /// began to stop). A column of `barca.queue` ([`QueueEntry::table_row`]), not a key of
    /// `GET /queue`: `None` on an entry read back from a server's answer.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub state: Option<QueueState>,
    /// For a tick's run: its slot (RFC 3339, UTC). A column of `barca.queue`, not a key of
    /// `GET /queue`.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub scheduled_at: Option<String>,
    /// What else a waiting run waits for: `restart` while the server drains (it starts on the
    /// next server); `null` otherwise. Experimental.
    #[serde(default)]
    pub waiting_for: Option<WaitingFor>,
    /// `last` for a run asked to queue last (`queue` of `POST /runs`, `--queue last`): it
    /// waits behind the scheduled runs accepted before it and is ordered with scheduled work;
    /// `null` for every other run. Experimental.
    #[serde(default)]
    pub queue: Option<QueuePlace>,
}

impl QueueEntry {
    /// The entry as a row of `barca.queue` ([`crate::sql::tables::QUEUE_COLUMNS`]): the keys
    /// `GET /queue` serves that the table has, with `state` and `scheduled_at`.
    pub fn table_row(&self) -> serde_json::Value {
        serde_json::json!({
            "run_id": self.run_id,
            "position": self.position,
            "state": self.state.map(|s| s.as_str()),
            "queued_at": self.queued_at,
            "started_at": self.started_at,
            "estimated_start_at": self.estimated_start_at,
            "wait_seconds": self.wait_seconds,
            "command": self.command,
            "target": self.target,
            "triggered_by": self.triggered_by,
            "schedule_id": self.schedule_id,
            "scheduled_at": self.scheduled_at,
        })
    }
}

/// `GET /queue` (and the answer of pause and resume).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct QueueView {
    /// Paused by an operator: nothing new starts until it is resumed.
    pub paused: bool,
    /// When it was paused, RFC 3339; `null` when not paused.
    pub paused_at: Option<String>,
    /// The server is stopping: nothing new starts; the queue waits for the next start.
    #[serde(default)]
    pub draining: bool,
    /// How many runs execute at once.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_runs: Option<usize>,
    /// The runs executing now, oldest first.
    pub running: Vec<QueueEntry>,
    /// The runs waiting to start, next first.
    pub queued: Vec<QueueEntry>,
    /// The run slots in use, in words (`barca_core::run_list::queue_slots_line`): `1 of 4
    /// running`, or `1 of 1 running, plus 1 requested run` while a run someone asked for
    /// executes in the extra slot past `max_runs`.
    #[serde(default)]
    pub slots: String,
    /// The capacity every run shares (`--capacity`, `[server] capacity`): the weight in use,
    /// by run, and the steps waiting for it, in the order they are admitted. `null` when the
    /// server has none (each run has only its own).
    #[serde(default)]
    pub capacity: Option<CapacitySnapshot>,
}

/// One row of `run_queue`, with what its server's description of the run says: the host that
/// wrote the description reads it (`barca-server`).
#[derive(Clone, Debug)]
pub struct RecordedRun {
    pub run_id: String,
    pub state: QueueState,
    /// When the server accepted it, unix seconds.
    pub accepted_at: f64,
    pub command: String,
    /// The targets as given; empty for every asset.
    pub targets: Vec<String>,
    pub trigger: Trigger,
    /// Where the request asked to join the queue.
    pub queue: QueuePlace,
}

/// The queue a metadata database records, as of `now` (unix seconds): `rows` in the order the
/// server accepted them, with the starts and estimates `timing` has
/// ([`crate::liveness::recorded_timing`]). A run counts as running once `timing` knows when it
/// started. The waiting runs someone asked for go ahead of the scheduled ones and of the runs
/// asked to queue last, as the server starts them. `draining` is the answering host's own: the records do not say whether the
/// server that writes them drains.
pub fn recorded(
    rows: Vec<RecordedRun>,
    paused_at: Option<i64>,
    timing: &RecordedTiming,
    draining: bool,
    now: i64,
) -> QueueView {
    let (mut running, mut queued) = (Vec::new(), Vec::new());
    for row in rows {
        let start = timing.started.get(&row.run_id).copied();
        let started = start.is_some();
        let estimate = timing
            .starts
            .get(&row.run_id)
            .map(|at| crate::estimates::rounded(*at, now));
        let target = (!row.targets.is_empty()).then(|| row.targets.join(","));
        let item = QueueEntry {
            control: crate::outcome::RunControl::elsewhere(),
            run_id: row.run_id,
            status: if started { "running" } else { "pending" }.into(),
            position: (!started).then_some(queued.len() + 1),
            command: row.command,
            target_line: crate::run_list::target_line(target.as_deref()),
            target,
            triggered_by: row.trigger.triggered_by,
            schedule_id: row.trigger.schedule_id,
            queued_at: timestamp(row.accepted_at),
            started_at: start.and_then(crate::time::rfc3339_secs),
            estimated_start_at: estimate.and_then(crate::time::rfc3339_secs),
            wait_seconds: start
                .or(estimate)
                .map(|at| (at - row.accepted_at as i64).max(0) as u64),
            state: Some(row.state),
            scheduled_at: row.trigger.scheduled_at,
            // The records do not say whether the server that writes them drains.
            waiting_for: None,
            queue: row.queue.shown(),
        };
        if started {
            running.push(item);
        } else {
            queued.push(item);
        }
    }
    queued.sort_by_key(|entry| entry.schedule_id.is_some() || entry.queue.is_some());
    for (i, entry) in queued.iter_mut().enumerate() {
        entry.position = Some(i + 1);
    }
    QueueView {
        paused: paused_at.is_some(),
        paused_at: paused_at.map(crate::time::rfc3339),
        draining,
        max_runs: timing.max_runs,
        slots: timing.max_runs.map_or_else(
            || format!("{} running · run slots unknown", running.len()),
            |max| crate::run_list::queue_slots_line(running.len(), max),
        ),
        running,
        queued,
        capacity: None,
    }
}
