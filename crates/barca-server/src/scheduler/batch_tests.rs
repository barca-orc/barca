//! Jobs that fire together, end to end: real runs of real pipelines, started by calling the
//! scheduler's `tick` and `catch_up` with a fixed time, never by waiting for the wall clock to
//! reach a cron match. Each test waits for a state with a deadline, and holds a slow step open
//! with a file it creates itself, so nothing depends on how fast the machine is.

use super::*;
use crate::state::ServeConfig;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tower::ServiceExt;

/// How long a test waits for a state it expects before failing.
const DEADLINE: Duration = Duration::from_secs(120);

/// Helpers every fixture pipeline starts with. `DIR` is the test's temp dir.
const PRELUDE: &str = r#"
import os
import time

from barca import asset, task, Schedule

DIR = "@DIR@"


def count_run(name):
    """Record that `name` ran, so a test can count how often it did."""
    with open(os.path.join(DIR, name + ".runs"), "a") as f:
        f.write("ran\n")


def wait_for_release():
    """Block until the test creates `release` (or two minutes pass)."""
    deadline = time.time() + 120
    while not os.path.exists(os.path.join(DIR, "release")) and time.time() < deadline:
        time.sleep(0.02)
"#;

/// A served project: its temp dir, the server state, the scheduler and the HTTP router.
struct Project {
    dir: tempfile::TempDir,
    state: AppState,
    scheduler: Scheduler,
    app: axum::Router,
}

