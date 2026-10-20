//! The run contract (RFC-0007 §3.4 "Runs"): `POST /runs` starts a run on this server,
//! `GET /runs/{run_id}` shows it (from the metadata DB once it has finished, so the answer
//! survives a restart), `GET /runs/{run_id}/events` streams it, `DELETE /runs/{run_id}`
//! cancels it and `GET /runs/{run_id}/output` serves its final artifact. The run id is the
//! database run id, allocated when the run is accepted.
//!
//! `GET /runs` lists durable history joined with this server's live runs. The older trigger
//! routes (`POST /run`, `POST /run/{target}`, `POST /get/{target}`, `GET /status/{run_id}`,
//! `GET /events/{run_id}`, `DELETE /run/{run_id}`) are aliases on the same functions for one
//! minor release.

use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::state::{AppState, RunChannel, RunState, RunStatus, now_ts};
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::{Json, http::StatusCode};
use barca_core::cache::CachePolicy;
use barca_core::commands::{RunResult, ServedRun, serve_run};
use barca_core::envelope::{Context, ErrorEnvelope, ErrorKind};
use barca_core::outcome::{
    Outcome, OutcomeStatus, RunCommand, RunOptions, RunRequest, RunView, Trigger,
};
use barca_core::{BarcaError, RunEvent, db, results::GetResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

/// Default timeout for a single run (10 minutes).
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

// ─── starting runs ──────────────────────────────────────────────────────────

/// A run to start: what to run, how, and what asked for it.
pub(crate) struct RunSpec {
    pub command: RunCommand,
    /// As given (`[]`: every asset and sensor).
    pub targets: Vec<String>,
    /// The full node ids the targets resolved to.
    pub nodes: Vec<String>,
    /// As given in the request (`[]`: the deployed set).
    pub files: Vec<String>,
    pub policy: CachePolicy,
    pub trigger: Trigger,
}

impl RunSpec {
    /// A run started over HTTP.
    pub fn api(command: RunCommand, targets: Vec<String>, nodes: Vec<String>) -> Self {
        Self {
            command,
            targets,
            nodes,
            files: Vec::new(),
            policy: CachePolicy::CacheAware,
            trigger: Trigger::api(),
        }
    }
}

/// Refuse a request that would run or cancel work on a `--read-only` server.
pub(crate) fn refuse_if_read_only(state: &AppState) -> Result<(), ApiError> {
    if state.config.read_only {
        return Err(ApiError::Forbidden(
            "this server is read-only (`barca serve --read-only`): it does not run or cancel work"
                .to_string(),
        ));
    }
    Ok(())
}

/// Check a trigger's target before a run is started for it, with the function `barca get`
/// and `barca run` use (`barca_core::targets::resolve_target_among`), so the server refuses what the
/// command line refuses, in the same words: an unknown name is `404`, a name that matches
/// several nodes `409`, the wrong verb for the node's kind `400`. So is source that does not
/// parse or a DAG that cannot be built. Returns the full node id the name resolved to.
pub(crate) async fn check_target(
    state: &AppState,
    name: &str,
    verb: &str,
) -> Result<String, ApiError> {
    check_target_as(state, name, verb, Wording::Cli).await
}

/// How a wrong-kind refusal names the fix: the CLI's words (`use barca run instead`, which a
/// `--server` client prints as a local run would), or the older routes' (`use POST /run/x`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wording {
    Cli,
    Route,
}

