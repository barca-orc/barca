//! Endpoint handlers. Core commands are `async fn`s that run directly on the
//! server's runtime — handlers simply `.await` them. Each run carries a
//! `CancellationToken` (a child of the server-wide shutdown token) so
//! `DELETE /run/{id}`, the run timeout, and Ctrl-C can all stop it mid-flight:
//! workers are terminated and the run is marked cancelled/failed.

use crate::error::ApiError;
use crate::state::{AppState, Durations, NodeState, RunChannel, RunState, RunStatus, now_ts};
use axum::Json;
use axum::extract::{Path, State};
use axum::response::Sse;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response};
use barca_core::cache::CachePolicy;
use barca_core::commands;
use barca_core::queries;
use barca_core::results::{AssetSummary, GetResult, PlanResult};
use barca_core::{BarcaError, RunEvent, db};
use futures::stream::{self, Stream, StreamExt};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

/// Default timeout for a single run (10 minutes).
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

/// `GET /health` — liveness, version, whether this server is read-only, and
/// whether it runs the scheduler.
/// No core work.
pub async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "read_only": state.config.read_only,
        // Whether this server fires `Schedule(...)` nodes: the same rule `serve`
        // uses to start the scheduler (on unless --no-schedule or --read-only).
        "scheduler": state.config.schedule && !state.config.read_only,
    }))
}

/// Refuse a request that would run or cancel work on a `--read-only` server.
fn refuse_if_read_only(state: &AppState) -> Result<(), ApiError> {
    if state.config.read_only {
        return Err(ApiError::Forbidden(
            "this server is read-only (`barca serve --read-only`): it does not run or cancel work"
                .to_string(),
        ));
    }
    Ok(())
}

/// A metadata DB a read-only request may query: a private snapshot of the real
/// one, or — when there is no DB yet — an empty scratch DB, so the absence is
/// preserved. Either way the schema is ensured on the copy, never on the
/// original. Dropping it deletes the copy.
pub(crate) struct SnapshotDb {
    _snapshot: Option<db::DbSnapshot>,
    _scratch: Option<tempfile::TempDir>,
    pub(crate) path: String,
}

pub(crate) async fn snapshot_db(state: &AppState) -> Result<SnapshotDb, ApiError> {
    let (snapshot, scratch, path) =
        match db::DbSnapshot::take(&state.config.resolved.db_path).await? {
            Some(s) => {
                let path = s.path().to_string();
                (Some(s), None, path)
            }
            None => {
                let dir = tempfile::tempdir()
                    .map_err(|e| BarcaError::Db(format!("failed to create scratch dir: {e}")))?;
                let path = dir.path().join("metadata.db").display().to_string();
                (None, Some(dir), path)
            }
        };
    db::init_db(&path).await?;
    Ok(SnapshotDb {
        _snapshot: snapshot,
        _scratch: scratch,
        path,
    })
}

/// `GET /state` — every node's cache state (would `barca get` reuse it?), latest
/// attempt, typical durations, and next scheduled run. Read-only by
/// construction: the cache check runs against a private snapshot of the DB.
pub async fn state(State(state): State<AppState>) -> Result<Json<Vec<NodeState>>, ApiError> {
    let cfg = &state.config;
    let zone = crate::scheduler::zone_of(cfg);
    Ok(Json(
        node_states_in(&cfg.resolved, &cfg.files, &cfg.python, &zone).await?,
    ))
}

/// Every node's [`NodeState`], in topological order: its `barca status` entry
/// (the same cache decision `--dry-run` makes), typical durations, and the next
/// fire time of its cron schedule, with cron evaluated in this machine's local
/// time. The server's `GET /state` evaluates it in its `--timezone` instead.
///
/// Read-only by construction: status and history are read from a private
/// snapshot of the metadata DB, so this never opens, locks for longer than the
/// copy, creates or writes the real one. Artifact shapes are skipped (they spawn
/// a reader process per call, too slow for a polled endpoint).
pub async fn node_states(
    cfg: &barca_core::config::ResolvedConfig,
    files: &[String],
    python: &std::path::Path,
) -> Result<Vec<NodeState>, BarcaError> {
    node_states_in(cfg, files, python, &crate::scheduler::Zone::Local).await
}

