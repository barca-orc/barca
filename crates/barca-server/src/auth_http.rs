//! HTTP adapter for project tokens. Authorization surrounds routing and ETag handling.
use crate::{auth, error::ApiError, state::AppState};
use axum::{
    Json,
    body::Body,
    extract::{Path, Request, State, rejection::JsonRejection},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use barca_core::{
    config::{AuthMode, ResolvedConfig},
    envelope::{ErrorEnvelope, ErrorKind},
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Deserialize;

pub(crate) struct Access {
    store: Option<Result<auth::Store, auth::Error>>,
    changed: tokio::sync::watch::Sender<u64>,
}
impl Access {
    pub(crate) fn new(config: &ResolvedConfig) -> Self {
        Self {
            store: (config.server_auth == AuthMode::Token)
                .then(|| auth::Store::open(&config.project_root)),
            changed: tokio::sync::watch::channel(0).0,
        }
    }
    fn store(&self) -> Result<auth::Store, ApiError> {
        match &self.store {
            None => Err(ApiError::Forbidden("token authentication is off".into())),
            Some(Ok(store)) => Ok(store.clone()),
            Some(Err(_)) => Err(ApiError::Barca(barca_core::BarcaError::Other(
                "project authentication state is unavailable".into(),
            ))),
        }
    }
    fn notify(&self) {
        self.changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
fn unauthorized(reason: &str) -> Response {
    let mut envelope =
        ErrorEnvelope::from_prose(ErrorKind::Infra, "server authentication required");
    envelope.reason = Some(reason.into());
    let mut response = (StatusCode::UNAUTHORIZED, Json(envelope)).into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    no_store(response)
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, auth::Error> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| {
            ApiError::Barca(barca_core::BarcaError::Other(
                "authentication operation failed".into(),
            ))
        })?
        .map_err(|error| match error {
            auth::Error::Full => ApiError::Conflict(error.to_string()),
            auth::Error::Name | auth::Error::Expiry => ApiError::BadRequest(error.to_string()),
            auth::Error::Invalid => ApiError::Forbidden(error.to_string()),
            auth::Error::State => ApiError::Barca(barca_core::BarcaError::Other(error.to_string())),
        })
}
pub(crate) async fn authorize(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let access = state.auth.clone();
    if access.store.is_none() {
        return next.run(request).await;
    }
    let path = request.uri().path();
    if ((request.method() == Method::GET || request.method() == Method::HEAD)
        && crate::ui::public(path))
        || (request.method() == Method::GET && path == "/auth/status")
        || (request.method() == Method::POST && path == "/auth/bootstrap")
    {
        return next.run(request).await;
    }
    let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
    let Some(header) = headers.next() else {
        return unauthorized("authentication_required");
    };
    let token = header
        .to_str()
        .ok()
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token.to_owned());
    if headers.next().is_some() {
        return unauthorized("invalid_token");
    }
    let Some(token) = token else {
        return unauthorized("invalid_token");
    };
    let store = match access.store() {
        Ok(store) => store,
        Err(_) => return unauthorized("invalid_token"),
    };
    let check = token.clone();
    let validator = store.clone();
    if tokio::task::spawn_blocking(move || validator.validate(&check))
        .await
        .ok()
        .and_then(Result::ok)
        .is_none()
    {
        return unauthorized("invalid_token");
    }
    // Subscribe before entering the handler so a concurrent revocation cannot be missed.
    let changes = access.changed.subscribe();
    let response = next.run(request).await;
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"text/event-stream"))
    {
        return response;
    }
    let (parts, body) = response.into_parts();
    let stream = futures::stream::unfold(
        (
            body.into_data_stream(),
            changes,
            tokio::time::interval(std::time::Duration::from_secs(1)),
            store,
            token,
        ),
        |(mut body, mut changes, mut interval, store, token)| async move {
            loop {
                tokio::select! {
                    biased;
                    _ = changes.changed() => {},
                    _ = interval.tick() => {},
                    item = body.next() => return item.map(|item| (item, (body, changes, interval, store, token))),
                }
                let validator = store.clone();
                let credential = token.clone();
                tokio::task::spawn_blocking(move || validator.validate(&credential))
                    .await
                    .ok()
                    .and_then(Result::ok)?;
            }
        },
    );
    Response::from_parts(parts, Body::from_stream(stream))
}
pub(crate) async fn status(State(state): State<AppState>) -> Response {
    no_store(
        Json(serde_json::json!({"mode": if state.auth.store.is_none() { "off" } else { "token" }}))
            .into_response(),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bootstrap {
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Create {
    name: String,
    expires_at: Option<DateTime<Utc>>,
}
fn json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(value)| value)
        .map_err(|_| ApiError::BadRequest("expected a valid JSON authentication request".into()))
}
pub(crate) async fn bootstrap(
    State(state): State<AppState>,
    body: Result<Json<Bootstrap>, JsonRejection>,
) -> Response {
    let result = async {
        let store = state.auth.store()?;
        let body = json(body)?;
        blocking(move || store.redeem(&body.code)).await
    }
    .await;
    match result {
        Ok(issued) => {
            state.auth.notify();
            no_store(Json(issued).into_response())
        }
        Err(ApiError::Forbidden(_)) if state.auth.store.is_some() => unauthorized("invalid_token"),
        Err(error) => no_store(error.into_response()),
    }
}
pub(crate) async fn list(State(state): State<AppState>) -> Response {
    let result = async {
        let store = state.auth.store()?;
        blocking(move || store.list()).await
    }
    .await;
    no_store(match result {
        Ok(tokens) => Json(serde_json::json!({"tokens": tokens})).into_response(),
        Err(error) => error.into_response(),
    })
}
pub(crate) async fn create(
    State(state): State<AppState>,
    body: Result<Json<Create>, JsonRejection>,
) -> Response {
    let result = async {
        let store = state.auth.store()?;
        let body = json(body)?;
        blocking(move || store.create(&body.name, body.expires_at)).await
    }
    .await;
    no_store(match result {
        Ok(issued) => (StatusCode::CREATED, Json(issued)).into_response(),
        Err(error) => error.into_response(),
    })
}
pub(crate) async fn revoke(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let result = async {
        let store = state.auth.store()?;
        blocking(move || store.revoke(&id)).await
    }
    .await;
    no_store(match result {
        Ok(()) => {
            state.auth.notify();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => error.into_response(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    async fn streaming() -> Response {
        let first = futures::stream::once(async {
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"data: ready\n\n"))
        });
        let stream = first.chain(futures::stream::pending());
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response()
    }

    #[tokio::test]
    async fn idle_streams_close_after_local_and_external_revocation() {
        for scenario in ["local", "external", "expiry"] {
            let root = tempfile::tempdir().unwrap();
            let mut resolved = barca_core::config::resolve_in(None, root.path()).unwrap();
            resolved.server_auth = AuthMode::Token;
            let state = AppState::new(crate::ServeConfig {
                files: Vec::new(),
                host: "127.0.0.1".parse().unwrap(),
                port: 0,
                watch: false,
                schedule: false,
                timezone: "utc".into(),
                python: "python".into(),
                resolved,
                read_only: true,
            });
            let store = auth::Store::open(root.path()).unwrap();
            let expiry =
                (scenario == "expiry").then(|| Utc::now() + chrono::Duration::milliseconds(100));
            let issued = store.create("stream", expiry).unwrap();
            let router = Router::new().route("/events", get(streaming)).layer(
                axum::middleware::from_fn_with_state(state.clone(), authorize),
            );
            let request = Request::builder()
                .uri("/events")
                .header(header::AUTHORIZATION, format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap();
            let response = router.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let mut body = response.into_body().into_data_stream();
            assert!(
                body.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .starts_with(b"data: ready")
            );
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), body.next())
                    .await
                    .is_err()
            );
            if scenario != "expiry" {
                store.revoke(&issued.metadata.id).unwrap();
            }
            if scenario == "local" {
                state.auth.notify();
            }
            let limit = if scenario == "local" { 200 } else { 1200 };
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(limit), body.next())
                    .await
                    .expect("revoked idle stream remained open")
                    .is_none()
            );
        }
    }
}
