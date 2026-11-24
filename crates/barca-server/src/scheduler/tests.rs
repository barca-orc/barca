use super::*;
use crate::ServeConfig;
use crate::run_model::{RunState, RunStatus};
use barca_service::loaded::describe_schedule;
use barca_service::results::AssetSummary;
use barca_service::schedule::{jobs_from_summaries, next_fire};
use barca_service::{CronExpr, Freshness, NodeKind, db};
use chrono::{DateTime, Local, TimeZone, Timelike, Utc};
use croner::Cron;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

/// Whether a scheduled tick elapsed between `last_fired` and `now` — i.e. the
/// next cron occurrence strictly after `last_fired` is already in the past. This
/// drives the single catch-up run after the daemon was down. Pure and generic
/// over timezone so it is testable and reusable once a `--timezone` is honored.
fn needs_catchup<Tz: TimeZone>(cron: &Cron, last_fired: &DateTime<Tz>, now: &DateTime<Tz>) -> bool {
    match cron.find_next_occurrence(last_fired, false) {
        Ok(next) => next <= *now,
        Err(_) => false,
    }
}

fn summary(id: &str, kind: NodeKind, cron: &str) -> AssetSummary {
    AssetSummary {
        id: id.to_string(),
        kind,
        freshness: Freshness::Schedule(CronExpr(cron.to_string())),
        inputs: vec![],
        env: vec![],
        file: "p.py".into(),
        line: 1,
    }
}

/// Build a single `ScheduledJob` through the real parse path.
fn job(id: &str, kind: NodeKind, cron: &str) -> ScheduledJob {
    jobs_from_summaries(vec![summary(id, kind, cron)])
        .pop()
        .expect("valid cron")
}

fn at(hour: u32, minute: u32) -> DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 7, 2, hour, minute, 0)
        .single()
        .unwrap()
}

fn at_s(hour: u32, minute: u32, sec: u32) -> DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 7, 2, hour, minute, sec)
        .single()
        .unwrap()
}

/// A minimal `AppState`. `files` points at a nonexistent path so any run this
/// triggers fails fast in the background (DAG read error) with no side effects.
fn app_state() -> AppState {
    let mut resolved =
        barca_service::config::resolve_in(None, std::path::Path::new("/nonexistent")).unwrap();
    // The run queue records each run it accepts: in a directory of its own.
    let dir = tempfile::tempdir().unwrap();
    resolved.db_path = dir.path().join("metadata.db").display().to_string();
    std::mem::forget(dir);
    AppState::new(ServeConfig {
        files: vec!["/nonexistent-barca-test-file.py".to_string()],
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        watch: false,
        schedule: true,
        timezone: "local".to_string(),
        python: std::path::PathBuf::from("python3"),
        resolved,
        read_only: false,
    })
}

/// Record a run as the server would: `command` is `get` or `run`, `target` the name as
/// given and `node` the full id it resolved to (`None` for `POST /run`, every asset).
fn insert_run(
    state: &AppState,
    handle: &str,
    status: RunStatus,
    command: &str,
    target: Option<&str>,
    node: Option<&str>,
    started_at: f64,
) {
    state.runs.registry.insert(
        handle.to_string(),
        RunState {
            handle: handle.to_string(),
            db_run_id: None,
            command: command.into(),
            target: target.map(str::to_string),
            nodes: node.map(str::to_string).into_iter().collect(),
            trigger: barca_service::outcome::Trigger::api(),
            queue: Default::default(),
            refreshes: false,
            files: Vec::new(),
            status,
            result: None,
            error: None,
            started_at,
            finished_at: None,
            outcome: None,
            cancel: barca_service::CancellationToken::new(),
            cancel_requested: false,
            cancellation: None,
        },
    );
}

#[tokio::test]
async fn startup_db_admission_cannot_acknowledge_a_source_generation_it_did_not_load() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pipeline.py");
    let initial = "from barca import task, Schedule\n@task(freshness=Schedule('0 5 1 1 *'))\ndef healthy(): pass\n";
    std::fs::write(&source, initial).unwrap();
    let mut config = app_state().config.as_ref().clone();
    config.files = vec![source.to_string_lossy().into()];
    config.watch = true;
    config.resolved.db_path = dir.path().join("metadata.db").to_string_lossy().into();
    let state = AppState::new(config);
    let held = db::db_guard().await;
    let task = spawn(state.clone());
    // The initial selected graph is published before scheduler DB admission.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.reads.sources.loaded.lock().await.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial graph not loaded");
    std::fs::write(
        &source,
        format!("{initial}\n@task(freshness=Schedule('0 5 1 1 *'))\ndef repaired(): pass\n"),
    )
    .unwrap();
    // This is the existing watch event signal, while startup still waits for the DB.
    state
        .reads
        .sources
        .generation
        .fetch_add(1, Ordering::Relaxed);
    assert_eq!(
        barca_service::queries::list_assets_from_dag(&state.reads.loaded_dag().await.unwrap())
            .len(),
        2
    );
    let health = || crate::state_routes::health(axum::extract::State(state.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while health()
            .await
            .scheduler_liveness
            .as_ref()
            .unwrap()
            .operation
            .as_deref()
            != Some("init_db")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduler did not reach init_db");
    // Hold through a startup deadline. A busy ledger is retried by this task.
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let h = health().await;
            let live = h.scheduler_liveness.as_ref().unwrap();
            assert!(h.scheduler_enabled);
            assert_eq!(live.restart_count, 0);
            if live
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("retrying init_db"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scheduler retried startup metadata");
    drop(held);
    let repaired = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if state
                .scheduling
                .jobs
                .read()
                .unwrap()
                .iter()
                .any(|job| job.id.ends_with(":repaired"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(
        health().await.status,
        barca_service::served::HealthStatus::Ok
    );
    task.abort();
    let _ = task.await;
    repaired.expect("startup swallowed the repaired source generation");
}

#[test]
fn invalid_and_empty_crons_are_dropped() {
    let summaries = vec![
        summary("f.py:good", NodeKind::Asset, "0 5 * * *"),
        summary("f.py:empty", NodeKind::Asset, ""),
        summary("f.py:garbage", NodeKind::Asset, "not a cron"),
    ];
    let jobs = jobs_from_summaries(summaries);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, "f.py:good");
    assert_eq!(jobs[0].kind, NodeKind::Asset);
}

#[test]
fn non_scheduled_freshness_is_ignored() {
    let summaries = vec![AssetSummary {
        id: "f.py:always".into(),
        kind: NodeKind::Asset,
        freshness: Freshness::Always,
        inputs: vec![],
        env: vec![],
        file: "p.py".into(),
        line: 1,
    }];
    assert!(jobs_from_summaries(summaries).is_empty());
}

#[test]
fn daily_cron_fires_only_at_its_minute() {
    let jobs = jobs_from_summaries(vec![summary("f.py:daily", NodeKind::Asset, "0 5 * * *")]);
    assert_eq!(due_jobs(&at(5, 0), &jobs).len(), 1, "05:00 should fire");
    assert!(
        due_jobs(&at(5, 1), &jobs).is_empty(),
        "05:01 should not fire"
    );
    assert!(
        due_jobs(&at(6, 0), &jobs).is_empty(),
        "06:00 should not fire"
    );
}

#[test]
fn step_cron_fires_on_multiples() {
    let jobs = jobs_from_summaries(vec![summary("f.py:poll", NodeKind::Sensor, "*/5 * * * *")]);
    assert_eq!(due_jobs(&at(5, 0), &jobs).len(), 1);
    assert_eq!(due_jobs(&at(5, 5), &jobs).len(), 1);
    assert!(due_jobs(&at(5, 3), &jobs).is_empty());
}

#[test]
fn five_field_cron_pinned_to_second_zero() {
    // A 5-field cron has its seconds field pinned to `0` by the parser, so it
    // matches only at second 0 of its minute — NOT at other seconds. This is
    // the regression guard that the per-second tick loop never fires a
    // 5-field job 60× within its matching minute.
    let jobs = jobs_from_summaries(vec![summary("f.py:daily", NodeKind::Asset, "0 5 * * *")]);
    assert_eq!(due_jobs(&at_s(5, 0, 0), &jobs).len(), 1, "05:00:00 fires");
    assert!(
        due_jobs(&at_s(5, 0, 30), &jobs).is_empty(),
        "05:00:30 does not fire"
    );
    assert!(
        due_jobs(&at_s(5, 0, 42), &jobs).is_empty(),
        "05:00:42 does not fire"
    );
}

#[test]
fn six_field_seconds_cron_parses() {
    // The shared parse helper must accept a 6-field cron in the *execution*
    // path (jobs_from_summaries), not just in CronExpr::validate — this was
    // rejected at parse time before sub-minute support (issue #109).
    let jobs = jobs_from_summaries(vec![summary("f.py:tick", NodeKind::Task, "*/5 * * * * *")]);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, NodeKind::Task);
}

#[test]
fn six_field_cron_fires_on_second_multiples() {
    // A 6-field cron fires at its declared seconds cadence, not once a minute.
    let jobs = jobs_from_summaries(vec![summary("f.py:tick", NodeKind::Task, "*/5 * * * * *")]);
    assert_eq!(due_jobs(&at_s(5, 0, 0), &jobs).len(), 1, ":00 fires");
    assert_eq!(due_jobs(&at_s(5, 0, 5), &jobs).len(), 1, ":05 fires");
    assert!(
        due_jobs(&at_s(5, 0, 3), &jobs).is_empty(),
        ":03 does not fire"
    );
}

// ─── startup log wording ───────────────────────────────────────────────

#[test]
fn the_schedule_line_names_each_kind_with_its_count() {
    let asset = || job("f.py:a", NodeKind::Asset, "0 5 * * *");
    let sensor = || job("f.py:s", NodeKind::Sensor, "0 5 * * *");
    let task = || job("f.py:t", NodeKind::Task, "0 5 * * *");
    assert_eq!(count_by_kind(&[task()]), "1 task");
    assert_eq!(count_by_kind(&[asset()]), "1 asset");
    assert_eq!(count_by_kind(&[asset(), asset()]), "2 assets");
    assert_eq!(count_by_kind(&[task(), asset()]), "1 asset and 1 task");
    assert_eq!(
        count_by_kind(&[task(), sensor(), asset(), sensor(), task()]),
        "1 asset, 2 sensors and 2 tasks"
    );
}

// ─── catch-up detection ────────────────────────────────────────────────

#[test]
fn needs_catchup_true_when_a_tick_was_missed() {
    let j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    // Last fired yesterday 05:00; now is today 06:00 → today's 05:00 was missed.
    let last = at(5, 0) - chrono::Duration::days(1);
    assert!(needs_catchup(&j.cron, &last, &at(6, 0)));
}

#[test]
fn needs_catchup_false_when_no_tick_elapsed() {
    let j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    // Fired at today 05:00; now 05:30 — next occurrence is tomorrow, not past.
    let last = at(5, 0);
    let now = Local
        .with_ymd_and_hms(2026, 7, 2, 5, 30, 0)
        .single()
        .unwrap();
    assert!(!needs_catchup(&j.cron, &last, &now));
}

#[test]
fn needs_catchup_true_after_long_downtime() {
    // A */5 job down for an hour: a tick is certainly in the past → catch up once.
    let j = job("f.py:poll", NodeKind::Sensor, "*/5 * * * *");
    let last = at(5, 0);
    assert!(needs_catchup(&j.cron, &last, &at(6, 0)));
}

// ─── timezone handling ─────────────────────────────────────────────────

#[test]
fn zone_parse_accepts_local_utc_and_iana_names() {
    assert!(matches!(Zone::parse("local"), Ok(Zone::Local)));
    assert!(matches!(Zone::parse("Local"), Ok(Zone::Local)));
    assert!(matches!(Zone::parse("utc"), Ok(Zone::Utc)));
    assert!(matches!(Zone::parse("UTC"), Ok(Zone::Utc)));
    assert!(matches!(Zone::parse(" utc "), Ok(Zone::Utc)));
    assert!(matches!(
        Zone::parse("America/New_York"),
        Ok(Zone::Named(chrono_tz::America::New_York))
    ));
    assert!(matches!(Zone::parse("Etc/UTC"), Ok(Zone::Named(_))));
}

#[test]
fn zone_parse_rejects_anything_else_and_names_it() {
    // An unknown zone used to fall back to local time with one line on stderr (#289).
    for bad in ["Not/AZone", "", "america/new_york", "EST5", "+02:00"] {
        let err = Zone::parse(bad).unwrap_err();
        assert!(
            err.contains(&format!("unknown timezone '{bad}'")),
            "{bad:?}: {err}"
        );
        assert!(err.contains("America/New_York"), "{err}");
    }
}

/// 2026-10-08 14:33:00 UTC.
fn an_instant() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 8, 14, 33, 0)
        .single()
        .unwrap()
}

