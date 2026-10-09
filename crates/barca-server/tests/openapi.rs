//! Normative current-wire conformance: real app() registration and responses, not mock handlers.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use barca_server::{ServeConfig, app};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, sync::OnceLock, time::Duration};
use tower::ServiceExt;

fn contract() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| {
        serde_yaml_ng::from_str(include_str!("../../../specs/server-api.openapi.yaml")).unwrap()
    })
}
fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::validator_for(&json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "components": contract()["components"], "allOf": [schema]
    }))
    .unwrap()
}
fn validate(schema: &Value, body: &Value) {
    let v = validator(schema);
    let errors: Vec<_> = v.iter_errors(body).map(|e| e.to_string()).collect();
    assert!(
        errors.is_empty(),
        "schema mismatch: {errors:?}\nbody: {body}"
    );
}
fn config(dir: &Path, read_only: bool) -> ServeConfig {
    let source = dir.join("pipeline.py");
    std::fs::write(
        &source,
        r#"
from barca import asset, task
@asset()
def first() -> dict:
    return {"n": 1}
@asset(inputs={"first": first})
def second(first: dict) -> dict:
    return {"n": first["n"] + 1}
@asset()
def broken() -> dict:
    raise ValueError("expected contract failure")
@task(inputs={"first": first})
def publish(first: dict) -> None:
    print("published", first["n"])
@task()
def slow() -> None:
    import time
    time.sleep(60)
"#,
    )
    .unwrap();
    let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
    resolved.db_path = dir.join("metadata.db").display().to_string();
    resolved.artifact_root = dir.join("artifacts").display().to_string();
    ServeConfig {
        files: vec![source.display().to_string()],
        host: "127.0.0.1".parse().unwrap(),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "utc".into(),
        python: barca_core::commands::find_python(),
        resolved,
        read_only,
    }
}
async fn request(router: &Router, method: &str, path: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn checked(router: &Router, method: &str, url: &str, path: &str, status: u16) -> Value {
    let resp = request(router, method, url).await;
    assert_eq!(resp.status().as_u16(), status, "{method} {url}");
    if status == 405 {
        assert!(
            resp.headers().contains_key("allow"),
            "405 lost Allow header"
        );
    }
    let operation = &contract()["paths"][path][method.to_lowercase()];
    let spec = if operation.is_null() && status == 405 {
        &contract()["x-fallback-responses"]["405"]
    } else {
        &operation["responses"][status.to_string()]
    };
    assert!(!spec.is_null(), "undocumented {method} {path} {status}");
    for (name, header) in spec["headers"].as_object().into_iter().flatten() {
        let actual = resp
            .headers()
            .get(name)
            .unwrap_or_else(|| panic!("missing {name} on {method} {url}"))
            .to_str()
            .unwrap();
        validate(&header["schema"], &json!(actual));
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.unwrap();
    if method == "HEAD" {
        assert!(bytes.is_empty());
        return Value::Null;
    }
    if let Some(mime) = content_type {
        let schema = &spec["content"][&mime]["schema"];
        assert!(
            !schema.is_null(),
            "undocumented content type {mime} on {method} {url}: {bytes:?}"
        );
        let body = if mime == "application/json" {
            serde_json::from_slice(&bytes).unwrap()
        } else {
            json!(String::from_utf8_lossy(&bytes))
        };
        validate(schema, &body);
        body
    } else {
        assert!(bytes.is_empty());
        Value::Null
    }
}

#[test]
fn spec_matches_router_paths_and_methods_and_all_schemas_compile() {
    let source = include_str!("../src/routes.rs");
    let mut registered = BTreeSet::new();
    for route in source.split(".route(").skip(1) {
        let path = route.split('"').nth(1).unwrap().replace("{*", "{");
        let mut has_method = false;
        for method in [
            "get", "post", "delete", "put", "patch", "head", "options", "trace", "connect",
        ] {
            if route.contains(&format!("{method}(")) {
                has_method = true;
                registered.insert((path.clone(), method.to_string()));
                if method == "get" {
                    registered.insert((path.clone(), "head".into()));
                }
            }
        }
        assert!(
            has_method,
            "unrecognized route wiring for {path}; update contract inventory check"
        );
    }
    let documented: BTreeSet<_> = contract()["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(p, v)| {
            v.as_object()
                .unwrap()
                .keys()
                .map(move |m| (p.clone(), m.clone()))
        })
        .collect();
    assert_eq!(
        registered, documented,
        "route or verb changed without normative contract update"
    );
    assert_eq!(contract()["openapi"], "3.1.0");
    assert_eq!(contract()["security"], json!([]));
    let mut operation_ids = BTreeSet::new();
    for methods in contract()["paths"].as_object().unwrap().values() {
        for operation in methods.as_object().unwrap().values() {
            assert!(operation_ids.insert(operation["operationId"].as_str().unwrap()));
        }
    }
    for schema in contract()["components"]["schemas"]
        .as_object()
        .unwrap()
        .values()
    {
        validator(schema);
    }
    // Prove the conformance boundary rejects missing fields, unknown keys and invented states.
    let v = validator(&json!({"$ref":"#/components/schemas/RunState"}));
    assert!(!v.is_valid(&json!({"handle":"r","status":"pending"})));
    assert!(!v.is_valid(&json!({"handle":"r","status":"queued","result":null,"error":null,"started_at":1.0,"finished_at":null})));
    let health = validator(&json!({"$ref":"#/components/schemas/Health"}));
    assert!(!health.is_valid(
        &json!({"status":"ok","version":"1","read_only":false,"scheduler":false,"invented":true})
    ));
    for event in [
        barca_core::RunEvent::RunStarted { run_id: "r".into() },
        barca_core::RunEvent::Log {
            node_id: "p.py:a".into(),
            line: "hello".into(),
        },
        barca_core::RunEvent::StepFinished {
            node_id: "p.py:a".into(),
            ok: false,
            elapsed_seconds: Some(1.0),
            error: Some("failed".into()),
        },
        barca_core::RunEvent::RunFinished {
            run_id: "r".into(),
            ok: false,
        },
    ] {
        validate(
            &contract()["components"]["schemas"]["RunEvent"],
            &serde_json::to_value(event).unwrap(),
        );
    }
}

#[tokio::test]
async fn inspection_methods_errors_read_only_and_ui_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let router = app(config(dir.path(), true));
    for (url, path) in [
        ("/health", "/health"),
        ("/plan", "/plan"),
        ("/assets", "/assets"),
        ("/assets/first", "/assets/{name}"),
        ("/assets/first/schema", "/assets/{name}/schema"),
        ("/state", "/state"),
        ("/schedule", "/schedule"),
        ("/runs?limit=0", "/runs"),
        ("/logs/missing", "/logs/{run_id}"),
    ] {
        checked(&router, "GET", url, path, 200).await;
        checked(&router, "HEAD", url, path, 200).await;
        checked(&router, "PUT", url, path, 405).await;
    }
    assert!(
        !dir.path().join("metadata.db").exists(),
        "inspection created source history"
    );
    for (url, path) in [
        ("/assets/missing", "/assets/{name}"),
        ("/assets/missing/schema", "/assets/{name}/schema"),
        ("/status/missing", "/status/{run_id}"),
        ("/runs/missing", "/runs/{id}"),
        ("/events/missing", "/events/{run_id}"),
    ] {
        checked(&router, "GET", url, path, 404).await;
    }
    checked(&router, "GET", "/runs?limit=invalid", "/runs", 400).await;
    for (method, url, path) in [
        ("POST", "/run", "/run"),
        ("POST", "/run/publish", "/run/{target}"),
        ("POST", "/get/first", "/get/{target}"),
        ("DELETE", "/run/missing", "/run/{target}"),
    ] {
        checked(&router, method, url, path, 403).await;
    }
    for path in ["/", "/ui"] {
        checked(&router, "GET", path, path, 303).await;
        checked(&router, "HEAD", path, path, 303).await;
    }
    let index_status = request(&router, "GET", "/ui/").await.status().as_u16();
    assert!(matches!(index_status, 200 | 404));
    let index = checked(&router, "GET", "/ui/", "/ui/", index_status).await;
    checked(&router, "HEAD", "/ui/", "/ui/", index_status).await;
    if index_status == 200 {
        for asset in index
            .as_str()
            .unwrap()
            .split('"')
            .filter_map(|value| value.strip_prefix("./"))
        {
            let url = format!("/ui/{asset}");
            checked(&router, "GET", &url, "/ui/{path}", 200).await;
            checked(&router, "HEAD", &url, "/ui/{path}", 200).await;
            let response = request(&router, "GET", &url).await;
            assert_eq!(
                response.headers()["cache-control"],
                if asset.starts_with("assets/") {
                    "public, max-age=31536000, immutable"
                } else {
                    "no-cache"
                }
            );
        }
    }
    checked(&router, "GET", "/ui/no-such-asset.css", "/ui/{path}", 404).await;
    let fallback = request(&router, "GET", "/not-a-route").await;
    assert_eq!(fallback.status(), StatusCode::NOT_FOUND);
    assert_eq!(fallback.headers()["content-type"], "application/json");
    let body: Value =
        serde_json::from_slice(&to_bytes(fallback.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    validate(
        &contract()["x-fallback-responses"]["404"]["content"]["application/json"]["schema"],
        &body,
    );
}
async fn wait(router: &Router, handle: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let state = checked(
                router,
                "GET",
                &format!("/status/{handle}"),
                "/status/{run_id}",
                200,
            )
            .await;
            if ["complete", "failed", "cancelled"].contains(&state["status"].as_str().unwrap()) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("run failed to terminate")
}
#[tokio::test]
async fn actual_runs_events_failure_cancel_and_restart_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), false);
    let router = app(cfg.clone());
    checked(&router, "POST", "/run/first", "/run/{target}", 400).await;
    checked(&router, "POST", "/get/publish", "/get/{target}", 400).await;
    checked(&router, "POST", "/get/missing", "/get/{target}", 404).await;
    let handle = checked(&router, "POST", "/get/second", "/get/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let events = request(&router, "GET", &format!("/events/{handle}")).await;
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    assert_eq!(events.headers()["x-accel-buffering"], "no");
    let mut stream = events.into_body().into_data_stream();
    let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = std::str::from_utf8(&frame).unwrap();
    let data = text
        .lines()
        .find_map(|s| s.strip_prefix("data: "))
        .expect("SSE data frame");
    validate(
        &contract()["components"]["schemas"]["RunEvent"],
        &serde_json::from_str(data).unwrap(),
    );
    drop(stream);
    let complete = wait(&router, &handle).await;
    assert_eq!(complete["status"], "complete");
    checked(&router, "GET", "/state", "/state", 200).await;
    checked(
        &router,
        "GET",
        "/assets/second/schema",
        "/assets/{name}/schema",
        200,
    )
    .await;
    checked(&router, "GET", "/assets/second", "/assets/{name}", 200).await;
    let durable = complete["result"]["run_id"].as_str().unwrap();
    checked(
        &router,
        "GET",
        &format!("/runs/{handle}"),
        "/runs/{id}",
        200,
    )
    .await;
    checked(
        &router,
        "DELETE",
        &format!("/run/{handle}"),
        "/run/{target}",
        409,
    )
    .await;
    let task = checked(&router, "POST", "/run/publish", "/run/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(wait(&router, &task).await["status"], "complete");
    let logs = checked(
        &router,
        "GET",
        &format!("/logs/{task}"),
        "/logs/{run_id}",
        200,
    )
    .await;
    assert!(!logs["logs"].as_array().unwrap().is_empty());
    let broken = checked(&router, "POST", "/get/broken", "/get/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(wait(&router, &broken).await["status"], "failed");
    let slow = checked(&router, "POST", "/run/slow", "/run/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    checked(
        &router,
        "DELETE",
        &format!("/run/{slow}"),
        "/run/{target}",
        200,
    )
    .await;
    assert_eq!(wait(&router, &slow).await["status"], "cancelled");
    let restarted = app(cfg);
    let detail = checked(
        &restarted,
        "GET",
        &format!("/runs/{durable}"),
        "/runs/{id}",
        200,
    )
    .await;
    assert!(detail["result"].is_null());
    assert_eq!(detail["run"]["status"], "success");
    assert!(!detail["steps"].as_array().unwrap().is_empty());
    checked(
        &restarted,
        "GET",
        &format!("/status/{handle}"),
        "/status/{run_id}",
        404,
    )
    .await;
    checked(&restarted, "GET", "/runs?limit=10000", "/runs", 200).await;
}

#[tokio::test]
async fn ambiguous_targets_and_snapshot_failure_keep_error_envelopes() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), false);
    let other = dir.path().join("other.py");
    std::fs::write(
        &other,
        "from barca import asset\n@asset()\ndef first() -> dict:\n    return {}\n",
    )
    .unwrap();
    cfg.files.push(other.display().to_string());
    let router = app(cfg.clone());
    checked(&router, "GET", "/assets/first", "/assets/{name}", 409).await;
    checked(&router, "POST", "/get/first", "/get/{target}", 409).await;
    let not_a_database = dir.path().join("database-is-a-directory");
    std::fs::create_dir(&not_a_database).unwrap();
    cfg.resolved.db_path = not_a_database.display().to_string();
    let broken_store = app(cfg);
    checked(&broken_store, "GET", "/runs", "/runs", 500).await;
    checked(&broken_store, "GET", "/health", "/health", 200).await;
}
