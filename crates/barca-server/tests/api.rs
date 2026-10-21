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

/// Invalid source is isolated from inspection, with its actual diagnostic on health.
#[tokio::test]
async fn an_unknown_decorator_argument_is_reported_without_fake_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let config = fixture_config(dir.path());
    std::fs::write(
        &config.files[0],
        "from barca import asset\n\n@asset(after=None)\ndef first(): return {}\n",
    )
    .unwrap();
    let app = app(config);
    let (status, assets) = send(&app, "GET", "/assets").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(assets, serde_json::json!([]));
    let (status, plan) = send(&app, "GET", "/plan").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["total_steps"], 0);
    let (_, health) = send(&app, "GET", "/health").await;
    assert!(
        health["load_errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("first (line 3): `after` is not an argument of @asset")
    );
    assert_eq!(
        health["load_errors"][0]["affected_nodes"],
        serde_json::json!([])
    );
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

/// A scheduled asset that ticks every second and sleeps far longer than the test takes.
const SLOW_SCHEDULED_FIXTURE: &str = r#"
import time
from barca import asset, Schedule

@asset(freshness=Schedule("* * * * * *"))
def slow_tick() -> dict:
    time.sleep(120)
    return {"done": True}
"#;

/// #432: a run started over HTTP is in flight for the scheduler too. A due tick of the same
/// node is skipped (no second run), and the skip is reported by `GET /schedule` with the
/// handle of the run that caused it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_due_tick_is_skipped_while_a_run_started_over_http_is_in_flight() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("ticks.py");
    std::fs::write(&source, SLOW_SCHEDULED_FIXTURE).unwrap();
    // As in `delete_cancels_in_flight_run`: workers import this checkout's python/ tree.
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
    let scaffold = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".barca");
    let scaffold_preexisting = scaffold.exists();

    let mut config = isolated_config(dir.path(), false);
    config.files = vec![source.display().to_string()];
    config.python = wrapper;
    config.schedule = true;
    config.timezone = "utc".to_string();
    config.resolved.local_artifact_dir = config.resolved.artifact_root.clone();
    std::fs::create_dir_all(&config.resolved.artifact_root).unwrap();
    let (app, start_scheduler) = barca_server::app_with_scheduler(config);

    // The run is started by hand, under the short name, before the scheduler has ever ticked.
    let (status, body) = send(&app, "POST", "/get/slow_tick").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handle = body["run_id"].as_str().expect("run handle").to_string();
    let scheduler = start_scheduler();

    // The job ticks every second. Wait for a tick to be recorded as skipped.
    let t0 = Instant::now();
    let entry = loop {
        let (_, schedule) = send(&app, "GET", "/schedule").await;
        if let Some(entry) = schedule
            .as_array()
            .and_then(|jobs| jobs.first())
            .filter(|job| !job["last_skipped"].is_null())
        {
            break entry.clone();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "no tick was skipped: {schedule}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        entry["id"].as_str().unwrap().ends_with(":slow_tick"),
        "{entry}"
    );
    assert_eq!(entry["skipped_for"], handle.as_str(), "{entry}");
    assert!(
        entry["last_run"].is_null(),
        "the scheduler started a second run: {entry}"
    );

    // Stop the scheduler before the run, so the next tick cannot start one, then cancel it.
    scheduler.abort();
    let _ = scheduler.await;
    let (status, _) = send(&app, "DELETE", &format!("/run/{handle}")).await;
    assert_eq!(status, StatusCode::OK);
    loop {
        let (_, s) = send(&app, "GET", &format!("/status/{handle}")).await;
        if s["status"] == "cancelled" {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "run did not stop: {s}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if !scaffold_preexisting {
        std::fs::remove_dir_all(&scaffold).ok();
    }
}

/// A fixture with an asset, a task and (in a second file) a second asset named like the first.
fn trigger_config(dir: &std::path::Path) -> ServeConfig {
    let mut config = isolated_config(dir, false);
    std::fs::write(
        &config.files[0],
        "from barca import asset, task\n\n\
         @asset()\ndef first() -> int:\n    return 1\n\n\
         @task()\ndef publish() -> None:\n    pass\n",
    )
    .unwrap();
    let other = dir.join("other.py");
    std::fs::write(
        &other,
        "from barca import asset\n\n@asset()\ndef first() -> int:\n    return 2\n",
    )
    .unwrap();
    config.files.push(other.display().to_string());
    config
}

#[tokio::test]
async fn a_trigger_with_a_target_that_cannot_run_is_refused_before_a_run_starts() {
    // These used to answer 200 with a run id; the failure only showed in /status (#289).
    let dir = tempfile::tempdir().unwrap();
    let app = app(trigger_config(dir.path()));
    for (uri, want, says) in [
        (
            "/get/nope",
            StatusCode::NOT_FOUND,
            "Asset 'nope' not found. Available: ",
        ),
        (
            "/run/nope",
            StatusCode::NOT_FOUND,
            "Asset 'nope' not found. Available: ",
        ),
        (
            "/get/first",
            StatusCode::CONFLICT,
            "'first' matches more than one node: ",
        ),
        (
            "/get/publish",
            StatusCode::BAD_REQUEST,
            "'publish' is a task: use POST /run/publish",
        ),
        (
            "/run/other.py:first",
            StatusCode::BAD_REQUEST,
            "'other.py:first' is an asset: use POST /get/other.py:first",
        ),
    ] {
        let (status, body) = send(&app, "POST", uri).await;
        assert_eq!(status, want, "{uri}: {body}");
        let error = body["error"].as_str().expect("an error message");
        assert!(error.starts_with(says), "{uri}: {error}");
        assert!(body.get("run_id").is_none(), "{uri} started a run: {body}");
    }
    // Nothing ran: a run would have created the metadata DB.
    assert!(!dir.path().join("metadata.db").exists());
}

#[tokio::test]
async fn a_trigger_is_checked_against_the_source_as_it_is_now() {
    // The nodes are kept between requests, but only while the source files are unchanged:
    // a run reads the source again, and the check must name what that run would find.
    let dir = tempfile::tempdir().unwrap();
    let config = trigger_config(dir.path());
    let pipeline = config.files[0].clone();
    let app = app(config);
    let (status, _) = send(&app, "POST", "/get/later").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, "POST", "/get/later").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the same answer from the kept nodes"
    );

    // A node is added: known at once (and refused for its kind, so nothing runs).
    let mut source = std::fs::read_to_string(&pipeline).unwrap();
    source.push_str("\n@task()\ndef later() -> None:\n    pass\n");
    std::fs::write(&pipeline, &source).unwrap();
    let (status, body) = send(&app, "POST", "/get/later").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A node is removed: unknown at once.
    let (status, _) = send(&app, "POST", "/get/publish").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    std::fs::write(
        &pipeline,
        source.replace("def publish", "def no_longer_publish"),
    )
    .unwrap();
    let (status, body) = send(&app, "POST", "/get/publish").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!dir.path().join("metadata.db").exists());
}