#[test]
fn the_next_fire_time_is_computed_in_the_given_zone() {
    let j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    let epoch = |zone: &Zone| next_fire(&j.cron, zone, an_instant()).unwrap().timestamp();
    let utc_5am = Utc.with_ymd_and_hms(2026, 10, 9, 5, 0, 0).single().unwrap();
    assert_eq!(epoch(&Zone::Utc), utc_5am.timestamp());
    // 05:00 in Kolkata (UTC+5:30) is 23:30 UTC the day before; in Honolulu (UTC-10), 15:00.
    let kolkata = Zone::parse("Asia/Kolkata").unwrap();
    assert_eq!(epoch(&kolkata), utc_5am.timestamp() - 5 * 3600 - 1800);
    let honolulu = Zone::parse("Pacific/Honolulu").unwrap();
    assert_eq!(
        epoch(&honolulu),
        Utc.with_ymd_and_hms(2026, 10, 8, 15, 0, 0)
            .single()
            .unwrap()
            .timestamp()
    );
}

#[test]
fn get_schedule_reports_next_fire_in_the_servers_zone() {
    // It used to compute it in local time whatever `--timezone` said (#289). Two zones, so
    // the test cannot pass by running on a machine whose local zone is one of them.
    for (zone, want) in [
        ("Asia/Kolkata", "2026-10-08T23:30:00Z"),
        ("Pacific/Honolulu", "2026-10-08T15:00:00Z"),
        ("utc", "2026-10-09T05:00:00Z"),
    ] {
        let mut config = (*app_state().config).clone();
        config.timezone = zone.to_string();
        let st = AppState::new(config);
        publish_registry(
            &st,
            &[job("f.py:daily", NodeKind::Task, "0 5 * * *")],
            &Bookkeeping::default(),
        );
        let body = serde_json::json!(crate::schedule_view::schedule_at(
            &st,
            an_instant(),
            &Default::default()
        ));
        assert_eq!(body[0]["next_fire"], want, "{zone}: {body}");
        assert_eq!(body[0]["id"], "f.py:daily");
    }
}

#[test]
fn zone_timestamp_round_trips_epoch() {
    assert_eq!(Zone::Utc.timestamp(0).timestamp(), 0);
    assert_eq!(Zone::Local.timestamp(1_000).timestamp(), 1_000);
}

#[test]
fn due_jobs_works_in_a_fixed_offset_zone() {
    use chrono::FixedOffset;
    // Matching must work for the DateTime<FixedOffset> the scheduler actually
    // uses in production (not just the Local type the other tests exercise).
    let jobs = jobs_from_summaries(vec![summary("f.py:daily", NodeKind::Asset, "0 5 * * *")]);
    let tz = FixedOffset::east_opt(5 * 3600).unwrap();
    let at_five = tz.with_ymd_and_hms(2026, 7, 2, 5, 0, 0).single().unwrap();
    assert_eq!(due_jobs(&at_five, &jobs).len(), 1);
    let at_six = tz.with_ymd_and_hms(2026, 7, 2, 6, 0, 0).single().unwrap();
    assert!(due_jobs(&at_six, &jobs).is_empty());
}

#[test]
fn scheduled_task_keeps_task_kind() {
    // Discovery must preserve kind so `trigger` can route tasks to the run path.
    let jobs = jobs_from_summaries(vec![summary(
        "f.py:cleanup",
        NodeKind::Task,
        "*/10 * * * *",
    )]);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, NodeKind::Task);
}

// ─── second-boundary arithmetic ────────────────────────────────────────

#[test]
fn millis_to_next_second_from_boundary_and_mid() {
    let with_nanos = |nanos: u32| {
        Local
            .with_ymd_and_hms(2026, 7, 2, 5, 0, 0)
            .single()
            .unwrap()
            .with_nanosecond(nanos)
            .unwrap()
    };
    assert_eq!(millis_to_next_second(&with_nanos(0)), 1_005);
    assert_eq!(millis_to_next_second(&with_nanos(500_000_000)), 505);
    assert_eq!(millis_to_next_second(&with_nanos(999_000_000)), 6);
}

// ─── in-flight detection ───────────────────────────────────────────────

#[test]
fn a_run_of_the_node_is_in_flight_only_while_pending_or_running() {
    let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    for (status, in_flight) in [
        (RunStatus::Pending, true),
        (RunStatus::Running, true),
        (RunStatus::Success, false),
        (RunStatus::Failed, false),
        (RunStatus::Cancelled, false),
    ] {
        let st = app_state();
        insert_run(
            &st,
            "h",
            status,
            "get",
            Some("f.py:daily"),
            Some("f.py:daily"),
            1.0,
        );
        assert_eq!(
            run_in_flight_for(&st, &daily).is_some(),
            in_flight,
            "{status:?}"
        );
    }
    assert_eq!(run_in_flight_for(&app_state(), &daily), None, "no runs");
}

#[test]
fn a_run_started_over_http_by_short_name_counts_for_the_scheduled_node() {
    // `POST /get/daily` records the name as given and the node it resolved to. The
    // scheduler only ever issued its own handles before, so this run was invisible (#432).
    let st = app_state();
    let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    insert_run(
        &st,
        "api",
        RunStatus::Running,
        "get",
        Some("daily"),
        Some("f.py:daily"),
        1.0,
    );
    assert_eq!(run_in_flight_for(&st, &daily).as_deref(), Some("api"));

    // `POST /run/{task}` for a scheduled task.
    let weekly = job("f.py:weekly", NodeKind::Task, "0 5 * * *");
    insert_run(
        &st,
        "task",
        RunStatus::Pending,
        "run",
        Some("weekly"),
        Some("f.py:weekly"),
        1.0,
    );
    assert_eq!(run_in_flight_for(&st, &weekly).as_deref(), Some("task"));
}

#[test]
fn a_run_of_another_node_does_not_count() {
    let st = app_state();
    insert_run(
        &st,
        "other",
        RunStatus::Running,
        "get",
        Some("f.py:other"),
        Some("f.py:other"),
        1.0,
    );
    let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    assert_eq!(run_in_flight_for(&st, &daily), None);
}

#[test]
fn a_run_of_every_asset_counts_for_scheduled_assets_and_sensors_not_tasks() {
    // `POST /run` gets every asset and sensor and skips tasks.
    let st = app_state();
    insert_run(&st, "all", RunStatus::Running, "get", None, None, 1.0);
    for kind in [NodeKind::Asset, NodeKind::Sensor] {
        let j = job("f.py:n", kind, "0 5 * * *");
        assert_eq!(
            run_in_flight_for(&st, &j).as_deref(),
            Some("all"),
            "{kind:?}"
        );
    }
    let task = job("f.py:t", NodeKind::Task, "0 5 * * *");
    assert_eq!(run_in_flight_for(&st, &task), None);
}

#[test]
fn the_oldest_run_in_flight_is_the_one_named() {
    let st = app_state();
    insert_run(
        &st,
        "newer",
        RunStatus::Pending,
        "get",
        Some("daily"),
        Some("f.py:daily"),
        20.0,
    );
    insert_run(
        &st,
        "older",
        RunStatus::Running,
        "get",
        Some("f.py:daily"),
        Some("f.py:daily"),
        10.0,
    );
    let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    assert_eq!(run_in_flight_for(&st, &daily).as_deref(), Some("older"));
}

