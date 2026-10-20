//! HTTP errors. Every error body is the CLI's stderr error envelope
//! (`{error, code, kind, remediation, node?, traceback?, artifact_dir?}`, see
//! `barca docs contract`), and the HTTP status follows its kind: `usage` is 400 (404 for
//! something that does not exist, 409 for an ambiguous name or a conflict with the state of
//! a run, 403 on a `--read-only` server), `infra` is 500, and a server that is shutting down
//! answers 503. A failed step is never an HTTP error: it is a run whose outcome is `failed`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use barca_core::BarcaError;
use barca_core::envelope::{Context, ErrorEnvelope, ErrorKind};

/// Error type returned by route handlers. Implements `IntoResponse` so handlers
/// can use `?` directly.
#[derive(Debug)]
pub enum ApiError {
    /// A core engine error (parse/dag/db/worker).
    Barca(BarcaError),
    /// A core engine error of a command run with this context (its remediation names it).
    Run(Box<(BarcaError, Context)>),
    /// A resource (asset, run) was not found.
    NotFound(String),
    /// Ambiguous lookup, or a request that conflicts with the state of a run.
    Conflict(String),
    /// Refused by server mode (e.g. a run requested of a `--read-only` server).
    Forbidden(String),
    /// The request cannot be served as asked (e.g. `POST /get/{target}` naming a task).
    BadRequest(String),
    /// The server is shutting down and takes no new work. Raised by shutdown admission (#190).
    #[allow(dead_code)]
    Unavailable(String),
}

impl From<BarcaError> for ApiError {
    fn from(e: BarcaError) -> Self {
        ApiError::Barca(e)
    }
}

impl ApiError {
    /// An engine error of a command run with this context.
    pub fn run(err: BarcaError, ctx: Context) -> Self {
        ApiError::Run(Box::new((err, ctx)))
    }

    /// The HTTP status and the envelope.
    pub fn parts(self) -> (StatusCode, ErrorEnvelope) {
        let usage =
            |status, msg: String| (status, ErrorEnvelope::from_prose(ErrorKind::Usage, msg));
        match self {
            ApiError::NotFound(msg) => usage(StatusCode::NOT_FOUND, msg),
            ApiError::Conflict(msg) => usage(StatusCode::CONFLICT, msg),
            ApiError::Forbidden(msg) => usage(StatusCode::FORBIDDEN, msg),
            ApiError::BadRequest(msg) => usage(StatusCode::BAD_REQUEST, msg),
            ApiError::Unavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorEnvelope::from_prose(ErrorKind::Infra, msg),
            ),
            ApiError::Barca(err) => engine(err, &Context::default()),
            ApiError::Run(run) => {
                let (err, ctx) = *run;
                engine(err, &ctx)
            }
        }
    }
}

fn engine(err: BarcaError, ctx: &Context) -> (StatusCode, ErrorEnvelope) {
    let status = match &err {
        BarcaError::AssetNotFound(..) => StatusCode::NOT_FOUND,
        e => match ErrorKind::of(e) {
            ErrorKind::Usage => StatusCode::BAD_REQUEST,
            ErrorKind::Cancelled => StatusCode::CONFLICT,
            ErrorKind::StepFailed | ErrorKind::Infra => StatusCode::INTERNAL_SERVER_ERROR,
        },
    };
    (status, barca_core::outcome::envelope_of(err, ctx))
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, envelope) = self.parts();
        (status, Json(envelope.to_json())).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_follow_the_kind_and_bodies_are_the_envelope() {
        let cases = [
            (
                ApiError::NotFound("no such run\nGET /runs lists them.".into()),
                404,
                "usage",
            ),
            (ApiError::Conflict("ambiguous".into()), 409, "usage"),
            (ApiError::BadRequest("bad".into()), 400, "usage"),
            (ApiError::Forbidden("read-only".into()), 403, "usage"),
            (ApiError::Unavailable("shutting down".into()), 503, "infra"),
            (
                ApiError::Barca(BarcaError::Db("locked".into())),
                500,
                "infra",
            ),
            (
                ApiError::Barca(BarcaError::Usage("nope".into())),
                400,
                "usage",
            ),
        ];
        for (error, status, kind) in cases {
            let (got, envelope) = error.parts();
            assert_eq!(got.as_u16(), status);
            let body = envelope.to_json();
            assert_eq!(body["kind"], kind);
            assert_eq!(body["code"], envelope.code());
            assert!(body["error"].is_string());
            assert!(body.get("remediation").is_some());
        }
        let (_, envelope) = ApiError::NotFound("no such run\nGET /runs lists them.".into()).parts();
        assert_eq!(envelope.error, "no such run");
        assert_eq!(
            envelope.remediation.as_deref(),
            Some("GET /runs lists them.")
        );
    }
}
