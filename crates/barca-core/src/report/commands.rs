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
/// `uploads` on a run's JSON, when something is left unpublished (#390).
fn uploads_field(out: &mut Value, uploads: Option<&crate::results::UploadsReport>) {
    if let (Value::Object(obj), Some(uploads)) = (out, uploads) {
        obj.insert(
            "uploads".into(),
            serde_json::to_value(uploads).unwrap_or(Value::Null),
        );
    }
}
/// The stdout JSON body of a successful single-target `barca get --json` / `barca run --json`,
/// before `--fields`. A served run's `outcome` is this body.
pub fn run_body(result: &GetResult, final_output: Option<Value>) -> Value {
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
    if let (Value::Object(obj), Some(cache)) = (&mut out, &result.cache) {
        obj.insert("cache".into(), serde_json::json!(cache));
    }
    uploads_field(&mut out, result.uploads.as_ref());
    out
}

/// What the human summary of a single-target run says it did: `got 'x'`, `all assets` or
/// `ran 'x'`.
pub fn run_label(command: &str, target: Option<&str>) -> String {
    match (command, target) {
        ("run", Some(t)) => format!("ran '{t}'"),
        ("run", None) => "ran ''".to_string(),
        (_, Some(t)) => format!("got '{t}'"),
        (_, None) => "all assets".to_string(),
    }
}