#[test]
fn a_skipped_tick_is_reported_with_status_skipped_and_the_run_in_flight() {
    let task = job("f.py:weekly", NodeKind::Task, "0 5 * * *");
    let report = skip_report(&task, "in_flight", Some("h1"), 1_000);
    assert_eq!(report.status, "skipped");
    assert_eq!(report.reason.as_deref(), Some("in_flight"));
    assert_eq!(report.in_flight_run.as_deref(), Some("h1"));
    assert_eq!(report.command, "run");
    assert_eq!(report.job, "f.py:weekly");
    assert_eq!(report.start_unix_ns, 1_000_000_000_000);
    assert!(report.steps.is_empty());
    assert_ne!(report.run_id, "h1", "a skip is its own trace");
    assert_eq!(
        report.trigger.triggered_by,
        barca_service::outcome::TriggeredBy::Schedule
    );
    assert_eq!(report.trigger.schedule_id.as_deref(), Some("f.py:weekly"));
    let paused = skip_report(&task, "paused", None, 1_000);
    assert_eq!(paused.reason.as_deref(), Some("paused"));
    assert!(paused.in_flight_run.is_none());
}

#[tokio::test]
async fn a_skip_is_published_to_get_schedule() {
    let st = app_state();
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let mut book = Bookkeeping::default();
    publish_registry(&st, &jobs, &book);
    let body = serde_json::json!(crate::schedule_view::schedule_at(
        &st,
        an_instant(),
        &Default::default()
    ));
    assert_eq!(body[0]["last_skipped"], serde_json::Value::Null);
    assert_eq!(body[0]["skipped_for"], serde_json::Value::Null);

    record_skip(&mut book, &jobs[0], Some("api"), 1_234, "scheduled run");
    publish_registry(&st, &jobs, &book);
    let body = serde_json::json!(crate::schedule_view::schedule_at(
        &st,
        an_instant(),
        &Default::default()
    ));
    assert_eq!(body[0]["last_skipped"], "1970-01-01T00:20:34Z");
    assert_eq!(body[0]["skipped_for"], "api");
    assert_eq!(body[0]["skip_reason"], "in_flight");
    assert_eq!(body[0]["paused"], false);
}

// ─── per-tick planning (the overlap-skip guard) ────────────────────────

fn fired_ids(plan: &[TickAction]) -> Vec<String> {
    plan.iter()
        .filter_map(|a| match a {
            TickAction::Fire(j) => Some(j.id.clone()),
            _ => None,
        })
        .collect()
}

fn skipped_ids(plan: &[TickAction]) -> Vec<String> {
    plan.iter()
        .filter_map(|a| match a {
            TickAction::Skip { job, .. } => Some(job.id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn plan_tick_skips_a_job_at_its_max_failures_and_fires_it_below() {
    let mut j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    assert_eq!(j.max_failures, 5, "the default");
    j.max_failures = 3;
    let jobs = vec![j];
    let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| None, |_| 2);
    assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    // Failing wins over an in-flight run: nothing would fire either way.
    let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| Some("h".into()), |_| 3);
    match plan.as_slice() {
        [TickAction::Failing { job, failures: 3 }] => assert_eq!(job.id, "f.py:daily"),
        _ => panic!("expected failing"),
    }
}

fn finished(state: &AppState, handle: &str, status: RunStatus, failed: &[&str]) {
    insert_run(state, handle, status, "get", None, Some("f.py:s"), 1.0);
    let steps = failed
        .iter()
        .map(|id| barca_service::results::StepReport {
            id: id.to_string(),
            status: Some("failed".into()),
            ..Default::default()
        })
        .collect();
    state.runs.registry.get_mut(handle).unwrap().result = Some(barca_service::results::GetResult {
        run_id: "r".into(),
        elapsed_seconds: 0.0,
        steps_executed: 0,
        phases: 0,
        final_output: None,
        steps,
        warnings: Vec::new(),
        partitions: None,
        key_summary: None,
        cache: None,
        uploads: None,
        metadata_seconds: None,
        partition_selection: None,
    });
}

#[test]
fn a_tick_run_counts_once_it_finished_and_not_when_cancelled() {
    let st = app_state();
    let j = job("f.py:s", NodeKind::Sensor, "* * * * *");
    insert_run(
        &st,
        "p",
        RunStatus::Running,
        "get",
        None,
        Some("f.py:s"),
        1.0,
    );
    assert_eq!(tick_outcome(&st.runs.registry.get("p").unwrap(), &j), None);
    finished(&st, "c", RunStatus::Cancelled, &[]);
    assert_eq!(tick_outcome(&st.runs.registry.get("c").unwrap(), &j), None);
    finished(&st, "ok", RunStatus::Success, &[]);
    assert_eq!(
        tick_outcome(&st.runs.registry.get("ok").unwrap(), &j),
        Some(vec![])
    );
    finished(&st, "f", RunStatus::Failed, &["f.py:report"]);
    assert_eq!(
        tick_outcome(&st.runs.registry.get("f").unwrap(), &j),
        Some(vec!["f.py:report".to_string()])
    );
    // A multi-target tick's failed steps come from its outcome.
    insert_run(
        &st,
        "m",
        RunStatus::Failed,
        "get",
        None,
        Some("f.py:s"),
        1.0,
    );
    st.runs.registry.get_mut("m").unwrap().outcome = Some(
        barca_service::outcome::Outcome::from_text(
            r#"{"status":"failed","steps":[{"id":"f.py:s","status":"success"},{"id":"f.py:publish","status":"failed"}],"targets":{"f.py:s":{"status":"success"},"f.py:publish":{"status":"failed","failed_node":"f.py:publish"},"f.py:late":{"status":"blocked","failed_node":"f.py:baseline"}}}"#,
        )
        .unwrap(),
    );
    assert_eq!(
        tick_outcome(&st.runs.registry.get("m").unwrap(), &j),
        Some(vec!["f.py:publish".to_string()])
    );
    // A failed run with no failed step (it could not start) failed on the scheduled node.
    finished(&st, "x", RunStatus::Failed, &[]);
    assert_eq!(
        tick_outcome(&st.runs.registry.get("x").unwrap(), &j),
        Some(vec!["f.py:s".to_string()])
    );
}

#[tokio::test]
async fn consecutive_failed_ticks_add_up_and_a_success_clears_them() {
    let st = app_state();
    let jobs = vec![job("f.py:s", NodeKind::Sensor, "* * * * *")];
    let mut book = Bookkeeping::default();
    for (n, handle) in ["a", "b"].into_iter().enumerate() {
        finished(&st, handle, RunStatus::Failed, &["f.py:report"]);
        book.last_handle.insert("f.py:s".into(), handle.into());
        assert!(count_outcomes(&st, &jobs, &mut book, None).await);
        assert_eq!(book.failures(&jobs[0]), n as u32 + 1);
        // Counted once, however many ticks look at it.
        assert!(!count_outcomes(&st, &jobs, &mut book, None).await);
    }
    assert_eq!(
        book.failures["f.py:s"].failed_nodes,
        vec!["f.py:report".to_string()]
    );
    finished(&st, "c", RunStatus::Success, &[]);
    book.last_handle.insert("f.py:s".into(), "c".into());
    assert!(count_outcomes(&st, &jobs, &mut book, None).await);
    assert_eq!(book.failures(&jobs[0]), 0);
}

#[test]
fn plan_tick_fires_when_nothing_is_in_flight() {
    let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| None, |_| 0);
    assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
}

#[test]
fn plan_tick_skips_when_a_run_of_the_node_is_in_flight() {
    let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| (j.id == "f.py:daily").then(|| "h1".to_string()),
        |_| 0,
    );
    assert!(fired_ids(&plan).is_empty());
    assert_eq!(skipped_ids(&plan), vec!["f.py:daily"]);
    // The skip carries the offending handle for the log line.
    match &plan[0] {
        TickAction::Skip { handle, .. } => assert_eq!(handle, "h1"),
        _ => panic!("expected skip"),
    }
}

#[test]
fn plan_tick_skips_for_a_run_started_over_http() {
    // End to end through the real lookup: the scheduler never issued this handle.
    let st = app_state();
    insert_run(
        &st,
        "api",
        RunStatus::Running,
        "get",
        Some("daily"),
        Some("f.py:daily"),
        1.0,
    );
    let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| run_in_flight_for(&st, j),
        |_| 0,
    );
    assert_eq!(skipped_ids(&plan), vec!["f.py:daily"]);

    // Once it finishes, the next due tick fires.
    st.runs.registry.get_mut("api").unwrap().status = RunStatus::Success;
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| run_in_flight_for(&st, j),
        |_| 0,
    );
    assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
}

#[test]
fn plan_tick_ignores_jobs_not_due_this_minute() {
    let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let plan = plan_tick(&at(6, 0), &jobs, |_| false, |_| Some("h".into()), |_| 0);
    assert!(plan.is_empty(), "06:00 is not the job's minute");
}

#[test]
fn plan_tick_mixes_fire_and_skip_across_jobs() {
    let jobs = vec![
        job("f.py:a", NodeKind::Asset, "0 5 * * *"),
        job("f.py:b", NodeKind::Task, "0 5 * * *"),
    ];
    // `a` has a run still going; `b` has none.
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| (j.id == "f.py:a").then(|| "ha".into()),
        |_| 0,
    );
    assert_eq!(skipped_ids(&plan), vec!["f.py:a"]);
    assert_eq!(fired_ids(&plan), vec!["f.py:b"]);
}

