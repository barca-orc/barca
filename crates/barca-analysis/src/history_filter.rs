//! The filters of `barca history` (`--status`, `--target`, `--triggered-by`,
//! `--not-triggered-by`, `--since`, `--until`, `--search`), and of the server's `GET /history`
//! and `GET /runs` (the same names as query parameters). They combine with AND, the database
//! keeps the runs ([`barca_db::db::RunFilter`]), and a limit applies to the runs they keep.

use barca_db::db::{RunFilter, RunRecord};
use barca_model::trigger::TriggeredBy;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

/// The statuses a recorded run can have: how it ended, `running` while it is in progress, and
/// `interrupted` for a run whose process died before recording an outcome.
pub(crate) const STATUSES: &[&str] = &[
    "success",
    "failed",
    "cancelled",
    "pending",
    "running",
    "interrupted",
];

/// `--status not_successful`: every status of a run that ended without succeeding, the runs
/// `not_successful` counts in the activity.
pub const NOT_SUCCESSFUL: &str = "not_successful";

/// The statuses [`NOT_SUCCESSFUL`] names.
pub(crate) const NOT_SUCCESSFUL_STATUSES: &[&str] = &["failed", "cancelled", "interrupted"];

/// The values of `--triggered-by`.
pub(crate) const TRIGGERS: &[&str] = &["manual", "api", "schedule", "client"];

/// The filter flags as text: the flags of `barca history`, or the query parameters of
/// `GET /history` and `GET /runs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct HistoryFlags<'a> {
    pub status: Option<&'a str>,
    pub target: Option<&'a str>,
    pub triggered_by: Option<&'a str>,
    pub not_triggered_by: Option<&'a str>,
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub search: Option<&'a str>,
}

/// Which runs `barca history` lists. Empty: every run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryFilter {
    /// Any of these statuses.
    pub statuses: Vec<String>,
    /// `--status` as it was given, passed on to a server as is.
    pub status_text: Option<String>,
    /// Runs whose target or job names this node (a function name or node id).
    pub target: Option<String>,
    /// Started by any of these.
    pub triggered_by: Vec<TriggeredBy>,
    /// Started by none of these; a run that recorded no trigger is kept.
    pub not_triggered_by: Vec<TriggeredBy>,
    /// Started at or after this time (UTC).
    pub since: Option<NaiveDateTime>,
    /// `--since` as it was given, passed on to a server as is.
    pub since_text: Option<String>,
    /// Started before this time (UTC).
    pub until: Option<NaiveDateTime>,
    /// `--until` as it was given, passed on to a server as is.
    pub until_text: Option<String>,
    /// Runs whose run id, target or job contains this text, whatever its case.
    pub search: Option<String>,
}

