//! The ledger routes (RFC-0007 section 3.4 "The ledger"): what a `barca get` on another
//! machine asks before it runs (`POST /ledger/lookup`) and posts after (`POST /ledger/runs`).
//! The server is the only writer of its database; a post is verified against the store before
//! anything from it is recorded. The logic is `barca_core::ledger`'s; this is its HTTP face.

use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::runs::refuse_if_read_only;
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use barca_core::ledger::{self, HelperStat, LedgerPost, LookupRequest, Rejection};

/// `POST /ledger/lookup` — for each `{node_id, run_hash}`, the latest successful result the
/// ledger holds (`exists`, `path`, `format`, `size_bytes`, `digest`) or `exists: false`.
pub async fn lookup(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    match lookup_inner(state, &body).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

async fn lookup_inner(state: AppState, body: &[u8]) -> Result<Response, ApiError> {
    let request: LookupRequest = serde_json::from_slice(body).map_err(|e| {
        ApiError::BadRequest(format!(
            "the request body is not a ledger lookup: {e}\nSend JSON such as \
             {{\"hashes\": [{{\"node_id\": \"pipeline.py:report\", \"run_hash\": \"…\"}}]}}."
        ))
    })?;
    let response = if state.config.read_only {
        // A read-only server never opens the database itself.
        let snapshot = snapshot_db(&state).await?;
        let mut cfg = state.config.resolved.clone();
        cfg.db_path = snapshot.path.clone();
        ledger::lookup(&cfg, &request).await?
    } else {
        ledger::lookup(&state.config.resolved, &request).await?
    };
    Ok(Json(response).into_response())
}

/// `POST /ledger/runs` — record a run another machine computed: `201 {"run_id",
/// "bytes_changed"}` once every object it names is in the store with its digest, or `409` with
/// an envelope naming the node, and nothing recorded.
pub async fn post_run(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    match post_inner(state, &body).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

async fn post_inner(state: AppState, body: &[u8]) -> Result<Response, ApiError> {
    // A draining server records no new runs (#190): the poster keeps the run and sends it
    // again with its next command, to the server that replaces this one.
    if state.is_draining() || state.shutdown.is_cancelled() {
        return Err(ApiError::Unavailable {
            message: "this server is shutting down and records no new runs\nThe run stays on \
                      the machine that posted it, and its next command against the server sends \
                      it again."
                .to_string(),
            retry_after: state.retry_after_secs(),
        });
    }
    refuse_if_read_only(&state)?;
    let post: LedgerPost = serde_json::from_slice(body).map_err(|e| {
        ApiError::BadRequest(format!(
            "the request body is not a ledger post: {e}\nSend {{\"run\": {{...}}, \"steps\": \
             [...]}} as `barca get` does after a run against this server."
        ))
    })?;
    let cfg = &state.config.resolved;
    let store = HelperStat {
        python: &state.config.python,
        cfg,
    };
    match ledger::accept(cfg, &store, &post).await {
        Ok(accepted) => Ok((StatusCode::CREATED, Json(accepted)).into_response()),
        Err(Rejection::Conflict(message)) => Err(ApiError::Conflict(message)),
        Err(Rejection::Usage(message)) => Err(ApiError::BadRequest(message)),
        Err(Rejection::Infra(e)) => Err(ApiError::Barca(e)),
    }
}
