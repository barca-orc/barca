use super::lifecycle::evict_over_budget;
use super::projection::{RunList, RunsQuery, view};
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
        refreshes: false,
        files: vec![],
        status: RunStatus::Pending,
        result: None,
        error: None,
        started_at: now_ts(),
        finished_at: None,
        outcome: None,
        cancel: barca_core::CancellationToken::new(),
        cancel_requested: false,
        cancellation: None,
    }
}
#[tokio::test]
async fn queued_and_failed_durable_runs_are_visible_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let mut run = live("durable");
    state.runs.registry.insert(run.handle.clone(), run.clone());
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
    state.runs.registry.insert(run.handle.clone(), run.clone());
    let wrapping_up = view(&state, "durable").await.unwrap();
    assert_eq!(
        wrapping_up.status, "running",
        "a persisted terminal row must not stop live polling before wrap-up"
    );
    run.status = RunStatus::Failed;
    run.error = Some("boom".into());
    state.runs.registry.insert(run.handle.clone(), run);
    let Json(history) = list(State(state.clone()), Query(RunsQuery::default()))
        .await
        .unwrap();
    assert_eq!(history.total, 1);
    assert_eq!(history.runs.len(), 1);
    assert_eq!(history.runs[0].id, "durable");
    assert_eq!(history.runs[0].error.as_deref(), Some("boom"));
}

/// #941: a run in progress is listed whatever the page holds. A long run started before a
/// page of newer ones leads the list; it is counted once; a filter that leaves it out does.
#[tokio::test]
async fn a_run_in_progress_older_than_the_page_is_still_listed_first() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let path = &state.config.resolved.db_path;
    db::init_db(path).await.unwrap();
    // The long run's row is the oldest; five ticks finished after it started.
    db::create_run(path, "long", "get", "[\"p.py\"]", Some("backfill"), Some(1))
        .await
        .unwrap();
    for i in 0..5 {
        let id = format!("tick{i}");
        db::create_run(path, &id, "get", "[\"p.py\"]", Some("first"), Some(1))
            .await
            .unwrap();
        db::finish_run(path, &id, "success", 1, 0, 1.0)
            .await
            .unwrap();
    }
    let mut run = live("long");
    run.db_run_id = Some("long".into());
    run.target = Some("backfill".into());
    run.status = RunStatus::Running;
    state.runs.registry.insert(run.handle.clone(), run);
    let query = |limit, search: Option<&str>, status: Option<&str>| RunsQuery {
        limit: Some(limit),
        search: search.map(str::to_string),
        status: status.map(str::to_string),
        ..Default::default()
    };
    let ids = |list: &RunList| list.runs.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    let Json(page) = list(State(state.clone()), Query(query(2, None, None)))
        .await
        .unwrap();
    assert_eq!(
        ids(&page),
        ["long", "tick4"],
        "in progress first, then the newest"
    );
    assert_eq!(page.runs[0].status, "running");
    assert_eq!(page.total, 6, "counted once");
    assert!(page.truncated);
    // The filters are the server's: a search keeps it or leaves it out, and counts.
    let Json(found) = list(
        State(state.clone()),
        Query(query(1, Some("BACKFILL"), None)),
    )
    .await
    .unwrap();
    assert_eq!((ids(&found), found.total), (vec!["long".to_string()], 1));
    let Json(others) = list(State(state.clone()), Query(query(2, Some("first"), None)))
        .await
        .unwrap();
    assert_eq!(ids(&others), ["tick4", "tick3"]);
    assert_eq!(others.total, 5);
    let Json(ended) = list(
        State(state.clone()),
        Query(query(10, None, Some("success"))),
    )
    .await
    .unwrap();
    assert_eq!(ended.total, 5);
    assert!(!ids(&ended).contains(&"long".to_string()));
    // A run waiting in the queue has no row yet: the filters apply to it too.
    let mut queued = live("queued");
    queued.trigger = Trigger {
        triggered_by: barca_core::outcome::TriggeredBy::Schedule,
        ..Trigger::api()
    };
    state.runs.registry.insert(queued.handle.clone(), queued);
    let hidden = RunsQuery {
        not_triggered_by: Some("schedule".into()),
        ..query(10, None, None)
    };
    let Json(mine) = list(State(state.clone()), Query(hidden)).await.unwrap();
    assert!(
        !ids(&mine).contains(&"queued".to_string()),
        "{:?}",
        ids(&mine)
    );
    let Json(all) = list(State(state.clone()), Query(query(10, None, None)))
        .await
        .unwrap();
    assert_eq!(&ids(&all)[..2], ["queued", "long"]);
    assert_eq!(all.total, 7);
    // An unknown value names the query parameter.
    let bad = RunsQuery {
        not_triggered_by: Some("cron".into()),
        ..Default::default()
    };
    let error = match list(State(state.clone()), Query(bad)).await {
        Err(crate::error::ApiError::BadRequest(message)) => message,
        _ => panic!("an unknown trigger is a 400"),
    };
    assert!(error.starts_with("not_triggered_by 'cron'"), "{error}");
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
        state.runs.registry.insert(handle.to_string(), run);
        let channel = RunChannel::new();
        channel.emit(RunEvent::Line {
            line: "x".repeat(10_000),
            when: barca_core::lines::LineWhen::Always,
        });
        state.runs.events.insert(handle.to_string(), channel);
    }
    let mut running = live("running");
    running.status = RunStatus::Running;
    state.runs.registry.insert("running".into(), running);
    // Under the budget: nothing is dropped.
    evict_over_budget(&state, 1 << 20);
    assert_eq!(state.runs.registry.len(), 6);
    // About two runs' worth: the oldest go, the newest and the running run stay.
    evict_over_budget(&state, 25_000);
    let mut kept: Vec<String> = state.runs.registry.iter().map(|r| r.key().clone()).collect();
    kept.sort();
    assert_eq!(kept, ["r4", "r5", "running"]);
    assert!(!state.runs.events.contains_key("r1") && state.runs.events.contains_key("r4"));
    // Even a budget of nothing keeps the run that finished last.
    evict_over_budget(&state, 0);
    let mut kept: Vec<String> = state.runs.registry.iter().map(|r| r.key().clone()).collect();
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

/// A run cancelled while it waited names the runs it waited behind, and the one doing the same
/// work (#925).
#[test]
fn a_queued_cancellation_names_the_same_work_already_running() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let mut same = live("same");
    same.status = RunStatus::Running;
    same.started_at = 1.0;
    let mut other = live("other");
    other.status = RunStatus::Running;
    other.target = Some("second".into());
    other.started_at = 0.5;
    state.runs.registry.insert("same".into(), same);
    state.runs.registry.insert("other".into(), other);
    state.runs.registry.insert("queued".into(), live("queued"));
    let c = super::handlers::queued_cancellation(&state, "queued", Some("token:a".into()));
    assert!(!c.started);
    assert_eq!(c.waiting_behind, ["other", "same"]);
    assert_eq!(c.same_as.as_deref(), Some("same"));
    assert_eq!(c.principal.as_deref(), Some("token:a"));
}
