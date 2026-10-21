//! Inspection endpoint handlers. Core commands are `async fn`s that run directly on the
//! server's runtime — handlers simply `.await` them. Runs are in [`crate::runs`].

use crate::error::ApiError;
use crate::state::{AppState, Durations, NodeState};
use axum::Json;
use axum::extract::{Path, State};
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
        "version": env!("CARGO_PKG_VERSION"),
        "api_version": barca_core::outcome::API_VERSION,
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
    Ok(state.note_metadata(snapshot_of(&state.config.resolved.db_path).await)?)
}

async fn snapshot_of(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    db::directory_check(db_path)?;
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
    let dag = state.current_dag().await?;
    Ok(Json(state.note_metadata(
        node_states_in(&cfg.resolved, &dag, &cfg.python, &zone, Some(&state)).await,
    )?))
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
    let schedule = barca_core::schedule::describe_schedule_from_dag(Some(dag), zone);
    let history = match &snapshot {
        Some(s) => db::materialization_history(s.path()).await?,
        None => Vec::new(),
    };
    let durations = durations_by_node(&history);
    let next_run: std::collections::HashMap<String, i64> = schedule
        .into_iter()
        .filter_map(|s| s.next_fire.map(|t| (s.id, t)))
        .collect();

    let nodes = status?.nodes;
    let mut running = match server {
        Some(state) => {
            let runs = crate::running::in_flight(state, snapshot.as_ref().map(|s| s.path())).await;
            crate::running::by_node(&runs, &nodes, dag)
        }
        None => Default::default(),
    };
    Ok(nodes
        .into_iter()
        .map(|n| NodeState {
            durations: durations.get(&n.id).cloned(),
            next_run: next_run.get(&n.id).copied(),
            running: running.remove(&n.id),
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
    let dag = state.loaded_dag().await?;
    let result = queries::plan_from_dag(&dag);
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

/// `GET /assets/{name}/schema` — inspect the selected node and its direct inputs.
/// Reads artifact shapes on demand rather than on every `/state` poll. Uses the
/// same safe inspector as `barca status`: parquet footers, JSON, pickle opcodes.
pub async fn asset_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<barca_core::status::NodeStatus>>, ApiError> {
    let dag = state.current_dag().await?;
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
    let snapshot = snapshot_db(&state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let mut result = barca_core::status::status_from_dag(
        &cfg,
        std::slice::from_ref(&id),
        &dag,
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

    let snap = snapshot_db(&state).await?;
    let stats = db::get_asset_stats(&snap.path, &summary.id).await?;

    let next_fire = next_fires(&dag).remove(&summary.id);
    let running = running_node(&state, &dag, &snap.path, &summary.id).await?;
    Ok(Json(json!({
        "asset": barca_core::report::list_node_json(&summary, next_fire.as_ref()),
        "stats": barca_core::report::stats_json(&stats),
        "running": running,
    })))
}

/// The run working on node `id` now ([`crate::running`]), read from the snapshot at
/// `read_path`. The cache state it needs is computed only while some run is in flight.
async fn running_node(
    state: &AppState,
    dag: &barca_core::dag::Dag,
    read_path: &str,
    id: &str,
) -> Result<Option<crate::running::NodeRunning>, ApiError> {
    let runs = crate::running::in_flight(state, Some(read_path)).await;
    if runs.is_empty() {
        return Ok(None);
    }
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = read_path.to_string();
    let status =
        barca_core::status::status_from_dag(&cfg, &[], dag, &state.config.python, 0, false).await?;
    Ok(crate::running::by_node(&runs, &status.nodes, dag).remove(id))
}

/// `GET /schedule` — list scheduled jobs with next fire time and last run status.
/// Reads the scheduler's published registry; volatile fields (next fire, live
/// status) are computed per request.
pub async fn schedule(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    crate::pause::pauses(&state).await?;
    Ok(Json(schedule_at(&state, chrono::Utc::now())))
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

    fn row(node_id: &str, status: &str, e: f64) -> db::MaterializationRow {
        db::MaterializationRow {
            node_id: node_id.into(),
            status: status.into(),
            created_at: String::new(),
            elapsed_seconds: Some(e),
            error_message: None,
        }
    }

    use crate::state::RunStatus;
    use std::time::Duration;

    #[tokio::test]
    async fn a_queued_run_revalidates_source_before_any_worker_starts() {
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
        let held = state.run_slots.acquire().await.unwrap();
        let handle = crate::runs::start(
            state.clone(),
            crate::runs::RunSpec::api(
                barca_core::outcome::RunCommand::Get,
                vec!["target".into()],
                vec![],
            ),
        );
        assert_eq!(state.runs.get(&handle).unwrap().status, RunStatus::Pending);
        std::fs::write(&file, "from barca import asset\ndef broken(: pass\n").unwrap();
        drop(held);
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
