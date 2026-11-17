//! The HTTP handlers of the run contract: starting, cancelling, inspecting and listing runs,
//! and the event stream. What they answer with is computed by [`super::projection`]; what
//! happens to a run once accepted is [`super::lifecycle`]'s.

use super::lifecycle::{cancelled, finish, start};
use super::projection::{
    ActiveStep, RunList, RunSummary, RunsQuery, live_runs, live_status, live_summary, summary,
    view, view_page,
};
use super::{
    RunSpec, Wording, check_any_asset, check_files, check_target, check_target_as, context,
    json_text, refuse_if_read_only, run_files, stdout_of,
};
use crate::auth_http::Principal;
use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::state::{AppState, RunChannel, RunState, RunStatus};
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::{Json, http::StatusCode};
use barca_core::cache::CachePolicy;
use barca_core::envelope::Context;
use barca_core::outcome::{Outcome, OutcomeStatus, RunCommand, RunRequest, RunView, Trigger};
use barca_core::run_explain::{Cancellation, CancelledBy};
use barca_core::{BarcaError, db};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// `POST /runs` — start a run on this server: `202 {"run_id"}`, or `200` with the `--dry-run`
/// JSON when `dry_run` is set (nothing starts). The targets are checked first: no run is
/// started for a request a local `barca get` / `barca run` would refuse.
pub async fn create(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    body: axum::body::Bytes,
) -> Response {
    match create_run(state, principal.map(|p| p.0.0), &body).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

/// What started a run over HTTP: the API, by whom (the request's token), and why (#925).
fn api_trigger(principal: Option<String>, reason: Option<String>) -> Trigger {
    Trigger {
        principal,
        reason,
        ..Trigger::api()
    }
}

/// `RunRequest.reason`, checked: trimmed, one line of at most
/// [`barca_core::outcome::MAX_REASON_CHARS`] characters; empty is none.
fn reason_of(request: &RunRequest) -> Result<Option<String>, ApiError> {
    let Some(reason) = request.reason.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if reason.chars().count() > barca_core::outcome::MAX_REASON_CHARS {
        return Err(ApiError::BadRequest(format!(
            "`reason` is longer than {} characters\nSay why in one short line.",
            barca_core::outcome::MAX_REASON_CHARS
        )));
    }
    if reason.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "`reason` holds a control character (a line break, a tab)\nSay why in one line.".into(),
        ));
    }
    Ok((!reason.is_empty()).then(|| reason.to_string()))
}