#[tokio::test]
async fn a_target_may_contain_a_slash_and_a_missing_one_is_a_json_404() {
    // A full node id of a file in a directory. Unencoded, its `/` used to match no route:
    // 404 with an empty body. So did a trigger with no target at all.
    let dir = tempfile::tempdir().unwrap();
    let app = app(trigger_config(dir.path()));
    for uri in ["/get/sub/other.py:publish", "/get/sub%2Fother.py:publish"] {
        let (status, body) = send(&app, "POST", uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        let error = body["error"].as_str().expect("an error message");
        assert!(
            error.starts_with("Asset 'sub/other.py:publish' not found. Available: "),
            "{uri}: {error}"
        );
    }
    // A path-suffixed id resolves the same either way (a task on /get: refused, nothing runs).
    let long = format!("{}:publish", dir.path().join("pipeline.py").display());
    let tail: String = long.split('/').skip(2).collect::<Vec<_>>().join("/");
    for target in [tail.clone(), tail.replace('/', "%2F")] {
        let (status, body) = send(&app, "POST", &format!("/get/{target}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{target}: {body}");
    }
    for (method, uri, says) in [
        ("POST", "/get/", "POST /get/{target} needs a target"),
        ("POST", "/run/", "POST /run/{target} needs a target"),
        ("GET", "/nowhere", "no such endpoint: GET /nowhere"),
        ("DELETE", "/run/", "no such endpoint: DELETE /run/"),
    ] {
        let (status, body) = send(&app, method, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        let error = body["error"].as_str().expect("an error message");
        assert!(error.starts_with(says), "{method} {uri}: {error}");
    }
}

#[tokio::test]
async fn the_wrong_method_on_an_endpoint_is_a_json_405_naming_the_right_ones() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(trigger_config(dir.path()));
    for (method, uri, allowed, says) in [
        (
            "GET",
            "/get/first",
            "POST",
            "GET is not allowed on /get/first: use POST",
        ),
        (
            "GET",
            "/run",
            "POST",
            "GET is not allowed on /run: use POST",
        ),
        (
            "PUT",
            "/run/x",
            "POST, DELETE",
            "PUT is not allowed on /run/x: use POST or DELETE",
        ),
        (
            "POST",
            "/health",
            "GET",
            "POST is not allowed on /health: use GET",
        ),
        (
            "DELETE",
            "/assets/first",
            "GET",
            "DELETE is not allowed on /assets/first: use GET",
        ),
    ] {
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
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri}"
        );
        assert_eq!(resp.headers()["allow"], allowed, "{method} {uri}");
        assert_eq!(body_json(resp).await["error"], says);
    }
}

