//! Pausing assets (`GET /pauses`, `POST /pauses`, `POST /pauses/resume`): an operator holds
//! back a node, everything below it, or every node of a file, and scheduled ticks skip them
//! (reason `paused`) until they are resumed (`barca_core::pauses`). Pauses are rows of the
//! metadata DB's `asset_pauses`, so a restart (a deploy) does not resume them.

use crate::error::ApiError;
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use barca_core::pauses;

pub use barca_core::served::{PauseRequest, Pauses};

/// Every paused node, and every paused schedule (`POST /schedule/{id}/pause`).
async fn body(state: &AppState) -> Result<Pauses, ApiError> {
    let db_path = &state.config.resolved.db_path;
    let paused = pauses::list(db_path).await?;
    let schedules = pauses::paused_schedules(db_path, chrono::Utc::now().timestamp()).await?;
    Ok(Pauses::of(paused, schedules))
}

/// `GET /pauses` — every paused node, with when and through what it was paused, and every
/// paused schedule.
pub async fn list(State(state): State<AppState>) -> Result<Json<Pauses>, ApiError> {
    Ok(Json(body(&state).await?))
}

async fn scope(state: &AppState, req: &PauseRequest) -> Result<Vec<String>, ApiError> {
    let Some(target) = req.target.as_deref().filter(|t| !t.is_empty()) else {
        return Err(ApiError::BadRequest(
            "a target is required\nSend {\"target\": \"<node or file.py>\"}.".into(),
        ));
    };
    let dag = state.reads.loaded_dag().await?;
    Ok(pauses::scope(&dag, target, req.downstream)?)
}

/// `POST /pauses` — pause `target` (and with `downstream`, everything below it). Answers the
/// ids it paused (`added`) and every paused node.
pub async fn pause(
    State(state): State<AppState>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Pauses>, ApiError> {
    crate::guards::refuse_if_read_only(&state.config)?;
    if req.all {
        return Err(ApiError::BadRequest(
            "`all` is for resuming\nPause a target, or a file to pause every node of it.".into(),
        ));
    }
    let ids = scope(&state, &req).await?;
    let cfg = &state.config.resolved;
    let via = req.target.clone().unwrap_or_default();
    pauses::pause(cfg, &ids, &via, chrono::Utc::now().timestamp()).await?;
    barca_core::errln!("[barca] paused {} node(s) via {via}", ids.len());
    Ok(Json(Pauses {
        added: Some(ids),
        ..body(&state).await?
    }))
}

/// `POST /pauses/resume` — resume `target` (with `downstream`, everything below it too), or
/// with `all`, every paused node. Answers the ids it resumed (`removed`).
pub async fn resume(
    State(state): State<AppState>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Pauses>, ApiError> {
    crate::guards::refuse_if_read_only(&state.config)?;
    let db_path = &state.config.resolved.db_path;
    let removed = if req.all {
        pauses::resume(db_path, None).await?
    } else {
        pauses::resume(db_path, Some(scope(&state, &req).await?)).await?
    };
    barca_core::errln!("[barca] resumed {} node(s)", removed.len());
    Ok(Json(Pauses {
        removed: Some(removed),
        ..body(&state).await?
    }))
}
