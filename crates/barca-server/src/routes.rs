//! Router wiring. The router is the single API boundary — middleware (CORS,
//! auth) would layer in here. The web UI is served under `/ui/`, beside the API
//! routes rather than in front of them, so API paths stay what the contract says.

use crate::handlers;
use crate::state::AppState;
use crate::ui;
use axum::Router;
use axum::routing::{get, post};

/// Build the v1 API router over the given state.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/plan", get(handlers::plan))
        .route("/groups", get(handlers::groups))
        .route("/assets", get(handlers::assets))
        .route("/assets/{name}", get(handlers::asset_detail))
        .route("/assets/{name}/schema", get(handlers::asset_schema))
        .route("/run", post(handlers::run))
        .route("/runs", get(crate::runs::list))
        .route("/runs/{id}", get(crate::runs::detail))
        // POST starts a task run; DELETE cancels an in-flight run by handle.
        // The target takes the rest of the path, so a full node id with a directory in it
        // (`sub/pipeline.py:orders`) works with its `/` percent-encoded or not.
        .route(
            "/run/{*target}",
            post(handlers::run_target).delete(handlers::cancel_run),
        )
        .route("/get/{*target}", post(handlers::get_target))
        .route("/status/{run_id}", get(handlers::status))
        .route("/state", get(handlers::state))
        .route("/events/{run_id}", get(handlers::events))
        .route("/logs/{run_id}", get(handlers::logs))
        .route("/schedule", get(handlers::schedule))
        // The web UI: `/` and `/ui` redirect to `ui/` (relative, so a reverse
        // proxy prefix is kept); the app and its assets live under `/ui/`.
        .route("/", get(ui::redirect_to_ui))
        .route("/ui", get(ui::redirect_to_ui))
        .route("/ui/", get(ui::index))
        .route("/ui/{*path}", get(ui::asset))
        // Everything else is a JSON 404, like the errors of the routes above.
        .fallback(handlers::no_route)
        .method_not_allowed_fallback(handlers::wrong_method)
        .with_state(state)
}
