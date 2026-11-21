//! Shared result rendering and JSON projection for frontends.
//!
//! Tables return their complete text, including newlines, so callers choose their output
//! stream. JSON helpers preserve the CLI schema and field selection.

mod commands;
mod keys;
pub use commands::{
    GetShape, HistoryExtras, HistoryNow, OutputChunk, RenderedOutput, ResultFormat, Returned,
    Shown, history_json, history_target, render_activity, render_explain, render_failed_get,
    render_failed_run, render_get, render_get_multi, render_history, render_list, render_multi,
    render_plan, render_rollup, render_run, render_run_detail, render_sql, render_stats,
    render_status,
};
pub(crate) use commands::{
    failed_run_body, multi_json, multi_parts, render_failed_get_returned, render_get_body,
    render_get_multi_parts, render_multi_parts, render_run_body, returned_from_pointers, run_body,
    run_label,
};
pub use keys::{
    render_partition_grid, render_partition_key, render_partition_keys, render_partition_overview,
};

use crate::cache_state::CacheState;
use crate::results::*;
use crate::step_status::StepStatus;
pub use crate::text::utc_cell;

use serde_json::{Map, Value};
use std::collections::HashMap;

/// A JSON object of `pairs` with keys in the given order (serde_json maps sort their keys).
pub(crate) fn ordered_object(pairs: &[(String, serde_json::Value)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}:{}",
                serde_json::Value::from(k.as_str()),
                serde_json::to_string(v).unwrap()
            )
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

/// STATUS / WHY / STEP table for dry runs and (in `-o pretty`) real runs.
pub(crate) fn render_step_table(steps: &[crate::results::StepReport], dry: bool) -> String {
    use std::fmt::Write;
    let mut rendered = String::new();
    let rows: Vec<(String, String, &str)> = steps
        .iter()
        .map(|s| {
            let verdict = s.action.as_deref().or(s.status.as_deref()).unwrap_or("?");
            let label = match (dry, verdict) {
                (true, "run") => "will run",
                (_, v) => v,
            };
            let why = match (&s.partitions, &s.detail) {
                (Some(p), _) if StepStatus::Partial.is(Some(verdict)) => {
                    format!(
                        "{} of {} keys cached; will run: {}",
                        p.cached,
                        p.total,
                        p.will_run_keys
                            .iter()
                            // What changed in a key that ran before (a dict source's new value).
                            .map(|k| match p.changed.as_ref().and_then(|c| c.get(k)) {
                                Some(changed) => {
                                    format!("{k} ({} changed)", crate::changes::summary(changed))
                                }
                                None => k.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    ) + &s
                        .batch
                        .as_ref()
                        .map(|b| format!("; {}", crate::batch::grouping_line(b)))
                        .unwrap_or_default()
                }
                (Some(p), Some(d)) => format!("{} keys; {d}", p.total),
                (None, Some(d)) => d.clone(),
                (Some(p), None) => format!("{} keys cached", p.total),
                (None, None) => "-".to_string(),
            };
            (label.to_string(), why, s.id.as_str())
        })
        .collect();
    let w_status = rows.iter().map(|r| r.0.len()).max().unwrap_or(6).max(6);
    let w_why = rows
        .iter()
        .map(|r| r.1.len())
        .max()
        .unwrap_or(3)
        .clamp(3, 70);
    writeln!(
        &mut rendered,
        "{:<w_status$}  {:<w_why$}  STEP",
        "STATUS", "WHY"
    )
    .unwrap();
    for (label, why, id) in &rows {
        writeln!(&mut rendered, "{label:<w_status$}  {why:<w_why$}  {id}").unwrap();
    }
    for s in steps {
        if let Some(w) = &s.warning {
            writeln!(&mut rendered, "\n  ! {w}").unwrap();
        }
    }
    rendered
}

/// `freshness` as `barca list` prints it: lowercase, like `kind`.
pub(crate) fn freshness_str(f: &crate::Freshness) -> &'static str {
    match f {
        crate::Freshness::Always => "always",
        crate::Freshness::Manual => "manual",
        crate::Freshness::Schedule(_) => "schedule",
    }
}

/// `freshness` as `barca list --json` and `GET /assets` show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ListFreshness {
    Always,
    Manual,
    Schedule,
}