#[tokio::test]
async fn an_unloaded_target_is_refused_before_run_admission() {
    let dir = tempfile::tempdir().unwrap();
    let config = isolated_config(dir.path(), false);
    std::fs::write(&config.files[0], "def broken(:\n").unwrap();
    let app = app(config);
    let (status, body) = send(&app, "POST", "/get/first").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        send(&app, "GET", "/health").await.1["load_errors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(body["error"].is_string());
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
async fn state_reports_the_next_run_in_the_servers_timezone() {
    // `0 5 * * *` in UTC is always 05:00 UTC; in Asia/Kolkata (UTC+5:30, no summer time) it
    // is always 23:30 UTC. The next run used to be computed in the machine's local time
    // whatever the server's `--timezone` (#289).
    for (zone, seconds_into_the_utc_day) in [("utc", 5 * 3600), ("Asia/Kolkata", 23 * 3600 + 1800)]
    {
        let dir = tempfile::tempdir().unwrap();
        let mut config = isolated_config(dir.path(), false);
        std::fs::write(
            &config.files[0],
            "from barca import asset, Schedule\n\n\
             @asset(freshness=Schedule(\"0 5 * * *\"))\ndef daily() -> int:\n    return 1\n",
        )
        .unwrap();
        config.timezone = zone.to_string();
        let (status, json) = send(&app(config), "GET", "/state").await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let next_run = json[0]["next_run"].as_i64().expect("a scheduled node");
        assert_eq!(next_run % 86_400, seconds_into_the_utc_day, "{zone}");
    }
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
    // A valid definition that fails in execution still owns a live event channel.
    let dir = tempfile::tempdir().unwrap();
    let config = isolated_config(dir.path(), false);
    std::fs::write(
        &config.files[0],
        "from barca import asset\n@asset()\ndef broken(): raise RuntimeError('execution failed')\n",
    )
    .unwrap();
    let app = app(config);
    let (status, body) = send(&app, "POST", "/run").await;
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

    let summaries = barca_core::queries::list_assets(&config.files, &config.python)
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

#[tokio::test]
async fn runs_inspection_survives_server_restart_and_never_mutates_history() {
    let dir = tempfile::tempdir().unwrap();
    let config = isolated_config(dir.path(), true);
    let path = &config.resolved.db_path;
    barca_core::db::init_db(path).await.unwrap();
    for (id, command, target, status) in [
        ("cli", "get", "first", "success"),
        ("scheduled", "run", "second", "failed"),
    ] {
        barca_core::db::create_run(path, id, command, "[\"old.py\"]", Some(target), Some(2))
            .await
            .unwrap();
        barca_core::db::insert_logs(path, id, &[("old.py:first".into(), format!("from {id}"))])
            .await
            .unwrap();
        barca_core::db::finish_run(path, id, status, 1, 1, 0.25)
            .await
            .unwrap();
    }
    let original = std::fs::read(path).unwrap();
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    for _ in 0..2 {
        let router = app(config.clone()); // fresh server state has no polling handles
        let (status, list) = send(&router, "GET", "/runs?limit=1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["total"], 2);
        assert_eq!(list["truncated"], true);
        assert_eq!(list["runs"][0]["id"], "scheduled");
        assert_eq!(list["runs"][0]["handle"], serde_json::Value::Null);
        assert_eq!(list["runs"][0]["status"], "failed");
        let (status, detail) = send(&router, "GET", "/runs/cli").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["run_id"], "cli");
        assert_eq!(detail["files"], serde_json::json!(["old.py"]));
        assert_eq!(detail["steps_cached"], 1);
        assert_eq!(detail["triggered_by"], "manual");
        assert!(
            detail["outcome"].is_null(),
            "a CLI run has no stored outcome"
        );
        assert_eq!(detail["logs"][0]["line"], "from cli");
        assert!(detail["started_at"].as_str().unwrap().ends_with('Z'));
        assert_eq!(
            send(&router, "GET", "/runs/missing").await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(std::fs::read(path).unwrap(), original);
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        modified
    );
}

#[tokio::test]
async fn runs_inspection_without_a_database_does_not_create_one() {
    let dir = tempfile::tempdir().unwrap();
    for read_only in [false, true] {
        let config = isolated_config(dir.path(), read_only);
        let path = config.resolved.db_path.clone();
        let router = app(config);
        let (status, list) = send(&router, "GET", "/runs").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["runs"], serde_json::json!([]));
        assert_eq!(
            send(&router, "GET", "/runs/missing").await.0,
            StatusCode::NOT_FOUND
        );
        assert!(!std::path::Path::new(&path).exists());
    }
}

#[tokio::test]
async fn groups_are_source_metadata_and_never_execution_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let config = isolated_config(dir.path(), true);
    let path = &config.files[0];
    std::fs::write(path, format!("{FIXTURE}\nfrom barca import group\npreparation = group('Preparation', members=[first], output=first)\ntraining = group('Training', members=[preparation, second], output=second)\n")).unwrap();
    let router = app(config.clone());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/groups")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let groups = body_json(resp).await;
    assert_eq!(groups.as_array().unwrap().len(), 2);
    assert_eq!(groups[1]["members"][0], format!("group:{path}:preparation"));
    assert_eq!(groups[1]["output"], format!("{path}:second"));
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let assets = body_json(resp).await;
    assert_eq!(assets.as_array().unwrap().len(), 2);
    assert!(!std::path::Path::new(&config.resolved.db_path).exists());
    // Fresh metadata on the next request, including invalid declarations.
    std::fs::write(
        path,
        format!(
            "{FIXTURE}\nfrom barca import group\ng = group('g', members=[first], output=second)\n"
        ),
    )
    .unwrap();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/groups")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, serde_json::json!([]));
    let (status, health) = send(&router, "GET", "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!health["load_errors"].as_array().unwrap().is_empty());
    let (_, assets) = send(&router, "GET", "/assets").await;
    assert_eq!(assets.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn groups_use_the_healthy_subset_beside_invalid_sources_and_definitions() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = isolated_config(dir.path(), true);
    let path = config.files[0].clone();
    std::fs::write(&path, format!("{FIXTURE}\nfrom barca import group\ng = group('Preparation', members=[first, second], output=second)\n")).unwrap();
    let broken = dir.path().join("broken.py");
    std::fs::write(&broken, "from barca import asset\n@asset\ndef broken(:\n").unwrap();
    config.files.push(broken.display().to_string());
    let router = app(config);
    let (status, groups) = send(&router, "GET", "/groups").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(groups.as_array().unwrap().len(), 1);
    assert_eq!(
        send(&router, "GET", "/assets")
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // A valid file can contain an excluded definition too. Its unusable group
    // must disappear, without suppressing the same file's healthy nodes.
    std::fs::write(&path, format!("{FIXTURE}\nfrom barca import group\n@asset(inputs={{'x': missing}})\ndef excluded(x): return x\ng = group('Preparation', members=[first, excluded], output=first)\n")).unwrap();
    let (status, groups) = send(&router, "GET", "/groups").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(groups, serde_json::json!([]));
    assert_eq!(
        send(&router, "GET", "/assets")
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        send(&router, "GET", "/health").await.1["load_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error["error"].as_str().unwrap().contains("invalid group"))
    );
}

// ─── the run contract (RFC-0007 §3.4, #393) ─────────────────────────────────

/// A pipeline whose runs need a Python worker: an asset chain, a failing asset, a task that
/// prints a few thousand lines, and a scheduled asset.
const CONTRACT_FIXTURE: &str = r#"
from barca import asset, task, Schedule

@asset()
def first() -> dict:
    return {"n": 1}

@asset(inputs={"first": first})
def second(first: dict) -> dict:
    return {"n": first["n"] + 1}

@asset()
def broken() -> dict:
    raise ValueError("expected failure")

@task()
def chatty() -> None:
    for i in range(3000):
        print("line", i)

@asset(freshness=Schedule("* * * * * *"))
def ticking() -> int:
    return 1
"#;

/// A config whose workers import this checkout's python/ tree, with the DB and artifacts in
/// `dir`.
#[cfg(unix)]
fn runnable_config(dir: &std::path::Path) -> ServeConfig {
    use std::os::unix::fs::PermissionsExt;
    let mut config = isolated_config(dir, false);
    let source = dir.join("pipeline.py");
    std::fs::write(&source, CONTRACT_FIXTURE).unwrap();
    let py_tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
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
    config.files = vec![source.display().to_string()];
    config.python = wrapper;
    config.timezone = "utc".into();
    config.resolved.local_artifact_dir = config.resolved.artifact_root.clone();
    std::fs::create_dir_all(&config.resolved.artifact_root).unwrap();
    config
}

/// Send `body` as JSON.
async fn send_json(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

/// Every event of a run's stream, `(id, event)`, reading it to its end. `pause` waits before
/// each read, as a slow client does.
async fn read_events(
    app: &axum::Router,
    uri: &str,
    pause: std::time::Duration,
) -> Vec<(u64, serde_json::Value)> {
    use futures::StreamExt;
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    let mut stream = resp.into_body().into_data_stream();
    let mut text = String::new();
    let read = async {
        while let Some(chunk) = stream.next().await {
            if !pause.is_zero() {
                tokio::time::sleep(pause).await;
            }
            text.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), read)
        .await
        .expect("the stream ends after run_finished");
    text.split("\n\n")
        .filter_map(|frame| {
            let id = frame.lines().find_map(|l| l.strip_prefix("id: "))?;
            let data = frame.lines().find_map(|l| l.strip_prefix("data: "))?;
            Some((id.parse().unwrap(), serde_json::from_str(data).unwrap()))
        })
        .collect()
}

/// Start `request` with `POST /runs` and return its run id.
async fn start_run(app: &axum::Router, request: serde_json::Value) -> String {
    let (status, body) = send_json(app, "POST", "/runs", request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    body["run_id"].as_str().expect("a run id").to_string()
}

#[tokio::test]
async fn post_runs_refuses_what_the_cli_refuses_before_anything_starts() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = trigger_config(dir.path());
    let app = app(config.clone());
    let deployed = config.files[0].clone();
    for (body, want, says) in [
        (
            serde_json::json!({"command": "get", "targets": ["nope"]}),
            StatusCode::NOT_FOUND,
            "Asset 'nope' not found",
        ),
        (
            serde_json::json!({"command": "get", "targets": ["first"]}),
            StatusCode::CONFLICT,
            "'first' matches more than one node",
        ),
        (
            serde_json::json!({"command": "get", "targets": ["publish"]}),
            StatusCode::BAD_REQUEST,
            "'publish' is a task",
        ),
        (
            serde_json::json!({"command": "run", "targets": []}),
            StatusCode::BAD_REQUEST,
            "a target task is required",
        ),
        (
            serde_json::json!({"command": "get", "invented": 1}),
            StatusCode::BAD_REQUEST,
            "the request body is not a run request",
        ),
        (
            serde_json::json!({"command": "sql"}),
            StatusCode::BAD_REQUEST,
            "the request body is not a run request",
        ),
        (
            serde_json::json!({"command": "get", "files": ["elsewhere.py"]}),
            StatusCode::BAD_REQUEST,
            "'elsewhere.py' is not one of the deployed files",
        ),
    ] {
        let (status, error) = send_json(&app, "POST", "/runs", body.clone()).await;
        assert_eq!(status, want, "{body}: {error}");
        // The CLI's error envelope, with a kind and the exit code a local command has.
        assert_eq!(error["kind"], "usage", "{error}");
        assert_eq!(error["code"], 2, "{error}");
        assert!(
            error["error"].as_str().unwrap().starts_with(says),
            "{body}: {error}"
        );
        assert!(error.get("remediation").is_some(), "{error}");
    }
    // A request naming a deployed file is accepted as far as the files go.
    let (status, _) = send_json(
        &app,
        "POST",
        "/runs",
        serde_json::json!({"command": "get", "targets": ["nope"], "files": [deployed]}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A dry run answers at once with the `--dry-run` JSON, and starts nothing.
    let (status, plan) = send_json(
        &app,
        "POST",
        "/runs",
        serde_json::json!({"command": "run", "targets": ["publish"], "dry_run": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert!(plan["steps"].is_array(), "{plan}");
    assert!(plan["summary"]["will_run"].is_number(), "{plan}");
    assert!(
        app.clone()
            .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
            .await
            .is_ok()
    );
    assert!(!dir.path().join("metadata.db").exists(), "nothing ran");
    // A read-only server refuses to run, but still answers a dry run.
    config.read_only = true;
    let read_only = barca_server::app(config);
    let (status, error) = send_json(
        &read_only,
        "POST",
        "/runs",
        serde_json::json!({"command": "run", "targets": ["publish"]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    let (status, _) = send_json(
        &read_only,
        "POST",
        "/runs",
        serde_json::json!({"command": "run", "targets": ["publish"], "dry_run": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// The whole contract on a real run: `POST /runs` answers the database id; the stream carries
/// `run_started`, the progress lines tagged with `when`, step results (cached ones too) and
/// `run_finished` with the outcome; `GET /runs/{id}` and `/output` answer from the DB after a
/// restart.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_run_streams_its_lines_and_outcome_and_is_answered_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = runnable_config(dir.path());
    let app = app(config.clone());

    let id = start_run(
        &app,
        serde_json::json!({"command": "get", "targets": ["second"], "agent": true}),
    )
    .await;
    let events = read_events(&app, &format!("/runs/{id}/events"), Default::default()).await;
    let ids: Vec<u64> = events.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        ids,
        (0..events.len() as u64).collect::<Vec<_>>(),
        "numbered from 0"
    );
    assert_eq!(
        events[0].1,
        serde_json::json!({"type": "run_started", "run_id": id})
    );
    let lines: Vec<&serde_json::Value> = events
        .iter()
        .map(|(_, e)| e)
        .filter(|e| e["type"] == "line")
        .collect();
    assert!(
        lines.iter().any(|l| l["when"] == "agent"
            && l["line"].as_str().unwrap().starts_with("[barca] step:")
            && l["line"].as_str().unwrap().contains(":second completed")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l["when"] == "always" && l["line"].as_str().unwrap().contains("done in")),
        "the end-of-run line: {lines:?}"
    );
    let (_, last) = events.last().unwrap();
    assert_eq!(last["type"], "run_finished");
    assert_eq!(last["ok"], true);
    let outcome = last["outcome"].clone();
    assert_eq!(outcome["status"], "success");
    assert_eq!(
        outcome["run_id"],
        id.as_str(),
        "the run id is the database id"
    );
    // As `barca get --json` prints it: a pointer to the artifact (paths by default, #404).
    let pointer = &outcome["final_output"]["_barca_artifact"];
    assert_eq!(pointer["format"], "json");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(pointer["path"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(value, serde_json::json!({"n": 2}));

    // The same answer from GET /runs/{id}, and the run is in the history as an API run.
    let (status, run) = send(&app, "GET", &format!("/runs/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["status"], "success");
    assert_eq!(run["triggered_by"], "api");
    assert_eq!(run["outcome"], outcome);
    assert_eq!(run["target"], "second");
    let (_, history) = send(&app, "GET", "/history").await;
    assert_eq!(history["runs"][0]["run_id"], id.as_str());
    assert_eq!(history["runs"][0]["triggered_by"], "api");

    // A second get serves both steps from cache, and says so in the stream.
    let again = start_run(
        &app,
        serde_json::json!({"command": "get", "targets": ["second"]}),
    )
    .await;
    let events = read_events(&app, &format!("/runs/{again}/events"), Default::default()).await;
    let cached: Vec<_> = events
        .iter()
        .filter(|(_, e)| e["type"] == "step_finished" && e["cached"] == true)
        .collect();
    assert_eq!(cached.len(), 2, "{events:?}");

    // A failed step: the outcome carries the failed-run body and the envelope.
    let failed = start_run(
        &app,
        serde_json::json!({"command": "get", "targets": ["broken"]}),
    )
    .await;
    let events = read_events(&app, &format!("/runs/{failed}/events"), Default::default()).await;
    let outcome = &events.last().unwrap().1["outcome"];
    assert_eq!(outcome["status"], "failed");
    assert!(
        outcome["failed_node"]
            .as_str()
            .unwrap()
            .ends_with(":broken")
    );
    assert_eq!(outcome["envelope"]["kind"], "step_failed");
    assert_eq!(outcome["envelope"]["code"], 1);
    assert!(
        outcome["envelope"]["traceback"]
            .as_str()
            .unwrap()
            .contains("expected failure")
            || outcome["error"]
                .as_str()
                .unwrap()
                .contains("expected failure")
    );
    let (status, _) = send(&app, "DELETE", &format!("/runs/{failed}")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a finished run cannot be cancelled"
    );
    let (status, _) = send(&app, "GET", &format!("/runs/{failed}/output")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a failed run has no output");

    let output = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/runs/{id}/output"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(output.status(), StatusCode::OK);
    assert_eq!(output.headers()["content-type"], "application/json");
    assert_eq!(body_json(output).await, serde_json::json!({"n": 2}));

    // A restart: the in-memory run state is gone, the answers are not.
    let restarted = barca_server::app(config);
    for (run_id, status) in [(&id, "success"), (&failed, "failed")] {
        let (code, run) = send(&restarted, "GET", &format!("/runs/{run_id}")).await;
        assert_eq!(code, StatusCode::OK, "{run}");
        assert_eq!(run["status"], status, "{run}");
        assert_eq!(run["triggered_by"], "api");
        assert_eq!(run["outcome"]["status"], status, "{run}");
        assert!(!run["steps"].as_array().unwrap().is_empty(), "{run}");
    }
    let output = restarted
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/runs/{id}/output"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(output.status(), StatusCode::OK);
    assert_eq!(body_json(output).await, serde_json::json!({"n": 2}));
    let (status, error) = send(&restarted, "GET", &format!("/runs/{id}/events")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert!(
        error["remediation"]
            .as_str()
            .unwrap()
            .contains(&format!("GET /runs/{id}"))
    );
}

/// A client that reads slowly receives every event once, in order, and a client that
/// reconnects receives the run again from its first event, every event once.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_lagging_and_a_reconnecting_subscriber_receive_every_event_once() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(runnable_config(dir.path()));
    let id = start_run(
        &app,
        serde_json::json!({"command": "run", "targets": ["chatty"]}),
    )
    .await;
    let uri = format!("/runs/{id}/events");
    // Reads one chunk per millisecond while the task prints 3000 lines.
    let slow = read_events(&app, &uri, std::time::Duration::from_millis(1)).await;
    let reconnect = read_events(&app, &uri, Default::default()).await;
    for events in [&slow, &reconnect] {
        let ids: Vec<u64> = events.iter().map(|(n, _)| *n).collect();
        assert_eq!(ids, (0..events.len() as u64).collect::<Vec<_>>());
        let logs: Vec<&str> = events
            .iter()
            .filter(|(_, e)| e["type"] == "log")
            .map(|(_, e)| e["line"].as_str().unwrap())
            .collect();
        assert_eq!(logs.len(), 3000, "every printed line, once");
        assert_eq!(logs[2999], "line 2999");
        assert_eq!(events.last().unwrap().1["type"], "run_finished");
    }
    assert_eq!(slow, reconnect);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn delete_runs_cancels_a_run_and_its_outcome_says_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let config = runnable_config(dir.path());
    std::fs::write(&config.files[0], SLOW_FIXTURE).unwrap();
    let app = app(config);
    let id = start_run(
        &app,
        serde_json::json!({"command": "get", "targets": ["slow_one"]}),
    )
    .await;
    let t0 = std::time::Instant::now();
    loop {
        let (_, run) = send(&app, "GET", &format!("/runs/{id}")).await;
        if run["status"] == "running" {
            break;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(30), "{run}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (status, body) = send(&app, "DELETE", &format!("/runs/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelling");
    let events = read_events(&app, &format!("/runs/{id}/events"), Default::default()).await;
    let outcome = &events.last().unwrap().1["outcome"];
    assert_eq!(outcome["status"], "cancelled");
    assert_eq!(outcome["envelope"]["code"], 130);
    let (_, run) = send(&app, "GET", &format!("/runs/{id}")).await;
    assert_eq!(run["status"], "cancelled");
}

/// Inspection in the CLI's shapes: `/list`, `/nodes`, `/history`, `/stats/{name}`,
/// `/project`, and `/health` with the API version, environment and store.
#[tokio::test]
async fn inspection_routes_answer_in_the_cli_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = isolated_config(dir.path(), false);
    config.resolved.local_artifact_dir = config.resolved.artifact_root.clone();
    let app = app(config.clone());
    let (status, list) = send(&app, "GET", "/list").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 2);
    assert_eq!(list["truncated"], false);
    assert_eq!(list["nodes"][0]["freshness"], "always");
    assert!(list["root"].is_string());
    let (status, nodes) = send(&app, "GET", "/nodes?targets=second").await;
    assert_eq!(status, StatusCode::OK, "{nodes}");
    assert!(
        nodes["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"].as_str().unwrap().ends_with(":second"))
    );
    let (status, all) = send(&app, "GET", "/nodes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["nodes"].as_array().unwrap().len(), 2);
    let (status, error) = send(&app, "GET", "/nodes?targets=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["kind"], "usage");
    let (status, history) = send(&app, "GET", "/history?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["runs"], serde_json::json!([]));
    assert_eq!(history["total"], 0);
    let (status, stats) = send(&app, "GET", "/stats/first").await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert!(
        stats["id"].as_str().unwrap().ends_with(":first"),
        "the CLI's `id` key: {stats}"
    );
    assert_eq!(stats["total_runs"], 0);
    let (status, _) = send(&app, "GET", "/stats/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, project) = send(&app, "GET", "/project").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(project["api_version"], 1);
    assert_eq!(project["env"], config.resolved.env.as_str());
    let files = project["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["sha256"].as_str().unwrap().len(), 64);
    let (_, health) = send(&app, "GET", "/health").await;
    assert_eq!(health["api_version"], 1);
    assert_eq!(health["env"], config.resolved.env.as_str());
    assert!(health["store_uri"].is_null(), "no separate store: {health}");
    let mut stored = config.clone();
    stored.resolved.artifact_root = "s3://team-bucket/barca/default/artifacts".into();
    let (_, health) = send(&barca_server::app(stored), "GET", "/health").await;
    assert_eq!(
        health["store_uri"],
        "s3://team-bucket/barca/default/artifacts"
    );
    // `/assets` and `/assets/{name}` in the CLI's shapes too.
    let (_, assets) = send(&app, "GET", "/assets").await;
    assert_eq!(assets[0]["freshness"], "always");
    let (_, asset) = send(&app, "GET", "/assets/first").await;
    assert_eq!(asset["asset"]["freshness"], "always");
    assert!(
        asset["stats"]["id"].as_str().unwrap().ends_with(":first"),
        "{asset}"
    );
    assert!(asset["stats"].get("node_id").is_none(), "{asset}");
    assert!(
        !dir.path().join("metadata.db").exists(),
        "inspection created the DB"
    );
}

// ─── pausing a schedule (#435) ──────────────────────────────────────────────

const SCHEDULED_FIXTURE: &str = r#"
from barca import asset, Schedule

@asset(freshness=Schedule("* * * * * *"))
def every_second() -> int:
    return 1

@asset()
def plain() -> int:
    return 2
"#;

/// Wait for `GET /schedule` to show a skipped tick of the only job.
async fn skipped_tick(app: &axum::Router) -> serde_json::Value {
    let t0 = std::time::Instant::now();
    loop {
        let (_, schedule) = send(app, "GET", "/schedule").await;
        if let Some(job) = schedule
            .as_array()
            .and_then(|jobs| jobs.first())
            .filter(|job| !job["last_skipped"].is_null())
        {
            return job.clone();
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(30),
            "{schedule}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_schedule_skips_its_ticks_and_stays_paused_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = isolated_config(dir.path(), false);
    std::fs::write(&config.files[0], SCHEDULED_FIXTURE).unwrap();
    config.schedule = true;
    config.timezone = "utc".into();

    let app = app(config.clone());
    for (uri, want) in [
        ("/schedule/nope/pause", StatusCode::NOT_FOUND),
        ("/schedule/plain/pause", StatusCode::NOT_FOUND),
        (
            "/schedule/every_second/pause?until=tomorrow",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/schedule/every_second/pause?until=2001-01-01T00:00:00Z",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, error) = send(&app, "POST", uri).await;
        assert_eq!(status, want, "{uri}: {error}");
        assert_eq!(error["kind"], "usage", "{error}");
    }
    let (status, job) = send(&app, "POST", "/schedule/every_second/pause").await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["paused"], true);
    assert!(job["paused_until"].is_null(), "until resumed");

    // A restart: the scheduler starts with the job paused, and its ticks are skipped.
    let (restarted, start_scheduler) = barca_server::app_with_scheduler(config.clone());
    let scheduler = start_scheduler();
    let job = skipped_tick(&restarted).await;
    assert_eq!(job["paused"], true, "{job}");
    assert_eq!(job["skip_reason"], "paused", "{job}");
    assert!(
        job["last_run"].is_null(),
        "a paused job started a run: {job}"
    );
    assert!(job["skipped_for"].is_null(), "{job}");

    // Paused until a time: reported, and resumed by hand.
    let until = (chrono::Utc::now() + chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let (status, job) = send(
        &restarted,
        "POST",
        &format!("/schedule/every_second/pause?until={until}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["paused"], true);
    let until_epoch = chrono::DateTime::parse_from_rfc3339(&until)
        .unwrap()
        .timestamp();
    assert_eq!(job["paused_until"], until_epoch);
    let (status, job) = send(&restarted, "POST", "/schedule/every_second/resume").await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["paused"], false);
    assert!(job["paused_until"].is_null());
    scheduler.abort();
    let _ = scheduler.await;

    // Resumed stays resumed across a restart.
    let again = barca_server::app(config.clone());
    let (status, job) = send(&again, "POST", "/schedule/every_second/resume").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job["paused"], false);
    let (_, schedule) = send(&again, "GET", "/schedule").await;
    assert_eq!(
        schedule,
        serde_json::json!([]),
        "no scheduler started on this one"
    );

    // A read-only server neither pauses nor resumes.
    config.read_only = true;
    let read_only = barca_server::app(config);
    let (status, _) = send(&read_only, "POST", "/schedule/every_second/pause").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn every_error_body_is_the_cli_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(trigger_config(dir.path()));
    for (method, uri, status) in [
        ("GET", "/runs/missing", StatusCode::NOT_FOUND),
        ("GET", "/runs/missing/events", StatusCode::NOT_FOUND),
        ("GET", "/runs/missing/output", StatusCode::NOT_FOUND),
        ("DELETE", "/runs/missing", StatusCode::NOT_FOUND),
        ("GET", "/assets/first", StatusCode::CONFLICT),
        ("GET", "/nowhere", StatusCode::NOT_FOUND),
        ("PUT", "/runs", StatusCode::METHOD_NOT_ALLOWED),
        ("POST", "/get/publish", StatusCode::BAD_REQUEST),
    ] {
        let (got, body) = send(&app, method, uri).await;
        assert_eq!(got, status, "{method} {uri}: {body}");
        assert_eq!(body["kind"], "usage", "{method} {uri}: {body}");
        assert_eq!(body["code"], 2, "{method} {uri}: {body}");
        assert!(body["error"].is_string(), "{method} {uri}: {body}");
        assert!(body.get("remediation").is_some(), "{method} {uri}: {body}");
    }
}

// ─── the ledger (#402) ──────────────────────────────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// A server whose store is a directory beside its local artifact directory, as a deployment
/// with `[remote] uri` has.
fn ledger_config(dir: &std::path::Path) -> ServeConfig {
    let mut config = runnable_config(dir);
    config.resolved.artifact_root = dir.join("store/default/artifacts").display().to_string();
    config.resolved.local_artifact_dir = dir.join("local").display().to_string();
    config
}

/// Put `bytes` in the store at `{node}/{run_hash}.json` and return its store path.
fn store_object(config: &ServeConfig, node: &str, run_hash: &str, bytes: &[u8]) -> String {
    let dir =
        std::path::Path::new(&config.resolved.artifact_root).join(node.replace([':', '/'], "_"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{run_hash}.json"));
    std::fs::write(&path, bytes).unwrap();
    path.display().to_string()
}

fn ledger_post(
    client_run_id: &str,
    node: &str,
    run_hash: &str,
    path: &str,
    bytes: &[u8],
) -> serde_json::Value {
    serde_json::json!({
        "run": {
            "run_id": client_run_id,
            "command": "get",
            "files": ["pipeline.py"],
            "target": "first",
            "status": "success",
            "steps_total": 1,
            "steps_executed": 1,
            "steps_cached": 0,
            "started_at": "2026-10-20 12:00:00",
            "finished_at": "2026-10-20 12:00:01",
            "elapsed_seconds": 1.0,
            "host": "laptop-a"
        },
        "steps": [{
            "node_id": node,
            "run_hash": run_hash,
            "status": "success",
            "path": path,
            "format": "json",
            "size_bytes": bytes.len(),
            "digest": sha256_hex(bytes),
            "elapsed_seconds": 0.5,
            "attempts": 1
        }]
    })
}

async fn lookup_one(app: &axum::Router, node: &str, run_hash: &str) -> serde_json::Value {
    let (status, body) = send_json(
        app,
        "POST",
        "/ledger/lookup",
        serde_json::json!({"hashes": [{"node_id": node, "run_hash": run_hash}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["results"].as_array().unwrap().len(), 1, "{body}");
    body["results"][0].clone()
}

#[tokio::test]
async fn the_ledger_records_only_posts_whose_objects_are_in_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let config = ledger_config(dir.path());
    let app = app(config.clone());
    let node = "pipeline.py:first";
    let hash = "a1b2c3d4e5f60718";

    // Nothing recorded yet.
    let miss = lookup_one(&app, node, hash).await;
    assert_eq!(miss["exists"], false, "{miss}");
    assert!(miss.get("path").is_none(), "{miss}");

    // The object is not in the store: 409 naming the node, and nothing recorded.
    let bytes = br#"{"n": 1}"#;
    let path = std::path::Path::new(&config.resolved.artifact_root)
        .join("pipeline.py_first")
        .join(format!("{hash}.json"))
        .display()
        .to_string();
    let (status, error) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-1", node, hash, &path, bytes),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["kind"], "usage");
    assert_eq!(error["code"], 2);
    assert!(
        error["error"].as_str().unwrap().starts_with(node),
        "{error}"
    );
    assert!(
        error["error"].as_str().unwrap().contains("no object"),
        "{error}"
    );
    assert!(error["remediation"].is_string(), "{error}");
    assert_eq!(lookup_one(&app, node, hash).await["exists"], false);
    let (_, history) = send(&app, "GET", "/history?limit=5").await;
    assert_eq!(
        history["total"], 0,
        "a refused post records nothing: {history}"
    );

    // The same post once the object is there: 201, and the ledger has it.
    let path = store_object(&config, node, hash, bytes);
    let (status, accepted) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-1", node, hash, &path, bytes),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{accepted}");
    let run_id = accepted["run_id"].as_str().unwrap().to_string();
    assert_eq!(accepted["bytes_changed"], serde_json::json!([]));
    let hit = lookup_one(&app, node, hash).await;
    assert_eq!(hit["exists"], true, "{hit}");
    assert_eq!(hit["path"], path.as_str());
    assert_eq!(hit["format"], "json");
    assert_eq!(hit["size_bytes"], bytes.len());
    assert_eq!(hit["digest"], sha256_hex(bytes).as_str());

    // The run is in the server's history, posted by a client, under the server's run id.
    let (_, history) = send(&app, "GET", "/history?limit=5").await;
    assert_eq!(history["total"], 1, "{history}");
    assert_eq!(history["runs"][0]["run_id"], run_id.as_str());
    assert_eq!(history["runs"][0]["triggered_by"], "client");
    assert_eq!(history["runs"][0]["client_run_id"], "c-1");
    assert_eq!(history["runs"][0]["status"], "success");

    // A repeated post of the same run (its answer was lost) is the run already recorded.
    let (status, again) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-1", node, hash, &path, bytes),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again["run_id"], run_id.as_str());
    let (_, history) = send(&app, "GET", "/history?limit=5").await;
    assert_eq!(history["total"], 1, "{history}");

    // A second machine posts the same run hash with other bytes: the ledger keeps its row and
    // says so.
    let other = br#"{"n": 2}"#;
    store_object(&config, node, hash, other);
    let (status, changed) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-2", node, hash, &path, other),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{changed}");
    assert_eq!(changed["bytes_changed"], serde_json::json!([node]));
    assert_eq!(
        lookup_one(&app, node, hash).await["digest"],
        sha256_hex(bytes).as_str()
    );

    // A digest that does not match the object is refused.
    let (status, error) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-3", node, hash, &path, bytes),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(
        error["error"].as_str().unwrap().contains("digest"),
        "{error}"
    );

    // An object outside the server's store is refused.
    let outside = dir.path().join("elsewhere.json");
    std::fs::write(&outside, bytes).unwrap();
    let (status, error) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-4", node, hash, &outside.display().to_string(), bytes),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("not in this server's artifact store"),
        "{error}"
    );

    // Not a post at all.
    let (status, error) =
        send_json(&app, "POST", "/ledger/runs", serde_json::json!({"run": 1})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["kind"], "usage");
}

#[tokio::test]
async fn a_server_without_a_store_or_a_read_only_one_takes_no_posts() {
    let dir = tempfile::tempdir().unwrap();
    let config = runnable_config(dir.path());
    let app = app(config.clone());
    let (status, error) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-1", "pipeline.py:first", "h", "/x.json", b"1"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("no artifact store"),
        "{error}"
    );

    let dir = tempfile::tempdir().unwrap();
    let mut config = ledger_config(dir.path());
    config.read_only = true;
    let app = barca_server::app(config);
    let (status, error) = send_json(
        &app,
        "POST",
        "/ledger/runs",
        ledger_post("c-1", "pipeline.py:first", "h", "/x.json", b"1"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    // A read-only server answers lookups from a snapshot, and creates no database.
    assert_eq!(
        lookup_one(&app, "pipeline.py:first", "h").await["exists"],
        false
    );
    assert!(!dir.path().join("metadata.db").exists());
}

#[tokio::test]
async fn health_reports_whether_the_scheduler_runs_and_no_metadata_error() {
    for (read_only, schedule, enabled) in [
        (false, true, true),
        (false, false, false),
        (true, true, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = isolated_config(dir.path(), read_only);
        config.schedule = schedule;
        let (_, health) = send(&app(config), "GET", "/health").await;
        assert_eq!(
            health["scheduler_enabled"], enabled,
            "{read_only} {schedule}"
        );
        assert_eq!(health["metadata_error"], serde_json::Value::Null);
    }
}

/// A metadata DB barca cannot use is one structured error naming it (`reason`
/// `metadata_unusable`, with a remediation), never a SQL error, on a writer and a read-only
/// server; `/health` reports it until a read succeeds again (#513).
#[tokio::test]
async fn an_unusable_metadata_db_is_a_structured_error_reported_on_health() {
    for read_only in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let config = isolated_config(dir.path(), read_only);
        let db = config.resolved.db_path.clone();
        std::fs::write(&db, "x".repeat(4096)).unwrap();
        let app = app(config);
        for uri in ["/state", "/runs"] {
            let (status, body) = send(&app, "GET", uri).await;
            assert_eq!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "GET {uri}: {body}"
            );
            assert_eq!(body["kind"], "infra");
            assert_eq!(body["code"], 3);
            assert_eq!(body["reason"], "metadata_unusable");
            let error = body["error"].as_str().unwrap();
            assert!(error.contains(&db), "{error}");
            assert!(!error.contains("Parse error"), "{error}");
            let fix = body["remediation"].as_str().unwrap();
            assert!(fix.contains("barca state restore"), "{fix}");
            assert!(fix.contains("upgrade barca"), "{fix}");
        }
        let (_, health) = send(&app, "GET", "/health").await;
        let reported = health["metadata_error"].as_str().unwrap();
        assert!(reported.contains(&db), "{reported}");

        // Moved aside: the next read succeeds, and /health clears.
        std::fs::remove_file(&db).unwrap();
        assert_eq!(send(&app, "GET", "/state").await.0, StatusCode::OK);
        let (_, health) = send(&app, "GET", "/health").await;
        assert_eq!(health["metadata_error"], serde_json::Value::Null);
    }
}

#[tokio::test]
async fn a_writer_serve_whose_metadata_db_cannot_be_used_fails_before_binding() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = isolated_config(dir.path(), false);
    config.schedule = true;
    let db = config.resolved.db_path.clone();
    std::fs::write(&db, b"").unwrap();
    let error = barca_server::serve(config).await.unwrap_err();
    let barca_server::ServeError::Metadata(barca_core::BarcaError::MetadataUnusable { path, why }) =
        &error
    else {
        panic!("not a metadata error: {error}");
    };
    assert_eq!(path, &db);
    assert_eq!(why, "it is empty (0 bytes)");
    assert_eq!(std::fs::read(&db).unwrap(), b"", "left as it was");
}
