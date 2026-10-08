//! Pure command rendering. Output chunks preserve stdout/stderr ordering.
use super::*;

/// Result representation requested by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultFormat {
    Json,
    Value,
    Pretty,
}

/// One output write, in the order it should be emitted.
#[derive(Debug, PartialEq, Eq)]
pub enum OutputChunk {
    Stdout(String),
    Stderr(String),
}

/// Complete command presentation, without writing to process streams.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RenderedOutput {
    pub chunks: Vec<OutputChunk>,
}
impl RenderedOutput {
    fn stdout(&mut self, text: String) {
        self.chunks.push(OutputChunk::Stdout(text));
    }
    fn stdout_line(&mut self, mut text: String) {
        text.push('\n');
        self.stdout(text);
    }
    fn stderr_line(&mut self, mut text: String) {
        text.push('\n');
        self.chunks.push(OutputChunk::Stderr(text));
    }
}
macro_rules! line { ($out:expr, $($args:tt)*) => { $out.stdout_line(format!($($args)*)) }; }
macro_rules! text { ($out:expr, $($args:tt)*) => { $out.stdout(format!($($args)*)) }; }
macro_rules! note { ($out:expr, $($args:tt)*) => { $out.stderr_line(format!($($args)*)) }; }
fn root_field(out: &mut Value, root: Option<&str>) {
    if let (Value::Object(obj), Some(root)) = (out, root) {
        obj.insert("root".into(), root.into());
    }
}
pub fn render_get(
    result: &GetResult,
    target: Option<&str>,
    final_output: Option<Value>,
    mode: ResultFormat,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();

    match mode {
        ResultFormat::Json => {
            let mut out = serde_json::json!({
                "status": "success",
                "run_id": result.run_id,
                "elapsed_seconds": result.elapsed_seconds,
                "steps_executed": result.steps_executed,
                "phases": result.phases,
                "final_output": final_output,
                "steps": &result.steps,
                "warnings": &result.warnings,
            });
            project_key(&mut out, "steps", fields);
            line!(rendered, "{out}");
        }
        ResultFormat::Value => {
            if let Some(ref val) = final_output {
                line!(rendered, "{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
        ResultFormat::Pretty => {
            let label = target
                .as_ref()
                .map(|t| format!("got '{t}'"))
                .unwrap_or_else(|| "all assets".to_string());
            line!(
                rendered,
                "Run {} | {} in {:.3}s ({} step{}, {} phase{})",
                result.run_id,
                label,
                result.elapsed_seconds,
                result.steps_executed,
                if result.steps_executed == 1 { "" } else { "s" },
                result.phases,
                if result.phases == 1 { "" } else { "s" }
            );
            if let Some(ref val) = final_output {
                line!(
                    rendered,
                    "\nValue:\n{}",
                    serde_json::to_string_pretty(val).unwrap()
                );
            }
        }
    }
    rendered
}

pub fn render_run(
    result: &GetResult,
    target: &str,
    final_output: Option<Value>,
    mode: ResultFormat,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();

    match mode {
        ResultFormat::Json => {
            let mut out = serde_json::json!({
                "status": "success",
                "run_id": result.run_id,
                "elapsed_seconds": result.elapsed_seconds,
                "steps_executed": result.steps_executed,
                "phases": result.phases,
                "final_output": final_output,
                "steps": &result.steps,
                "warnings": &result.warnings,
            });
            project_key(&mut out, "steps", fields);
            line!(rendered, "{out}");
        }
        ResultFormat::Value => {
            if let Some(ref val) = final_output {
                line!(rendered, "{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
        ResultFormat::Pretty => {
            line!(
                rendered,
                "Run {} | ran '{}' in {:.3}s ({} step{}, {} phase{})",
                result.run_id,
                target,
                result.elapsed_seconds,
                result.steps_executed,
                if result.steps_executed == 1 { "" } else { "s" },
                result.phases,
                if result.phases == 1 { "" } else { "s" }
            );
            if let Some(ref val) = final_output {
                line!(
                    rendered,
                    "\nValue:\n{}",
                    serde_json::to_string_pretty(val).unwrap()
                );
            }
        }
    }
    rendered
}

pub fn render_history(
    runs: &[crate::db::RunRecord],
    total: usize,
    json: bool,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    let page = Page::new(runs.len(), total);
    if json || fields.is_some() {
        let mut items: Vec<serde_json::Value> = runs
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Null))
            .collect();
        if let Some(f) = fields {
            project(&mut items, f);
        }
        let out = page.envelope("runs", items, "runs");
        line!(rendered, "{}", serde_json::to_string_pretty(&out).unwrap());
        return rendered;
    }
    if runs.is_empty() {
        match page.note(0, "runs") {
            Some(note) => note!(rendered, "{note}"),
            None => line!(rendered, "No run history found."),
        }
        return rendered;
    }
    // Table header.
    line!(
        rendered,
        "{:<14} {:<7} {:<11} {:>5} {:>6} {:>6} {:<20}",
        "RUN_ID",
        "CMD",
        "STATUS",
        "STEPS",
        "CACHED",
        "TIME",
        "STARTED"
    );
    line!(rendered, "{}", "-".repeat(77));
    for r in runs {
        let elapsed_str = r
            .elapsed_seconds
            .map(|e| format!("{:.1}s", e))
            .unwrap_or_else(|| "-".to_string());
        line!(
            rendered,
            "{:<14} {:<7} {:<11} {:>5} {:>6} {:>6} {:<20}",
            r.run_id,
            r.command,
            r.status,
            r.steps_executed,
            r.steps_cached,
            elapsed_str,
            r.started_at,
        );
    }
    if let Some(note) = page.note(runs.len(), "runs") {
        note!(rendered, "{note}");
    }
    rendered
}

pub fn render_stats(
    stats: &crate::db::AssetStats,
    json: bool,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if json || fields.is_some() {
        let mut out = stats_json(stats);
        project_key(&mut out, "recent_runs", fields);
        line!(rendered, "{}", serde_json::to_string_pretty(&out).unwrap());
        return rendered;
    }
    let fmt = |v: Option<f64>| v.map(|e| format!("{:.3}s", e)).unwrap_or("-".to_string());
    line!(rendered, "Asset: {}", stats.node_id);
    line!(rendered, "Total materializations: {}", stats.total_runs);
    line!(
        rendered,
        "Timing:  avg {}  median {}  p95 {}  max {}",
        fmt(stats.avg_elapsed_seconds),
        fmt(stats.median_elapsed_seconds),
        fmt(stats.p95_elapsed_seconds),
        fmt(stats.max_elapsed_seconds),
    );
    line!(
        rendered,
        "Cache hit rate: {:.1}%",
        stats.cache_hit_rate * 100.0
    );
    if !stats.recent_runs.is_empty() {
        line!(rendered, "\nRecent runs:");
        line!(
            rendered,
            "  {:<10} {:<9} {:<8} {:<20}",
            "ELAPSED",
            "STATUS",
            "ATTEMPTS",
            "CREATED"
        );
        for entry in &stats.recent_runs {
            let elapsed_str = entry
                .elapsed_seconds
                .map(|e| format!("{:.3}s", e))
                .unwrap_or_else(|| "-".to_string());
            line!(
                rendered,
                "  {:<10} {:<9} {:<8} {:<20}",
                elapsed_str,
                entry.status,
                entry.attempts,
                entry.created_at,
            );
            if entry.status == "failed"
                && let Some(msg) = &entry.error_message
                && !msg.is_empty()
            {
                line!(rendered, "      └─ {msg}");
            }
        }
    }
    rendered
}

pub fn render_sql(result: crate::sql::SqlResult, json: bool) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    for note in &result.notes {
        note!(rendered, "barca: {note}");
    }
    let page = Page::new(result.rows.len(), result.total as usize);
    if json {
        let mut out = page.envelope("rows", result.rows, "rows");
        if let serde_json::Value::Object(obj) = &mut out {
            obj.insert(
                "columns".into(),
                serde_json::Value::Array(
                    result
                        .columns
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        line!(rendered, "{}", serde_json::to_string_pretty(&out).unwrap());
        return rendered;
    }
    let cell = |v: &serde_json::Value| match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let grid: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|r| result.columns.iter().map(|c| cell(&r[c])).collect())
        .collect();
    let widths: Vec<usize> = result
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            grid.iter()
                .map(|r| r[i].chars().count())
                .chain([c.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    if !result.columns.is_empty() {
        line!(rendered, "{}", line(&result.columns));
    }
    for row in &grid {
        line!(rendered, "{}", line(row));
    }
    if let Some(note) = page.note(grid.len(), "rows") {
        note!(rendered, "{note}");
    }
    rendered
}

pub fn render_status(
    mut result: crate::status::StatusResult,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
    root: Option<&str>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    // Bounded like `list`: the summary still counts every node; `nodes` is cut to the limit.
    let total = result.nodes.len();
    if let Some(limit) = limit {
        result.nodes.truncate(limit);
    }
    let page = Page::new(result.nodes.len(), total);
    if json {
        let mut out = serde_json::to_value(&result).unwrap();
        project_key(&mut out, "nodes", fields);
        if let serde_json::Value::Object(obj) = &mut out
            && let serde_json::Value::Object(env) = page.envelope("nodes", vec![], "nodes")
        {
            // Add `total`, `truncated` and (when truncated) `hint` beside `nodes`.
            for (k, v) in env {
                if k != "nodes" {
                    obj.insert(k, v);
                }
            }
        }
        root_field(&mut out, root);
        line!(rendered, "{}", serde_json::to_string_pretty(&out).unwrap());
        return rendered;
    }
    text!(rendered, "{}", render_status_table(&result));
    if let Some(note) = page.note(result.nodes.len(), "nodes") {
        note!(rendered, "{note}");
    }
    rendered
}

pub fn render_list(
    assets: &[AssetSummary],
    total: usize,
    next_fires: &HashMap<String, String>,
    json: bool,
    fields: Option<&[String]>,
    root: Option<&str>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    let page = Page::new(assets.len(), total);
    if json || fields.is_some() {
        let mut nodes: Vec<serde_json::Value> = assets
            .iter()
            .map(|a| list_node_json(a, next_fires.get(&a.id)))
            .collect();
        if let Some(f) = fields {
            project(&mut nodes, f);
        }
        let mut out = page.envelope("nodes", nodes, "nodes");
        root_field(&mut out, root);
        line!(rendered, "{}", serde_json::to_string_pretty(&out).unwrap());
        return rendered;
    }
    if assets.is_empty() {
        match page.note(0, "nodes") {
            Some(note) => note!(rendered, "{note}"),
            None => line!(rendered, "No definitions found."),
        }
        return rendered;
    }
    let has_schedule = !next_fires.is_empty();
    // Like NEXT FIRE, the ENV column only appears when some node declares env.
    let has_env = assets.iter().any(|a| !a.env.is_empty());

    // Render each row's cells up front so column widths fit the actual content.
    let mut header = vec!["NAME", "KIND", "FRESHNESS"];
    if has_schedule {
        header.push("NEXT FIRE");
    }
    header.push("DEPS");
    if has_env {
        header.push("ENV");
    }
    let list_or_dash = |v: &[String]| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.join(", ")
        }
    };
    let rows: Vec<Vec<String>> = assets
        .iter()
        .map(|a| {
            let kind = serde_json::to_value(a.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{:?}", a.kind).to_lowercase());
            let freshness = match &a.freshness {
                crate::Freshness::Schedule(cron) => format!("cron: {}", cron.0),
                f => freshness_str(f).to_string(),
            };
            let mut row = vec![a.id.clone(), kind, freshness];
            if has_schedule {
                row.push(next_fires.get(&a.id).cloned().unwrap_or_else(|| "-".into()));
            }
            row.push(list_or_dash(&a.inputs));
            if has_env {
                row.push(list_or_dash(&a.env));
            }
            row
        })
        .collect();

    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .map(|r| r[i].len())
                .chain(std::iter::once(header[i].len()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    // Every column but the last is padded; the last runs to the end of the line.
    let last = header.len() - 1;
    let render = |cells: &[&str]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == last {
                    c.to_string()
                } else {
                    format!("{c:<w$}", w = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    line!(rendered, "{}", render(&header));
    line!(
        rendered,
        "{}",
        "-".repeat(widths[..last].iter().sum::<usize>() + 2 * last + 4)
    );
    for row in &rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        line!(rendered, "{}", render(&cells));
    }
    if let Some(note) = page.note(rows.len(), "nodes") {
        note!(rendered, "{note}");
    }
    rendered
}

pub fn render_explain(
    result: &ExplainResult,
    label: &str,
    mode: ResultFormat,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    match mode {
        ResultFormat::Json => {
            let mut out = serde_json::to_value(result).unwrap();
            project_key(&mut out, "steps", fields);
            // `targets` (several targets) is keyed in the order given, like a real run; the
            // other keys sort before it.
            match out.as_object_mut().and_then(|o| o.remove("targets")) {
                Some(_) => {
                    let per_target: Vec<(String, serde_json::Value)> = result
                        .targets
                        .iter()
                        .map(|(n, p)| (n.clone(), serde_json::to_value(p).unwrap()))
                        .collect();
                    let rest = out.to_string();
                    line!(
                        rendered,
                        "{},\"targets\":{}}}",
                        &rest[..rest.len() - 1],
                        ordered_object(&per_target)
                    );
                }
                None => line!(rendered, "{out}"),
            }
        }
        ResultFormat::Value => line!(
            rendered,
            "{}",
            serde_json::to_string_pretty(&result.steps).unwrap()
        ),
        ResultFormat::Pretty => {
            line!(
                rendered,
                "Dry run: barca {label}{} (nothing executed, nothing written)\n",
                if result.targets.len() > 1 {
                    format!(" {}", result.target_names().join(","))
                } else {
                    result
                        .target
                        .as_deref()
                        .map(|t| format!(" {t}"))
                        .unwrap_or_default()
                }
            );
            text!(rendered, "{}", render_step_table(&result.steps, true));
            line!(
                rendered,
                "\n{} will run, {} cached, {} unknown",
                result.summary.will_run,
                result.summary.cached,
                result.summary.unknown
            );
        }
    }
    rendered
}

pub fn first_multi_failure(result: &MultiResult) -> Option<crate::BarcaError> {
    // The first failed target becomes the run's error: exit 1 with the error envelope.
    if let Some((name, t)) = result.targets.iter().find(|(_, t)| t.status != "success") {
        return Some(crate::BarcaError::WorkerFailed(Box::new(
            crate::FailedStep {
                node: t.failed_node.clone().unwrap_or_else(|| name.clone()),
                message: t.error.clone().unwrap_or_else(|| "unknown error".into()),
                artifact_dir: None,
                run: None,
            },
        )));
    }
    None
}

/// Render values supplied in the same order as `result.targets`; absent values are null.
pub fn render_multi(
    result: &MultiResult,
    final_outputs: &[Option<Value>],
    mode: ResultFormat,
    verb: &str,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    let per_target: Vec<(String, serde_json::Value)> = result
        .targets
        .iter()
        .enumerate()
        .map(|(index, (name, t))| {
            let mut obj = serde_json::Map::new();
            obj.insert("status".into(), t.status.clone().into());
            if t.status == "success" {
                let value = final_outputs
                    .get(index)
                    .cloned()
                    .flatten()
                    .unwrap_or(Value::Null);
                obj.insert("final_output".into(), value);
            }
            if let Some(step) = &t.failed_node {
                obj.insert("failed_node".into(), step.clone().into());
            }
            if let Some(err) = &t.error {
                obj.insert("error".into(), err.clone().into());
            }
            (name.clone(), serde_json::Value::Object(obj))
        })
        .collect();

    match mode {
        ResultFormat::Json => {
            // Keys of the run object sort before `targets`, which keeps the given order.
            let mut run = serde_json::json!({
                "status": if result.any_failed() { "failed" } else { "success" },
                "run_id": result.run_id,
                "elapsed_seconds": result.elapsed_seconds,
                "steps_executed": result.steps_executed,
                "phases": result.phases,
                "steps": &result.steps,
                "warnings": &result.warnings,
            });
            project_key(&mut run, "steps", fields);
            let run = run.to_string();
            line!(
                rendered,
                "{},\"targets\":{}}}",
                &run[..run.len() - 1],
                ordered_object(&per_target)
            );
        }
        ResultFormat::Value => {
            // Each target's value (null when it failed), keyed by target name.
            let values: serde_json::Map<String, serde_json::Value> = per_target
                .iter()
                .map(|(name, t)| {
                    let v = t.get("final_output").cloned().unwrap_or_default();
                    (name.clone(), v)
                })
                .collect();
            line!(
                rendered,
                "{}",
                serde_json::to_string_pretty(&values).unwrap()
            );
        }
        ResultFormat::Pretty => {
            let failed = result.targets.iter().filter(|(_, t)| t.status != "success");
            line!(
                rendered,
                "Run {} | {verb} {} targets in {:.3}s ({} step{}, {} phase{}, {} failed)",
                result.run_id,
                result.targets.len(),
                result.elapsed_seconds,
                result.steps_executed,
                if result.steps_executed == 1 { "" } else { "s" },
                result.phases,
                if result.phases == 1 { "" } else { "s" },
                failed.count(),
            );
            for (name, t) in &per_target {
                let status = t["status"].as_str().unwrap_or("?");
                line!(rendered, "\n{name}: {status}");
                match t.get("final_output") {
                    Some(v) => line!(rendered, "{}", serde_json::to_string_pretty(v).unwrap()),
                    None => line!(
                        rendered,
                        "  failed at {}",
                        t["failed_node"].as_str().unwrap_or("(did not run)")
                    ),
                }
            }
        }
    }

    if result.any_failed() {
        for (name, t) in &result.targets {
            if t.status == "success" {
                continue;
            }
            let at = t
                .failed_node
                .as_deref()
                .map(|s| format!(" (failed step: {s})"))
                .unwrap_or_default();
            note!(
                rendered,
                "error: target '{name}' failed{at}: {}",
                t.error.as_deref().unwrap_or("unknown error")
            );
        }
    }
    rendered
}

pub fn render_failed_run(err: &crate::BarcaError, mode: ResultFormat) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    let (crate::BarcaError::WorkerFailed(f), ResultFormat::Json) = (err, mode) else {
        return rendered;
    };
    let Some(run) = &f.run else {
        return rendered;
    };
    line!(
        rendered,
        "{}",
        serde_json::json!({
            "status": "failed",
            "run_id": run.run_id,
            "elapsed_seconds": run.elapsed_seconds,
            "steps_executed": run.steps_executed,
            "phases": run.phases,
            "failed_node": f.node,
            "error": f.summary(),
            "steps": &run.steps,
            "warnings": &run.warnings,
        })
    );
    rendered
}

pub fn render_plan(result: &PlanResult) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    line!(
        rendered,
        "{}",
        serde_json::to_string_pretty(result).unwrap()
    );
    rendered
}
