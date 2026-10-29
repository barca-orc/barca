//! Real router conformance for opt-in project token authentication.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use barca_core::config::{AuthMode, resolve_in};
use barca_server::{ServeConfig, app, auth::Store};
use serde_json::{Value, json};
use tower::ServiceExt;

fn fixture(root: &std::path::Path, mode: AuthMode, read_only: bool) -> Router {
    let mut resolved = resolve_in(None, root).unwrap();
    resolved.server_auth = mode;
    resolved.db_path = root.join("metadata.db").display().to_string();
    app(ServeConfig {
        files: Vec::new(),
        host: "127.0.0.1".parse().unwrap(),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "utc".into(),
        python: "python".into(),
        resolved,
        read_only,
    })
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = if let Some(body) = body {
        request = request.header(header::CONTENT_TYPE, "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
}
fn validate(schema: &str, body: &Value) {
    let contract: Value = serde_yaml_ng::from_str(include_str!(
        "../../../specs/interfaces/server-api.openapi.yaml"
    ))
    .unwrap();
    let schema = json!({"components": contract["components"], "allOf": [{"$ref": format!("#/components/schemas/{schema}")}]});
    let validator = jsonschema::validator_for(&schema).unwrap();
    // Do not print response bodies: an issuance response contains a secret.
    assert!(
        validator.is_valid(body),
        "authentication response does not match its schema"
    );
}

async fn json_body(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn every_api_path_and_method_authenticates_before_dispatch_or_etag() {
    let root = tempfile::tempdir().unwrap();
    let router = fixture(root.path(), AuthMode::Token, true);
    for method in ["GET", "HEAD", "POST", "DELETE", "OPTIONS"] {
        for path in [
            "/health",
            "/state",
            "/ledger/lookup",
            "/ledger/runs",
            "/runs/missing/events",
            "/events/missing",
            "/queue",
            "/no-route",
            "/ui/not-an-asset",
            "/auth/status/",
        ] {
            let response = call(&router, method, path, None, None).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            if method != "HEAD" {
                let body = json_body(response).await;
                validate("Error", &body);
                assert_eq!(body["kind"], "infra");
                assert_eq!(body["code"], 3);
                assert_eq!(body["reason"], "authentication_required");
            }
        }
    }
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(header::IF_NONE_MATCH, "*")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(call(&router, "GET", "/auth/status", None, None).await).await,
        json!({"mode": "token"})
    );
    assert_ne!(
        call(&router, "GET", "/ui/", None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&router, "HEAD", "/auth/status", None, None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(!root.path().join("metadata.db").exists());
}

#[tokio::test]
async fn bootstrap_and_administration_work_read_only_and_revocation_is_immediate() {
    let root = tempfile::tempdir().unwrap();
    let router = fixture(root.path(), AuthMode::Token, true);
    let store = Store::open(root.path()).unwrap();
    let code = store.bootstrap().unwrap();
    let response = call(
        &router,
        "POST",
        "/auth/bootstrap",
        None,
        Some(json!({"code": code})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let connection = json_body(response).await;
    validate("IssuedToken", &connection);
    let token = connection["token"].as_str().unwrap();
    assert_eq!(
        call(
            &router,
            "POST",
            "/auth/bootstrap",
            None,
            Some(json!({"code": code}))
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = call(
        &router,
        "POST",
        "/auth/tokens",
        Some(token),
        Some(json!({"name": "agent"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let issued = json_body(response).await;
    validate("IssuedToken", &issued);
    let secret = issued["token"].as_str().unwrap();
    let metadata = json_body(call(&router, "GET", "/auth/tokens", Some(secret), None).await).await;
    assert_eq!(metadata["tokens"].as_array().unwrap().len(), 2);
    assert!(!metadata.to_string().contains(secret));
    assert!(!metadata.to_string().contains("digest"));
    assert_eq!(
        call(
            &router,
            "POST",
            "/runs",
            Some(secret),
            Some(json!({"command":"get"}))
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let path = format!("/auth/tokens/{}", issued["id"].as_str().unwrap());
    assert_eq!(
        call(&router, "DELETE", &path, Some(token), None)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&router, "DELETE", &path, Some(token), None)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&router, "GET", "/auth/tokens", Some(secret), None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(!root.path().join("metadata.db").exists());
}

#[tokio::test]
async fn off_mode_never_exposes_unauthenticated_token_administration() {
    let root = tempfile::tempdir().unwrap();
    let router = fixture(root.path(), AuthMode::Off, true);
    assert_eq!(
        json_body(call(&router, "GET", "/auth/status", None, None).await).await,
        json!({"mode": "off"})
    );
    for (method, path, body) in [
        ("GET", "/auth/tokens", None),
        ("POST", "/auth/tokens", Some(json!({"name":"agent"}))),
        ("POST", "/auth/bootstrap", Some(json!({"code":"unused"}))),
        ("DELETE", "/auth/tokens/missing", None),
    ] {
        assert_eq!(
            call(&router, method, path, None, body).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    assert!(!root.path().join(".barca/auth").exists());
}

#[tokio::test]
async fn malformed_json_and_invalid_credentials_never_echo_secrets() {
    let root = tempfile::tempdir().unwrap();
    let router = fixture(root.path(), AuthMode::Token, true);
    let invalid = "do-not-echo-private-credential";
    let response = call(&router, "GET", "/health", Some(invalid), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(!json_body(response).await.to_string().contains(invalid));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/bootstrap")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!("{{broken {invalid}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!json_body(response).await.to_string().contains(invalid));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(header::AUTHORIZATION, format!("Bearer {invalid}"))
                .header(header::AUTHORIZATION, "Basic ignored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
