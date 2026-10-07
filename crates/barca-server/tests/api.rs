//! Smoke tests for the HTTP API. These drive the router directly via
//! `tower::ServiceExt::oneshot` (no socket bind). They cover the pure
//! static-analysis endpoints (/health, /plan, /assets) and the 404 path, which
//! do not require a Python execution environment.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use barca_server::{ServeConfig, app};
use std::io::Write;
use tower::ServiceExt; // for `oneshot`

const FIXTURE: &str = r#"
from barca import asset

@asset()
def first() -> dict:
    return {"n": 1}

@asset(inputs={"first": first})
def second(first: dict) -> dict:
    return {"n": first["n"] + 1}
"#;

/// Write the fixture module to a temp dir and build a config pointing at it.
fn fixture_config(dir: &std::path::Path) -> ServeConfig {
    let path = dir.join("pipeline.py");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(FIXTURE.as_bytes()).unwrap();
    ServeConfig {
        files: vec![path.display().to_string()],
        host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "local".to_string(),
        python: barca_core::commands::find_python(),
        resolved: barca_core::config::resolve_in(None, dir).unwrap(),
        read_only: false,
    }
}

/// Like [`fixture_config`], with the DB pointed into the temp dir so tests can
/// assert whether it was created.
fn isolated_config(dir: &std::path::Path, read_only: bool) -> ServeConfig {
    let mut config = fixture_config(dir);
    config.resolved.db_path = dir.join("metadata.db").display().to_string();
    config.resolved.artifact_root = dir.join("artifacts").display().to_string();
    config.read_only = read_only;
    config
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_reports_ok_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "ok");
    assert!(json["version"].is_string());
}

