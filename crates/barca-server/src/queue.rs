//! The run queue. Every run the server accepts (`POST /runs`, the older trigger routes, the web
//! UI, `--server` clients, scheduled ticks) joins it, and is recorded in the metadata DB
//! (`run_queue`) before the server answers, under the run id it answers with. Runs start in the
//! order they were accepted, at most `--max-runs` at once (`[server] max_runs`, default one per
//! core), while the queue is neither paused nor draining. A run someone asked for (API, UI,
//! CLI) starts ahead of waiting scheduled runs, and when every slot is taken it may use one
//! extra slot of its own, so an interactive `get` never waits behind cron work. A request
//! that says `queue: "last"` gives that up: it waits its turn with the scheduled runs, in the
//! order they were accepted, and takes no extra slot.
//!
//! A run leaves the queue when it has recorded how it ended. A server that starts resumes the runs
//! it finds queued, in order (`runs::resume_at_startup`). A run the stopping server stopped at its
//! drain deadline goes back to its place ([`crate::runs`], `requeue`) and starts again from the
//! beginning after the restart: completed assets with matching hashes satisfy a refresh when
//! recorded at/after its original acceptance. Interrupted work runs again. Tasks outside a
//! tick can repeat even after completion, including their side effects.

use crate::run_model::RunSpec;
use barca_service::cache::CachePolicy;
use barca_service::outcome::{QueuePlace, RunCommand, Trigger, TriggeredBy};
use barca_service::served::QueueView;
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
    /// first, then scheduled runs and the runs asked to queue last ([`goes_ahead`]), each in
    /// the order they were accepted.
    pub fn waiting_ids(&self) -> Vec<String> {
        let waiting = self.waiting.lock().unwrap();
        let (asked, scheduled): (Vec<_>, Vec<_>) =
            waiting.iter().partition(|w| goes_ahead(&w.spec));
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
    /// `queue` of `POST /runs`, when it is not the default: `"last"`.
    #[serde(default, skip_serializing_if = "QueuePlace::is_next")]
    queue: QueuePlace,
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
            CachePolicy::RefreshAll { .. } => StoredPolicy::RefreshAll,
            CachePolicy::RefreshSelective { names, cascade, .. } => StoredPolicy::Refresh {
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
        queue: spec.queue,
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
            StoredPolicy::RefreshAll => CachePolicy::RefreshAll {
                since: stored.accepted_at,
            },
            StoredPolicy::Refresh { names, cascade } => CachePolicy::RefreshSelective {
                names,
                cascade,
                since: stored.accepted_at,
            },
        },
        trigger: stored.trigger,
        values: stored.values,
        partition: stored
            .partition
            .map(|value| barca_service::selection::PartitionSelection::parse(&value.to_string()))
            .transpose()?,
        batch: stored.batch,
        rules: barca_service::freshness::RunRules {
            pin_manual: stored.pin_manual,
            tick: stored.tick,
            status: false,
        },
        timeout: stored.timeout,
        kill_requeues: stored.kill_requeues,
        queue: stored.queue,
    };
    Ok((spec, stored.accepted_at))
}

/// A run a cron tick started, rather than a person or client.
pub(crate) fn is_scheduled(spec: &RunSpec) -> bool {
    spec.trigger.triggered_by == TriggeredBy::Schedule
}

/// A run that starts ahead of waiting scheduled work: someone asked for it, and did not ask
/// for it to queue last (`queue: "last"`). Every other run, a tick's or one queued last, starts
/// in the order it was accepted, after the runs that go ahead.
pub(crate) fn goes_ahead(spec: &RunSpec) -> bool {
    !is_scheduled(spec) && spec.queue != QueuePlace::Last
}

/// Whether a run of this trigger and place is ordered with scheduled work: for the extra run
/// slot, and for the shared capacity its steps wait for.
pub(crate) fn with_scheduled(trigger: &Trigger, queue: QueuePlace) -> bool {
    trigger.triggered_by == TriggeredBy::Schedule || queue == QueuePlace::Last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(trigger: Trigger, queue: QueuePlace) -> RunSpec {
        RunSpec {
            trigger,
            queue,
            ..RunSpec::api(RunCommand::Get, vec!["a".into()], vec![])
        }
    }

    /// The rule of `queue: "last"` for a run slot: requests that did not ask for it first, in
    /// the order accepted; then the scheduled runs and the runs queued last, together, in the
    /// order accepted. So a run queued last is behind the scheduled runs accepted before it
    /// and ahead of the ones accepted after it.
    #[test]
    fn a_run_queued_last_waits_with_the_scheduled_runs_in_the_order_accepted() {
        let queue = Queue::new(1);
        let tick = |id: &str| Trigger::schedule(id, 0);
        for (id, spec) in [
            ("tick-a", spec(tick("p.py:a"), QueuePlace::Next)),
            ("asked-1", spec(Trigger::api(), QueuePlace::Next)),
            ("last-1", spec(Trigger::api(), QueuePlace::Last)),
            ("asked-2", spec(Trigger::api(), QueuePlace::Next)),
            ("tick-b", spec(tick("p.py:b"), QueuePlace::Next)),
            ("last-2", spec(Trigger::api(), QueuePlace::Last)),
        ] {
            queue.waiting.lock().unwrap().push_back(Waiting {
                run_id: id.into(),
                spec,
            });
        }
        assert_eq!(
            queue.waiting_ids(),
            ["asked-1", "asked-2", "tick-a", "last-1", "tick-b", "last-2"]
        );
    }

    #[test]
    fn the_place_is_kept_in_the_stored_description_and_absent_by_default() {
        let last = store(&spec(Trigger::api(), QueuePlace::Last), 1.0);
        assert!(last.contains(r#""queue":"last""#), "{last}");
        assert_eq!(load(&last).unwrap().0.queue, QueuePlace::Last);
        // The default writes no key, so a description is what earlier releases wrote, and
        // one they wrote reads as the default.
        let next = store(&spec(Trigger::api(), QueuePlace::Next), 1.0);
        assert!(!next.contains(r#""queue":"#), "{next}");
        assert_eq!(load(&next).unwrap().0.queue, QueuePlace::Next);
    }
}
