use super::lifecycle::evict_over_budget;
use super::projection::{RunsQuery, view};
use super::*;
use crate::ServeConfig;
use crate::state::{RunChannel, RunState, RunStatus};
use axum::Json;
use axum::extract::{Query, State};
use barca_core::outcome::Trigger;
use barca_core::time::now_secs_f64 as now_ts;
use barca_core::{RunEvent, db};
use futures::StreamExt;
fn state(dir: &std::path::Path) -> AppState {
    let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
    resolved.db_path = dir.join("metadata.db").display().to_string();
    AppState::new(ServeConfig {
        files: vec!["p.py".into()],
        host: "127.0.0.1".parse().unwrap(),
        port: 0,
        watch: false,
        schedule: false,
        timezone: "utc".into(),
        python: "python3".into(),
        resolved,
        read_only: true,
    })
}
fn live(handle: &str) -> RunState {
    RunState {
        handle: handle.into(),
        db_run_id: None,
        command: "get".into(),
        target: Some("first".into()),
        nodes: vec![],
        trigger: Trigger::api(),
        files: vec![],
        status: RunStatus::Pending,
        result: None,
        error: None,
        started_at: now_ts(),
        finished_at: None,
        outcome: None,
        cancel: barca_core::CancellationToken::new(),
        cancel_requested: false,
    }
}
#[tokio::test]
async fn queued_and_failed_durable_runs_are_visible_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let mut run = live("durable");
    state.runs.insert(run.handle.clone(), run.clone());
    let Json(queued) = list(State(state.clone()), Query(RunsQuery::default()))
        .await
        .unwrap();
    assert_eq!(queued.runs.len(), 1);
    assert_eq!(queued.runs[0].id, "durable");
    assert_eq!(queued.runs[0].status, "pending");
    let path = &state.config.resolved.db_path;
    db::init_db(path).await.unwrap();
    db::create_run(path, "durable", "get", "[\"p.py\"]", Some("first"), Some(1))
        .await
        .unwrap();
    db::finish_run(path, "durable", "failed", 0, 0, 1.0)
        .await
        .unwrap();
    run.db_run_id = Some("durable".into());
    run.status = RunStatus::Running;
    state.runs.insert(run.handle.clone(), run.clone());
    let wrapping_up = view(&state, "durable").await.unwrap();
    assert_eq!(
        wrapping_up.status, "running",
        "a persisted terminal row must not stop live polling before wrap-up"
    );
    run.status = RunStatus::Failed;
    run.error = Some("boom".into());
    state.runs.insert(run.handle.clone(), run);
    let Json(history) = list(State(state.clone()), Query(RunsQuery::default()))
        .await
        .unwrap();
    assert_eq!(history.total, 1);
    assert_eq!(history.runs.len(), 1);
    assert_eq!(history.runs[0].id, "durable");
    assert_eq!(history.runs[0].error.as_deref(), Some("boom"));
}

#[test]
fn finished_runs_past_the_memory_budget_are_dropped_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    // Five finished runs whose event backlogs hold about 10 KB each, and one still running.
    for (i, handle) in ["r1", "r2", "r3", "r4", "r5"].iter().enumerate() {
        let mut run = live(handle);
        run.status = RunStatus::Complete;
        run.finished_at = Some(1000.0 + i as f64);
        state.runs.insert(handle.to_string(), run);
        let channel = RunChannel::new();
        channel.emit(RunEvent::Line {
            line: "x".repeat(10_000),
            when: barca_core::lines::LineWhen::Always,
        });
        state.events.insert(handle.to_string(), channel);
    }
    let mut running = live("running");
    running.status = RunStatus::Running;
    state.runs.insert("running".into(), running);
    // Under the budget: nothing is dropped.
    evict_over_budget(&state, 1 << 20);
    assert_eq!(state.runs.len(), 6);
    // About two runs' worth: the oldest go, the newest and the running run stay.
    evict_over_budget(&state, 25_000);
    let mut kept: Vec<String> = state.runs.iter().map(|r| r.key().clone()).collect();
    kept.sort();
    assert_eq!(kept, ["r4", "r5", "running"]);
    assert!(!state.events.contains_key("r1") && state.events.contains_key("r4"));
    // Even a budget of nothing keeps the run that finished last.
    evict_over_budget(&state, 0);
    let mut kept: Vec<String> = state.runs.iter().map(|r| r.key().clone()).collect();
    kept.sort();
    assert_eq!(kept, ["r5", "running"]);
}

#[tokio::test]
async fn a_slow_subscriber_and_a_reconnect_receive_every_event_once() {
    let channel = RunChannel::new();
    // Far more than any buffer a subscriber could have had: none is dropped.
    let total = 5_000;
    let mut slow = Box::pin(channel.subscribe(0));
    for i in 0..total {
        channel.emit(RunEvent::log(
            "p.py:a".into(),
            i.to_string(),
            barca_core::LogStream::Stdout,
        ));
    }
    // The first reader takes a few events and goes away, as a dropped connection does.
    let mut first = Box::pin(channel.subscribe(0));
    for want in 0..3 {
        assert_eq!(first.next().await.unwrap().0, want);
    }
    drop(first);
    channel.emit(RunEvent::RunFinished {
        run_id: "r".into(),
        ok: true,
        outcome: None,
    });
    let reconnect: Vec<_> = channel.subscribe(0).collect().await;
    let lagged: Vec<_> = slow.as_mut().collect().await;
    for got in [reconnect, lagged] {
        let ids: Vec<usize> = got.iter().map(|(n, _)| *n).collect();
        assert_eq!(ids, (0..=total).collect::<Vec<_>>());
        assert!(got.last().unwrap().1.is_terminal(), "ends at run_finished");
    }
    // A channel dropped before its run finished ends its streams too.
    let orphan = RunChannel::new();
    orphan.emit(RunEvent::RunStarted { run_id: "r".into() });
    let stream = orphan.subscribe(0);
    drop(orphan);
    assert_eq!(stream.collect::<Vec<_>>().await.len(), 1);
}