#[tokio::test]
async fn a_missed_tick_is_skipped_at_startup_while_its_node_is_running() {
    let (_dir, db_path) = schedule_db().await;
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
    catch_up(
        &app_state(),
        &[],
        &jobs,
        &Zone::Utc,
        &utc(2, 5, 0),
        &db_path,
    )
    .await
    .unwrap();

    let st = app_state();
    insert_run(
        &st,
        "api",
        RunStatus::Running,
        "get",
        Some("daily"),
        Some("f.py:daily"),
        1.0,
    );
    let back = utc(3, 9, 0);
    let book = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
        .await
        .unwrap();
    assert!(book.last_handle.is_empty());
    assert_eq!(st.runs.registry.len(), 1, "no second run of the node");
    assert_eq!(
        book.last_skip["f.py:daily"],
        (back.timestamp(), "in_flight", Some("api".to_string()))
    );
    assert_eq!(
        book.last_fired["f.py:daily"],
        back.timestamp(),
        "not caught up again"
    );
}

fn alive(run_id: &str, command: &str, job: &str) -> db::AliveRun {
    db::AliveRun {
        run_id: run_id.into(),
        command: command.into(),
        job: job.into(),
    }
}

#[test]
fn a_cli_run_counts_when_its_job_covers_the_node() {
    let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
    let task = job("f.py:t", NodeKind::Task, "0 5 * * *");
    let one = [alive("cli", "get", "f.py:daily")];
    assert_eq!(cli_run_in_flight_for(&one, &daily).as_deref(), Some("cli"));
    let several = [alive("cli", "get", "f.py:a,f.py:daily")];
    assert_eq!(
        cli_run_in_flight_for(&several, &daily).as_deref(),
        Some("cli")
    );
    // `barca get` with no target gets every asset and sensor, not tasks.
    let all = [alive("cli", "get", "all")];
    assert_eq!(cli_run_in_flight_for(&all, &daily).as_deref(), Some("cli"));
    assert_eq!(cli_run_in_flight_for(&all, &task), None);
    let run = [alive("cli", "run", "f.py:t")];
    assert_eq!(cli_run_in_flight_for(&run, &task).as_deref(), Some("cli"));
    // Another node, a prefix of the id, or a row with no job: no.
    for rows in [
        [alive("cli", "get", "f.py:other")],
        [alive("cli", "get", "f.py:dail")],
        [alive("cli", "get", "")],
    ] {
        assert_eq!(cli_run_in_flight_for(&rows, &daily), None);
    }
    // The oldest is named.
    let two = [
        alive("first", "get", "f.py:daily"),
        alive("second", "get", "all"),
    ];
    assert_eq!(
        cli_run_in_flight_for(&two, &daily).as_deref(),
        Some("first")
    );
}

#[tokio::test]
async fn plan_tick_skips_for_a_cli_run_on_the_same_project() {
    let st = app_state();
    let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let max = jobs[0].max_failures;
    let cli = [alive("cli", "get", "f.py:daily")];
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| any_run_in_flight_for(&st, &cli, j),
        |_| max - 1,
    );
    let [TickAction::Skip { job, handle }] = &plan[..] else {
        panic!("expected a skip");
    };
    assert_eq!(handle, "cli");
    // The skip is not a failed tick: nothing is counted toward `max_failures`.
    let mut book = Bookkeeping::default();
    book.failures.insert(
        job.id.clone(),
        db::ScheduleFailures {
            failures: max - 1,
            ..db::ScheduleFailures::default()
        },
    );
    record_skip(&mut book, job, Some(handle), 1, "scheduled run");
    assert!(!count_outcomes(&st, &jobs, &mut book, None).await);
    assert_eq!(book.failures(job), max - 1);
    // Pause, then failing, come before the in-flight skip.
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| any_run_in_flight_for(&st, &cli, j),
        |_| max,
    );
    assert!(matches!(plan[..], [TickAction::Failing { .. }]));
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| true,
        |j| any_run_in_flight_for(&st, &cli, j),
        |_| max,
    );
    assert!(matches!(plan[..], [TickAction::Paused(_)]));
    // A run the server holds is named first.
    insert_run(
        &st,
        "api",
        RunStatus::Running,
        "get",
        Some("daily"),
        Some("f.py:daily"),
        1.0,
    );
    let plan = plan_tick(
        &at(5, 0),
        &jobs,
        |_| false,
        |j| any_run_in_flight_for(&st, &cli, j),
        |_| 0,
    );
    match &plan[..] {
        [TickAction::Skip { handle, .. }] => assert_eq!(handle, "api"),
        _ => panic!("expected a skip"),
    }
}

#[tokio::test]
async fn a_missed_tick_is_skipped_at_startup_while_a_cli_run_of_its_node_is_going() {
    let (_dir, db_path) = schedule_db().await;
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
    catch_up(
        &app_state(),
        &[],
        &jobs,
        &Zone::Utc,
        &utc(2, 5, 0),
        &db_path,
    )
    .await
    .unwrap();
    // A `barca get daily` from a shell, still going.
    let cli = [alive("cli", "get", "f.py:daily")];
    let st = app_state();
    let back = utc(3, 9, 0);
    let book = catch_up(&st, &cli, &jobs, &Zone::Utc, &back, &db_path)
        .await
        .unwrap();
    assert!(st.runs.registry.is_empty(), "no second run of the node");
    assert_eq!(
        book.last_skip["f.py:daily"],
        (back.timestamp(), "in_flight", Some("cli".to_string()))
    );
}

// ─── catch-up across a restart (fixed times, a real schedule_state table) ──

/// A metadata DB in a temp dir, and the UTC instant `hour:minute` on 2026-07-02.
async fn schedule_db() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.db").display().to_string();
    barca_service::db::init_db(&path).await.unwrap();
    (dir, path)
}

fn utc(day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    Utc.with_ymd_and_hms(2026, 7, day, hour, minute, 0)
        .single()
        .unwrap()
        .fixed_offset()
}

#[test]
fn plan_tick_skips_a_paused_job_before_anything_else() {
    let jobs = vec![
        job("f.py:a", NodeKind::Asset, "0 5 * * *"),
        job("f.py:b", NodeKind::Task, "0 5 * * *"),
    ];
    let plan = plan_tick(&at(5, 0), &jobs, |j| j.id == "f.py:a", |_| None, |_| 0);
    assert!(matches!(plan[0], TickAction::Paused(j) if j.id == "f.py:a"));
    assert_eq!(fired_ids(&plan), vec!["f.py:b"]);
}

/// A paused job's ticks are skipped as paused, also when it is failing: a paused tick is
/// not a failed one, and nothing about it is counted.
#[test]
fn plan_tick_reports_a_paused_failing_job_as_paused() {
    let jobs = vec![job("f.py:a", NodeKind::Asset, "0 5 * * *")];
    let plan = plan_tick(&at(5, 0), &jobs, |_| true, |_| Some("h".into()), |_| 9);
    assert!(matches!(plan.as_slice(), [TickAction::Paused(j)] if j.id == "f.py:a"));
}

#[tokio::test]
async fn a_paused_job_is_not_caught_up_and_its_skip_says_paused() {
    let (_dir, db_path) = schedule_db().await;
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
    catch_up(
        &app_state(),
        &[],
        &jobs,
        &Zone::Utc,
        &utc(2, 5, 0),
        &db_path,
    )
    .await
    .unwrap();
    // Paused until resumed, across the restart: the missed 06:00 tick is skipped.
    db::set_schedule_pause(
        &db_path,
        "f.py:daily",
        Some(db::SchedulePause {
            paused_at: utc(2, 5, 30).timestamp(),
            until: None,
        }),
        utc(2, 5, 30).timestamp(),
    )
    .await
    .unwrap();
    let st = app_state();
    let back = utc(3, 9, 0);
    let book = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
        .await
        .unwrap();
    assert!(st.runs.registry.is_empty(), "a paused job does not run");
    assert_eq!(
        book.last_skip["f.py:daily"],
        (back.timestamp(), "paused", None)
    );

    // A pause that ended counts from when it ended: the ticks inside it are not caught up.
    let (_dir, db_path) = schedule_db().await;
    catch_up(
        &app_state(),
        &[],
        &jobs,
        &Zone::Utc,
        &utc(2, 5, 0),
        &db_path,
    )
    .await
    .unwrap();
    db::set_schedule_pause(
        &db_path,
        "f.py:daily",
        Some(db::SchedulePause {
            paused_at: utc(2, 5, 30).timestamp(),
            until: Some(utc(4, 7, 0).timestamp()),
        }),
        utc(2, 5, 30).timestamp(),
    )
    .await
    .unwrap();
    let st = app_state();
    let book = catch_up(&st, &[], &jobs, &Zone::Utc, &utc(4, 8, 0), &db_path)
        .await
        .unwrap();
    assert!(
        st.runs.registry.is_empty(),
        "no tick between the end of the pause and now"
    );
    assert!(book.last_skip.is_empty());
    let st = app_state();
    let book = catch_up(&st, &[], &jobs, &Zone::Utc, &utc(5, 8, 0), &db_path)
        .await
        .unwrap();
    assert_eq!(
        book.last_handle.len(),
        1,
        "the 06:00 tick after the pause is caught up"
    );
}

#[tokio::test]
async fn unreadable_schedule_history_never_becomes_an_empty_first_start() {
    let dir = tempfile::tempdir().unwrap();
    let st = app_state();
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
    // A directory cannot be opened as metadata. A failed read must not anchor
    // or trigger jobs as if the durable schedule history were simply absent.
    let error = catch_up(
        &st,
        &[],
        &jobs,
        &Zone::Utc,
        &utc(2, 9, 0),
        &dir.path().display().to_string(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, barca_service::BarcaError::MetadataUnusable { path, .. }
        if path == dir.path().to_string_lossy())
    );
    assert!(st.runs.registry.is_empty());
    assert!(st.scheduling.jobs.read().unwrap().is_empty());
}

#[tokio::test]
async fn a_first_start_anchors_every_job_to_now_and_runs_nothing() {
    let (_dir, db_path) = schedule_db().await;
    let st = app_state();
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
    let now = utc(2, 9, 0);

    let Bookkeeping {
        last_handle: handles,
        last_fired: fired,
        ..
    } = catch_up(&st, &[], &jobs, &Zone::Utc, &now, &db_path)
        .await
        .unwrap();

    assert!(handles.is_empty());
    assert!(
        st.runs.registry.is_empty(),
        "nothing may run on a first start"
    );
    assert_eq!(
        fired,
        HashMap::from([("f.py:daily".to_string(), now.timestamp())])
    );
    assert_eq!(db::get_schedule_state(&db_path).await.unwrap(), fired);
}