/// Render a successful single-target run from its JSON body ([`run_body`]). The one renderer
/// for a local run and for a served run's `outcome`. `final_output` is the value to show, or
/// `None` when the run has none (a `get` of every asset); the body says `null` for both that
/// and a target whose value is `None`.
pub fn render_run_body(
    body: &Value,
    label: &str,
    final_output: Option<&Value>,
    mode: ResultFormat,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    match mode {
        ResultFormat::Json => {
            let mut out = body.clone();
            project_key(&mut out, "steps", fields);
            line!(rendered, "{out}");
        }
        ResultFormat::Value => {
            if let Some(val) = final_output {
                line!(rendered, "{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
        ResultFormat::Pretty => {
            let count = |key: &str| body[key].as_u64().unwrap_or(0);
            let (steps, phases) = (count("steps_executed"), count("phases"));
            line!(
                rendered,
                "Run {} | {} in {:.3}s ({} step{}, {} phase{})",
                body["run_id"].as_str().unwrap_or_default(),
                label,
                body["elapsed_seconds"].as_f64().unwrap_or(0.0),
                steps,
                if steps == 1 { "" } else { "s" },
                phases,
                if phases == 1 { "" } else { "s" }
            );
            if let Some(val) = final_output {
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

/// How `barca get` prints its result on stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GetShape {
    /// One JSON line: the run, with `final_output` (or `targets`).
    Json,
    /// For people: a summary line, then the returned paths (or the value).
    Pretty,
    /// Only what was returned: one path per line (`-o path`) or the value (`-o value`).
    Bare,
}

/// One returned artifact, ready to print.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The partition key (`dim=value,...`) when the returned node is partitioned.
    pub key: Option<String>,
    pub path: String,
    /// A small json value shown under its path in a terminal.
    pub preview: Option<String>,
}

/// What one target returned, prepared by the caller in the form it asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Returned {
    /// `final_output`: the artifact pointer or the value, or for a partitioned target a mapping
    /// of key to either. `None` when nothing was returned.
    pub final_output: Option<Value>,
    /// Whether `final_output` holds values (`-o value`) rather than pointers.
    pub values: bool,
    /// The returned artifacts, one per key.
    pub shown: Vec<Shown>,
}

fn shown_lines(rendered: &mut RenderedOutput, shown: &[Shown], shape: GetShape) {
    for s in shown {
        match (shape, &s.key) {
            (GetShape::Pretty, Some(key)) => line!(rendered, "{key}: {}", s.path),
            _ => line!(rendered, "{}", s.path),
        }
        if shape == GetShape::Pretty
            && let Some(preview) = &s.preview
        {
            for l in preview.lines() {
                line!(rendered, "  {l}");
            }
        }
    }
}

/// `barca get` with one target (or none): paths by default, values under `-o value`.
pub fn render_get(
    result: &GetResult,
    target: Option<&str>,
    returned: &Returned,
    shape: GetShape,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let body = run_body(result, returned.final_output.clone());
    render_get_body(&body, target, returned, shape, fields)
}

/// Render a successful single-target `get` from its JSON body ([`run_body`]): the one renderer
/// for a local run and for a served run's `outcome`.
pub fn render_get_body(
    body: &Value,
    target: Option<&str>,
    returned: &Returned,
    shape: GetShape,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    match shape {
        GetShape::Json => {
            let mut out = body.clone();
            project_key(&mut out, "steps", fields);
            line!(rendered, "{out}");
        }
        GetShape::Bare if returned.values => {
            if let Some(val) = &returned.final_output {
                line!(rendered, "{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
        GetShape::Bare => shown_lines(&mut rendered, &returned.shown, shape),
        GetShape::Pretty => {
            let count = |key: &str| body[key].as_u64().unwrap_or(0);
            let (steps, phases) = (count("steps_executed"), count("phases"));
            line!(
                rendered,
                "Run {} | {} in {:.3}s ({} step{}, {} phase{})",
                body["run_id"].as_str().unwrap_or_default(),
                run_label("get", target),
                body["elapsed_seconds"].as_f64().unwrap_or(0.0),
                steps,
                if steps == 1 { "" } else { "s" },
                phases,
                if phases == 1 { "" } else { "s" }
            );
            if returned.values {
                if let Some(val) = &returned.final_output {
                    line!(
                        rendered,
                        "\nValue:\n{}",
                        serde_json::to_string_pretty(val).unwrap()
                    );
                }
            } else if !returned.shown.is_empty() {
                line!(rendered, "");
                shown_lines(&mut rendered, &returned.shown, shape);
            }
        }
    }
    rendered
}

/// What a `get` returned, read back from the pointers in its JSON `final_output` (an artifact
/// pointer, or an object of them keyed by partition key): the paths to print.
pub fn returned_from_pointers(final_output: Option<&Value>) -> Returned {
    let path = |v: &Value| v["_barca_artifact"]["path"].as_str().map(str::to_string);
    let shown = match final_output {
        Some(Value::Object(map)) if !map.contains_key("_barca_artifact") => map
            .iter()
            .filter_map(|(key, v)| {
                path(v).map(|path| Shown {
                    key: Some(key.clone()),
                    path,
                    preview: None,
                })
            })
            .collect(),
        Some(v) => path(v)
            .map(|path| Shown {
                key: None,
                path,
                preview: None,
            })
            .into_iter()
            .collect(),
        None => Vec::new(),
    };
    Returned {
        final_output: final_output.filter(|v| !v.is_null()).cloned(),
        values: false,
        shown,
    }
}

/// `barca get a,b`: [`render_get`] per target. JSON carries `targets`, keyed by target in the
/// order given: `{status: success, final_output}` or `{status: failed, failed_node, error}`,
/// where a failed partitioned target also has `final_output` with the keys that succeeded.
pub fn render_get_multi(
    result: &MultiResult,
    returned: &[Returned],
    shape: GetShape,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let empty = Returned::default();
    let returned: Vec<&Returned> = (0..result.targets.len())
        .map(|i| returned.get(i).unwrap_or(&empty))
        .collect();
    let finals: Vec<Option<Value>> = returned.iter().map(|r| r.final_output.clone()).collect();
    let (run, per_target) = multi_parts(result, &finals);
    render_get_multi_parts(&run, &per_target, &returned, shape, fields)
}

/// Render a multi-target `get` from its two JSON parts ([`multi_parts`]) and what each target
/// returned, in the same order: the one renderer for a local run and a served run's `outcome`.
pub fn render_get_multi_parts(
    run: &Value,
    per_target: &[(String, Value)],
    returned: &[&Returned],
    shape: GetShape,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    let empty = Returned::default();
    let each = |i: usize| returned.get(i).copied().unwrap_or(&empty);
    let status = |t: &Value| t["status"].as_str().unwrap_or("?").to_string();
    match shape {
        GetShape::Json => {
            let mut run = run.clone();
            project_key(&mut run, "steps", fields);
            line!(rendered, "{}", multi_json(&run, per_target));
        }
        GetShape::Bare if returned.iter().any(|r| r.values) => {
            // Each target's value (null when it failed), keyed by target name.
            let values: serde_json::Map<String, Value> = per_target
                .iter()
                .enumerate()
                .map(|(i, (name, _))| {
                    (
                        name.clone(),
                        each(i).final_output.clone().unwrap_or_default(),
                    )
                })
                .collect();
            line!(
                rendered,
                "{}",
                serde_json::to_string_pretty(&values).unwrap()
            );
        }
        GetShape::Bare => {
            for i in 0..per_target.len() {
                shown_lines(&mut rendered, &each(i).shown, shape);
            }
        }
        GetShape::Pretty => {
            let failed = per_target
                .iter()
                .filter(|(_, t)| t["status"].as_str() != Some("success"));
            let count = |key: &str| run[key].as_u64().unwrap_or(0);
            let (steps, phases) = (count("steps_executed"), count("phases"));
            line!(
                rendered,
                "Run {} | got {} targets in {:.3}s ({} step{}, {} phase{}, {} failed)",
                run["run_id"].as_str().unwrap_or_default(),
                per_target.len(),
                run["elapsed_seconds"].as_f64().unwrap_or(0.0),
                steps,
                if steps == 1 { "" } else { "s" },
                phases,
                if phases == 1 { "" } else { "s" },
                failed.count(),
            );
            for (i, (name, t)) in per_target.iter().enumerate() {
                let r = each(i);
                let status = status(t);
                if status != "success" && r.shown.is_empty() {
                    continue;
                }
                line!(rendered, "\n{name}: {status}");
                if r.values {
                    if let Some(v) = &r.final_output {
                        line!(rendered, "{}", serde_json::to_string_pretty(v).unwrap());
                    }
                } else {
                    shown_lines(&mut rendered, &r.shown, shape);
                }
            }
        }
    }
    rendered
}

/// A single-target `barca get` whose step failed: the JSON result line (as
/// [`render_failed_run`]), with `final_output` holding a partitioned target's keys that
/// succeeded; outside JSON, those keys' paths (or values).
pub fn render_failed_get(
    err: &crate::BarcaError,
    returned: Option<&Returned>,
    shape: GetShape,
) -> RenderedOutput {
    let crate::BarcaError::WorkerFailed(f) = err else {
        return RenderedOutput::default();
    };
    let Some(run) = &f.run else {
        return RenderedOutput::default();
    };
    let mut rendered = RenderedOutput::default();
    match shape {
        GetShape::Json => {
            let mut out = serde_json::json!({
                "status": "failed",
                "run_id": run.run_id,
                "elapsed_seconds": run.elapsed_seconds,
                "steps_executed": run.steps_executed,
                "phases": run.phases,
                "failed_node": f.node,
                "error": f.summary(),
                "steps": &run.steps,
                "warnings": &run.warnings,
            });
            if let (Some(obj), Some(final_output)) = (
                out.as_object_mut(),
                returned.and_then(|r| r.final_output.clone()),
            ) {
                obj.insert("final_output".into(), final_output);
            }
            line!(rendered, "{out}");
        }
        _ => {
            if let Some(r) = returned {
                rendered = render_failed_get_returned(r, shape);
            }
        }
    }
    rendered
}

/// What a failed `get` prints outside JSON: the paths (or values) of the keys that succeeded.
pub fn render_failed_get_returned(r: &Returned, shape: GetShape) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if r.values {
        if let Some(v) = &r.final_output {
            line!(rendered, "{}", serde_json::to_string_pretty(v).unwrap());
        }
    } else {
        shown_lines(&mut rendered, &r.shown, shape);
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
    let body = run_body(result, final_output.clone());
    render_run_body(
        &body,
        &run_label("run", Some(target)),
        final_output.as_ref(),
        mode,
        fields,
    )
}

/// Width of the `barca history` TARGET column; longer targets end in `…`.
const TARGET_WIDTH: usize = 24;

/// A run's TARGET cell: its job's function names (`kpis`, `country_report,weekly_summary`), or
/// `all` for a run without targets. Runs recorded before the job was stored show the target as
/// typed. Cut to [`TARGET_WIDTH`] characters with a trailing `…`.
pub fn history_target(run: &crate::db::RunRecord) -> String {
    let names = match run.job.as_deref().or(run.target.as_deref()) {
        None | Some("all") => "all".to_string(),
        Some(ids) => ids
            .split(',')
            .map(|id| crate::telemetry::short_name(id.trim()))
            .collect::<Vec<_>>()
            .join(","),
    };
    if names.chars().count() > TARGET_WIDTH {
        let cut: String = names.chars().take(TARGET_WIDTH - 1).collect();
        format!("{cut}…")
    } else {
        names
    }
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
        "{:<14} {:<7} {:<TARGET_WIDTH$} {:<11} {:>5} {:>6} {:>6} {:<20}",
        "RUN_ID",
        "CMD",
        "TARGET",
        "STATUS",
        "STEPS",
        "CACHED",
        "TIME",
        "STARTED"
    );
    line!(rendered, "{}", "-".repeat(78 + TARGET_WIDTH));
    for r in runs {
        let elapsed_str = r
            .elapsed_seconds
            .map(|e| format!("{:.1}s", e))
            .unwrap_or_else(|| "-".to_string());
        line!(
            rendered,
            "{:<14} {:<7} {:<TARGET_WIDTH$} {:<11} {:>5} {:>6} {:>6} {:<20}",
            r.run_id,
            r.command,
            history_target(r),
            r.status,
            r.steps_executed,
            r.steps_cached,
            elapsed_str,
            crate::report::utc_cell(&r.started_at),
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
    // Not a cache hit rate: hits execute nothing and are not recorded per asset (#496).
    line!(
        rendered,
        "Re-executed with unchanged inputs: {:.1}%",
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
                crate::report::utc_cell(&entry.created_at),
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
        header.push("NEXT FIRE (LOCAL TIME)");
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
            if let Some(uploads) = &result.uploads {
                line!(
                    rendered,
                    "{} pending upload{} from an earlier run, uploaded first: {}{}",
                    uploads.pending,
                    if uploads.pending == 1 { "" } else { "s" },
                    uploads.nodes.join(", "),
                    uploads
                        .error
                        .as_deref()
                        .map(|e| format!(" (last error: {e})"))
                        .unwrap_or_default()
                );
            }
        }
    }
    rendered
}

pub fn first_multi_failure(result: &MultiResult) -> Option<crate::BarcaError> {
    // The first failed target becomes the run's error: exit 1 with the error envelope.
    if let Some((name, t)) = result.targets.iter().find(|(_, t)| t.failed()) {
        return Some(crate::BarcaError::WorkerFailed(Box::new(
            crate::FailedStep {
                node: t.failed_node.clone().unwrap_or_else(|| name.clone()),
                message: t.error.clone().unwrap_or_else(|| "unknown error".into()),
                artifact_dir: None,
                run: Some(Box::new(crate::PartialRun {
                    run_id: result.run_id.clone(),
                    elapsed_seconds: result.elapsed_seconds,
                    steps_executed: result.steps_executed,
                    phases: result.phases,
                    steps: result.steps.clone(),
                    warnings: result.warnings.clone(),
                    partitions: t.partitions.clone(),
                })),
            },
        )));
    }
    None
}

/// The run's own keys of a multi-target run's JSON (everything but `targets`).
fn multi_run(result: &MultiResult) -> Value {
    let mut run = serde_json::json!({
        "status": if result.any_failed() { "failed" } else { "success" },
        "run_id": result.run_id,
        "elapsed_seconds": result.elapsed_seconds,
        "steps_executed": result.steps_executed,
        "phases": result.phases,
        "steps": &result.steps,
        "warnings": &result.warnings,
    });
    if let (Value::Object(obj), Some(cache)) = (&mut run, &result.cache) {
        obj.insert("cache".into(), serde_json::json!(cache));
    }
    uploads_field(&mut run, result.uploads.as_ref());
    run
}

/// The JSON of a multi-target run, in two parts: the run's own keys (sorted, without
/// `targets`) and each target's object in the order given. Printed, they are
/// `{...run keys...,"targets":{...}}`.
pub fn multi_parts(
    result: &MultiResult,
    final_outputs: &[Option<Value>],
) -> (Value, Vec<(String, Value)>) {
    let per_target: Vec<(String, serde_json::Value)> = result
        .targets
        .iter()
        .enumerate()
        .map(|(index, (name, t))| {
            let mut obj = serde_json::Map::new();
            obj.insert("status".into(), t.status.clone().into());
            let value = final_outputs.get(index).cloned().flatten();
            if t.status == "success" {
                obj.insert("final_output".into(), value.unwrap_or(Value::Null));
            } else if t.partitions.is_some()
                && let Some(value) = value
            {
                // A failed partitioned target returns the keys that succeeded.
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
    (multi_run(result), per_target)
}

/// `{...run keys...,"targets":{...}}` with the targets in the order given.
pub fn multi_json(run: &Value, per_target: &[(String, Value)]) -> String {
    let run = run.to_string();
    format!(
        "{},\"targets\":{}}}",
        &run[..run.len() - 1],
        ordered_object(per_target)
    )
}

/// Render values supplied in the same order as `result.targets`; absent values are null.
pub fn render_multi(
    result: &MultiResult,
    final_outputs: &[Option<Value>],
    mode: ResultFormat,
    verb: &str,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let (run, per_target) = multi_parts(result, final_outputs);
    render_multi_parts(&run, &per_target, mode, verb, fields)
}

/// Render a multi-target run from its two JSON parts ([`multi_parts`]). The one renderer for a
/// local run and for a served run's `outcome`.
pub fn render_multi_parts(
    run: &Value,
    per_target: &[(String, Value)],
    mode: ResultFormat,
    verb: &str,
    fields: Option<&[String]>,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    match mode {
        ResultFormat::Json => {
            // Keys of the run object sort before `targets`, which keeps the given order.
            let mut run = run.clone();
            project_key(&mut run, "steps", fields);
            line!(rendered, "{}", multi_json(&run, per_target));
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
            let failed = per_target
                .iter()
                .filter(|(_, t)| t["status"].as_str() != Some("success"));
            let count = |key: &str| run[key].as_u64().unwrap_or(0);
            let (steps, phases) = (count("steps_executed"), count("phases"));
            line!(
                rendered,
                "Run {} | {verb} {} targets in {:.3}s ({} step{}, {} phase{}, {} failed)",
                run["run_id"].as_str().unwrap_or_default(),
                per_target.len(),
                run["elapsed_seconds"].as_f64().unwrap_or(0.0),
                steps,
                if steps == 1 { "" } else { "s" },
                phases,
                if phases == 1 { "" } else { "s" },
                failed.count(),
            );
            for (name, t) in per_target {
                let status = t["status"].as_str().unwrap_or("?");
                if status != "success" {
                    continue;
                }
                line!(rendered, "\n{name}: {status}");
                if let Some(v) = t.get("final_output") {
                    line!(rendered, "{}", serde_json::to_string_pretty(v).unwrap());
                }
            }
        }
    }

    rendered
}

/// The stdout JSON body of a failed single-target run: what ran before the failing step,
/// the step and its error. `None` for any other error.
pub fn failed_run_body(err: &crate::BarcaError) -> Option<Value> {
    let crate::BarcaError::WorkerFailed(f) = err else {
        return None;
    };
    let run = f.run.as_ref()?;
    Some(serde_json::json!({
        "status": "failed",
        "run_id": run.run_id,
        "elapsed_seconds": run.elapsed_seconds,
        "steps_executed": run.steps_executed,
        "phases": run.phases,
        "failed_node": f.node,
        "error": f.summary(),
        "steps": &run.steps,
        "warnings": &run.warnings,
    }))
}

pub fn render_failed_run(err: &crate::BarcaError, mode: ResultFormat) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if mode == ResultFormat::Json
        && let Some(body) = failed_run_body(err)
    {
        line!(rendered, "{body}");
    }
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

#[cfg(test)]
mod history_target_tests {
    use super::history_target;

    fn run(target: Option<&str>, job: Option<&str>) -> crate::db::RunRecord {
        crate::db::RunRecord {
            run_id: "r".into(),
            command: "get".into(),
            files: vec![],
            target: target.map(str::to_string),
            job: job.map(str::to_string),
            status: "success".into(),
            steps_total: None,
            steps_executed: 0,
            steps_cached: 0,
            attempts: None,
            started_at: String::new(),
            finished_at: None,
            elapsed_seconds: None,
            triggered_by: None,
            scheduled_at: None,
            schedule_id: None,
            client_run_id: None,
        }
    }

    #[test]
    fn the_job_by_function_name_else_the_typed_target_else_all() {
        assert_eq!(history_target(&run(None, Some("all"))), "all");
        assert_eq!(
            history_target(&run(Some("kpis"), Some("p/r.py:kpis"))),
            "kpis"
        );
        assert_eq!(
            history_target(&run(Some("b,a"), Some("p.py:a,p.py:b"))),
            "a,b"
        );
        // Rows recorded before the job was stored.
        assert_eq!(history_target(&run(Some("p.py:kpis"), None)), "kpis");
        assert_eq!(history_target(&run(None, None)), "all");
        let long = history_target(&run(None, Some("p.py:country_report,p.py:weekly_summary")));
        assert_eq!(long, "country_report,weekly_s…");
        assert_eq!(long.chars().count(), 24);
    }
}
