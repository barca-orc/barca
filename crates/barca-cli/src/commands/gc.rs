//! `barca gc`: evict cached results to keep `.barca/` under its size limit (RFC-0012).

use crate::error::{CliError, ErrorKind};
use barca_core::gc::{GcOptions, GcResult, size::human};

#[allow(clippy::result_large_err)] // one error envelope on the command failure path
pub(crate) async fn gc_cmd(
    env: Option<&str>,
    max_size: Option<&str>,
    dry_run: bool,
    json: bool,
) -> Result<(), CliError> {
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, &Default::default());
    let max_size = max_size
        .map(barca_core::gc::size::parse)
        .transpose()
        .map_err(|e| CliError::from_prose(ErrorKind::Usage, format!("error: --max-size: {e}")))?;
    let cfg = barca_core::config::resolve(env).map_err(engine)?;
    let opts = GcOptions {
        max_size,
        dry_run,
        env: env.map(String::from),
    };
    let result = barca_core::gc::gc(&cfg, &opts).await.map_err(engine)?;
    if json {
        barca_core::outln!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        render(&result);
    }
    Ok(())
}

fn render(r: &GcResult) {
    let limit = match r.limit_bytes {
        Some(limit) => format!(" of {}", human(limit)),
        None => " (no limit: leftovers and superseded results only)".to_string(),
    };
    if r.evicted.is_empty() {
        barca_core::outln!("nothing to evict: {}{limit}", human(r.size_bytes));
    } else {
        barca_core::outln!(
            "{} {} file{} ({}), leaving {}{limit}:",
            if r.dry_run { "would evict" } else { "evicted" },
            r.evicted_files,
            if r.evicted_files == 1 { "" } else { "s" },
            human(r.evicted_bytes),
            human(r.after_bytes),
        );
        for tier in &r.tiers {
            let name = match tier.tier.as_str() {
                "leftovers" => "leftovers",
                "superseded" => "superseded",
                "store_held" => "held by the store",
                _ => "least recently read",
            };
            barca_core::outln!(
                "  {name:<20} {:>7} {:<5} {:>9}",
                tier.files,
                if tier.files == 1 { "file" } else { "files" },
                human(tier.bytes)
            );
        }
    }
    // Over the target after evicting what could go: say what holds the rest.
    if r.target_bytes.is_some_and(|t| r.after_bytes > t) && !r.held.is_empty() {
        let parts: Vec<String> = r
            .held
            .iter()
            .map(|h| {
                let words = match h.what.as_str() {
                    "results" => "cached results",
                    "unreferenced" => "files no result names (kept while a run is in flight or for a minute after they are written)",
                    "metadata" => "metadata databases (never evicted)",
                    "db_copies" => "databases moved aside by state reset/restore (never evicted; delete them once you no longer need to undo)",
                    "backups" => "metadata backups (never evicted)",
                    _ => "other",
                };
                format!("{} {words}", human(h.bytes))
            })
            .collect();
        barca_core::outln!("held by: {}", parts.join(", "));
    }
    if let Some(why) = &r.restricted {
        barca_core::errln!("[barca] {why}: only leftovers and superseded results were considered");
    }
    if let Some(f) = r.failed.first() {
        barca_core::errln!(
            "[barca] warning: could not evict {}: {} ({} skipped)",
            f.path,
            f.error,
            r.failed.len()
        );
    }
}
