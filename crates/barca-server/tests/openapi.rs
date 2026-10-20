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
    let event = validator(&json!({"$ref":"#/components/schemas/BytesChange"}));
    assert!(event.is_valid(&json!({"node_id":"p.py:a", "run_hash":"h", "content_hash":"digest", "created_at":"2026-10-15 00:00:00"})));
    assert!(!event.is_valid(
        &json!({"node_id":"p.py:a", "run_hash":"h", "created_at":"2026-10-15 00:00:00"})
    ));
    let health = validator(&json!({"$ref":"#/components/schemas/Health"}));
    let ok = json!({"status":"ok","version":"1","api_version":1,"env":"default","store_uri":null,"read_only":false,"scheduler":false,"load_errors":[]});
    assert!(health.is_valid(&ok));
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
    assert!(!request.is_valid(&json!({"command":"sql"})));
    assert!(!request.is_valid(&json!({"command":"get","invented":1})));
    let load_error = validator(&json!({"$ref":"#/components/schemas/LoadError"}));
    assert!(!load_error.is_valid(&json!({"file":"p.py","error":"broken"})));
    assert!(!load_error.is_valid(&json!({"file":"p.py","error":"broken","affected_nodes":[1]})));
    for event in [
        barca_core::RunEvent::RunStarted { run_id: "r".into() },
        barca_core::RunEvent::Log {
            node_id: "p.py:a".into(),
            line: "hello".into(),
        },
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
        ("/history?limit=5", "/history"),
        ("/history?all=1", "/history"),
        ("/stats/first", "/stats/{name}"),
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
    for (method, url, path) in [
        ("POST", "/run", "/run"),
        ("POST", "/run/publish", "/run/{target}"),
        ("POST", "/get/first", "/get/{target}"),
        ("DELETE", "/run/missing", "/run/{target}"),
        ("DELETE", "/runs/missing", "/runs/{run_id}"),
        ("POST", "/schedule/missing/pause", "/schedule/{id}/pause"),
        ("POST", "/schedule/missing/resume", "/schedule/{id}/resume"),
    ] {
        checked(&router, method, url, path, 403).await;
    }
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
    validate(&contract()["components"]["schemas"]["RunHandle"], &accepted);
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
    checked(&broken_store, "GET", "/runs", "/runs", 500).await;
    checked(&broken_store, "GET", "/health", "/health", 200).await;
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
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("DAG analysis task failed")
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
