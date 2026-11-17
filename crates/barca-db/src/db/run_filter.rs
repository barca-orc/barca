//! Which recorded runs a list, its count and the activity read ([`RunFilter`]): the filters of
//! `barca history` and `GET /runs` as one SQL condition, so the database keeps the runs and a
//! `LIMIT` applies to the runs kept. Every part is answered from an index of `runs`:
//!
//! - a status: `idx_runs_status`;
//! - what started it, or did not: `idx_runs_trigger`;
//! - when it started: `idx_runs_started`;
//! - text in its run id, target or job: a scan of `idx_runs_search`, which holds the three
//!   columns, so the scan never reads the table's rows (a run's stored outcome is in them).

use super::*;
use barca_model::trigger::TriggeredBy;

/// Which runs to read. Empty: every run. The parts combine with AND.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunFilter {
    /// Any of these statuses. `interrupted` also names a `running` row whose owner is gone,
    /// and `running` leaves those out, as a run list reports them.
    pub statuses: Vec<String>,
    /// Started by any of these.
    pub triggers: Vec<TriggeredBy>,
    /// Started by none of these: a run that recorded no trigger is kept.
    pub not_triggers: Vec<TriggeredBy>,
    /// Started at or after this (`YYYY-MM-DD HH:MM:SS`, UTC).
    pub since: Option<String>,
    /// Started before this (`YYYY-MM-DD HH:MM:SS`, UTC).
    pub until: Option<String>,
    /// Its run id, target or job contains this text, whatever its case (ASCII).
    pub search: Option<String>,
    /// Its target or job contains this text: narrows the read for a caller that then checks
    /// each run itself (`barca history --target`).
    pub names: Option<String>,
}

/// A text as an SQL string literal.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\0', "").replace('\'', "''"))
}

/// `%text%` for `LIKE ... ESCAPE '\'`, as a literal: the text's own `%` and `_` match
/// themselves.
fn contains(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    quote(&format!("%{escaped}%"))
}

fn in_list<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values.map(quote).collect::<Vec<_>>().join(",")
}

/// What of `runs` and its indexes a database has: a snapshot an older barca wrote may lack
/// the newer columns, and is read without migrating it.
pub(super) struct RunsShape {
    pub trigger: bool,
    pub job: bool,
    pub search_index: bool,
}

impl RunsShape {
    pub(super) async fn of(conn: &libsql::Connection) -> Result<Self, BarcaError> {
        Ok(RunsShape {
            trigger: has_column(conn, "runs", "triggered_by").await?,
            job: has_column(conn, "runs", "job").await?,
            search_index: has_index(conn, "idx_runs_search").await?,
        })
    }
}

impl RunFilter {
    pub fn is_empty(&self) -> bool {
        *self == RunFilter::default()
    }

    /// The filter without its time window and its triggers: what the activity's query keeps
    /// (the window is its own, and the triggers are counted apart).
    pub fn for_activity(&self) -> RunFilter {
        RunFilter {
            search: self.search.clone(),
            names: self.names.clone(),
            ..Default::default()
        }
    }

    /// Runs started at or after `since`. `started_at` is `datetime('now')`
    /// (`2026-10-22 08:00:00`) or RFC 3339 (`2026-10-22T08:00:00Z`), and a `T` sorts after a
    /// space: the rows of the day written either way, then every later day.
    pub(super) fn since_sql(since: &str) -> String {
        let (date, time) = since.split_at(since.len().min(10));
        let time = time.trim_start();
        format!(
            "((started_at >= {} AND started_at < {}) OR started_at >= {})",
            quote(since),
            quote(&format!("{date}T")),
            quote(&format!("{date}T{time}")),
        )
    }

