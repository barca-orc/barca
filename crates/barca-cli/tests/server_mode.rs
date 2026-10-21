//! `--server` against an in-process `barca serve` on port 0, for the paths that need no Python:
//! inspection, a dry run, usage errors, and how the client fails (a server that is not barca,
//! another API version, a stream that drops mid-run). The Python parity suite
//! (`python/tests/test_server_mode_parity.py`) covers runs that execute steps.
//!
//! One test function: the in-process server reads its project relative to the process's
//! working directory, which this test sets once.

use axum::Router;
use axum::routing::{get, post};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Output};

const PIPELINE: &str = r#"
from barca import asset, task


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"rows": numbers})
def total(rows: list) -> dict:
    return {"total": sum(rows)}


@task(inputs={"t": total})
def report(t: dict) -> None:
    print(t)
"#;

fn barca(cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_barca"));
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("BARCA_SERVER")
        .env_remove("BARCA_ENV")
        .env_remove("BARCA_OUTPUT")
        .env_remove("BARCA_REMOTE_URI");
    // Through helper_proc's lock, as every child process of barca's tests is started.
    barca_core::helper_proc::output_std(&mut cmd).expect("barca runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The last stderr line as the JSON error envelope.
fn envelope(out: &Output) -> Value {
    let stderr = text(&out.stderr);
    let line = stderr.lines().last().unwrap_or_default();
    serde_json::from_str(line).unwrap_or_else(|_| panic!("not an envelope: {stderr}"))
}

/// Serve `router` on 127.0.0.1:0 on `rt`; the base URL.
fn serve(rt: &tokio::runtime::Runtime, router: Router) -> String {
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(SocketAddr::from((
            [127, 0, 0, 1],
            0,
        ))))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    rt.spawn(async move { axum::serve(listener, router).await });
    format!("http://{addr}")
}

/// `root` replaced so a local and a served copy compare equal.
fn normalized(out: &Output, roots: &[&Path]) -> (Option<i32>, String, String) {
    let clean = |s: String| {
        roots.iter().fold(s, |s, root| {
            s.replace(&root.display().to_string(), "<root>")
        })
    };
    (
        out.status.code(),
        clean(text(&out.stdout)),
        clean(text(&out.stderr)),
    )
}

#[test]
fn server_mode_without_python() {
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("local");
    let served = tmp.path().join("served");
    for dir in [&local, &served] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pipeline.py"), PIPELINE).unwrap();
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // The real server, in this process, on the served copy.
    std::env::set_current_dir(&served).unwrap();
    let resolved = barca_core::config::resolve(None).unwrap();
    let config = barca_server::ServeConfig {
        files: vec!["pipeline.py".into()],
        host: [127, 0, 0, 1].into(),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "local".into(),
        python: barca_core::commands::find_python(),
        resolved,
        read_only: false,
    };
    let url = serve(&rt, barca_server::app(config));
    let served = served.canonicalize().unwrap();
    let local = local.canonicalize().unwrap();
    let roots = [served.as_path(), local.as_path()];

    // Parity: each command prints the same here and through the server.
    for args in [
        vec!["list", "pipeline.py", "--json"],
        vec!["list", "pipeline.py", "--pretty"],
        vec![
            "list",
            "pipeline.py",
            "--json",
            "--limit",
            "1",
            "--fields",
            "id",
        ],
        vec!["history", "--json"],
        vec!["history", "--pretty"],
        vec!["get", "total", "pipeline.py", "--dry-run", "--json"],
        vec!["run", "report", "pipeline.py", "--dry-run", "--pretty"],
        vec!["get", "nope", "pipeline.py", "--json"],
        vec!["run", "total", "pipeline.py", "--json"],
        vec!["stats", "total", "pipeline.py", "--json"],
        vec!["status", "total", "pipeline.py", "--json"],
    ] {
        let here = barca(&local, &args);
        let mut remote_args = args.clone();
        remote_args.extend(["--server", url.as_str()]);
        let there = barca(&local, &remote_args);
        assert_eq!(
            normalized(&here, &roots),
            normalized(&there, &roots),
            "barca {}",
            args.join(" ")
        );
    }

    // A file the deployment does not have.
    let out = barca(&local, &["list", "other.py", "--json", "--server", &url]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        envelope(&out)["error"]
            .as_str()
            .unwrap()
            .contains("not one of the files deployed")
    );

    // A server that is not barca answers /health with something else.
    let not_barca = serve(
        &rt,
        Router::new().route("/health", get(|| async { "<html>hello</html>" })),
    );
    let out = barca(&local, &["history", "--json", "--server", &not_barca]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let e = envelope(&out);
    assert_eq!(e["kind"], "infra");
    assert!(e["error"].as_str().unwrap().contains(&not_barca));

    // Another API version.
    let future = serve(
        &rt,
        Router::new().route(
            "/health",
            get(|| async {
                axum::Json(json!({"status": "ok", "api_version": 2, "env": "default"}))
            }),
        ),
    );
    let out = barca(&local, &["history", "--json", "--server", &future]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        envelope(&out)["error"]
            .as_str()
            .unwrap()
            .contains("API version 2")
    );

    // A stream that ends before the run does: exit 3 naming the run, which goes on.
    let dropping = serve(
        &rt,
        Router::new()
            .route(
                "/health",
                get(|| async {
                    axum::Json(json!({"status": "ok", "api_version": 1, "env": "default"}))
                }),
            )
            .route(
                "/runs",
                post(|| async {
                    (
                        axum::http::StatusCode::ACCEPTED,
                        axum::Json(json!({"run_id": "0123456789ab"})),
                    )
                }),
            )
            .route(
                "/runs/{id}/events",
                get(|| async {
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        "id: 0\ndata: {\"type\":\"run_started\",\"run_id\":\"0123456789ab\"}\n\n",
                    )
                }),
            ),
    );
    let out = barca(&local, &["get", "total", "--json", "--server", &dropping]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let e = envelope(&out);
    assert_eq!(e["kind"], "infra");
    assert!(e["error"].as_str().unwrap().contains("0123456789ab"));
    assert!(
        e["remediation"]
            .as_str()
            .unwrap()
            .contains("barca history --server")
    );

    // A refusal that is not barca's envelope (a proxy's error page).
    let proxy = serve(
        &rt,
        Router::new()
            .route(
                "/health",
                get(|| async {
                    axum::Json(json!({"status": "ok", "api_version": 1, "env": "default"}))
                }),
            )
            .route(
                "/runs",
                post(|| async { (axum::http::StatusCode::BAD_GATEWAY, "Bad Gateway") }),
            ),
    );
    let out = barca(&local, &["get", "total", "--json", "--server", &proxy]);
    assert_eq!(out.status.code(), Some(3));
    assert!(envelope(&out)["error"].as_str().unwrap().contains("502"));
}
