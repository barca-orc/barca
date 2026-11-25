//! Pausing assets (`GET /pauses`, `POST /pauses`, `POST /pauses/resume`): an operator holds
//! back a node, everything below it, or every node of a file, and scheduled ticks skip them
//! (reason `paused`) until they are resumed (`barca_service::pauses`). Pauses are rows of the
//! metadata DB's `asset_pauses`, so a restart (a deploy) does not resume them.

use crate::error::ApiError;
use crate::read_cache::Reads;
use axum::Json;
use axum::extract::{Query, State};
use barca_service::pauses;

pub use barca_service::served::{PauseRequest, Pauses};

/// Every paused node, and every paused schedule (`POST /schedule/{id}/pause`).
async fn body(reads: &Reads) -> Result<Pauses, ApiError> {
    let db_path = &reads.config.resolved.db_path;
    let paused = pauses::list(db_path).await?;
    let schedules = pauses::paused_schedules(db_path, chrono::Utc::now().timestamp()).await?;
    Ok(Pauses::of(paused, schedules))
}

/// `GET /pauses?target=&downstream=`: the pause to describe, not to make.
#[derive(Debug, Default, serde::Deserialize)]
pub struct CoverQuery {
    /// A node or a file, as `POST /pauses` takes it.
    target: Option<String>,
    /// What is below the target too.
    #[serde(default)]
    downstream: bool,
}

/// `GET /pauses` — every paused node, with when and through what it was paused, and every
/// paused schedule. With `?target=` (and `&downstream=true`), also `covers`: what a pause of
/// that target would cover (`barca_service::refresh_path::pause_cover`). It pauses nothing, so
/// a read-only server answers it.
pub async fn list(
    State(reads): State<Reads>,
    Query(query): Query<CoverQuery>,
) -> Result<Json<Pauses>, ApiError> {
    let mut pauses = body(&reads).await?;
    if let Some(target) = query.target.as_deref().filter(|t| !t.is_empty()) {
        let dag = reads.loaded_dag().await?;
        pauses.covers = Some(barca_service::refresh_path::pause_cover(
            &dag,
            target,
            query.downstream,
        )?);
    }
    Ok(Json(pauses))
}

/// The target a request names, refused when it names none.
fn target(req: &PauseRequest) -> Result<&str, ApiError> {
    req.target
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            ApiError::BadRequest(
                "a target is required\nSend {\"target\": \"<node or file.py>\"}.".into(),
            )
        })
}

/// `POST /pauses` — pause `target` (and with `downstream`, everything below it). Answers the
/// ids it paused (`added`) and every paused node.
pub async fn pause(
    State(reads): State<Reads>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Pauses>, ApiError> {
    crate::guards::refuse_if_read_only(&reads.config)?;
    if req.all {
        return Err(ApiError::BadRequest(
            "`all` is for resuming\nPause a target, or a file to pause every node of it.".into(),
        ));
    }
    let via = target(&req)?;
    let dag = reads.loaded_dag().await?;
    let cfg = &reads.config.resolved;
    let ids = pauses::pause(
        cfg,
        &dag,
        via,
        req.downstream,
        chrono::Utc::now().timestamp(),
    )
    .await?;
    barca_service::errln!("[barca] paused {} node(s) via {via}", ids.len());
    Ok(Json(Pauses {
        added: Some(ids),
        ..body(&reads).await?
    }))
}

/// `POST /pauses/resume` — resume `target` (with `downstream`, the nodes below it that its
/// pause holds too; a node another pause holds stays paused), or with `all`, every paused
/// node. Answers the ids it resumed (`removed`).
pub async fn resume(
    State(reads): State<Reads>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Pauses>, ApiError> {
    crate::guards::refuse_if_read_only(&reads.config)?;
    let db_path = &reads.config.resolved.db_path;
    let removed = if req.all {
        pauses::resume_all(db_path).await?
    } else {
        let target = target(&req)?;
        let dag = reads.loaded_dag().await?;
        pauses::resume_target(db_path, &dag, target, req.downstream).await?
    };
    barca_service::errln!("[barca] resumed {} node(s)", removed.len());
    Ok(Json(Pauses {
        removed: Some(removed),
        ..body(&reads).await?
    }))
}