#[tokio::test]
async fn schedule_endpoint_returns_json_array() {
    // `app()` does not spawn the scheduler, so the registry is empty — this
    // asserts the route is wired and returns a well-formed (empty) array.
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/schedule")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn assets_lists_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let assets = json.as_array().expect("array of assets");
    assert_eq!(assets.len(), 2);
    let ids: Vec<&str> = assets.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(ids.iter().any(|id| id.ends_with(":first")));
    assert!(ids.iter().any(|id| id.ends_with(":second")));
    // `second` depends on `first`.
    let second = assets
        .iter()
        .find(|a| a["id"].as_str().unwrap().ends_with(":second"))
        .unwrap();
    assert_eq!(second["inputs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn plan_returns_phases() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(Request::builder().uri("/plan").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["total_steps"], 2);
    // Plan warnings (`barca docs contract`): always an array, empty for this pipeline.
    assert_eq!(json["warnings"], serde_json::json!([]));
    assert!(json["phases"].is_array());
}

#[tokio::test]
async fn unknown_asset_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets/does_not_exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// An asset that sleeps far longer than the test is allowed to take — the only
/// way the test finishes quickly is if cancellation genuinely stops the run.
const SLOW_FIXTURE: &str = r#"
import time
from barca import asset

@asset()
def slow_one() -> dict:
    time.sleep(120)
    return {"done": True}
"#;

/// Send a request to a clone of the router and return (status, parsed body).
async fn send(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

/// Count live `barca._worker` processes whose environment carries `marker`
/// (this test's unique artifact root), so concurrent tests can't interfere.
#[cfg(target_os = "linux")]
fn workers_running(marker: &str) -> usize {
    let mut n = 0;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    for e in entries.flatten() {
        let pid = e.file_name();
        let Some(pid) = pid
            .to_str()
            .filter(|p| p.bytes().all(|c| c.is_ascii_digit()))
        else {
            continue;
        };
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        if !environ
            .windows(marker.len())
            .any(|w| w == marker.as_bytes())
        {
            continue;
        }
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let needle = b"barca._worker";
        if cmdline.windows(needle.len()).any(|w| w == needle) {
            n += 1;
        }
    }
    n
}

/// The review-critical path for #79: a run started over HTTP must be
/// cancellable mid-flight — status transitions to `cancelled` long before the
/// asset's sleep elapses, and the run's worker process is terminated.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn delete_cancels_in_flight_run() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let slow = dir.path().join("slow.py");
    std::fs::write(&slow, SLOW_FIXTURE).unwrap();

    // Worker children import `barca._worker`; a python wrapper injects the repo
    // checkout's python/ tree so plain `cargo test` works without an installed
    // wheel (CI runs unit tests before building one).
    let py_tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    let wrapper = dir.path().join("python");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nPYTHONPATH=\"{}${{PYTHONPATH:+:$PYTHONPATH}}\" exec python3 \"$@\"\n",
            py_tree.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    // The run derives its local `.barca` scaffolding from the process cwd; note
    // whether it pre-existed so this test can clean up what it caused.
    let scaffold = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".barca");
    let scaffold_preexisting = scaffold.exists();

    let mut config = fixture_config(dir.path());
    config.files = vec![slow.display().to_string()];
    config.python = wrapper;
    // Absolute tempdir paths so the run's DB and artifacts never land in the repo.
    config.resolved.db_path = dir.path().join("metadata.db").display().to_string();
    // Both: a local artifact dir and a store root that differ would make the
    // run sync through the transfer helper (and workers would get the local
    // dir, not the marker below).
    config.resolved.artifact_root = dir.path().join("artifacts").display().to_string();
    config.resolved.local_artifact_dir = config.resolved.artifact_root.clone();
    let marker = config.resolved.artifact_root.clone();
    std::fs::create_dir_all(&marker).unwrap();
    let app = app(config);

    let t0 = Instant::now();
    let (status, body) = send(&app, "POST", "/get/slow_one").await;
    assert_eq!(status, StatusCode::OK);
    let handle = body["run_id"].as_str().expect("run handle").to_string();
    let status_uri = format!("/status/{handle}");

    // Wait until the run is executing (and, on Linux, its worker is alive).
    loop {
        let (_, s) = send(&app, "GET", &status_uri).await;
        match s["status"].as_str() {
            Some("running") => {
                #[cfg(target_os = "linux")]
                if workers_running(&marker) == 0 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                break;
            }
            Some("pending") => {}
            other => panic!("unexpected status before cancel: {other:?}"),
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "run never started executing"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (status, body) = send(&app, "DELETE", &format!("/run/{handle}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "cancelling");

    loop {
        let (_, s) = send(&app, "GET", &status_uri).await;
        match s["status"].as_str() {
            Some("cancelled") => {
                assert_eq!(s["error"], "run cancelled");
                break;
            }
            Some("running" | "pending") => {}
            other => panic!("expected cancelled, got {other:?}"),
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "run did not reach cancelled after DELETE"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The asset sleeps 120s; reaching `cancelled` this fast proves the run was
    // stopped mid-flight rather than left to finish in the background.
    assert!(
        t0.elapsed() < Duration::from_secs(60),
        "cancellation took {:?} — run was not stopped mid-flight",
        t0.elapsed()
    );

    // The run's worker process must be terminated, not orphaned.
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if workers_running(&marker) == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker process still alive after cancellation"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Cancelling a finished run is a conflict, not a repeat cancel.
    let (status, _) = send(&app, "DELETE", &format!("/run/{handle}")).await;
    assert_eq!(status, StatusCode::CONFLICT);

    if !scaffold_preexisting {
        std::fs::remove_dir_all(&scaffold).ok();
    }
}

#[tokio::test]
async fn cancel_for_unknown_run_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/run/deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_for_unknown_run_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/status/deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn events_for_unknown_run_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/events/deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn logs_for_unknown_run_returns_empty() {
    // Unknown run id → empty list, not an error: logs are durable history and
    // "no rows" is a valid answer.
    let dir = tempfile::tempdir().unwrap();
    let app = app(fixture_config(dir.path()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/logs/deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["logs"], serde_json::json!([]));
}

#[tokio::test]
async fn state_reports_every_node_without_creating_a_db() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), false));
    let (status, json) = send(&app, "GET", "/state").await;
    assert_eq!(status, StatusCode::OK);
    let nodes = json.as_array().expect("array of node states");
    assert_eq!(nodes.len(), 2);
    for n in nodes {
        assert_eq!(n["cache"]["state"], "never_run", "{n}");
        assert!(n["last_materialization"].is_null());
        assert!(n["durations"].is_null());
        assert!(n["next_run"].is_null(), "unscheduled: {n}");
    }
    assert!(
        !dir.path().join("metadata.db").exists(),
        "GET /state created the DB"
    );
}

#[tokio::test]
async fn read_only_rejects_everything_that_runs() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), true));
    for (method, uri) in [
        ("POST", "/run"),
        ("POST", "/run/second"),
        ("POST", "/get/second"),
        ("DELETE", "/run/deadbeef"),
    ] {
        let (status, json) = send(&app, method, uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        assert!(
            json["error"].as_str().unwrap_or("").contains("read-only"),
            "{method} {uri}: {json}"
        );
    }
    let (_, health) = send(&app, "GET", "/health").await;
    assert_eq!(health["read_only"], true);
    // A read-only server never schedules, whatever --no-schedule says.
    assert_eq!(health["scheduler"], false);
}

#[tokio::test]
async fn read_only_reads_never_create_or_touch_the_db() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), true));
    for uri in ["/state", "/assets/first", "/logs/deadbeef"] {
        let (status, _) = send(&app, "GET", uri).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}");
    }
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("metadata.db"))
        .collect();
    assert!(entries.is_empty(), "read-only reads created {entries:?}");
}

