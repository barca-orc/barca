//! The HTTP handlers of the run contract: starting, cancelling, inspecting and listing runs,
//! and the event stream. What they answer with is computed by [`super::projection`]; what
//! happens to a run once accepted is [`super::lifecycle`]'s.

use super::lifecycle::{cancelled, finish, start};
use super::projection::{
    ActiveStep, RunList, RunsFilter, RunsQuery, live_runs, live_status, live_summary, summary,
    view, view_page,
};
use super::{
    RunSpec, Wording, check_any_asset, check_files, check_target, check_target_as, context,
    json_text, refuse_if_read_only, run_files, stdout_of,
};
use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::state::{AppState, RunChannel, RunState, RunStatus};
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::{Json, http::StatusCode};
use barca_core::cache::CachePolicy;
use barca_core::envelope::Context;
use barca_core::outcome::{Outcome, OutcomeStatus, RunCommand, RunRequest, RunView, Trigger};
use barca_core::{BarcaError, db};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// `POST /runs` — start a run on this server: `202 {"run_id"}`, or `200` with the `--dry-run`
/// JSON when `dry_run` is set (nothing starts). The targets are checked first: no run is
/// started for a request a local `barca get` / `barca run` would refuse.
pub async fn create(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    match create_run(state, &body).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

async fn create_run(state: AppState, body: &[u8]) -> Result<Response, ApiError> {
    let request: RunRequest = serde_json::from_slice(body).map_err(|e| {
        ApiError::BadRequest(format!(
            "the request body is not a run request: {e}\nSend JSON such as {{\"command\": \"get\", \"targets\": [\"daily_report\"]}}."
        ))
    })?;
    let files = check_files(&state, &request.files)?;
    if request.command == RunCommand::Run && request.targets.is_empty() {
        return Err(ApiError::BadRequest(
            "a target task is required\n`run` needs the task to run in `targets`.".into(),
        ));
    }
    let partition = partition_of(&request)?;
    batch_of(&request)?;
    if request.dry_run {
        return dry_run(&state, &request, &files).await;
    }
    refuse_if_read_only(&state)?;
    let verb = request.command.as_str();
    let mut nodes = Vec::new();
    for target in &request.targets {
        nodes.push(check_target(&state, target, verb).await?);
    }
    if request.targets.is_empty() {
        check_any_asset(&state).await?;
    }
    let admitted = crate::queue::admit(
        &state,
        RunSpec {
            command: request.command,
            policy: request.policy(),
            targets: request.targets,
            nodes,
            files,
            trigger: Trigger::api(),
            values: request.values,
            partition,
            batch: request.batch.clone(),
            // The run contract runs what `barca get` / `barca run` would run here.
            rules: barca_core::freshness::RunRules::request(),
            timeout: None,
            kill_requeues: 0,
        },
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "run_id": admitted.run_id,
            "status": live_status(admitted.status),
            "position": admitted.position,
        })),
    )
        .into_response())
}

/// `partition`, parsed as `--partition` is; `400` when it does not parse.
/// `RunRequest.batch`, checked (RFC-0013): a bad value is `400`.
fn batch_of(request: &RunRequest) -> Result<barca_core::batch::BatchOverride, ApiError> {
    barca_core::batch::BatchOverride::parse(&request.batch)
        .map_err(|msg| ApiError::BadRequest(format!("error: {msg}")))
}

fn partition_of(
    request: &RunRequest,
) -> Result<Option<barca_core::selection::PartitionSelection>, ApiError> {
    request
        .partition_selection()
        .map_err(|msg| ApiError::BadRequest(format!("error: {msg}")))
}

