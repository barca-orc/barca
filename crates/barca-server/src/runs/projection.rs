//! What the run routes answer with, computed from the durable record and the live state: one
//! run in full ([`view_page`]) and the rows of the run list ([`summary`], [`live_summary`]).

use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::state::{AppState, RunState, RunStatus};
use barca_core::RunEvent;
use barca_core::db;
use barca_core::outcome::{Outcome, RunView};
use barca_core::step_status::StepStatus;
use barca_core::time::{rfc3339_millis as timestamp, sql_datetime_rfc3339 as durable_timestamp};
use serde::{Deserialize, Serialize};

pub(crate) async fn view(state: &AppState, id: &str) -> Result<RunView, ApiError> {
    view_page(state, id, Some(db::RUN_STEPS_LIMIT)).await
}

/// [`view`], listing at most `limit` steps (every one with `None`).
pub(crate) async fn view_page(
    state: &AppState,
    id: &str,
    limit: Option<usize>,
) -> Result<RunView, ApiError> {
    let snapshot = snapshot_db(state).await?;
    let live = state.runs.get(id).map(|r| r.clone());
    let record = db::get_run(&snapshot.path, id).await?;
    let stored = match &live {
        Some(RunState {
            outcome: Some(_), ..
        }) => None,
        _ => db::get_run_outcome(&snapshot.path, id)
            .await?
            .and_then(|text| Outcome::from_text(text).ok()),
    };
    let outcome = live.as_ref().and_then(|r| r.outcome.clone()).or(stored);
    let mut view = match (&record, &live) {
        (Some(record), _) => RunView {
            run_id: record.run_id.clone(),
            status: record.status.clone(),
            command: record.command.clone(),
            target: record.target.clone(),
            files: record.files.clone(),
            triggered_by: record.triggered_by,
            scheduled_at: record.scheduled_at.clone(),
            schedule_id: record.schedule_id.clone(),
            started_at: durable_timestamp(&record.started_at),
            finished_at: record.finished_at.as_deref().map(durable_timestamp),
            elapsed_seconds: record.elapsed_seconds,
            steps_total: record.steps_total,
            steps_executed: record.steps_executed,
            steps_cached: record.steps_cached,
            job: record.job.clone(),
            attempts: record.attempts,
            client_run_id: record.client_run_id.clone(),
            failure: record.failure.clone(),
            outcome: None,
            error: None,
            steps: Vec::new(),
            steps_page: db::StepsPage::default(),
            logs: Vec::new(),
        },
        (None, Some(run)) => RunView {
            run_id: run.handle.clone(),
            status: live_status(run.status).into(),
            command: run.command.clone(),
            target: run.target.clone(),
            files: if run.files.is_empty() {
                state.config.files.clone()
            } else {
                run.files.clone()
            },
            triggered_by: Some(run.trigger.triggered_by),
            scheduled_at: run.trigger.scheduled_at.clone(),
            schedule_id: run.trigger.schedule_id.clone(),
            started_at: timestamp(run.started_at),
            finished_at: run.finished_at.map(timestamp),
            elapsed_seconds: run.finished_at.map(|t| t - run.started_at),
            steps_total: None,
            steps_executed: 0,
            steps_cached: 0,
            job: None,
            attempts: None,
            client_run_id: None,
            failure: None,
            outcome: None,
            error: None,
            steps: Vec::new(),
            steps_page: db::StepsPage::default(),
            logs: Vec::new(),
        },
        (None, None) => return Err(ApiError::NotFound(format!("run '{id}' not found"))),
    };
    // A live run is reported as live until it has recorded its outcome, even when its row
    // already says how it ended.
    if let Some(run) = &live
        && matches!(run.status, RunStatus::Pending | RunStatus::Running)
    {
        view.status = live_status(run.status).into();
    }
    if let Some(outcome) = &outcome {
        view.status = outcome.status().as_str().into();
        view.error = outcome.envelope().map(|e| e.error);
    }
    view.outcome = outcome;
    let (steps, mut steps_page) = db::get_run_steps_page(&snapshot.path, id, limit).await?;
    view.steps = steps;
    view.logs = db::get_logs(&snapshot.path, id).await?;
    if let Some(run) = &live
        && let Some(channel) = state.events.get(&run.handle)
    {
        // The steps it streamed and has not recorded yet are listed after the recorded ones.
        let mut recorded: std::collections::HashSet<String> = if steps_page.truncated {
            db::get_run_steps_page(&snapshot.path, id, None)
                .await?
                .0
                .into_iter()
                .map(|s| s.node_id)
                .collect()
        } else {
            view.steps.iter().map(|s| s.node_id.clone()).collect()
        };
        // Replay contains only its retained tail. Recorded logs remain canonical at wrap-up.
        let live_logs = view.logs.is_empty();
        for event in channel.snapshot() {
            match event {
                RunEvent::Log {
                    node_id,
                    line,
                    stream,
                    ts,
                } if live_logs => view.logs.push(db::LogEntry {
                    node_id,
                    seq: view.logs.len() as i64,
                    line,
                    stream,
                    ts,
                }),
                RunEvent::StepFinished {
                    node_id,
                    ok,
                    cached,
                    elapsed_seconds,
                    error,
                    ..
                } if recorded.insert(node_id.clone()) => {
                    steps_page.total += 1;
                    if limit.is_some_and(|n| view.steps.len() >= n) {
                        continue;
                    }
                    view.steps.push(db::RunStep {
                        node_id,
                        status: match (cached, ok) {
                            (true, _) => StepStatus::Cached,
                            (_, true) => StepStatus::Success,
                            _ => StepStatus::Failed,
                        }
                        .as_str()
                        .into(),
                        elapsed_seconds,
                        error,
                        ..Default::default()
                    })
                }
                _ => {}
            }
        }
    }
    steps_page.truncated = view.steps.len() < steps_page.total;
    // The steps streamed and not yet recorded get their trace ids as the recorded ones did.
    if record.is_some() && db::get_run_traced(&snapshot.path, &view.run_id).await? {
        db::attach_trace_ids(&mut view.steps, &view.run_id);
    }
    // A finished run's planned nodes, the skipped and cached ones included, when today's
    // sources still plan what it ran (#758). Live runs stream their steps instead.
    if let Some(record) = &record
        && live.as_ref().is_none_or(|run| run.outcome.is_some())
        && !matches!(view.status.as_str(), "pending" | "running")
        && let Some(job) = &record.job
        && let Ok(dag) = state.inspect_dag().await
    {
        let targets: Vec<&str> = if job == "all" {
            Vec::new()
        } else {
            job.split(',').collect()
        };
        let recorded = if steps_page.nodes.is_empty() {
            db::run_node_summaries(&snapshot.path, id).await?
        } else {
            steps_page.nodes.clone()
        };
        if let Some(nodes) = db::planned_run_nodes(
            &dag,
            &record.command,
            &targets,
            &view.status,
            record.steps_total,
            recorded,
        ) {
            steps_page.nodes = nodes;
        }
    }
    view.steps_page = steps_page;
    if view.error.is_none() {
        view.error = view.steps.iter().find_map(|step| step.error.clone());
    }
    Ok(view)
}

