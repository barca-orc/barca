//! The structs of [`crate::served`] against the `json!` bodies the routes answered with before
//! they were typed (#1056, B3): the same bytes. `serde_json` writes a `json!` object's keys in
//! sorted order, so each struct declares its fields in that order; a field added out of order,
//! renamed or retyped fails here.

use super::*;
use serde_json::json;

fn bytes<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn health_is_the_body_the_route_built_by_hand() {
    let health = ServerHealth {
        api_version: 1,
        cache: Some(CacheSize {
            limit_bytes: 20,
            size_bytes: 10,
        }),
        capabilities: Capabilities { event_resume: 1 },
        env: "dev".into(),
        graph_stale: Some(false),
        load_errors: Vec::new(),
        metadata_error: None,
        queue: HealthQueue {
            capacity: Some(HealthCapacity {
                in_use: 1,
                total: 4,
                waiting: 2,
            }),
            max_runs: Some(2),
            paused: false,
            queued: 3,
            running: 1,
        },
        read_only: false,
        scheduler: true,
        scheduler_enabled: true,
        started_at: "2026-11-19T10:00:00.000Z".into(),
        status: HealthStatus::Ok,
        store_uri: None,
        version: "0.45.0".into(),
    };
    let by_hand = json!({
        "status": "ok",
        "started_at": "2026-11-19T10:00:00.000Z",
        "queue": {
            "paused": false,
            "queued": 3,
            "running": 1,
            "max_runs": 2,
            "capacity": {"total": 4, "in_use": 1, "waiting": 2},
        },
        "version": "0.45.0",
        "api_version": 1,
        "capabilities": { "event_resume": 1 },
        "env": "dev",
        "store_uri": null,
        "read_only": false,
        "graph_stale": false,
        "load_errors": [],
        "scheduler": true,
        "scheduler_enabled": true,
        "metadata_error": null,
        "cache": {"size_bytes": 10, "limit_bytes": 20},
    });
    assert_eq!(bytes(&health), by_hand.to_string());
    let draining = ServerHealth {
        status: HealthStatus::Draining,
        cache: None,
        ..health
    };
    assert_eq!(json!(draining)["status"], "draining");
    assert_eq!(json!(draining)["cache"], json!(null));
}

#[test]
fn a_schedule_entry_is_the_body_the_route_built_by_hand() {
    let job = ScheduleJob {
        cron: "0 5 * * *".into(),
        failing: false,
        failures: 1,
        id: "p.py:daily".into(),
        kind: crate::NodeKind::Asset,
        last_fired: Some(1_700_000_000),
        last_run: Some("run-1".into()),
        last_run_id: Some("run-1".into()),
        last_skipped: Some(1_700_000_100),
        last_status: Some("success".into()),
        late: true,
        late_since: Some(1_700_000_200),
        max_failures: 5,
        next_fire: Some(1_700_086_400),
        paused: true,
        paused_until: None,
        queue_bound: false,
        skip_reason: Some(SkipReason::InFlight),
        skipped_for: Some("run-0".into()),
        state: crate::schedule_standing::ScheduleState::Paused,
        state_detail: None,
        typical_queue_wait_seconds: 3,
        typical_run_seconds: Some(40),
    };
    let by_hand = json!({
        "state": "paused",
        "state_detail": null,
        "id": "p.py:daily",
        "cron": "0 5 * * *",
        "kind": "asset",
        "next_fire": 1_700_086_400,
        "last_fired": 1_700_000_000,
        "last_run_id": "run-1",
        "last_run": "run-1",
        "last_status": "success",
        "late": true,
        "late_since": 1_700_000_200,
        "typical_queue_wait_seconds": 3,
        "typical_run_seconds": 40,
        "queue_bound": false,
        "last_skipped": 1_700_000_100,
        "skipped_for": "run-0",
        "skip_reason": "in_flight",
        "paused": true,
        "paused_until": null,
        "failures": 1,
        "max_failures": 5,
        "failing": false,
    });
    assert_eq!(bytes(&job), by_hand.to_string());
    // What a client reads back is what was served.
    assert_eq!(
        serde_json::from_value::<ScheduleJob>(by_hand.clone()).unwrap(),
        job
    );
    assert_eq!(
        serde_json::from_value::<SchedulePauseAnswer>(by_hand).unwrap(),
        SchedulePauseAnswer::Job(Box::new(job))
    );
    let pause = SchedulePauseAnswer::Pause {
        id: "p.py:daily".into(),
        paused: true,
        paused_until: Some(5),
    };
    let by_hand = json!({"id": "p.py:daily", "paused": true, "paused_until": 5});
    assert_eq!(bytes(&pause), by_hand.to_string());
    assert_eq!(
        serde_json::from_value::<SchedulePauseAnswer>(by_hand).unwrap(),
        pause
    );
    assert_eq!(SkipReason::parse("paused"), Some(SkipReason::Paused));
    assert_eq!(json!(SkipReason::Paused), "paused");
}

