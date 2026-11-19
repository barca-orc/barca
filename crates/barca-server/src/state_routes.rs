//! `GET /health`, `GET /state` and `GET /state/rollup`: how the server and every node are now.

use crate::error::ApiError;
use crate::health_view::{health_facts, state_rows_for_targets};
use crate::read_cache::Reads;
use crate::state::AppState;
use crate::views::NodeState;
use axum::Json;
use axum::extract::{Query, State};
use barca_core::served::{CacheSize, Capabilities, HealthStatus, ServerHealth};

/// `GET /health` — liveness (or `draining`), version, the API version a client checks, the
/// environment and artifact store this server uses, whether it is read-only, whether it runs
/// the scheduler (and whether it is still running), why the metadata DB cannot be used if it
/// cannot, and the cache size against its limit. No core work, and no wait on a read.
pub async fn health(State(state): State<AppState>) -> Json<ServerHealth> {
    let cfg = &state.config.resolved;
    Json(ServerHealth {
        // `draining` once the server was told to stop: it finishes its runs and starts none.
        status: if state.lifecycle.is_draining() {
            HealthStatus::Draining
        } else {
            HealthStatus::Ok
        },
        // When this server started: a client that reconnects tells a restart by it.
        started_at: state.lifecycle.started.clone(),
        // The run queue: paused or not, how many runs wait and how many execute.
        queue: crate::queue_view::health(&state),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: barca_core::outcome::API_VERSION,
        capabilities: Capabilities { event_resume: 1 },
        env: cfg.env.clone(),
        // The artifact store this server writes to, so a client can refuse a mismatched
        // `[remote]`; null when artifacts stay on this machine.
        store_uri: cfg.remote_artifacts().then(|| cfg.artifact_root.clone()),
        read_only: state.config.read_only,
        graph_stale: state.reads.sources.graph_stale(),
        load_errors: state.reads.sources.load_errors.read().unwrap().clone(),
        // Whether this server fires `Schedule(...)` nodes: the same rule `serve`
        // uses to start the scheduler (on unless --no-schedule or --read-only).
        scheduler: state.config.schedule && !state.config.read_only,
        // `scheduler`, and false once the scheduler stopped on an error.
        scheduler_enabled: state.scheduler_enabled(),
        // Why the metadata DB cannot be used, while it cannot; null otherwise.
        metadata_error: state.reads.metadata_error.read().unwrap().clone(),
        // `.barca/` against its size limit, as last measured after a run or on the minute
        // checkpoint (RFC-0012); null before the first measurement or with the limit off.
        cache: barca_core::gc::last_measured()
            .filter(|_| !state.config.read_only)
            .map(|(size_bytes, limit_bytes)| CacheSize {
                size_bytes,
                limit_bytes,
            }),
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
    #[serde(default)]
    pub now: bool,
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
    state.reads.cache.polled();
    let targets = scope_targets(&state.reads, &query).await?;
    Ok(Json(state_rows_for_targets(&state, &targets).await?))
}

/// The targets a `GET /state` or `GET /state/rollup` query names, sorted: empty for every node.
async fn scope_targets(reads: &Reads, query: &StateQuery) -> Result<Vec<String>, ApiError> {
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
        expand_targets(&*reads.inspect_dag().await?, targets, &files)?
    } else {
        targets
    };
    targets.sort();
    targets.dedup();
    Ok(targets)
}

/// `GET /state/rollup` — the rows of `GET /state` summed into one tree (root, group, file,
/// node), each level the exact sum of its children, in nodes and in assets
/// ([`barca_core::rollup`]). It takes the scope of `GET /state` and sums the status result
/// that route's rows are built from ([`crate::read_cache`]): no status is computed for it
/// alone. Health is classified per answer, on the server's clock, exactly as `GET /state`
/// classifies it, so a node's health is the same word in both. `as_of` is the start of the
/// current minute (a schedule's lateness moves by the minute), so an unchanged poll within a
/// minute is a `304`.
pub async fn rollup(
    State(state): State<AppState>,
    Query(query): Query<StateQuery>,
) -> Result<Json<barca_core::rollup::Rollup>, ApiError> {
    state.reads.cache.polled();
    let targets = scope_targets(&state.reads, &query).await?;
    let dag = state.reads.inspect_dag().await?;
    let db_state = state.reads.db_state();
    let snapshot = state.reads.snapshot_db().await?;
    let status = state.reads.status_for_targets(&dag, &db_state, &snapshot, &targets).await?;
    let mut result = (*status).clone();
    let now = chrono::Utc::now();
    let as_of = if query.now {
        now
    } else {
        chrono::DateTime::from_timestamp(now.timestamp() / 60 * 60, 0).unwrap_or(now)
    };
    let facts = health_facts(&state, &dag, &db_state, &snapshot.path, as_of).await?;
    let health = facts.inputs().assemble();
    // "N running" counts the nodes a run is executing, not every node of every run in flight.
    let running =
        crate::running::executing(&crate::running::by_node(&facts.runs, &result.nodes, &dag));
    health.attach(&mut result, &running);
    let mut rollup = health.rollup(&result, &dag.groups, &running, as_of, true);
    // Named by the targets alone, as `barca status --rollup` names it locally: a rollup of
    // whole files keeps the root's `all`.
    let scope: Vec<_> = query
        .targets
        .as_deref()
        .into_iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !scope.is_empty() {
        rollup.entries[0].label = scope.join(", ");
    }
    Ok(Json(rollup))
}

/// `targets` with each group id replaced by the nodes in it (nested groups included), and
/// every node of each of `files` added. An unknown group or file is `404`.
fn expand_targets(
    dag: &barca_core::dag::Dag,
    targets: Vec<String>,
    files: &[String],
) -> Result<Vec<String>, ApiError> {
    for target in &targets {
        if target.starts_with("group:") && !dag.groups.iter().any(|g| g.id == *target) {
            return Err(ApiError::NotFound(format!(
                "no group {target}\nGET /groups lists them."
            )));
        }
    }
    let mut out = barca_core::status::group_targets(dag, &targets)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
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
