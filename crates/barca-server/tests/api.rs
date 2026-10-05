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
    }
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