    /// The time window as ranges of `started_at`, each one a range of `idx_runs_started`.
    fn window_sql(&self) -> Option<String> {
        let split = |at: &str| {
            let (date, time) = at.split_at(at.len().min(10));
            (date.to_string(), time.trim_start().to_string())
        };
        match (&self.since, &self.until) {
            (None, None) => None,
            (Some(since), None) => Some(Self::since_sql(since)),
            (None, Some(until)) => {
                let (date, time) = split(until);
                Some(format!(
                    "(started_at < {} OR (started_at >= {} AND started_at < {}))",
                    quote(until),
                    quote(&format!("{date}T")),
                    quote(&format!("{date}T{time}")),
                ))
            }
            (Some(since), Some(until)) => {
                let (from_date, from_time) = split(since);
                let (to_date, to_time) = split(until);
                // The since day's rows written with a space, its rows written with a `T` and
                // every later day's, then the until day's rows written with a `T`.
                let from_t = format!("{from_date}T{from_time}");
                let day_t = format!("{from_date}T");
                let to_day_t = format!("{to_date}T");
                let to_t = format!("{to_date}T{to_time}");
                Some(format!(
                    "((started_at >= {} AND started_at < {}) \
                      OR (started_at >= {} AND started_at < {}) \
                      OR (started_at >= {} AND started_at < {}))",
                    quote(since),
                    quote(day_t.as_str().min(until.as_str())),
                    quote(&from_t),
                    quote(until),
                    quote(from_t.as_str().max(to_day_t.as_str())),
                    quote(&to_t),
                ))
            }
        }
    }

    /// The statuses as a condition; `interrupted` are the `running` rows whose owner is gone.
    fn status_sql(&self, interrupted: &[String]) -> Option<String> {
        if self.statuses.is_empty() {
            return None;
        }
        let gone = in_list(interrupted.iter().map(String::as_str));
        let mut any = Vec::new();
        let plain: Vec<&str> = self
            .statuses
            .iter()
            .map(String::as_str)
            .filter(|s| *s != "running")
            .collect();
        if !plain.is_empty() {
            any.push(format!("status IN ({})", in_list(plain.iter().copied())));
        }
        if self.statuses.iter().any(|s| s == "interrupted") && !interrupted.is_empty() {
            any.push(format!("run_id IN ({gone})"));
        }
        if self.statuses.iter().any(|s| s == "running") {
            any.push(if interrupted.is_empty() {
                "status = 'running'".to_string()
            } else {
                format!("(status = 'running' AND run_id NOT IN ({gone}))")
            });
        }
        Some(format!("({})", any.join(" OR ")))
    }

    /// The condition the filter is, over `runs`, and the index to scan when nothing else in
    /// it can use one. `None`: every run.
    pub(super) fn sql(&self, shape: &RunsShape, interrupted: &[String]) -> Option<RunsWhere> {
        let mut all = Vec::new();
        let mut indexed = false;
        if let Some(window) = self.window_sql() {
            all.push(window);
            indexed = true;
        }
        if let Some(status) = self.status_sql(interrupted) {
            all.push(status);
            indexed = true;
        }
        if !self.triggers.is_empty() {
            all.push(if shape.trigger {
                format!(
                    "triggered_by IN ({})",
                    in_list(self.triggers.iter().map(|t| t.as_str()))
                )
            } else {
                // No run of this database recorded what started it.
                "0".to_string()
            });
            indexed = true;
        }
        if !self.not_triggers.is_empty() && shape.trigger {
            let others: Vec<&str> = TriggeredBy::ALL
                .iter()
                .filter(|t| !self.not_triggers.contains(t))
                .map(|t| t.as_str())
                .collect();
            all.push(if others.is_empty() {
                "triggered_by IS NULL".to_string()
            } else {
                format!(
                    "(triggered_by IN ({}) OR triggered_by IS NULL)",
                    in_list(others.into_iter())
                )
            });
            indexed = true;
        }
        let job = if shape.job {
            " OR job LIKE {} ESCAPE '\\'"
        } else {
            ""
        };
        if let Some(text) = self.search.as_deref().filter(|t| !t.is_empty()) {
            let like = contains(text);
            all.push(format!(
                "(run_id LIKE {like} ESCAPE '\\' OR target LIKE {like} ESCAPE '\\'{})",
                job.replace("{}", &like)
            ));
        }
        if let Some(text) = self.names.as_deref().filter(|t| !t.is_empty()) {
            let like = contains(text);
            all.push(format!(
                "(target LIKE {like} ESCAPE '\\'{})",
                job.replace("{}", &like)
            ));
        }
        if all.is_empty() {
            return None;
        }
        Some(RunsWhere {
            condition: all.join(" AND "),
            index: if !indexed && shape.search_index {
                " INDEXED BY idx_runs_search"
            } else {
                ""
            },
        })
    }
}