pub(super) fn live_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Complete => "success",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

// ─── the run list ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunSummary {
    pub id: String,
    pub handle: Option<String>,
    pub run_id: Option<String>,
    pub command: String,
    pub files: Vec<String>,
    pub target: Option<String>,
    pub status: String,
    pub triggered_by: Option<barca_core::outcome::TriggeredBy>,
    pub scheduled_at: Option<String>,
    pub schedule_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub steps_total: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_executed: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_cached: i64,
    /// Recorded steps that failed, a partitioned node's keys each counted (#758); absent for
    /// a run not yet recorded. Experimental.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null", optional))]
    pub steps_failed: Option<i64>,
    pub error: Option<String>,
    /// The run's first failed step and its error; `null` when no step failed.
    pub first_failure: Option<FirstFailure>,
    /// With `?status=running` only: the nodes the run is working on now.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub active_steps: Option<Vec<ActiveStep>>,
}
/// A run's first failed step (`RunSummary::first_failure`).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FirstFailure {
    /// The step's node id, with its partition key when it has one.
    pub node_id: String,
    /// `ErrorType: message`, the message's first line only.
    pub error: String,
}
/// A node an in-flight run is working on (`GET /runs?status=running`).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActiveStep {
    pub node_id: String,
    pub step: crate::running::RunningStep,
    pub partitions: Option<crate::running::PartitionProgress>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunList {
    pub runs: Vec<RunSummary>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    pub truncated: bool,
}
#[derive(Default, Deserialize)]
pub struct RunsQuery {
    pub limit: Option<usize>,
    /// `running`: only the runs in flight (pending or running), this server's and the CLI's.
    pub status: Option<RunsFilter>,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunsFilter {
    Running,
}

pub(super) fn summary(record: db::RunRecord, live: Option<&RunState>) -> RunSummary {
    RunSummary {
        id: record.run_id.clone(),
        handle: live.map(|r| r.handle.clone()),
        run_id: Some(record.run_id),
        command: record.command,
        files: record.files,
        target: record.target,
        status: live.map_or(record.status, |r| live_status(r.status).into()),
        triggered_by: record.triggered_by,
        scheduled_at: record.scheduled_at,
        schedule_id: record.schedule_id,
        started_at: durable_timestamp(&record.started_at),
        finished_at: record.finished_at.map(|t| durable_timestamp(&t)),
        elapsed_seconds: record.elapsed_seconds,
        steps_total: record.steps_total,
        steps_executed: record.steps_executed,
        steps_cached: record.steps_cached,
        steps_failed: record.steps_failed,
        error: live.and_then(|r| r.error.clone()),
        first_failure: record.failure.map(|f| FirstFailure {
            node_id: f.node,
            error: f.error,
        }),
        active_steps: None,
    }
}
pub(super) fn live_summary(run: &RunState, files: &[String]) -> RunSummary {
    RunSummary {
        id: run.handle.clone(),
        handle: Some(run.handle.clone()),
        run_id: run.db_run_id.clone(),
        command: run.command.clone(),
        files: files.to_vec(),
        target: run.target.clone(),
        status: live_status(run.status).into(),
        triggered_by: Some(run.trigger.triggered_by),
        scheduled_at: run.trigger.scheduled_at.clone(),
        schedule_id: run.trigger.schedule_id.clone(),
        started_at: timestamp(run.started_at),
        finished_at: run.finished_at.map(timestamp),
        elapsed_seconds: run.finished_at.map(|t| t - run.started_at),
        steps_total: run.result.as_ref().map(|r| r.steps.len() as i64),
        steps_executed: run.result.as_ref().map_or(0, |r| r.steps_executed as i64),
        steps_cached: run.result.as_ref().map_or(0, |r| {
            r.steps
                .iter()
                .filter(|s| StepStatus::Cached.is(s.status.as_deref()))
                .count() as i64
        }),
        steps_failed: None,
        error: run.error.clone(),
        // Not in the database yet: nothing has recorded a failed step.
        first_failure: None,
        active_steps: None,
    }
}
pub(super) fn live_runs(state: &AppState) -> Vec<RunState> {
    state.runs.iter().map(|r| r.value().clone()).collect()
}