#[tokio::test]
async fn health_reports_writable_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), false));
    let (_, health) = send(&app, "GET", "/health").await;
    assert_eq!(health["read_only"], false);
    // fixture_config sets `schedule: false` (as `--no-schedule` would).
    assert_eq!(health["scheduler"], false);
}

#[tokio::test]
async fn health_reports_the_scheduler_when_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = isolated_config(dir.path(), false);
    config.schedule = true;
    let app = app(config);
    let (_, health) = send(&app, "GET", "/health").await;
    assert_eq!(health["scheduler"], true);
}

#[tokio::test]
async fn the_root_and_ui_redirect_relatively_to_the_ui() {
    // Relative `Location`s keep a reverse-proxy prefix: from /barca/ the
    // browser resolves `ui/` to /barca/ui/.
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), false));
    for uri in ["/", "/ui"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(
            resp.status().is_redirection(),
            "GET {uri}: {}",
            resp.status()
        );
        assert_eq!(resp.headers()["location"], "ui/", "GET {uri}");
    }
}

#[tokio::test]
async fn ui_page_is_served_or_explains_it_was_not_built() {
    // Whether `ui/dist` was built before this test binary decides which; both
    // are valid, a bare 404 or an API error is not.
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), false));
    let resp = app
        .oneshot(Request::builder().uri("/ui/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let ctype = resp.headers().get("content-type").cloned();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body);
    if status == StatusCode::OK {
        assert_eq!(ctype.unwrap(), "text/html; charset=utf-8");
        assert!(body.contains("<div id=\"root\">"), "{body}");
    } else {
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("built without its web UI"), "{body}");
    }
}

#[tokio::test]
async fn run_events_are_not_buffered_by_proxies() {
    // Start a run (it fails fast: the target doesn't exist) so a live event
    // channel exists, then check the SSE response's headers.
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), false));
    let (status, body) = send(&app, "POST", "/get/does_not_exist").await;
    assert_eq!(status, StatusCode::OK);
    let handle = body["run_id"].as_str().unwrap().to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/events/{handle}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-accel-buffering"], "no");
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
}

