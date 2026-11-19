//! barca's own records as tables in the `barca` schema of `barca sql`: `barca.runs` (the runs
//! of `barca history --json`), `barca.materializations` (one row per step result),
//! `barca.nodes` (`barca status --json`, one row per node, flattened), `barca.schedules` (one
//! row per scheduled node: the `schedule` block of its status), `barca.ticks` (one row per
//! recorded tick, `schedule_ticks`) and `barca.queue` (one row per run in a server's run
//! queue, `GET /queue`). Every row is built here from the values the JSON surfaces print;
//! nothing is derived a second time.
//!
//! A table is loaded only when the query names it (its name as a word, as for remote views);
//! the others are typed placeholders, so `show tables` and `describe` see them, and reading one
//! is an error that says so. On this machine every row is loaded; from a server at most
//! [`SERVER_ROWS`] of each (newest first), with a note when there are more.

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::dag::Dag;
use crate::db::{RunRecord, StepRow, TickRecord};
use crate::schedule::Zone;
use crate::status::NodeStatus;
use crate::time::rfc3339_secs;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

/// The schema barca's tables live in, apart from the views.
pub const SCHEMA: &str = "barca";

/// Most rows of one table `barca sql` reads from a server.
pub const SERVER_ROWS: usize = 10_000;

pub const RUNS: &str = "runs";
pub const MATERIALIZATIONS: &str = "materializations";
pub const NODES: &str = "nodes";
pub const SCHEDULES: &str = "schedules";
pub const TICKS: &str = "ticks";
pub const QUEUE: &str = "queue";

/// Every table of the `barca` schema, by name.
pub const NAMES: &[&str] = &[RUNS, MATERIALIZATIONS, NODES, SCHEDULES, TICKS, QUEUE];

/// One of barca's tables, as the SQL helper loads it.
#[derive(Debug, Clone, Serialize)]
pub struct Table {
    pub schema: &'static str,
    pub name: &'static str,
    /// (column, DuckDB type), in order.
    pub columns: &'static [(&'static str, &'static str)],
    /// `None`: not named by the query, a placeholder.
    pub rows: Option<Vec<Value>>,
    /// A note for stderr when the query names it (rows left out).
    #[serde(skip)]
    pub note: Option<String>,
}

const V: &str = "VARCHAR";
const I: &str = "BIGINT";
const F: &str = "DOUBLE";
const B: &str = "BOOLEAN";
const L: &str = "VARCHAR[]";

/// `barca history --json`'s run keys.
pub const RUN_COLUMNS: &[(&str, &str)] = &[
    ("run_id", V),
    ("command", V),
    ("files", L),
    ("target", V),
    ("job", V),
    ("status", V),
    ("steps_total", I),
    ("steps_executed", I),
    ("steps_cached", I),
    ("attempts", I),
    ("started_at", V),
    ("finished_at", V),
    ("elapsed_seconds", F),
    ("triggered_by", V),
    ("scheduled_at", V),
    ("schedule_id", V),
    ("client_run_id", V),
    ("failure", "STRUCT(node VARCHAR, error VARCHAR)"),
    ("failed_node", V),
    ("error", V),
    ("principal", V),
    ("trigger_reason", V),
    (
        "cancellation",
        "STRUCT(\"by\" VARCHAR, principal VARCHAR, started BOOLEAN, waiting_behind VARCHAR[], same_as VARCHAR)",
    ),
];

/// [`StepRow`]'s keys.
pub const MATERIALIZATION_COLUMNS: &[(&str, &str)] = &[
    ("node_id", V),
    ("partition", V),
    ("run_id", V),
    ("run_hash", V),
    ("status", V),
    ("created_at", V),
    ("elapsed_seconds", F),
    ("cpu_seconds", F),
    ("max_rss_bytes", I),
    ("attempts", I),
    ("format", V),
    ("size_bytes", I),
    ("artifact", V),
    ("content_hash", V),
    ("error_type", V),
    ("error_message", V),
    ("rows", I),
];

