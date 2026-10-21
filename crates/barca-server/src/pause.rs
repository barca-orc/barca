//! Pausing one schedule (`POST /schedule/{id}/pause`, `POST /schedule/{id}/resume`). A paused
//! job's due ticks are skipped with reason `paused` until it is resumed or its `until` time
//! passes. Pauses are kept in `schedule_state`, so a restart (a deploy) does not resume a job
//! someone paused.

use crate::error::ApiError;
use crate::handlers::{schedule_at, snapshot_db};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use barca_core::db::{self, SchedulePause};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;

/// The paused schedules, read from `schedule_state` on first use.
pub(crate) async fn pauses(
    state: &AppState,
) -> Result<&RwLock<HashMap<String, SchedulePause>>, ApiError> {
    state
        .pauses
        .get_or_try_init(|| async {
            let snapshot = snapshot_db(state).await?;
            let saved = db::get_schedule_pauses(&snapshot.path).await?;
            Ok::<_, ApiError>(RwLock::new(saved))
        })
        .await
}

/// `node_id`'s pause, if it is paused at `now` (unix seconds). A pause whose `until` has
/// passed is over.
pub(crate) fn active(state: &AppState, node_id: &str, now: i64) -> Option<SchedulePause> {
    let pauses = state.pauses.get()?.read().ok()?;
    pauses
        .get(node_id)
        .copied()
        .filter(|p| p.until.is_none_or(|until| until > now))
}

#[derive(Default, Deserialize)]
pub struct PauseQuery {
    /// RFC 3339; absent: until resumed.
    pub until: Option<String>,
}

/// `POST /schedule/{id}/pause` — hold one scheduled job back, until `?until=<RFC 3339>` or
/// until it is resumed. Answers the job's `GET /schedule` entry.
pub async fn pause(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PauseQuery>,
) -> Result<Json<Value>, ApiError> {
    let now = chrono::Utc::now().timestamp();
    let until = match &query.until {
        None => None,
        Some(text) => {
            let at = chrono::DateTime::parse_from_rfc3339(text).map_err(|e| {
                ApiError::BadRequest(format!(
                    "until '{text}' is not an RFC 3339 time: {e}\nUse a time such as 2026-10-21T06:00:00Z."
                ))
            })?;
            if at.timestamp() <= now {
                return Err(ApiError::BadRequest(format!(
                    "until '{text}' is not in the future\nGive a later time, or leave `until` out to pause until resumed."
                )));
            }
            Some(at.timestamp())
        }
    };
    set(
        &state,
        &id,
        Some(SchedulePause {
            paused_at: now,
            until,
        }),
        now,
    )
    .await
}

/// `POST /schedule/{id}/resume` — let a paused job fire again from its next tick. The ticks it
/// skipped while paused are not caught up. Resuming a job that is not paused changes nothing.
pub async fn resume(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let now = chrono::Utc::now().timestamp();
    set(&state, &id, None, now).await
}

async fn set(
    state: &AppState,
    id: &str,
    pause: Option<SchedulePause>,
    now: i64,
) -> Result<Json<Value>, ApiError> {
    crate::runs::refuse_if_read_only(state)?;
    let node = scheduled_node(state, id).await?;
    let cfg = &state.config.resolved;
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::set_schedule_pause(&cfg.db_path, &node, pause, now).await?;
    {
        let mut pauses = pauses(state).await?.write().unwrap();
        match pause {
            Some(pause) => pauses.insert(node.clone(), pause),
            None => pauses.remove(&node),
        };
    }
    barca_core::errln!(
        "[barca] schedule {node} {}",
        match pause.and_then(|p| p.until) {
            Some(until) => format!("paused until {}", barca_core::outcome::rfc3339(until)),
            None if pause.is_some() => "paused until resumed".to_string(),
            None => "resumed".to_string(),
        }
    );
    let body = schedule_at(state, chrono::Utc::now());
    let entry = body
        .as_array()
        .into_iter()
        .flatten()
        .find(|job| job["id"] == node.as_str())
        .cloned();
    // The scheduler has not published this job (it is off, or still starting): answer what
    // the pause is.
    Ok(Json(entry.unwrap_or_else(|| {
        serde_json::json!({
            "id": node,
            "paused": pause.is_some(),
            "paused_until": pause.and_then(|p| p.until),
        })
    })))
}

/// The scheduled node `id` names: its full id or its name, as for a run target. Anything that
/// is not a scheduled node is `404`.
async fn scheduled_node(state: &AppState, id: &str) -> Result<String, ApiError> {
    let dag = state.loaded_dag().await?;
    let jobs =
        barca_core::schedule::jobs_from_summaries(barca_core::queries::list_assets_from_dag(&dag));
    let matches: Vec<&str> = jobs
        .iter()
        .map(|j| j.id.as_str())
        .filter(|job| *job == id || job.ends_with(&format!(":{id}")))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Err(ApiError::NotFound(format!(
            "'{id}' is not a scheduled node\nGET /schedule lists the scheduled nodes."
        ))),
        many => Err(ApiError::Conflict(format!(
            "'{id}' matches {} scheduled nodes: {}\nUse the full id.",
            many.len(),
            many.join(", ")
        ))),
    }
}
