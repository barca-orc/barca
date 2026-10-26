//! Normative current-wire conformance: real app() registration and responses, not mock handlers.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use barca_server::{ServeConfig, app, draining_app};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, sync::OnceLock, time::Duration};
use tower::ServiceExt;

fn contract() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| {
        serde_yaml_ng::from_str(include_str!(
            "../../../specs/interfaces/server-api.openapi.yaml"
        ))
        .unwrap()
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
    resolved.local_artifact_dir = resolved.artifact_root.clone();
    let python = barca_core::commands::find_python();
    // Plain cargo test runs before CI installs a wheel. Follow api.rs's repository
    // launcher convention, scoped to children instead of changing parallel tests' env.
    #[cfg(unix)]
    let python = {
        use std::os::unix::fs::PermissionsExt;
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
        let launcher = dir.join("contract-python");
        let source = serde_json::to_string(&source.display().to_string()).unwrap();
        let interpreter = serde_json::to_string(&python.display().to_string()).unwrap();
        std::fs::write(&launcher, format!(
            "#!/usr/bin/env python3\nimport os, sys\nos.environ['PYTHONPATH'] = {source} + (os.pathsep + os.environ['PYTHONPATH'] if os.environ.get('PYTHONPATH') else '')\nos.execvp({interpreter}, [{interpreter}, *sys.argv[1:]])\n"
        )).unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        launcher
    };
    ServeConfig {
        files: vec![source.display().to_string()],
        host: "127.0.0.1".parse().unwrap(),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "utc".into(),
        python,
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
async fn request_json(router: &Router, method: &str, path: &str, body: Value) -> (u16, Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
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

/// A repeated `GET` of `url` with the `ETag` of the first answer is the documented `304`: no
/// body, the same `ETag`.
async fn not_modified(router: &Router, url: &str, path: &str) {
    let first = request(router, "GET", url).await;
    let tag = first
        .headers()
        .get("etag")
        .unwrap_or_else(|| panic!("no ETag on GET {url}"))
        .clone();
    let again = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(url)
                .header("if-none-match", tag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "GET {url} again");
    let spec = &contract()["paths"][path]["get"]["responses"]["304"];
    assert!(!spec.is_null(), "undocumented GET {path} 304");
    for (name, header) in spec["headers"].as_object().into_iter().flatten() {
        let actual = again
            .headers()
            .get(name)
            .unwrap_or_else(|| panic!("missing {name} on the 304 of {url}"));
        validate(&header["schema"], &json!(actual.to_str().unwrap()));
    }
    assert_eq!(again.headers().get("etag"), Some(&tag));
    assert!(to_bytes(again.into_body(), 1024).await.unwrap().is_empty());
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
    let step = validator(&json!({"$ref":"#/components/schemas/StepReport"}));
    assert!(step.is_valid(&json!({"id":"p.py:a", "kind":"asset", "bytes_changed":true})));
    assert!(!step.is_valid(&json!({"id":"p.py:a", "kind":"asset", "bytes_changed":false})));
    assert!(!step.is_valid(&json!({"id":"p.py:a", "kind":"asset", "bytes_changed":"true"})));
    let uploads = validator(&json!({"$ref":"#/components/schemas/Uploads"}));
    let pending = barca_core::results::UploadsReport {
        pending: 1,
        error: Some("denied".into()),
        nodes: vec!["p.py:a".into()],
    };
    assert!(uploads.is_valid(&serde_json::to_value(&pending).unwrap()));
    assert!(!uploads.is_valid(&json!({"pending":1})));
    let cache = validator(&json!({"$ref":"#/components/schemas/Cache"}));
    assert!(cache.is_valid(&json!({"state":"unsynced","reason":"local_only","detail":"d"})));
    let event = validator(&json!({"$ref":"#/components/schemas/BytesChange"}));
    assert!(event.is_valid(&json!({"node_id":"p.py:a", "run_hash":"h", "content_hash":"digest", "created_at":"2026-10-15 00:00:00"})));
    assert!(!event.is_valid(
        &json!({"node_id":"p.py:a", "run_hash":"h", "created_at":"2026-10-15 00:00:00"})
    ));
    let health = validator(&json!({"$ref":"#/components/schemas/Health"}));
    let ok = json!({"status":"ok","version":"1","api_version":1,"env":"default","store_uri":null,"read_only":false,"scheduler":false,"load_errors":[],"scheduler_enabled":false,"metadata_error":null,"started_at":"2026-10-22T10:00:00.000Z","queue":{"paused":false,"queued":0,"running":0,"max_runs":4,"capacity":null}});
    assert!(health.is_valid(&ok));
    let mut shared = ok.clone();
    shared["queue"]["capacity"] = json!({"total": 4, "in_use": 3, "waiting": 1});
    assert!(health.is_valid(&shared));
    let queue = validator(&json!({"$ref":"#/components/schemas/Queue"}));
    let mut view = json!({"paused":false,"paused_at":null,"draining":false,"max_runs":4,"running":[],"queued":[],"capacity":null});
    assert!(queue.is_valid(&view));
    let capacity = barca_core::capacity::SharedCapacity::new(4, "--capacity");
    assert!(capacity.try_acquire("r1", "p.py:a", 3));
    assert!(!capacity.try_acquire("r2", "p.py:b", 2));
    view["capacity"] = serde_json::to_value(capacity.snapshot()).unwrap();
    assert!(queue.is_valid(&view), "{view}");
    let mut no_queue = ok.clone();
    no_queue.as_object_mut().unwrap().remove("queue");
    assert!(!health.is_valid(&no_queue));
    let accepted = validator(&json!({"$ref":"#/components/schemas/RunAccepted"}));
    assert!(accepted.is_valid(&json!({"run_id":"r","status":"pending","position":2})));
    assert!(accepted.is_valid(&json!({"run_id":"r","status":"running","position":null})));
    assert!(!accepted.is_valid(&json!({"run_id":"r","status":"queued","position":0})));
    let mut unusable = ok.clone();
    unusable["metadata_error"] = json!("the metadata DB .barca/metadata.db cannot be used: x");
    assert!(health.is_valid(&unusable));
    unusable["metadata_error"] = json!(1);
    assert!(!health.is_valid(&unusable));
    let mut draining = ok.clone();
    draining["status"] = json!("draining");
    assert!(health.is_valid(&draining));
    let mut stopping = ok.clone();
    stopping["status"] = json!("stopping");
    assert!(!health.is_valid(&stopping));
    let mut missing = ok.clone();
    missing.as_object_mut().unwrap().remove("api_version");
    assert!(!health.is_valid(&missing));
    let mut invented = ok.clone();
    invented["invented"] = json!(true);
    assert!(!health.is_valid(&invented));
    // Every error is the CLI envelope: a kind with its exit code.
    let error = validator(&json!({"$ref":"#/components/schemas/Error"}));
    assert!(error.is_valid(&json!({"error":"no","code":2,"kind":"usage","remediation":null})));
    assert!(!error.is_valid(&json!({"error":"no"})));
    assert!(!error.is_valid(&json!({"error":"no","code":2,"kind":"invented","remediation":null})));
    // An outcome is tagged by status; cancelled and error carry the envelope only.
    let outcome = validator(&json!({"$ref":"#/components/schemas/Outcome"}));
    let envelope =
        json!({"error":"run cancelled","code":130,"kind":"cancelled","remediation":null});
    assert!(outcome.is_valid(&json!({"status":"cancelled","envelope":envelope})));
    assert!(!outcome.is_valid(&json!({"status":"cancelled"})));
    assert!(!outcome.is_valid(&json!({"status":"invented","envelope":envelope})));
    let request = validator(&json!({"$ref":"#/components/schemas/RunRequest"}));
    assert!(request.is_valid(&json!({"command":"get","targets":["a"]})));
    assert!(
        request.is_valid(&json!({"command":"get","targets":["a"],"values":true,"partition":"k=a"}))
    );
    assert!(!request.is_valid(&json!({"command":"sql"})));
    assert!(!request.is_valid(&json!({"command":"get","invented":1})));
    let load_error = validator(&json!({"$ref":"#/components/schemas/LoadError"}));
    assert!(!load_error.is_valid(&json!({"file":"p.py","error":"broken"})));
    assert!(!load_error.is_valid(&json!({"file":"p.py","error":"broken","affected_nodes":[1]})));
    for event in [
        barca_core::RunEvent::RunStarted { run_id: "r".into() },
        barca_core::RunEvent::log(
            "p.py:a".into(),
            "hello".into(),
            barca_core::LogStream::Stderr,
        ),
        barca_core::RunEvent::StepFinished {
            node_id: "p.py:a".into(),
            ok: false,
            cached: false,
            elapsed_seconds: Some(1.0),
            error: Some("failed".into()),
            import_seconds: Some(1.25),
        },
        barca_core::RunEvent::RunFinished {
            run_id: "r".into(),
            ok: false,
            outcome: None,
        },
        barca_core::RunEvent::Line {
            line: "[barca] step:p.py:a cached".into(),
            when: barca_core::lines::LineWhen::Agent,
        },
        barca_core::RunEvent::StepFinished {
            node_id: "p.py:a".into(),
            ok: true,
            cached: true,
            elapsed_seconds: None,
            error: None,
            import_seconds: None,
        },
        barca_core::RunEvent::RunFinished {
            run_id: "r".into(),
            ok: false,
            outcome: Some(
                serde_json::value::RawValue::from_string(
                    json!({"status":"error","envelope":{"error":"x","code":3,"kind":"infra","remediation":null}}).to_string(),
                )
                .unwrap(),
            ),
        },
    ] {
        validate(
            &contract()["components"]["schemas"]["RunEvent"],
            &serde_json::to_value(event).unwrap(),
        );
    }
}

#[tokio::test]
async fn nested_groups_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), true);
    let source = config.files[0].clone();
    let mut pipeline = std::fs::read_to_string(&source).unwrap();
    pipeline.push_str(
        r#"
from barca import group
preparation = group("Preparation", members=[first], output=first)
modeling = group("Modeling", members=[preparation, second, publish], output=second)
"#,
    );
    std::fs::write(&source, pipeline).unwrap();
    let router = app(config);
    let groups = checked(&router, "GET", "/groups", "/groups", 200).await;
    let groups = groups.as_array().unwrap();
    assert_eq!(groups.len(), 2);
    let preparation = groups.iter().find(|g| g["name"] == "Preparation").unwrap();
    let modeling = groups.iter().find(|g| g["name"] == "Modeling").unwrap();
    assert_eq!(modeling["members"][0], preparation["id"]);
    assert_eq!(preparation["output"], format!("{source}:first"));
    assert_eq!(modeling["output"], format!("{source}:second"));
}