/// `dry_run`: the `--dry-run --json` body, decided against a private copy of the metadata DB.
/// `files` are the request's files as deployed (`[]`: every deployed file).
async fn dry_run(
    state: &AppState,
    request: &RunRequest,
    files: &[String],
) -> Result<Response, ApiError> {
    let snapshot = snapshot_db(state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let result = barca_core::queries::explain(
        &cfg,
        &request.targets,
        run_files(state, files),
        &state.config.python,
        request.policy(),
        request.command.as_str(),
        partition_of(request)?.as_ref(),
        &batch_of(request)?,
    )
    .await
    .map_err(|e| ApiError::run(e, context(request.command, files)))?;
    let rendered = barca_core::report::render_explain(
        &result,
        request.command.as_str(),
        barca_core::report::ResultFormat::Json,
        None,
    );
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

// ─── older trigger routes ───────────────────────────────────────────────────

/// `POST /run` (alias) — get every asset and sensor; `200 {"run_id"}`.
pub async fn run_all(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    check_any_asset(&state).await?;
    let run_id = start(state, RunSpec::api(RunCommand::Get, vec![], vec![])).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// `POST /run/{target}` (alias) — run a task, recomputing its upstream assets as
/// `--refresh-all` does; `200 {"run_id"}`.
pub async fn run_target(
    State(state): State<AppState>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let node = check_target_as(&state, &target, "run", Wording::Route).await?;
    let mut spec = RunSpec::api(RunCommand::Run, vec![target], vec![node]);
    spec.policy = CachePolicy::RefreshAll;
    Ok(Json(json!({ "run_id": start(state, spec).await? })))
}

/// `POST /get/{target}` (alias) — a cache-aware get of one target; `200 {"run_id"}`.
pub async fn get_target(
    State(state): State<AppState>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let node = check_target_as(&state, &target, "get", Wording::Route).await?;
    let spec = RunSpec::api(RunCommand::Get, vec![target], vec![node]);
    Ok(Json(json!({ "run_id": start(state, spec).await? })))
}

/// `GET /status/{run_id}` (alias) — the live state of a run this server started.
pub async fn status(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunState>, ApiError> {
    state
        .runs
        .get(&run_id)
        .map(|r| Json(r.clone()))
        .ok_or_else(|| ApiError::NotFound(format!("run '{run_id}' not found")))
}

// ─── cancel ─────────────────────────────────────────────────────────────────

/// `DELETE /runs/{run_id}` (and the alias `DELETE /run/{run_id}`) — cancel a pending or
/// running run. A queued run leaves the queue; a running one has its workers terminated.
/// Either ends `cancelled`, and its event stream ends with that outcome. A finished run is
/// `409`.
pub async fn cancel(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    if let Some(waiting) = state.queue.take(&run_id) {
        if let Some(mut run) = state.runs.get_mut(&run_id) {
            run.cancel_requested = true;
            run.cancel.cancel();
        }
        let ctx = context(waiting.spec.command, &waiting.spec.files);
        let outcome = cancelled(&state, &ctx);
        if let Some(channel) = state.events.get(&run_id).map(|c| c.clone()) {
            finish(
                &state,
                &run_id,
                &waiting.spec,
                &channel,
                None,
                outcome,
                false,
                false,
            )
            .await;
        }
        return Ok(Json(json!({ "run_id": run_id, "status": "cancelling" })));
    }
    let finished = match state.runs.get_mut(&run_id) {
        Some(mut run) => match run.status {
            RunStatus::Pending | RunStatus::Running => {
                run.cancel_requested = true;
                run.cancel.cancel();
                return Ok(Json(json!({ "run_id": run_id, "status": "cancelling" })));
            }
            status => json!(status).as_str().unwrap_or("finished").to_string(),
        },
        None => {
            let snapshot = snapshot_db(&state).await?;
            match db::get_run(&snapshot.path, &run_id).await? {
                Some(record) => record.status,
                None => return Err(ApiError::NotFound(format!("run '{run_id}' not found"))),
            }
        }
    };
    Err(ApiError::Conflict(format!(
        "run '{run_id}' already finished ({finished})"
    )))
}

// ─── inspection of one run ──────────────────────────────────────────────────

/// `GET /runs/{run_id}` — the run: what started it, where it is and, once finished, its
/// outcome, with the steps it recorded and the lines they printed.
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(page): Query<StepsQuery>,
) -> Result<Json<RunView>, ApiError> {
    let limit = (!page.all).then(|| page.limit.unwrap_or(db::RUN_STEPS_LIMIT));
    Ok(Json(view_page(&state, &id, limit).await?))
}

/// `GET /runs/{run_id}?limit=N` lists at most N of the run's steps (default
/// [`db::RUN_STEPS_LIMIT`]), `?all=true` every one; `steps_page` says how many there are.
#[derive(Debug, Default, Deserialize)]
pub struct StepsQuery {
    pub limit: Option<usize>,
    #[serde(default)]
    pub all: bool,
}

/// `?step=` and `?stream=` of `GET /runs/{run_id}/logs`.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    step: Option<String>,
    stream: Option<String>,
}

/// `GET /runs/{run_id}/logs` — `{run_id, status, logs}`: the run's recorded lines as
/// `barca logs` reads them (those of a run in progress so far), filtered as `--step` and
/// `--stream` filter them. An unknown run is a 404; an unknown or ambiguous step, or a stream
/// that is neither `stdout` nor `stderr`, a 400.
pub async fn logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Value>, ApiError> {
    let stream = match query.stream.as_deref() {
        None | Some("") => None,
        Some(text) => Some(barca_core::LogStream::parse(text).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "stream '{text}' is not a stream: use stdout or stderr"
            ))
        })?),
    };
    let snapshot = snapshot_db(&state).await?;
    let live = state.runs.get(&id).map(|r| r.status);
    let record = db::get_run(&snapshot.path, &id).await?;
    let mut status = match (&record, live) {
        (Some(record), _) => record.status.clone(),
        (None, Some(live)) => live_status(live).to_string(),
        (None, None) => {
            return Err(ApiError::NotFound(format!(
                "run '{id}' was not found on this server
A run recorded on another machine is \
                 read there (`barca logs RUN_ID --local` where a server is configured)."
            )));
        }
    };
    if let Some(live) = live
        && matches!(live, RunStatus::Pending | RunStatus::Running)
    {
        status = live_status(live).to_string();
    }
    let step = query.step.as_deref().filter(|s| !s.is_empty());
    let lines = if record.is_some() {
        // Errors read as `barca logs` reports them here.
        barca_core::queries::logs_at(
            &state.config.resolved.db_path,
            &snapshot.path,
            &id,
            step,
            stream,
        )
        .await
        .map_err(|e| {
            ApiError::run(
                e,
                Context {
                    command: "logs",
                    files: Vec::new(),
                },
            )
        })?
    } else {
        Vec::new()
    };
    Ok(Json(
        json!({ "run_id": id, "status": status, "logs": lines }),
    ))
}