/// `barca status --json`'s node, flattened: `cache.*` without the prefix, `partitions.*` and
/// `last_materialization.*` as `partitions_*` and `last_*`.
pub const NODE_COLUMNS: &[(&str, &str)] = &[
    ("id", V),
    ("name", V),
    ("kind", V),
    ("partitioned", B),
    ("inputs", L),
    ("env", L),
    ("state", V),
    ("reason", V),
    ("detail", V),
    ("health", V),
    ("health_reason", V),
    ("signals", L),
    ("measured", B),
    ("cause", V),
    ("root_id", V),
    ("root_kind", V),
    ("root_node", V),
    ("gap_seconds", I),
    ("last_check_at", V),
    ("last_check_outcome", V),
    ("refresh_schedule", V),
    ("next_tick", V),
    ("missed_ticks", I),
    ("stale_since", V),
    ("expected_fresh_at", V),
    ("recheck_at", V),
    ("no_estimate", V),
    ("run_hash", V),
    ("artifact", V),
    ("local", B),
    ("partitions_total", I),
    ("partitions_cached", I),
    ("partitions_missing", I),
    ("partitions_empty", I),
    ("last_status", V),
    ("last_created_at", V),
    ("last_elapsed_seconds", F),
    ("last_run_hash", V),
    ("last_artifact", V),
    ("last_format", V),
    ("last_size_bytes", I),
    ("last_rows", I),
    ("last_content_hash", V),
    ("last_error", V),
];

/// A scheduled node's `schedule` block (`barca status --json`, `GET /nodes`) with the node's
/// `id`, `name` and `kind`, how far apart its slots are and the targets of one tick.
pub const SCHEDULE_COLUMNS: &[(&str, &str)] = &[
    ("id", V),
    ("name", V),
    ("kind", V),
    ("cron", V),
    ("interval_seconds", I),
    ("targets", L),
    ("next_fire", V),
    ("last_fired", V),
    ("last_status", V),
    ("last_run_id", V),
    ("state", V),
    ("state_detail", V),
    ("late", B),
    ("late_since", V),
    ("paused", B),
    ("paused_until", V),
    ("failing", B),
    ("failures", I),
    ("max_failures", I),
    ("skip_reason", V),
    ("live", B),
    ("typical_queue_wait_seconds", I),
    ("typical_run_seconds", I),
    ("queue_bound", B),
];

/// A recorded tick ([`TickRecord`]): its times as RFC 3339 UTC (the run's with
/// milliseconds), its counts as columns.
pub const TICK_COLUMNS: &[(&str, &str)] = &[
    ("schedule_id", V),
    ("scheduled_at", V),
    ("action", V),
    ("skipped_for", V),
    ("run_id", V),
    ("run_status", V),
    ("started_at", V),
    ("finished_at", V),
    ("queue_wait_seconds", F),
    ("rebuilt", I),
    ("up_to_date", I),
    ("failed", I),
    ("not_reached", I),
    ("targets", "STRUCT(node VARCHAR, outcome VARCHAR)[]"),
];

/// The columns of `barca.queue` ([`queue`]).
pub const QUEUE_COLUMNS: &[(&str, &str)] = &[
    ("run_id", V),
    ("position", I),
    ("state", V),
    ("queued_at", V),
    ("started_at", V),
    ("estimated_start_at", V),
    ("wait_seconds", I),
    ("command", V),
    ("target", V),
    ("triggered_by", V),
    ("schedule_id", V),
    ("scheduled_at", V),
];

/// A table of the `barca` schema over rows already in its columns' shape (`None`: a
/// placeholder): what a server's `POST /sql/query` answered for `select * from barca.<name>`.
pub fn table(
    name: &'static str,
    columns: &'static [(&'static str, &'static str)],
    rows: Option<Vec<Value>>,
) -> Table {
    Table {
        schema: SCHEMA,
        name,
        columns,
        rows,
        note: None,
    }
}

fn values<T: Serialize>(rows: &[T]) -> Vec<Value> {
    rows.iter()
        .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
        .collect()
}

pub fn runs(rows: Option<&[RunRecord]>) -> Table {
    table(
        RUNS,
        RUN_COLUMNS,
        rows.map(|rows| rows.iter().map(crate::report::history_json).collect()),
    )
}

pub fn materializations(rows: Option<&[StepRow]>) -> Table {
    table(MATERIALIZATIONS, MATERIALIZATION_COLUMNS, rows.map(values))
}

