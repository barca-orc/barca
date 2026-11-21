//! What the scheduler keeps about each job between ticks ([`Bookkeeping`]), and the failing
//! rule (RFC-0008 rule 9): consecutive failed ticks are counted, a job at its `max_failures`
//! is skipped until something it failed on succeeds again.

use crate::run_model::RunStatus;
use crate::state::AppState;
use barca_service::db;
use barca_service::schedule::ScheduledJob;
use barca_service::step_status::StepStatus;
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
    /// Earlier runs admitted during replay, in admission order.
    outstanding: HashMap<String, std::collections::VecDeque<String>>,
    pub(super) dirty_failures: HashSet<String>,
    /// When the tick record was last trimmed by age (unix seconds); 0 before the first trim.
    pub(super) ticks_trimmed_at: i64,
}

impl Bookkeeping {
    pub(super) fn accepted(&mut self, job: &ScheduledJob, handle: String) {
        if let Some(previous) = self.last_handle.insert(job.id.clone(), handle) {
            self.outstanding
                .entry(job.id.clone())
                .or_default()
                .push_back(previous);
        }
    }

    pub(super) fn failures(&self, job: &ScheduledJob) -> u32 {
        self.failures.get(&job.id).map_or(0, |f| f.failures)
    }
}

/// How a finished tick run ended: `None` while it is pending or running, or when it was
/// cancelled (which is not a failure of the job); otherwise the nodes that failed, empty on
/// success. A failed run with no failed step (it could not start) failed on the scheduled node.
pub(super) fn tick_outcome(
    run: &crate::run_model::RunState,
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
pub(super) fn failed_steps(run: &crate::run_model::RunState) -> Vec<String> {
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
        if step["status"] == StepStatus::Failed.as_str()
            && let Some(id) = step["id"].as_str()
        {
            nodes.push(id.to_string());
        }
    }
    if let Some(targets) = outcome.as_ref().and_then(|o| o["targets"].as_object()) {
        for target in targets.values() {
            if target["status"] == StepStatus::Failed.as_str()
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
            .filter(|s| StepStatus::Failed.is(s.status.as_deref()))
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
        let previous = book.outstanding.get(&job.id).and_then(|q| q.front());
        let Some(handle) = previous.or_else(|| book.last_handle.get(&job.id)) else {
            continue;
        };
        let Some(failed) = state
            .runs
            .registry
            .get(handle)
            .and_then(|r| tick_outcome(&r, job))
        else {
            continue;
        };
        if previous.is_some() {
            book.outstanding.get_mut(&job.id).unwrap().pop_front();
        } else {
            book.last_handle.remove(&job.id);
        }
        let before = book.failures(job);
        let next = if failed.is_empty() {
            db::ScheduleFailures::default()
        } else {
            let mark = match db_path {
                Some(dbp) => super::metadata("failure watermark", db::max_materialization_id(dbp))
                    .await
                    .unwrap_or_else(|e| {
                        barca_service::errln!("[barca] {e}");
                        0
                    }),
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
            barca_service::jsonlog::line(
                barca_service::jsonlog::Level::Error,
                Some(&job.id),
                &format!(
                    "[barca] {} failing: {} consecutive failed ticks (max_failures={}); skipped \
                     until an explicit request or a change in its inputs",
                    job.id, next.failures, job.max_failures
                ),
            );
        }
        book.dirty_failures.insert(job.id.clone());
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
    if let Ok(Ok(dag)) = tokio::time::timeout(super::METADATA_WAIT, state.reads.loaded_dag()).await
    {
        for node in &failures.failed_nodes {
            watch.extend(dag.upstream(node).into_iter().map(str::to_string));
        }
    }
    super::metadata(
        "failure reset",
        db::succeeded_since(db_path, failures.mark, &watch),
    )
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
        barca_service::jsonlog::line(
            barca_service::jsonlog::Level::Info,
            Some(&job.id),
            &format!(
                "[barca] {} no longer failing: something it failed on succeeded since",
                job.id
            ),
        );
        book.failures.remove(&job.id);
        book.dirty_failures.insert(job.id.clone());
    }
}

/// Record a job's consecutive failed ticks. Best-effort durability, like [`persist_fired`].
pub(super) async fn persist_failures(db_path: &str, job: &str, failures: &db::ScheduleFailures) {
    if let Err(e) = super::metadata(
        "persist_failures",
        db::set_schedule_failures(db_path, job, failures),
    )
    .await
    {
        barca_service::errln!("[barca] schedule_failures write failed for {}: {e}", job);
    }
}
