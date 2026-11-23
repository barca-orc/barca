//! What a command prints, wherever it ran: the printers both backends use (`commands` on this
//! machine, `client` against a server), so a command's stdout and stderr are the same bytes
//! from either.
//!
//! Most results are rendered by `barca_service::report` into a `RenderedOutput`, which [`emit`]
//! writes. The printers below are the ones the CLI keeps: `pin`, `gc`, `list --groups` and
//! `logs`. They write straight to stdout and stderr, and a server prints none of them (it
//! answers with the typed value as JSON), so they stay here and not in `barca_service::report`
//! (`docs/engineering/cli-layering.md`).

use crate::request::LogsArgs;
use barca_service::db::{AssetPin, LogEntry, Version};
use barca_service::gc::{GcResult, size::human};
use barca_service::pins::{Pinned, Unpinned, Versions};
use barca_service::{BarcaError, LogStream};

/// Emit ordered rendering writes directly, preserving result/error order.
pub(crate) fn emit(rendered: barca_service::report::RenderedOutput) {
    use barca_service::report::OutputChunk;
    for chunk in rendered.chunks {
        match chunk {
            OutputChunk::Stdout(text) => barca_service::term::stdout_str(&text),
            OutputChunk::Stderr(text) => {
                barca_service::term::stderr_str(&text);
            }
        }
    }
}

// ─── list --groups ──────────────────────────────────────────────────────────

/// `barca list --groups`: the group hierarchy, as JSON `{"groups": [...]}` or as a tree.
pub(crate) fn print_groups(groups: &[barca_service::groups::NodeGroup], json: bool) {
    if json {
        barca_service::outln!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"groups": groups})).unwrap()
        );
        return;
    }
    let by_id: std::collections::HashMap<_, _> =
        groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let nested: std::collections::HashSet<_> = groups
        .iter()
        .flat_map(|g| g.members.iter().map(String::as_str))
        .collect();
    fn show(
        id: &str,
        depth: usize,
        groups: &std::collections::HashMap<&str, &barca_service::groups::NodeGroup>,
    ) {
        if let Some(g) = groups.get(id) {
            barca_service::outln!("{}{} [{}] → {}", "  ".repeat(depth), g.name, g.id, g.output);
            for m in &g.members {
                show(m, depth + 1, groups);
            }
        } else {
            barca_service::outln!("{}{}", "  ".repeat(depth), id);
        }
    }
    if groups.is_empty() {
        barca_service::outln!("No groups declared.");
    }
    for g in groups {
        if !nested.contains(g.id.as_str()) {
            show(&g.id, 0, &by_id);
        }
    }
}

// ─── gc ─────────────────────────────────────────────────────────────────────

/// The result of an eviction, as `barca gc` prints it wherever it ran.
pub(crate) fn print_gc(result: &GcResult, json: bool) {
    if json {
        barca_service::outln!("{}", serde_json::to_string_pretty(result).unwrap());
    } else {
        gc_text(result);
    }
}

fn gc_text(r: &GcResult) {
    let limit = match r.limit_bytes {
        Some(limit) => format!(" of {}", human(limit)),
        None => " (no limit: leftovers and superseded results only)".to_string(),
    };
    if r.evicted.is_empty() {
        barca_service::outln!(
            "nothing to evict: {} budgeted{limit}",
            human(r.budget_before_bytes)
        );
    } else {
        barca_service::outln!(
            "{} {} file{} ({}), leaving {}{limit}:",
            if r.dry_run { "would evict" } else { "evicted" },
            r.evicted_files,
            if r.evicted_files == 1 { "" } else { "s" },
            human(r.evicted_bytes),
            human(r.budget_after_bytes),
        );
        for tier in &r.tiers {
            let name = match tier.tier.as_str() {
                "leftovers" => "leftovers",
                "superseded" => "superseded",
                "store_held" => "held by the store",
                _ => "least recently read",
            };
            barca_service::outln!(
                "  {name:<20} {:>7} {:<5} {:>9}",
                tier.files,
                if tier.files == 1 { "file" } else { "files" },
                human(tier.bytes)
            );
        }
    }
    // Over the target after evicting what could go: say what holds the rest.
    barca_service::outln!(
        "{} metadata backups outside the budget; {} physical total",
        human(r.backup_bytes),
        human(r.after_bytes)
    );
    if r.target_bytes.is_some_and(|t| r.budget_after_bytes > t) && !r.held.is_empty() {
        let parts: Vec<String> = r
            .held
            .iter()
            .map(|h| {
                let words = match h.what.as_str() {
                    "results" => "cached results",
                    "unreferenced" => "files no result names (kept while a run is in flight or for a minute after they are written)",
                    "metadata" => "metadata databases (never evicted)",
                    "db_copies" => "databases moved aside by state reset/restore (never evicted; delete them once you no longer need to undo)",
                    "backups" => "metadata backups (outside the budget; never evicted)",
                    _ => "other",
                };
                format!("{} {words}", human(h.bytes))
            })
            .collect();
        barca_service::outln!("held by: {}", parts.join(", "));
    }
    if let Some(why) = &r.restricted {
        barca_service::errln!(
            "[barca] {why}: only leftovers and superseded results were considered"
        );
    }
    if let Some(f) = r.failed.first() {
        barca_service::errln!(
            "[barca] warning: could not evict {}: {} ({} skipped)",
            f.path,
            f.error,
            r.failed.len()
        );
    }
}