/// Attempt-scoped SSE from the beginning or strictly after Last-Event-ID. Retention is
/// bounded; expired replay is 410 and lagging readers close without skipping a gap.
/// Terminal cursors return 204. Authentication wraps this handler before cursor inspection.
pub async fn events(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let mut cursor_headers = headers.get_all("last-event-id").iter();
    let cursor = cursor_headers
        .next()
        .map(|header| {
            header
                .to_str()
                .map_err(|_| ApiError::EventCursor(crate::replay::CursorError::Invalid))
        })
        .transpose()?;
    if cursor_headers.next().is_some() {
        return Err(ApiError::EventCursor(crate::replay::CursorError::Invalid));
    }
    if let Some(cursor) = cursor {
        crate::replay::parse(cursor).map_err(ApiError::EventCursor)?;
    }
    let channel: RunChannel = state
        .events
        .get(&run_id)
        .map(|c| c.clone())
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "run '{run_id}' is not live on this server\nGET /runs/{run_id} shows a finished run."
            ))
        })?;
    let Some(stream) = channel.resume(cursor).map_err(ApiError::EventCursor)? else {
        return Ok((
            StatusCode::NO_CONTENT,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
        )
            .into_response());
    };
    let stream = stream.map(|frame| {
        Ok::<_, std::convert::Infallible>(Event::default().id(frame.id()).data(frame.data.as_ref()))
    });
    Ok((
        [(
            axum::http::HeaderName::from_static("x-accel-buffering"),
            "no",
        )],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    )
        .into_response())
}

