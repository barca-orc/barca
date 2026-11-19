//! The run queue as the routes answer it: the runs executing and waiting, when each started or
//! is expected to start, and the `queue` block of `GET /health`. A `--read-only` server holds
//! no queue and reports the one the metadata DB records.

use crate::error::ApiError;
use crate::run_model::{RunState, RunStatus};
use crate::state::AppState;
use barca_service::serve::run_queue::{self, QueueState};
use barca_service::time::rfc3339_millis as timestamp;
use std::sync::atomic::Ordering;

pub use barca_service::served::{QueueEntry, QueueView};

fn entry(state: &AppState, run: &RunState, position: Option<usize>, draining: bool) -> QueueEntry {
    QueueEntry {
        run_id: run.handle.clone(),
        status: if run.status == RunStatus::Running {
            "running"
        } else {
            "pending"
        }
        .into(),
        position,
        command: run.command.clone(),
        target_line: barca_service::run_list::target_line(run.target.as_deref()),
        target: run.target.clone(),
        triggered_by: run.trigger.triggered_by,
        schedule_id: run.trigger.schedule_id.clone(),
        queued_at: timestamp(run.started_at),
        started_at: None,
        estimated_start_at: None,
        wait_seconds: None,
        state: Some(match (run.status == RunStatus::Running, draining) {
            (false, _) => QueueState::Queued,
            (true, false) => QueueState::Running,
            (true, true) => QueueState::Draining,
        }),
        scheduled_at: run.trigger.scheduled_at.clone(),
        waiting_for: state.lifecycle.waiting_for(run.status),
    }
}

/// When the runs of this server's queue started and are expected to start, as of `now` (unix
/// seconds): what `GET /queue` and every node's `expected_fresh_at` read.
#[derive(Clone, Debug, Default)]
pub(crate) struct QueueEstimate {
    /// Each running run: when it started executing.
    pub started: std::collections::HashMap<String, i64>,
    /// Each waiting run: when it is expected to start. Empty while the queue starts nothing.
    pub starts: std::collections::HashMap<String, i64>,
    /// When a run that joined now would start, when no run slot is free.
    pub clear_at: Option<i64>,
    /// The queue starts runs: neither paused nor draining.
    pub open: bool,
}

/// [`QueueEstimate`] over `ticks`, what is recorded of each schedule's ticks: a run's typical
/// time is its schedule's ([`barca_service::estimates::typical_run_seconds`]).
pub(crate) fn estimate(
    state: &AppState,
    ticks: &std::collections::HashMap<String, barca_service::schedule_standing::Ticks>,
    now: i64,
) -> QueueEstimate {
    use barca_service::estimates::{QueueRun, queue_starts, typical_run_seconds};
    let open = !state.lifecycle.is_draining() && state.runs.queue.paused_at().is_none();
    let of = |run: &RunState, started_at: Option<i64>| QueueRun {
        run_id: run.handle.clone(),
        started_at,
        typical_seconds: typical_run_seconds(ticks, run.trigger.schedule_id.as_deref()),
    };
    let mut started = std::collections::HashMap::new();
    let mut runs: Vec<QueueRun> = state
        .runs
        .registry
        .iter()
        .filter(|r| r.status == RunStatus::Running)
        .map(|r| {
            let at = state
                .runs
                .events
                .get(&r.handle)
                .and_then(|c| c.progress().started)
                .unwrap_or(r.started_at) as i64;
            started.insert(r.handle.clone(), at);
            of(&r, Some(at))
        })
        .collect();
    let running = runs.len();
    runs.extend(
        state
            .runs
            .queue
            .waiting_ids()
            .iter()
            .filter_map(|id| state.runs.registry.get(id).map(|r| of(&r, None))),
    );
    let (starts, clear) = queue_starts(&runs, state.runs.queue.max_runs, now);
    QueueEstimate {
        started,
        starts: if open { starts } else { Default::default() },
        // With a free slot and nothing waiting, a run that joins starts at once.
        clear_at: (open && (running >= state.runs.queue.max_runs || runs.len() > running))
            .then_some(clear)
            .filter(|at| *at > now),
        open,
    }
}

/// The ticks recorded of each schedule, for [`estimate`]; none when they cannot be read (the
/// estimates then use no typical run time).
async fn recorded_ticks(state: &AppState) -> std::sync::Arc<crate::read_cache::ScheduleFacts> {
    crate::schedule_view::published_ticks(state)
        .await
        .unwrap_or_default()
}

/// [`view`] with each run's start: `started_at` of a running run, `estimated_start_at` of a
/// waiting one, and how long each waits ([`estimate`]).
pub(crate) async fn view_with_starts(state: &AppState) -> QueueView {
    let mut view = view(state);
    let facts = recorded_ticks(state).await;
    let now = chrono::Utc::now().timestamp();
    let estimate = estimate(state, &facts.ticks, now);
    let waited = |entry: &QueueEntry, until: i64| {
        barca_service::time::parse_rfc3339(&entry.queued_at)
            .map(|from| (until - from).max(0) as u64)
    };
    for entry in &mut view.running {
        if let Some(&at) = estimate.started.get(&entry.run_id) {
            entry.started_at = barca_service::time::rfc3339_secs(at);
            entry.wait_seconds = waited(entry, at);
        }
    }
    for entry in &mut view.queued {
        if let Some(&at) = estimate.starts.get(&entry.run_id) {
            let at = barca_service::estimates::rounded(at, now);
            entry.estimated_start_at = barca_service::time::rfc3339_secs(at);
            entry.wait_seconds = waited(entry, at);
        }
    }
    view
}