#[tokio::test]
async fn catch_up_does_not_fire_early_across_the_autumn_offset_change() {
    let (_dir, db_path) = schedule_db().await;
    let zone = Zone::parse("America/New_York").unwrap();
    let jobs = [job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
    let before = Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap();
    db::upsert_schedule_state(&db_path, "f.py:daily", before.timestamp())
        .await
        .unwrap();
    // 09:30 UTC is 04:30 after the offset changed; the old fixed-offset
    // search incorrectly treated the next 05:00 as 09:00 UTC.
    let early = zone.at(Utc.with_ymd_and_hms(2026, 11, 1, 9, 30, 0).unwrap());
    let st = app_state();
    let Bookkeeping {
        last_handle: handles,
        last_fired: fired,
        ..
    } = catch_up(&st, &[], &jobs, &zone, &early, &db_path)
        .await
        .unwrap();
    assert!(handles.is_empty() && st.runs.registry.is_empty());
    assert_eq!(fired["f.py:daily"], before.timestamp());
    let due = zone.at(Utc.with_ymd_and_hms(2026, 11, 1, 10, 0, 0).unwrap());
    let Bookkeeping {
        last_handle: handles,
        ..
    } = catch_up(&st, &[], &jobs, &zone, &due, &db_path)
        .await
        .unwrap();
    assert_eq!(handles.len(), 1);
}

#[tokio::test]
async fn a_tick_missed_while_the_server_was_down_runs_once_when_it_comes_back() {
    let (_dir, db_path) = schedule_db().await;
    let jobs = [
        job("f.py:daily", NodeKind::Asset, "0 6 * * *"),
        job("f.py:hourly", NodeKind::Task, "0 * * * *"),
    ];
    // Running at 05:10: both jobs are anchored. The server then stops.
    let stopped = utc(2, 5, 10);
    catch_up(&app_state(), &[], &jobs, &Zone::Utc, &stopped, &db_path)
        .await
        .unwrap();

    // Back at 05:50: no tick of either job has passed.
    let st = app_state();
    let early = utc(2, 5, 50);
    let Bookkeeping {
        last_handle: handles,
        last_fired: fired,
        ..
    } = catch_up(&st, &[], &jobs, &Zone::Utc, &early, &db_path)
        .await
        .unwrap();
    assert!(handles.is_empty() && st.runs.registry.is_empty());
    assert_eq!(
        fired["f.py:daily"],
        stopped.timestamp(),
        "the record is not moved"
    );

    // Back three days later: `daily` missed three ticks and `hourly` dozens. Each runs
    // exactly once, and its record moves to the restart time.
    let st = app_state();
    let back = utc(5, 9, 30);
    let Bookkeeping {
        last_handle: handles,
        last_fired: fired,
        ..
    } = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
        .await
        .unwrap();
    assert_eq!(handles.len(), 2);
    assert_eq!(
        st.runs.registry.len(),
        2,
        "one run per job, however many ticks were missed"
    );
    for id in ["f.py:daily", "f.py:hourly"] {
        assert!(st.runs.registry.contains_key(&handles[id]), "{id}");
        assert_eq!(fired[id], back.timestamp(), "{id}");
    }
    assert_eq!(db::get_schedule_state(&db_path).await.unwrap(), fired);

    // A restart right after has nothing left to catch up: the catch-up does not repeat.
    let st = app_state();
    let again = utc(5, 9, 31);
    let Bookkeeping {
        last_handle: handles,
        last_fired: fired,
        ..
    } = catch_up(&st, &[], &jobs, &Zone::Utc, &again, &db_path)
        .await
        .unwrap();
    assert!(handles.is_empty() && st.runs.registry.is_empty());
    assert_eq!(fired["f.py:hourly"], back.timestamp());
}

#[tokio::test]
async fn a_job_added_while_the_server_was_down_is_anchored_not_run() {
    let (_dir, db_path) = schedule_db().await;
    let old = job("f.py:daily", NodeKind::Asset, "0 6 * * *");
    catch_up(
        &app_state(),
        &[],
        std::slice::from_ref(&old),
        &Zone::Utc,
        &utc(2, 5, 0),
        &db_path,
    )
    .await
    .unwrap();

    let st = app_state();
    let jobs = [old, job("f.py:new", NodeKind::Asset, "0 6 * * *")];
    let back = utc(4, 9, 0);
    let Bookkeeping {
        last_handle: handles,
        last_fired: fired,
        ..
    } = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
        .await
        .unwrap();

    assert_eq!(handles.keys().collect::<Vec<_>>(), ["f.py:daily"]);
    assert_eq!(st.runs.registry.len(), 1);
    assert_eq!(fired["f.py:new"], back.timestamp());
}

// ─── kind-based dispatch ───────────────────────────────────────────────

#[tokio::test]
async fn trigger_registers_a_tracked_run_for_asset_and_task() {
    let st = app_state();
    let asset = job("f.py:a", NodeKind::Asset, "* * * * *");
    let task = job("f.py:t", NodeKind::Task, "* * * * *");
    // Both kinds must produce a handle that is registered in the runs map,
    // proving each routes into a real run-trigger path (get vs run).
    let ha = trigger(&st, &asset, 0, db::generate_run_id())
        .await
        .unwrap();
    let ht = trigger(&st, &task, 0, db::generate_run_id()).await.unwrap();
    assert!(st.runs.registry.contains_key(&ha));
    assert!(st.runs.registry.contains_key(&ht));
    assert_ne!(ha, ht);
}

// ─── real-parser enumeration seam ──────────────────────────────────────

#[tokio::test]
async fn collect_jobs_discovers_scheduled_nodes_from_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipeline.py");
    std::fs::write(
        &path,
        concat!(
            "from barca import asset, sensor, Schedule, Always\n\n",
            "@asset(freshness=Schedule(\"0 6 * * *\"))\n",
            "def daily_report() -> dict:\n    return {}\n\n",
            "@sensor(freshness=Schedule(\"*/5 * * * *\"))\n",
            "def inbox() -> tuple:\n    return (True, [])\n\n",
            "@asset(freshness=Always)\n",
            "def raw() -> dict:\n    return {}\n",
        ),
    )
    .unwrap();

    let summaries = barca_service::queries::list_assets(
        &[path.display().to_string()],
        &barca_service::commands::find_python(),
    )
    .await
    .unwrap();
    let jobs = jobs_from_summaries(summaries);

    assert_eq!(
        jobs.len(),
        2,
        "only the two Schedule nodes, not the Always asset"
    );
    assert!(
        jobs.iter().any(|j| j.id.ends_with(":daily_report")
            && j.kind == NodeKind::Asset
            && j.cron_str == "0 6 * * *"),
        "scheduled asset discovered with round-tripped cron",
    );
    assert!(
        jobs.iter().any(|j| j.id.ends_with(":inbox")
            && j.kind == NodeKind::Sensor
            && j.cron_str == "*/5 * * * *"),
        "scheduled sensor discovered with round-tripped cron",
    );
}

#[tokio::test]
async fn describe_schedule_reports_next_fire() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipeline.py");
    std::fs::write(
        &path,
        concat!(
            "from barca import asset, Schedule\n\n",
            "@asset(freshness=Schedule(\"*/5 * * * *\"))\n",
            "def poll() -> dict:\n    return {}\n",
        ),
    )
    .unwrap();

    let infos = describe_schedule(
        &[path.display().to_string()],
        &barca_service::commands::find_python(),
    )
    .await;
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].cron, "*/5 * * * *");
    assert_eq!(infos[0].kind, NodeKind::Asset);
    // A `*/5` cron always has an upcoming occurrence.
    assert!(infos[0].next_fire.is_some());
    assert!(infos[0].next_fire_local.is_some());
}

// ─── the tick record (#960) ────────────────────────────────────────────

/// [`app_state`] over an initialized metadata database of its own, which the scheduler and the
/// runs it starts share, as in a server.
async fn recording_state() -> (tempfile::TempDir, AppState, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.db").display().to_string();
    db::init_db(&path).await.unwrap();
    (dir, state_at(&path), path)
}

/// A server on the metadata database at `path`: a second one is the first one restarted.
fn state_at(path: &str) -> AppState {
    let st = app_state();
    let mut config = (*st.config).clone();
    config.resolved.db_path = path.to_string();
    AppState::new(config)
}

async fn ticks(path: &str, job: &ScheduledJob) -> Vec<db::TickRecord> {
    db::ticks_for_schedule(path, &job.id, 100).await.unwrap()
}

