//! What a request no route takes is answered with: a JSON `404`, or a `405` naming the
//! methods the path does take.

use crate::error::ApiError;
use axum::Json;
use axum::response::{IntoResponse, Response};

/// Any request no route matches: `404` with the error envelope every other error has,
/// naming what was asked for. A trigger with nothing after `/get/` or `/run/` is told what is
/// missing.
pub async fn no_route(method: axum::http::Method, uri: axum::http::Uri) -> ApiError {
    let path = uri.path();
    match (method.as_str(), path) {
        ("POST", "/get/" | "/get") => ApiError::NotFound(
            "POST /get/{target} needs a target: an asset or sensor name or its full id\nPOST /runs with {\"command\": \"get\", \"targets\": [...]} is the run route.".into(),
        ),
        ("POST", "/run/") => ApiError::NotFound(
            "POST /run/{target} needs a target: a task or sensor name or its full id \
             (POST /run, without the slash, gets every asset and sensor)\nPOST /runs with {\"command\": \"run\", \"targets\": [...]} is the run route."
                .into(),
        ),
        _ => ApiError::NotFound(format!(
            "no such endpoint: {method} {path}\nThe routes are listed at https://barca.sh/reference/server-api-endpoints/."
        )),
    }
}

/// The methods `path` takes, for a `405`.
fn allowed_methods(path: &str) -> &'static str {
    let runs_item = path
        .strip_prefix("/runs/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
    if path == "/runs" {
        "GET, POST"
    } else if runs_item {
        "GET, DELETE"
    } else if path == "/run"
        || path.starts_with("/get/")
        || (path.starts_with("/schedule/")
            && (path.ends_with("/pause") || path.ends_with("/resume")))
    {
        "POST"
    } else if path.starts_with("/run/") {
        "POST, DELETE"
    } else {
        "GET"
    }
}

/// A known path asked with a method it does not take: `405` with the error envelope and the
/// methods it does take, also in the `Allow` header.
pub async fn wrong_method(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    let path = uri.path();
    let allowed = allowed_methods(path);
    let error = ApiError::BadRequest(format!(
        "{method} is not allowed on {path}: use {}",
        allowed.replace(", ", " or ")
    ));
    let (_, envelope) = error.parts();
    (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, allowed)],
        Json(envelope.to_json()),
    )
        .into_response()
}
