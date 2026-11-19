//! The run queue. Every run the server accepts (`POST /runs`, the older trigger routes, the web
//! UI, `--server` clients, scheduled ticks) joins it, and is recorded in the metadata DB
//! (`run_queue`) before the server answers, under the run id it answers with. Runs start in the
//! order they were accepted, at most `--max-runs` at once (`[server] max_runs`, default one per
//! core), while the queue is neither paused nor draining. A run someone asked for (API, UI,
//! CLI) starts ahead of waiting scheduled runs, and when every slot is taken it may use one
//! extra slot of its own, so an interactive `get` never waits behind cron work.
//!
//! A run leaves the queue when it has recorded how it ended. A server that starts resumes the runs
//! it finds queued, in order ([`resume_at_startup`]). A run the stopping server stopped at its
//! drain deadline goes back to its place ([`crate::runs`], `requeue`) and starts again from the
//! beginning after the restart: steps that finished are cache hits then, so a run is executed
//! at least once, and a step that was interrupted runs again.

use crate::run_model::RunSpec;
use barca_core::cache::CachePolicy;
use barca_core::outcome::{RunCommand, Trigger, TriggeredBy};
use barca_core::served::QueueView;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

/// The queue of one server.
pub struct Queue {
    /// The runs waiting to start, in order.
    pub(crate) waiting: Mutex<VecDeque<Waiting>>,
    /// When an operator paused the queue (unix seconds), while it is paused.
    paused_at: Mutex<Option<i64>>,
    /// Held while a run is recorded and joins `waiting`, so both orders agree.
    pub(crate) admit: tokio::sync::Mutex<()>,
    /// How many runs execute at once.
    pub max_runs: usize,
    /// Set when the drain deadline passed: the runs it stops go back to the queue instead of
    /// ending `cancelled`.
    pub requeue: AtomicBool,
    /// A `--read-only` server's last read of the queue the metadata DB records, as `GET /health`
    /// reports it without waiting on a read: (paused, queued, running) and when it was read.
    recorded: Mutex<Option<(RecordedCounts, std::time::Instant)>>,
    /// A background read of the recorded queue is in flight.
    pub(crate) recording: AtomicBool,
}

/// The counts of a recorded queue: paused, queued, running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedCounts {
    pub max_runs: Option<usize>,
    pub paused: bool,
    pub queued: usize,
    pub running: usize,
}

/// A run waiting to start.
pub struct Waiting {
    pub run_id: String,
    pub(crate) spec: RunSpec,
}

impl Queue {
    pub fn new(max_runs: usize) -> Self {
        Self {
            waiting: Mutex::new(VecDeque::new()),
            paused_at: Mutex::new(None),
            admit: tokio::sync::Mutex::new(()),
            max_runs: max_runs.max(1),
            requeue: AtomicBool::new(false),
            recorded: Mutex::new(None),
            recording: AtomicBool::new(false),
        }
    }

    /// Note what a read of the recorded queue found.
    pub(crate) fn note_recorded(&self, view: &QueueView) {
        let counts = RecordedCounts {
            max_runs: view.max_runs,
            paused: view.paused,
            queued: view.queued.len(),
            running: view.running.len(),
        };
        *self.recorded.lock().unwrap() = Some((counts, std::time::Instant::now()));
    }

    /// The last recorded counts, and whether they are older than `max_age` (or missing).
    pub(crate) fn recorded(&self, max_age: std::time::Duration) -> (Option<RecordedCounts>, bool) {
        let got = *self.recorded.lock().unwrap();
        match got {
            Some((counts, at)) => (Some(counts), at.elapsed() > max_age),
            None => (None, true),
        }
    }

    /// When the queue was paused, while it is.
    pub fn paused_at(&self) -> Option<i64> {
        *self.paused_at.lock().unwrap()
    }

    pub(crate) fn set_paused(&self, at: Option<i64>) {
        *self.paused_at.lock().unwrap() = at;
    }

