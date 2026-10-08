//! Static schedule analysis shared by the CLI and server.

use crate::queries;
use crate::results::AssetSummary;
use crate::{CronExpr, Freshness, NodeKind};
use chrono::Local;
use croner::Cron;
use serde::Serialize;

/// Static description of one scheduled job, for `barca list` (no server).
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleInfo {
    pub id: String,
    pub cron: String,
    pub kind: NodeKind,
    /// Next fire time as unix epoch seconds (for programmatic use).
    pub next_fire: Option<i64>,
    /// Next fire time formatted in local time (for display).
    pub next_fire_local: Option<String>,
}

/// Enumerate scheduled jobs from source and compute each one's next fire time.
/// Pure static analysis — used by the `barca list` CLI, no running server.
pub async fn describe_schedule(files: &[String], python: &std::path::Path) -> Vec<ScheduleInfo> {
    let now = Local::now();
    collect_jobs(files, python)
        .await
        .iter()
        .map(|j| {
            let next = j.cron.find_next_occurrence(&now, false).ok();
            ScheduleInfo {
                id: j.id.clone(),
                cron: j.cron_str.clone(),
                kind: j.kind,
                next_fire: next.map(|t| t.timestamp()),
                next_fire_local: next.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            }
        })
        .collect()
}

/// A parsed cron job discovered from the DAG at startup.
pub struct ScheduledJob {
    /// Full node id (e.g. `pipeline.py:daily_report`), used verbatim as the run target.
    pub id: String,
    /// Node kind decides the trigger path: asset/sensor → `get`, task → `run`.
    pub kind: NodeKind,
    /// The original cron string, kept for logging.
    pub cron_str: String,
    /// Parsed cron (5-field minute-granular, or 6-field seconds-granular),
    /// evaluated in the scheduler's configured timezone.
    pub cron: Cron,
}

/// Enumerate every node whose freshness is `Schedule(cron)` and parse each cron.
/// A DAG-analysis failure disables the scheduler (returns empty); individual
/// invalid/empty cron strings are logged and skipped rather than aborting.
pub async fn collect_jobs(files: &[String], python: &std::path::Path) -> Vec<ScheduledJob> {
    match queries::list_assets(files, python).await {
        Ok(summaries) => jobs_from_summaries(summaries),
        Err(e) => {
            eprintln!("[barca] scheduler disabled: failed to analyze DAG: {e}");
            Vec::new()
        }
    }
}

/// Pure summary → job mapping (split out from [`collect_jobs`] so it is testable
/// without a Python interpreter). Drops entries whose cron fails to parse.
pub fn jobs_from_summaries(summaries: Vec<AssetSummary>) -> Vec<ScheduledJob> {
    let mut jobs = Vec::new();
    for s in summaries {
        let Freshness::Schedule(expr) = &s.freshness else {
            continue;
        };
        match CronExpr::parse(&expr.0) {
            Ok(cron) => jobs.push(ScheduledJob {
                id: s.id,
                kind: s.kind,
                cron_str: expr.0.clone(),
                cron,
            }),
            Err(e) => eprintln!(
                "[barca] skipping '{}': invalid cron {:?}: {e}",
                s.id, expr.0
            ),
        }
    }
    jobs
}