/// One `nodes[]` entry of `barca list --json`, and one entry of the HTTP API's `GET /assets`:
/// `freshness` is a flat lowercase word, with the cron expression in `schedule` and the next
/// fire time in `next_fire` for scheduled nodes.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ListNode {
    pub id: String,
    pub kind: crate::NodeKind,
    pub freshness: ListFreshness,
    /// The cron expression of a scheduled node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub schedule: Option<String>,
    /// A scheduled node's next fire time, in local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub next_fire: Option<String>,
    pub inputs: Vec<String>,
    pub env: Vec<String>,
    /// Source file of the definition, as in the id.
    pub file: String,
    /// 1-based line of the definition's first decorator.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub line: usize,
}

impl ListNode {
    pub fn of(a: &crate::results::AssetSummary, next_fire: Option<&String>) -> Self {
        Self {
            id: a.id.clone(),
            kind: a.kind,
            freshness: match &a.freshness {
                crate::Freshness::Always => ListFreshness::Always,
                crate::Freshness::Manual => ListFreshness::Manual,
                crate::Freshness::Schedule(_) => ListFreshness::Schedule,
            },
            schedule: match &a.freshness {
                crate::Freshness::Schedule(cron) => Some(cron.0.clone()),
                _ => None,
            },
            next_fire: next_fire.cloned(),
            inputs: a.inputs.clone(),
            env: a.env.clone(),
            file: a.file.clone(),
            line: a.line,
        }
    }
}

/// [`ListNode`] as JSON.
pub fn list_node_json(
    a: &crate::results::AssetSummary,
    next_fire: Option<&String>,
) -> serde_json::Value {
    serde_json::to_value(ListNode::of(a, next_fire)).unwrap_or(serde_json::Value::Null)
}