/// The newest tick of `job` once its run recorded how it ended.
async fn finished_tick(path: &str, job: &ScheduledJob) -> db::TickRecord {
    for _ in 0..600 {
        if let Some(tick) = ticks(path, job).await.into_iter().next()
            && tick.finished_at.is_some()
        {
            return tick;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "the tick's run never finished: {:?}",
        ticks(path, job).await
    );
}

#[tokio::test]
async fn a_fired_paused_skipped_and_failing_tick_each_leave_one_row() {
    let (_dir, st, path) = recording_state().await;
    let j = job("f.py:daily", NodeKind::Asset, "0 6 * * *");
    record_tick(&path, &j, 60, db::TickDecision::Paused, None).await;
    record_tick(&path, &j, 120, db::TickDecision::Skipped, Some("run-x")).await;
    record_tick(&path, &j, 180, db::TickDecision::Failing, None).await;
    let handle = fire(&st, &j, 240).await.unwrap();

    let done = finished_tick(&path, &j).await;
    let rows = ticks(&path, &j).await;
    let seen: Vec<_> = rows
        .iter()
        .map(|t| (t.scheduled_at, t.action, t.skipped_for.as_deref()))
        .collect();
    use db::TickDecision::*;
    assert_eq!(
        seen,
        vec![
            (240, Fired, None),
            (180, Failing, None),
            (120, Skipped, Some("run-x")),
            (60, Paused, None),
        ]
    );
    // The fired tick names the run it started, before the run could start.
    assert_eq!(done.run_id.as_deref(), Some(handle.as_str()));
    assert!(rows[1..].iter().all(|t| t.run_id.is_none()));
    // This server's source file does not exist: the run started, failed before any step, and
    // so reached none of the tick's targets.
    assert!(done.started_at.is_some() && done.queue_wait_seconds.is_some());
    assert!(done.finished_at >= done.started_at);
    assert_eq!(done.run_status.as_deref(), Some("failed"));
    assert_eq!(
        done.targets,
        Some(vec![(j.id.clone(), db::TickTargetOutcome::NotReached)])
    );
    assert_eq!(done.counts.map(|c| c.not_reached), Some(1));
}

#[tokio::test]
async fn catch_up_records_what_it_decided_for_each_missed_tick() {
    let (_dir, st, path) = recording_state().await;
    let jobs = [
        job("f.py:fires", NodeKind::Asset, "0 6 * * *"),
        job("f.py:paused", NodeKind::Asset, "0 6 * * *"),
        job("f.py:busy", NodeKind::Asset, "0 6 * * *"),
        job("f.py:failing", NodeKind::Asset, "0 6 * * *"),
        job("f.py:not_due", NodeKind::Asset, "0 6 1 1 *"),
    ];
    catch_up(&st, &[], &jobs, &Zone::Utc, &utc(2, 5, 0), &path)
        .await
        .unwrap();
    for j in &jobs {
        assert!(ticks(&path, j).await.is_empty(), "anchoring is not a tick");
    }
    let pause = db::SchedulePause {
        paused_at: utc(2, 5, 30).timestamp(),
        until: None,
    };
    db::set_schedule_pause(&path, "f.py:paused", Some(pause), pause.paused_at)
        .await
        .unwrap();
    let failing = db::ScheduleFailures {
        failures: jobs[3].max_failures,
        failed_nodes: vec!["f.py:failing".into()],
        mark: i64::MAX / 2,
    };
    db::set_schedule_failures(&path, "f.py:failing", &failing)
        .await
        .unwrap();
    insert_run(
        &st,
        "run-busy",
        RunStatus::Running,
        "get",
        Some("busy"),
        Some("f.py:busy"),
        1.0,
    );

    let back = utc(2, 7, 0);
    let book = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &path)
        .await
        .unwrap();
    let one = |rows: Vec<db::TickRecord>| {
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].scheduled_at, back.timestamp());
        rows.into_iter().next().unwrap()
    };
    use db::TickDecision::*;
    let fired = one(ticks(&path, &jobs[0]).await);
    assert_eq!(fired.action, Fired);
    assert_eq!(fired.run_id.as_ref(), book.last_handle.get("f.py:fires"));
    assert_eq!(one(ticks(&path, &jobs[1]).await).action, Paused);
    let skipped = one(ticks(&path, &jobs[2]).await);
    assert_eq!(
        (skipped.action, skipped.skipped_for.as_deref()),
        (Skipped, Some("run-busy"))
    );
    assert_eq!(one(ticks(&path, &jobs[3]).await).action, Failing);
    assert!(ticks(&path, &jobs[4]).await.is_empty(), "no tick was due");
}

#[tokio::test]
async fn queue_wait_runs_from_joining_the_queue_to_the_runs_start() {
    let (_dir, st, path) = recording_state().await;
    let j = job("f.py:daily", NodeKind::Asset, "0 6 * * *");
    // A paused queue: the run joins it and waits.
    let _ = crate::queue_routes::pause(axum::extract::State(st.clone()))
        .await
        .unwrap();
    let handle = fire(&st, &j, 240).await.unwrap();
    let queued_at = st.runs.registry.get(&handle).unwrap().started_at;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let waiting = &ticks(&path, &j).await[0];
    assert_eq!(waiting.run_id.as_deref(), Some(handle.as_str()));
    assert_eq!(
        (waiting.started_at, waiting.queue_wait_seconds),
        (None, None),
        "nothing about a start until it starts"
    );

    let _ = crate::queue_routes::resume(axum::extract::State(st.clone()))
        .await
        .unwrap();
    let done = finished_tick(&path, &j).await;
    let (started, wait) = (done.started_at.unwrap(), done.queue_wait_seconds.unwrap());
    assert!((0.3..30.0).contains(&wait), "waited {wait}s");
    assert!(
        (started - queued_at - wait).abs() < 1e-6,
        "the wait is start - queued_at, not start - slot: {started} - {queued_at} != {wait}"
    );
}

#[tokio::test]
async fn a_restart_while_a_ticks_run_is_queued_or_running_leaves_a_consistent_row() {
    let (_dir, st, path) = recording_state().await;
    let queued = job("f.py:queued", NodeKind::Asset, "0 6 * * *");
    let killed = job("f.py:killed", NodeKind::Asset, "0 6 * * *");
    let _ = crate::queue_routes::pause(axum::extract::State(st.clone()))
        .await
        .unwrap();
    let q = fire(&st, &queued, 240).await.unwrap();
    let k = fire(&st, &killed, 240).await.unwrap();
    // `killed`'s run had started when the server was killed: its queue row says `running`.
    db::record_tick_started(&path, &killed.id, 240, &k, 241.0, 1.0)
        .await
        .unwrap();
    barca_service::serve::run_queue::mark_running(&path, &k)
        .await
        .unwrap();
    drop(st);

    // Between the servers: each row names its run and no end.
    for (j, run) in [(&queued, &q), (&killed, &k)] {
        let row = &ticks(&path, j).await[0];
        assert_eq!(row.action, db::TickDecision::Fired);
        assert_eq!(row.run_id.as_ref(), Some(run));
        assert_eq!(
            (row.finished_at, &row.run_status, &row.targets),
            (None, &None, &None)
        );
    }

    // The next server: the killed run is not started again, and its tick ends with it, no
    // target reached. The start it recorded stays.
    let st = state_at(&path);
    crate::runs::resume_at_startup(&st).await.unwrap();
    let row = &ticks(&path, &killed).await[0];
    assert_eq!(row.run_status.as_deref(), Some("interrupted"));
    assert_eq!(
        (row.started_at, row.queue_wait_seconds),
        (Some(241.0), Some(1.0))
    );
    assert!(row.finished_at.is_some());
    assert_eq!(
        row.targets,
        Some(vec![(killed.id.clone(), db::TickTargetOutcome::NotReached)])
    );
    // The queued run starts under its id when the queue is resumed, and ends on the same row.
    assert!(ticks(&path, &queued).await[0].started_at.is_none());
    let _ = crate::queue_routes::resume(axum::extract::State(st.clone()))
        .await
        .unwrap();
    let done = finished_tick(&path, &queued).await;
    assert_eq!(done.run_id.as_ref(), Some(&q));
    assert!(done.started_at.is_some() && done.run_status.is_some());
    assert_eq!(ticks(&path, &queued).await.len(), 1, "one row per tick");
}