pub fn nodes(rows: Option<&[NodeStatus]>) -> Table {
    table(
        NODES,
        NODE_COLUMNS,
        rows.map(|nodes| nodes.iter().map(node_row).collect()),
    )
}

fn node_row(n: &NodeStatus) -> Value {
    let p = n.partitions.as_ref();
    let m = n.last_materialization.as_ref();
    let mut row = json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind,
        "partitioned": n.partitioned,
        "inputs": n.inputs,
        "env": n.env,
        "state": n.cache.state,
        "reason": n.cache.reason,
        "detail": n.cache.detail,
        "health": n.health,
        "health_reason": n.health_reason,
        "signals": n.signals.iter().map(|s| s.kind()).collect::<Vec<_>>(),
        "measured": n.measured,
        "last_check_at": n.last_check.as_ref().map(|c| &c.at),
        "last_check_outcome": n.last_check.as_ref().map(|c| c.outcome),
        "refresh_schedule": n.refresh_path.as_ref().map(|p| &p.schedule),
        "next_tick": n.refresh_path.as_ref().and_then(|p| p.next_tick.as_ref()),
        "missed_ticks": n.refresh_path.as_ref().map(|p| p.missed_ticks),
        "stale_since": n.stale_since,
        "expected_fresh_at": n.expected_fresh_at,
        "recheck_at": n.recheck_at,
        "no_estimate": n.no_estimate.map(crate::health::NoEstimate::as_str),
        "run_hash": n.cache.run_hash,
        "artifact": n.cache.artifact,
        "local": n.local,
        "partitions_total": p.map(|p| p.total),
        "partitions_cached": p.map(|p| p.cached),
        "partitions_missing": p.map(|p| p.missing),
        "partitions_empty": p.map(|p| p.empty),
        "last_status": m.map(|m| &m.status),
        "last_created_at": m.map(|m| &m.created_at),
        "last_elapsed_seconds": m.and_then(|m| m.elapsed_seconds),
        "last_run_hash": m.and_then(|m| m.run_hash.as_ref()),
        "last_artifact": m.and_then(|m| m.artifact.as_ref()),
        "last_format": m.and_then(|m| m.format.as_ref()),
        "last_size_bytes": m.and_then(|m| m.size_bytes),
        "last_rows": m.and_then(|m| m.rows),
        "last_content_hash": m.and_then(|m| m.content_hash.as_ref()),
        "last_error": m.and_then(|m| m.error.as_ref()),
    });
    // Beside the macro: one more key in it passes its recursion limit.
    let cause = json!({
        "cause": n.cause,
        "root_id": n.root.as_ref().map(|r| &r.id),
        "root_kind": n.root.as_ref().map(|r| r.kind),
        "root_node": n.root.as_ref().map(|r| &r.node),
        "gap_seconds": n.gap_seconds,
    });
    if let (Value::Object(row), Value::Object(cause)) = (&mut row, cause) {
        row.extend(cause);
    }
    row
}

/// `barca.schedules`: one row per scheduled node of `nodes` (the ones with a `schedule`
/// block), in their order.
pub fn schedules(rows: Option<Vec<Value>>) -> Table {
    table(SCHEDULES, SCHEDULE_COLUMNS, rows)
}