/// [`node_states`] with cron evaluated in `zone`.
async fn node_states_in(
    cfg: &barca_core::config::ResolvedConfig,
    files: &[String],
    python: &std::path::Path,
    zone: &crate::scheduler::Zone,
) -> Result<Vec<NodeState>, BarcaError> {
    let snapshot = db::DbSnapshot::take(&cfg.db_path).await?;
    let scratch = tempfile::tempdir()
        .map_err(|e| BarcaError::Db(format!("failed to create scratch dir: {e}")))?;
    let mut snap_cfg = cfg.clone();
    // No DB yet: point at a path that doesn't exist — "nothing cached", and
    // nothing is created.
    snap_cfg.db_path = match &snapshot {
        Some(s) => s.path().to_string(),
        None => scratch.path().join("metadata.db").display().to_string(),
    };

    let python_buf = python.to_path_buf();
    let (status, schedule) = tokio::join!(
        barca_core::status::status(&snap_cfg, &[], files, python, 0, false),
        barca_core::schedule::describe_schedule_in(files, &python_buf, zone),
    );
    let history = match &snapshot {
        Some(s) => db::materialization_history(s.path()).await?,
        None => Vec::new(),
    };
    let durations = durations_by_node(&history);
    let next_run: std::collections::HashMap<String, i64> = schedule
        .into_iter()
        .filter_map(|s| s.next_fire.map(|t| (s.id, t)))
        .collect();

    Ok(status?
        .nodes
        .into_iter()
        .map(|n| NodeState {
            durations: durations.get(&n.id).cloned(),
            next_run: next_run.get(&n.id).copied(),
            status: n,
        })
        .collect())
}

/// Successful materializations considered for typical durations.
const DURATION_WINDOW: usize = 20;

/// Median and p95 over each node's last [`DURATION_WINDOW`] successful runs.
/// Partition rows fold into their base node.
fn durations_by_node(
    rows: &[db::MaterializationRow],
) -> std::collections::HashMap<String, Durations> {
    let mut elapsed: std::collections::HashMap<String, Vec<f64>> = Default::default();
    for r in rows {
        if r.status != "success" {
            continue;
        }
        if let Some(e) = r.elapsed_seconds {
            let base = barca_core::StepId::parse(&r.node_id).base_id().to_string();
            elapsed.entry(base).or_default().push(e);
        }
    }
    elapsed
        .into_iter()
        .filter_map(|(id, all)| {
            let mut recent = all[all.len().saturating_sub(DURATION_WINDOW)..].to_vec();
            if recent.is_empty() {
                return None;
            }
            recent.sort_by(f64::total_cmp);
            Some((
                id,
                Durations {
                    median_seconds: percentile(&recent, 0.5),
                    p95_seconds: percentile(&recent, 0.95),
                    samples: recent.len(),
                },
            ))
        })
        .collect()
}

/// Nearest-rank percentile over sorted values.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// `GET /plan` — execution plan for the server's files (cache-aware).
pub async fn plan(State(state): State<AppState>) -> Result<Json<PlanResult>, ApiError> {
    if let Some(cached) = state.cache.read().unwrap().plan.clone() {
        return Ok(Json(cached));
    }
    let result = queries::plan(&state.config.files, &state.config.python).await?;
    state.cache.write().unwrap().plan = Some(result.clone());
    Ok(Json(result))
}

/// `GET /assets` — list every node with kind/freshness/inputs (cache-aware).
pub async fn assets(State(state): State<AppState>) -> Result<Json<Vec<AssetSummary>>, ApiError> {
    if let Some(cached) = state.cache.read().unwrap().assets.clone() {
        return Ok(Json(cached));
    }
    let result = queries::list_assets(&state.config.files, &state.config.python).await?;
    state.cache.write().unwrap().assets = Some(result.clone());
    Ok(Json(result))
}