impl Project {
    /// Write `pipeline` (after [`PRELUDE`]) and load a scheduler over it. `configure` can
    /// adjust the server state before anything starts.
    async fn new(pipeline: &str, configure: impl FnOnce(&mut AppState)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("pipeline.py");
        let prelude = PRELUDE.replace("@DIR@", &root.display().to_string());
        std::fs::write(&file, format!("{prelude}\n{pipeline}")).unwrap();

        // Workers import `barca._worker`; the wrapper puts this checkout's python/ tree on the
        // path so `cargo test` needs no installed wheel.
        let py_tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
        let python = root.join("python");
        std::fs::write(
            &python,
            format!(
                "#!/bin/sh\nPYTHONPATH=\"{}${{PYTHONPATH:+:$PYTHONPATH}}\" exec python3 \"$@\"\n",
                py_tree.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut resolved = barca_core::config::resolve_in(None, &root).unwrap();
        // Absolute paths in the temp dir, so nothing lands in the repo.
        resolved.db_path = root.join("metadata.db").display().to_string();
        resolved.artifact_root = root.join("artifacts").display().to_string();
        resolved.local_artifact_dir = resolved.artifact_root.clone();
        let mut state = AppState::new(ServeConfig {
            files: vec![file.display().to_string()],
            host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 0,
            watch: false,
            schedule: true,
            timezone: "utc".to_string(),
            python,
            resolved,
            read_only: false,
        });
        configure(&mut state);
        let scheduler = Scheduler::load(state.clone()).await;
        scheduler.publish();
        let app = crate::routes::router(state.clone());
        Self {
            dir,
            state,
            scheduler,
            app,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The full node id of the pipeline function `name`.
    fn id(&self, name: &str) -> String {
        let suffix = format!(":{name}");
        let job = self.scheduler.jobs.iter().find(|j| j.id.ends_with(&suffix));
        job.unwrap_or_else(|| panic!("no scheduled job named {name}"))
            .id
            .clone()
    }

    /// How often the pipeline function `name` ran (it calls `count_run`).
    fn runs_of(&self, name: &str) -> usize {
        std::fs::read_to_string(self.path(&format!("{name}.runs")))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// Let every step blocked in `wait_for_release` go on.
    fn release(&self) {
        std::fs::write(self.path("release"), "").unwrap();
    }

    /// A tick at a fixed time. Every fixture job is on an every-second cron, so all are due.
    async fn tick(&self, second: u32) {
        let now = Utc
            .with_ymd_and_hms(2026, 7, 2, 5, 0, second)
            .single()
            .unwrap();
        self.scheduler.tick(now.fixed_offset()).await;
    }

    /// The scheduler's record of the job `name`: its most recent run, and its status in it.
    fn job(&self, name: &str) -> JobRun {
        let record = self.scheduler.ledger.get(&self.id(name));
        record.last_run.expect("the job has fired")
    }

    fn run_status(&self, handle: &str) -> RunStatus {
        self.state.runs.get(handle).expect("run is tracked").status
    }

    /// Wait until `check` holds, failing with `what` after [`DEADLINE`].
    async fn until(&self, what: &str, check: impl Fn(&Self) -> bool) {
        let started = Instant::now();
        while !check(self) {
            assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until the run `handle` is over and every job fired into it has its final status.
    async fn until_run_is_over(&self, handle: &str) {
        self.until(&format!("run {handle} to finish"), |p| {
            !still_going(p.run_status(handle))
                && p.scheduler.jobs.iter().all(|j| {
                    let run = p.scheduler.ledger.get(&j.id).last_run;
                    run.is_none_or(|run| run.handle != handle || !still_going(run.status))
                })
        })
        .await;
    }

    async fn request(&self, method: &str, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, uri: &str) -> Value {
        let (status, body) = self.request("GET", uri).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {body}");
        body
    }

    /// `GET /schedule`, as the entry of the job `name`.
    async fn schedule_entry(&self, name: &str) -> Value {
        let id = self.id(name);
        let schedule = self.get("/schedule").await;
        let entries = schedule.as_array().expect("an array");
        let entry = entries.iter().find(|e| e["id"] == id.as_str());
        entry
            .unwrap_or_else(|| panic!("{name} not in {schedule}"))
            .clone()
    }

    /// The events `GET /events/{handle}` replays, as the JSON it sends.
    fn events(&self, handle: &str) -> Vec<Value> {
        let channel = self.state.events.get(handle).expect("run has events");
        let (backlog, _) = channel.snapshot_and_subscribe();
        backlog
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect()
    }

    /// The `target_finished` events of a run, as `(node id, ok)`, in the order they happened.
    fn finished_targets(&self, handle: &str) -> Vec<(String, bool)> {
        self.events(handle)
            .iter()
            .filter(|e| e["type"] == "target_finished")
            .map(|e| {
                (
                    e["node_id"].as_str().unwrap().to_string(),
                    e["ok"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    /// What `barca history` shows, newest first.
    async fn history(&self) -> Vec<db::RunRecord> {
        let (runs, _) = commands::history(&self.state.config.resolved, None)
            .await
            .unwrap();
        runs
    }
}

/// A scheduled asset and a scheduled task that read the same upstream.
const SHARED_UPSTREAM: &str = r#"
@asset()
def base() -> dict:
    count_run("base")
    return {"n": 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def tracked(base: dict) -> dict:
    return {"n": base["n"] + 1}


@task(freshness=Schedule("* * * * * *"), inputs={"base": base})
def report(base: dict) -> None:
    print("report", base["n"])
"#;

#[tokio::test(flavor = "multi_thread")]
async fn jobs_due_at_one_tick_share_one_run_and_their_upstream_is_computed_once() {
    let p = Project::new(SHARED_UPSTREAM, |_| {}).await;
    p.tick(0).await;

    let handle = p.job("tracked").handle;
    assert_eq!(p.job("report").handle, handle, "one run for both jobs");
    p.until_run_is_over(&handle).await;

    assert_eq!(p.runs_of("base"), 1, "the shared upstream ran once");
    assert_eq!(p.run_status(&handle), RunStatus::Complete);

    // `/status` of a run over several targets: `targets` in place of `final_output`.
    let status = p.get(&format!("/status/{handle}")).await;
    assert_eq!(status["status"], "complete");
    assert_eq!(status["error"], Value::Null);
    let result = &status["result"];
    assert!(result.get("final_output").is_none(), "{result}");
    for name in ["tracked", "report"] {
        let target = &result["targets"][p.id(name).as_str()];
        assert_eq!(target["status"], "success", "{name}: {result}");
    }
    assert!(
        result["targets"][p.id("tracked").as_str()]["final_output"]["path"].is_string(),
        "an asset target carries its output: {result}"
    );

    // One history row for the run: the targets as a comma-separated list, as `barca get a,b`
    // records them, under `serve` because no CLI command takes an asset and a task together.
    let history = p.history().await;
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].run_id, result["run_id"].as_str().unwrap());
    assert_eq!(history[0].command, "serve");
    let job_ids: Vec<&str> = p.scheduler.jobs.iter().map(|j| j.id.as_str()).collect();
    assert_eq!(job_ids.len(), 2);
    assert_eq!(
        history[0].target.as_deref(),
        Some(job_ids.join(",").as_str())
    );
    assert_eq!(history[0].status, "success");

    // Both jobs report the shared run, each with its own status.
    for name in ["tracked", "report"] {
        let entry = p.schedule_entry(name).await;
        assert_eq!(entry["last_run"], handle.as_str(), "{entry}");
        assert_eq!(entry["last_status"], "complete", "{entry}");
    }
}

/// Two scheduled assets (no task) reading the same upstream.
const SHARED_BY_ASSETS: &str = r#"
@asset()
def base() -> dict:
    count_run("base")
    return {"n": 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def left(base: dict) -> dict:
    return {"n": base["n"] + 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def right(base: dict) -> dict:
    return {"n": base["n"] + 2}
"#;

#[tokio::test(flavor = "multi_thread")]
async fn jobs_caught_up_at_startup_share_one_run() {
    let p = Project::new(SHARED_BY_ASSETS, |_| {}).await;
    let now = Utc.with_ymd_and_hms(2026, 7, 2, 5, 0, 0).single().unwrap();
    // Both jobs last fired an hour ago: the server was down over their ticks.
    let db_path = p.scheduler.db_path.clone().expect("durability is on");
    for name in ["left", "right"] {
        db::upsert_schedule_state(&db_path, &p.id(name), now.timestamp() - 3600)
            .await
            .unwrap();
    }

    p.scheduler.catch_up(now.fixed_offset()).await;

    let handle = p.job("left").handle;
    assert_eq!(p.job("right").handle, handle, "one catch-up run for both");
    p.until_run_is_over(&handle).await;
    assert_eq!(p.runs_of("base"), 1, "the shared upstream ran once");

    // Assets only: recorded as `barca get left,right` would be.
    let history = p.history().await;
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].command, "get");
    assert_eq!(history[0].status, "success");

    // The catch-up was recorded as each job's last fire time.
    let saved = db::get_schedule_state(&db_path).await.unwrap();
    for name in ["left", "right"] {
        assert_eq!(saved.get(&p.id(name)), Some(&now.timestamp()), "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_never_seen_before_is_anchored_not_fired_at_startup() {
    let p = Project::new(SHARED_BY_ASSETS, |_| {}).await;
    let now = Utc.with_ymd_and_hms(2026, 7, 2, 5, 0, 0).single().unwrap();
    p.scheduler.catch_up(now.fixed_offset()).await;
    for name in ["left", "right"] {
        let record = p.scheduler.ledger.get(&p.id(name));
        assert_eq!(record.last_run, None, "{name} did not fire");
        assert_eq!(record.last_fired, Some(now.timestamp()), "{name}");
    }
    assert!(p.state.runs.is_empty());
}

/// Four jobs on one upstream: one fails, one is downstream of the failure, and two neither
/// fail nor depend on it.
const ONE_FAILS: &str = r#"
@asset()
def base() -> dict:
    count_run("base")
    return {"n": 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def good(base: dict) -> dict:
    return {"n": 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def broken(base: dict) -> dict:
    raise RuntimeError("boom")


@task(freshness=Schedule("* * * * * *"), inputs={"broken": broken})
def after_broken(broken: dict) -> None:
    count_run("after_broken")


@task(freshness=Schedule("* * * * * *"), inputs={"good": good})
def report(good: dict) -> None:
    count_run("report")
"#;

#[tokio::test(flavor = "multi_thread")]
async fn when_one_job_fails_every_surface_says_the_run_failed_and_which_job_did() {
    let p = Project::new(ONE_FAILS, |_| {}).await;
    p.tick(0).await;
    let handle = p.job("good").handle;
    for name in ["broken", "after_broken", "report"] {
        assert_eq!(p.job(name).handle, handle, "{name} shares the run");
    }
    p.until_run_is_over(&handle).await;

    // The failure stopped only what depends on it.
    assert_eq!(p.runs_of("report"), 1, "an unrelated job still ran");
    assert_eq!(p.runs_of("after_broken"), 0, "downstream of the failure");

    // 1. `/status`: failed, saying which targets, with every target's outcome.
    let status = p.get(&format!("/status/{handle}")).await;
    assert_eq!(status["status"], "failed", "{status}");
    // The failed targets are named in the order the jobs were fired.
    let failed: Vec<&str> = p
        .scheduler
        .jobs
        .iter()
        .map(|j| j.id.as_str())
        .filter(|id| id.ends_with("broken"))
        .collect();
    assert_eq!(
        status["error"],
        format!("2 of 4 targets failed: {}", failed.join(", ")).as_str()
    );
    let targets = &status["result"]["targets"];
    let target = |name: &str| targets[p.id(name).as_str()].clone();
    assert_eq!(target("good")["status"], "success");
    assert_eq!(target("report")["status"], "success");
    assert_eq!(target("broken")["status"], "failed");
    assert_eq!(target("broken")["failed_node"], p.id("broken").as_str());
    let error = target("broken")["error"].as_str().unwrap().to_string();
    assert!(error.contains("RuntimeError: boom"), "{error}");
    // A job downstream of the failure names the step that failed.
    assert_eq!(target("after_broken")["status"], "failed");
    assert_eq!(
        target("after_broken")["failed_node"],
        p.id("broken").as_str()
    );

    // 2. Events: the run finished not ok, and each target says how it ended.
    let events = p.events(&handle);
    assert_eq!(
        events.last().unwrap(),
        &serde_json::json!({"type": "run_finished", "run_id": handle, "ok": false})
    );
    let mut finished = p.finished_targets(&handle);
    finished.sort();
    let mut expected = vec![
        (p.id("good"), true),
        (p.id("report"), true),
        (p.id("broken"), false),
        (p.id("after_broken"), false),
    ];
    expected.sort();
    assert_eq!(finished, expected);

    // 3. History: one row, failed.
    let history = p.history().await;
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(
        history[0].run_id,
        status["result"]["run_id"].as_str().unwrap()
    );
    assert_eq!(history[0].status, "failed");
    assert_eq!(history[0].command, "serve");

    // 4. `/schedule`: the same run for every job, and each job's own status.
    for (name, expected) in [
        ("good", "complete"),
        ("report", "complete"),
        ("broken", "failed"),
        ("after_broken", "failed"),
    ] {
        let entry = p.schedule_entry(name).await;
        assert_eq!(entry["last_run"], handle.as_str(), "{entry}");
        assert_eq!(entry["last_status"], expected, "{name}: {entry}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_failing_alone_reports_the_same_way() {
    // The same failure outside a shared run: `failed` everywhere too.
    let p = Project::new(
        r#"
@asset(freshness=Schedule("* * * * * *"))
def broken() -> dict:
    raise RuntimeError("boom")
"#,
        |_| {},
    )
    .await;
    p.tick(0).await;
    let handle = p.job("broken").handle;
    p.until_run_is_over(&handle).await;

    let status = p.get(&format!("/status/{handle}")).await;
    assert_eq!(status["status"], "failed");
    assert_eq!(status["result"], Value::Null);
    assert_eq!(
        p.events(&handle).last().unwrap(),
        &serde_json::json!({"type": "run_finished", "run_id": handle, "ok": false})
    );
    assert_eq!(p.finished_targets(&handle), vec![(p.id("broken"), false)]);
    let history = p.history().await;
    assert_eq!(
        (history[0].status.as_str(), history[0].command.as_str()),
        ("failed", "get")
    );
    let entry = p.schedule_entry("broken").await;
    assert_eq!(entry["last_status"], "failed");
}

/// Jobs on one upstream: a fast task, a fast asset, an asset that fails, and a task that stays
/// running until the test releases it.
const FAST_AND_SLOW: &str = r#"
@asset()
def base() -> dict:
    count_run("base")
    return {"n": 1}


@task(freshness=Schedule("* * * * * *"), inputs={"base": base})
def fast(base: dict) -> None:
    count_run("fast")


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def quick(base: dict) -> dict:
    count_run("quick")
    return {"n": base["n"] + 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def broken(base: dict) -> dict:
    count_run("broken")
    raise RuntimeError("boom")


@task(freshness=Schedule("* * * * * *"), inputs={"base": base})
def slow(base: dict) -> None:
    count_run("slow")
    wait_for_release()
"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_fast_job_keeps_ticking_while_a_job_it_fired_with_is_still_running() {
    let p = Project::new(FAST_AND_SLOW, |_| {}).await;
    p.tick(0).await;
    let first = p.job("slow").handle;
    for name in ["fast", "quick", "broken"] {
        assert_eq!(p.job(name).handle, first, "{name} shares the run");
    }

    // `fast` and `quick` end and `broken` fails; `slow` holds the shared run open.
    p.until("fast, quick and broken to end in the first run", |p| {
        p.job("fast").status == RunStatus::Complete
            && p.job("quick").status == RunStatus::Complete
            && p.job("broken").status == RunStatus::Failed
    })
    .await;
    p.until("slow to start", |p| p.runs_of("slow") == 1).await;
    assert_eq!(p.run_status(&first), RunStatus::Running);
    assert_eq!(p.job("slow").status, RunStatus::Running);
    // `/schedule` already tells them apart, while the run is going.
    assert_eq!(p.schedule_entry("fast").await["last_status"], "complete");
    assert_eq!(p.schedule_entry("broken").await["last_status"], "failed");
    assert_eq!(p.schedule_entry("slow").await["last_status"], "running");
    // So do the run's events: the three that ended were announced, `slow` was not.
    let mut announced = p.finished_targets(&first);
    announced.sort();
    let mut expected = vec![
        (p.id("fast"), true),
        (p.id("quick"), true),
        (p.id("broken"), false),
    ];
    expected.sort();
    assert_eq!(announced, expected);

    // The next tick fires the three that ended, together, and skips only `slow`.
    p.tick(1).await;
    let second = p.job("fast").handle;
    assert_ne!(second, first, "fast fired again");
    assert_eq!(p.job("quick").handle, second, "so did quick");
    assert_eq!(p.job("broken").handle, second, "and broken");
    assert_eq!(p.job("slow").handle, first, "slow's tick was skipped");
    p.until_run_is_over(&second).await;
    assert_eq!(p.runs_of("fast"), 2, "a task runs on every tick");
    assert_eq!(p.runs_of("broken"), 2);
    assert_eq!(p.runs_of("slow"), 1, "slow did not overlap itself");
    assert_eq!(p.run_status(&first), RunStatus::Running);
    // A job is only said to have ended once its steps are recorded, so the run it fires into
    // next finds them cached, although the run that computed them is still going.
    assert_eq!(
        p.runs_of("base"),
        1,
        "the shared upstream was not recomputed"
    );
    assert_eq!(p.runs_of("quick"), 1, "the asset was served from cache");

    // And the tick after that, with `slow` still running.
    p.tick(2).await;
    let third = p.job("fast").handle;
    assert_ne!(third, second);
    assert_eq!(p.job("slow").handle, first);
    p.until_run_is_over(&third).await;
    assert_eq!(p.runs_of("fast"), 3);
    assert_eq!(p.runs_of("slow"), 1);
    assert_eq!(p.runs_of("base"), 1);

    // `last_run` is per job: the run each one last fired into.
    assert_eq!(p.schedule_entry("fast").await["last_run"], third.as_str());
    assert_eq!(p.schedule_entry("slow").await["last_run"], first.as_str());

    // Once `slow` ends, its next tick fires it again.
    p.release();
    p.until_run_is_over(&first).await;
    assert_eq!(p.job("slow").status, RunStatus::Complete);
    assert_eq!(
        p.run_status(&first),
        RunStatus::Failed,
        "the first run had a failed job"
    );
    p.tick(3).await;
    assert_ne!(p.job("slow").handle, first);
    let fourth = p.job("slow").handle;
    p.until_run_is_over(&fourth).await;
    assert_eq!(p.runs_of("slow"), 2);
}

/// A scheduled asset and a scheduled task on a shared upstream, and a task that stays running.
const SHARED_UPSTREAM_AND_SLOW: &str = r#"
@asset()
def base() -> dict:
    count_run("base")
    return {"n": 1}


@asset(freshness=Schedule("* * * * * *"), inputs={"base": base})
def tracked(base: dict) -> dict:
    count_run("tracked")
    return {"n": base["n"] + 1}


@task(freshness=Schedule("* * * * * *"), inputs={"base": base})
def report(base: dict) -> None:
    count_run("report")


@task(freshness=Schedule("* * * * * *"))
def slow() -> None:
    wait_for_release()
"#;

#[tokio::test(flavor = "multi_thread")]
async fn jobs_with_nothing_in_common_do_not_share_a_run() {
    let p = Project::new(SHARED_UPSTREAM_AND_SLOW, |_| {}).await;
    p.tick(0).await;
    // `tracked` and `report` read the same upstream; `slow` has nothing in common with them.
    let shared = p.job("tracked").handle;
    assert_eq!(p.job("report").handle, shared);
    let own = p.job("slow").handle;
    assert_ne!(own, shared, "slow got a run of its own");

    // So the two are not held back by it: their run ends while `slow` is still running. (In one
    // run, their steps would be in a phase behind `slow`'s and would wait for it.)
    p.until_run_is_over(&shared).await;
    assert_eq!(p.run_status(&shared), RunStatus::Complete);
    assert_eq!(p.run_status(&own), RunStatus::Running);
    assert_eq!(p.runs_of("base"), 1);

    // And `slow`'s run is an ordinary single-target run.
    p.release();
    p.until_run_is_over(&own).await;
    let status = p.get(&format!("/status/{own}")).await;
    assert_eq!(status["status"], "complete");
    assert!(status["result"].get("targets").is_none(), "{status}");
    let history = p.history().await;
    let commands: Vec<&str> = history.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(history.len(), 2, "{history:?}");
    assert!(
        commands.contains(&"run"),
        "slow alone is `run`: {commands:?}"
    );
    assert!(commands.contains(&"serve"), "the shared run: {commands:?}");
}

/// Two tasks on one upstream: one ends at once, one stays running until released.
const FAST_AND_HELD: &str = r#"
@asset()
def base() -> dict:
    return {"n": 1}


@task(freshness=Schedule("* * * * * *"), inputs={"base": base})
def fast(base: dict) -> None:
    count_run("fast")


@task(freshness=Schedule("* * * * * *"), inputs={"base": base})
def slow(base: dict) -> None:
    count_run("slow")
    wait_for_release()
"#;

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_shared_run_stops_the_jobs_still_running_in_it() {
    let p = Project::new(FAST_AND_HELD, |_| {}).await;
    p.tick(0).await;
    let handle = p.job("slow").handle;
    assert_eq!(p.job("fast").handle, handle, "one run for both");
    p.until("fast to end and slow to start", |p| {
        p.job("fast").status == RunStatus::Complete && p.runs_of("slow") == 1
    })
    .await;

    let (status, body) = p.request("DELETE", &format!("/run/{handle}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelling");
    p.until_run_is_over(&handle).await;

    let status = p.get(&format!("/status/{handle}")).await;
    assert_eq!(status["status"], "cancelled");
    assert_eq!(status["error"], "run cancelled");
    assert_eq!(status["result"], Value::Null);
    assert_eq!(
        p.events(&handle).last().unwrap(),
        &serde_json::json!({"type": "run_finished", "run_id": handle, "ok": false})
    );

    // The job whose step had ended keeps its result; the one still running was cancelled.
    assert_eq!(p.schedule_entry("fast").await["last_status"], "complete");
    assert_eq!(p.schedule_entry("slow").await["last_status"], "cancelled");

    // Tasks only: recorded as `barca run fast,slow` would be.
    let history = p.history().await;
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(
        (history[0].status.as_str(), history[0].command.as_str()),
        ("cancelled", "run")
    );

    // A cancelled job is not "still running": its next tick fires.
    p.release();
    p.tick(1).await;
    assert_ne!(p.job("slow").handle, handle);
    let next = p.job("slow").handle;
    p.until_run_is_over(&next).await;
    assert_eq!(p.run_status(&next), RunStatus::Complete);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_run_gets_the_time_limit_once_per_job() {
    // 3 seconds per target, so 6 for a run over two jobs.
    let limit = Duration::from_secs(3);
    let p = Project::new(FAST_AND_HELD, |state| state.run_timeout = limit).await;
    p.tick(0).await;
    let handle = p.job("slow").handle;
    assert_eq!(p.job("fast").handle, handle, "one run for both");
    p.until_run_is_over(&handle).await;

    let status = p.get(&format!("/status/{handle}")).await;
    assert_eq!(status["status"], "failed", "{status}");
    assert_eq!(status["error"], "run timed out after 6s");
    let ran_for = status["finished_at"].as_f64().unwrap() - status["started_at"].as_f64().unwrap();
    assert!(
        ran_for >= 6.0,
        "stopped after {ran_for}s: before both jobs had their {limit:?}"
    );
    assert_eq!(p.schedule_entry("slow").await["last_status"], "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_alone_in_its_run_keeps_the_single_time_limit() {
    let limit = Duration::from_secs(2);
    let p = Project::new(
        r#"
@task(freshness=Schedule("* * * * * *"))
def slow() -> None:
    wait_for_release()
"#,
        |state| state.run_timeout = limit,
    )
    .await;
    p.tick(0).await;
    let handle = p.job("slow").handle;
    p.until_run_is_over(&handle).await;
    let status = p.get(&format!("/status/{handle}")).await;
    assert_eq!(status["status"], "failed", "{status}");
    assert_eq!(status["error"], "run timed out after 2s");
}