async fn create_run(
    state: AppState,
    principal: Option<String>,
    body: &[u8],
) -> Result<Response, ApiError> {
    let request: RunRequest = serde_json::from_slice(body).map_err(|e| {
        ApiError::BadRequest(format!(
            "the request body is not a run request: {e}\nSend JSON such as {{\"command\": \"get\", \"targets\": [\"daily_report\"]}}."
        ))
    })?;
    let reason = reason_of(&request)?;
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
    // A request naming a paused node says it means it (`--allow-paused`).
    if !request.allow_paused {
        let paused = barca_core::pauses::list(&state.config.resolved.db_path).await?;
        if let Some(id) = nodes.iter().find(|id| paused.iter().any(|p| &p.id == *id)) {
            return Err(ApiError::run(
                barca_core::pauses::refused(id),
                Context {
                    command: crate::runs::verb_command(verb),
                    files: Vec::new(),
                },
            ));
        }
    }
    let admitted = crate::queue::admit(
        &state,
        RunSpec {
            command: request.command,
            policy: request.policy(),
            targets: request.targets,
            nodes,
            files,
            trigger: api_trigger(principal, reason),
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
pub async fn run_all(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    check_any_asset(&state).await?;
    let mut spec = RunSpec::api(RunCommand::Get, vec![], vec![]);
    spec.trigger = api_trigger(principal.map(|p| p.0.0), None);
    let run_id = start(state, spec).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// `POST /run/{target}` (alias) — run a task, recomputing its upstream assets as
/// `--refresh-all` does; `200 {"run_id"}`.
pub async fn run_target(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let node = check_target_as(&state, &target, "run", Wording::Route).await?;
    let mut spec = RunSpec::api(RunCommand::Run, vec![target], vec![node]);
    spec.policy = CachePolicy::RefreshAll;
    spec.trigger = api_trigger(principal.map(|p| p.0.0), None);
    Ok(Json(json!({ "run_id": start(state, spec).await? })))
}

/// `POST /get/{target}` (alias) — a cache-aware get of one target; `200 {"run_id"}`.
pub async fn get_target(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let node = check_target_as(&state, &target, "get", Wording::Route).await?;
    let mut spec = RunSpec::api(RunCommand::Get, vec![target], vec![node]);
    spec.trigger = api_trigger(principal.map(|p| p.0.0), None);
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
    principal: Option<Extension<Principal>>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let principal = principal.map(|p| p.0.0);
    if let Some(waiting) = state.queue.take(&run_id) {
        let cancellation = queued_cancellation(&state, &run_id, principal);
        if let Some(mut run) = state.runs.get_mut(&run_id) {
            run.cancel_requested = true;
            run.cancellation = Some(cancellation);
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
                run.cancellation = Some(Cancellation {
                    by: CancelledBy::Request,
                    principal,
                    started: run.status == RunStatus::Running,
                    waiting_behind: Vec::new(),
                    same_as: None,
                });
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

/// How many of the runs a queued run waited behind its cancellation names.
const WAITING_BEHIND: usize = 10;

/// The cancellation of `run_id`, taken off the queue before it started: the runs executing
/// then, and one of them doing the same work (the same command, targets and files), if any.
pub(super) fn queued_cancellation(
    state: &AppState,
    run_id: &str,
    principal: Option<String>,
) -> Cancellation {
    let this = state.runs.get(run_id).map(|r| {
        (
            r.command.clone(),
            r.nodes.clone(),
            r.target.clone(),
            r.files.clone(),
        )
    });
    let mut running: Vec<(f64, String, bool)> = state
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Running && r.handle != run_id)
        .map(|r| {
            let same = this
                .as_ref()
                .is_some_and(|(command, nodes, target, files)| {
                    *command == r.command
                        && *files == r.files
                        && if nodes.is_empty() && r.nodes.is_empty() {
                            *target == r.target
                        } else {
                            *nodes == r.nodes
                        }
                });
            (r.started_at, r.handle.clone(), same)
        })
        .collect();
    running.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let same_as = running.iter().find(|r| r.2).map(|r| r.1.clone());
    Cancellation {
        by: CancelledBy::Request,
        principal,
        started: false,
        waiting_behind: running
            .into_iter()
            .take(WAITING_BEHIND)
            .map(|r| r.1)
            .collect(),
        same_as,
    }
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
    let offset = if page.all { 0 } else { page.offset };
    Ok(Json(view_page(&state, &id, limit, offset).await?))
}

/// `GET /runs/{run_id}?limit=N&offset=M` lists at most N of the run's steps (default
/// [`db::RUN_STEPS_LIMIT`]) from the M-th (failed steps first, then as recorded), `?all=true`
/// every one; `steps_page` says how many there are and where the next page starts.
#[derive(Debug, Default, Deserialize)]
pub struct StepsQuery {
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
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

/// `GET /runs` — durable run history joined with this server's live runs: the runs in
/// progress first, whatever `limit` lists (#941), then the newest. The filters are the
/// database's ([`RunsQuery`]): `total` counts the runs they keep.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<RunList>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let filter = query.filter().map_err(ApiError::BadRequest)?;
    let db_state = state.db_state();
    let snapshot = snapshot_db(&state).await?;
    // Counted by the database over the window, whatever `limit` lists (#991).
    let activity =
        barca_core::queries::activity_at(&snapshot.path, &filter, chrono::Utc::now()).await?;
    if filter.statuses == ["running"] {
        let mut runs: Vec<RunSummary> = in_flight(&state, &db_state, &snapshot)
            .await?
            .into_iter()
            .filter(|run| {
                filter.keeps_unrecorded(
                    "running",
                    run.triggered_by,
                    &run.started_at,
                    &searched(run),
                )
            })
            .collect();
        let total = runs.len();
        runs.truncate(limit);
        return Ok(Json(RunList {
            truncated: total > runs.len(),
            runs,
            total,
            activity: Some(activity),
        }));
    }
    // Clone after the snapshot: a new live run not yet in the snapshot stays visible.
    let live = live_runs(&state);
    let by_id: HashMap<_, _> = live.iter().map(|r| (r.handle.as_str(), r)).collect();
    let kept = filter.run_filter();
    let records = db::get_runs_filtered(&snapshot.path, limit, &kept).await?;
    let durable_total = db::count_runs_filtered(&snapshot.path, &kept).await?;
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
        let in_progress = matches!(run.status, RunStatus::Pending | RunStatus::Running);
        match db::get_run(&snapshot.path, &run.handle).await? {
            // An older run than the page holds: listed while it is in progress (#941), and
            // already counted.
            Some(mut record) => {
                record.status = "running".into();
                if in_progress && filter.matches(&record) {
                    runs.push(summary(record, Some(run)));
                }
            }
            None => {
                let row = live_summary(run, &state.config.files);
                if filter.keeps_unrecorded(
                    &row.status,
                    row.triggered_by,
                    &row.started_at,
                    &searched(&row),
                ) {
                    runs.push(row);
                    extra += 1;
                }
            }
        }
    }
    // A CLI run on the same `.barca/` whose owner is alive, older than the page holds.
    for alive in db::alive_running_runs_in(&snapshot.path, &state.config.resolved.db_path).await {
        if runs.iter().any(|r| r.id == alive.run_id) {
            continue;
        }
        if let Some(mut record) = db::get_run(&snapshot.path, &alive.run_id).await? {
            record.status = "running".into();
            if filter.matches(&record) {
                runs.push(summary(record, None));
            }
        }
    }
    let live_first = |run: &RunSummary| !matches!(run.status.as_str(), "pending" | "running");
    // Stable: runs started in the same second stay as the database listed them, the one
    // recorded last first (a run id says nothing of the order).
    runs.sort_by(|a, b| {
        live_first(a)
            .cmp(&live_first(b))
            .then_with(|| b.started_at.cmp(&a.started_at))
    });
    let total = durable_total + extra;
    let in_progress = runs.iter().filter(|r| !live_first(r)).count();
    runs.truncate(limit.max(in_progress));
    Ok(Json(RunList {
        truncated: total > runs.len(),
        runs,
        total,
        activity: Some(activity),
    }))
}

/// What a search looks in for a run the database is not asked about: its ids and target.
fn searched(run: &RunSummary) -> Vec<&str> {
    [
        Some(run.id.as_str()),
        run.run_id.as_deref(),
        run.target.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// What each run in flight is working on now, by run id (a server run under its handle and
/// its durable id): `now` of `GET /history?status=running`, the same object as on
/// `GET /runs?status=running`.
pub(crate) async fn in_flight_now(
    state: &AppState,
    snapshot: &std::sync::Arc<crate::handlers::SnapshotDb>,
) -> Result<barca_core::report::HistoryNow, ApiError> {
    let db_state = state.db_state();
    let mut out = barca_core::report::HistoryNow::new();
    for run in in_flight(state, &db_state, snapshot).await? {
        let Some(now) = run.now else { continue };
        if let Some(durable) = run.run_id.filter(|id| *id != run.id) {
            out.insert(durable, now.clone());
        }
        out.insert(run.id, now);
    }
    Ok(out)
}

/// The runs in flight, newest first, each with the nodes it is working on now
/// ([`crate::running`]): `GET /runs?status=running`.
async fn in_flight(
    state: &AppState,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<crate::handlers::SnapshotDb>,
) -> Result<Vec<RunSummary>, ApiError> {
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
                        row.worded()
                    }
                    None => continue,
                },
            };
            let by_node =
                crate::running::by_node(std::slice::from_ref(flight), &status.nodes, &dag);
            let active: Vec<ActiveStep> = status
                .nodes
                .iter()
                .filter_map(|n| {
                    by_node.get(&n.id).map(|r| ActiveStep {
                        node_id: n.id.clone(),
                        step: r.step,
                        partitions: r.partitions.clone(),
                    })
                })
                .collect();
            let facts: Vec<_> = active
                .iter()
                .map(|a| barca_core::run_list::ActiveNodeFacts {
                    node_id: &a.node_id,
                    running: a.step == crate::running::RunningStep::Running,
                    keys: a.partitions.as_ref().map(|p| (p.done, p.total)),
                })
                .collect();
            row.now = Some(barca_core::run_list::run_now(&facts, &dag.groups));
            row.active_steps = Some(active);
            runs.push(row);
        }
    }
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(runs)
}
