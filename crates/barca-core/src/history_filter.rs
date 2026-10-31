//! The filters of `barca history` (`--status`, `--target`, `--triggered-by`, `--since`), and
//! of the server's `GET /history` (the same names as query parameters). They combine with AND,
//! and a limit applies to the runs they keep.

use crate::db::RunRecord;
use crate::outcome::TriggeredBy;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

/// The statuses a recorded run can have: how it ended, `running` while it is in progress, and
/// `interrupted` for a run whose process died before recording an outcome.
pub const STATUSES: &[&str] = &[
    "success",
    "failed",
    "cancelled",
    "error",
    "running",
    "interrupted",
];

/// The values of `--triggered-by`.
pub const TRIGGERS: &[&str] = &["manual", "api", "schedule", "client"];

/// Which runs `barca history` lists. Empty: every run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryFilter {
    /// Any of these statuses.
    pub statuses: Vec<String>,
    /// Runs whose target or job names this node (a function name or node id).
    pub target: Option<String>,
    /// Started by any of these.
    pub triggered_by: Vec<TriggeredBy>,
    /// Started at or after this time (UTC).
    pub since: Option<NaiveDateTime>,
    /// `--since` as it was given, passed on to a server as is.
    pub since_text: Option<String>,
}

impl HistoryFilter {
    /// From the flags' (or query parameters') text. An unknown value is an error naming the
    /// valid ones. `now` is when a relative `--since` counts back from.
    pub fn parse(
        status: Option<&str>,
        target: Option<&str>,
        triggered_by: Option<&str>,
        since: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Self, String> {
        let mut filter = Self::default();
        for status in list(status) {
            if !STATUSES.contains(&status) {
                return Err(format!(
                    "--status '{status}' is not a run status: use {}",
                    STATUSES.join(", ")
                ));
            }
            filter.statuses.push(status.to_string());
        }
        for trigger in list(triggered_by) {
            let parsed = TriggeredBy::parse(trigger).ok_or_else(|| {
                format!(
                    "--triggered-by '{trigger}' is not a trigger: use {}",
                    TRIGGERS.join(", ")
                )
            })?;
            filter.triggered_by.push(parsed);
        }
        filter.target = target
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        if let Some(text) = since.map(str::trim).filter(|t| !t.is_empty()) {
            filter.since = Some(parse_since(text, now)?);
            filter.since_text = Some(text.to_string());
        }
        Ok(filter)
    }

    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
            && self.target.is_none()
            && self.triggered_by.is_empty()
            && self.since.is_none()
    }

    /// Whether `run` is one to list.
    pub fn matches(&self, run: &RunRecord) -> bool {
        if !self.statuses.is_empty() && !self.statuses.contains(&run.status) {
            return false;
        }
        if !self.triggered_by.is_empty()
            && !run
                .triggered_by
                .is_some_and(|t| self.triggered_by.contains(&t))
        {
            return false;
        }
        if let Some(target) = &self.target
            && !names_node(run, target)
        {
            return false;
        }
        if let Some(since) = self.since {
            match stored_time(&run.started_at) {
                Some(started) if started >= since => {}
                _ => return false,
            }
        }
        true
    }

    /// The query string of `GET /history` for these filters (without `?`), each value
    /// percent-encoded.
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if !self.statuses.is_empty() {
            pairs.push(("status", self.statuses.join(",")));
        }
        if let Some(target) = &self.target {
            pairs.push(("target", target.clone()));
        }
        if !self.triggered_by.is_empty() {
            let names: Vec<_> = self.triggered_by.iter().map(|t| t.as_str()).collect();
            pairs.push(("triggered_by", names.join(",")));
        }
        if let Some(since) = &self.since_text {
            pairs.push(("since", since.clone()));
        }
        pairs
    }
}

/// The comma-separated values of a flag, blanks left out.
fn list(text: Option<&str>) -> impl Iterator<Item = &str> {
    text.into_iter()
        .flat_map(|t| t.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// Whether the run's target (as typed) or its job (resolved node ids) names `node`. A run of
/// every node (`all`, or no target) names none in particular.
fn names_node(run: &RunRecord, node: &str) -> bool {
    run.target
        .iter()
        .chain(run.job.iter())
        .flat_map(|ids| ids.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty() && *id != "all")
        .any(|id| id == node || crate::queries::step_matches(id, node))
}

/// `--since`: a duration back from `now` (`45s`, `30m`, `24h`, `7d`, `2w`), an RFC 3339 time
/// (`2026-10-22T08:00:00Z`), or a UTC date (`2026-10-22`) or time (`2026-10-22 08:00:00`).
pub fn parse_since(text: &str, now: DateTime<Utc>) -> Result<NaiveDateTime, String> {
    let t = text.trim();
    if let Some((digits, unit)) = t.char_indices().last().and_then(|(i, c)| {
        (c.is_ascii_alphabetic() && i > 0 && t[..i].bytes().all(|b| b.is_ascii_digit()))
            .then(|| (&t[..i], c.to_ascii_lowercase()))
    }) {
        let scale: i64 = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            _ => 0,
        };
        if scale > 0
            && let Ok(n) = digits.parse::<i64>()
            && let Some(back) = n.checked_mul(scale).and_then(chrono::Duration::try_seconds)
            && let Some(at) = now.checked_sub_signed(back)
        {
            return Ok(at.naive_utc());
        }
    }
    if let Some(at) = stored_time(t) {
        return Ok(at);
    }
    if let Ok(date) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).unwrap_or_default());
    }
    Err(format!(
        "--since '{text}' is neither a duration nor a time: use 30m, 24h, 7d, \
         2026-10-22 or 2026-10-22T08:00:00Z"
    ))
}

