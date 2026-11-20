//! The graph's own routes: `GET /plan`, `GET /groups`, `GET /assets` and one asset's detail
//! and schema.

use crate::error::ApiError;
use crate::read_cache::Reads;
use crate::schedule_view::next_fires;
use crate::snapshot::SnapshotDb;
use crate::state::{AppState, Runs};
use axum::Json;
use axum::extract::{Path, Query, State};
use barca_service::db;
use barca_service::queries;
use barca_service::results::PlanResult;
use serde_json::{Value, json};

/// `GET /plan` — execution plan for the server's files (cache-aware).
pub async fn plan(
    State(reads): State<Reads>,
    State(runs): State<Runs>,
) -> Result<Json<PlanResult>, ApiError> {
    let dag = reads.loaded_dag().await?;
    let cfg = &reads.config.resolved;
    let sizing = barca_service::sizing(cfg.capacity, cfg.pool_size, runs.server_capacity());
    Ok(Json(queries::plan_from_dag(&dag, sizing)))
}

/// `GET /assets` — every node as `barca list --json` shows it: `freshness` a lowercase
/// word, the cron expression in `schedule` and the next fire time (this machine's local
/// time) in `next_fire` for a scheduled node.
pub async fn assets(State(reads): State<Reads>) -> Result<Json<Vec<Value>>, ApiError> {
    let dag = reads.loaded_dag().await?;
    let result = queries::list_assets_from_dag(&dag);
    let next_fires = next_fires(&dag);
    let nodes = result
        .iter()
        .map(|a| barca_service::report::list_node_json(a, next_fires.get(&a.id)))
        .collect();
    Ok(Json(nodes))
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
    State(reads): State<Reads>,
    Path(name): Path<String>,
    Query(query): Query<SchemaQuery>,
) -> Result<Json<Vec<barca_service::status::NodeStatus>>, ApiError> {
    let dag = reads.inspect_dag().await?;
    let summaries = queries::list_assets_from_dag(&dag);
    let matches = crate::resolve::matching(&summaries, |s| s.id.as_str(), &name);
    let summary = match matches.len() {
        0 => return Err(ApiError::NotFound(format!("asset '{name}' not found"))),
        1 => matches[0],
        _ => return Err(ApiError::Conflict(format!("'{name}' is ambiguous"))),
    };
    let id = summary.id.clone();
    let inputs = &summary.inputs;
    let db_state = reads.db_state();
    let snapshot = reads.snapshot_db().await?;
    let mut cfg = reads.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let mut nodes = reads
        .whole_status(&dag, &db_state, &snapshot)
        .await?
        .nodes
        .clone();
    nodes.retain(|n| n.id == id || inputs.contains(&n.id));
    barca_service::status::read_schemas(
        &reads.config.python,
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
    state.reads.loaded_dag().await?;
    let dag = state.reads.loaded_dag().await?;
    let summaries = queries::list_assets_from_dag(&dag);

    let matches = crate::resolve::matching(&summaries, |s| s.id.as_str(), &name);

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

    let db_state = state.reads.db_state();
    let snap = state.reads.snapshot_db().await?;
    let stats = db::get_asset_stats(&snap.path, &summary.id).await?;
    let root = std::env::current_dir().unwrap_or_default();
    let failure = db::failure_facts(&snap.path, &summary.id, &root).await?;

    let next_fire = next_fires(&dag).remove(&summary.id);
    let running = running_node(&state, &db_state, &snap, &summary.id).await?;
    Ok(Json(json!({
        "asset": barca_service::report::list_node_json(&summary, next_fire.as_ref()),
        "stats": barca_service::report::stats_json(&stats),
        "running": running,
        "failure": failure,
    })))
}

/// The run working on node `id` now ([`crate::running`]), read from `snapshot` of the database
/// in state `db_state`. The cache state it needs is computed only while some run is in flight.
async fn running_node(
    state: &AppState,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
    id: &str,
) -> Result<Option<crate::views::NodeRunning>, ApiError> {
    let runs = crate::running::in_flight(state, Some(&snapshot.path)).await;
    if runs.is_empty() {
        return Ok(None);
    }
    let dag = state.reads.inspect_dag().await?;
    let status = state.reads.whole_status(&dag, db_state, snapshot).await?;
    Ok(crate::running::by_node(&runs, &status.nodes, &dag).remove(id))
}

/// Organizational metadata; never part of the execution graph.
pub async fn groups(
    State(reads): State<Reads>,
) -> Result<Json<Vec<barca_service::groups::NodeGroup>>, ApiError> {
    let dag = reads.loaded_dag().await?;
    Ok(Json(dag.groups.clone()))
}
