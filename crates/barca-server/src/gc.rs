//! `POST /gc` — `barca gc` run by the server: evict cached results to bring `.barca/` under its
//! size limit, now. A `barca gc` typed in the server's project is sent here (over the project
//! socket), because the server's own writes hold the metadata database's lock in turn with
//! each other, and a second process waiting for that lock from outside can wait a minute and
//! give up (#1054). Inside the server the eviction takes its turn like any other write.
//!
//! The body is `barca gc`'s flags and the answer is `barca gc --json`'s
//! (`barca_core::gc::GcResult`). What this server's runs in flight hold is left alone result by
//! result (the runs pin it in this process), where a second process beside a run can only
//! hold back every current result (`restricted`).

use crate::error::ApiError;
use crate::guards::refuse_if_read_only;
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use barca_core::gc::{GcOptions, GcResult};
use serde::Deserialize;

/// `barca gc`'s flags. Unknown fields are refused, so a misspelt `dry_run` never evicts.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcRequest {
    /// `--max-size`: a limit for this pass in place of the configured one (`20GB`, `10%`,
    /// `off`).
    #[serde(default)]
    max_size: Option<String>,
    /// `--dry-run`: report what would be evicted and delete nothing.
    #[serde(default)]
    dry_run: bool,
    /// `--env`: evict only this environment's results (leftovers are shared by all). It must
    /// be the environment this server serves; without it every environment of the project
    /// is considered, as `barca gc` does.
    #[serde(default)]
    env: Option<String>,
}

/// `POST /gc`. `400` for a `max_size` that is not a size or an `env` this server does not
/// serve, `403` on a `--read-only` server
/// (it deletes nothing, a dry run included: its answer would describe a cache it does not
/// manage).
pub async fn gc(
    State(state): State<AppState>,
    body: Option<Json<GcRequest>>,
) -> Result<Json<GcResult>, ApiError> {
    refuse_if_read_only(&state.config)?;
    let Json(request) = body.unwrap_or_default();
    let max_size = request
        .max_size
        .as_deref()
        .map(barca_core::gc::size::parse)
        .transpose()
        .map_err(|e| ApiError::BadRequest(format!("max_size: {e}")))?;
    let cfg = &state.config.resolved;
    if let Some(env) = request.env.as_deref().filter(|env| *env != cfg.env) {
        return Err(ApiError::BadRequest(format!(
            "env: this server serves environment '{}', not '{env}'\nLeave out env, or name \
             the server's own.",
            cfg.env
        )));
    }
    let opts = GcOptions {
        max_size,
        dry_run: request.dry_run,
        env: request.env,
    };
    let result = barca_core::gc::gc(cfg, &opts).await?;
    if !result.dry_run {
        barca_core::errln!(
            "[barca] gc on request: evicted {} file{} ({} bytes)",
            result.evicted_files,
            if result.evicted_files == 1 { "" } else { "s" },
            result.evicted_bytes
        );
    }
    Ok(Json(result))
}
