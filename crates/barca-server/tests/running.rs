//! #534: `GET /state`, `GET /assets/{name}` and `GET /runs?status=running` show what is running
//! now, whoever started it: a `POST /runs` run and a scheduler tick here; a CLI run on the same
//! `.barca/` (another process) in `python/tests/test_serve_running.py`. Each shows within one
//! poll and clears when it ends.
#![cfg(unix)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use barca_server::ServeConfig;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};
use tower::ServiceExt;

const FIXTURE: &str = r#"
import time
from barca import asset, partitions, Schedule

@asset()
def first() -> dict:
    return {"n": 1}

@asset(inputs={"first": first})
def slow(first: dict) -> dict:
    time.sleep(4)
    return {"n": first["n"] + 1}

@asset(inputs={"slow": slow})
def after(slow: dict) -> dict:
    return slow

@asset(partitions={"k": partitions(["a", "b", "c", "d"])})
def keyed(k: str) -> str:
    time.sleep(1.5)
    return k

@asset(freshness=Schedule("* * * * * *"))
def ticking() -> dict:
    time.sleep(120)
    return {"done": True}
"#;

/// A served project in `dir` whose workers import this checkout's python/ tree.
fn config(dir: &Path, schedule: bool) -> ServeConfig {
    use std::os::unix::fs::PermissionsExt;
    let source = dir.join("pipeline.py");
    std::fs::write(&source, FIXTURE).unwrap();
    let py_tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    let wrapper = dir.join("python");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nPYTHONPATH=\"{}${{PYTHONPATH:+:$PYTHONPATH}}\" exec python3 \"$@\"\n",
            py_tree.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
    resolved.db_path = dir.join("metadata.db").display().to_string();
    resolved.artifact_root = dir.join("artifacts").display().to_string();
    resolved.local_artifact_dir = resolved.artifact_root.clone();
    std::fs::create_dir_all(&resolved.artifact_root).unwrap();
    ServeConfig {
        files: vec![source.display().to_string()],
        host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        port: 0,
        watch: false,
        schedule,
        timezone: "utc".into(),
        python: wrapper,
        resolved,
        read_only: false,
    }
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// The `running` field of node `name` in `GET /state`.
async fn running_of(app: &axum::Router, name: &str) -> Value {
    let (status, nodes) = send(app, "GET", "/state", None).await;
    assert_eq!(status, StatusCode::OK, "{nodes}");
    nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == name)
        .unwrap_or_else(|| panic!("no node {name}: {nodes}"))["running"]
        .clone()
}

/// Poll `GET /state` until `check` accepts node `name`'s `running` field.
async fn wait_for(
    app: &axum::Router,
    name: &str,
    what: &str,
    check: impl Fn(&Value) -> bool,
) -> Value {
    let t0 = Instant::now();
    loop {
        let running = running_of(app, name).await;
        if check(&running) {
            return running;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "{name} never {what}: {running}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Every entry of `GET /runs?status=running`.
async fn in_flight(app: &axum::Router) -> Vec<Value> {
    let (status, body) = send(app, "GET", "/runs?status=running", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["runs"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_runs_run_shows_as_running_with_its_steps_and_clears_when_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let app = barca_server::app(config(dir.path(), false));
    let (status, body) = send(
        &app,
        "POST",
        "/runs",
        Some(json!({"command": "get", "targets": ["after"]})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();

    // `first` finishes at once; `slow` then runs for seconds, and `after` waits for it.
    let running = wait_for(&app, "slow", "ran", |r| r["step"] == "running").await;
    let after = running_of(&app, "after").await;
    assert_eq!(after["step"], "queued", "{after}");
    assert_eq!(after["run_id"], run_id.as_str(), "{after}");
    assert_eq!(running["run_id"], run_id.as_str(), "{running}");
    assert_eq!(running["triggered_by"], "api", "{running}");
    assert!(running["partitions"].is_null(), "{running}");
    assert!(
        running["started_at"].as_str().unwrap().ends_with('Z'),
        "{running}"
    );
    // Nodes the run does not touch are not running.
    assert!(running_of(&app, "keyed").await.is_null());

    // The node detail says the same.
    let (status, detail) = send(&app, "GET", "/assets/slow", None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["running"]["run_id"], run_id.as_str(), "{detail}");

    // The in-flight list has the run with the node it is working on.
    let runs = in_flight(&app).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["id"], run_id.as_str());
    assert_eq!(runs[0]["status"], "running");
    let steps = runs[0]["active_steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s["node_id"].as_str().unwrap().ends_with(":slow") && s["step"] == "running"),
        "{steps:?}"
    );
    // The plain history has no `active_steps`.
    let (_, history) = send(&app, "GET", "/runs", None).await;
    assert!(
        history["runs"][0].get("active_steps").is_none(),
        "{history}"
    );

    wait_for(&app, "after", "cleared", Value::is_null).await;
    // A step finishing clears its progress before the run persists its outcome and
    // leaves the queue. Observe run-level clearing on its own poll boundary too.
    let finishing = Instant::now();
    loop {
        let runs = in_flight(&app).await;
        if runs.is_empty() {
            break;
        }
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0]["id"], run_id.as_str(), "{runs:?}");
        assert!(
            finishing.elapsed() < Duration::from_secs(60),
            "run never cleared: {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (status, finished) = send(&app, "GET", &format!("/runs/{run_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{finished}");
    assert_eq!(finished["status"], "success", "{finished}");
    assert_eq!(finished["outcome"]["status"], "success", "{finished}");
    assert!(running_of(&app, "slow").await.is_null());
    let (_, detail) = send(&app, "GET", "/assets/slow", None).await;
    assert!(detail["running"].is_null(), "{detail}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partitioned_node_shows_its_keys_done() {
    let dir = tempfile::tempdir().unwrap();
    let app = barca_server::app(config(dir.path(), false));
    let (status, body) = send(
        &app,
        "POST",
        "/runs",
        Some(json!({"command": "get", "targets": ["keyed"]})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let first = wait_for(&app, "keyed", "ran", |r| r["step"] == "running").await;
    assert_eq!(first["partitions"]["total"], 4, "{first}");
    let done = |r: &Value| r["partitions"]["done"].as_u64().unwrap_or(0);
    assert!(done(&first) < 4, "{first}");
    // Keys finish one after another (or a few at a time): some are done before all are.
    let partway = wait_for(&app, "keyed", "made progress", |r| {
        r.is_null() || (1..4).contains(&done(r))
    })
    .await;
    if !partway.is_null() {
        assert_eq!(
            partway["partitions"]["running"].as_u64().unwrap() + done(&partway),
            4,
            "{partway}"
        );
    }
    wait_for(&app, "keyed", "cleared", Value::is_null).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scheduler_run_shows_as_running_and_clears_when_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (app, start_scheduler) = barca_server::app_with_scheduler(config(dir.path(), true));
    let scheduler = start_scheduler();

    let running = wait_for(&app, "ticking", "ran", |r| !r.is_null()).await;
    assert_eq!(running["triggered_by"], "schedule", "{running}");
    let run_id = running["run_id"].as_str().unwrap().to_string();
    let runs = in_flight(&app).await;
    assert!(runs.iter().any(|r| r["id"] == run_id.as_str()), "{runs:?}");

    // Stop the scheduler, so no next tick starts another run, then end this one.
    scheduler.abort();
    let _ = scheduler.await;
    let (status, body) = send(&app, "DELETE", &format!("/runs/{run_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_for(&app, "ticking", "cleared", Value::is_null).await;
    assert!(in_flight(&app).await.is_empty());
}