impl HistoryFilter {
    /// From the flags' (or query parameters') text. An unknown value is an error naming the
    /// valid ones. `now` is when a relative `--since` or `--until` counts back from.
    pub fn parse(flags: HistoryFlags<'_>, now: DateTime<Utc>) -> Result<Self, String> {
        let mut filter = Self::default();
        for status in list(flags.status) {
            if status == NOT_SUCCESSFUL {
                for s in NOT_SUCCESSFUL_STATUSES {
                    if !filter.statuses.iter().any(|have| have == s) {
                        filter.statuses.push(s.to_string());
                    }
                }
                continue;
            }
            if !STATUSES.contains(&status) {
                return Err(format!(
                    "--status '{status}' is not a run status: use {}, or {NOT_SUCCESSFUL} \
                     (every terminal status but success)",
                    STATUSES.join(", ")
                ));
            }
            if !filter.statuses.iter().any(|have| have == status) {
                filter.statuses.push(status.to_string());
            }
        }
        if !filter.statuses.is_empty() {
            filter.status_text = flags.status.map(|s| s.trim().to_string());
        }
        let triggers = |text: Option<&str>, flag: &str| {
            list(text)
                .map(|trigger| {
                    TriggeredBy::parse(trigger).ok_or_else(|| {
                        format!(
                            "{flag} '{trigger}' is not a trigger: use {}",
                            TRIGGERS.join(", ")
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        };
        filter.triggered_by = triggers(flags.triggered_by, "--triggered-by")?;
        filter.not_triggered_by = triggers(flags.not_triggered_by, "--not-triggered-by")?;
        let text = |t: Option<&str>| {
            t.map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        filter.target = text(flags.target);
        filter.search = text(flags.search);
        if let Some(since) = text(flags.since) {
            filter.since = Some(parse_time("--since", &since, now)?);
            filter.since_text = Some(since);
        }
        if let Some(until) = text(flags.until) {
            filter.until = Some(parse_time("--until", &until, now)?);
            filter.until_text = Some(until);
        }
        Ok(filter)
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The filter as the database reads it. `--target` narrows the read by the node's name
    /// and is then checked run by run ([`HistoryFilter::names`]).
    pub fn run_filter(&self) -> RunFilter {
        let stamp = |t: NaiveDateTime| t.format("%Y-%m-%d %H:%M:%S").to_string();
        RunFilter {
            statuses: self.statuses.clone(),
            triggers: self.triggered_by.clone(),
            not_triggers: self.not_triggered_by.clone(),
            since: self.since.map(stamp),
            until: self.until.map(stamp),
            search: self.search.clone(),
            // Every id that names the node holds its name without a partition key.
            names: self
                .target
                .as_deref()
                .map(|t| t.split('[').next().unwrap_or(t).to_string()),
        }
    }

    /// Whether `run`'s target or job names `--target` (every run without one).
    pub fn names(&self, run: &RunRecord) -> bool {
        self.target
            .as_deref()
            .is_none_or(|target| names_node(run, target))
    }

    /// Whether a run the database does not hold yet (one waiting in a server's queue, or one
    /// whose row is newer than the copy read) is one to list. `status` is its status as a run
    /// list says it; `pending` is in progress, which `running` names. `texts` are its id and
    /// target, for a search.
    pub fn keeps_unrecorded(
        &self,
        status: &str,
        triggered_by: Option<TriggeredBy>,
        started_at: &str,
        texts: &[&str],
    ) -> bool {
        if !self.statuses.is_empty() && !self.statuses.iter().any(|s| s == status) {
            return false;
        }
        if !self.triggered_by.is_empty()
            && !triggered_by.is_some_and(|t| self.triggered_by.contains(&t))
        {
            return false;
        }
        if triggered_by.is_some_and(|t| self.not_triggered_by.contains(&t)) {
            return false;
        }
        if self.since.is_some() || self.until.is_some() {
            let Some(started) = stored_time(started_at) else {
                return false;
            };
            if self.since.is_some_and(|since| started < since)
                || self.until.is_some_and(|until| started >= until)
            {
                return false;
            }
        }
        match &self.search {
            Some(needle) => {
                let needle = needle.to_ascii_lowercase();
                texts
                    .iter()
                    .any(|t| t.to_ascii_lowercase().contains(&needle))
            }
            None => true,
        }
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
        if run
            .triggered_by
            .is_some_and(|t| self.not_triggered_by.contains(&t))
        {
            return false;
        }
        if !self.names(run) {
            return false;
        }
        if self.since.is_some() || self.until.is_some() {
            let Some(started) = stored_time(&run.started_at) else {
                return false;
            };
            if self.since.is_some_and(|since| started < since)
                || self.until.is_some_and(|until| started >= until)
            {
                return false;
            }
        }
        match &self.search {
            Some(needle) => {
                let needle = needle.to_ascii_lowercase();
                [Some(&run.run_id), run.target.as_ref(), run.job.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|t| t.to_ascii_lowercase().contains(&needle))
            }
            None => true,
        }
    }

    /// The query string of `GET /history` for these filters (without `?`), each value
    /// percent-encoded.
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(status) = &self.status_text {
            pairs.push(("status", status.clone()));
        }
        if let Some(target) = &self.target {
            pairs.push(("target", target.clone()));
        }
        let names = |triggers: &[TriggeredBy]| {
            triggers
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        if !self.triggered_by.is_empty() {
            pairs.push(("triggered_by", names(&self.triggered_by)));
        }
        if !self.not_triggered_by.is_empty() {
            pairs.push(("not_triggered_by", names(&self.not_triggered_by)));
        }
        if let Some(since) = &self.since_text {
            pairs.push(("since", since.clone()));
        }
        if let Some(until) = &self.until_text {
            pairs.push(("until", until.clone()));
        }
        if let Some(search) = &self.search {
            pairs.push(("search", search.clone()));
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
        .any(|id| id == node || step_matches(id, node))
}

/// Whether `--step <step>` selects the line of node `id`: its exact id, its base id (every
/// partition key), or its function name.
pub fn step_matches(id: &str, step: &str) -> bool {
    let parsed = barca_model::StepId::parse(id);
    if id == step || parsed.base_id() == step || parsed.base_id().rsplit(':').next() == Some(step) {
        return true;
    }
    // One key by function name: `scored[retailer=r7]`.
    let wanted = barca_model::StepId::parse(step);
    !wanted.partition.is_empty()
        && wanted.partition == parsed.partition
        && parsed.base_id().rsplit(':').next() == Some(wanted.base_id())
}

/// `--since`: a duration back from `now` (`45s`, `30m`, `24h`, `7d`, `2w`), an RFC 3339 time
/// (`2026-10-22T08:00:00Z`), or a UTC date (`2026-10-22`) or time (`2026-10-22 08:00:00`).
#[cfg(test)]
pub(crate) fn parse_since(text: &str, now: DateTime<Utc>) -> Result<NaiveDateTime, String> {
    parse_time("--since", text, now)
}

/// A time flag (`--since`, `--until`), as [`parse_since`] reads it.
pub(crate) fn parse_time(
    flag: &str,
    text: &str,
    now: DateTime<Utc>,
) -> Result<NaiveDateTime, String> {
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
        "{flag} '{text}' is neither a duration nor a time: use 30m, 24h, 7d, \
         2026-10-22 or 2026-10-22T08:00:00Z"
    ))
}

/// A stored or typed time as UTC: RFC 3339 (any offset), or SQLite's `datetime('now')`
/// (`2026-10-22 08:00:00`, UTC).
pub(crate) fn stored_time(text: &str) -> Option<NaiveDateTime> {
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
            ..Default::default()
        }
    }

    fn filter(
        status: Option<&str>,
        target: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
    ) -> HistoryFilter {
        HistoryFilter::parse(
            HistoryFlags {
                status,
                target,
                triggered_by: trigger,
                since,
                ..Default::default()
            },
            now(),
        )
        .unwrap()
    }

    fn flags(flags: HistoryFlags<'_>) -> HistoryFilter {
        HistoryFilter::parse(flags, now()).unwrap()
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
        let parse = |flags| HistoryFilter::parse(flags, now());
        let err = parse(HistoryFlags {
            status: Some("failed,broken"),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("'broken'") && err.contains("interrupted"),
            "{err}"
        );
        let err = parse(HistoryFlags {
            triggered_by: Some("cron"),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("'cron'") && err.contains("schedule"), "{err}");
        let err = parse(HistoryFlags {
            not_triggered_by: Some("cron"),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.starts_with("--not-triggered-by 'cron'"), "{err}");
        let err = parse(HistoryFlags {
            until: Some("soon"),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.starts_with("--until 'soon'"), "{err}");
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

    /// #969: leaving a trigger out keeps the runs that recorded none; a window has an end;
    /// text is looked for in the run id, target and job.
    #[test]
    fn exclusion_window_and_search() {
        let mut legacy = run("failed", Some("clean"), None, "2026-10-22 11:00:00");
        legacy.triggered_by = None;
        let tick = run("success", Some("clean"), None, "2026-10-22 11:00:00");
        let hide = flags(HistoryFlags {
            not_triggered_by: Some("schedule"),
            ..Default::default()
        });
        assert!(hide.matches(&legacy) && !hide.matches(&tick));
        assert_eq!(hide.run_filter().not_triggers, vec![TriggeredBy::Schedule]);
        let hour = flags(HistoryFlags {
            since: Some("2026-10-22T11:00:00Z"),
            until: Some("2026-10-22T12:00:00Z"),
            ..Default::default()
        });
        assert!(hour.matches(&tick));
        assert!(!hour.matches(&run("success", None, None, "2026-10-22 12:00:00")));
        assert!(!hour.matches(&run("success", None, None, "2026-10-22 10:59:59")));
        assert_eq!(
            hour.run_filter().until.as_deref(),
            Some("2026-10-22 12:00:00")
        );
        let search = flags(HistoryFlags {
            search: Some("LEA"),
            ..Default::default()
        });
        assert!(search.matches(&tick));
        assert!(!search.matches(&run("success", Some("load"), None, "2026-10-22 11:00:00")));
        // `not_successful` is every status of a run that ended without succeeding.
        let bad = flags(HistoryFlags {
            status: Some("not_successful,failed"),
            ..Default::default()
        });
        assert_eq!(bad.statuses, ["failed", "cancelled", "interrupted"]);
        assert_eq!(
            bad.query_pairs(),
            vec![("status", "not_successful,failed".to_string())]
        );
        // A run waiting in a server's queue is in progress.
        let at = "2026-10-22T11:30:00.250Z";
        let api = Some(TriggeredBy::Api);
        assert!(hour.keeps_unrecorded("pending", api, at, &[]));
        assert!(!hour.keeps_unrecorded("pending", api, "2026-10-22T12:00:00Z", &[]));
        assert!(!hide.keeps_unrecorded("running", Some(TriggeredBy::Schedule), at, &[]));
        assert!(hide.keeps_unrecorded("running", None, at, &[]));
        assert!(!bad.keeps_unrecorded("pending", api, at, &[]));
        assert!(bad.keeps_unrecorded("failed", api, at, &[]));
        assert!(search.keeps_unrecorded("running", api, at, &["clean"]));
        assert!(!search.keeps_unrecorded("running", api, at, &["load"]));
        let live = flags(HistoryFlags {
            status: Some("running"),
            ..Default::default()
        });
        assert!(!live.keeps_unrecorded("pending", api, at, &[]));
        assert!(!live.keeps_unrecorded("success", api, at, &[]));
        // `--target` narrows the database's read by the node's name without its key.
        let keyed = flags(HistoryFlags {
            target: Some("scored[day=1]"),
            ..Default::default()
        });
        assert_eq!(keyed.run_filter().names.as_deref(), Some("scored"));
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