#[test]
fn pauses_and_pins_are_the_bodies_the_routes_built_by_hand() {
    let paused = vec![crate::db::AssetPause {
        id: "p.py:orders".into(),
        paused_at: 10,
        via: "p.py".into(),
    }];
    let schedules = vec![crate::pauses::PausedSchedule {
        id: "p.py:daily".into(),
        paused_at: 11,
        until: None,
    }];
    let listed = Pauses::of(paused.clone(), schedules.clone());
    let mut by_hand = json!({
        "total": paused.len(),
        "paused": paused,
        "schedules": schedules,
    });
    assert_eq!(bytes(&listed), by_hand.to_string());
    by_hand["added"] = json!(["p.py:orders"]);
    let added = Pauses {
        added: Some(vec!["p.py:orders".into()]),
        ..listed.clone()
    };
    assert_eq!(bytes(&added), by_hand.to_string());
    assert_eq!(serde_json::from_value::<Pauses>(by_hand).unwrap(), added);
    let removed = Pauses {
        removed: Some(Vec::new()),
        ..listed
    };
    assert_eq!(json!(removed)["removed"], json!([]));

    let pins = Pins { pins: Vec::new() };
    assert_eq!(bytes(&pins), json!({ "pins": [] }).to_string());
    // The request a client sends names only what it sets, and an unknown key is refused.
    let request = PauseRequest {
        target: Some("p.py".into()),
        downstream: true,
        all: false,
    };
    assert_eq!(
        json!(request),
        json!({"target": "p.py", "downstream": true})
    );
    assert!(serde_json::from_str::<PauseRequest>(r#"{"targets": []}"#).is_err());
}

#[test]
fn the_run_answers_are_the_bodies_the_routes_built_by_hand() {
    let handle = RunHandle {
        position: Some(2),
        run_id: "run-1".into(),
        status: RunViewStatus::Pending,
        waiting_for: Some(crate::run_explain::WaitingFor::Restart),
        queue: None,
    };
    let by_hand = json!({
        "run_id": "run-1",
        "status": LiveStatus::Pending.as_str(),
        "position": 2,
        "waiting_for": "restart",
        "queue": null,
    });
    assert_eq!(bytes(&handle), by_hand.to_string());
    let started = RunHandle {
        position: None,
        status: RunViewStatus::Running,
        waiting_for: None,
        ..handle
    };
    assert_eq!(json!(started)["status"], LiveStatus::Running.as_str());
    assert_eq!(
        bytes(&RunCancelling::of("run-1")),
        json!({ "run_id": "run-1", "status": "cancelling" }).to_string()
    );
    let logs = RunLogs {
        logs: Vec::new(),
        run_id: "run-1".into(),
        status: "running".into(),
    };
    assert_eq!(
        bytes(&logs),
        json!({ "run_id": "run-1", "status": "running", "logs": [] }).to_string()
    );
    // The words a run's status takes are the ones served today, whatever is decided of them
    // (#943).
    let words: Vec<String> = [
        RunViewStatus::Pending,
        RunViewStatus::Running,
        RunViewStatus::Success,
        RunViewStatus::Failed,
        RunViewStatus::Cancelled,
        RunViewStatus::Error,
        RunViewStatus::Interrupted,
    ]
    .iter()
    .map(|w| json!(w).as_str().unwrap().to_string())
    .collect();
    assert_eq!(
        words,
        [
            "pending",
            "running",
            "success",
            "failed",
            "cancelled",
            "error",
            "interrupted"
        ]
    );
}

#[test]
fn project_and_auth_status_are_the_bodies_the_routes_built_by_hand() {
    let project = Project {
        api_version: 1,
        commit: None,
        deployed_at: "2026-11-19T10:00:00Z".into(),
        env: "dev".into(),
        files: vec![ProjectFile {
            path: "p.py".into(),
            sha256: "ab".into(),
        }],
        root: Some("/srv".into()),
        telemetry: ProjectTelemetry { datadog_url: None },
    };
    let by_hand = json!({
        "api_version": 1,
        "root": "/srv",
        "env": "dev",
        "deployed_at": "2026-11-19T10:00:00Z",
        "commit": null,
        "files": [{"path": "p.py", "sha256": "ab"}],
        "telemetry": { "datadog_url": null },
    });
    assert_eq!(bytes(&project), by_hand.to_string());
    assert_eq!(
        bytes(&AuthStatus {
            mode: AuthMode::Token
        }),
        json!({"mode": "token"}).to_string()
    );
    assert_eq!(json!(AuthMode::Off), "off");
}

#[test]
fn the_recorded_queue_orders_requested_runs_first_and_numbers_them() {
    use crate::run_queue::QueueState;
    use crate::trigger::{Trigger, TriggeredBy};
    let run = |id: &str, by, schedule: Option<&str>, state| RecordedRun {
        run_id: id.into(),
        state,
        accepted_at: 100.0,
        command: "get".into(),
        targets: vec!["p.py:a".into(), "p.py:b".into()],
        trigger: Trigger {
            triggered_by: by,
            schedule_id: schedule.map(str::to_string),
            scheduled_at: schedule.map(|_| "2026-11-19T10:00:00Z".to_string()),
            ..Default::default()
        },
        queue: Default::default(),
    };
    let timing = crate::liveness::RecordedTiming {
        started: [("r1".to_string(), 130)].into(),
        starts: [("r3".to_string(), 198)].into(),
        max_runs: Some(1),
        ..Default::default()
    };
    let view = recorded_queue(
        vec![
            run("r1", TriggeredBy::Api, None, QueueState::Running),
            run(
                "r2",
                TriggeredBy::Schedule,
                Some("p.py:a"),
                QueueState::Queued,
            ),
            run("r3", TriggeredBy::Api, None, QueueState::Queued),
        ],
        Some(50),
        &timing,
        false,
        150,
    );
    assert!(view.paused);
    assert_eq!(view.slots, "1 of 1 running");
    assert_eq!(view.running.len(), 1);
    assert_eq!(view.running[0].status, "running");
    assert_eq!(view.running[0].wait_seconds, Some(30));
    assert_eq!(view.running[0].position, None);
    let waiting: Vec<_> = view
        .queued
        .iter()
        .map(|e| (e.run_id.as_str(), e.position, e.status.as_str()))
        .collect();
    assert_eq!(
        waiting,
        [("r3", Some(1), "pending"), ("r2", Some(2), "pending")]
    );
    assert_eq!(view.queued[0].target.as_deref(), Some("p.py:a,p.py:b"));
    // The estimate is rounded up to 5 seconds: 198 reads 200.
    assert_eq!(
        view.queued[0].estimated_start_at.as_deref(),
        Some("1970-01-01T00:03:20Z")
    );
    assert_eq!(view.queued[0].wait_seconds, Some(100));
    // The two columns of `barca.queue` that `GET /queue` does not serve stay off the wire.
    let served = json!(view);
    assert!(served["queued"][1].get("state").is_none());
    assert!(served["queued"][1].get("scheduled_at").is_none());
    assert_eq!(view.queued[1].state, Some(QueueState::Queued));
    assert!(view.queued[1].scheduled_at.is_some());
    // And a client reads the answer back.
    let read: QueueView = serde_json::from_value(served).unwrap();
    assert_eq!(read.queued[0].run_id, "r3");
    assert_eq!(read.queued[0].state, None);
}
