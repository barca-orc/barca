//! `POST /state/reset` — start the metadata database over from the UI (#508), as
//! `barca state reset` does on the command line: the database and its `-wal` and `-shm` move
//! aside to `<db>.reset-<UTC>` and an empty one takes its place. Nothing is deleted.
//!
//! The server owns the database: every handler opens a short-lived connection under
//! barca-core's database lock, and no connection is kept between requests, so the swap is
//! made under that lock and the next request opens the new file. What the server keeps from
//! the database in memory (the paused schedules, the finished runs it still lists) is cleared
//! with it.

use crate::error::ApiError;
use crate::runs::{refuse_if_draining, refuse_if_read_only};
use crate::state::{AppState, RunStatus};
use axum::Json;
use axum::extract::State;
use barca_core::backup::Reset;

/// `POST /state/reset`. `403` on a `--read-only` server, `503` while it drains, `409` while a
/// run is pending or running (this server's, or a local run's on this machine).
pub async fn reset(State(state): State<AppState>) -> Result<Json<Reset>, ApiError> {
    refuse_if_read_only(&state)?;
    refuse_if_draining(&state)?;
    let cfg = &state.config.resolved;
    let reset = barca_core::backup::reset(cfg, |running| {
        // Checked under the database's lock: a run admitted after this check writes nothing
        // until the swap is done, and then writes to the new database.
        let mut in_flight: Vec<String> = state
            .runs
            .iter()
            .filter(|r| matches!(r.status, RunStatus::Pending | RunStatus::Running))
            .map(|r| r.handle.clone())
            .collect();
        for id in running {
            if !in_flight.contains(id) {
                in_flight.push(id.clone());
            }
        }
        if in_flight.is_empty() {
            return Ok(());
        }
        in_flight.sort();
        Err(ApiError::Conflict(format!(
            "cannot reset the metadata database while a run is in flight ({}). Nothing was \
             changed.\nWait for it to finish, or cancel it (DELETE /runs/{{run_id}}).",
            in_flight.join(", ")
        )))
    })
    .await?;
    if let Some(pauses) = state.pauses.get() {
        pauses.write().unwrap().clear();
    }
    state.queue.reset();
    crate::queue::dispatch(&state);
    // The finished runs this server still remembers go with the history they were part of
    // (`GET /runs` joins them in). One admitted after the check above stays.
    state
        .runs
        .retain(|_, r| matches!(r.status, RunStatus::Pending | RunStatus::Running));
    state.events.retain(|id, _| state.runs.contains_key(id));
    barca_core::errln!(
        "[barca] reset the metadata database{}",
        match &reset.moved_to {
            Some(moved) => format!(": the old one is {moved}"),
            None => String::new(),
        }
    );
    Ok(Json(reset))
}