/// One row per node: NAME KIND STATE WHY LAST RUN SHAPE DEPS.
pub(crate) fn render_status_table(result: &crate::status::StatusResult) -> String {
    use std::fmt::Write;
    let mut rendered = String::new();
    let now = chrono::Utc::now().timestamp();
    let short = |id: &str| -> String {
        let base = id.split('[').next().unwrap_or(id);
        base.rsplit(':').next().unwrap_or(base).to_string()
    };
    // Like NEXT FIRE in `barca list`, SCHEDULE appears only when a shown node is scheduled.
    let has_schedule = result.nodes.iter().any(|n| n.schedule.is_some());
    // "no refresh path" is said only where something shown is scheduled: a project with no
    // schedule has none anywhere, and every row would say so.
    let has_paths = has_schedule || result.nodes.iter().any(|n| n.refresh_path.is_some());
    let rows: Vec<Vec<String>> = result
        .nodes
        .iter()
        .map(|n| {
            let why = match &n.partitions {
                // `39 of 39 missing keys wait on 'demand_forecast': ...` (#764).
                Some(_) if n.cache.state == CacheState::Blocked => n.cache.detail.clone(),
                Some(p) if n.cache.state != CacheState::Cached => {
                    format!(
                        "{} of {} keys cached; {}",
                        p.cached, p.total, n.cache.reason
                    )
                }
                // Human form of an `unsynced` reason: `local only`, `not in ledger`.
                _ if n.cache.state == CacheState::Unsynced => n.cache.reason.replace('_', " "),
                // `changed: helper pipelines/common.py:bucket_ltv (+1 more)` (#558).
                _ if n.cache.reason == "changed" && n.cache.changed.is_some() => {
                    n.cache.detail.clone()
                }
                _ => n.cache.reason.clone(),
            };
            // A failure's message in short: `failed: ZeroDivisionError: division by zero`.
            let why = match n.last_materialization.as_ref() {
                Some(m) if m.status == "failed" && n.cache.state != CacheState::Cached => {
                    let error = m
                        .error
                        .as_deref()
                        .map(|e| crate::status::short_error(e, 80))
                        .filter(|e| !e.is_empty())
                        .unwrap_or_else(|| "no message".to_string());
                    if n.cache.reason == "failed" {
                        format!("failed: {error}")
                    } else if n.cache.state == CacheState::AlwaysRuns {
                        format!("last run failed: {error}")
                    } else {
                        format!("{why}; last run failed: {error}")
                    }
                }
                _ => why,
            };
            // What the read path cannot decide, a tick did: `cutoff_pending; checked 3m ago,
            // up to date` (#964).
            let why = match &n.last_check {
                Some(check) if n.cache.state == CacheState::Unknown => {
                    format!("{why}; {}", check.line(now))
                }
                _ => why,
            };
            // What will bring a node that is not fresh up to date: `stale: upstream_stale;
            // next tick 15:05 UTC by inbox`, or `no refresh path` (#961). A scheduled node's
            // own schedule is in its SCHEDULE cell.
            let why = match &n.refresh_path {
                _ if n.health.is_fresh() || n.schedule.is_some() => why,
                Some(path) => format!("{why}; {}", path.line(now)),
                None if has_paths => format!("{why}; {}", crate::refresh_path::NO_REFRESH_PATH),
                None => why,
            };
            // When: `fresh ~15:05 UTC` (an estimate), `overdue 16h`, or `rechecked 15:05 UTC`
            // where a tick can only look again (#962). The path's line already names the
            // next tick, so a recheck at that tick is not said twice.
            let why = match crate::estimates::line(n, now) {
                Some(when) if !when.starts_with("rechecked") || n.schedule.is_some() => {
                    format!("{why}; {when}")
                }
                _ => why,
            };
            let last = n
                .last_materialization
                .as_ref()
                .map(|m| {
                    let secs = m
                        .elapsed_seconds
                        .map(|e| format!(" {e:.2}s"))
                        .unwrap_or_default();
                    format!("{} {}{secs}", m.status, utc_cell(&m.created_at))
                })
                .unwrap_or_else(|| "-".to_string());
            let deps = if n.inputs.is_empty() {
                "-".to_string()
            } else {
                n.inputs
                    .iter()
                    .map(|i| short(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mut row = vec![
                n.name.clone(),
                n.kind.clone(),
                // Human form: `never-run`, `always-runs` (JSON says `never_run`, `always_runs`).
                n.cache.state.as_str().replace('_', "-"),
                why,
                last,
                n.shape
                    .as_ref()
                    .map(shape_cell)
                    .unwrap_or_else(|| "-".to_string()),
            ];
            if has_schedule {
                row.push(
                    n.schedule
                        .as_ref()
                        .map(|s| s.cell())
                        .unwrap_or_else(|| "-".to_string()),
                );
            }
            row.push(deps);
            row
        })
        .collect();
    let mut header = vec!["NAME", "KIND", "STATE", "WHY", "LAST RUN", "SHAPE"];
    if has_schedule {
        header.push("SCHEDULE");
    }
    header.push("DEPS");
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain([header[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut line = |cells: &[String]| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                out.push_str(c);
            } else {
                out.push_str(&format!("{c:<w$}  ", w = widths[i]));
            }
        }
        writeln!(&mut rendered, "{}", out.trim_end()).unwrap();
    };
    let filter = result.filter.as_ref().filter(|f| !f.is_empty());
    if rows.is_empty() && filter.is_some() {
        writeln!(
            &mut rendered,
            "No node matches {}.",
            filter.map(|f| f.describe()).unwrap_or_default()
        )
        .unwrap();
    } else {
        line(&header.iter().map(|h| h.to_string()).collect::<Vec<_>>());
        for r in &rows {
            line(r);
        }
    }
    for node in &result.nodes {
        if let Some(event) = &node.last_bytes_change {
            writeln!(
                &mut rendered,
                "{}: different bytes for the same code and inputs at {} ({})",
                node.name,
                utc_cell(&event.created_at),
                event.node_id
            )
            .unwrap();
        }
    }
    let s = &result.summary;
    if let Some(filter) = filter {
        let all = s.cached
            + s.unsynced
            + s.stale
            + s.never_run
            + s.partial
            + s.blocked
            + s.unknown
            + s.always_runs;
        writeln!(
            &mut rendered,
            "\n{} of {} match {}; the counts below are for every node",
            filter.matched,
            plural(all as u64, "node"),
            filter.describe()
        )
        .unwrap();
    }
    writeln!(
        &mut rendered,
        "\n{} cached, {}{} stale, {} never run, {} partial, {}{} unknown, {} always run",
        s.cached,
        match s.unsynced {
            0 => String::new(),
            n => format!("{n} unsynced, "),
        },
        s.stale,
        s.never_run,
        s.partial,
        match s.blocked {
            0 => String::new(),
            n => format!("{n} blocked, "),
        },
        s.unknown,
        s.always_runs
    )
    .unwrap();
    // Health beside the cache states: the words of the nodes listed, most severe first, and
    // how much of the scope's data was measured (never "no suspect data" alone).
    let words: Vec<String> = crate::health::Health::ALL
        .iter()
        .filter_map(|h| {
            let n = result.rollup.as_ref().map_or_else(
                || result.nodes.iter().filter(|node| node.health == *h).count(),
                |scope| scope.health.get(*h),
            );
            (n > 0).then(|| format!("{n} {}", h.word()))
        })
        .collect();
    if !words.is_empty() {
        let listed = if result.rollup.is_none() && filter.is_some() {
            " (nodes listed)"
        } else {
            ""
        };
        writeln!(&mut rendered, "Health{listed}: {}", words.join(", ")).unwrap();
    }
    // How far the scope is from fresh, and why: every node in scope, whatever is listed.
    if let Some(rollup) = &result.rollup {
        let as_of = result
            .as_of
            .as_deref()
            .and_then(crate::time::parse_rfc3339)
            .unwrap_or(now);
        writeln!(&mut rendered, "Gap: {}", rollup.gap_line(as_of)).unwrap();
    }
    if result.data_coverage.assets > 0 {
        writeln!(&mut rendered, "Data: {}", result.data_coverage.line()).unwrap();
    }
    let usage = &result.disk_usage;
    let bytes = |n: Option<u64>| {
        n.map(crate::text::human)
            .unwrap_or_else(|| "unmeasured".to_string())
    };
    let limit = usage
        .limit_bytes
        .map(crate::text::human)
        .unwrap_or_else(|| "off".to_string());
    writeln!(&mut rendered, "Cache: {} budgeted (limit {limit}); {} metadata backups outside the budget; {} physical total",
        bytes(usage.budget_bytes), bytes(usage.backup_bytes), bytes(usage.size_bytes)).unwrap();
    for trim in usage.metadata_trim.iter().filter(|t| t.reason.is_some()) {
        writeln!(
            &mut rendered,
            "Metadata ({}): {} over its {} share; {}{}",
            trim.env,
            crate::text::human(trim.after_bytes),
            crate::text::human(trim.allowance_bytes),
            trim.detail.as_deref().unwrap_or_default(),
            trim.next_attempt
                .as_ref()
                .map(|at| format!("; next trim attempt after {at}"))
                .unwrap_or_default()
        )
        .unwrap();
    }
    rendered
}

/// Compact text for a shape object: `3 rows x 2 cols`, `dict (4 keys)`, `pandas.DataFrame`.
pub(crate) fn shape_cell(shape: &serde_json::Value) -> String {
    let rows = shape.get("rows").and_then(|v| v.as_u64());
    let cols = shape
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|c| c.len());
    let ty = shape.get("type").and_then(|v| v.as_str());
    match (rows, cols, ty) {
        (Some(r), Some(c), _) => format!("{} x {}", plural(r, "row"), plural(c as u64, "col")),
        (Some(r), None, _) => plural(r, "row"),
        (None, _, Some("dict")) => {
            let n = shape
                .get("key_count")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    shape
                        .get("keys")
                        .and_then(|k| k.as_array())
                        .map(|k| k.len() as u64)
                })
                .unwrap_or(0);
            format!("dict ({})", plural(n, "key"))
        }
        (None, _, Some(t)) => t.to_string(),
        _ => shape
            .get("note")
            .and_then(|v| v.as_str())
            .map(|n| format!("? ({n})"))
            .unwrap_or_else(|| "-".to_string()),
    }
}

