//! Inspection endpoint handlers. CPU-heavy status and history reads and snapshot migrations
//! use the blocking pool so cheap routes remain responsive. Runs are in [`crate::runs`].

use crate::error::ApiError;
use crate::state::{AppState, Durations, NodeState};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use barca_core::queries;
use barca_core::results::PlanResult;
use barca_core::{BarcaError, db};
use serde_json::{Value, json};

/// `GET /health` — liveness (or `draining`), version, the API version a client checks, the
/// environment and artifact store this server uses, whether it is read-only, whether it runs
/// the scheduler (and whether it is still running), why the metadata DB cannot be used if it
/// cannot, and the cache size against its limit. No core work.
pub async fn health(State(state): State<AppState>) -> Json<Value> {
    let cfg = &state.config.resolved;
    Json(json!({
        // `draining` once the server was told to stop: it finishes its runs and starts none.
        "status": if state.is_draining() { "draining" } else { "ok" },
        // When this server started: a client that reconnects tells a restart by it.
        "started_at": state.started,
        // The run queue: paused or not, how many runs wait and how many execute.
        "queue": crate::queue::health(&state),
        "version": env!("CARGO_PKG_VERSION"),
        "api_version": barca_core::outcome::API_VERSION,
        "capabilities": { "event_resume": 1 },
        "env": cfg.env,
        // The artifact store this server writes to, so a client can refuse a mismatched
        // `[remote]`; null when artifacts stay on this machine.
        "store_uri": cfg.remote_artifacts().then(|| cfg.artifact_root.clone()),
        "read_only": state.config.read_only,
        "load_errors": *state.load_errors.read().unwrap(),
        // Whether this server fires `Schedule(...)` nodes: the same rule `serve`
        // uses to start the scheduler (on unless --no-schedule or --read-only).
        "scheduler": state.config.schedule && !state.config.read_only,
        // `scheduler`, and false once the scheduler stopped on an error.
        "scheduler_enabled": state.scheduler_enabled(),
        // Why the metadata DB cannot be used, while it cannot; null otherwise.
        "metadata_error": *state.metadata_error.read().unwrap(),
        // `.barca/` against its size limit, as last measured after a run or on the minute
        // checkpoint (RFC-0012); null before the first measurement or with the limit off.
        "cache": barca_core::gc::last_measured()
            .filter(|_| !state.config.read_only)
            .map(|(size, limit)| json!({"size_bytes": size, "limit_bytes": limit})),
    }))
}

/// A metadata DB a read-only request may query: the real one read in place
/// ([`db::ReadView`]) when this release uses it as it is; otherwise a private
/// snapshot of it, or — when there is no DB yet — an empty scratch DB, so the
/// absence is preserved. A copy has the schema ensured on it, never on the
/// original. Dropping it deletes the copy.
pub(crate) struct SnapshotDb {
    _view: Option<db::ReadView>,
    _snapshot: Option<db::DbSnapshot>,
    _scratch: Option<tempfile::TempDir>,
    pub(crate) path: String,
}

/// The shared snapshot of the metadata DB ([`AppState::read_snapshot`]): a copy taken when
/// the database last changed, shared by every request until it changes again.
pub(crate) async fn snapshot_db(state: &AppState) -> Result<std::sync::Arc<SnapshotDb>, ApiError> {
    Ok(state.note_metadata(state.read_snapshot().await)?)
}

/// Read a compatible WAL database in place, including on a `--read-only` server.
/// SQLite can create `-wal` and `-shm`; the reader never migrates or checkpoints the DB.
pub(crate) async fn snapshot_of(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    db::directory_check(db_path)?;
    if let Some(view) = db::ReadView::open(db_path).await? {
        return Ok(SnapshotDb {
            path: view.path().to_string(),
            _view: Some(view),
            _snapshot: None,
            _scratch: None,
        });
    }
    let db_path = db_path.to_owned();
    // Schema/index additions on the private copy can also take seconds on a large DB.
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(snapshot_copy(&db_path))
    })
    .await
    .map_err(|e| BarcaError::Db(format!("snapshot task failed: {e}")))?
}

