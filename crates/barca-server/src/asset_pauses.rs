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

/// The request's target: a pause or a resume of nodes needs one.
fn target_of(req: &PauseRequest) -> Result<&str, ApiError> {
    req.target
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            ApiError::BadRequest(
                "a target is required\nSend {\"target\": \"<node or file.py>\"}.".into(),
            )
        })
}

async fn scope(reads: &Reads, req: &PauseRequest) -> Result<Vec<String>, ApiError> {
    let target = target_of(req)?;
    let dag = reads.loaded_dag().await?;
    Ok(pauses::scope(&dag, target, req.downstream)?)
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
    let ids = scope(&reads, &req).await?;
    let cfg = &reads.config.resolved;
    let via = req.target.clone().unwrap_or_default();
    pauses::pause(cfg, &ids, &via, chrono::Utc::now().timestamp()).await?;
    barca_service::errln!("[barca] paused {} node(s) via {via}", ids.len());
    Ok(Json(Pauses {
        added: Some(ids),
        ..body(&reads).await?
    }))
}

/// `POST /pauses/resume` — resume `target` (with `downstream`, everything below it too). The
/// nodes `target` names lose every pause; a node below them loses only the pause that named
/// `target`, and stays paused (`kept`) while another pause holds it. With `all`, every paused node, whatever holds it. Answers the ids it
/// resumed (`removed`).
pub async fn resume(
    State(reads): State<Reads>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Pauses>, ApiError> {
    crate::guards::refuse_if_read_only(&reads.config)?;
    let db_path = &reads.config.resolved.db_path;
    let (removed, kept) = if req.all {
        (pauses::resume(db_path, None).await?, None)
    } else {
        let target = target_of(&req)?;
        let dag = reads.loaded_dag().await?;
        let resumed = pauses::resume_target(db_path, &dag, target, req.downstream).await?;
        (resumed.removed, Some(resumed.kept))
    };
    match kept.as_ref().filter(|k| !k.is_empty()) {
        Some(kept) => barca_service::errln!(
            "[barca] resumed {} node(s); {} stay paused by another pause",
            removed.len(),
            kept.len()
        ),
        None => barca_service::errln!("[barca] resumed {} node(s)", removed.len()),
    }
    Ok(Json(Pauses {
        removed: Some(removed),
        kept,
        ..body(&reads).await?
    }))
}
