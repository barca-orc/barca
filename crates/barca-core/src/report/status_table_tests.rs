//! The status table over rows a test writes by hand. `status`'s own tests are in
//! `barca-analysis`, below the renderer; this one needs both.

use crate::status::{
    CacheStatus, LastMaterialization, NodeStatus, ScheduleStatus, StatusFilter, StatusResult,
    StatusSummary,
};

/// As in `barca_analysis::status`'s tests.
fn node(state: &str, run_hash: Option<&str>) -> NodeStatus {
    NodeStatus {
        id: "p.py:a".into(),
        name: "a".into(),
        kind: "asset".into(),
        inputs: vec![],
        partitioned: false,
        cache: CacheStatus {
            state: state.parse().unwrap(),
            reason: "no_record".into(),
            detail: String::new(),
            run_hash: run_hash.map(String::from),
            artifact: None,
            changed: None,
            change_causes: None,
        },
        health: Default::default(),
        health_reason: Default::default(),
        signals: Vec::new(),
        measured: None,
        cause: None,
        root: None,
        gap_seconds: 0,
        last_check: None,
        refresh_path: None,
        stale_since: None,
        expected_fresh_at: None,
        recheck_at: None,
        no_estimate: None,
        last_attempt: Default::default(),
        typical_run_seconds: None,
        partitions: None,
        last_materialization: None,
        last_bytes_change: None,
        shape: None,
        env: vec![],
        undeclared_env: vec![],
        local: Some(false),
        schedule: None,
    }
}

/// As in `barca_analysis::status`'s tests.
fn failed(mut n: NodeStatus, error: &str) -> NodeStatus {
    n.last_materialization = Some(LastMaterialization {
        content_hash: None,
        status: "failed".into(),
        created_at: "2026-10-22 06:00:00".into(),
        elapsed_seconds: None,
        run_hash: None,
        artifact: None,
        format: None,
        size_bytes: None,
        rows: None,
        partition: None,
        error: Some(error.into()),
    });
    n
}

#[test]
fn the_table_shows_the_error_the_schedule_and_the_filter() {
    let mut task = failed(
        node("always_runs", None),
        "RuntimeError: 503\n  File \"p.py\"",
    );
    task.name = "sync".into();
    task.kind = "task".into();
    task.cache.reason = "task".into();
    task.schedule = Some(ScheduleStatus {
        cron: "* * * * *".into(),
        failures: 2,
        max_failures: 2,
        failing: true,
        ..Default::default()
    });
    let mut asset = failed(
        node("never_run", None),
        "ZeroDivisionError: division by zero",
    );
    asset.cache.reason = "failed".into();
    let nodes = vec![task, asset, node("cached", None)];
    let mut result = StatusResult {
        disk_usage: Default::default(),
        target: None,
        targets: vec![],
        summary: StatusSummary::of(&nodes),
        data_coverage: Default::default(),
        as_of: None,
        rollup: None,
        nodes,
        filter: None,
    };
    StatusFilter {
        problems: true,
        ..Default::default()
    }
    .apply(&mut result);
    let table = super::render_status_table(&result);
    assert!(table.contains("SCHEDULE"), "{table}");
    assert!(
        table.contains("last run failed: RuntimeError: 503"),
        "{table}"
    );
    assert!(table.contains("* * * * *; held, failing 2/2"), "{table}");
    assert!(
        table.contains("failed: ZeroDivisionError: division by zero"),
        "{table}"
    );
    assert!(
        table.contains("2 of 3 nodes match --problems; the counts below are for every node"),
        "{table}"
    );
    assert!(table.contains("1 cached, 0 stale, 1 never run"), "{table}");

    result.nodes.clear();
    let table = super::render_status_table(&result);
    assert!(table.contains("No node matches --problems."), "{table}");
}