    /// The ids of the runs waiting, in the order they will start: runs someone asked for
    /// first, then scheduled runs, each in the order they were accepted.
    pub fn waiting_ids(&self) -> Vec<String> {
        let waiting = self.waiting.lock().unwrap();
        let (asked, scheduled): (Vec<_>, Vec<_>) =
            waiting.iter().partition(|w| !is_scheduled(&w.spec));
        asked
            .into_iter()
            .chain(scheduled)
            .map(|w| w.run_id.clone())
            .collect()
    }

    /// Take `run_id` off the queue, if it is still waiting.
    pub fn take(&self, run_id: &str) -> Option<Waiting> {
        let mut waiting = self.waiting.lock().unwrap();
        let at = waiting.iter().position(|w| w.run_id == run_id)?;
        waiting.remove(at)
    }

    /// The metadata DB was started over (`POST /state/reset`, refused while a run is queued):
    /// the pause it kept goes with it, as schedule pauses do.
    pub fn reset(&self) {
        self.set_paused(None);
    }
}

// ─── what is recorded ───────────────────────────────────────────────────────

/// A run as `run_queue.spec` keeps it: everything [`RunSpec`] holds.
#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    command: RunCommand,
    targets: Vec<String>,
    nodes: Vec<String>,
    files: Vec<String>,
    policy: StoredPolicy,
    trigger: Trigger,
    #[serde(default)]
    values: bool,
    #[serde(default)]
    partition: Option<serde_json::Value>,
    /// `--batch` (RFC-0013).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    batch: Vec<String>,
    #[serde(default)]
    pin_manual: bool,
    #[serde(default)]
    tick: Option<String>,
    /// `Schedule(..., timeout_seconds=N)` of the tick that started it.
    #[serde(default)]
    timeout: Option<u64>,
    /// How many times it was put back after its server was killed during a drain.
    #[serde(default)]
    kill_requeues: u32,
    /// When the server accepted it (unix seconds).
    accepted_at: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredPolicy {
    CacheAware,
    RefreshAll,
    Refresh { names: Vec<String>, cascade: bool },
}

pub(crate) fn store(spec: &RunSpec, accepted_at: f64) -> String {
    let stored = Stored {
        command: spec.command,
        targets: spec.targets.clone(),
        nodes: spec.nodes.clone(),
        files: spec.files.clone(),
        policy: match &spec.policy {
            CachePolicy::CacheAware => StoredPolicy::CacheAware,
            CachePolicy::RefreshAll => StoredPolicy::RefreshAll,
            CachePolicy::RefreshSelective { names, cascade } => StoredPolicy::Refresh {
                names: names.clone(),
                cascade: *cascade,
            },
        },
        trigger: spec.trigger.clone(),
        values: spec.values,
        partition: spec.partition.as_ref().map(|p| p.to_json()),
        batch: spec.batch.clone(),
        pin_manual: spec.rules.pin_manual,
        tick: spec.rules.tick.clone(),
        timeout: spec.timeout,
        kill_requeues: spec.kill_requeues,
        accepted_at,
    };
    serde_json::to_string(&stored).expect("a run spec is JSON")
}

pub(crate) fn load(text: &str) -> Result<(RunSpec, f64), String> {
    let stored: Stored = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let spec = RunSpec {
        command: stored.command,
        targets: stored.targets,
        nodes: stored.nodes,
        files: stored.files,
        policy: match stored.policy {
            StoredPolicy::CacheAware => CachePolicy::CacheAware,
            StoredPolicy::RefreshAll => CachePolicy::RefreshAll,
            StoredPolicy::Refresh { names, cascade } => {
                CachePolicy::RefreshSelective { names, cascade }
            }
        },
        trigger: stored.trigger,
        values: stored.values,
        partition: stored
            .partition
            .map(|value| barca_core::selection::PartitionSelection::parse(&value.to_string()))
            .transpose()?,
        batch: stored.batch,
        rules: barca_core::freshness::RunRules {
            pin_manual: stored.pin_manual,
            tick: stored.tick,
            status: false,
        },
        timeout: stored.timeout,
        kill_requeues: stored.kill_requeues,
    };
    Ok((spec, stored.accepted_at))
}

/// A run a cron tick started, rather than a person or client.
pub(crate) fn is_scheduled(spec: &RunSpec) -> bool {
    spec.trigger.triggered_by == TriggeredBy::Schedule
}
