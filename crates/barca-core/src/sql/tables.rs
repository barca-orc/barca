//! barca's own records as tables in the `barca` schema of `barca sql`: `barca.runs` (the runs
//! of `barca history --json`), `barca.materializations` (one row per step result) and
//! `barca.nodes` (`barca status --json`, one row per node, flattened).
//!
//! A table is loaded only when the query names it (its name as a word, as for remote views);
//! the others are typed placeholders, so `show tables` and `describe` see them, and reading one
//! is an error that says so. On this machine every row is loaded; from a server at most
//! [`SERVER_ROWS`] of each (newest first), with a note when there are more.

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::db::{RunRecord, StepRow};
use crate::status::NodeStatus;
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
    ("run_hash", V),
    ("artifact", V),
    ("local", B),
    ("partitions_total", I),
    ("partitions_cached", I),
    ("partitions_missing", I),
    ("last_status", V),
    ("last_created_at", V),
    ("last_elapsed_seconds", F),
    ("last_run_hash", V),
    ("last_artifact", V),
    ("last_format", V),
    ("last_size_bytes", I),
    ("last_content_hash", V),
    ("last_error", V),
];

fn table(
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
    json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind,
        "partitioned": n.partitioned,
        "inputs": n.inputs,
        "env": n.env,
        "state": n.cache.state,
        "reason": n.cache.reason,
        "detail": n.cache.detail,
        "run_hash": n.cache.run_hash,
        "artifact": n.cache.artifact,
        "local": n.local,
        "partitions_total": p.map(|p| p.total),
        "partitions_cached": p.map(|p| p.cached),
        "partitions_missing": p.map(|p| p.missing),
        "last_status": m.map(|m| &m.status),
        "last_created_at": m.map(|m| &m.created_at),
        "last_elapsed_seconds": m.and_then(|m| m.elapsed_seconds),
        "last_run_hash": m.and_then(|m| m.run_hash.as_ref()),
        "last_artifact": m.and_then(|m| m.artifact.as_ref()),
        "last_format": m.and_then(|m| m.format.as_ref()),
        "last_size_bytes": m.and_then(|m| m.size_bytes),
        "last_content_hash": m.and_then(|m| m.content_hash.as_ref()),
        "last_error": m.and_then(|m| m.error.as_ref()),
    })
}

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

/// barca's tables from this machine's metadata DB, loading the ones `query` names. `nodes` is
/// the status the views were resolved from. A database that does not exist yet has no rows.
pub async fn local(
    cfg: &ResolvedConfig,
    query: &str,
    status: &[NodeStatus],
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
    Ok(vec![
        runs(runs_rows.as_deref()),
        materializations(step_rows.as_deref()),
        nodes(named(query, NODES).then_some(status)),
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
        let mut keys = keys;
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

    #[test]
    fn a_table_is_named_with_or_without_its_schema() {
        assert!(named("select * from barca.runs", RUNS));
        assert!(named("SELECT * FROM BARCA.RUNS", RUNS));
        assert!(named("select * from barca.\"nodes\"", NODES));
        assert!(!named("select * from runs_daily", RUNS));
    }
}