/// `GET /assets/{name}/schema` — inspect the selected node and its direct inputs.
/// Reads artifact shapes on demand rather than on every `/state` poll. Uses the
/// same safe inspector as `barca status`: parquet footers, JSON, pickle opcodes.
pub async fn asset_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<barca_core::status::NodeStatus>>, ApiError> {
    let summaries = queries::list_assets(&state.config.files, &state.config.python).await?;
    let matches: Vec<_> = summaries
        .iter()
        .filter(|s| s.id == name || s.id.ends_with(&format!(":{name}")))
        .collect();
    let summary = match matches.len() {
        0 => return Err(ApiError::NotFound(format!("asset '{name}' not found"))),
        1 => matches[0],
        _ => return Err(ApiError::Conflict(format!("'{name}' is ambiguous"))),
    };
    let id = summary.id.clone();
    let inputs = &summary.inputs;
    let snapshot = snapshot_db(&state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let mut result = barca_core::status::status(
        &cfg,
        std::slice::from_ref(&id),
        &state.config.files,
        &state.config.python,
        0,
        false,
    )
    .await?;
    result
        .nodes
        .retain(|n| n.id == id || inputs.contains(&n.id));
    barca_core::status::read_schemas(&state.config.python, &cfg, &mut result.nodes).await;
    Ok(Json(result.nodes))
}

/// `GET /assets/{name}` — summary joined with timing/cache stats for one asset.
pub async fn asset_detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // Use cached assets if available, otherwise fetch and cache them.
    let summaries = if let Some(cached) = state.cache.read().unwrap().assets.clone() {
        cached
    } else {
        let result = queries::list_assets(&state.config.files, &state.config.python).await?;
        state.cache.write().unwrap().assets = Some(result.clone());
        result
    };

    // Exact match first, then colon-prefixed match. No unbounded ends_with.
    let matches: Vec<_> = summaries
        .iter()
        .filter(|s| s.id == name || s.id.ends_with(&format!(":{name}")))
        .collect();

    let summary = match matches.len() {
        0 => return Err(ApiError::NotFound(format!("asset '{name}' not found"))),
        1 => matches[0].clone(),
        n => {
            let ids: Vec<_> = matches.iter().map(|s| s.id.as_str()).collect();
            return Err(ApiError::Conflict(format!(
                "'{name}' is ambiguous — matches {n} assets: {}",
                ids.join(", ")
            )));
        }
    };

    let stats = if state.config.read_only {
        let snap = snapshot_db(&state).await?;
        db::get_asset_stats(&snap.path, &summary.id).await?
    } else {
        queries::stats(
            &state.config.resolved,
            &summary.id,
            &state.config.files,
            &state.config.python,
        )
        .await?
    };

    Ok(Json(json!({
        "asset": summary,
        "stats": stats,
    })))
}

/// `POST /run` — get every asset and sensor (`barca get <files>` with no target; tasks are
/// skipped); returns a polling handle immediately.
pub async fn run(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let handle = start_run(state, None);
    Ok(Json(json!({ "run_id": handle })))
}