#[tokio::test]
async fn inspection_methods_errors_read_only_and_ui_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let router = app(config(dir.path(), true));
    for (url, path) in [
        ("/health", "/health"),
        ("/plan", "/plan"),
        ("/assets", "/assets"),
        ("/groups", "/groups"),
        ("/assets/first", "/assets/{name}"),
        ("/assets/first/schema", "/assets/{name}/schema"),
        ("/state", "/state"),
        ("/schedule", "/schedule"),
        ("/runs?limit=0", "/runs"),
        ("/logs/missing", "/logs/{run_id}"),
        ("/project", "/project"),
        ("/list", "/list"),
        ("/nodes", "/nodes"),
        ("/nodes?targets=first&sample=1", "/nodes"),
        ("/nodes?state=never_run,stale&problems=1", "/nodes"),
        ("/history?limit=5", "/history"),
        ("/history?all=1", "/history"),
        ("/stats/first", "/stats/{name}"),
        ("/sql/views", "/sql/views"),
        ("/sql/views?files=pipeline.py", "/sql/views"),
        ("/materializations", "/materializations"),
        ("/materializations?limit=5&offset=1", "/materializations"),
    ] {
        checked(&router, "GET", url, path, 200).await;
        not_modified(&router, url, path).await;
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
        ("/runs/missing", "/runs/{run_id}"),
        ("/runs/missing/events", "/runs/{run_id}/events"),
        ("/runs/missing/output", "/runs/{run_id}/output"),
        ("/events/missing", "/events/{run_id}"),
        ("/stats/missing", "/stats/{name}"),
        ("/nodes?targets=missing", "/nodes"),
    ] {
        checked(&router, "GET", url, path, 404).await;
    }
    checked(&router, "GET", "/runs?limit=invalid", "/runs", 400).await;
    checked(
        &router,
        "GET",
        "/materializations?limit=invalid",
        "/materializations",
        400,
    )
    .await;
    // Nothing has run: no views, and every node names the command that produces it.
    let catalog = checked(&router, "GET", "/sql/views", "/sql/views", 200).await;
    assert_eq!(catalog["views"], json!([]));
    assert!(
        catalog["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "second" && m["command"] == "barca get second"),
        "{catalog}"
    );
    let steps = checked(
        &router,
        "GET",
        "/materializations",
        "/materializations",
        200,
    )
    .await;
    assert_eq!(steps["total"], 0);
    assert_eq!(steps["limit"], 1000);
    checked(&router, "GET", "/nodes?state=failed", "/nodes", 400).await;
    for (method, url, path) in [
        ("POST", "/run", "/run"),
        ("POST", "/run/publish", "/run/{target}"),
        ("POST", "/get/first", "/get/{target}"),
        ("DELETE", "/run/missing", "/run/{target}"),
        ("DELETE", "/runs/missing", "/runs/{run_id}"),
        ("POST", "/schedule/missing/pause", "/schedule/{id}/pause"),
        ("POST", "/schedule/missing/resume", "/schedule/{id}/resume"),
        ("POST", "/state/reset", "/state/reset"),
    ] {
        checked(&router, method, url, path, 403).await;
    }
    assert!(
        !dir.path().join("metadata.db").exists(),
        "a read-only reset touched history"
    );
    let refused = request_json(
        &router,
        "POST",
        "/runs",
        json!({"command":"get","targets":["first"]}),
    )
    .await;
    assert_eq!(refused.0, 403);
    validate(
        &contract()["paths"]["/runs"]["post"]["responses"]["403"]["content"]["application/json"]["schema"],
        &refused.1,
    );
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
        "/runs/{run_id}",
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
    // The route `barca logs` reads: the run's status and lines, filtered by step and stream.
    let run_logs = checked(
        &router,
        "GET",
        &format!("/runs/{task}/logs"),
        "/runs/{run_id}/logs",
        200,
    )
    .await;
    assert_eq!(run_logs["status"], "success");
    assert_eq!(run_logs["logs"], logs["logs"]);
    let stdout = checked(
        &router,
        "GET",
        &format!("/runs/{task}/logs?step=publish&stream=stdout"),
        "/runs/{run_id}/logs",
        200,
    )
    .await;
    assert!(
        stdout["logs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["stream"] == "stdout" && l["ts"].is_string())
    );
    for (query, status) in [("?stream=both", 400), ("?step=invented", 400)] {
        checked(
            &router,
            "GET",
            &format!("/runs/{task}/logs{query}"),
            "/runs/{run_id}/logs",
            status,
        )
        .await;
    }
    checked(
        &router,
        "GET",
        "/runs/unknown/logs",
        "/runs/{run_id}/logs",
        404,
    )
    .await;
    let broken = checked(&router, "POST", "/get/broken", "/get/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(wait(&router, &broken).await["status"], "failed");
    let slow = checked(&router, "POST", "/run/slow", "/run/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // While it runs: `running` on /state and the node detail, and /runs?status=running.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let state = checked(&router, "GET", "/state", "/state", 200).await;
            let node = state
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["name"] == "slow")
                .unwrap()
                .clone();
            if node["running"]["step"] == "running" {
                assert_eq!(node["running"]["run_id"], slow.as_str());
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the slow run shows as running");
    let detail = checked(&router, "GET", "/assets/slow", "/assets/{name}", 200).await;
    assert_eq!(detail["running"]["run_id"], slow.as_str());
    let in_flight = checked(&router, "GET", "/runs?status=running", "/runs", 200).await;
    assert_eq!(in_flight["runs"][0]["id"], slow.as_str());
    assert!(in_flight["runs"][0]["active_steps"].is_array());
    checked(&router, "GET", "/runs?status=finished", "/runs", 400).await;
    checked(
        &router,
        "DELETE",
        &format!("/run/{slow}"),
        "/run/{target}",
        200,
    )
    .await;
    assert_eq!(wait(&router, &slow).await["status"], "cancelled");
    // The run contract: POST /runs, its stream, its outcome, its output.
    for (body, status) in [
        (json!({"command":"get","targets":["missing"]}), 404),
        (json!({"command":"get","targets":["publish"]}), 400),
        (json!({"command":"run","targets":[]}), 400),
        (json!({"command":"get","invented":true}), 400),
    ] {
        let (got, error) = request_json(&router, "POST", "/runs", body.clone()).await;
        assert_eq!(got, status, "{body}: {error}");
        validate(
            &contract()["paths"]["/runs"]["post"]["responses"][status.to_string()]["content"]["application/json"]
                ["schema"],
            &error,
        );
    }
    let (got, dry) = request_json(
        &router,
        "POST",
        "/runs",
        json!({"command":"get","targets":["second"],"dry_run":true}),
    )
    .await;
    assert_eq!(got, 200);
    validate(&contract()["components"]["schemas"]["Explain"], &dry);
    let (got, accepted) = request_json(
        &router,
        "POST",
        "/runs",
        json!({"command":"get","targets":["second"]}),
    )
    .await;
    assert_eq!(got, 202);
    validate(
        &contract()["components"]["schemas"]["RunAccepted"],
        &accepted,
    );
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();
    let events = request(&router, "GET", &format!("/runs/{run_id}/events")).await;
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(events.headers()["x-accel-buffering"], "no");
    let text = String::from_utf8(
        to_bytes(events.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let mut finished = Value::Null;
    for frame in text.split("\n\n").filter(|f| f.contains("data: ")) {
        assert!(
            frame.lines().any(|l| l.starts_with("id: ")),
            "every event has an id: {frame}"
        );
        let data: Value = serde_json::from_str(
            frame
                .lines()
                .find_map(|l| l.strip_prefix("data: "))
                .unwrap(),
        )
        .unwrap();
        validate(&contract()["components"]["schemas"]["RunEvent"], &data);
        if data["type"] == "run_finished" {
            finished = data;
        }
    }
    assert_eq!(finished["outcome"]["status"], "success", "{text}");
    let view = checked(
        &router,
        "GET",
        &format!("/runs/{run_id}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    assert_eq!(view["outcome"], finished["outcome"]);
    checked(
        &router,
        "GET",
        &format!("/runs/{run_id}/output"),
        "/runs/{run_id}/output",
        200,
    )
    .await;
    checked(
        &router,
        "HEAD",
        &format!("/runs/{run_id}/output"),
        "/runs/{run_id}/output",
        200,
    )
    .await;
    checked(
        &router,
        "DELETE",
        &format!("/runs/{run_id}"),
        "/runs/{run_id}",
        409,
    )
    .await;
    checked(
        &router,
        "GET",
        &format!("/runs/{broken}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    checked(
        &router,
        "GET",
        &format!("/runs/{broken}/output"),
        "/runs/{run_id}/output",
        404,
    )
    .await;
    checked(
        &router,
        "GET",
        &format!("/runs/{slow}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    // `second` and its input ran: each is a view at the result the ledger records, and a step
    // row; `broken` failed, so it has a step row and no view.
    let catalog = checked(&router, "GET", "/sql/views", "/sql/views", 200).await;
    let view = |name: &str| {
        catalog["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .cloned()
    };
    let second = view("second").expect("a view of second");
    assert_eq!(second["format"], "json");
    assert_eq!(second["stale"], false);
    let path = second["files"][0]["path"].as_str().unwrap();
    assert!(Path::new(path).is_file(), "{path}");
    assert!(view("broken").is_none());
    let steps = checked(
        &router,
        "GET",
        "/materializations?limit=100",
        "/materializations",
        200,
    )
    .await;
    let rows = steps["materializations"].as_array().unwrap();
    assert_eq!(steps["total"].as_u64().unwrap() as usize, rows.len());
    assert!(
        rows.iter()
            .any(|r| r["artifact"] == path && r["status"] == "success")
    );
    assert!(rows.iter().any(|r| r["status"] == "failed"));
    let page = checked(
        &router,
        "GET",
        "/materializations?limit=1&offset=1",
        "/materializations",
        200,
    )
    .await;
    assert_eq!(page["materializations"].as_array().unwrap().len(), 1);
    assert_eq!(page["materializations"][0], rows[1]);
    for url in ["/history", "/list", "/nodes", "/stats/second"] {
        let path = if url.starts_with("/stats") {
            "/stats/{name}"
        } else {
            url
        };
        checked(&router, "GET", url, path, 200).await;
    }
    let restarted = app(cfg);
    let detail = checked(
        &restarted,
        "GET",
        &format!("/runs/{durable}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    assert_eq!(detail["status"], "success");
    assert_eq!(
        detail["outcome"]["status"], "success",
        "the outcome survives a restart"
    );
    assert!(!detail["steps"].as_array().unwrap().is_empty());
    checked(
        &restarted,
        "GET",
        &format!("/runs/{run_id}/output"),
        "/runs/{run_id}/output",
        200,
    )
    .await;
    checked(
        &restarted,
        "GET",
        &format!("/runs/{run_id}/events"),
        "/runs/{run_id}/events",
        404,
    )
    .await;
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
    let error = checked(&broken_store, "GET", "/runs", "/runs", 500).await;
    assert_eq!(error["reason"], "metadata_unusable", "{error}");
    let health = checked(&broken_store, "GET", "/health", "/health", 200).await;
    assert!(health["metadata_error"].is_string(), "{health}");
}

#[tokio::test]
async fn partial_and_empty_graph_health_and_run_admission_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), false);
    let source = dir.path().join("pipeline.py");
    let broken = dir.path().join("broken.py");
    std::fs::write(&source, "from barca import asset, asset_ref\n@asset(inputs={'value': asset_ref('broken.py:bad')})\ndef blocked(value): return value\n@asset()\ndef safe(): return 7\n").unwrap();
    std::fs::write(
        &broken,
        "from barca import asset\n@asset()\ndef bad(: pass\n",
    )
    .unwrap();
    cfg.files.push(broken.display().to_string());
    let router = app(cfg.clone());
    let assets = checked(&router, "GET", "/assets", "/assets", 200).await;
    assert_eq!(assets.as_array().unwrap().len(), 1);
    assert!(assets[0]["id"].as_str().unwrap().ends_with(":safe"));
    let health = checked(&router, "GET", "/health", "/health", 200).await;
    let errors = health["load_errors"].as_array().unwrap();
    assert!(errors.iter().any(
        |error| error["file"].as_str().unwrap().ends_with("broken.py")
            && error["affected_nodes"].as_array().unwrap().is_empty()
    ));
    assert!(errors.iter().any(|error| {
        error["affected_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id.as_str().unwrap().ends_with(":blocked"))
    }));
    let handle = checked(&router, "POST", "/run", "/run", 200).await;
    assert_eq!(
        wait(&router, handle["run_id"].as_str().unwrap()).await["status"],
        "complete"
    );
    std::fs::write(&source, "def broken(: pass\n").unwrap();
    let empty = app(cfg.clone());
    assert_eq!(
        checked(&empty, "GET", "/assets", "/assets", 200).await,
        json!([])
    );
    assert_eq!(
        checked(&empty, "GET", "/state", "/state", 200).await,
        json!([])
    );
    let health = checked(&empty, "GET", "/health", "/health", 200).await;
    assert_eq!(health["load_errors"].as_array().unwrap().len(), 2);
    let refusal = checked(&empty, "POST", "/run", "/run", 400).await;
    assert!(
        refusal["error"]
            .as_str()
            .unwrap()
            .contains("no loaded assets or sensors")
    );
    std::fs::write(
        &source,
        "from barca import task\n@task()\ndef healthy(): pass\n",
    )
    .unwrap();
    cfg.files = vec![source.display().to_string()];
    let tasks_only = app(cfg);
    let assets = checked(&tasks_only, "GET", "/assets", "/assets", 200).await;
    assert_eq!(assets.as_array().unwrap().len(), 1);
    assert_eq!(assets[0]["kind"], "task");
    let health = checked(&tasks_only, "GET", "/health", "/health", 200).await;
    assert_eq!(health["load_errors"], json!([]));
    checked(&tasks_only, "POST", "/run", "/run", 400).await;
}

#[tokio::test]
async fn run_loader_infrastructure_failure_matches_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), false);
    std::fs::write(dir.path().join("pipeline.py"), "from barca import asset, partitions\n@asset(partitions={'x': partitions(list(range(2)))})\ndef value(x): return x\n").unwrap();
    cfg.python = dir.path().join("unavailable-python");
    let body = checked(&app(cfg), "POST", "/run", "/run", 500).await;
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("cannot evaluate partitions(list(range(2))) of ")
            && error.contains("pipeline.py:value: cannot run"),
        "{error}"
    );
}

#[tokio::test]
async fn declared_asset_name_matches_schema_state_and_get_target() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), false);
    std::fs::write(
        dir.path().join("pipeline.py"),
        "from barca import asset\n@asset(name='published')\ndef implementation(): return 7\n@asset()\ndef ordinary(): return 8\n",
    )
    .unwrap();
    let router = app(cfg);
    let asset = checked(&router, "GET", "/assets/published", "/assets/{name}", 200).await;
    let id = asset["asset"]["id"].as_str().unwrap();
    assert_eq!(id, "published");
    let schema = checked(
        &router,
        "GET",
        "/assets/published/schema",
        "/assets/{name}/schema",
        200,
    )
    .await;
    assert_eq!(schema[0]["id"], id);
    assert_eq!(schema[0]["name"], "published");
    let state = checked(&router, "GET", "/state", "/state", 200).await;
    let named = state
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .unwrap();
    assert_eq!(named["name"], "published");
    let ordinary = state
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "ordinary")
        .unwrap();
    assert!(
        ordinary["id"]
            .as_str()
            .unwrap()
            .ends_with("pipeline.py:ordinary")
    );
    let handle = checked(&router, "POST", "/get/published", "/get/{target}", 200).await;
    assert_eq!(
        wait(&router, handle["run_id"].as_str().unwrap()).await["status"],
        "complete"
    );
}

#[tokio::test]
async fn a_draining_server_queues_runs_for_the_next_start_and_says_so_on_health() {
    let dir = tempfile::tempdir().unwrap();
    let router = draining_app(config(dir.path(), false));
    let health = checked(&router, "GET", "/health", "/health", 200).await;
    assert_eq!(health["status"], "draining");
    // The older trigger routes are accepted: the runs wait in the queue.
    for (url, path) in [
        ("/run", "/run"),
        ("/run/publish", "/run/{target}"),
        ("/get/first", "/get/{target}"),
    ] {
        let body = checked(&router, "POST", url, path, 200).await;
        assert!(body["run_id"].is_string(), "{body}");
    }
    // So is `POST /runs`: 202, pending, with its place in the queue.
    let (status, body) = request_json(
        &router,
        "POST",
        "/runs",
        json!({"command": "get", "targets": ["first"]}),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    validate(
        &contract()["paths"]["/runs"]["post"]["responses"]["202"]["content"]["application/json"]["schema"],
        &body,
    );
    assert_eq!(body["status"], "pending");
    assert_eq!(body["position"], 4);
    let queue = checked(&router, "GET", "/queue", "/queue", 200).await;
    assert_eq!(queue["draining"], true);
    assert_eq!(queue["running"], json!([]));
    let ids: Vec<_> = queue["queued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["position"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [1, 2, 3, 4]);
    assert_eq!(queue["queued"][3]["run_id"], body["run_id"]);
    // Recorded: the next server resumes them.
    let recorded =
        barca_core::run_queue::list(&dir.path().join("metadata.db").display().to_string())
            .await
            .unwrap();
    assert_eq!(recorded.len(), 4);
    let health = checked(&router, "GET", "/health", "/health", 200).await;
    assert_eq!(health["queue"]["queued"], 4);
    assert_eq!(health["queue"]["running"], 0);
    // A reset of the metadata database is still refused.
    let resp = request(&router, "POST", "/state/reset").await;
    assert_eq!(resp.headers()["retry-after"].to_str().unwrap(), "30");
    checked(&router, "POST", "/state/reset", "/state/reset", 503).await;
    // Reads stay available while the runs in flight finish.
    checked(&router, "GET", "/runs?limit=0", "/runs", 200).await;
    // A queued run can be cancelled: it leaves the queue and ends cancelled.
    let id = body["run_id"].as_str().unwrap();
    let cancelled = checked(
        &router,
        "DELETE",
        &format!("/runs/{id}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    assert_eq!(cancelled["status"], "cancelling");
    let run = checked(
        &router,
        "GET",
        &format!("/runs/{id}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    assert_eq!(run["status"], "cancelled", "{run}");
    let queue = checked(&router, "GET", "/queue", "/queue", 200).await;
    assert_eq!(queue["queued"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn queue_pause_and_resume_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let router = app(config(dir.path(), false));
    let queue = checked(&router, "GET", "/queue", "/queue", 200).await;
    assert_eq!(queue["paused"], false);
    assert_eq!(queue["paused_at"], Value::Null);
    checked(&router, "HEAD", "/queue", "/queue", 200).await;
    let paused = checked(&router, "POST", "/queue/pause", "/queue/pause", 200).await;
    assert_eq!(paused["paused"], true);
    assert!(paused["paused_at"].is_string());
    // Pausing again changes nothing.
    let again = checked(&router, "POST", "/queue/pause", "/queue/pause", 200).await;
    assert_eq!(again["paused_at"], paused["paused_at"]);
    // While paused a run is accepted and waits.
    let (status, body) = request_json(
        &router,
        "POST",
        "/runs",
        json!({"command": "get", "targets": ["first"]}),
    )
    .await;
    assert_eq!((status, body["status"].as_str()), (202, Some("pending")));
    assert_eq!(body["position"], 1);
    let health = checked(&router, "GET", "/health", "/health", 200).await;
    assert_eq!(health["queue"]["paused"], true);
    // A server started on the same database keeps the pause and the queued run.
    let db = dir.path().join("metadata.db").display().to_string();
    assert!(
        barca_core::run_queue::paused_at(&db)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(barca_core::run_queue::list(&db).await.unwrap().len(), 1);
    let resumed = checked(&router, "POST", "/queue/resume", "/queue/resume", 200).await;
    assert_eq!(resumed["paused"], false);
    assert!(
        barca_core::run_queue::paused_at(&db)
            .await
            .unwrap()
            .is_none()
    );
    let done = wait(&router, body["run_id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "complete", "{done}");
    assert!(
        barca_core::run_queue::list(&db).await.unwrap().is_empty(),
        "a finished run leaves the queue"
    );
    checked(&router, "POST", "/queue/resume", "/queue/resume", 200).await;
    checked(&router, "GET", "/queue/pause", "/queue/pause", 405).await;
    // A read-only server refuses both and shows what the database records.
    let read_only = app(config(dir.path(), true));
    checked(&read_only, "POST", "/queue/pause", "/queue/pause", 403).await;
    checked(&read_only, "POST", "/queue/resume", "/queue/resume", 403).await;
    let view = checked(&read_only, "GET", "/queue", "/queue", 200).await;
    assert_eq!(view["queued"], json!([]));
}

#[tokio::test]
async fn schedule_pause_and_resume_match_contract() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), false);
    std::fs::write(
        &cfg.files[0],
        "from barca import asset, Schedule\n@asset(freshness=Schedule('0 5 * * *'))\ndef daily() -> int:\n    return 1\n",
    )
    .unwrap();
    let router = app(cfg);
    let paused = checked(
        &router,
        "POST",
        "/schedule/daily/pause",
        "/schedule/{id}/pause",
        200,
    )
    .await;
    assert_eq!(paused["paused"], true);
    // `GET /nodes` carries the schedule; a paused one is a problem.
    let nodes = checked(&router, "GET", "/nodes?problems=1", "/nodes", 200).await;
    let daily = &nodes["nodes"][0];
    assert_eq!(daily["schedule"]["paused"], true, "{nodes}");
    assert_eq!(daily["schedule"]["cron"], "0 5 * * *");
    assert_eq!(nodes["filter"]["problems"], true);
    checked(
        &router,
        "POST",
        "/schedule/daily/pause?until=2999-01-01T00:00:00Z",
        "/schedule/{id}/pause",
        200,
    )
    .await;
    checked(
        &router,
        "POST",
        "/schedule/daily/pause?until=yesterday",
        "/schedule/{id}/pause",
        400,
    )
    .await;
    checked(
        &router,
        "POST",
        "/schedule/missing/pause",
        "/schedule/{id}/pause",
        404,
    )
    .await;
    let resumed = checked(
        &router,
        "POST",
        "/schedule/daily/resume",
        "/schedule/{id}/resume",
        200,
    )
    .await;
    assert_eq!(resumed["paused"], false);
    checked(
        &router,
        "GET",
        "/schedule/daily/pause",
        "/schedule/{id}/pause",
        405,
    )
    .await;
    checked(&router, "PUT", "/runs", "/runs", 405).await;
    checked(&router, "PUT", "/runs/x", "/runs/{run_id}", 405).await;
}

#[tokio::test]
async fn ledger_lookup_and_post_match_contract() {
    use sha2::Digest;
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path(), false);
    config.resolved.artifact_root = dir
        .path()
        .join("store/default/artifacts")
        .display()
        .to_string();
    config.resolved.local_artifact_dir = dir.path().join("local").display().to_string();
    let router = barca_server::app(config.clone());
    let response = |path: &str, status: u16| {
        contract()["paths"][path]["post"]["responses"][status.to_string()]["content"]["application/json"]
            ["schema"]
            .clone()
    };
    let lookup = json!({"hashes": [{"node_id": "pipeline.py:first", "run_hash": "h1"}]});
    validate(
        &contract()["paths"]["/ledger/lookup"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"],
        &lookup,
    );
    let (status, body) = request_json(&router, "POST", "/ledger/lookup", lookup.clone()).await;
    assert_eq!(status, 200, "{body}");
    validate(&response("/ledger/lookup", 200), &body);
    let (status, body) =
        request_json(&router, "POST", "/ledger/lookup", json!({"hashes": 1})).await;
    assert_eq!(status, 400);
    validate(&response("/ledger/lookup", 400), &body);

    let bytes = br#"{"n": 1}"#;
    let object = Path::new(&config.resolved.artifact_root).join("first/h1.json");
    let post = json!({
        "run": {"run_id": "c-1", "command": "get", "files": ["pipeline.py"], "target": "first",
                "status": "success", "steps_total": 1, "steps_executed": 1, "steps_cached": 0,
                "started_at": "2026-10-20 12:00:00", "finished_at": "2026-10-20 12:00:01",
                "elapsed_seconds": 1.0, "host": "laptop"},
        "steps": [{"node_id": "pipeline.py:first", "run_hash": "h1", "status": "success",
                   "path": object.display().to_string(), "format": "json",
                   "size_bytes": bytes.len(), "digest": format!("{:x}", sha2::Sha256::digest(bytes)),
                   "elapsed_seconds": 0.1, "attempts": 1, "error": null}]
    });
    validate(
        &contract()["paths"]["/ledger/runs"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"],
        &post,
    );
    let (status, body) = request_json(&router, "POST", "/ledger/runs", post.clone()).await;
    assert_eq!(status, 409, "{body}");
    validate(&response("/ledger/runs", 409), &body);
    std::fs::create_dir_all(object.parent().unwrap()).unwrap();
    std::fs::write(&object, bytes).unwrap();
    let (status, body) = request_json(&router, "POST", "/ledger/runs", post.clone()).await;
    assert_eq!(status, 201, "{body}");
    validate(&response("/ledger/runs", 201), &body);
    let (status, body) = request_json(&router, "POST", "/ledger/lookup", lookup).await;
    assert_eq!(status, 200);
    validate(&response("/ledger/lookup", 200), &body);
    assert_eq!(body["results"][0]["exists"], true);
    let (status, body) = request_json(&router, "POST", "/ledger/runs", json!([])).await;
    assert_eq!(status, 400);
    validate(&response("/ledger/runs", 400), &body);
    // History rows carry client_run_id.
    let history = checked(&router, "GET", "/history?limit=5", "/history", 200).await;
    assert_eq!(history["runs"][0]["client_run_id"], "c-1");
    assert_eq!(history["runs"][0]["triggered_by"], "client");

    let mut read_only = config.clone();
    read_only.read_only = true;
    let (status, body) =
        request_json(&barca_server::app(read_only), "POST", "/ledger/runs", post).await;
    assert_eq!(status, 403);
    validate(&response("/ledger/runs", 403), &body);
    checked(&router, "GET", "/ledger/runs", "/ledger/runs", 405).await;
}

/// The files in `dir` whose name starts with `metadata.db.reset-`.
fn moved_aside(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("metadata.db.reset-"))
        .collect();
    names.sort();
    names
}

async fn run_count(router: &Router) -> usize {
    checked(router, "GET", "/runs?limit=100", "/runs", 200).await["runs"]
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn state_reset_moves_the_database_aside_refuses_during_a_run_and_mends_a_broken_one() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), false);
    let router = app(cfg.clone());
    let handle = checked(&router, "POST", "/get/first", "/get/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(wait(&router, &handle).await["status"], "complete");
    assert_eq!(run_count(&router).await, 1);

    // Refused while a run is in flight; nothing moves.
    let slow = checked(&router, "POST", "/run/slow", "/run/{target}", 200).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused = checked(&router, "POST", "/state/reset", "/state/reset", 409).await;
    assert!(
        refused["error"].as_str().unwrap().contains(&slow),
        "the error names the run: {refused}"
    );
    assert!(moved_aside(dir.path()).is_empty());
    checked(
        &router,
        "DELETE",
        &format!("/runs/{slow}"),
        "/runs/{run_id}",
        200,
    )
    .await;
    assert_eq!(wait(&router, &slow).await["status"], "cancelled");

    // The database moves aside and the next request sees an empty history.
    let reset = checked(&router, "POST", "/state/reset", "/state/reset", 200).await;
    let moved = reset["moved_to"].as_str().unwrap().to_owned();
    assert!(Path::new(&moved).is_file(), "{reset}");
    assert_eq!(reset["database"], cfg.resolved.db_path.as_str());
    assert!(
        reset["undo"]
            .as_str()
            .unwrap()
            .starts_with("barca state restore --from "),
        "{reset}"
    );
    assert_eq!(
        moved_aside(dir.path()).first().map(String::as_str),
        Path::new(&moved).file_name().and_then(|n| n.to_str())
    );
    assert_eq!(run_count(&router).await, 0);
    let state = checked(&router, "GET", "/state", "/state", 200).await;
    let first = state
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "first")
        .unwrap();
    assert!(first["last_materialization"].is_null(), "{first}");
    assert_eq!(barca_core::db::count_runs(&moved).await.unwrap(), 2);

    // The moved file restores (with the server stopped, as the CLI says).
    let restored = barca_core::backup::restore(&cfg.python, &cfg.resolved, Some(moved.as_str()))
        .await
        .unwrap();
    assert_eq!(restored.from, "file");
    assert_eq!(run_count(&router).await, 2);

    // A database that cannot be read: `/state` fails, a reset mends it.
    std::fs::write(&cfg.resolved.db_path, b"this is not a database").unwrap();
    let _ = std::fs::remove_file(format!("{}-wal", cfg.resolved.db_path));
    let broken = request(&router, "GET", "/runs?limit=1").await;
    assert_eq!(broken.status().as_u16(), 500);
    let reset = checked(&router, "POST", "/state/reset", "/state/reset", 200).await;
    let moved = reset["moved_to"].as_str().unwrap();
    assert_eq!(std::fs::read(moved).unwrap(), b"this is not a database");
    assert_eq!(run_count(&router).await, 0);
    checked(&router, "GET", "/state", "/state", 200).await;
    checked(&router, "GET", "/state/reset", "/state/reset", 405).await;
}