async fn snapshot_copy(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    let (snapshot, scratch, path) = match db::DbSnapshot::take(db_path).await? {
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
    db::init_db_as(&path, db_path).await?;
    Ok(SnapshotDb {
        _view: None,
        _snapshot: snapshot,
        _scratch: scratch,
        path,
    })
}

/// Experimental target selection for `GET /state`.
#[derive(Default, serde::Deserialize)]
pub struct StateQuery {
    /// Experimental: comma-separated target names, full ids or group ids (`group:FILE:NAME`,
    /// every node in the group); absent/empty with no `files` means all nodes.
    pub targets: Option<String>,
    /// Experimental: comma-separated source files, as in node ids; adds every node they define.
    pub files: Option<String>,
}

/// `GET /state` — every node's cache state (would `barca get` reuse it?), latest
/// attempt, typical durations, and next scheduled run. Read-only by
/// construction: the cache check reads the DB through read-only connections ([`SnapshotDb`]).
///
/// The cache states and durations are kept until the sources or the database change
/// ([`crate::read_cache`]); what runs now and the next fire times are read for every request.
pub async fn state(
    State(state): State<AppState>,
    Query(query): Query<StateQuery>,
) -> Result<Json<Vec<NodeState>>, ApiError> {
    state.reads.polled();
    let split = |list: &Option<String>| -> Vec<String> {
        list.iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (targets, files) = (split(&query.targets), split(&query.files));
    // A group or a file stands for its nodes, so a client scopes a view of thousands of
    // nodes with one short query instead of a URL listing every id.
    let mut targets = if targets.iter().any(|t| t.starts_with("group:")) || !files.is_empty() {
        expand_targets(&*state.inspect_dag().await?, targets, &files)?
    } else {
        targets
    };
    targets.sort();
    targets.dedup();
    Ok(Json(state_rows_for_targets(&state, &targets).await?))
}

/// `targets` with each group id replaced by the nodes in it (nested groups included), and
/// every node of each of `files` added. An unknown group or file is `404`.
fn expand_targets(
    dag: &barca_core::dag::Dag,
    targets: Vec<String>,
    files: &[String],
) -> Result<Vec<String>, ApiError> {
    let groups: std::collections::HashMap<&str, &barca_core::groups::NodeGroup> =
        dag.groups.iter().map(|g| (g.id.as_str(), g)).collect();
    fn leaves(
        id: &str,
        groups: &std::collections::HashMap<&str, &barca_core::groups::NodeGroup>,
        out: &mut Vec<String>,
    ) {
        match groups.get(id) {
            Some(group) => group.members.iter().for_each(|m| leaves(m, groups, out)),
            None => out.push(id.to_owned()),
        }
    }
    let mut out = Vec::new();
    for target in targets {
        if target.starts_with("group:") && !groups.contains_key(target.as_str()) {
            return Err(ApiError::NotFound(format!(
                "no group {target}\nGET /groups lists them."
            )));
        }
        leaves(&target, &groups, &mut out);
    }
    for file in files {
        let before = out.len();
        out.extend(
            dag.topo_order()
                .into_iter()
                .filter(|id| id.rsplit_once(':').is_some_and(|(f, _)| f == file))
                .map(str::to_owned),
        );
        if out.len() == before {
            return Err(ApiError::NotFound(format!(
                "no nodes in {file}\nGET /assets lists every node id, whose file is before its last `:`."
            )));
        }
    }
    Ok(out)
}

/// The body of `GET /state`, also warmed at startup.
pub(crate) async fn state_rows(state: &AppState) -> Result<Vec<NodeState>, ApiError> {
    state_rows_for_targets(state, &[]).await
}

async fn state_rows_for_targets(
    state: &AppState,
    targets: &[String],
) -> Result<Vec<NodeState>, ApiError> {
    let cfg = &state.config;
    let zone = crate::scheduler::zone_of(cfg);
    let dag = state.inspect_dag().await?;
    let db_state = state.db_state();
    let snapshot = snapshot_db(state).await?;
    // Read at most once per database state, whatever the scope (`read_cache`).
    let ages = state
        .cached_ages(&db_state, {
            let snapshot = snapshot.clone();
            move || async move { db::node_ages(&snapshot.path).await }
        })
        .await?;
    let (status, durations) = if targets.is_empty() {
        // Together: after a change, each waits a moment for its new value (`read_cache`).
        let (status, durations) = tokio::join!(
            whole_status(state, &dag, &db_state, &snapshot),
            state.cached_durations(&dag, &db_state, {
                let (dag, snapshot) = (dag.clone(), snapshot.clone());
                move || async move {
                    let ids: Vec<String> =
                        dag.topo_order().into_iter().map(str::to_string).collect();
                    Ok(durations_from(
                        db::recent_durations(&snapshot.path, &ids, DURATION_WINDOW).await?,
                    ))
                }
            })
        );
        (status?, durations?)
    } else {
        let status = status_for_targets(state, &dag, &db_state, &snapshot, targets).await?;
        let ids = status
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let snapshot = snapshot.clone();
        let durations = crate::read_cache::off_the_workers(move || async move {
            Ok(durations_from(
                db::recent_durations(&snapshot.path, &ids, DURATION_WINDOW).await?,
            ))
        })
        .await?;
        (status, std::sync::Arc::new(durations))
    };
    let runs = crate::running::in_flight(state, Some(&snapshot.path)).await;
    let running = crate::running::by_node(&runs, &status.nodes, &dag);
    Ok(rows(&status.nodes, &durations, &ages, &dag, &zone, running))
}

/// `barca status` of every node without artifact shapes, against `snapshot` of the database in
/// state `db_state`: what `GET /state` and the `running` fields are built from, kept like every
/// status result.
pub(crate) async fn whole_status(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
) -> Result<std::sync::Arc<barca_core::status::StatusResult>, ApiError> {
    status_for_targets(state, dag, db_state, snapshot, &[]).await
}

async fn status_for_targets(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
    targets: &[String],
) -> Result<std::sync::Arc<barca_core::status::StatusResult>, ApiError> {
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let python = state.config.python.clone();
    let (graph, snapshot, names) = (dag.clone(), snapshot.clone(), targets.to_vec());
    Ok(state
        .cached_status(
            dag,
            db_state,
            (targets.to_vec(), 0, false),
            move || async move {
                let _snapshot = snapshot;
                barca_core::status::status_from_dag(&cfg, &names, &graph, &python, 0, false).await
            },
        )
        .await?)
}

/// The `GET /state` rows: each node's status with its durations, next fire time and run.
fn rows(
    nodes: &[barca_core::status::NodeStatus],
    durations: &std::collections::HashMap<String, Durations>,
    ages: &std::collections::HashMap<String, db::NodeAge>,
    dag: &barca_core::dag::Dag,
    zone: &crate::scheduler::Zone,
    mut running: std::collections::HashMap<String, crate::running::NodeRunning>,
) -> Vec<NodeState> {
    let next_run: std::collections::HashMap<String, i64> =
        barca_core::schedule::describe_schedule_from_dag(Some(dag), zone)
            .into_iter()
            .filter_map(|s| s.next_fire.map(|t| (s.id, t)))
            .collect();
    nodes
        .iter()
        .map(|n| {
            let age = ages.get(&n.id).cloned().unwrap_or_default();
            NodeState {
                durations: durations.get(&n.id).cloned(),
                next_run: next_run.get(&n.id).copied(),
                running: running.remove(&n.id),
                status: n.clone(),
                last_success_at: age.last_success_at,
                failing_since: age.failing_since,
                consecutive_failures: age.consecutive_failures,
                last_run: age.last_run.map(Into::into),
            }
        })
        .collect()
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
    let dag = barca_core::load::build_dag(files, python).await?;
    node_states_in(cfg, &dag, python, &crate::scheduler::Zone::Local, None).await
}

/// [`node_states`] with cron evaluated in `zone`, and, for a server, what its runs and the
/// CLI runs on the same project are working on now ([`crate::running`]).
pub(crate) async fn node_states_in(
    cfg: &barca_core::config::ResolvedConfig,
    dag: &barca_core::dag::Dag,
    python: &std::path::Path,
    zone: &crate::scheduler::Zone,
    server: Option<&AppState>,
) -> Result<Vec<NodeState>, BarcaError> {
    db::directory_check(&cfg.db_path)?;
    let snapshot = db::DbSnapshot::take(&cfg.db_path).await?;
    // A database this release cannot use is one error naming it, not a failed query.
    if let Some(s) = &snapshot {
        db::init_db_as(s.path(), &cfg.db_path).await?;
    }
    let scratch = tempfile::tempdir()
        .map_err(|e| BarcaError::Db(format!("failed to create scratch dir: {e}")))?;
    let mut snap_cfg = cfg.clone();
    // No DB yet: point at a path that doesn't exist — "nothing cached", and
    // nothing is created.
    snap_cfg.db_path = match &snapshot {
        Some(s) => s.path().to_string(),
        None => scratch.path().join("metadata.db").display().to_string(),
    };

    let status = barca_core::status::status_from_dag(&snap_cfg, &[], dag, python, 0, false).await;
    let (durations, ages) = match &snapshot {
        Some(s) => {
            let ids: Vec<String> = dag.topo_order().into_iter().map(str::to_string).collect();
            (
                durations_from(db::recent_durations(s.path(), &ids, DURATION_WINDOW).await?),
                db::node_ages(s.path()).await?,
            )
        }
        None => Default::default(),
    };
    let nodes = status?.nodes;
    let running = match server {
        Some(state) => {
            let runs = crate::running::in_flight(state, snapshot.as_ref().map(|s| s.path())).await;
            crate::running::by_node(&runs, &nodes, dag)
        }
        None => Default::default(),
    };
    Ok(rows(&nodes, &durations, &ages, dag, zone, running))
}

/// Successful materializations considered for typical durations.
const DURATION_WINDOW: usize = 20;

/// Median and p95 of each node's recent successful runs ([`db::recent_durations`], at most
/// [`DURATION_WINDOW`] of them, partition keys folded into their node).
fn durations_from(
    recent: std::collections::HashMap<String, Vec<f64>>,
) -> std::collections::HashMap<String, Durations> {
    recent
        .into_iter()
        .filter_map(|(id, mut recent)| {
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
    let dag = state.loaded_dag().await?;
    let cfg = &state.config.resolved;
    let sizing = barca_core::sizing(
        cfg.capacity,
        cfg.pool_size,
        state
            .capacity
            .as_ref()
            .map(|c| (c.total() as usize, c.source())),
    );
    let result = queries::plan_from_dag(&dag, sizing);
    state.cache.write().unwrap().plan = Some(result.clone());
    Ok(Json(result))
}

/// `GET /assets` — every node as `barca list --json` shows it: `freshness` a lowercase
/// word, the cron expression in `schedule` and the next fire time (this machine's local
/// time) in `next_fire` for a scheduled node.
pub async fn assets(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let dag = state.loaded_dag().await?;
    let result = queries::list_assets_from_dag(&dag);
    let next_fires = next_fires(&dag);
    let nodes = result
        .iter()
        .map(|a| barca_core::report::list_node_json(a, next_fires.get(&a.id)))
        .collect();
    state.cache.write().unwrap().assets = Some(result);
    Ok(Json(nodes))
}

/// Each scheduled node's next fire time in this machine's local time, as `barca list` shows.
pub(crate) fn next_fires(dag: &barca_core::dag::Dag) -> std::collections::HashMap<String, String> {
    barca_core::schedule::describe_schedule_from_dag(Some(dag), &barca_core::schedule::Zone::Local)
        .into_iter()
        .filter_map(|j| j.next_fire_local.map(|t| (j.id, t)))
        .collect()
}

/// The most rows `GET /assets/{name}/schema?sample=N` returns per artifact.
const MAX_SCHEMA_SAMPLE: usize = 20;

#[derive(serde::Deserialize)]
pub struct SchemaQuery {
    /// First rows of each json/csv/parquet artifact, in `shape.sample`.
    sample: Option<usize>,
}

/// `GET /assets/{name}/schema` — inspect the selected node and its direct inputs.
/// Reads artifact shapes on demand rather than on every `/state` poll. Uses the
/// same safe inspector as `barca status`: parquet footers, JSON, pickle opcodes.
pub async fn asset_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<SchemaQuery>,
) -> Result<Json<Vec<barca_core::status::NodeStatus>>, ApiError> {
    let dag = state.inspect_dag().await?;
    let summaries = queries::list_assets_from_dag(&dag);
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
    let db_state = state.db_state();
    let snapshot = snapshot_db(&state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let mut nodes = whole_status(&state, &dag, &db_state, &snapshot)
        .await?
        .nodes
        .clone();
    nodes.retain(|n| n.id == id || inputs.contains(&n.id));
    barca_core::status::read_schemas(
        &state.config.python,
        &cfg,
        &mut nodes,
        query.sample.unwrap_or(0).min(MAX_SCHEMA_SAMPLE),
    )
    .await;
    Ok(Json(nodes))
}

/// `GET /assets/{name}` — one node as `barca list --json` shows it, with its
/// `barca stats --json` body.
pub async fn asset_detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.loaded_dag().await?;
    let dag = state.loaded_dag().await?;
    let summaries = queries::list_assets_from_dag(&dag);

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

    let db_state = state.db_state();
    let snap = snapshot_db(&state).await?;
    let stats = db::get_asset_stats(&snap.path, &summary.id).await?;

    let next_fire = next_fires(&dag).remove(&summary.id);
    let running = running_node(&state, &db_state, &snap, &summary.id).await?;
    Ok(Json(json!({
        "asset": barca_core::report::list_node_json(&summary, next_fire.as_ref()),
        "stats": barca_core::report::stats_json(&stats),
        "running": running,
    })))
}

/// The run working on node `id` now ([`crate::running`]), read from `snapshot` of the database
/// in state `db_state`. The cache state it needs is computed only while some run is in flight.
async fn running_node(
    state: &AppState,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
    id: &str,
) -> Result<Option<crate::running::NodeRunning>, ApiError> {
    let runs = crate::running::in_flight(state, Some(&snapshot.path)).await;
    if runs.is_empty() {
        return Ok(None);
    }
    let dag = state.inspect_dag().await?;
    let status = whole_status(state, &dag, db_state, snapshot).await?;
    Ok(crate::running::by_node(&runs, &status.nodes, &dag).remove(id))
}

/// `GET /schedule` — list scheduled jobs with next fire time and last run status.
/// Reads the scheduler's published registry; volatile fields (next fire, live
/// status) are computed per request.
pub async fn schedule(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    crate::pause::pauses(&state).await?;
    Ok(Json(schedule_at(&state, chrono::Utc::now())))
}

/// Each scheduled node's `schedule` in `GET /nodes` (`barca status`): what the metadata DB holds
/// (failures, pauses, the last tick), with next fire times in the server's `--timezone`, and,
/// while the scheduler runs, its `GET /schedule` entry, so the numbers are the ones the web UI
/// shows.
pub(crate) async fn attach_live_schedules(
    state: &AppState,
    nodes: &mut [barca_core::status::NodeStatus],
    dag: &barca_core::dag::Dag,
    db_path: &str,
) -> Result<(), ApiError> {
    let now = chrono::Utc::now();
    let zone = crate::scheduler::zone_of(&state.config);
    barca_core::status::attach_schedules(nodes, dag, Some(db_path), &zone, now).await?;
    crate::pause::pauses(state).await?;
    let live = schedule_at(state, now);
    let secs = |v: &Value| v.as_i64().and_then(barca_core::status::rfc3339_secs);
    let text = |v: &Value| v.as_str().map(str::to_string);
    for node in nodes.iter_mut() {
        let Some(schedule) = node.schedule.as_mut() else {
            continue;
        };
        let Some(job) = live
            .as_array()
            .into_iter()
            .flatten()
            .find(|j| j["id"].as_str() == Some(node.id.as_str()))
        else {
            continue;
        };
        schedule.next_fire = secs(&job["next_fire"]);
        schedule.last_fired = secs(&job["last_fired"]);
        schedule.last_status = text(&job["last_status"]);
        schedule.failures = job["failures"].as_u64().unwrap_or(0) as u32;
        schedule.max_failures = job["max_failures"]
            .as_u64()
            .map_or(schedule.max_failures, |n| n as u32);
        schedule.failing = job["failing"].as_bool().unwrap_or(false);
        schedule.paused = job["paused"].as_bool().unwrap_or(false);
        schedule.paused_until = secs(&job["paused_until"]);
        schedule.skip_reason = text(&job["skip_reason"]);
        schedule.live = true;
    }
    Ok(())
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
            let pause = crate::pause::active(state, &j.id, now.timestamp());
            json!({
                "id": j.id,
                "cron": j.cron,
                "kind": j.kind,
                "next_fire": next_fire,
                "last_fired": j.last_fired,
                "last_run": j.last_handle,
                "last_status": last_status,
                "last_skipped": j.last_skipped,
                "skipped_for": j.skipped_for,
                "skip_reason": j.skip_reason,
                "paused": pause.is_some(),
                "paused_until": pause.and_then(|p| p.until),
                "failures": j.failures,
                "max_failures": j.max_failures,
                "failing": j.failures >= j.max_failures,
            })
        })
        .collect();
    json!(items)
}

/// `GET /logs/{run_id}` — persisted stdout lines for a run (durable history).
pub async fn logs(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let snap = snapshot_db(&state).await?;
    let entries = db::get_logs(&snap.path, &run_id).await?;
    Ok(Json(json!({ "logs": entries })))
}

/// Any request no route matches: `404` with the error envelope every other error has,
/// naming what was asked for. A trigger with nothing after `/get/` or `/run/` is told what is
/// missing.
pub async fn no_route(method: axum::http::Method, uri: axum::http::Uri) -> ApiError {
    let path = uri.path();
    match (method.as_str(), path) {
        ("POST", "/get/" | "/get") => ApiError::NotFound(
            "POST /get/{target} needs a target: an asset or sensor name or its full id\nPOST /runs with {\"command\": \"get\", \"targets\": [...]} is the run route.".into(),
        ),
        ("POST", "/run/") => ApiError::NotFound(
            "POST /run/{target} needs a target: a task or sensor name or its full id \
             (POST /run, without the slash, gets every asset and sensor)\nPOST /runs with {\"command\": \"run\", \"targets\": [...]} is the run route."
                .into(),
        ),
        _ => ApiError::NotFound(format!(
            "no such endpoint: {method} {path}\nThe routes are listed at https://barca.sh/reference/server-api/."
        )),
    }
}

/// The methods `path` takes, for a `405`.
fn allowed_methods(path: &str) -> &'static str {
    let runs_item = path
        .strip_prefix("/runs/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
    if path == "/runs" {
        "GET, POST"
    } else if runs_item {
        "GET, DELETE"
    } else if path == "/run"
        || path.starts_with("/get/")
        || (path.starts_with("/schedule/")
            && (path.ends_with("/pause") || path.ends_with("/resume")))
    {
        "POST"
    } else if path.starts_with("/run/") {
        "POST, DELETE"
    } else {
        "GET"
    }
}

/// A known path asked with a method it does not take: `405` with the error envelope and the
/// methods it does take, also in the `Allow` header.
pub async fn wrong_method(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    let path = uri.path();
    let allowed = allowed_methods(path);
    let error = ApiError::BadRequest(format!(
        "{method} is not allowed on {path}: use {}",
        allowed.replace(", ", " or ")
    ));
    let (_, envelope) = error.parts();
    (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, allowed)],
        Json(envelope.to_json()),
    )
        .into_response()
}

