//! Router wiring. The router is the single API boundary — middleware (CORS,
//! auth) would layer in here. The web UI is served under `/ui/`, beside the API
//! routes rather than in front of them, so API paths stay what the contract says.

use crate::state::AppState;
use crate::{handlers, inspect, ledger, pause, reset, runs, ui};
use axum::Router;
use axum::routing::{get, post};

/// Build the v1 API router over the given state.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/project", get(inspect::project))
        .route("/plan", get(handlers::plan))
        .route("/groups", get(handlers::groups))
        // Inspection in the CLI's `--json` shapes.
        .route("/list", get(inspect::list))
        .route("/nodes", get(inspect::nodes))
        .route("/history", get(inspect::history))
        .route("/stats/{name}", get(inspect::stats))
        .route("/assets", get(handlers::assets))
        .route("/assets/{name}", get(handlers::asset_detail))
        .route("/assets/{name}/schema", get(handlers::asset_schema))
        // The ledger: what a local run on another machine asks and posts (RFC-0007).
        .route("/ledger/lookup", post(ledger::lookup))
        .route("/ledger/runs", post(ledger::post_run))
        // The run contract.
        .route("/runs", get(runs::list).post(runs::create))
        .route("/runs/{run_id}", get(runs::detail).delete(runs::cancel))
        .route("/runs/{run_id}/events", get(runs::events))
        .route("/runs/{run_id}/output", get(runs::output))
        .route("/logs/{run_id}", get(handlers::logs))
        .route("/state", get(handlers::state))
        // Start the metadata database over (the old one is moved aside, never deleted).
        .route("/state/reset", post(reset::reset))
        .route("/schedule", get(handlers::schedule))
        // A schedule id is a node id: percent-encode a `/` in it.
        .route("/schedule/{id}/pause", post(pause::pause))
        .route("/schedule/{id}/resume", post(pause::resume))
        // Aliases of the run routes, kept for one minor release.
        .route("/run", post(runs::run_all))
        // The target takes the rest of the path, so a full node id with a directory in it
        // (`sub/pipeline.py:orders`) works with its `/` percent-encoded or not.
        .route(
            "/run/{*target}",
            post(runs::run_target).delete(runs::cancel),
        )
        .route("/get/{*target}", post(runs::get_target))
        .route("/status/{run_id}", get(runs::status))
        .route("/events/{run_id}", get(runs::events))
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
