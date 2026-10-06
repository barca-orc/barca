//! Bounded output: `--limit`/`--all` truncation envelopes and `--fields` projection.
//!
//! List-shaped JSON (`list`, `history`) is an envelope `{<key>: [...], total, truncated, hint?}`
//! so a reader always knows whether it saw everything. `--fields` keeps only the named keys on
//! each item; the valid names per command are the constants below, which clap enforces (an
//! unknown name is a usage error, exit 2, that lists them). Field *values* are never shortened:
//! limits bound how many items are printed, not what an item says (error messages and
//! tracebacks are always complete).

use serde_json::{Map, Value};

/// Default number of nodes `barca list` prints. High enough that typical pipelines (tens of
/// nodes) are never truncated; a generated 300-node DAG is bounded and says so.
pub const LIST_DEFAULT_LIMIT: usize = 100;

/// Item fields of `barca list --json` (`nodes[]`). `next_fire` appears only on scheduled nodes.
pub const LIST_FIELDS: &[&str] = &[
    "id",
    "kind",
    "freshness",
    "schedule",
    "inputs",
    "env",
    "next_fire",
];

/// Item fields of `barca history --json` (`runs[]`).
pub const HISTORY_FIELDS: &[&str] = &[
    "run_id",
    "command",
    "files",
    "target",
    "status",
    "steps_total",
    "steps_executed",
    "steps_cached",
    "started_at",
    "finished_at",
    "elapsed_seconds",
];

/// Item fields of `steps[]` in `get`/`run` JSON (real and `--dry-run`). Optional fields only
/// appear on steps where they apply.
pub const STEP_FIELDS: &[&str] = &[
    "id",
    "kind",
    "action",
    "status",
    "reason",
    "detail",
    "run_hash",
    "artifact",
    "warning",
    "partitions",
    "env",
];

/// Item fields of `recent_runs[]` in `barca stats --json`.
pub const STATS_FIELDS: &[&str] = &[
    "elapsed_seconds",
    "status",
    "created_at",
    "error_message",
    "attempts",
];

/// Item fields of `barca status --json` (`nodes[]`). `partitions` appears only on partitioned
/// nodes.
pub const STATUS_FIELDS: &[&str] = &[
    "id",
    "name",
    "kind",
    "inputs",
    "partitioned",
    "cache",
    "partitions",
    "last_materialization",
    "shape",
    "env",
];

/// Fields of `barca docs --json`: `topics[]` items, or the single topic object.
pub const DOCS_FIELDS: &[&str] = &["name", "summary", "content"];

/// Keep only `fields` on each object in `items` (non-objects are left alone).
pub fn project(items: &mut [Value], fields: &[String]) {
    for item in items {
        project_one(item, fields);
    }
}

/// Keep only `fields` on one object.
pub fn project_one(item: &mut Value, fields: &[String]) {
    if let Value::Object(obj) = item {
        obj.retain(|k, _| fields.iter().any(|f| f == k));
    }
}

/// Apply `--fields` to the array at `obj[key]`, if both are present.
pub fn project_key(obj: &mut Value, key: &str, fields: Option<&[String]>) {
    if let (Some(fields), Some(Value::Array(items))) = (fields, obj.get_mut(key)) {
        project(items, fields);
    }
}

/// A truncated (or complete) page of a longer list.
pub struct Page {
    /// How many items exist in total.
    pub total: usize,
    /// True when fewer than `total` items are shown.
    pub truncated: bool,
}

impl Page {
    pub fn new(shown: usize, total: usize) -> Self {
        Page {
            total,
            truncated: shown < total,
        }
    }

    /// What to pass to see more, for the JSON `hint` and the table note.
    fn hint(&self, noun: &str) -> String {
        format!(
            "pass --limit N for more, or --all for all {} {noun}",
            self.total
        )
    }

    /// `{key: items, total, truncated, hint?}` — `hint` only when truncated.
    pub fn envelope(&self, key: &str, items: Vec<Value>, noun: &str) -> Value {
        let mut obj = Map::new();
        obj.insert(key.into(), Value::Array(items));
        obj.insert("total".into(), self.total.into());
        obj.insert("truncated".into(), self.truncated.into());
        if self.truncated {
            obj.insert("hint".into(), self.hint(noun).into());
        }
        Value::Object(obj)
    }

