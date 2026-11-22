//! What the run routes answer with: one run in full ([`view_page`]) and the run list, both
//! computed in `barca_service::served` from the durable record. This module fills what only the
//! server holds: its runs in memory, as the overlay ([`live_of`]), and the events a run
//! streamed.

use crate::error::ApiError;
use crate::run_model::{RunState, RunStatus};
use crate::state::AppState;
use barca_service::db;
use barca_service::outcome::RunView;
use barca_service::served::{LiveRun, LiveStatus, LiveSteps};
use barca_service::step_status::StepStatus;

pub use barca_service::served::{RunList, RunsQuery};

pub(crate) async fn view(state: &AppState, id: &str) -> Result<RunView, ApiError> {
    view_page(state, id, Some(db::RUN_STEPS_LIMIT), 0).await
}

/// [`view`], listing at most `limit` steps (every one with `None`) from the `offset`-th.
pub(crate) async fn view_page(
    state: &AppState,
    id: &str,
    limit: Option<usize>,
    offset: usize,
) -> Result<RunView, ApiError> {
    let snapshot = state.reads.snapshot_db().await?;
    let live = state.runs.registry.get(id).map(|r| live_of(state, &r));
    let events = live
        .as_ref()
        .and_then(|run| state.runs.events.get(&run.handle))
        .map(|channel| channel.snapshot());
    let draft = barca_service::served::run_view(barca_service::served::RunViewRequest {
        read_path: &snapshot.path,
        db_path: &state.config.resolved.db_path,
        run_id: id,
        limit,
        offset,
        live: live.as_ref(),
        events,
        default_files: &state.config.files,
        logs: true,
    })
    .await?;
    let Some(mut draft) = draft else {
        return Err(ApiError::NotFound(format!("run '{id}' not found")));
    };
    // Planned over the deployed graph, when today's sources still plan what it ran (#758).
    if draft.plans().is_some()
        && let Ok(dag) = state.reads.inspect_dag().await
    {
        draft.plan(&dag).await?;
    }
    Ok(draft.finish().view)
}

pub(super) fn live_status(status: RunStatus) -> LiveStatus {
    match status {
        RunStatus::Pending => LiveStatus::Pending,
        RunStatus::Running => LiveStatus::Running,
        RunStatus::Complete => LiveStatus::Complete,
        RunStatus::Skipped => LiveStatus::Skipped,
        RunStatus::Failed => LiveStatus::Failed,
        RunStatus::Cancelled => LiveStatus::Cancelled,
    }
}

/// A run this server holds, as the overlay the views read ([`LiveRun`]).
pub(crate) fn live_of(state: &AppState, run: &RunState) -> LiveRun {
    LiveRun {
        handle: run.handle.clone(),
        db_run_id: run.db_run_id.clone(),
        status: live_status(run.status),
        command: run.command.clone(),
        target: run.target.clone(),
        files: run.files.clone(),
        trigger: run.trigger.clone(),
        started_at: run.started_at,
        finished_at: run.finished_at,
        outcome: run.outcome.clone(),
        error: run.error.clone(),
        cancellation: run.cancellation.clone(),
        waiting_for: state.lifecycle.waiting_for(run.status),
        queue: run.queue,
        steps: run.result.as_ref().map(|r| LiveSteps {
            total: r.steps.len() as i64,
            executed: r.steps_executed as i64,
            cached: r
                .steps
                .iter()
                .filter(|s| StepStatus::Cached.is(s.status.as_deref()))
                .count() as i64,
        }),
    }
}

/// Every run this server holds, as the overlay of the run list.
pub(super) fn live_runs(state: &AppState) -> Vec<LiveRun> {
    state
        .runs
        .registry
        .iter()
        .map(|r| live_of(state, r.value()))
        .collect()
}
