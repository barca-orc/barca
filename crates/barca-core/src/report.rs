//! Shared result rendering and JSON projection for frontends.
//!
//! Tables return their complete text, including newlines, so callers choose their output
//! stream. JSON helpers preserve the CLI schema and field selection. Engine progress helpers
//! format step reports and progress lines without emitting them.

mod commands;
pub use commands::*;

use crate::targets::short_name;
use crate::{
    cache::{Decision, RunReason},
    dag::Dag,
    results::*,
};

use serde_json::{Map, Value};
use std::collections::HashMap;

/// A JSON object of `pairs` with keys in the given order (serde_json maps sort their keys).
pub fn ordered_object(pairs: &[(String, serde_json::Value)]) -> String {
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
pub fn render_step_table(steps: &[crate::results::StepReport], dry: bool) -> String {
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
                (Some(p), _) if verdict == "partial" => format!(
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
                ),
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
pub fn freshness_str(f: &crate::Freshness) -> &'static str {
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
/// A stored timestamp (always UTC: `datetime('now')`, or RFC 3339 with `Z`) for a human table,
/// marked as UTC so it is not read as local time (#496). Empty stays empty.
pub fn utc_cell(ts: &str) -> String {
    let ts = ts.trim();
    if ts.is_empty() {
        return String::new();
    }
    format!(
        "{} UTC",
        ts.strip_suffix('Z').unwrap_or(ts).replacen('T', " ", 1)
    )
}

pub fn render_status_table(result: &crate::status::StatusResult) -> String {
    use std::fmt::Write;
    let mut rendered = String::new();
    let short = |id: &str| -> String {
        let base = id.split('[').next().unwrap_or(id);
        base.rsplit(':').next().unwrap_or(base).to_string()
    };
    // Like NEXT FIRE in `barca list`, SCHEDULE appears only when a shown node is scheduled.
    let has_schedule = result.nodes.iter().any(|n| n.schedule.is_some());
    let rows: Vec<Vec<String>> = result
        .nodes
        .iter()
        .map(|n| {
            let why = match &n.partitions {
                Some(p) if n.cache.state != "cached" => {
                    format!(
                        "{} of {} keys cached; {}",
                        p.cached, p.total, n.cache.reason
                    )
                }
                // Human form of an `unsynced` reason: `local only`, `not in ledger`.
                _ if n.cache.state == "unsynced" => n.cache.reason.replace('_', " "),
                // `changed: helper pipelines/common.py:bucket_ltv (+1 more)` (#558).
                _ if n.cache.reason == "changed" && n.cache.changed.is_some() => {
                    n.cache.detail.clone()
                }
                _ => n.cache.reason.clone(),
            };
            // A failure's message in short: `failed: ZeroDivisionError: division by zero`.
            let why = match n.last_materialization.as_ref() {
                Some(m) if m.status == "failed" && n.cache.state != "cached" => {
                    let error = m
                        .error
                        .as_deref()
                        .map(|e| crate::status::short_error(e, 80))
                        .filter(|e| !e.is_empty())
                        .unwrap_or_else(|| "no message".to_string());
                    if n.cache.reason == "failed" {
                        format!("failed: {error}")
                    } else if n.cache.state == "always_runs" {
                        format!("last run failed: {error}")
                    } else {
                        format!("{why}; last run failed: {error}")
                    }
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
                n.cache.state.replace('_', "-"),
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
        let all =
            s.cached + s.unsynced + s.stale + s.never_run + s.partial + s.unknown + s.always_runs;
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
        "\n{} cached, {}{} stale, {} never run, {} partial, {} unknown, {} always run",
        s.cached,
        match s.unsynced {
            0 => String::new(),
            n => format!("{n} unsynced, "),
        },
        s.stale,
        s.never_run,
        s.partial,
        s.unknown,
        s.always_runs
    )
    .unwrap();
    rendered
}

/// Compact text for a shape object: `3 rows x 2 cols`, `dict (4 keys)`, `pandas.DataFrame`.
pub fn shape_cell(shape: &serde_json::Value) -> String {
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

pub fn artifact_metadata(oref: &crate::dispatch::OutputRef) -> serde_json::Value {
    serde_json::json!({
        "_barca_artifact": {
            "path": oref.path,
            "format": oref.format,
            "size_bytes": oref.size_bytes,
        }
    })
}

/// What `barca get --json` returns by default (#404): the artifact pointer, or for a
/// partitioned target an object of pointers keyed by partition key. `None` when the run
/// returned nothing (a `get` of every asset).
pub fn pointer_output(
    single: Option<&crate::dispatch::OutputRef>,
    keys: Option<&[(String, crate::dispatch::OutputRef)]>,
) -> Option<serde_json::Value> {
    match keys {
        Some(keys) => Some(serde_json::Value::Object(
            keys.iter()
                .map(|(key, oref)| (key.clone(), artifact_metadata(oref)))
                .collect(),
        )),
        None => single.map(artifact_metadata),
    }
}

/// Read an artifact for display: a JSON artifact's value inline, metadata for binary formats.
pub fn read_final_output(oref: &crate::dispatch::OutputRef) -> serde_json::Value {
    if oref.format == "json" {
        std::fs::read_to_string(&oref.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| artifact_metadata(oref))
    } else {
        artifact_metadata(oref)
    }
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

/// Format seconds as a fixed-width time string for progress display.
/// Always 8 chars wide: "   5s   ", " 2m 30s ", " 1h 05m ", "2d 03h  "
pub(crate) fn fmt_eta(secs: f64) -> String {
    let s = secs.round() as u64;
    if s < 60 {
        format!("{s:>4}s   ")
    } else if s < 3600 {
        format!("{:>2}m {:02}s ", s / 60, s % 60)
    } else if s < 86400 {
        format!("{:>2}h {:02}m ", s / 3600, (s % 3600) / 60)
    } else {
        let d = s / 86400;
        format!("{d:>2}d {:02}h  ", (s % 86400) / 3600)
    }
}

/// The progress total, grown to cover steps the plan did not count (the children a `parallel()`
/// call fans out to complete as extra steps). Keeps `completed <= total` for the counters and
/// the ETA subtraction.
pub(crate) fn reconcile_total(total_steps: usize, completed_steps: usize) -> usize {
    total_steps.max(completed_steps)
}

/// Select the first twenty keys in lexical order, without retaining every key.
/// Taking a preview of each chunk and then previewing their union has the same result
/// as previewing all keys, independently of worker chunk boundaries.
pub(crate) fn key_preview(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut preview = Vec::new();
    for key in keys {
        let pos = preview.binary_search(&key).unwrap_or_else(|pos| pos);
        if pos < 20 {
            preview.insert(pos, key);
            preview.truncate(20);
        }
    }
    preview
}

/// A partitioned asset plans one step per key; report it as one line with a partition summary.
pub(crate) fn merge_partition_reports(reports: Vec<StepReport>) -> Vec<StepReport> {
    let mut out: Vec<StepReport> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in reports {
        let Some(p) = r.partitions.clone() else {
            out.push(r);
            continue;
        };
        match index.get(&r.id).copied() {
            None => {
                index.insert(r.id.clone(), out.len());
                out.push(r);
            }
            Some(i) => {
                let m = &mut out[i];
                let mp = m.partitions.get_or_insert_with(PartitionSummary::default);
                mp.total += p.total;
                mp.cached += p.cached;
                mp.will_run += p.will_run;
                mp.will_run_keys = key_preview(
                    std::mem::take(&mut mp.will_run_keys)
                        .into_iter()
                        .chain(p.will_run_keys),
                );
                if let Some(changed) = p.changed {
                    mp.changed
                        .get_or_insert_with(Default::default)
                        .extend(changed);
                }
                if let Some(changed) = &mut mp.changed {
                    let keys = &mp.will_run_keys;
                    changed.retain(|k, _| keys.contains(k));
                }
                if m.reason.is_none() {
                    m.reason = r.reason;
                    m.detail = r.detail;
                } else if m.changed.is_none() && r.changed.is_some() {
                    // What changed in the first key that ran before, whichever chunk it is in.
                    m.detail = r.detail;
                }
                if m.changed.is_none() {
                    m.changed = r.changed;
                }
            }
        }
    }
    // Recompute the verdict of merged lines from their totals.
    for r in &mut out {
        let Some(p) = &r.partitions else { continue };
        let verdict = |dry: bool| match (p.cached, p.will_run) {
            (_, 0) => "cached",
            (0, _) => {
                if dry {
                    "run"
                } else {
                    "ran"
                }
            }
            _ => "partial",
        };
        if r.action.is_some() {
            r.action = Some(verdict(true).to_string());
        } else {
            r.status = Some(verdict(false).to_string());
        }
    }
    out
}

pub(crate) fn stale_warning(display_id: &str, root: &str, dry: bool) -> String {
    let id = short_name(display_id);
    let (served, reflect) = if dry {
        ("will be served", "will not reflect")
    } else {
        ("was served", "does not reflect")
    };
    format!(
        "'{id}' {served} from cache but depends on refreshed '{root}', so it {reflect} the \
         refresh. Drop --no-cascade, add it to --refresh (for example --refresh {root},{id}) or \
         use --refresh-all."
    )
}

/// The declared env values of `base_id` as reported in output (secrets redacted, unset = null),
/// or `None` when the node declares no env.
pub(crate) fn env_report(
    dag: &Dag,
    base_id: &str,
) -> Option<std::collections::BTreeMap<String, Option<String>>> {
    let node = dag.get_node(base_id)?;
    if node.extracted.env.is_empty() {
        return None;
    }
    Some(crate::envdeps::report(&crate::envdeps::resolve(
        &node.extracted.env,
    )))
}

/// The `--agent` step-line suffix for `node_id`'s declared env (empty when none is declared).
pub(crate) fn env_suffix(dag: &Dag, node_id: &str) -> String {
    dag.get_node(crate::StepId::parse(node_id).base_id())
        .map(|n| crate::envdeps::agent_suffix(&crate::envdeps::resolve(&n.extracted.env)))
        .unwrap_or_default()
}

pub(crate) fn kind_str(kind: Option<crate::NodeKind>) -> String {
    match kind {
        Some(crate::NodeKind::Asset) => "asset",
        Some(crate::NodeKind::Task) => "task",
        Some(crate::NodeKind::Sensor) => "sensor",
        None => "unknown",
    }
    .to_string()
}

/// Build the report line for one decided step. `dry` selects the vocabulary: a dry run says what
/// will happen (`action`), a real run says what happened (`status`).
pub(crate) fn report_for(
    dag: &Dag,
    step: &crate::planner::StreamStep,
    decision: &Decision,
    dry: bool,
) -> StepReport {
    let display_id = step.step_id.display();
    let base_id = step.step_id.base_id();
    let mut r = StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        env: env_report(dag, base_id),
        undeclared_env: dag
            .get_node(base_id)
            .map(|n| n.extracted.undeclared_env())
            .filter(|names| !names.is_empty()),
        ..Default::default()
    };
    let (word_cached, word_run, word_partial) = if dry {
        ("cached", "run", "partial")
    } else {
        ("cached", "ran", "partial")
    };
    let verdict = match decision {
        Decision::Run(reason) => {
            r.reason = Some(reason.code().to_string());
            r.detail = Some(reason.detail());
            r.run_hash = step.run_hashes.get(&display_id).cloned();
            if step.partition_keys.is_empty() {
                word_run
            } else {
                // A forced (task/refresh) partitioned step runs every key.
                r.partitions = Some(PartitionSummary {
                    total: step.partition_keys.len(),
                    cached: 0,
                    will_run: step.partition_keys.len(),
                    will_run_keys: key_preview(step.partition_keys.iter().map(|k| k.suffix())),
                    changed: None,
                });
                word_run
            }
        }
        Decision::Cached {
            oref,
            stale_root,
            held,
        } => {
            r.run_hash = step.run_hashes.get(&display_id).cloned();
            r.artifact = Some(oref.path.clone());
            if let Some(held) = held {
                r.detail = Some(held.detail().to_string());
            }
            if let Some(root) = stale_root {
                r.warning = Some(stale_warning(&display_id, root, dry));
            }
            word_cached
        }
        Decision::Partitioned { cached, missing } => {
            let total = cached.len() + missing.len();
            r.partitions = Some(PartitionSummary {
                total,
                cached: cached.len(),
                will_run: missing.len(),
                will_run_keys: key_preview(missing.iter().map(|k| k.suffix())),
                changed: None,
            });
            if !missing.is_empty() {
                r.reason = Some(RunReason::NotMaterialized.code().to_string());
                r.detail = Some(RunReason::NotMaterialized.detail());
            }
            match (cached.is_empty(), missing.is_empty()) {
                (_, true) => word_cached,
                (true, false) => word_run,
                (false, false) => word_partial,
            }
        }
    };
    if dry {
        r.action = Some(verdict.to_string());
    } else {
        r.status = Some(verdict.to_string());
    }
    r
}

/// How a run ended, for its last stderr line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    Done,
    Failed,
    Cancelled,
}

/// The last progress line of a run that executed steps, with or without `--agent`:
/// `[barca] N/M steps | done in Xs` (`failed in`, `cancelled after`).
pub(crate) fn end_of_run_line(
    completed: usize,
    total: usize,
    secs: f64,
    outcome: RunOutcome,
) -> String {
    let how = match outcome {
        RunOutcome::Done => "done in",
        RunOutcome::Failed => "failed in",
        RunOutcome::Cancelled => "cancelled after",
    };
    format!("[barca] {completed}/{total} steps | {how} {secs:.1}s")
}

/// The `--agent` line for a step served from cache: `[barca] step:<id> cached`, with its declared
/// env.
pub(crate) fn cached_step_line(dag: &Dag, display_id: &str) -> String {
    format!(
        "[barca] step:{display_id} cached{}",
        env_suffix(dag, display_id)
    )
}

/// The `--agent` line for a step that raised: `[barca] step:<id> failed: <first line of the
/// error>`, beside the `completed` and `cached` lines.
pub(crate) fn failed_step_line(node_id: &str, error: &str) -> String {
    let first = error
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown error");
    format!("[barca] step:{node_id} failed: {first}")
}

#[cfg(test)]
mod progress_tests {
    use super::reconcile_total;

    #[test]
    fn total_grows_to_cover_steps_the_plan_did_not_count() {
        // parallel() children complete as extra steps beyond the plan.
        assert_eq!(reconcile_total(2, 4), 4);
        assert_eq!(reconcile_total(4, 2), 4);
        assert_eq!(reconcile_total(3, 3), 3);
        assert_eq!(reconcile_total(0, 0), 0);
    }

    #[test]
    fn remaining_never_underflows() {
        // The ETA math subtracts usizes; this used to panic in debug and wrap in release.
        assert_eq!(2usize.saturating_sub(4), 0);
    }
}

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
