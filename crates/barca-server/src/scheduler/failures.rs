//! What the scheduler keeps about each job between ticks ([`Bookkeeping`]), and the failing
//! rule (RFC-0008 rule 9): consecutive failed ticks are counted, a job at its `max_failures`
//! is skipped until something it failed on succeeds again.

use crate::state::{AppState, RunStatus};
use barca_core::db;
use barca_core::schedule::ScheduledJob;
use chrono::{DateTime, TimeZone};
use std::collections::{HashMap, HashSet};

use super::tick::due_jobs;

/// What the scheduler records about each job between ticks, for `GET /schedule`.
#[derive(Debug, Default)]
pub(super) struct Bookkeeping {
    /// Handle of the last run the scheduler triggered, per job.
    pub(super) last_handle: HashMap<String, String>,
    /// Last-fired epoch per job; persisted for catch-up.
    pub(super) last_fired: HashMap<String, i64>,
    /// Last skipped tick per job: its epoch, why (`in_flight`, `paused`) and the run in
    /// flight that caused it.
    pub(super) last_skip: HashMap<String, (i64, &'static str, Option<String>)>,
    /// Consecutive failed ticks per job (RFC-0008 rule 9); persisted for restarts.
    pub(super) failures: HashMap<String, db::ScheduleFailures>,
    /// Handles of tick runs whose outcome has been counted.
    pub(super) counted: HashSet<String>,
}

impl Bookkeeping {
    pub(super) fn failures(&self, job: &ScheduledJob) -> u32 {
        self.failures.get(&job.id).map_or(0, |f| f.failures)
    }
}

/// How a finished tick run ended: `None` while it is pending or running, or when it was
/// cancelled (which is not a failure of the job); otherwise the nodes that failed, empty on
/// success. A failed run with no failed step (it could not start) failed on the scheduled node.
pub(super) fn tick_outcome(
    run: &crate::state::RunState,
    job: &ScheduledJob,
) -> Option<Vec<String>> {
    match run.status {
        RunStatus::Pending | RunStatus::Running | RunStatus::Cancelled => None,
        RunStatus::Complete => Some(Vec::new()),
        RunStatus::Failed => {
            let mut nodes = failed_steps(run);
            nodes.dedup();
            if nodes.is_empty() {
                nodes.push(job.id.clone());
            }
            Some(nodes)
        }
    }
}

/// The ids of the steps that failed in a finished run: from its outcome (`steps`, and each
/// failed target's `failed_node` of a multi-target run), or from its single-target result.
pub(super) fn failed_steps(run: &crate::state::RunState) -> Vec<String> {
    let mut nodes: Vec<String> = Vec::new();
    let outcome: Option<serde_json::Value> = run
        .outcome
        .as_ref()
        .and_then(|o| serde_json::from_str(o.text()).ok());
    let steps = outcome
        .as_ref()
        .and_then(|o| o["steps"].as_array().cloned())
        .unwrap_or_default();
    for step in &steps {
        if step["status"] == "failed"
            && let Some(id) = step["id"].as_str()
        {
            nodes.push(id.to_string());
        }
    }
    if let Some(targets) = outcome.as_ref().and_then(|o| o["targets"].as_object()) {
        for target in targets.values() {
            if target["status"] == "failed"
                && let Some(node) = target["failed_node"].as_str()
                && !nodes.iter().any(|n| n == node)
            {
                nodes.push(node.to_string());
            }
        }
    }
    if nodes.is_empty() {
        nodes = run
            .result
            .iter()
            .flat_map(|r| &r.steps)
            .filter(|s| s.status.as_deref() == Some("failed"))
            .map(|s| s.id.clone())
            .collect();
    }
    nodes
}

/// Count the outcome of each job's last tick run that has finished since the last call: a
/// failure adds one to the job's consecutive failures, a success clears them.
pub(super) async fn count_outcomes(
    state: &AppState,
    jobs: &[ScheduledJob],
    book: &mut Bookkeeping,
    db_path: Option<&str>,
) -> bool {
    let mut changed = false;
    for job in jobs {
        let Some(handle) = book.last_handle.get(&job.id).cloned() else {
            continue;
        };
        if book.counted.contains(&handle) {
            continue;
        }
        let Some(failed) = state.runs.get(&handle).and_then(|r| tick_outcome(&r, job)) else {
            continue;
        };
        book.counted.insert(handle);
        let before = book.failures(job);
        let next = if failed.is_empty() {
            db::ScheduleFailures::default()
        } else {
            let mark = match db_path {
                Some(dbp) => db::max_materialization_id(dbp).await.unwrap_or(0),
                None => 0,
            };
            db::ScheduleFailures {
                failures: before + 1,
                failed_nodes: failed,
                mark,
            }
        };
        if next.failures == before && before == 0 {
            continue;
        }
        if next.failures >= job.max_failures && before < job.max_failures {
            barca_core::jsonlog::line(
                barca_core::jsonlog::Level::Error,
                Some(&job.id),
                &format!(
                    "[barca] {} failing: {} consecutive failed ticks (max_failures={}); skipped \
                     until an explicit request or a change in its inputs",
                    job.id, next.failures, job.max_failures
                ),
            );
        }
        if let Some(dbp) = db_path {
            persist_failures(dbp, job, &next).await;
        }
        book.failures.insert(job.id.clone(), next);
        changed = true;
    }
    changed
}

/// Whether a failing job may fire again: a success recorded after its last counted failure for
/// the scheduled node (an explicit request), a node it failed on, or a direct input of one (an
/// input change).
pub(super) async fn failing_reset(
    state: &AppState,
    job: &ScheduledJob,
    failures: &db::ScheduleFailures,
    db_path: &str,
) -> bool {
    let mut watch: Vec<String> = vec![job.id.clone()];
    watch.extend(failures.failed_nodes.iter().cloned());
    if let Ok(dag) = state.loaded_dag().await {
        for node in &failures.failed_nodes {
            watch.extend(dag.upstream(node).into_iter().map(str::to_string));
        }
    }
    db::succeeded_since(db_path, failures.mark, &watch)
        .await
        .unwrap_or(false)
}

/// Clear the failures of each due failing job that may fire again ([`failing_reset`]).
pub(super) async fn reset_failing<Tz: TimeZone>(
    state: &AppState,
    now: &DateTime<Tz>,
    jobs: &[ScheduledJob],
    book: &mut Bookkeeping,
    db_path: &str,
) {
    for job in due_jobs(now, jobs) {
        let Some(failures) = book.failures.get(&job.id) else {
            continue;
        };
        if failures.failures < job.max_failures
            || !failing_reset(state, job, failures, db_path).await
        {
            continue;
        }
        barca_core::jsonlog::line(
            barca_core::jsonlog::Level::Info,
            Some(&job.id),
            &format!(
                "[barca] {} no longer failing: something it failed on succeeded since",
                job.id
            ),
        );
        book.failures.remove(&job.id);
        persist_failures(db_path, job, &db::ScheduleFailures::default()).await;
    }
}

/// Record a job's consecutive failed ticks. Best-effort durability, like [`persist_fired`].
pub(super) async fn persist_failures(
    db_path: &str,
    job: &ScheduledJob,
    failures: &db::ScheduleFailures,
) {
    if let Err(e) = db::set_schedule_failures(db_path, &job.id, failures).await {
        barca_core::errln!("[barca] schedule_failures write failed for {}: {e}", job.id);
    }
}
