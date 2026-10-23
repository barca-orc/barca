//! Router wiring. The router is the single API boundary — middleware (CORS,
//! auth) would layer in here. The web UI is served under `/ui/`, beside the API
//! routes rather than in front of them, so API paths stay what the contract says.

use crate::state::AppState;
use crate::{handlers, inspect, ledger, pause, queue, reset, runs, ui};
use axum::Router;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

/// Build the v1 API router over the given state.
pub fn router(state: AppState) -> Router {
    // The read routes answer a repeated request with `304 Not Modified` when the body would be
    // the same ([`etag`]): a poll that finds nothing changed transfers nothing.
    let reads = Router::new()
        .route("/health", get(handlers::health))
        .route("/project", get(inspect::project))
        .route("/plan", get(handlers::plan))
        .route("/groups", get(handlers::groups))
        // Inspection in the CLI's `--json` shapes.
        .route("/list", get(inspect::list))
        .route("/nodes", get(inspect::nodes))
        .route("/history", get(inspect::history))
        .route("/stats/{name}", get(inspect::stats))
        .route("/sql/views", get(inspect::sql_views))
        .route("/materializations", get(inspect::materializations))
        .route("/assets", get(handlers::assets))
        .route("/assets/{name}", get(handlers::asset_detail))
        .route("/assets/{name}/schema", get(handlers::asset_schema))
        // The run contract's reads; the middleware leaves the other methods alone.
        .route("/runs", get(runs::list).post(runs::create))
        .route("/runs/{run_id}", get(runs::detail).delete(runs::cancel))
        .route("/runs/{run_id}/logs", get(runs::logs))
        .route("/logs/{run_id}", get(handlers::logs))
        // The run queue: what waits to start, and pausing it.
        .route("/queue", get(queue::get))
        .route("/queue/pause", post(queue::pause))
        .route("/queue/resume", post(queue::resume))
        .route("/state", get(handlers::state))
        .route("/schedule", get(handlers::schedule))
        .route_layer(axum::middleware::from_fn(etag));
    Router::new()
        .merge(reads)
        // The ledger: what a local run on another machine asks and posts (RFC-0007).
        .route("/ledger/lookup", post(ledger::lookup))
        .route("/ledger/runs", post(ledger::post_run))
        // The rest of the run contract.
        .route("/runs/{run_id}/events", get(runs::events))
        .route("/runs/{run_id}/output", get(runs::output))
        // Start the metadata database over (the old one is moved aside, never deleted).
        .route("/state/reset", post(reset::reset))
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

/// An `ETag` on every successful `GET` of a read route, a digest of its body, with
/// `Cache-Control: no-cache` so a browser asks again each time; a request whose
/// `If-None-Match` names the digest gets `304 Not Modified` and no body. The body is still
/// computed (the read cache makes that cheap); what is saved is sending it and parsing it.
async fn etag(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::http::{HeaderValue, Method, StatusCode, header};
    let get = request.method() == Method::GET;
    let asked = request.headers().get(header::IF_NONE_MATCH).cloned();
    let response = next.run(request).await;
    if !get || response.status() != StatusCode::OK {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let tag = {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(&bytes);
        let hex: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
        format!("\"{hex}\"")
    };
    let value = HeaderValue::from_str(&tag).expect("hex is a valid header value");
    parts.headers.insert(header::ETAG, value.clone());
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let matches = asked
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(|t| t.trim().trim_start_matches("W/"))
                .any(|t| t == tag || t == "*")
        });
    if matches {
        let mut not_modified = StatusCode::NOT_MODIFIED.into_response();
        let headers = not_modified.headers_mut();
        headers.insert(header::ETAG, value);
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return not_modified;
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}