/// A stored or typed time as UTC: RFC 3339 (any offset), or SQLite's `datetime('now')`
/// (`2026-10-22 08:00:00`, UTC).
pub fn stored_time(text: &str) -> Option<NaiveDateTime> {
    let t = text.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(t) {
        return Some(at.with_timezone(&Utc).naive_utc());
    }
    let t = t.strip_suffix('Z').unwrap_or(t);
    ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(t, format).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn run(status: &str, target: Option<&str>, job: Option<&str>, started: &str) -> RunRecord {
        RunRecord {
            run_id: "r".into(),
            command: "get".into(),
            files: vec![],
            target: target.map(str::to_string),
            job: job.map(str::to_string),
            status: status.into(),
            steps_total: None,
            steps_executed: 0,
            steps_cached: 0,
            steps_failed: None,
            attempts: None,
            started_at: started.into(),
            finished_at: None,
            elapsed_seconds: None,
            triggered_by: Some(TriggeredBy::Schedule),
            scheduled_at: None,
            schedule_id: None,
            client_run_id: None,
            failure: None,
        }
    }

    fn filter(
        status: Option<&str>,
        target: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
    ) -> HistoryFilter {
        HistoryFilter::parse(status, target, trigger, since, now()).unwrap()
    }

    #[test]
    fn since_takes_durations_times_and_dates() {
        let at = |t: &str| parse_since(t, now()).unwrap().to_string();
        assert_eq!(at("24h"), "2026-10-21 12:00:00");
        assert_eq!(at("30m"), "2026-10-22 11:30:00");
        assert_eq!(at("7d"), "2026-10-15 12:00:00");
        assert_eq!(at("2w"), "2026-10-08 12:00:00");
        assert_eq!(at("2026-10-22T10:00:00+02:00"), "2026-10-22 08:00:00");
        assert_eq!(at("2026-10-20"), "2026-10-20 00:00:00");
        assert_eq!(at("2026-10-20 06:30:00"), "2026-10-20 06:30:00");
        for bad in ["yesterday", "24x", "h", "2026-13-01"] {
            assert!(parse_since(bad, now()).is_err(), "{bad}");
        }
    }

    #[test]
    fn unknown_values_name_the_valid_ones() {
        let err = HistoryFilter::parse(Some("failed,broken"), None, None, None, now()).unwrap_err();
        assert!(
            err.contains("'broken'") && err.contains("interrupted"),
            "{err}"
        );
        let err = HistoryFilter::parse(None, None, Some("cron"), None, now()).unwrap_err();
        assert!(err.contains("'cron'") && err.contains("schedule"), "{err}");
    }

    #[test]
    fn filters_combine_with_and() {
        let old = run(
            "failed",
            Some("clean"),
            Some("p.py:clean"),
            "2026-10-01 00:00:00",
        );
        let new = run(
            "failed",
            None,
            Some("p.py:clean,p.py:load"),
            "2026-10-22 11:00:00",
        );
        let all = run("success", None, Some("all"), "2026-10-22T11:30:00Z");
        assert!(filter(None, None, None, None).is_empty());
        assert!(filter(Some("failed"), None, None, None).matches(&old));
        assert!(!filter(Some("failed"), None, None, None).matches(&all));
        assert!(filter(Some("success,failed"), None, None, None).matches(&all));
        for name in ["clean", "p.py:clean"] {
            assert!(filter(None, Some(name), None, None).matches(&old));
            assert!(filter(None, Some(name), None, None).matches(&new));
            assert!(!filter(None, Some(name), None, None).matches(&all));
        }
        assert!(!filter(None, Some("lean"), None, None).matches(&old));
        let recent = filter(Some("failed"), Some("clean"), None, Some("24h"));
        assert!(!recent.matches(&old) && recent.matches(&new) && !recent.matches(&all));
        assert!(filter(None, None, None, Some("1h")).matches(&all));
        assert!(filter(None, None, Some("schedule"), None).matches(&all));
        assert!(!filter(None, None, Some("manual,api"), None).matches(&all));
    }

    #[test]
    fn query_pairs_carry_the_flags_as_given() {
        let f = filter(
            Some("failed,interrupted"),
            Some("clean"),
            Some("schedule"),
            Some("24h"),
        );
        assert_eq!(
            f.query_pairs(),
            vec![
                ("status", "failed,interrupted".to_string()),
                ("target", "clean".to_string()),
                ("triggered_by", "schedule".to_string()),
                ("since", "24h".to_string()),
            ]
        );
    }
}