// ─── pin, unpin ─────────────────────────────────────────────────────────────

fn short(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, name)| name)
}

fn hash(h: &str) -> &str {
    &h[..h.len().min(12)]
}

fn json_line(value: &impl serde::Serialize) {
    barca_service::outln!("{}", serde_json::to_string(value).unwrap_or_default());
}

fn by(pin: &AssetPin) -> String {
    let mut out = format!("since {}", pin.pinned_at);
    if let Some(by) = &pin.pinned_by {
        out.push_str(&format!(" by {by}"));
    }
    if let Some(reason) = &pin.reason {
        out.push_str(&format!(": {reason}"));
    }
    out
}

pub(crate) fn print_pins(pins: &[AssetPin], json: bool) {
    if json {
        json_line(&serde_json::json!({ "pins": pins }));
        return;
    }
    if pins.is_empty() {
        barca_service::outln!("no pinned assets");
        return;
    }
    for pin in pins {
        barca_service::outln!(
            "{}  pinned at {}  {}",
            pin.node_id,
            hash(&pin.run_hash),
            by(pin)
        );
    }
}

fn size(v: &Version) -> String {
    v.size_bytes
        .map(|b| barca_service::gc::size::human(b.max(0) as u64))
        .unwrap_or_else(|| "-".into())
}

pub(crate) fn print_versions(v: &Versions, json: bool) {
    if json {
        json_line(v);
        return;
    }
    if let Some(pin) = &v.pin {
        barca_service::outln!(
            "{} is pinned at {} {}",
            short(&v.node_id),
            hash(&pin.run_hash),
            by(pin)
        );
    }
    if !v.pinnable {
        barca_service::outln!(
            "{} cannot be pinned (only an asset without partitions can)",
            short(&v.node_id)
        );
    }
    if v.versions.is_empty() {
        barca_service::outln!("no successful materializations of {}", short(&v.node_id));
        return;
    }
    barca_service::outln!(
        "{:<12}  {:<20}  {:>9}  {:>9}  {:<24}  note",
        "run hash",
        "materialized at",
        "duration",
        "size",
        "run id"
    );
    for ver in &v.versions {
        let mut note = Vec::new();
        if ver.current {
            note.push("latest");
        }
        if ver.pinned {
            note.push("pinned");
        }
        if !ver.available {
            note.push("evicted");
        }
        if ver.upload.is_some() {
            note.push("uploaded");
        }
        barca_service::outln!(
            "{:<12}  {:<20}  {:>9}  {:>9}  {:<24}  {}",
            hash(&ver.run_hash),
            ver.materialized_at,
            ver.elapsed_seconds
                .map(|s| format!("{s:.2}s"))
                .unwrap_or_else(|| "-".into()),
            size(ver),
            ver.run_id.as_deref().unwrap_or("-"),
            note.join(", ")
        );
    }
}

pub(crate) fn print_pinned(p: &Pinned, json: bool) {
    if json {
        json_line(p);
        return;
    }
    if let Some(up) = &p.version.upload {
        barca_service::outln!(
            "pinned {} at {}, uploaded from {} ({}); runs read it and never recompute it until \
             `barca unpin {}`",
            short(&p.node_id),
            hash(&p.pin.run_hash),
            up.file_name.as_deref().unwrap_or("a file"),
            size(&p.version),
            short(&p.node_id)
        );
        if let Some(old) = &p.replaced {
            barca_service::errln!("[barca] replaced its pin at {}", hash(&old.run_hash));
        }
        return;
    }
    barca_service::outln!(
        "pinned {} at {} (materialized {}, run {}); runs read it and never recompute it until \
         `barca unpin {}`",
        short(&p.node_id),
        hash(&p.pin.run_hash),
        p.version.materialized_at,
        p.version.run_id.as_deref().unwrap_or("-"),
        short(&p.node_id)
    );
    if let Some(old) = &p.replaced {
        barca_service::errln!("[barca] replaced its pin at {}", hash(&old.run_hash));
    }
}