pub fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// `barca stats --json` (and `GET /stats/{name}`, `GET /assets/{name}`): the node id is `id`,
/// as on every other command.
pub fn stats_json(stats: &crate::db::AssetStats) -> serde_json::Value {
    let mut out = serde_json::to_value(stats).unwrap();
    if let Some(obj) = out.as_object_mut()
        && let Some(id) = obj.remove("node_id")
    {
        obj.insert("id".into(), id);
    }
    out
}

// The JSON form of a result's `final_output` is the result's own (`results`), and reading a
// task's JSON artifacts is `values`; these keep the paths the CLI and the server name.
pub(crate) use crate::results::{KEY_SUMMARY, parse_key_summary};
pub use crate::results::{artifact_metadata, first_multi_failure, keyed_output, pointer_output};
pub(crate) use crate::values::read_task_output;

/// The stderr note under the paths of a summarized partitioned output.
pub(crate) fn key_summary_note(s: &crate::results::KeySummary) -> String {
    format!(
        "{} of {} keys not listed ({} cached{}): more than {} keys; select keys with \
         --partition to list their paths",
        s.keys - s.computed.min(crate::results::LISTED_KEYS),
        s.keys,
        s.keys - s.computed,
        match s.computed.saturating_sub(crate::results::LISTED_KEYS) {
            0 => String::new(),
            n => format!(", {n} computed"),
        },
        crate::results::LISTED_KEYS
    )
}

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
mod audit_rollup_tests;
#[cfg(test)]
mod status_table_tests;

