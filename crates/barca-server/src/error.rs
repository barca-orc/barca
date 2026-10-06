//! HTTP error mapping — turns `BarcaError` into JSON responses with sensible
//! status codes.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use barca_core::BarcaError;
use serde_json::json;

/// Error type returned by route handlers. Implements `IntoResponse` so handlers
/// can use `?` directly.
#[derive(Debug)]
pub enum ApiError {
    /// A core engine error (parse/dag/db/worker).
    Barca(BarcaError),
    /// A resource (asset, run) was not found.
    NotFound(String),
    /// Ambiguous lookup — multiple matches.
    Conflict(String),
    /// Refused by server mode (e.g. a run requested of a `--read-only` server).
    Forbidden(String),
}

impl From<BarcaError> for ApiError {
    fn from(e: BarcaError) -> Self {
        ApiError::Barca(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            ApiError::Barca(err) => {
                let status = match &err {
                    BarcaError::AssetNotFound(..) => StatusCode::NOT_FOUND,
                    BarcaError::Parse(_) | BarcaError::Dag(_) | BarcaError::Usage(_) => {
                        StatusCode::BAD_REQUEST
                    }
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                (status, err.to_string())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