async fn check_target_as(
    state: &AppState,
    name: &str,
    verb: &str,
    wording: Wording,
) -> Result<String, ApiError> {
    let dag = state.loaded_dag().await?;
    let nodes: Vec<(String, barca_core::NodeKind)> =
        barca_core::queries::list_assets_from_dag(&dag)
            .into_iter()
            .map(|n| (n.id, n.kind))
            .collect();
    let nodes = nodes.iter().map(|(id, kind)| (id.as_str(), *kind));
    match barca_core::targets::resolve_target_among(nodes, name, verb) {
        Ok(node) => Ok(node),
        // The CLI's error and remediation (`Run `barca list` ...`).
        Err(barca_core::targets::TargetError::NotFound { name, available }) => Err(ApiError::run(
            BarcaError::AssetNotFound(name, available.join(", ")),
            Context {
                command: verb_command(verb),
                files: Vec::new(),
            },
        )),
        Err(e @ barca_core::targets::TargetError::Ambiguous { .. }) => {
            Err(ApiError::Conflict(e.to_string()))
        }
        Err(e @ barca_core::targets::TargetError::WrongKind { .. }) if wording == Wording::Cli => {
            Err(ApiError::BadRequest(e.to_string()))
        }
        Err(barca_core::targets::TargetError::WrongKind { name, kind }) => {
            let (what, endpoint) = match kind {
                barca_core::NodeKind::Task => ("a task", "run"),
                _ => ("an asset", "get"),
            };
            Err(ApiError::BadRequest(format!(
                "'{name}' is {what}: use POST /{endpoint}/{name}"
            )))
        }
    }
}

fn verb_command(verb: &str) -> &'static str {
    match verb {
        "run" => "run",
        _ => "get",
    }
}

/// Refuse a run of every asset and sensor when no asset or sensor loaded.
pub(crate) async fn check_any_asset(state: &AppState) -> Result<(), ApiError> {
    let dag = state.loaded_dag().await?;
    if !barca_core::queries::list_assets_from_dag(&dag)
        .iter()
        .any(|n| n.kind != barca_core::NodeKind::Task)
    {
        return Err(ApiError::BadRequest(
            "no loaded assets or sensors to run; inspect /health load_errors".into(),
        ));
    }
    Ok(())
}

/// The request's files, each one of the deployed files; a usage error listing the deployed
/// files otherwise.
fn check_files(state: &AppState, files: &[String]) -> Result<(), ApiError> {
    let canonical = |f: &str| std::fs::canonicalize(f).ok();
    let deployed: Vec<_> = state
        .config
        .files
        .iter()
        .filter_map(|f| canonical(f))
        .collect();
    for file in files {
        if !canonical(file).is_some_and(|f| deployed.contains(&f)) {
            return Err(ApiError::BadRequest(format!(
                "'{file}' is not one of the deployed files\nThe server runs its deployed files: {}. Leave `files` empty to use all of them.",
                state.config.files.join(", ")
            )));
        }
    }
    Ok(())
}

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
    check_files(&state, &request.files)?;
    if request.command == RunCommand::Run && request.targets.is_empty() {
        return Err(ApiError::BadRequest(
            "a target task is required\n`run` needs the task to run in `targets`.".into(),
        ));
    }
    if request.dry_run {
        return dry_run(&state, &request).await;
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
    let run_id = start(
        state,
        RunSpec {
            command: request.command,
            policy: request.policy(),
            targets: request.targets,
            nodes,
            files: request.files,
            trigger: Trigger::api(),
        },
    );
    Ok((StatusCode::ACCEPTED, Json(json!({ "run_id": run_id }))).into_response())
}