#[cfg(test)]
mod utc_tests {
    use super::utc_cell;

    #[test]
    fn stored_timestamps_are_marked_utc() {
        assert_eq!(utc_cell("2026-10-21 02:04:05"), "2026-10-21 02:04:05 UTC");
        assert_eq!(utc_cell("2026-10-21T02:04:05Z"), "2026-10-21 02:04:05 UTC");
        assert_eq!(utc_cell(""), "");
    }
}

#[cfg(test)]
mod key_summary_output_tests {
    use crate::dispatch::OutputRef;
    use crate::results::{KeySummary, LISTED_KEYS};

    #[test]
    fn a_summarized_output_lists_the_computed_keys_and_reads_back() {
        let oref = OutputRef {
            path: "a.json".into(),
            format: "json".into(),
            size_bytes: 3,
            elapsed_seconds: None,
            content_hash: None,
        };
        let keys = vec![("k=a".to_string(), oref.clone())];
        // Few keys: an object keyed by partition key, as before.
        let small = super::pointer_output(None, Some(&keys), None).unwrap();
        assert_eq!(small["k=a"]["_barca_artifact"]["path"], "a.json");
        let summary = KeySummary {
            keys: 5000,
            computed: 1,
            listed: keys.clone(),
        };
        let out = super::pointer_output(None, Some(&keys), Some(&summary)).unwrap();
        let s = &out[super::KEY_SUMMARY];
        assert_eq!(s["keys"], 5000);
        assert_eq!(s["computed"], 1);
        assert_eq!(s["cached"], 4999);
        assert_eq!(s["truncated"], false);
        assert_eq!(s["outputs"]["k=a"]["_barca_artifact"]["path"], "a.json");
        assert!(s["hint"].as_str().unwrap().contains("--partition"));
        // A client prints the listed paths and the note from the JSON alone.
        let returned = crate::report::returned_from_pointers(Some(&out));
        assert_eq!(returned.shown.len(), 1);
        assert_eq!(returned.shown[0].key.as_deref(), Some("k=a"));
        assert_eq!(returned.key_summary, Some(summary.counts()));
        let note = super::key_summary_note(&summary);
        assert!(
            note.starts_with("4999 of 5000 keys not listed (4999 cached)"),
            "{note}"
        );
        // More computed keys than are listed: truncated.
        let cold = KeySummary {
            keys: 5000,
            computed: 5000,
            listed: keys,
        };
        let out = super::pointer_output(None, None, Some(&cold)).unwrap();
        assert_eq!(out[super::KEY_SUMMARY]["truncated"], true);
        assert!(
            super::key_summary_note(&cold).contains(&format!("{} computed", 5000 - LISTED_KEYS))
        );
    }
}
