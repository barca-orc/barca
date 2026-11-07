//! Pinning an asset to an earlier materialization (`barca docs pins`): `GET /pins`,
//! `GET /assets/{name}/versions`, `POST /assets/{name}/pin` and `DELETE /assets/{name}/pin`.
//! A pin is recorded in the server's metadata database, so its runs (scheduled or requested)
//! read the pinned materialization and never compute the asset until it is unpinned.

use crate::auth_http::Principal;
use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use barca_core::pins::{Choice, Pinned, Unpinned, Versions};
use serde::Deserialize;
use serde_json::{Value, json};

/// `GET /pins` — every pinned asset: `{pins: [...]}`.
pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let snapshot = snapshot_db(&state).await?;
    let pins = barca_core::pins::list(&snapshot.path).await?;
    Ok(Json(json!({ "pins": pins })))
}

#[derive(Default, Deserialize)]
pub struct VersionsQuery {
    pub limit: Option<usize>,
}

/// `GET /assets/{name}/versions?limit=N` — the asset's recent successful materializations (one
/// per run hash, newest first) and its pin, as `barca pin <name> --versions --json`.
pub async fn versions(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<VersionsQuery>,
) -> Result<Json<Versions>, ApiError> {
    let dag = state.loaded_dag().await?;
    let snapshot = snapshot_db(&state).await?;
    let limit = query
        .limit
        .unwrap_or(barca_core::pins::DEFAULT_VERSIONS)
        .clamp(1, 500);
    Ok(Json(
        barca_core::pins::versions(&snapshot.path, &dag, &name, limit).await?,
    ))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinBody {
    /// The run hash (or a unique prefix) to pin.
    pub run_hash: Option<String>,
    /// Pin the version before the latest one.
    #[serde(default)]
    pub previous: bool,
    pub reason: Option<String>,
}

/// `POST /assets/{name}/pin` — pin the asset to `run_hash` or the `previous` version, as
/// `barca pin`. `pinned_by` is the request's project token (`token:<name>`), if any.
pub async fn pin(
    State(state): State<AppState>,
    Path(name): Path<String>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<PinBody>,
) -> Result<Json<Pinned>, ApiError> {
    crate::runs::refuse_if_read_only(&state)?;
    let choice = match (body.run_hash, body.previous) {
        (Some(_), true) => {
            return Err(ApiError::BadRequest(
                "give run_hash or previous, not both".into(),
            ));
        }
        (Some(hash), false) => Choice::RunHash(hash),
        (None, true) => Choice::Previous,
        (None, false) => {
            return Err(ApiError::BadRequest(format!(
                "say which materialization to pin: {{\"run_hash\": \"...\"}} or {{\"previous\": true}}\nGET /assets/{name}/versions lists them."
            )));
        }
    };
    let dag = state.loaded_dag().await?;
    let cfg = &state.config.resolved;
    barca_core::db::ensure_env_dirs(&cfg.env)?;
    barca_core::db::init_db(&cfg.db_path).await?;
    let pinned = barca_core::pins::pin(
        &cfg.db_path,
        &dag,
        &name,
        &choice,
        body.reason,
        principal.map(|Extension(p)| p.0),
    )
    .await?;
    barca_core::errln!(
        "[barca] pinned {} at {}",
        pinned.node_id,
        pinned.pin.run_hash
    );
    Ok(Json(pinned))
}

/// `DELETE /assets/{name}/pin` — remove the asset's pin, as `barca unpin`.
pub async fn unpin(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Unpinned>, ApiError> {
    crate::runs::refuse_if_read_only(&state)?;
    let dag = state.loaded_dag().await?;
    let cfg = &state.config.resolved;
    let unpinned = barca_core::pins::unpin(&cfg.db_path, &dag, &name).await?;
    if unpinned.removed.is_some() {
        barca_core::errln!("[barca] unpinned {}", unpinned.node_id);
    }
    Ok(Json(unpinned))
}