/// The rows of [`schedules`]: each scheduled node's `schedule` block as status attached it
/// (this machine's records, or a server's scheduler), with the targets of one tick
/// (`targets`: the scheduler's own list for the scheduled node's id) and the distance between
/// its slots as of `now` ([`crate::schedule_standing::interval_seconds`]).
pub fn schedule_rows(
    nodes: &[NodeStatus],
    targets: impl Fn(&str) -> Vec<String>,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Value> {
    nodes
        .iter()
        .filter_map(|n| {
            let s = n.schedule.as_ref()?;
            let interval = crate::CronExpr::parse(&s.cron)
                .ok()
                .and_then(|cron| crate::schedule_standing::interval_seconds(&cron, zone, now));
            let mut row = serde_json::to_value(s).unwrap_or(Value::Null);
            let more = json!({
                "id": n.id,
                "name": n.name,
                "kind": n.kind,
                "interval_seconds": interval,
                "targets": targets(&n.id),
            });
            if let (Value::Object(row), Value::Object(more)) = (&mut row, more) {
                row.extend(more);
            }
            Some(row)
        })
        .collect()
}

/// The targets of one tick of `schedule`, read off the walk kept per graph
/// ([`crate::refresh_path::Paths`], the scheduler's own): the scheduled node, then every other
/// node of `nodes` its ticks have as a target, in their (dependency) order.
pub fn targets_in(
    nodes: &[NodeStatus],
    paths: &crate::refresh_path::Paths,
    schedule: &str,
) -> Vec<String> {
    std::iter::once(schedule.to_string())
        .chain(
            nodes
                .iter()
                .filter(|n| {
                    n.id != schedule && paths.schedules(&n.id).iter().any(|s| s == schedule)
                })
                .map(|n| n.id.clone()),
        )
        .collect()
}

/// `barca.ticks`: one row per recorded tick.
pub fn ticks(rows: Option<&[TickRecord]>) -> Table {
    table(
        TICKS,
        TICK_COLUMNS,
        rows.map(|ticks| ticks.iter().map(tick_row).collect()),
    )
}

fn tick_row(t: &TickRecord) -> Value {
    // The run's times to the millisecond, as a served run's are; the slot is whole seconds.
    let at = |secs: Option<f64>| secs.map(crate::time::rfc3339_millis);
    json!({
        "schedule_id": t.schedule_id,
        "scheduled_at": rfc3339_secs(t.scheduled_at),
        "action": t.action,
        "skipped_for": t.skipped_for,
        "run_id": t.run_id,
        "run_status": t.run_status,
        "started_at": at(t.started_at),
        "finished_at": at(t.finished_at),
        "queue_wait_seconds": t.queue_wait_seconds,
        "rebuilt": t.counts.map(|c| c.rebuilt),
        "up_to_date": t.counts.map(|c| c.up_to_date),
        "failed": t.counts.map(|c| c.failed),
        "not_reached": t.counts.map(|c| c.not_reached),
        "targets": t.targets.as_ref().map(|targets| {
            targets
                .iter()
                .map(|(node, outcome)| json!({"node": node, "outcome": outcome}))
                .collect::<Vec<_>>()
        }),
    })
}

/// `barca.queue`: one row per run in a server's run queue, started runs first, then the
/// waiting ones in order. A row is an entry of `GET /queue` in the table's columns
/// (`views::QueueEntry::table_row`).
pub fn queue(rows: Option<Vec<Value>>) -> Table {
    table(QUEUE, QUEUE_COLUMNS, rows)
}

/// What stderr says when a query names `barca.queue` on a machine without a server.
pub const NO_QUEUE_HERE: &str = "barca.queue is empty: the run queue is a server's, and no \
     server answered this query (`--server <url>` reads one's)";

/// Whether the query names table `name` (its name as a word, with or without `barca.`).
pub fn named(query: &str, name: &str) -> bool {
    super::names(query, name)
}

/// The note for a table whose rows were cut off at a server's limit.
pub fn truncated_note(name: &str, shown: usize, total: usize, url: &str) -> Option<String> {
    (total > shown).then(|| {
        format!(
            "barca.{name} holds the newest {shown} of the {total} rows on the server at {url} \
             (barca sql reads at most {SERVER_ROWS} rows of each of barca's tables from a server)"
        )
    })
}

/// barca's tables from this machine's metadata DB, loading the ones `query` names. `status` is
/// the status the views were resolved from, `dag` the graph it was computed over. A database that does not exist yet has no rows.
pub async fn local(
    cfg: &ResolvedConfig,
    query: &str,
    status: &[NodeStatus],
    dag: &Dag,
) -> Result<Vec<Table>, BarcaError> {
    let exists = Path::new(&cfg.db_path).exists();
    let runs_rows = if named(query, RUNS) {
        Some(if exists {
            let total = crate::db::count_runs(&cfg.db_path).await?;
            crate::db::get_recent_runs(&cfg.db_path, total).await?
        } else {
            Vec::new()
        })
    } else {
        None
    };
    let step_rows = if named(query, MATERIALIZATIONS) {
        Some(if exists {
            crate::db::read_step_rows(&cfg.db_path, usize::MAX >> 1, 0)
                .await?
                .0
        } else {
            Vec::new()
        })
    } else {
        None
    };
    let tick_rows = if named(query, TICKS) {
        Some(if exists {
            crate::db::recent_ticks(&cfg.db_path, usize::MAX >> 1)
                .await?
                .0
        } else {
            Vec::new()
        })
    } else {
        None
    };
    // The schedules as this machine's metadata records them: no scheduler runs here.
    let schedule_rows = if named(query, SCHEDULES) {
        let now = chrono::Utc::now();
        let jobs = crate::schedule::jobs_from_dag(dag);
        let mut scheduled: Vec<NodeStatus> = status
            .iter()
            .filter(|n| jobs.iter().any(|j| j.id == n.id))
            .cloned()
            .collect();
        crate::status::attach_schedules_of(
            &mut scheduled,
            &jobs,
            Some(&cfg.db_path),
            &Zone::Local,
            now,
        )
        .await?;
        let targets = |id: &str| {
            jobs.iter()
                .find(|j| j.id == id)
                .map_or_else(Vec::new, |j| j.targets())
        };
        Some(schedule_rows(&scheduled, targets, &Zone::Local, now))
    } else {
        None
    };
    // Only a server holds a run queue: here the table exists and is empty, and says why.
    let mut queue = queue(named(query, QUEUE).then(Vec::new));
    if queue.rows.is_some() {
        queue.note = Some(NO_QUEUE_HERE.to_string());
    }
    Ok(vec![
        runs(runs_rows.as_deref()),
        materializations(step_rows.as_deref()),
        nodes(named(query, NODES).then_some(status)),
        schedules(schedule_rows),
        ticks(tick_rows.as_deref()),
        queue,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_columns_are_the_history_keys() {
        let run: RunRecord = serde_json::from_value(json!({
            "run_id": "r", "command": "get", "files": [], "target": null, "status": "success",
            "steps_total": 1, "steps_executed": 1, "steps_cached": 0, "started_at": "t",
            "finished_at": null, "elapsed_seconds": null
        }))
        .unwrap();
        let keys: Vec<String> = crate::report::history_json(&run)
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut columns: Vec<String> = RUN_COLUMNS.iter().map(|(c, _)| c.to_string()).collect();
        // `line` is the run in the words of a list row (#959): computed from these columns, not
        // one of them.
        let mut keys: Vec<String> = keys.into_iter().filter(|k| k != "line").collect();
        keys.sort();
        columns.sort();
        assert_eq!(keys, columns);
    }

    #[test]
    fn materialization_columns_are_the_step_row_keys() {
        let row = StepRow {
            node_id: "p.py:a".into(),
            partition: None,
            run_id: None,
            run_hash: None,
            status: "success".into(),
            created_at: String::new(),
            elapsed_seconds: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            attempts: None,
            format: None,
            size_bytes: None,
            artifact: None,
            content_hash: None,
            error_type: None,
            error_message: None,
            rows: None,
        };
        let value = serde_json::to_value(&row).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        let columns: Vec<&str> = MATERIALIZATION_COLUMNS.iter().map(|(c, _)| *c).collect();
        let mut keys: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
        let mut sorted = columns.clone();
        keys.sort();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    fn sorted_keys(row: &Value) -> Vec<String> {
        let mut keys: Vec<String> = row.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    fn sorted_columns(columns: &[(&str, &str)]) -> Vec<String> {
        let mut names: Vec<String> = columns.iter().map(|(c, _)| c.to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn schedule_columns_are_the_schedule_block_and_what_names_the_node() {
        let mut node: NodeStatus = serde_json::from_value(json!({
            "id": "pipeline.py:inbox", "name": "inbox", "kind": "sensor", "inputs": [],
            "partitioned": false,
            "cache": {"state": "always_runs", "reason": "always_runs", "detail": ""}
        }))
        .unwrap();
        node.schedule = Some(crate::status::ScheduleStatus {
            cron: "*/5 * * * *".into(),
            ..Default::default()
        });
        let other = NodeStatus {
            id: "pipeline.py:orders".into(),
            schedule: None,
            ..node.clone()
        };
        let now = chrono::DateTime::from_timestamp(1_794_927_780, 0).unwrap();
        let rows = schedule_rows(
            &[node, other],
            |id| vec![id.to_string(), "pipeline.py:orders".into()],
            &Zone::Utc,
            now,
        );
        // One row per scheduled node; a node without a schedule is not one.
        assert_eq!(rows.len(), 1);
        assert_eq!(sorted_keys(&rows[0]), sorted_columns(SCHEDULE_COLUMNS));
        assert_eq!(rows[0]["interval_seconds"], 300);
        assert_eq!(
            rows[0]["targets"],
            json!(["pipeline.py:inbox", "pipeline.py:orders"])
        );
    }

    #[test]
    fn tick_columns_are_the_tick_record_with_times_as_text() {
        use crate::db::{TickCounts, TickDecision, TickTargetOutcome};
        let tick = TickRecord {
            schedule_id: "pipeline.py:inbox".into(),
            scheduled_at: 1_794_927_600,
            action: TickDecision::Fired,
            skipped_for: None,
            run_id: Some("run-1".into()),
            started_at: Some(1_794_927_602.5),
            finished_at: Some(1_794_927_610.25),
            run_status: Some("success".into()),
            queue_wait_seconds: Some(2.5),
            counts: Some(TickCounts {
                rebuilt: 1,
                up_to_date: 1,
                failed: 0,
                not_reached: 0,
            }),
            targets: Some(vec![
                ("pipeline.py:inbox".into(), TickTargetOutcome::Rebuilt),
                ("pipeline.py:orders".into(), TickTargetOutcome::UpToDate),
            ]),
        };
        let row = tick_row(&tick);
        assert_eq!(sorted_keys(&row), sorted_columns(TICK_COLUMNS));
        assert_eq!(row["scheduled_at"], "2026-11-17T15:00:00Z");
        assert_eq!(row["started_at"], "2026-11-17T15:00:02.500Z");
        assert_eq!(row["action"], "fired");
        assert_eq!(row["up_to_date"], 1);
        assert_eq!(
            row["targets"][1],
            json!({"node": "pipeline.py:orders", "outcome": "up_to_date"})
        );
        // A tick that did not fire has no counts and no targets.
        let paused = tick_row(&TickRecord {
            action: TickDecision::Paused,
            run_id: None,
            counts: None,
            targets: None,
            ..tick
        });
        assert_eq!(paused["rebuilt"], Value::Null);
        assert_eq!(paused["targets"], Value::Null);
    }

    #[test]
    fn queue_columns_are_the_queue_row_keys() {
        let row = crate::views::QueueEntry {
            run_id: "run-1".into(),
            status: "pending".into(),
            position: Some(1),
            state: Some(crate::run_queue::QueueState::Queued),
            queued_at: "2026-11-17T15:00:00.000Z".into(),
            started_at: None,
            estimated_start_at: None,
            wait_seconds: None,
            command: "get".into(),
            target: None,
            target_line: "all assets".into(),
            triggered_by: crate::trigger::TriggeredBy::Schedule,
            schedule_id: Some("pipeline.py:inbox".into()),
            scheduled_at: Some("2026-11-17T15:00:00Z".into()),
            waiting_for: None,
        };
        let table = queue(Some(vec![row.table_row()]));
        let rows = table.rows.unwrap();
        assert_eq!(sorted_keys(&rows[0]), sorted_columns(QUEUE_COLUMNS));
        assert_eq!(rows[0]["triggered_by"], "schedule");
        assert_eq!(rows[0]["state"], "queued");
    }

    #[test]
    fn every_table_is_listed_by_name() {
        let listed = [
            runs(None).name,
            materializations(None).name,
            nodes(None).name,
            schedules(None).name,
            ticks(None).name,
            queue(None).name,
        ];
        assert_eq!(listed, NAMES);
    }

    #[test]
    fn a_table_is_named_with_or_without_its_schema() {
        assert!(named("select * from barca.runs", RUNS));
        assert!(named("SELECT * FROM BARCA.RUNS", RUNS));
        assert!(named("select * from barca.\"nodes\"", NODES));
        assert!(!named("select * from runs_daily", RUNS));
    }
}