/// Organizational metadata; never part of the execution graph.
pub async fn groups(
    State(state): State<AppState>,
) -> Result<Json<Vec<barca_core::groups::NodeGroup>>, ApiError> {
    let dag = state.loaded_dag().await?;
    Ok(Json(dag.groups.clone()))
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    use crate::state::RunStatus;
    use std::time::Duration;

    #[test]
    fn a_queued_run_revalidates_source_before_any_worker_starts() {
        crate::test_support::bounded(|| async {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("p.py").display().to_string();
            std::fs::write(
                &file,
                "from barca import asset\n@asset()\ndef target(): return 1\n",
            )
            .unwrap();
            let mut cfg = barca_core::config::resolve_in(None, dir.path()).unwrap();
            cfg.db_path = dir.path().join("metadata.db").display().to_string();
            let mut state = AppState::new(crate::ServeConfig {
                files: vec![file.clone()],
                host: "127.0.0.1".parse().unwrap(),
                port: 0,
                watch: false,
                schedule: false,
                timezone: "utc".into(),
                // A stale graph would try spawning this nonexistent interpreter.
                python: "/nonexistent-barca-python".into(),
                resolved: cfg,
                read_only: false,
            });
            state.run_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
            assert!(
                crate::runs::check_target(&state, "target", "get")
                    .await
                    .is_ok()
            );
            // Every slot taken: the run waits in the queue.
            let held = state.run_slots.clone().try_acquire_owned().unwrap();
            let handle = crate::runs::start(
                state.clone(),
                crate::runs::RunSpec::api(
                    barca_core::outcome::RunCommand::Get,
                    vec!["target".into()],
                    vec![],
                ),
            )
            .await
            .unwrap();
            assert_eq!(state.runs.get(&handle).unwrap().status, RunStatus::Pending);
            std::fs::write(&file, "from barca import asset\ndef broken(: pass\n").unwrap();
            drop(held);
            crate::queue::dispatch(&state);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if state.runs.get(&handle).unwrap().status == RunStatus::Failed {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let run = state.runs.get(&handle).unwrap();
            assert!(
                run.error.as_ref().unwrap().contains("not found"),
                "{:?}",
                run.error
            );
            assert!(
                state.load_errors.read().unwrap()[0]
                    .error
                    .contains("syntax error")
            );
        });
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v = [1.0, 2.0, 3.0, 4.0, 100.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.95), 100.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }
}
