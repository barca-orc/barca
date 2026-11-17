//! Pausing assets (`GET /pauses`, `POST /pauses`, `POST /pauses/resume`): an operator holds
//! back a node, everything below it, or every node of a file, and scheduled ticks skip them
//! (reason `paused`) until they are resumed (`barca_core::pauses`). Pauses are rows of the
//! metadata DB's `asset_pauses`, so a restart (a deploy) does not resume them.

use crate::error::ApiError;
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use barca_core::pauses;
use serde::Deserialize;
use serde_json::{Value, json};

/// The body of `POST /pauses` and `POST /pauses/resume`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    /// A node, named as a run target is, or a source file (`pipeline.py`): every node of it.
    #[serde(default)]
    pub target: Option<String>,
    /// Everything downstream of the target too.
    #[serde(default)]
    pub downstream: bool,
    /// Resume only: every paused node.
    #[serde(default)]
    pub all: bool,
}

/// `{paused: [{id, paused_at, via}], total, schedules: [{id, paused_at, until}]}`: every
/// paused node, and every paused schedule (`POST /schedule/{id}/pause`).
async fn body(
    state: &AppState,
    changed: Option<(&str, Vec<String>)>,
) -> Result<Json<Value>, ApiError> {
    let db_path = &state.config.resolved.db_path;
    let paused = pauses::list(db_path).await?;
    let schedules = pauses::paused_schedules(db_path, chrono::Utc::now().timestamp()).await?;
    let mut body = json!({
        "total": paused.len(),
        "paused": paused,
        "schedules": schedules,
    });
    if let Some((key, ids)) = changed {
        body[key] = json!(ids);
    }
    Ok(Json(body))
}

/// `GET /pauses` — every paused node, with when and through what it was paused, and every
/// paused schedule.
pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    body(&state, None).await
}

async fn scope(state: &AppState, req: &PauseRequest) -> Result<Vec<String>, ApiError> {
    let Some(target) = req.target.as_deref().filter(|t| !t.is_empty()) else {
        return Err(ApiError::BadRequest(
            "a target is required\nSend {\"target\": \"<node or file.py>\"}.".into(),
        ));
    };
    let dag = state.loaded_dag().await?;
    Ok(pauses::scope(&dag, target, req.downstream)?)
}

/// `POST /pauses` — pause `target` (and with `downstream`, everything below it). Answers the
/// ids it paused (`added`) and every paused node.
pub async fn pause(
    State(state): State<AppState>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Value>, ApiError> {
    crate::runs::refuse_if_read_only(&state)?;
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
    body(&state, Some(("added", ids))).await
}

/// `POST /pauses/resume` — resume `target` (with `downstream`, everything below it too), or
/// with `all`, every paused node. Answers the ids it resumed (`removed`).
pub async fn resume(
    State(state): State<AppState>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<Value>, ApiError> {
    crate::runs::refuse_if_read_only(&state)?;
    let db_path = &state.config.resolved.db_path;
    let removed = if req.all {
        pauses::resume(db_path, None).await?
    } else {
        pauses::resume(db_path, Some(scope(&state, &req).await?)).await?
    };
    barca_core::errln!("[barca] resumed {} node(s)", removed.len());
    body(&state, Some(("removed", removed))).await
}
