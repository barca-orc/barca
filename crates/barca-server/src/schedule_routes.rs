//! `GET /schedule`, and pausing one schedule (`POST /schedule/{id}/pause`,
//! `POST /schedule/{id}/resume`). A paused job's due ticks are skipped with reason `paused`
//! until it is resumed or its `until` time passes. Pauses are kept in `schedule_state`, so a
//! restart (a deploy) does not resume a job someone paused.

use crate::error::ApiError;
use crate::read_cache::Reads;
use crate::schedule_view::{pauses, published_ticks, schedule_at, schedule_items};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use barca_service::db::{self, SchedulePause};
use barca_service::served::{ScheduleJob, SchedulePauseAnswer};
use serde::Deserialize;

/// `GET /schedule` — list scheduled jobs with next fire time and last run status.
/// Reads the scheduler's published registry; volatile fields (next fire, live
/// status) are computed per request.
pub async fn schedule(State(state): State<AppState>) -> Result<Json<Vec<ScheduleJob>>, ApiError> {
    crate::schedule_view::pauses(&state).await?;
    if state.config.read_only || !state.config.schedule {
        let dag = state.reads.loaded_dag().await?;
        let jobs = barca_service::schedule::jobs_from_dag(&dag);
        let snap = state.reads.snapshot_db().await?;
        let facts = state
            .reads
            .cached_ticks(
                &state.reads.db_state(),
                &snap.path,
                jobs.iter().map(|j| j.id.clone()).collect(),
            )
            .await?;
        // No scheduler here: failed ticks are the recorded ones, in the recorded zone.
        return Ok(Json(schedule_items(
            &state,
            chrono::Utc::now(),
            &facts,
            &[],
            jobs,
            barca_service::liveness::recorded(&snap.path)
                .await?
                .zone()
                .unwrap_or_else(|| crate::config::zone_of(&state.config)),
        )));
    }
    let facts = published_ticks(&state).await?;
    Ok(Json(schedule_at(&state, chrono::Utc::now(), &facts)))
}

#[derive(Default, Deserialize)]
pub struct PauseQuery {
    /// A duration from now (`2h`) or an RFC 3339 time; absent: until resumed.
    pub until: Option<String>,
}

/// `POST /schedule/{id}/pause` — hold one scheduled job back, until `?until=` (a duration from
/// now such as `2h`, or an RFC 3339 time) or until it is resumed. Answers the job's `GET /schedule` entry.
pub async fn pause(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PauseQuery>,
) -> Result<Json<SchedulePauseAnswer>, ApiError> {
    let now = chrono::Utc::now().timestamp();
    let until = query
        .until
        .as_deref()
        .map(|text| barca_service::pauses::parse_until(text, chrono::Utc::now()))
        .transpose()
        .map_err(ApiError::BadRequest)?;
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
) -> Result<Json<SchedulePauseAnswer>, ApiError> {
    let now = chrono::Utc::now().timestamp();
    set(&state, &id, None, now).await
}

async fn set(
    state: &AppState,
    id: &str,
    pause: Option<SchedulePause>,
    now: i64,
) -> Result<Json<SchedulePauseAnswer>, ApiError> {
    crate::guards::refuse_if_read_only(&state.config)?;
    let node = scheduled_node(&state.reads, id).await?;
    let cfg = &state.config.resolved;
    barca_service::queries::open_metadata(cfg).await?;
    db::set_schedule_pause(&cfg.db_path, &node, pause, now).await?;
    {
        let mut pauses = pauses(state).await?.write().unwrap();
        match pause {
            Some(pause) => pauses.insert(node.clone(), pause),
            None => pauses.remove(&node),
        };
    }
    barca_service::errln!(
        "[barca] schedule {node} {}",
        match pause.and_then(|p| p.until) {
            Some(until) => format!("paused until {}", barca_service::outcome::rfc3339(until)),
            None if pause.is_some() => "paused until resumed".to_string(),
            None => "resumed".to_string(),
        }
    );
    let facts = crate::schedule_view::published_ticks(state).await?;
    let entry = schedule_at(state, chrono::Utc::now(), &facts)
        .into_iter()
        .find(|job| job.id == node);
    // The scheduler has not published this job (it is off, or still starting): answer what
    // the pause is.
    Ok(Json(match entry {
        Some(job) => SchedulePauseAnswer::Job(Box::new(job)),
        None => SchedulePauseAnswer::Pause {
            id: node,
            paused: pause.is_some(),
            paused_until: pause.and_then(|p| p.until),
        },
    }))
}

/// The scheduled node `id` names: its full id or its name, as for a run target. Anything that
/// is not a scheduled node is `404`.
async fn scheduled_node(reads: &Reads, id: &str) -> Result<String, ApiError> {
    let dag = reads.loaded_dag().await?;
    let jobs = barca_service::schedule::jobs_from_summaries(
        barca_service::queries::list_assets_from_dag(&dag),
    );
    let matches: Vec<&str> = crate::resolve::matching(&jobs, |j| j.id.as_str(), id)
        .into_iter()
        .map(|j| j.id.as_str())
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
