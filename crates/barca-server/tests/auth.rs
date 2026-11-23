//! Real router conformance for opt-in project token authentication.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use barca_server::{ServeConfig, app, auth::Store};
use barca_service::config::resolve_in;
use barca_service::serve::AuthMode;
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
            "/health/",
            "/%68ealth",
            "/ui/%2e%2e/health",
            "/state",
            "/ledger/lookup",
            "/ledger/runs",
            "/ledger/refresh",
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

#[tokio::test]
async fn cookies_and_query_tokens_do_not_authenticate() {
    let root = tempfile::tempdir().unwrap();
    let router = fixture(root.path(), AuthMode::Token, true);
    let issued = Store::open(root.path())
        .unwrap()
        .create("browser", None)
        .unwrap();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(header::COOKIE, format!("barca_token={}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(
            &router,
            "GET",
            &format!("/health?token={}", issued.token),
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

/// A run records who asked for it and why, and who cancelled it and that it never started
/// (#925).
#[tokio::test]
async fn a_run_records_its_requesting_token_and_reason_and_who_cancelled_it() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("pipeline.py");
    std::fs::write(
        &source,
        "from barca import asset\n@asset()\ndef first() -> dict:\n    return {\"n\": 1}\n",
    )
    .unwrap();
    let mut resolved = resolve_in(None, root.path()).unwrap();
    resolved.server_auth = AuthMode::Token;
    resolved.db_path = root.path().join("metadata.db").display().to_string();
    let router = app(ServeConfig {
        files: vec![source.display().to_string()],
        host: "127.0.0.1".parse().unwrap(),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "utc".into(),
        python: "python".into(),
        resolved,
        read_only: false,
    });
    let store = Store::open(root.path()).unwrap();
    let token = store.create("ci-bot", None).unwrap().token;
    let paused = call(&router, "POST", "/queue/pause", Some(&token), None).await;
    assert_eq!(paused.status(), StatusCode::OK);
    let too_long = call(
        &router,
        "POST",
        "/runs",
        Some(&token),
        Some(json!({"command": "get", "reason": "x".repeat(501)})),
    )
    .await;
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);
    let accepted = call(
        &router,
        "POST",
        "/runs",
        Some(&token),
        Some(json!({"command": "get", "targets": ["first"], "reason": "  backfill  "})),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let run_id = json_body(accepted).await["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/runs/{run_id}");
    let queued = json_body(call(&router, "GET", &path, Some(&token), None).await).await;
    assert_eq!(queued["principal"], "token:ci-bot");
    assert_eq!(queued["trigger_reason"], "backfill");
    let cancel = call(&router, "DELETE", &path, Some(&token), None).await;
    assert_eq!(cancel.status(), StatusCode::OK);
    let run = json_body(call(&router, "GET", &path, Some(&token), None).await).await;
    validate("RunView", &run);
    assert_eq!(run["status"], "cancelled");
    assert_eq!(run["principal"], "token:ci-bot");
    assert_eq!(run["trigger_reason"], "backfill");
    assert_eq!(
        run["cancellation"],
        json!({"by": "request", "principal": "token:ci-bot", "started": false, "waiting_behind": [], "same_as": null})
    );
    let list = json_body(call(&router, "GET", "/runs", Some(&token), None).await).await;
    assert_eq!(list["runs"][0]["cancellation"]["started"], false);
    assert_eq!(list["runs"][0]["principal"], "token:ci-bot");
}

#[tokio::test]
async fn foreign_hosts_are_refused_before_authentication_or_route_dispatch() {
    for mode in [AuthMode::Off, AuthMode::Token] {
        let root = tempfile::tempdir().unwrap();
        let router = fixture(root.path(), mode, true);
        for method in ["GET", "HEAD", "POST", "DELETE", "OPTIONS"] {
            for path in ["/health", "/ui/", "/auth/status", "/runs", "/missing"] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(path)
                            .header(header::HOST, "evil.example")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
            }
        }
        for duplicate in [false, true] {
            let mut request = Request::builder()
                .uri("http://evil.example/health")
                .version(axum::http::Version::HTTP_2);
            if duplicate {
                request = request.header(header::HOST, "localhost");
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header(header::HOST, "localhost")
                    .header(header::HOST, "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!root.path().join("metadata.db").exists());
    }
}
