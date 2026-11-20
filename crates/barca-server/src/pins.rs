//! Pinning an asset to an earlier materialization (`barca docs pins`): `GET /pins`,
//! `GET /assets/{name}/versions`, `POST /assets/{name}/pin` and `DELETE /assets/{name}/pin`.
//! A pin is recorded in the server's metadata database, so its runs (scheduled or requested)
//! read the pinned materialization and never compute the asset until it is unpinned.

use crate::auth_http::Principal;
use crate::error::ApiError;
use crate::read_cache::Reads;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use barca_service::pins::{Choice, Pinned, Unpinned, Versions};
use barca_service::served::Pins;
use serde::Deserialize;

/// `GET /pins` — every pinned asset: `{pins: [...]}`.
pub async fn list(State(reads): State<Reads>) -> Result<Json<Pins>, ApiError> {
    let snapshot = reads.snapshot_db().await?;
    let pins = barca_service::pins::list(&snapshot.path).await?;
    Ok(Json(Pins { pins }))
}

#[derive(Default, Deserialize)]
pub struct VersionsQuery {
    pub limit: Option<usize>,
}

/// `GET /assets/{name}/versions?limit=N` — the asset's recent successful materializations (one
/// per run hash, newest first) and its pin, as `barca pin <name> --versions --json`.
pub async fn versions(
    State(reads): State<Reads>,
    Path(name): Path<String>,
    Query(query): Query<VersionsQuery>,
) -> Result<Json<Versions>, ApiError> {
    let dag = reads.loaded_dag().await?;
    let snapshot = reads.snapshot_db().await?;
    let limit = query
        .limit
        .unwrap_or(barca_service::pins::DEFAULT_VERSIONS)
        .clamp(1, 500);
    Ok(Json(
        barca_service::pins::versions(&snapshot.path, &dag, &name, limit).await?,
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
    State(reads): State<Reads>,
    Path(name): Path<String>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<PinBody>,
) -> Result<Json<Pinned>, ApiError> {
    crate::guards::refuse_if_read_only(&reads.config)?;
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
    let dag = reads.loaded_dag().await?;
    let cfg = &reads.config.resolved;
    barca_service::queries::open_metadata(cfg).await?;
    let pinned = barca_service::pins::pin(
        &cfg.db_path,
        &dag,
        &name,
        &choice,
        body.reason,
        principal.map(|Extension(p)| p.0),
    )
    .await?;
    barca_service::errln!(
        "[barca] pinned {} at {}",
        pinned.node_id,
        pinned.pin.run_hash
    );
    Ok(Json(pinned))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadQuery {
    /// `json`, `parquet`, `csv` or `pickle`; default: the extension of `file_name`.
    pub format: Option<String>,
    /// The name of the file supplied, recorded with the upload.
    pub file_name: Option<String>,
    pub reason: Option<String>,
}

/// `POST /assets/{name}/pin/upload?format=&file_name=&reason=` — the body is the file's bytes
/// (any content type): checked by loading it, written as a new artifact of the asset, recorded as
/// an uploaded materialization and pinned, as `barca pin --from-file`.
pub async fn upload(
    State(reads): State<Reads>,
    Path(name): Path<String>,
    Query(query): Query<UploadQuery>,
    principal: Option<Extension<Principal>>,
    body: axum::body::Bytes,
) -> Result<Json<Pinned>, ApiError> {
    crate::guards::refuse_if_read_only(&reads.config)?;
    let file_name = query
        .file_name
        .as_deref()
        .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n).to_string())
        .unwrap_or_default();
    if query.format.is_none() && file_name.is_empty() {
        return Err(ApiError::BadRequest(
            "say what the body is: ?format=json|parquet|csv|pickle or ?file_name=<name.ext>".into(),
        ));
    }
    let dag = reads.loaded_dag().await?;
    let cfg = &reads.config.resolved;
    barca_service::queries::open_metadata(cfg).await?;
    let supplied = barca_service::pins::Supplied {
        bytes: body.to_vec(),
        file_name,
        format: query.format,
    };
    let pinned = barca_service::pins::pin_upload(
        cfg,
        &reads.config.python,
        &dag,
        &name,
        supplied,
        query.reason,
        principal.map(|Extension(p)| p.0),
    )
    .await?;
    barca_service::errln!(
        "[barca] pinned {} at {} (uploaded)",
        pinned.node_id,
        pinned.pin.run_hash
    );
    Ok(Json(pinned))
}

/// `DELETE /assets/{name}/pin` — remove the asset's pin, as `barca unpin`.
pub async fn unpin(
    State(reads): State<Reads>,
    Path(name): Path<String>,
) -> Result<Json<Unpinned>, ApiError> {
    crate::guards::refuse_if_read_only(&reads.config)?;
    let dag = reads.loaded_dag().await?;
    let cfg = &reads.config.resolved;
    let unpinned = barca_service::pins::unpin(&cfg.db_path, &dag, &name).await?;
    if unpinned.removed.is_some() {
        barca_service::errln!("[barca] unpinned {}", unpinned.node_id);
    }
    Ok(Json(unpinned))
}
