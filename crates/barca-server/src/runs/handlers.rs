//! The HTTP handlers of the run contract: starting, cancelling, inspecting and listing runs,
//! and the event stream. What they answer with is computed by [`super::projection`]; what
//! happens to a run once accepted is [`super::lifecycle`]'s.

use super::lifecycle::{cancelled, finish, start};
use super::projection::{RunList, RunsQuery, live_of, live_runs, live_status, view, view_page};
use super::{
    RunSpec, Wording, check_any_asset, check_files, check_target, check_target_as, context,
    run_files,
};
use crate::auth_http::Principal;
use crate::error::ApiError;
use crate::guards::refuse_if_read_only;
use crate::replay::RunChannel;
use crate::respond::{json_text, stdout_of};
use crate::run_model::{RunState, RunStatus};
use crate::state::AppState;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::{Json, http::StatusCode};
use barca_service::cache::CachePolicy;
use barca_service::envelope::Context;
use barca_service::outcome::{Outcome, OutcomeStatus, RunCommand, RunRequest, RunView, Trigger};
use barca_service::run_explain::{Cancellation, CancelledBy};
use barca_service::served::{
    ActiveStep, LiveRun, RunCancelling, RunHandle, RunLogs, RunSummary, RunViewStatus,
};
use barca_service::{BarcaError, db};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

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
/// [`barca_service::outcome::MAX_REASON_CHARS`] characters; empty is none.
fn reason_of(request: &RunRequest) -> Result<Option<String>, ApiError> {
    let Some(reason) = request.reason.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if reason.chars().count() > barca_service::outcome::MAX_REASON_CHARS {
        return Err(ApiError::BadRequest(format!(
            "`reason` is longer than {} characters\nSay why in one short line.",
            barca_service::outcome::MAX_REASON_CHARS
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
    refuse_if_read_only(&state.config)?;
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
        let paused = barca_service::pauses::list(&state.config.resolved.db_path).await?;
        if let Some(id) = nodes.iter().find(|id| paused.iter().any(|p| &p.id == *id)) {
            return Err(ApiError::run(
                barca_service::pauses::refused(id),
                Context {
                    command: crate::runs::verb_command(verb),
                    files: Vec::new(),
                },
            ));
        }
    }
    let rules = request.rules();
    let admitted = crate::runs::admit(
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
            rules,
            timeout: None,
            kill_requeues: 0,
            queue: request.queue,
        },
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(RunHandle {
            run_id: admitted.run_id,
            status: match admitted.status {
                RunStatus::Pending => RunViewStatus::Pending,
                _ => RunViewStatus::Running,
            },
            position: admitted.position,
            waiting_for: admitted.waiting_for,
            queue: request.queue.shown(),
        }),
    )
        .into_response())
}

/// `partition`, parsed as `--partition` is; `400` when it does not parse.
/// `RunRequest.batch`, checked (RFC-0013): a bad value is `400`.
fn batch_of(request: &RunRequest) -> Result<barca_service::batch::BatchOverride, ApiError> {
    barca_service::batch::BatchOverride::parse(&request.batch)
        .map_err(|msg| ApiError::BadRequest(format!("error: {msg}")))
}

fn partition_of(
    request: &RunRequest,
) -> Result<Option<barca_service::selection::PartitionSelection>, ApiError> {
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
    let snapshot = state.reads.snapshot_db().await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let result = barca_service::queries::explain_with_rules(
        &cfg,
        &request.targets,
        run_files(state, files),
        &state.config.python,
        request.policy(),
        request.command.as_str(),
        partition_of(request)?.as_ref(),
        &batch_of(request)?,
        &request.rules(),
    )
    .await
    .map_err(|e| ApiError::run(e, context(request.command, files)))?;
    let rendered = barca_service::report::render_explain(
        &result,
        request.command.as_str(),
        barca_service::report::ResultFormat::Json,
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
    refuse_if_read_only(&state.config)?;
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
    refuse_if_read_only(&state.config)?;
    let node = check_target_as(&state, &target, "run", Wording::Route).await?;
    let mut spec = RunSpec::api(RunCommand::Run, vec![target], vec![node]);
    spec.policy = CachePolicy::RefreshAll {
        since: barca_service::time::now_secs_f64(),
    };
    spec.trigger = api_trigger(principal.map(|p| p.0.0), None);
    Ok(Json(json!({ "run_id": start(state, spec).await? })))
}

/// `POST /get/{target}` (alias) — a cache-aware get of one target; `200 {"run_id"}`.
pub async fn get_target(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state.config)?;
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
        .registry
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
) -> Result<Json<RunCancelling>, ApiError> {
    refuse_if_read_only(&state.config)?;
    let principal = principal.map(|p| p.0.0);
    let _transition = state.runs.queue.admit.lock().await;
    if let Some(waiting) = state.runs.queue.take(&run_id) {
        let cancellation = queued_cancellation(&state, &run_id, principal);
        if let Some(mut run) = state.runs.registry.get_mut(&run_id) {
            run.cancel_requested = true;
            run.cancellation = Some(cancellation);
            run.cancel.cancel();
        }
        let ctx = context(waiting.spec.command, &waiting.spec.files);
        let outcome = cancelled(&state, &ctx);
        if let Some(channel) = state.runs.events.get(&run_id).map(|c| c.clone()) {
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
        return Ok(Json(RunCancelling::cancelled(run_id)));
    }
    let finished = match state.runs.registry.get_mut(&run_id) {
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
                return Ok(Json(RunCancelling::of(run_id)));
            }
            status => json!(status).as_str().unwrap_or("finished").to_string(),
        },
        None => {
            let snapshot = state.reads.snapshot_db().await?;
            match db::get_run(&snapshot.path, &run_id).await? {
                Some(record) => {
                    let control = barca_service::served::run_control(
                        &snapshot.path,
                        &run_id,
                        &record.status,
                        false,
                    )
                    .await?;
                    return Err(ApiError::Conflict(format!(
                        "run '{run_id}': {}",
                        control.reason.unwrap_or_default()
                    )));
                }
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
    let this = state.runs.registry.get(run_id).map(|r| {
        (
            r.command.clone(),
            r.nodes.clone(),
            r.target.clone(),
            r.files.clone(),
        )
    });
    let mut running: Vec<(f64, String, bool)> = state
        .runs
        .registry
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
) -> Result<Json<RunLogs>, ApiError> {
    let stream = match query.stream.as_deref() {
        None | Some("") => None,
        Some(text) => Some(barca_service::LogStream::parse(text).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "stream '{text}' is not a stream: use stdout or stderr"
            ))
        })?),
    };
    let snapshot = state.reads.snapshot_db().await?;
    let live = state.runs.registry.get(&id).map(|r| r.status);
    let record = db::get_run(&snapshot.path, &id).await?;
    let mut status = match (&record, live) {
        (Some(record), _) => record.status.clone(),
        (None, Some(live)) => live_status(live).as_str().to_string(),
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
        status = live_status(live).as_str().to_string();
    }
    let step = query.step.as_deref().filter(|s| !s.is_empty());
    let lines = if record.is_some() || live.is_some() {
        // Errors read as `barca logs` reports them here.
        barca_service::queries::logs_at(
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
    Ok(Json(RunLogs {
        run_id: id,
        status,
        logs: lines,
    }))
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
                .map_err(|_| ApiError::EventCursor(crate::error::CursorError::Invalid))
        })
        .transpose()?;
    if cursor_headers.next().is_some() {
        return Err(ApiError::EventCursor(crate::error::CursorError::Invalid));
    }
    if let Some(cursor) = cursor {
        crate::replay::parse(cursor).map_err(ApiError::EventCursor)?;
    }
    let channel: RunChannel = state
        .runs.events
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
        .registry
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
                        .is_some_and(|id| crate::resolve::names(id, &target))
                })
                .ok_or_else(no_output)?;
            let node = step["id"].as_str().unwrap_or_default().to_string();
            match step["artifact"].as_str() {
                Some(path) => (
                    path.to_string(),
                    barca_service::artifact_format::format_of(path).to_string(),
                ),
                None => {
                    let snapshot = state.reads.snapshot_db().await?;
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
    let content_type = barca_service::artifact_format::content_type(&format);
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// `GET /runs` — durable run history joined with this server's live runs: the runs in
/// progress first, whatever `limit` lists (#941), then the newest
/// ([`barca_service::served::run_list`]). The filters are the database's ([`RunsQuery`]): `total`
/// counts the runs they keep.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<RunList>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let filter = query.filter().map_err(ApiError::BadRequest)?;
    let db_state = state.reads.db_state();
    let snapshot = state.reads.snapshot_db().await?;
    let now = chrono::Utc::now();
    if filter.statuses == ["running"] {
        let rows = in_flight(&state, &db_state, &snapshot).await?;
        return Ok(Json(
            barca_service::served::in_flight_list(&snapshot.path, rows, &filter, limit, now)
                .await?,
        ));
    }
    // Clone after the snapshot: a new live run not yet in the snapshot stays visible.
    let live = live_runs(&state);
    Ok(Json(
        barca_service::served::run_list(barca_service::served::RunListRequest {
            read_path: &snapshot.path,
            db_path: &state.config.resolved.db_path,
            limit,
            filter: &filter,
            live: &live,
            default_files: &state.config.files,
            now,
        })
        .await?,
    ))
}

/// What each run in flight is working on now, by run id (a server run under its handle and
/// its durable id): `now` of `GET /history?status=running`, the same object as on
/// `GET /runs?status=running`.
pub(crate) async fn in_flight_now(
    state: &AppState,
    snapshot: &std::sync::Arc<crate::snapshot::SnapshotDb>,
) -> Result<barca_service::report::HistoryNow, ApiError> {
    let db_state = state.reads.db_state();
    Ok(barca_service::served::in_flight_now(
        in_flight(state, &db_state, snapshot).await?,
    ))
}

/// The runs in flight, newest first, each with the nodes it is working on now
/// ([`crate::running`]): the rows of `GET /runs?status=running`
/// ([`barca_service::served::in_flight`]). This fills which nodes each run works on.
async fn in_flight(
    state: &AppState,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<crate::snapshot::SnapshotDb>,
) -> Result<Vec<RunSummary>, ApiError> {
    let read_path = snapshot.path.as_str();
    let flights = crate::running::in_flight(state, Some(read_path)).await;
    if flights.is_empty() {
        return Ok(Vec::new());
    }
    let dag = state.reads.inspect_dag().await?;
    let status = state.reads.whole_status(&dag, db_state, snapshot).await?;
    let live: Vec<Option<LiveRun>> = flights
        .iter()
        .map(|flight| {
            state
                .runs
                .registry
                .get(&flight.run_id)
                .map(|run| live_of(state, &run))
        })
        .collect();
    let rows = flights
        .iter()
        .zip(&live)
        .map(|(flight, live)| {
            let by_node =
                crate::running::by_node(std::slice::from_ref(flight), &status.nodes, &dag);
            barca_service::served::Flight {
                run_id: &flight.run_id,
                live: live.as_ref(),
                active: status
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
            }
        })
        .collect();
    Ok(barca_service::served::in_flight(read_path, rows, &state.config.files, &dag.groups).await?)
}