#[tokio::test]
async fn schema_reports_the_target_and_direct_inputs_without_creating_a_db() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(isolated_config(dir.path(), true));
    let (status, json) = send(&app, "GET", "/assets/second/schema").await;
    assert_eq!(status, StatusCode::OK);
    let nodes = json.as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["name"], "first");
    assert_eq!(nodes[1]["name"], "second");
    assert!(nodes.iter().all(|n| n["shape"].is_null()));
    assert!(!dir.path().join("metadata.db").exists());
    let (status, _) = send(&app, "GET", "/assets/missing/schema").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn schema_excludes_indirect_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    let config = isolated_config(dir.path(), true);
    let mut fixture = FIXTURE.to_string();
    fixture
        .push_str("\n@asset(inputs={\"second\": second})\ndef third(second):\n    return second\n");
    std::fs::write(&config.files[0], fixture).unwrap();
    let app = app(config);
    let (status, json) = send(&app, "GET", "/assets/third/schema").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["second", "third"]);
}

#[tokio::test]
async fn schema_reads_materialized_columns_and_reports_missing_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = isolated_config(dir.path(), true);
    // Run the repository's inspector with system Python, without requiring an
    // installed wheel or mutating the process-wide environment in parallel tests.
    let launcher = dir.path().join("inspect-python");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    std::fs::write(&launcher, format!(
        "#!/usr/bin/env python3\nimport sys, runpy\nsys.path.insert(0, {})\nrunpy.run_module(sys.argv[2], run_name='__main__')\n",
        serde_json::to_string(&source.display().to_string()).unwrap()
    )).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    config.python = launcher;

    let summaries = barca_core::commands::list_assets(&config.files, &config.python)
        .await
        .unwrap();
    let first = summaries.iter().find(|n| n.id.ends_with(":first")).unwrap();
    let artifact = dir.path().join("rows.json");
    std::fs::write(
        &artifact,
        r#"[{"order_id":1,"region":"East"},{"order_id":2,"region":null}]"#,
    )
    .unwrap();
    barca_core::db::init_db(&config.resolved.db_path)
        .await
        .unwrap();
    let outputs = std::collections::HashMap::from([(
        first.id.clone(),
        barca_core::dispatch::OutputRef {
            path: artifact.display().to_string(),
            format: "json".into(),
            size_bytes: 64,
            elapsed_seconds: None,
            content_hash: None,
        },
    )]);
    barca_core::db::persist_outputs(&config.resolved.db_path, &outputs, &Default::default())
        .await
        .unwrap();
    let object_path = dir.path().join("object.json");
    std::fs::write(&object_path, r#"{"count":4,"enabled":true,"nothing":null}"#).unwrap();
    let second = summaries
        .iter()
        .find(|n| n.id.ends_with(":second"))
        .unwrap();
    let output = barca_core::dispatch::OutputRef {
        path: object_path.display().to_string(),
        format: "json".into(),
        size_bytes: 41,
        elapsed_seconds: None,
        content_hash: None,
    };
    barca_core::db::persist_outputs(
        &config.resolved.db_path,
        &std::collections::HashMap::from([(second.id.clone(), output)]),
        &Default::default(),
    )
    .await
    .unwrap();
    let app = app(config);
    let (status, json) = send(&app, "GET", "/assets/second/schema").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json[1]["shape"]["columns"],
        serde_json::json!([
            {"name":"count","type":"int"}, {"name":"enabled","type":"bool"}, {"name":"nothing","type":"null"}
        ])
    );
    assert_eq!(json[0]["shape"]["rows"], 2);
    assert_eq!(
        json[0]["shape"]["columns"][0],
        serde_json::json!({"name":"order_id","type":"int"})
    );
    assert_eq!(json[0]["shape"]["columns"][1]["type"], "str | null");
    std::fs::remove_file(artifact).unwrap();
    let (_, json) = send(&app, "GET", "/assets/second/schema").await;
    assert_eq!(json[0]["shape"]["note"], "artifact file not found");
}