/// A [`RunFilter`] as SQL: `FROM runs{index} WHERE {condition}`.
pub(super) struct RunsWhere {
    pub condition: String,
    pub index: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPE: RunsShape = RunsShape {
        trigger: true,
        job: true,
        search_index: true,
    };

    fn sql(filter: &RunFilter, interrupted: &[&str]) -> String {
        let gone: Vec<String> = interrupted.iter().map(|s| s.to_string()).collect();
        let w = filter.sql(&SHAPE, &gone).unwrap();
        format!("{}|{}", w.index.trim(), w.condition)
    }

    #[test]
    fn an_empty_filter_is_every_run() {
        assert!(RunFilter::default().sql(&SHAPE, &[]).is_none());
        assert!(RunFilter::default().is_empty());
    }

    #[test]
    fn leaving_a_trigger_out_keeps_the_runs_that_recorded_none() {
        let filter = RunFilter {
            not_triggers: vec![TriggeredBy::Schedule],
            ..Default::default()
        };
        assert_eq!(
            sql(&filter, &[]),
            "|(triggered_by IN ('api','client','manual') OR triggered_by IS NULL)"
        );
        // A database from before triggers were recorded: nothing to leave out.
        let old = RunsShape {
            trigger: false,
            job: true,
            search_index: false,
        };
        assert!(filter.sql(&old, &[]).is_none());
    }

    #[test]
    fn a_running_row_whose_owner_is_gone_is_interrupted_not_running() {
        let of = |statuses: &[&str]| RunFilter {
            statuses: statuses.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        assert_eq!(
            sql(&of(&["running"]), &["r'1"]),
            "|((status = 'running' AND run_id NOT IN ('r''1')))"
        );
        assert_eq!(
            sql(&of(&["failed", "interrupted"]), &["r1"]),
            "|(status IN ('failed','interrupted') OR run_id IN ('r1'))"
        );
        assert_eq!(sql(&of(&["running"]), &[]), "|(status = 'running')");
    }

    #[test]
    fn text_is_matched_literally_and_alone_scans_the_search_index() {
        let filter = RunFilter {
            search: Some("50%_o'k".into()),
            ..Default::default()
        };
        assert_eq!(
            sql(&filter, &[]),
            "INDEXED BY idx_runs_search|(run_id LIKE '%50\\%\\_o''k%' ESCAPE '\\' \
             OR target LIKE '%50\\%\\_o''k%' ESCAPE '\\' OR job LIKE '%50\\%\\_o''k%' ESCAPE '\\')"
        );
        // With a part an index answers, the planner picks.
        let narrowed = RunFilter {
            statuses: vec!["failed".into()],
            ..filter
        };
        assert!(sql(&narrowed, &[]).starts_with("|"));
    }

    #[test]
    fn a_window_covers_both_ways_a_start_is_written() {
        let hour = RunFilter {
            since: Some("2026-10-22 23:00:00".into()),
            until: Some("2026-10-23 00:00:00".into()),
            ..Default::default()
        };
        assert_eq!(
            sql(&hour, &[]),
            "|((started_at >= '2026-10-22 23:00:00' AND started_at < '2026-10-22T') \
             OR (started_at >= '2026-10-22T23:00:00' AND started_at < '2026-10-23 00:00:00') \
             OR (started_at >= '2026-10-23T' AND started_at < '2026-10-23T00:00:00'))"
        );
    }
}
