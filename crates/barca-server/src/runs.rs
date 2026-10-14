//! Read-only run inspection joins durable history with this server's live handles.
use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::state::{AppState, RunState};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use barca_core::{db, results::GetResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub steps_total: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_executed: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_cached: i64,
    pub error: Option<String>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunList {
    pub runs: Vec<RunSummary>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    pub truncated: bool,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunDetail {
    pub run: RunSummary,
    pub steps: Vec<db::RunStep>,
    pub logs: Vec<db::LogEntry>,
    pub result: Option<GetResult>,
}
#[derive(Default, Deserialize)]
pub struct RunsQuery {
    pub limit: Option<usize>,
}

fn timestamp(at: f64) -> String {
    chrono::DateTime::from_timestamp_millis((at * 1000.0) as i64)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}
fn durable_timestamp(at: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S")
        .map(|t| {
            t.and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| at.to_string())
}
fn summary(record: db::RunRecord, live: Option<&RunState>) -> RunSummary {
    RunSummary {
        id: record.run_id.clone(),
        handle: live.map(|r| r.handle.clone()),
        run_id: Some(record.run_id),
        command: record.command,
        files: record.files,
        target: record.target,
        status: live.map_or(record.status, |r| {
            match r.status {
                crate::state::RunStatus::Pending => "pending",
                crate::state::RunStatus::Running => "running",
                crate::state::RunStatus::Complete => "success",
                crate::state::RunStatus::Failed => "failed",
                crate::state::RunStatus::Cancelled => "cancelled",
            }
            .into()
        }),
        started_at: durable_timestamp(&record.started_at),
        finished_at: record.finished_at.map(|t| durable_timestamp(&t)),
        elapsed_seconds: record.elapsed_seconds,
        steps_total: record.steps_total,
        steps_executed: record.steps_executed,
        steps_cached: record.steps_cached,
        error: live.and_then(|r| r.error.clone()),
    }
}
fn live_summary(run: &RunState, files: &[String]) -> RunSummary {
    RunSummary {
        id: run.db_run_id.clone().unwrap_or_else(|| run.handle.clone()),
        handle: Some(run.handle.clone()),
        run_id: run.db_run_id.clone(),
        command: run.command.clone(),
        files: files.to_vec(),
        target: run.target.clone(),
        status: serde_json::to_value(run.status)
            .unwrap()
            .as_str()
            .unwrap()
            .into(),
        started_at: timestamp(run.started_at),
        finished_at: run.finished_at.map(timestamp),
        elapsed_seconds: run.finished_at.map(|t| t - run.started_at),
        steps_total: run.result.as_ref().map(|r| r.steps.len() as i64),
        steps_executed: run.result.as_ref().map_or(0, |r| r.steps_executed as i64),
        steps_cached: run.result.as_ref().map_or(0, |r| {
            r.steps
                .iter()
                .filter(|s| s.status.as_deref() == Some("cached"))
                .count() as i64
        }),
        error: run.error.clone(),
    }
}
fn live_runs(state: &AppState) -> Vec<RunState> {
    state.runs.iter().map(|r| r.value().clone()).collect()
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<RunList>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let snapshot = snapshot_db(&state).await?;
    // Clone after the snapshot: a new live run not yet in the snapshot stays visible.
    let live = live_runs(&state);
    let by_id: HashMap<_, _> = live
        .iter()
        .filter_map(|r| r.db_run_id.as_deref().map(|id| (id, r)))
        .collect();
    let records = db::get_recent_runs(&snapshot.path, limit).await?;
    let durable_total = db::count_runs(&snapshot.path).await?;
    let mut runs: Vec<_> = records
        .into_iter()
        .map(|r| {
            let handle = by_id.get(r.run_id.as_str()).copied();
            summary(r, handle)
        })
        .collect();
    let mut extra = 0;
    for run in &live {
        if runs
            .iter()
            .any(|r| r.run_id.as_deref() == run.db_run_id.as_deref() && r.run_id.is_some())
        {
            continue;
        }
        // An old live handle outside the requested history page must not be counted twice.
        if let Some(id) = &run.db_run_id
            && db::get_run(&snapshot.path, id).await?.is_some()
        {
            continue;
        }
        runs.push(live_summary(run, &state.config.files));
        extra += 1;
    }
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let total = durable_total + extra;
    runs.truncate(limit);
    Ok(Json(RunList {
        truncated: total > runs.len(),
        runs,
        total,
    }))
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    let snapshot = snapshot_db(&state).await?;
    let live = live_runs(&state)
        .into_iter()
        .find(|r| r.handle == id || r.db_run_id.as_deref() == Some(&id));
    let db_id = live
        .as_ref()
        .and_then(|r| r.db_run_id.as_deref())
        .unwrap_or(&id);
    let record = db::get_run(&snapshot.path, db_id).await?;
    let run = match record {
        Some(record) => summary(record, live.as_ref()),
        None => live
            .as_ref()
            .map(|r| live_summary(r, &state.config.files))
            .ok_or_else(|| ApiError::NotFound(format!("run '{id}' not found")))?,
    };
    let mut steps = db::get_run_steps(&snapshot.path, db_id).await?;
    let mut logs = db::get_logs(&snapshot.path, db_id).await?;
    let result = live.as_ref().and_then(|r| r.result.clone());
    if let Some(result) = &result {
        for step in &result.steps {
            if !steps.iter().any(|s| s.node_id == step.id) {
                steps.push(db::RunStep {
                    node_id: step.id.clone(),
                    status: step.status.clone().unwrap_or_else(|| "unknown".into()),
                    elapsed_seconds: None,
                    error: None,
                });
            }
        }
    }
    if let Some(live) = &live
        && let Some(channel) = state.events.get(&live.handle)
    {
        let (events, _) = channel.snapshot_and_subscribe();
        // Live event backlog has all logs; persisted logs only become canonical at wrap-up.
        let live_logs = logs.is_empty();
        for event in events {
            match event {
                barca_core::RunEvent::Log { node_id, line } if live_logs => {
                    logs.push(db::LogEntry {
                        node_id,
                        seq: logs.len() as i64,
                        line,
                    })
                }
                barca_core::RunEvent::StepFinished {
                    node_id,
                    ok,
                    elapsed_seconds,
                    error,
                } if !steps.iter().any(|s| s.node_id == node_id) => steps.push(db::RunStep {
                    node_id,
                    status: if ok { "success" } else { "failed" }.into(),
                    elapsed_seconds,
                    error,
                }),
                _ => {}
            }
        }
    }
    let mut run = run;
    if run.error.is_none() {
        run.error = steps.iter().find_map(|step| step.error.clone());
    }
    Ok(Json(RunDetail {
        run,
        steps,
        logs,
        result,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ServeConfig,
        state::{RunStatus, now_ts},
    };
    fn state(dir: &std::path::Path) -> AppState {
        let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
        resolved.db_path = dir.join("metadata.db").display().to_string();
        AppState::new(ServeConfig {
            files: vec!["p.py".into()],
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: "python3".into(),
            resolved,
            read_only: true,
        })
    }
    #[test]
    fn durable_and_live_timestamps_sort_in_the_same_utc_format() {
        let old = durable_timestamp("2026-10-09 12:00:00");
        let newer = timestamp(
            chrono::NaiveDateTime::parse_from_str("2026-10-09 12:00:00", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc()
                .timestamp() as f64
                + 0.9,
        );
        assert!(newer > old);
        assert_eq!(old, "2026-10-09T12:00:00.000Z");
        assert_eq!(newer, "2026-10-09T12:00:00.900Z");
    }
    #[tokio::test]
    async fn queued_handles_and_failed_durable_runs_are_visible_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let mut run = RunState {
            handle: "handle".into(),
            db_run_id: None,
            command: "get".into(),
            target: Some("first".into()),
            status: RunStatus::Pending,
            result: None,
            error: None,
            started_at: now_ts(),
            finished_at: None,
            cancel: barca_core::CancellationToken::new(),
        };
        state.runs.insert(run.handle.clone(), run.clone());
        let Json(queued) = list(State(state.clone()), Query(RunsQuery::default()))
            .await
            .unwrap();
        assert_eq!(queued.runs.len(), 1);
        assert_eq!(queued.runs[0].id, "handle");
        assert_eq!(queued.runs[0].run_id, None);
        let path = &state.config.resolved.db_path;
        db::init_db(path).await.unwrap();
        db::create_run(path, "durable", "get", "[\"p.py\"]", Some("first"), Some(1))
            .await
            .unwrap();
        db::finish_run(path, "durable", "failed", 0, 0, 1.0)
            .await
            .unwrap();
        run.db_run_id = Some("durable".into());
        run.status = RunStatus::Running;
        state.runs.insert(run.handle.clone(), run.clone());
        let Json(wrapping_up) = detail(State(state.clone()), Path("durable".into()))
            .await
            .unwrap();
        assert_eq!(
            wrapping_up.run.status, "running",
            "a persisted terminal row must not stop live polling before wrap-up"
        );
        run.status = RunStatus::Failed;
        run.error = Some("boom".into());
        state.runs.insert(run.handle.clone(), run);
        let Json(history) = list(State(state.clone()), Query(RunsQuery::default()))
            .await
            .unwrap();
        assert_eq!(history.total, 1);
        assert_eq!(history.runs.len(), 1);
        assert_eq!(history.runs[0].id, "durable");
        for id in ["handle", "durable"] {
            let Json(details) = detail(State(state.clone()), Path(id.into())).await.unwrap();
            assert_eq!(details.run.id, "durable");
            assert_eq!(details.run.error.as_deref(), Some("boom"));
        }
    }
}