/// `dry_run`: the `--dry-run --json` body, decided against a private copy of the metadata DB.
async fn dry_run(state: &AppState, request: &RunRequest) -> Result<Response, ApiError> {
    let snapshot = snapshot_db(state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let result = barca_core::queries::explain(
        &cfg,
        &request.targets,
        &state.config.files,
        &state.config.python,
        request.policy(),
        request.command.as_str(),
    )
    .await
    .map_err(|e| ApiError::run(e, context(request.command, &request.files)))?;
    let rendered = barca_core::report::render_explain(
        &result,
        request.command.as_str(),
        barca_core::report::ResultFormat::Json,
        None,
    );
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

/// The stdout text of a rendering, without its final newline.
pub(crate) fn stdout_of(rendered: barca_core::report::RenderedOutput) -> String {
    let mut text: String = rendered
        .chunks
        .into_iter()
        .filter_map(|c| match c {
            barca_core::report::OutputChunk::Stdout(s) => Some(s),
            barca_core::report::OutputChunk::Stderr(_) => None,
        })
        .collect();
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

/// A JSON body sent as it is (its keys in the order the CLI prints them).
pub(crate) fn json_text(status: StatusCode, text: String) -> Response {
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        text,
    )
        .into_response()
}

/// The envelope context of a local command with these files.
fn context(command: RunCommand, files: &[String]) -> Context {
    Context {
        command: command.as_str(),
        files: files.to_vec(),
    }
}

/// Accept a run: allocate its id, record it as pending, and execute it in the background.
pub(crate) fn start(state: AppState, spec: RunSpec) -> String {
    let run_id = db::generate_run_id();
    // Child of the server-wide shutdown token: DELETE /runs/{id} cancels just this run;
    // shutdown cancels all of them.
    let cancel = state.shutdown.child_token();
    state.runs.insert(
        run_id.clone(),
        RunState {
            handle: run_id.clone(),
            db_run_id: None,
            command: spec.command.as_str().into(),
            target: (!spec.targets.is_empty()).then(|| spec.targets.join(",")),
            nodes: spec.nodes.clone(),
            trigger: spec.trigger.clone(),
            files: spec.files.clone(),
            status: RunStatus::Pending,
            result: None,
            error: None,
            started_at: now_ts(),
            finished_at: None,
            outcome: None,
            cancel: cancel.clone(),
        },
    );
    let channel = RunChannel::new();
    state.events.insert(run_id.clone(), channel.clone());
    tokio::spawn(execute(state, run_id.clone(), spec, cancel, channel));
    run_id
}

async fn execute(
    state: AppState,
    run_id: String,
    spec: RunSpec,
    cancel: barca_core::CancellationToken,
    channel: RunChannel,
) {
    // Bound concurrency: acquire a run slot. Runs execute in parallel; the shared
    // metadata.db is kept safe by barca-core's process-wide DB lock.
    let _permit = state.run_slots.acquire().await.ok();
    let ctx = context(spec.command, &spec.files);

    if cancel.is_cancelled() {
        // Cancelled while queued: never started, nothing to clean up.
        let outcome = Outcome::of_error(BarcaError::Cancelled, &ctx);
        finish(&state, &run_id, &spec, &channel, None, outcome, false).await;
        return;
    }
    if let Some(mut r) = state.runs.get_mut(&run_id) {
        r.status = RunStatus::Running;
    }
    channel.emit(RunEvent::RunStarted {
        run_id: run_id.clone(),
    });

    // Core streams events over an unbounded channel (sync send from inside the worker-pool
    // loop). A drain task forwards them onto the run's channel. The engine's own
    // `run_started` says its database row exists; it is not forwarded (the run already
    // announced itself).
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunEvent>();
    let drain = {
        let (channel, state, run_id) = (channel.clone(), state.clone(), run_id.clone());
        tokio::spawn(async move {
            let mut recorded = false;
            while let Some(event) = event_rx.recv().await {
                if let RunEvent::RunStarted { run_id: id } = event {
                    recorded = true;
                    if let Some(mut run) = state.runs.get_mut(&run_id) {
                        run.db_run_id = Some(id);
                    }
                } else {
                    channel.emit(event);
                }
            }
            recorded
        })
    };

    let mut timed_out = false;
    let result: Result<RunResult, BarcaError> = {
        let fut = async {
            let dag = state.current_dag().await?;
            serve_run(ServedRun {
                dag: (*dag).clone(),
                cfg: &state.config.resolved,
                command: spec.command,
                targets: &spec.targets,
                files: &state.config.files,
                python: &state.config.python,
                policy: spec.policy.clone(),
                cancel: cancel.clone(),
                event_tx: Some(event_tx),
                options: RunOptions {
                    run_id: Some(run_id.clone()),
                    trigger: spec.trigger.clone(),
                },
            })
            .await
        };
        tokio::pin!(fut);
        // On timeout, cancel the token and keep awaiting: the run observes the cancellation,
        // terminates its workers, persists partial results, and returns — nothing is left
        // running in the background.
        tokio::select! {
            res = &mut fut => res,
            _ = tokio::time::sleep(RUN_TIMEOUT) => {
                // An operator cancel that is still unwinding when the deadline hits stays
                // classified as cancelled, not as a timeout.
                timed_out = !cancel.is_cancelled();
                cancel.cancel();
                fut.await
            }
        }
        // `fut` (and with it the event sender) is dropped here.
    };
    // The run has returned and its event sender is gone: wait for the drain so trailing
    // lines and logs land before `run_finished`.
    let recorded = drain.await.unwrap_or(false);

    let single = match &result {
        Ok(RunResult::Single(r)) => Some(r.clone()),
        _ => None,
    };
    let outcome = match result {
        Ok(RunResult::Single(r)) => Outcome::success(&r),
        Ok(RunResult::Multi(m)) => Outcome::multi(&m, &ctx),
        Err(BarcaError::Cancelled) if timed_out => Outcome::ended(
            OutcomeStatus::Error,
            &ErrorEnvelope::from_prose(
                ErrorKind::Infra,
                format!(
                    "run timed out after {}s\nThe server stops a run after {}s; steps that finished are cached.",
                    RUN_TIMEOUT.as_secs(),
                    RUN_TIMEOUT.as_secs()
                ),
            ),
        ),
        Err(e) => Outcome::of_error(e, &ctx),
    };
    finish(&state, &run_id, &spec, &channel, single, outcome, recorded).await;
    // The run slot is released here, freeing capacity for a queued run.
}

/// Record how a run ended: its outcome in the metadata DB (a row of its own when the run
/// never wrote one), the live state, and the terminal `run_finished` event.
async fn finish(
    state: &AppState,
    run_id: &str,
    spec: &RunSpec,
    channel: &RunChannel,
    result: Option<GetResult>,
    outcome: Outcome,
    recorded: bool,
) {
    let status = outcome.status();
    if let Err(e) = persist_outcome(state, run_id, spec, &outcome, recorded).await {
        barca_core::errln!("[barca] could not record the outcome of run {run_id}: {e}");
    }
    let error = outcome.envelope().map(|e| e.error);
    if let Some(mut r) = state.runs.get_mut(run_id) {
        r.finished_at = Some(now_ts());
        r.status = match status {
            OutcomeStatus::Success => RunStatus::Complete,
            OutcomeStatus::Cancelled => RunStatus::Cancelled,
            OutcomeStatus::Failed | OutcomeStatus::Error => RunStatus::Failed,
        };
        r.result = result;
        r.error = error;
        r.outcome = Some(outcome.clone());
    }
    channel.emit(RunEvent::RunFinished {
        run_id: run_id.to_string(),
        ok: status == OutcomeStatus::Success,
        outcome: Some(outcome.into_raw()),
    });
}

async fn persist_outcome(
    state: &AppState,
    run_id: &str,
    spec: &RunSpec,
    outcome: &Outcome,
    recorded: bool,
) -> Result<(), BarcaError> {
    let cfg = &state.config.resolved;
    let status = outcome.status().as_str();
    if recorded && db::set_run_outcome(&cfg.db_path, run_id, status, outcome.text()).await? {
        return Ok(());
    }
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    let target = (!spec.targets.is_empty()).then(|| spec.targets.join(","));
    db::record_unstarted_run(
        &cfg.db_path,
        db::NewRun {
            run_id,
            command: spec.command.as_str(),
            files: &db::encode_files(&state.config.files),
            target: target.as_deref(),
            steps_total: None,
            trigger: &spec.trigger,
        },
        status,
        outcome.text(),
    )
    .await
}

// ─── older trigger routes ───────────────────────────────────────────────────

/// `POST /run` (alias) — get every asset and sensor; `200 {"run_id"}`.
pub async fn run_all(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    check_any_asset(&state).await?;
    let run_id = start(state, RunSpec::api(RunCommand::Get, vec![], vec![]));
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
    Ok(Json(json!({ "run_id": start(state, spec) })))
}

/// `POST /get/{target}` (alias) — a cache-aware get of one target; `200 {"run_id"}`.
pub async fn get_target(
    State(state): State<AppState>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let node = check_target_as(&state, &target, "get", Wording::Route).await?;
    let spec = RunSpec::api(RunCommand::Get, vec![target], vec![node]);
    Ok(Json(json!({ "run_id": start(state, spec) })))
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
/// running run. Its workers are terminated and it ends `cancelled`; its event stream ends with
/// that outcome. A finished run is `409`.
pub async fn cancel(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let finished = match state.runs.get(&run_id) {
        Some(run) => match run.status {
            RunStatus::Pending | RunStatus::Running => {
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
) -> Result<Json<RunView>, ApiError> {
    Ok(Json(view(&state, &id).await?))
}

pub(crate) async fn view(state: &AppState, id: &str) -> Result<RunView, ApiError> {
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
            outcome: None,
            error: None,
            steps: Vec::new(),
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
            outcome: None,
            error: None,
            steps: Vec::new(),
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
    view.steps = db::get_run_steps(&snapshot.path, id).await?;
    view.logs = db::get_logs(&snapshot.path, id).await?;
    if let Some(run) = &live
        && let Some(channel) = state.events.get(&run.handle)
    {
        // The live backlog has every line; recorded logs only become canonical at wrap-up.
        let live_logs = view.logs.is_empty();
        for event in channel.snapshot() {
            match event {
                RunEvent::Log { node_id, line } if live_logs => view.logs.push(db::LogEntry {
                    node_id,
                    seq: view.logs.len() as i64,
                    line,
                }),
                RunEvent::StepFinished {
                    node_id,
                    ok,
                    cached,
                    elapsed_seconds,
                    error,
                    ..
                } if !view.steps.iter().any(|s| s.node_id == node_id) => {
                    view.steps.push(db::RunStep {
                        node_id,
                        status: match (cached, ok) {
                            (true, _) => "cached",
                            (_, true) => "success",
                            _ => "failed",
                        }
                        .into(),
                        elapsed_seconds,
                        error,
                    })
                }
                _ => {}
            }
        }
    }
    if view.error.is_none() {
        view.error = view.steps.iter().find_map(|step| step.error.clone());
    }
    Ok(view)
}

fn live_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Complete => "success",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

/// `GET /runs/{run_id}/events` (and the alias `GET /events/{run_id}`) — the run's events as
/// server-sent events, from its first: `run_started`, `line`, `log`, `step_finished`, then
/// `run_finished` with the outcome, after which the stream ends. Each has an `id:`, its
/// number in the run. A subscriber that reads slowly receives every event, once.
///
/// Sends `X-Accel-Buffering: no` so nginx (and proxies that honor it) pass events through as
/// they happen; idle streams carry a keep-alive comment every 15s, inside nginx's default 60s
/// read timeout.
pub async fn events(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<
    (
        [(axum::http::HeaderName, &'static str); 1],
        Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>>,
    ),
    ApiError,
> {
    let channel: RunChannel = state
        .events
        .get(&run_id)
        .map(|c| c.clone())
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "run '{run_id}' is not live on this server\nGET /runs/{run_id} shows a finished run."
            ))
        })?;
    let stream = channel.subscribe(0).map(|(n, event)| {
        Ok(Event::default()
            .id(n.to_string())
            .json_data(&event)
            .unwrap_or_else(|_| Event::default()))
    });
    Ok((
        [(
            axum::http::HeaderName::from_static("x-accel-buffering"),
            "no",
        )],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    ))
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
                Some(path) => (path.to_string(), format_of(path)),
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
    let content_type = match format.as_str() {
        "json" => "application/json",
        "parquet" => "application/vnd.apache.parquet",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    };
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// An artifact's format from its file name.
fn format_of(path: &str) -> String {
    match std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("json") => "json",
        Some("parquet") => "parquet",
        Some("csv") => "csv",
        _ => "pickle",
    }
    .to_string()
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
        error: live.and_then(|r| r.error.clone()),
    }
}
fn live_summary(run: &RunState, files: &[String]) -> RunSummary {
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
                .filter(|s| s.status.as_deref() == Some("cached"))
                .count() as i64
        }),
        error: run.error.clone(),
    }
}
fn live_runs(state: &AppState) -> Vec<RunState> {
    state.runs.iter().map(|r| r.value().clone()).collect()
}

/// `GET /runs` — durable run history joined with this server's live runs, newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<RunList>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let snapshot = snapshot_db(&state).await?;
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

/// Evict finished runs older than `max_age` from the in-memory runs map, checking every
/// `interval`. Their history stays in the DB.
pub async fn evict_finished_runs(state: AppState, interval: Duration, max_age: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        let cutoff = now_ts() - max_age.as_secs_f64();
        state.runs.retain(|handle, run| {
            let keep = match run.status {
                RunStatus::Complete | RunStatus::Failed | RunStatus::Cancelled => {
                    run.finished_at.is_none_or(|t| t > cutoff)
                }
                _ => true,
            };
            if !keep {
                // Drop the live event channel too; subscribers' streams end.
                state.events.remove(handle);
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServeConfig, state::now_ts};
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
    fn live(handle: &str) -> RunState {
        RunState {
            handle: handle.into(),
            db_run_id: None,
            command: "get".into(),
            target: Some("first".into()),
            nodes: vec![],
            trigger: Trigger::api(),
            files: vec![],
            status: RunStatus::Pending,
            result: None,
            error: None,
            started_at: now_ts(),
            finished_at: None,
            outcome: None,
            cancel: barca_core::CancellationToken::new(),
        }
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
    async fn queued_and_failed_durable_runs_are_visible_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let mut run = live("durable");
        state.runs.insert(run.handle.clone(), run.clone());
        let Json(queued) = list(State(state.clone()), Query(RunsQuery::default()))
            .await
            .unwrap();
        assert_eq!(queued.runs.len(), 1);
        assert_eq!(queued.runs[0].id, "durable");
        assert_eq!(queued.runs[0].status, "pending");
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
        let wrapping_up = view(&state, "durable").await.unwrap();
        assert_eq!(
            wrapping_up.status, "running",
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
        assert_eq!(history.runs[0].error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn a_slow_subscriber_and_a_reconnect_receive_every_event_once() {
        let channel = RunChannel::new();
        // Far more than any buffer a subscriber could have had: none is dropped.
        let total = 5_000;
        let mut slow = Box::pin(channel.subscribe(0));
        for i in 0..total {
            channel.emit(RunEvent::Log {
                node_id: "p.py:a".into(),
                line: i.to_string(),
            });
        }
        // The first reader takes a few events and goes away, as a dropped connection does.
        let mut first = Box::pin(channel.subscribe(0));
        for want in 0..3 {
            assert_eq!(first.next().await.unwrap().0, want);
        }
        drop(first);
        channel.emit(RunEvent::RunFinished {
            run_id: "r".into(),
            ok: true,
            outcome: None,
        });
        let reconnect: Vec<_> = channel.subscribe(0).collect().await;
        let lagged: Vec<_> = slow.as_mut().collect().await;
        for got in [reconnect, lagged] {
            let ids: Vec<usize> = got.iter().map(|(n, _)| *n).collect();
            assert_eq!(ids, (0..=total).collect::<Vec<_>>());
            assert!(got.last().unwrap().1.is_terminal(), "ends at run_finished");
        }
        // A channel dropped before its run finished ends its streams too.
        let orphan = RunChannel::new();
        orphan.emit(RunEvent::RunStarted { run_id: "r".into() });
        let stream = orphan.subscribe(0);
        drop(orphan);
        assert_eq!(stream.collect::<Vec<_>>().await.len(), 1);
    }
}