/// Check a trigger's target before a run is started for it, with the function `barca get`
/// and `barca run` use (`barca_core::targets::resolve_target_among`), so the server refuses what the
/// command line refuses, in the same words: an unknown name is `404`, a name that matches
/// several nodes `409`, the wrong verb for the node's kind `400`. So is source that does not
/// parse or a DAG that cannot be built.
async fn check_target(state: &AppState, name: &str, verb: &str) -> Result<(), ApiError> {
    let nodes = target_nodes(state).await?;
    let nodes = nodes.iter().map(|(id, kind)| (id.as_str(), *kind));
    match barca_core::targets::resolve_target_among(nodes, name, verb) {
        Ok(_) => Ok(()),
        Err(e @ barca_core::targets::TargetError::NotFound { .. }) => {
            Err(ApiError::NotFound(e.to_string()))
        }
        Err(e @ barca_core::targets::TargetError::Ambiguous { .. }) => {
            Err(ApiError::Conflict(e.to_string()))
        }
        // The resolver's own words name a command (`use `barca run` instead`); an HTTP
        // client is told the endpoint.
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

/// The nodes a run started now would find: read from the source, and kept for as long as
/// the source files do not change (their sizes and modification times are compared on every
/// call, which costs one `stat` per file). A run reads the source itself each time, so
/// checking against anything older, such as the `/assets` cache of a server without
/// `--watch`, would refuse a node that was just added and accept one that was just removed.
async fn target_nodes(
    state: &AppState,
) -> Result<std::sync::Arc<Vec<(String, barca_core::NodeKind)>>, ApiError> {
    // Taken before reading: if a file changes while it is read, the nodes are stored under
    // the older stamp and the next check reads again.
    let stamp = crate::state::SourceStamp::of(&state.config.files);
    if let Some(stamp) = &stamp
        && let Some(index) = state.cache.read().unwrap().targets.as_ref()
        && index.stamp == *stamp
    {
        return Ok(index.nodes.clone());
    }
    let nodes: Vec<(String, barca_core::NodeKind)> =
        queries::list_assets(&state.config.files, &state.config.python)
            .await?
            .into_iter()
            .map(|n| (n.id, n.kind))
            .collect();
    let nodes = std::sync::Arc::new(nodes);
    // Kept only when the files have been still for a moment (see `SourceStamp::settled`).
    state.cache.write().unwrap().targets = stamp
        .filter(|stamp| stamp.settled(std::time::SystemTime::now()))
        .map(|stamp| crate::state::TargetIndex {
            stamp,
            nodes: nodes.clone(),
        });
    Ok(nodes)
}

/// `POST /run/{target}` — trigger a task run; returns a polling handle. The target is
/// checked first ([`check_target`]): no run is started for a name that cannot run.
pub async fn run_target(
    State(state): State<AppState>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    check_target(&state, &target, "run").await?;
    let handle = start_run_task(state, target);
    Ok(Json(json!({ "run_id": handle })))
}

/// `POST /get/{target}` — trigger a target-scoped get; returns a polling handle. The target
/// is checked first ([`check_target`]): no run is started for a name that cannot be got.
pub async fn get_target(
    State(state): State<AppState>,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    check_target(&state, &target, "get").await?;
    let handle = start_run(state, Some(target));
    Ok(Json(json!({ "run_id": handle })))
}

/// `DELETE /run/{run_id}` — cancel an in-flight run. The run's workers are
/// terminated and its status transitions to `cancelled`; poll `/status/{id}`
/// to observe the transition. Cancelling a finished run is a no-op conflict.
pub async fn cancel_run(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    refuse_if_read_only(&state)?;
    let run = state
        .runs
        .get(&run_id)
        .ok_or_else(|| ApiError::NotFound(format!("run '{run_id}' not found")))?;
    match run.status {
        RunStatus::Pending | RunStatus::Running => {
            run.cancel.cancel();
            Ok(Json(json!({ "run_id": run_id, "status": "cancelling" })))
        }
        status => Err(ApiError::Conflict(format!(
            "run '{run_id}' already finished ({})",
            json!(status).as_str().unwrap_or("finished")
        ))),
    }
}

/// `GET /schedule` — list scheduled jobs with next fire time and last run status.
/// Reads the scheduler's published registry; volatile fields (next fire, live
/// status) are computed per request.
pub async fn schedule(State(state): State<AppState>) -> Json<Value> {
    Json(schedule_at(&state, chrono::Utc::now()))
}

/// The body of `GET /schedule` as of `now`. `next_fire` is computed in the zone the server
/// evaluates cron in (`--timezone`), so it is the time the scheduler will fire the job.
pub(crate) fn schedule_at(state: &AppState, now: chrono::DateTime<chrono::Utc>) -> Value {
    use barca_core::CronExpr;

    let zone = crate::scheduler::zone_of(&state.config);
    let jobs = state.schedule.read().map(|g| g.clone()).unwrap_or_default();
    let items: Vec<Value> = jobs
        .into_iter()
        .map(|j| {
            let next_fire = CronExpr::parse(&j.cron)
                .ok()
                .and_then(|c| barca_core::schedule::next_fire(&c, &zone, now))
                .map(|t| t.timestamp());
            let last_status = j
                .last_handle
                .as_ref()
                .and_then(|h| state.runs.get(h).map(|r| r.status));
            json!({
                "id": j.id,
                "cron": j.cron,
                "kind": j.kind,
                "next_fire": next_fire,
                "last_fired": j.last_fired,
                "last_run": j.last_handle,
                "last_status": last_status,
            })
        })
        .collect();
    json!(items)
}

/// `GET /status/{run_id}` — poll an in-flight or finished run.
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

/// `GET /events/{run_id}` — Server-Sent Events stream of a run's live events
/// (run lifecycle, logs, step completion). Replays the backlog first so a client
/// that connects a beat after the run starts still sees everything, then streams
/// live. Each SSE message is a JSON-encoded [`RunEvent`].
///
/// Sends `X-Accel-Buffering: no` so nginx (and proxies that honor it) pass
/// events through as they happen instead of buffering the response; idle
/// streams carry a keep-alive comment every 15s, inside nginx's default 60s
/// read timeout.
pub async fn events(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<
    (
        [(axum::http::HeaderName, &'static str); 1],
        Sse<impl Stream<Item = Result<Event, Infallible>>>,
    ),
    ApiError,
> {
    let channel: RunChannel = state
        .events
        .get(&run_id)
        .map(|c| c.clone())
        .ok_or_else(|| ApiError::NotFound(format!("run '{run_id}' not found")))?;

    let (backlog, rx) = channel.snapshot_and_subscribe();

    let backlog_stream = stream::iter(backlog);
    let live_stream = BroadcastStream::new(rx).filter_map(|r| async move { r.ok() });

    let stream = backlog_stream.chain(live_stream).map(|ev: RunEvent| {
        Ok(Event::default()
            .json_data(&ev)
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

/// `GET /logs/{run_id}` — persisted stdout lines for a run (durable history).
///
/// Accepts the server-side polling handle and resolves it to the DB run id
/// (which `execution::execute` generates and surfaces in the completed result);
/// also accepts a raw DB run id directly.
pub async fn logs(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // Map a polling handle to its DB run id if we know it; otherwise treat the
    // path param as a DB run id.
    let db_run_id = state
        .runs
        .get(&run_id)
        .and_then(|r| {
            r.db_run_id
                .clone()
                .or_else(|| r.result.as_ref().map(|res| res.run_id.clone()))
        })
        .unwrap_or(run_id);

    let entries = if state.config.read_only {
        let snap = snapshot_db(&state).await?;
        db::get_logs(&snap.path, &db_run_id).await?
    } else {
        let cfg = &state.config.resolved;
        db::ensure_env_dirs(&cfg.env)?;
        // Ensure the schema exists — /logs may be hit before any run, since the
        // server inits the DB lazily on first execution.
        db::init_db(&cfg.db_path).await?;
        db::get_logs(&cfg.db_path, &db_run_id).await?
    };

    Ok(Json(json!({ "logs": entries })))
}

/// Which core command a background run executes.
enum RunKind {
    /// `commands::get` with an optional target (assets).
    Get(Option<String>),
    /// `commands::run` for a task target, with how its upstream assets are treated.
    Task(String, CachePolicy),
}

/// Insert a `Pending` run, spawn the background execution task, and return the
/// server-side handle. The real DB run id is surfaced in the completed payload.
pub(crate) fn start_run(state: AppState, target: Option<String>) -> String {
    spawn_run(state, RunKind::Get(target))
}

/// Insert a `Pending` run for a task, spawn the background execution via
/// `commands::run`, and return the server-side handle.
pub(crate) fn start_run_task(state: AppState, target: String) -> String {
    spawn_run(state, RunKind::Task(target, CachePolicy::RefreshAll))
}

/// A cron tick for a scheduled task: the task runs, as a task always does, and each upstream
/// asset is recomputed only if something on its input side changed (what `barca run <task>`
/// does). A scheduled asset already goes through the cache-aware [`start_run`].
pub(crate) fn start_scheduled_task(state: AppState, target: String) -> String {
    spawn_run(state, RunKind::Task(target, CachePolicy::CacheAware))
}

fn spawn_run(state: AppState, kind: RunKind) -> String {
    let handle = db::generate_run_id();
    // Child of the server-wide shutdown token: DELETE /run/{id} cancels just
    // this run; graceful shutdown cancels all of them.
    let cancel = state.shutdown.child_token();
    state.runs.insert(
        handle.clone(),
        RunState {
            handle: handle.clone(),
            db_run_id: None,
            command: match &kind {
                RunKind::Get(_) => "get",
                RunKind::Task(_, _) => "run",
            }
            .into(),
            target: match &kind {
                RunKind::Get(target) => target.clone(),
                RunKind::Task(target, _) => Some(target.clone()),
            },
            status: RunStatus::Pending,
            result: None,
            error: None,
            started_at: now_ts(),
            finished_at: None,
            cancel: cancel.clone(),
        },
    );
    let channel = RunChannel::new();
    state.events.insert(handle.clone(), channel.clone());

    let st = state.clone();
    let h = handle.clone();
    tokio::spawn(async move {
        // Bound concurrency: acquire a run slot. Runs execute in parallel; the
        // shared metadata.db is kept safe by barca-core's process-wide DB lock.
        let _permit = st.run_slots.acquire().await.ok();

        // Cancelled while queued — never started, nothing to clean up.
        if cancel.is_cancelled() {
            if let Some(mut r) = st.runs.get_mut(&h) {
                r.status = RunStatus::Cancelled;
                r.error = Some("run cancelled".to_string());
                r.finished_at = Some(now_ts());
            }
            channel.emit(RunEvent::RunFinished {
                run_id: h.clone(),
                ok: false,
            });
            return;
        }

        if let Some(mut r) = st.runs.get_mut(&h) {
            r.status = RunStatus::Running;
        }
        channel.emit(RunEvent::RunStarted { run_id: h.clone() });

        // Core streams RunEvents over an unbounded channel (sync send from
        // inside the worker-pool loop). A drain task forwards them onto the
        // run's broadcast/backlog channel.
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunEvent>();
        let drain_ch = channel.clone();
        let drain_state = st.clone();
        let drain_handle = h.clone();
        let drain = tokio::spawn(async move {
            while let Some(ev) = event_rx.recv().await {
                if let RunEvent::RunStarted { run_id } = ev {
                    if let Some(mut run) = drain_state.runs.get_mut(&drain_handle) {
                        run.db_run_id = Some(run_id);
                    }
                } else {
                    drain_ch.emit(ev);
                }
            }
        });

        let files = st.config.files.clone();
        let python = st.config.python.clone();
        let cfg = st.config.resolved.clone();

        let mut timed_out = false;
        let outcome: Result<GetResult, BarcaError> = {
            let fut = async {
                match &kind {
                    RunKind::Get(target) => {
                        commands::get_streaming(
                            &cfg,
                            target.as_deref(),
                            &files,
                            &python,
                            CachePolicy::CacheAware,
                            true,
                            cancel.clone(),
                            Some(event_tx),
                        )
                        .await
                    }
                    RunKind::Task(target, policy) => {
                        commands::run_streaming(
                            &cfg,
                            target,
                            &files,
                            &python,
                            policy.clone(),
                            true,
                            cancel.clone(),
                            Some(event_tx),
                        )
                        .await
                    }
                }
            };
            tokio::pin!(fut);

            // On timeout, cancel the token and keep awaiting: the run observes the
            // cancellation, terminates its workers, persists partial results, and
            // returns — nothing is left running in the background.
            tokio::select! {
                res = &mut fut => res,
                _ = tokio::time::sleep(RUN_TIMEOUT) => {
                    // An operator cancel that is still unwinding when the deadline
                    // hits stays classified as cancelled, not as a timeout.
                    timed_out = !cancel.is_cancelled();
                    cancel.cancel();
                    fut.await
                }
            }
            // `fut` (and with it the event sender) is dropped here.
        };

        // The run has returned and its event sender is gone — await the drain
        // so trailing logs land before RunFinished.
        drain.await.ok();
        let ok = outcome.is_ok();

        if let Some(mut r) = st.runs.get_mut(&h) {
            r.finished_at = Some(now_ts());
            match outcome {
                Ok(result) => {
                    r.status = RunStatus::Complete;
                    r.result = Some(result);
                }
                Err(BarcaError::Cancelled) if timed_out => {
                    r.status = RunStatus::Failed;
                    r.error = Some(format!("run timed out after {}s", RUN_TIMEOUT.as_secs()));
                }
                Err(BarcaError::Cancelled) => {
                    r.status = RunStatus::Cancelled;
                    r.error = Some("run cancelled".to_string());
                }
                Err(e) => {
                    r.status = RunStatus::Failed;
                    r.error = Some(e.to_string());
                }
            }
        }
        channel.emit(RunEvent::RunFinished {
            run_id: h.clone(),
            ok,
        });
        // The run slot is released here, freeing capacity for a queued run.
    });

    handle
}

/// Evict completed/failed runs older than `max_age` from the in-memory runs map.
/// Intended to be spawned as a background task from `serve_async`.
pub async fn evict_finished_runs(state: AppState, interval: Duration, max_age: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        let cutoff = now_ts() - max_age.as_secs_f64();
        state.runs.retain(|handle, run| {
            let keep = match run.status {
                RunStatus::Complete | RunStatus::Failed | RunStatus::Cancelled => {
                    // Keep if it finished recently (or hasn't finished yet somehow).
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

/// Any request no route matches: `404` with the same `{"error": ...}` body as every other
/// error, naming what was asked for. A trigger with nothing after `/get/` or `/run/` is told
/// what is missing.
pub async fn no_route(method: axum::http::Method, uri: axum::http::Uri) -> ApiError {
    let path = uri.path();
    match (method.as_str(), path) {
        ("POST", "/get/" | "/get") => ApiError::NotFound(
            "POST /get/{target} needs a target: an asset or sensor name or its full id".into(),
        ),
        ("POST", "/run/") => ApiError::NotFound(
            "POST /run/{target} needs a target: a task or sensor name or its full id \
             (POST /run, without the slash, gets every asset and sensor)"
                .into(),
        ),
        _ => ApiError::NotFound(format!("no such endpoint: {method} {path}")),
    }
}

/// A known path asked with a method it does not take: `405` with the `{"error": ...}` body
/// and the methods it does take, also in the `Allow` header.
pub async fn wrong_method(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    let path = uri.path();
    let allowed = if path == "/run" || path.starts_with("/get/") {
        "POST"
    } else if path.starts_with("/run/") {
        "POST, DELETE"
    } else {
        "GET"
    };
    (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, allowed)],
        Json(json!({
            "error": format!("{method} is not allowed on {path}: use {}", allowed.replace(", ", " or "))
        })),
    )
        .into_response()
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    fn row(node_id: &str, status: &str, e: f64) -> db::MaterializationRow {
        db::MaterializationRow {
            node_id: node_id.into(),
            status: status.into(),
            created_at: String::new(),
            elapsed_seconds: Some(e),
            error_message: None,
        }
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v = [1.0, 2.0, 3.0, 4.0, 100.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.95), 100.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }

    #[test]
    fn durations_use_recent_successes_and_fold_partitions() {
        let mut rows: Vec<_> = (0..5).map(|_| row("p.py:a", "success", 1.0)).collect();
        rows.extend((0..20).map(|_| row("p.py:a", "success", 10.0)));
        rows.push(row("p.py:a", "failed", 999.0));
        rows.push(row("p.py:f[k=x]", "success", 2.0));
        rows.push(row("p.py:f[k=y]", "success", 4.0));
        let d = durations_by_node(&rows);
        let a = &d["p.py:a"];
        assert_eq!(
            (a.samples, a.median_seconds),
            (20, 10.0),
            "window + failures ignored"
        );
        assert_eq!(d["p.py:f"].samples, 2, "partitions fold into the base node");
        assert!(!d.contains_key("p.py:missing"));
    }
}