/// `GET /runs/{run_id}/output` — the bytes of a successful single-target run's final
/// artifact, with `Content-Type` by format.
pub async fn output(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Response, ApiError> {
    let view = view(&state, &run_id).await?;
    if matches!(view.status.as_str(), "pending" | "running") {
        return Err(ApiError::Conflict(format!(
            "run '{run_id}' has not finished\nWait for its `run_finished` event, then ask again."
        )));
    }
    let no_output = || {
        ApiError::NotFound(format!(
            "run '{run_id}' has no final output: only a successful run of one target has one"
        ))
    };
    let live = state
        .runs
        .get(&run_id)
        .and_then(|r| r.result.as_ref().and_then(|r| r.final_output.clone()));
    let (path, format) = match live {
        Some(oref) => (oref.path, oref.format),
        None => {
            if view.outcome.as_ref().map(Outcome::status) != Some(OutcomeStatus::Success) {
                return Err(no_output());
            }
            let target = view.target.clone().filter(|t| !t.contains(','));
            let target = target.ok_or_else(no_output)?;
            let body: Value = serde_json::from_str(view.outcome.as_ref().unwrap().text())
                .map_err(|_| no_output())?;
            let step = body["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| {
                    s["id"]
                        .as_str()
                        .is_some_and(|id| id == target || id.ends_with(&format!(":{target}")))
                })
                .ok_or_else(no_output)?;
            let node = step["id"].as_str().unwrap_or_default().to_string();
            match step["artifact"].as_str() {
                Some(path) => (
                    path.to_string(),
                    barca_core::artifact_format::format_of(path).to_string(),
                ),
                None => {
                    let snapshot = snapshot_db(&state).await?;
                    db::run_artifact(&snapshot.path, &run_id, &node)
                        .await?
                        .ok_or_else(no_output)?
                }
            }
        }
    };
    let read = path.clone();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(read))
        .await
        .map_err(|e| ApiError::Barca(BarcaError::Other(e.to_string())))?
        .map_err(|e| {
            ApiError::NotFound(format!(
                "the final artifact of run '{run_id}' cannot be read at {path}: {e}"
            ))
        })?;
    let content_type = barca_core::artifact_format::content_type(&format);
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// `GET /runs` — durable run history joined with this server's live runs, newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<RunList>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let db_state = state.db_state();
    let snapshot = snapshot_db(&state).await?;
    if query.status == Some(RunsFilter::Running) {
        return in_flight_list(&state, &db_state, &snapshot, limit)
            .await
            .map(Json);
    }
    // Clone after the snapshot: a new live run not yet in the snapshot stays visible.
    let live = live_runs(&state);
    let by_id: HashMap<_, _> = live.iter().map(|r| (r.handle.as_str(), r)).collect();
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
        if runs.iter().any(|r| r.id == run.handle) {
            continue;
        }
        // An old live run outside the requested history page must not be counted twice.
        if db::get_run(&snapshot.path, &run.handle).await?.is_some() {
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

/// `GET /runs?status=running`: the runs in flight, newest first, each with the nodes it is
/// working on now ([`crate::running`]).
async fn in_flight_list(
    state: &AppState,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<crate::handlers::SnapshotDb>,
    limit: usize,
) -> Result<RunList, ApiError> {
    let read_path = snapshot.path.as_str();
    let flights = crate::running::in_flight(state, Some(read_path)).await;
    let mut runs = Vec::with_capacity(flights.len());
    if !flights.is_empty() {
        let dag = state.inspect_dag().await?;
        let status = crate::handlers::whole_status(state, &dag, db_state, snapshot).await?;
        for flight in &flights {
            let mut row = match state.runs.get(&flight.run_id) {
                Some(live) => live_summary(&live, &state.config.files),
                None => match db::get_run(read_path, &flight.run_id).await? {
                    Some(record) => {
                        let mut row = summary(record, None);
                        // The snapshot's owner diagnosis is not this machine's: the owner
                        // was found alive.
                        row.status = "running".into();
                        row
                    }
                    None => continue,
                },
            };
            let by_node =
                crate::running::by_node(std::slice::from_ref(flight), &status.nodes, &dag);
            row.active_steps = Some(
                status
                    .nodes
                    .iter()
                    .filter_map(|n| {
                        by_node.get(&n.id).map(|r| ActiveStep {
                            node_id: n.id.clone(),
                            step: r.step,
                            partitions: r.partitions.clone(),
                        })
                    })
                    .collect(),
            );
            runs.push(row);
        }
    }
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let total = runs.len();
    runs.truncate(limit);
    Ok(RunList {
        truncated: total > runs.len(),
        runs,
        total,
    })
}
