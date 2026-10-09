//! Static schedule analysis shared by the CLI and server.

use crate::queries;
use crate::results::AssetSummary;
use crate::{CronExpr, Freshness, NodeKind};
use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};
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
    describe_schedule_in(files, python, &Zone::Local).await
}

/// Describe jobs using the configured server timezone.
pub async fn describe_schedule_in(
    files: &[String],
    python: &std::path::Path,
    zone: &Zone,
) -> Vec<ScheduleInfo> {
    let now = Utc::now();
    collect_jobs(files, python)
        .await
        .iter()
        .map(|j| {
            let next = next_fire(&j.cron, zone, now);
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
            crate::errln!("[barca] scheduler disabled: failed to analyze DAG: {e}");
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
            Err(e) => crate::errln!(
                "[barca] skipping '{}': invalid cron {:?}: {e}",
                s.id,
                expr.0
            ),
        }
    }
    jobs
}

/// Resolved timezone that cron expressions are evaluated in.
#[derive(Debug)]
pub enum Zone {
    Local,
    Utc,
    Named(chrono_tz::Tz),
}

impl Zone {
    /// Parse a `--timezone` value: `local` or `utc` (in any letter case), or an IANA name
    /// spelled as in the tz database (`America/New_York`, `Etc/UTC`; case-sensitive).
    /// Surrounding whitespace is ignored. Anything else is an error that names the value and
    /// shows valid ones.
    pub fn parse(s: &str) -> Result<Self, String> {
        let name = s.trim();
        match name.to_ascii_lowercase().as_str() {
            "local" => Ok(Zone::Local),
            "utc" => Ok(Zone::Utc),
            _ => name.parse::<chrono_tz::Tz>().map(Zone::Named).map_err(|_| {
                format!(
                    "unknown timezone '{s}': use `local`, `utc`, or an IANA name such as \
                     `America/New_York` (IANA names are case-sensitive)"
                )
            }),
        }
    }

    /// The current instant in this zone as a fixed-offset datetime.
    pub fn now(&self) -> DateTime<FixedOffset> {
        self.at(Utc::now())
    }

    /// An instant as a fixed-offset datetime in this zone.
    pub fn at(&self, instant: DateTime<Utc>) -> DateTime<FixedOffset> {
        match self {
            Zone::Local => instant.with_timezone(&Local).fixed_offset(),
            Zone::Utc => instant.fixed_offset(),
            Zone::Named(tz) => instant.with_timezone(tz).fixed_offset(),
        }
    }

    /// An epoch-seconds timestamp interpreted in this zone.
    pub fn timestamp(&self, secs: i64) -> DateTime<FixedOffset> {
        let dt = match self {
            Zone::Local => Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.fixed_offset()),
            Zone::Utc => Utc
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.fixed_offset()),
            Zone::Named(tz) => tz.timestamp_opt(secs, 0).single().map(|t| t.fixed_offset()),
        };
        dt.unwrap_or_else(|| self.now())
    }
}

/// Find the next occurrence without freezing the timezone's daylight-saving offset.
pub fn next_fire(cron: &Cron, zone: &Zone, now: DateTime<Utc>) -> Option<DateTime<FixedOffset>> {
    match zone {
        Zone::Local => cron
            .find_next_occurrence(&now.with_timezone(&Local), false)
            .ok()
            .map(|t| t.fixed_offset()),
        Zone::Utc => cron
            .find_next_occurrence(&now, false)
            .ok()
            .map(|t| t.fixed_offset()),
        Zone::Named(tz) => cron
            .find_next_occurrence(&now.with_timezone(tz), false)
            .ok()
            .map(|t| t.fixed_offset()),
    }
}

#[cfg(test)]
mod timezone_tests {
    use super::*;

    #[test]
    fn next_fire_preserves_named_zone_across_daylight_saving_transitions() {
        let zone = Zone::parse("America/New_York").unwrap();
        let cron = CronExpr::parse("0 5 * * *").unwrap();
        for (start, expected) in [
            ((2026, 10, 31, 12), (2026, 11, 1, 10)),
            ((2026, 3, 7, 12), (2026, 3, 8, 9)),
        ] {
            let now = Utc
                .with_ymd_and_hms(start.0, start.1, start.2, start.3, 0, 0)
                .unwrap();
            let expected = Utc
                .with_ymd_and_hms(expected.0, expected.1, expected.2, expected.3, 0, 0)
                .unwrap();
            let next = next_fire(&cron, &zone, now).unwrap();
            assert_eq!(next.timestamp(), expected.timestamp());
            assert_eq!(next.format("%H:%M").to_string(), "05:00");
        }
    }
}