    /// One-line stderr note for human tables when truncated, e.g.
    /// `showing 10 of 57 runs; pass --limit N for more, or --all for all 57 runs`.
    pub fn note(&self, shown: usize, noun: &str) -> Option<String> {
        self.truncated.then(|| {
            format!(
                "showing {shown} of {} {noun}; {}",
                self.total,
                self.hint(noun)
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn keys(v: &Value) -> BTreeSet<String> {
        v.as_object().unwrap().keys().cloned().collect()
    }

    fn set(fields: &[&str]) -> BTreeSet<String> {
        fields.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn project_keeps_only_named_fields() {
        let mut items = vec![json!({"id": "a", "kind": "asset", "inputs": []}), json!(3)];
        project(&mut items, &["id".into(), "missing".into()]);
        assert_eq!(items, vec![json!({"id": "a"}), json!(3)]);
    }

    #[test]
    fn envelope_reports_truncation_with_a_hint() {
        let page = Page::new(2, 5);
        let v = page.envelope("runs", vec![json!(1), json!(2)], "runs");
        assert_eq!(v["total"], 5);
        assert_eq!(v["truncated"], true);
        assert!(v["hint"].as_str().unwrap().contains("--all"));
        assert_eq!(
            page.note(2, "runs").unwrap(),
            "showing 2 of 5 runs; pass --limit N for more, or --all for all 5 runs"
        );

        let full = Page::new(5, 5);
        assert_eq!(
            full.envelope("runs", vec![], "runs"),
            json!({"runs": [], "total": 5, "truncated": false})
        );
        assert!(full.note(5, "runs").is_none());
    }

    // The field lists drive `--fields` validation, so they must match what is serialized.

    #[test]
    fn list_fields_match_the_serialized_node() {
        let node = barca_core::commands::AssetSummary {
            id: "p.py:a".into(),
            kind: barca_core::NodeKind::Asset,
            freshness: barca_core::Freshness::Schedule(barca_core::CronExpr("0 6 * * *".into())),
            inputs: vec![],
            env: vec![],
        };
        let next = "2026-01-01 06:00".to_string();
        let v = crate::list_node_json(&node, Some(&next));
        assert_eq!(v["freshness"], "schedule");
        assert_eq!(v["schedule"], "0 6 * * *");
        assert_eq!(keys(&v), set(LIST_FIELDS));
    }

    #[test]
    fn history_fields_match_the_serialized_run() {
        let run = barca_core::db::RunRecord {
            run_id: String::new(),
            command: String::new(),
            files: vec![],
            target: None,
            status: String::new(),
            steps_total: None,
            steps_executed: 0,
            steps_cached: 0,
            started_at: String::new(),
            finished_at: None,
            elapsed_seconds: None,
        };
        assert_eq!(
            keys(&serde_json::to_value(run).unwrap()),
            set(HISTORY_FIELDS)
        );
    }

    #[test]
    fn step_fields_match_a_fully_populated_step() {
        let s = || Some(String::new());
        let step = barca_core::commands::StepReport {
            id: String::new(),
            kind: String::new(),
            action: s(),
            status: s(),
            reason: s(),
            detail: s(),
            run_hash: s(),
            artifact: s(),
            warning: s(),
            partitions: Some(Default::default()),
            env: Some(Default::default()),
        };
        assert_eq!(keys(&serde_json::to_value(step).unwrap()), set(STEP_FIELDS));
    }

    #[test]
    fn stats_fields_match_the_serialized_entry() {
        let e = barca_core::db::AssetRunEntry {
            elapsed_seconds: None,
            status: String::new(),
            created_at: String::new(),
            error_message: None,
            attempts: 0,
        };
        assert_eq!(keys(&serde_json::to_value(e).unwrap()), set(STATS_FIELDS));
    }

    #[test]
    fn status_fields_match_a_fully_populated_node() {
        use barca_core::status::{CacheStatus, NodeStatus, PartitionState};
        let node = NodeStatus {
            id: String::new(),
            name: String::new(),
            kind: String::new(),
            inputs: vec![],
            partitioned: true,
            cache: CacheStatus {
                state: String::new(),
                reason: String::new(),
                detail: String::new(),
                run_hash: None,
                artifact: None,
            },
            partitions: Some(PartitionState {
                total: 0,
                cached: 0,
                missing: 0,
                missing_keys: vec![],
            }),
            last_materialization: None,
            shape: None,
            env: vec![],
        };
        assert_eq!(
            keys(&serde_json::to_value(node).unwrap()),
            set(STATUS_FIELDS)
        );
    }

    #[test]
    fn docs_fields_match_the_topic_json() {
        let all = crate::docs::all_json();
        assert_eq!(keys(&all["topics"][0]), set(DOCS_FIELDS));
    }
}