#[tokio::test]
async fn a_run_nobody_scheduled_writes_no_tick() {
    let (_dir, st, path) = recording_state().await;
    let j = job("f.py:daily", NodeKind::Asset, "0 6 * * *");
    let spec = crate::runs::RunSpec {
        command: barca_service::outcome::RunCommand::Get,
        targets: vec![j.id.clone()],
        nodes: vec![j.id.clone()],
        files: Vec::new(),
        policy: barca_service::cache::CachePolicy::CacheAware,
        trigger: barca_service::outcome::Trigger::api(),
        values: false,
        partition: None,
        batch: Vec::new(),
        rules: barca_service::freshness::RunRules::default(),
        timeout: None,
        kill_requeues: 0,
        queue: Default::default(),
    };
    let handle = crate::runs::start_as(st.clone(), db::generate_run_id(), spec)
        .await
        .unwrap();
    for _ in 0..600 {
        if st
            .runs
            .registry
            .get(&handle)
            .is_some_and(|r| r.finished_at.is_some())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(st.runs.registry.get(&handle).unwrap().finished_at.is_some());
    assert!(ticks(&path, &j).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn supervisor_reports_exit_and_restart_then_stops_for_drain() {
    let st = app_state();
    let (started, mut attempts) = tokio::sync::mpsc::unbounded_channel();
    let supervisor = tokio::spawn(supervise(st.clone(), move |state| {
        let started = started.clone();
        async move {
            let attempt = state.scheduling.progress.snapshot().unwrap().restart_count;
            started.send(attempt).unwrap();
            if attempt == 0 {
                panic!("scheduler test failure");
            }
            state.lifecycle.draining.cancelled().await;
            Ok("draining")
        }
    }));
    assert_eq!(attempts.recv().await, Some(0));
    let health = || crate::state_routes::health(axum::extract::State(st.clone()));
    wait_for_backoff(&st).await;
    assert_eq!(
        health().await.status,
        barca_service::served::HealthStatus::Degraded
    );
    assert_eq!(attempts.recv().await, Some(1));
    let h = health().await;
    assert_eq!(h.status, barca_service::served::HealthStatus::Ok);
    let live = h.0.scheduler_liveness.unwrap();
    assert_eq!(live.restart_count, 1);
    assert!(
        live.last_exit_reason
            .unwrap()
            .contains("scheduler test failure")
    );
    st.lifecycle.draining.cancel();
    supervisor.await.unwrap();
    assert_eq!(attempts.recv().await, None);
    assert_eq!(
        health()
            .await
            .scheduler_liveness
            .as_ref()
            .unwrap()
            .restart_count,
        1
    );
}

#[tokio::test]
async fn restart_after_enqueue_before_cursor_write_preserves_root_and_pause() {
    let (_dir, st, path) = recording_state().await;
    st.scheduling.progress.start();
    let jobs = [
        job("f.py:daily", NodeKind::Asset, "0 6 * * *"),
        job("f.py:paused", NodeKind::Asset, "0 6 * * *"),
    ];
    let before = utc(2, 5, 0);
    let due = utc(2, 6, 0);
    catch_up(&st, &[], &jobs, &Zone::Utc, &before, &path)
        .await
        .unwrap();
    let pause = db::SchedulePause {
        paused_at: before.timestamp(),
        until: None,
    };
    db::set_schedule_pause(&path, &jobs[1].id, Some(pause), before.timestamp())
        .await
        .unwrap();
    let root = fire(&st, &jobs[0], due.timestamp()).await.unwrap();
    finished_tick(&path, &jobs[0]).await;
    // A fresh process has no in-memory claim. Its durable cursor was committed with the root.
    let st = state_at(&path);
    st.scheduling.progress.start();
    let book = catch_up(&st, &[], &jobs, &Zone::Utc, &utc(2, 7, 0), &path)
        .await
        .unwrap();
    assert!(book.last_handle.is_empty());
    assert_eq!(
        st.scheduling.progress.snapshot().unwrap().last_due_slot,
        Some(due.timestamp())
    );
    let records = ticks(&path, &jobs[0]).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id.as_deref(), Some(root.as_str()));
    assert_eq!(
        ticks(&path, &jobs[1]).await[0].action,
        db::TickDecision::Paused
    );
}

#[tokio::test(start_paused = true)]
async fn idle_heartbeat_waiting_for_metadata_has_a_three_period_grace() {
    use barca_service::served::HealthStatus;
    use std::time::Duration;
    let state = app_state();
    let held = db::db_guard().await;
    state.scheduling.progress.beat();
    let writer = state.clone();
    let blocked = tokio::spawn(async move {
        drop(
            writer
                .scheduling
                .progress
                .waiting("heartbeat", db::db_guard())
                .await,
        );
        writer.scheduling.progress.beat();
    });
    tokio::task::yield_now().await;
    let health = || crate::state_routes::health(axum::extract::State(state.clone()));
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;
    let router = crate::routes::router(state.clone());
    let response = router
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let etag = response.headers()[header::ETAG].clone();
    let first = health().await.0.scheduler_liveness.unwrap();
    assert_eq!(first.operation.as_deref(), Some("heartbeat"));
    assert!(first.operation_started_at_ms.is_some());
    tokio::time::advance(Duration::from_secs(10)).await;
    let idle = health().await;
    assert_eq!(idle.status, HealthStatus::Ok);
    assert_eq!(idle.0.scheduler_liveness.unwrap(), first);
    let response = router
        .oneshot(
            Request::get("/health")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    tokio::time::advance(Duration::from_secs(51)).await;
    assert_eq!(health().await.status, HealthStatus::Degraded);
    drop(held);
    blocked.await.unwrap();
    assert_eq!(health().await.status, HealthStatus::Ok);
}

async fn wait_for_backoff(state: &AppState) {
    for _ in 0..100 {
        if state
            .scheduling
            .progress
            .snapshot()
            .is_some_and(|s| s.reason.is_some())
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("scheduler did not enter restart backoff");
}

#[tokio::test(start_paused = true)]
async fn drain_during_backoff_starts_no_replacement() {
    let state = app_state();
    let task = tokio::spawn(supervise(state.clone(), |_| async { Err("exited".into()) }));
    wait_for_backoff(&state).await;
    state.lifecycle.draining.cancel();
    task.await.unwrap();
    let view = state.scheduling.progress.snapshot().unwrap();
    assert_eq!(view.restart_count, 0);
    assert_eq!(view.last_exit_reason.as_deref(), Some("exited"));
}

#[tokio::test(start_paused = true)]
async fn backoff_resets_after_a_long_lived_attempt() {
    use tokio::time::{Duration, Instant};
    let st = app_state();
    let (started, mut attempts) = tokio::sync::mpsc::unbounded_channel();
    let supervisor = tokio::spawn(supervise(st.clone(), move |state| {
        let started = started.clone();
        async move {
            let attempt = state.scheduling.progress.snapshot().unwrap().restart_count;
            started.send(Instant::now()).unwrap();
            if attempt == 2 {
                tokio::time::sleep(Duration::from_secs(600)).await;
            }
            if attempt == 3 {
                return Ok("idle");
            }
            Err("exited".into())
        }
    }));
    let first = attempts.recv().await.unwrap();
    let second = attempts.recv().await.unwrap();
    let third = attempts.recv().await.unwrap();
    let fourth = attempts.recv().await.unwrap();
    assert_eq!(second - first, Duration::from_secs(1));
    assert_eq!(third - second, Duration::from_secs(2));
    assert_eq!(fourth - third, Duration::from_secs(601));
    supervisor.await.unwrap();
}

#[tokio::test]
async fn idle_scheduler_keeps_health_etag_across_loop_iterations() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use std::time::Duration;
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pipeline.py");
    std::fs::write(&source, "from barca import task, Schedule\n@task(freshness=Schedule('0 5 1 1 *'))\ndef yearly(): pass\n").unwrap();
    let mut cfg = app_state().config.as_ref().clone();
    cfg.files = vec![source.display().to_string()];
    cfg.resolved.db_path = dir.path().join("metadata.db").display().to_string();
    let state = AppState::new(cfg);
    let task = spawn(state.clone());
    let router = crate::routes::router(state.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.scheduling.jobs.read().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scheduler did not publish the yearly job");
    let first = router
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let tag = first.headers()[header::ETAG].clone();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // Bookkeeping may be visible mid-iteration. Poll until the loop is idle again.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = router
                .clone()
                .oneshot(
                    Request::get("/health")
                        .header(header::IF_NONE_MATCH, tag.clone())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if response.status() == StatusCode::NOT_MODIFIED {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle loop changed the health ETag");
    state.lifecycle.draining.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn a_live_metadata_hold_preserves_slots_and_loop_heartbeat() {
    use std::time::Duration;
    let (dir, st, path) = recording_state().await;
    let source = dir.path().join("pipeline.py");
    std::fs::write(&source,"from barca import task, Schedule\n@task(freshness=Schedule('* * * * * *'))\ndef probe(): pass\n").unwrap();
    let mut config = (*st.config).clone();
    config.files = vec![source.display().to_string()];
    let st = AppState::new(config);
    let _ = crate::queue_routes::pause(axum::extract::State(st.clone()))
        .await
        .unwrap();
    let scheduler = spawn(st.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while st.scheduling.jobs.read().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // The actual file lock isolates this hold from other tests' databases.
    let held = std::fs::File::open(format!("{path}.lock")).unwrap();
    held.try_lock().unwrap();
    let first = Utc::now().timestamp() + 1;
    let initial_beat = st
        .scheduling
        .progress
        .snapshot()
        .unwrap()
        .heartbeat_at_ms
        .unwrap();
    let mut heartbeat_timeout = false;
    // Span the persisted heartbeat as well as multiple every-second cron slots.
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let live = st.scheduling.progress.snapshot().unwrap();
            heartbeat_timeout |=
                live.reason.as_deref() == Some("scheduler timed out waiting for heartbeat");
            if heartbeat_timeout && live.heartbeat_at_ms.unwrap() > initial_beat {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let last = Utc::now().timestamp() - 1;
    assert!(last - first >= 3);
    drop(held);
    tokio::time::timeout(Duration::from_secs(12), async {
        while st.runs.registry.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let runs: Vec<_> = st
        .runs
        .registry
        .iter()
        .map(|r| {
            barca_service::time::parse_rfc3339(r.trigger.scheduled_at.as_deref().unwrap()).unwrap()
        })
        .collect();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0] >= last,
        "missed slots coalesce at the latest eligible slot"
    );
    scheduler.abort();
}

#[tokio::test(start_paused = true)]
async fn failed_admission_retries_once_and_replay_respects_a_pause() {
    let (_dir, st, path) = recording_state().await;
    let jobs = [job("p.py:probe", NodeKind::Task, "*/2 * * * * *")];
    let mut book = Bookkeeping::default();
    let gate = st.runs.queue.admit.lock().await;
    let cursor = evaluate_through(&st, &jobs, &mut book, &Zone::Utc, 99, 100, Some(&path)).await;
    assert_eq!(cursor, 100);
    assert_eq!(book.retries[&jobs[0].id].0, 100);
    assert!(
        st.scheduling
            .progress
            .snapshot()
            .unwrap()
            .reason
            .unwrap()
            .contains("trigger")
    );
    assert!(db::get_schedule_state(&path).await.unwrap().is_empty());
    drop(gate);
    for wall in [100, 100, 101] {
        evaluate_through(&st, &jobs, &mut book, &Zone::Utc, 100, wall, Some(&path)).await;
    }
    assert_eq!(
        db::get_schedule_state(&path).await.unwrap()[&jobs[0].id],
        100
    );
    assert_eq!(book.last_fired[&jobs[0].id], 100);
    assert_eq!(st.runs.registry.len(), 1);
    let pauses = crate::schedule_view::pauses(&st).await.unwrap();
    pauses.write().unwrap().insert(
        jobs[0].id.clone(),
        db::SchedulePause {
            paused_at: 102,
            until: Some(106),
        },
    );
    evaluate_through(&st, &jobs, &mut book, &Zone::Utc, 101, 104, Some(&path)).await;
    assert_eq!(st.runs.registry.len(), 1);
    assert_eq!(
        ticks(&path, &jobs[0])
            .await
            .iter()
            .map(|t| (t.scheduled_at, t.action))
            .collect::<Vec<_>>(),
        vec![
            (104, db::TickDecision::Paused),
            (100, db::TickDecision::Fired)
        ]
    );
}

#[tokio::test]
async fn delayed_wakeup_between_cron_seconds_replays_the_open_closed_interval() {
    let (_dir, state, path) = recording_state().await;
    let jobs = [job("p.py:probe", NodeKind::Task, "*/2 * * * * *")];
    let mut book = Bookkeeping::default();
    // Neither wakeup lands on a matching cron second. Only the latest elapsed slot fires.
    let cursor =
        evaluate_through(&state, &jobs, &mut book, &Zone::Utc, 101, 105, Some(&path)).await;
    assert_eq!(cursor, 105);
    assert_eq!(
        evaluate_through(
            &state,
            &jobs,
            &mut book,
            &Zone::Utc,
            cursor,
            105,
            Some(&path)
        )
        .await,
        105
    );
    assert_eq!(
        ticks(&path, &jobs[0])
            .await
            .into_iter()
            .map(|t| t.scheduled_at)
            .collect::<Vec<_>>(),
        vec![104]
    );
    assert_eq!(state.runs.registry.len(), 1);
    assert_eq!(
        state.scheduling.progress.snapshot().unwrap().last_due_slot,
        Some(104)
    );
    assert_eq!(
        db::get_schedule_state(&path).await.unwrap()[&jobs[0].id],
        104
    );
}

#[tokio::test]
async fn long_replay_coalesces_and_checks_already_admitted_coverage() {
    let (_dir, state, path) = recording_state().await;
    state.runs.queue.set_paused(Some(1));
    let mut first = job("p.py:first", NodeKind::Asset, "0 * * * * *");
    first.followers.push("p.py:second".into());
    let jobs = [first, job("p.py:second", NodeKind::Asset, "1 * * * * *")];
    let mut book = Bookkeeping::default();
    evaluate_through(&state, &jobs, &mut book, &Zone::Utc, 0, 28802, Some(&path)).await;
    assert_eq!(state.runs.registry.len(), 1);
    assert_eq!(book.last_fired[&jobs[0].id], 28800);
    assert_eq!(book.last_skip[&jobs[1].id].0, 28801);
    assert_eq!(book.last_skip[&jobs[1].id].1, "in_flight");
}

#[tokio::test]
async fn cancelled_and_evicted_handles_do_not_hide_the_next_failure() {
    let st = app_state();
    let jobs = [job("p.py:probe", NodeKind::Task, "* * * * *")];
    let mut book = Bookkeeping::default();
    finished(&st, "cancelled", RunStatus::Cancelled, &[]);
    finished(&st, "failed", RunStatus::Failed, &["p.py:probe"]);
    for handle in ["cancelled", "evicted", "failed"] {
        book.accepted(&jobs[0], handle.into());
    }
    assert!(count_outcomes(&st, &jobs, &mut book, None).await);
    assert_eq!(book.failures(&jobs[0]), 1);
    assert!(!count_outcomes(&st, &jobs, &mut book, None).await);
}

#[tokio::test]
async fn lasting_admission_error_only_retries_the_affected_job() {
    let (_dir, st, path) = recording_state().await;
    st.runs.queue.set_paused(Some(1));
    let jobs = [
        job("p.py:bad", NodeKind::Task, "*/2 * * * * *"),
        job("p.py:good", NodeKind::Task, "41 * * * * *"),
    ];
    {
        let _guard = db::db_guard().await;
        let (_db, conn) = db::open_conn(&path).await.unwrap();
        conn.execute("CREATE TRIGGER reject_one BEFORE INSERT ON run_queue WHEN instr(NEW.spec,'p.py:bad') > 0 BEGIN SELECT RAISE(ABORT,'rejected job'); END",()).await.unwrap();
    }
    let mut book = Bookkeeping::default();
    let mut cursor = 99;
    for wall in 100..=102 {
        cursor =
            evaluate_through(&st, &jobs, &mut book, &Zone::Utc, cursor, wall, Some(&path)).await;
        let health = crate::state_routes::health(axum::extract::State(st.clone())).await;
        if wall == 102 {
            assert_eq!(health.status, barca_service::served::HealthStatus::Ok);
            continue;
        }
        assert_eq!(health.status, barca_service::served::HealthStatus::Degraded);
        assert!(
            health
                .scheduler_liveness
                .as_ref()
                .unwrap()
                .reason
                .as_ref()
                .unwrap()
                .contains("rejected job")
        );
    }
    assert_eq!(book.retries.get(&jobs[0].id), Some(&(102, 0)));
    assert_eq!(book.last_fired[&jobs[0].id], 100);
    assert_eq!(book.last_fired[&jobs[1].id], 101);
    {
        let _guard = db::db_guard().await;
        let (_db, conn) = db::open_conn(&path).await.unwrap();
        conn.execute("DROP TRIGGER reject_one", ()).await.unwrap();
    }
    evaluate_through(&st, &jobs, &mut book, &Zone::Utc, cursor, 109, Some(&path)).await;
    assert_eq!(book.last_fired[&jobs[0].id], 108);
    assert_eq!(st.runs.registry.len(), 2);
    assert!(st.scheduling.progress.snapshot().unwrap().reason.is_none());
}

#[tokio::test]
async fn backwards_clock_clamps_live_and_restart_cursors() {
    let (_dir, st, path) = recording_state().await;
    let jobs = [job("p.py:probe", NodeKind::Task, "*/2 * * * * *")];
    db::upsert_schedule_state(&path, &jobs[0].id, 300)
        .await
        .unwrap();
    let mut book = catch_up(
        &st,
        &[],
        &jobs,
        &Zone::Utc,
        &Zone::Utc.timestamp(200),
        &path,
    )
    .await
    .unwrap();
    assert_eq!(
        db::get_schedule_state(&path).await.unwrap()[&jobs[0].id],
        200
    );
    let last = evaluate_through(&st, &jobs, &mut book, &Zone::Utc, 200, 100, Some(&path)).await;
    assert_eq!(last, 100);
    assert_eq!(
        db::get_schedule_state(&path).await.unwrap()[&jobs[0].id],
        100
    );
    evaluate_through(&st, &jobs, &mut book, &Zone::Utc, last, 102, Some(&path)).await;
    assert_eq!(book.last_fired[&jobs[0].id], 102);
    assert_eq!(st.runs.registry.len(), 1);
}

#[tokio::test]
async fn edited_cron_discards_its_old_slot_and_admission_error() {
    let (_dir, state, path) = recording_state().await;
    let old = [job("p.py:probe", NodeKind::Task, "40 * * * * *")];
    let fresh = [job("p.py:probe", NodeKind::Task, "41 * * * * *")];
    let id = &old[0].id;
    let mut book = Bookkeeping::default();
    state.scheduling.progress.beat();
    book.retries.insert(id.clone(), (100, 1));
    state
        .scheduling
        .progress
        .admission_failure(id, "old admission".into());
    discard_changed_retries(&state, &mut book, &old, &old);
    assert_eq!(book.retries.get(id), Some(&(100, 1)));
    discard_changed_retries(&state, &mut book, &old, &fresh);
    assert!(!book.retries.contains_key(id));
    assert!(
        state
            .scheduling
            .progress
            .snapshot()
            .unwrap()
            .reason
            .is_none()
    );

    // Also exercise a stale retry reaching the planner after a schedule edit.
    book.retries.insert(id.clone(), (100, 1));
    state
        .scheduling
        .progress
        .admission_failure(id, "old admission".into());
    evaluate_through(&state, &fresh, &mut book, &Zone::Utc, 100, 100, Some(&path)).await;
    assert!(!book.retries.contains_key(id));
    assert!(
        state
            .scheduling
            .progress
            .snapshot()
            .unwrap()
            .reason
            .is_none()
    );
    evaluate_through(&state, &fresh, &mut book, &Zone::Utc, 100, 101, Some(&path)).await;
    assert_eq!(book.last_fired[id], 101);
    assert_eq!(state.runs.registry.len(), 1);
}

#[tokio::test]
async fn json_compression_preserves_conditional_gets() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;
    let router = crate::routes::router(app_state());
    let response = router
        .clone()
        .oneshot(
            Request::get("/health")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    assert!(
        response.headers()[header::VARY]
            .to_str()
            .unwrap()
            .contains("accept-encoding")
    );
    let tag = response.headers()[header::ETAG].clone();
    assert!(tag.to_str().unwrap().starts_with("W/"));
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
    let response = router
        .oneshot(
            Request::get("/health")
                .header(header::IF_NONE_MATCH, tag)
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn explicit_task_skips_preserve_failure_streak_and_next_tick_runs() {
    let st = app_state();
    let jobs = vec![job("f.py:copy", NodeKind::Task, "* * * * *")];
    let mut book = Bookkeeping::default();
    finished(&st, "failed-before", RunStatus::Failed, &["f.py:copy"]);
    book.last_handle
        .insert(jobs[0].id.clone(), "failed-before".into());
    assert!(count_outcomes(&st, &jobs, &mut book, None).await);
    assert_eq!(book.failures(&jobs[0]), 1);
    for n in 0..5 {
        let handle = format!("skip-{n}");
        finished(&st, &handle, RunStatus::Skipped, &[]);
        book.last_handle.insert(jobs[0].id.clone(), handle);
        assert!(!count_outcomes(&st, &jobs, &mut book, None).await);
        assert_eq!(book.failures(&jobs[0]), 1);
    }
    finished(&st, "mixed", RunStatus::Success, &[]);
    st.runs.registry.get_mut("mixed").unwrap().outcome = Some(
        barca_service::outcome::Outcome::from_text(
            r#"{"status":"success","steps":[{"id":"f.py:copy[key=a]","status":"skipped","skip_reason":"locked"},{"id":"f.py:other","status":"ran"}]}"#
        ).unwrap()
    );
    assert_eq!(
        tick_outcome(&st.runs.registry.get("mixed").unwrap(), &jobs[0]),
        None
    );
    st.runs.registry.get_mut("mixed").unwrap().status = RunStatus::Failed;
    assert!(
        tick_outcome(&st.runs.registry.get("mixed").unwrap(), &jobs[0])
            .is_some_and(|failed| !failed.is_empty())
    );
    let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| None, |j| book.failures(j));
    assert_eq!(fired_ids(&plan), vec!["f.py:copy"]);
    finished(&st, "next-success", RunStatus::Success, &[]);
    book.last_handle
        .insert(jobs[0].id.clone(), "next-success".into());
    assert!(count_outcomes(&st, &jobs, &mut book, None).await);
    assert_eq!(book.failures(&jobs[0]), 0);
}