pub(crate) fn print_unpinned(u: &Unpinned, json: bool) {
    if json {
        json_line(u);
        return;
    }
    match &u.removed {
        Some(pin) => barca_service::outln!(
            "unpinned {} (was pinned at {}); the next run decides it as before",
            short(&u.node_id),
            hash(&pin.run_hash)
        ),
        None => barca_service::outln!("{} was not pinned", short(&u.node_id)),
    }
}

// ─── logs ───────────────────────────────────────────────────────────────────

impl LogsArgs<'_> {
    /// Print lines read all at once: a JSON array, or JSON Lines while following.
    pub(crate) fn print_all(&self, lines: &[LogEntry]) -> Result<(), BarcaError> {
        if self.json && !self.follow {
            let entries: Vec<_> = lines.iter().map(entry_json).collect();
            barca_service::outln!("[{}]", entries.join(","));
        } else {
            for line in lines {
                self.print_one(line);
            }
        }
        Ok(())
    }

    /// Print one line as it arrives: its text (after its time with --timestamps), or with
    /// --json one object on a line of its own.
    pub(crate) fn print_one(&self, entry: &LogEntry) {
        if self.json {
            barca_service::outln!("{}", entry_json(entry));
        } else if self.timestamps {
            barca_service::outln!(
                "{} {}",
                entry.ts.as_deref().unwrap_or(UNKNOWN_TS),
                entry.line
            );
        } else {
            barca_service::outln!("{}", entry.line);
        }
    }
}

/// The time column of a line recorded before barca stamped lines.
const UNKNOWN_TS: &str = "-                       ";

/// One line in `--json`: `{node_id, stream, ts, line}`, in that order.
#[derive(serde::Serialize)]
struct EntryJson<'a> {
    node_id: &'a str,
    stream: LogStream,
    ts: Option<&'a str>,
    line: &'a str,
}

fn entry_json(entry: &LogEntry) -> String {
    serde_json::to_string(&EntryJson {
        node_id: &entry.node_id,
        stream: entry.stream,
        ts: entry.ts.as_deref(),
        line: &entry.line,
    })
    .unwrap_or_default()
}

/// The exit code `-f` ends with, from the run's status: 0 succeeded, 130 cancelled, 1 otherwise.
pub(crate) fn follow_exit_code(status: &str) -> i32 {
    match status {
        "success" | "skipped" => 0,
        "cancelled" => 130,
        _ => 1,
    }
}

/// The stderr line that ends `-f`.
pub(crate) fn follow_ended(run_id: &str, status: &str) {
    barca_service::errln!("barca: run {run_id} ended: {status}");
}

/// The stderr line naming the run read without a run id.
pub(crate) fn name_run(run: &barca_service::db::RunRecord, follow: bool) {
    let what = if follow && run.status == "running" {
        "following the newest running run"
    } else {
        "logs of the most recent run"
    };
    barca_service::errln!(
        "barca: {what}: {} ({} {}, {}, started {})",
        run.run_id,
        run.command,
        barca_service::report::history_target(run),
        run.status,
        barca_service::report::utc_cell(&run.started_at)
    );
}

/// Shared local/server presentation for a cached read; misses retain every key's outcome.
pub(crate) fn print_cached(
    answer: &barca_service::cached::Answer,
    mode: crate::input::GetMode,
    fields: Option<&[String]>,
) -> Result<(), barca_service::BarcaError> {
    if mode.shape == barca_service::report::GetShape::Json {
        let mut value = serde_json::to_value(answer)
            .map_err(|e| barca_service::BarcaError::Other(e.to_string()))?;
        if let Some(fields) = fields
            && let Some(obj) = value.as_object_mut()
        {
            obj.retain(|k, _| fields.contains(k));
        }
        barca_service::outln!("{}", value);
    } else if mode.values {
        if let Some(value) = &answer.final_output {
            barca_service::outln!("{}", value);
        }
    } else {
        for outcome in &answer.outcomes {
            if let Some(path) = outcome
                .get(if mode.url_expires.is_some() {
                    "url"
                } else {
                    "path"
                })
                .and_then(serde_json::Value::as_str)
            {
                barca_service::outln!("{path}");
            }
        }
    }
    answer.check()
}