/// The queue as this server holds it.
pub fn view(state: &AppState) -> QueueView {
    let draining = state.lifecycle.is_draining();
    let queued = state
        .runs
        .queue
        .waiting_ids()
        .iter()
        .enumerate()
        .filter_map(|(at, id)| {
            state
                .runs
                .registry
                .get(id)
                .map(|r| entry(state, &r, Some(at + 1), draining))
        })
        .collect();
    let mut running: Vec<QueueEntry> = state
        .runs
        .registry
        .iter()
        .filter(|r| r.status == RunStatus::Running)
        .map(|r| entry(state, &r, None, draining))
        .collect();
    running.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));
    let paused_at = state.runs.queue.paused_at();
    QueueView {
        paused: paused_at.is_some(),
        paused_at: paused_at.map(barca_service::outcome::rfc3339),
        draining: state.lifecycle.is_draining(),
        max_runs: Some(state.runs.queue.max_runs),
        slots: barca_service::run_list::queue_slots_line(running.len(), state.runs.queue.max_runs),
        running,
        queued,
        capacity: state.runs.capacity.as_ref().map(|c| c.snapshot()),
    }
}

/// A `--read-only` server runs nothing: its view is what the metadata DB records, read from a
/// snapshot (the queue of the server that writes it, as of its last write).
pub(crate) async fn recorded_view(state: &AppState) -> Result<QueueView, ApiError> {
    let snapshot = state.reads.snapshot_db().await?;
    let rows = run_queue::list(&snapshot.path).await?;
    let paused_at = run_queue::paused_at(&snapshot.path).await?;
    let now = chrono::Utc::now().timestamp();
    let timing = if rows.is_empty() {
        barca_service::liveness::RecordedTiming {
            max_runs: barca_service::liveness::serving(&snapshot.path)
                .await?
                .map(|s| s.max_runs),
            ..Default::default()
        }
    } else {
        let dag = state.reads.inspect_dag().await?;
        crate::schedule_view::recorded_queue(state, &dag, &snapshot.path, now).await?
    };
    // What each row's description says: this server wrote it ([`crate::queue::store`]).
    let rows = rows
        .into_iter()
        .filter_map(|row| {
            let (spec, accepted_at) = crate::queue::load(&row.spec).ok()?;
            Some(barca_service::served::RecordedRun {
                run_id: row.run_id,
                state: row.state,
                accepted_at,
                command: spec.command.as_str().into(),
                targets: spec.targets,
                trigger: spec.trigger,
            })
        })
        .collect();
    let view = barca_service::served::recorded_queue(
        rows,
        paused_at,
        &timing,
        state.lifecycle.is_draining(),
        now,
    );
    state.runs.queue.note_recorded(&view);
    Ok(view)
}

/// The rows of `barca.queue` (`POST /sql/query`): the entries of `GET /queue`, the started
/// runs first, then the waiting ones in order. A `--read-only` server lists the recorded
/// queue, without starts or estimates, as its `GET /queue` does.
pub(crate) async fn table_rows(state: &AppState) -> Result<Vec<QueueEntry>, ApiError> {
    let view = match state.config.read_only {
        true => recorded_view(state).await?,
        false => view_with_starts(state).await,
    };
    Ok(view.running.into_iter().chain(view.queued).collect())
}

/// `queue` of `GET /health`: whether the queue is paused and how many runs wait and execute.
/// A `--read-only` server runs nothing itself, so it reports the queue the metadata DB records
/// (what `GET /queue` lists), not its own empty one. `/health` never waits on a read: it reports
/// the last recorded counts (read by `GET /queue`, or in the background when they are older than
/// a few seconds) and its own counts until there are any.
pub fn health(state: &AppState) -> barca_service::served::HealthQueue {
    let mut max_runs = Some(state.runs.queue.max_runs);
    let mut paused = state.runs.queue.paused_at().is_some();
    let mut queued = state.runs.queue.waiting_ids().len();
    let mut running = state
        .runs
        .registry
        .iter()
        .filter(|r| r.status == RunStatus::Running)
        .count();
    if state.config.read_only {
        let (recorded, old) = state.runs.queue.recorded(std::time::Duration::from_secs(5));
        max_runs = recorded.and_then(|c| c.max_runs);
        if let Some(c) = recorded {
            (paused, queued, running) = (c.paused, c.queued, c.running);
        }
        if old && !state.runs.queue.recording.swap(true, Ordering::AcqRel) {
            let state = state.clone();
            tokio::spawn(async move {
                let _ = recorded_view(&state).await;
                state.runs.queue.recording.store(false, Ordering::Release);
            });
        }
    }
    barca_service::served::HealthQueue {
        paused,
        queued,
        running,
        max_runs,
        capacity: state.runs.capacity.as_ref().map(|c| {
            let s = c.snapshot();
            barca_service::served::HealthCapacity {
                total: s.total,
                in_use: s.in_use,
                waiting: s.waiting.len(),
            }
        }),
    }
}
