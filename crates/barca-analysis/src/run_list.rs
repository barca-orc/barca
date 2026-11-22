//! A run as a list shows it (#959): the words of one row of `GET /runs` and of
//! `barca history --json` ([`RunLine`]), what a run in progress is working on now
//! ([`RunNow`], with `GET /runs?status=running` and `barca history --status running`), and the
//! runs of the last day hour by hour ([`RunActivity`], with `GET /runs` and
//! `barca history --activity`; #991). Beside [`crate::run_explanation`], which
//! explains one run in full: both are computed here so the web UI and the CLI only render
//! them (`specs/behavior/health.md`, "One computation").
//!
//! This reads what a run recorded and writes nothing.

use crate::rollup::headline;
use crate::run_explanation::count;
pub use crate::run_explanation::{target_ids, target_line};
use barca_compile::targets::short_name;
use barca_model::ir::NodeGroup;
use barca_model::trigger::TriggeredBy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// One run in the words of a list row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunLine {
    /// What went wrong, in one line: `stopped at orders · KeyError: 'region'`, or for a
    /// partitioned node `demand_forecast: 7 keys failed · BatchKeyFailed: …`. `null` for a
    /// run with nothing wrong to say.
    pub failure: Option<String>,
    /// The same in full: the run's own error, or `<node id>: <error>` of its first failed
    /// step. `null` with `failure`.
    pub failure_detail: Option<String>,
    /// The run's steps by outcome: `7 ok · 1 failed · 0 cached`, or `3 started · 0 cached` for
    /// a run that did not finish. Steps are keys: a partitioned node counts one per key.
    pub steps: String,
    /// What `steps` counts, with what was planned and where the run stopped:
    /// `Steps are keys: a partitioned node counts one per key · 12 steps planned · stopped at
    /// billing.py:orders`.
    pub steps_detail: String,
    /// The run's targets in a few words ([`target_line`]): `all assets`, `billing.py:orders`,
    /// `billing.py:orders +21 more`. The full list is `target` (and `explanation.targets` of
    /// one run).
    #[serde(default)]
    pub target: String,
    /// How many targets the run named; 0 for a run of everything.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub target_count: u64,
}

/// Where a node is in a run in flight (#1042): the one distinction every count and cell of
/// "running" reads. Only `running` is counted as running anywhere; a `waiting` or `queued`
/// node keeps its own state word, with the run as a note beside it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RunningStep {
    /// At least one worker is executing a step of this node.
    Running,
    /// A run that is executing will still reach it: it waits for its upstream steps.
    Waiting,
    /// It is a node of a run that waits in the queue for a run slot: nothing executes it.
    Queued,
}

impl RunningStep {
    /// The words every surface prints for it: `running`, `in this run`, `in a queued run`.
    pub fn label(self) -> &'static str {
        match self {
            RunningStep::Running => "running",
            RunningStep::Waiting => "in this run",
            RunningStep::Queued => "in a queued run",
        }
    }
}

/// The run slots in use, in words (`GET /queue`'s `slots`, the first line of `barca queue`):
/// `1 of 4 running`, and when a run someone asked for executes in the extra slot past
/// `max_runs` (a requested run never waits behind scheduled work), `1 of 1 running, plus 1
/// requested run`.
pub fn queue_slots_line(running: usize, max_runs: usize) -> String {
    let extra = running.saturating_sub(max_runs);
    if extra == 0 {
        return format!("{running} of {max_runs} running");
    }
    format!(
        "{max_runs} of {max_runs} running, plus {extra} requested {}",
        if extra == 1 { "run" } else { "runs" }
    )
}

/// What [`run_line`] reads of a run.
#[derive(Clone, Copy, Debug)]
pub struct RunLineFacts<'a> {
    pub status: &'a str,
    pub steps_total: Option<i64>,
    pub steps_executed: i64,
    pub steps_cached: i64,
    /// Recorded steps that failed, each key counted; `None` when not recorded.
    pub steps_failed: Option<i64>,
    /// The run's own error (a run a server holds that failed outside a step).
    pub error: Option<&'a str>,
    /// The first failed step: its node id (with its key) and its error.
    pub first_failure: Option<(&'a str, &'a str)>,
    /// The targets as given, comma-separated; `None` for a run of everything.
    pub target: Option<&'a str>,
}

impl<'a> From<&'a barca_db::db::RunRecord> for RunLineFacts<'a> {
    fn from(r: &'a barca_db::db::RunRecord) -> Self {
        RunLineFacts {
            status: &r.status,
            steps_total: r.steps_total,
            steps_executed: r.steps_executed,
            steps_cached: r.steps_cached,
            steps_failed: r.steps_failed,
            error: r.reason.as_deref(),
            first_failure: r
                .failure
                .as_ref()
                .map(|f| (f.node.as_str(), f.error.as_str())),
            target: r.target.as_deref(),
        }
    }
}

/// A run that ended and did not succeed: failed, errored, cancelled or interrupted.
fn not_successful(status: &str) -> bool {
    matches!(status, "failed" | "error" | "cancelled" | "interrupted")
}

/// A step's short name, with its partition key: `pipeline.py:a[day=1]` is `a[day=1]`.
fn step_name(node_id: &str) -> String {
    match node_id.find('[') {
        Some(key) => format!("{}{}", short_name(node_id), &node_id[key..]),
        None => short_name(node_id).to_string(),
    }
}

fn line_of(error: &str) -> String {
    headline(error).unwrap_or_default()
}

/// The row's failure line and its full text.
fn failure(run: RunLineFacts<'_>) -> (Option<String>, Option<String>) {
    let (text, detail) = match (run.error, run.first_failure) {
        (Some(error), _) => (line_of(error), error.to_string()),
        _ if !not_successful(run.status) => return (None, None),
        (None, Some((node, error))) => (
            format!("{}: {}", step_name(node), line_of(error)),
            format!("{node}: {error}"),
        ),
        (None, None) if run.status == "failed" => {
            let none = "No failed step recorded: open the run".to_string();
            (none.clone(), none)
        }
        (None, None) => return (None, None),
    };
    let Some((node, step_error)) = run.first_failure else {
        return (Some(text), Some(detail));
    };
    let why = line_of(run.error.unwrap_or(step_error));
    let line = match node.find('[') {
        None => format!("stopped at {} · {why}", step_name(node)),
        Some(_) => {
            let failed = run.steps_failed.unwrap_or(1).max(1) as u64;
            format!(
                "{}: {} {} failed · {why}",
                short_name(node),
                count(failed),
                if failed == 1 { "key" } else { "keys" }
            )
        }
    };
    (Some(line), Some(detail))
}

/// The words of `run`'s row in a list of runs.
pub fn run_line(run: RunLineFacts<'_>) -> RunLine {
    // Counted where a run's panel counts them ([`crate::run_explanation::step_counts`]), so a
    // row and the panel it opens say the same numbers.
    let counts = crate::run_explanation::step_counts(crate::run_explanation::StepFacts {
        status: run.status,
        steps_total: run.steps_total,
        steps_executed: run.steps_executed,
        steps_cached: run.steps_cached,
        steps_failed: run.steps_failed.unwrap_or(0).max(0) as u64,
    });
    let steps = counts.outcome;
    let mut detail = vec![crate::run_explanation::STEP_UNITS.to_string()];
    if run.steps_total.is_some() {
        detail.push(counts.planned);
    }
    if let Some((node, _)) = run.first_failure {
        detail.push(format!("stopped at {node}"));
    }
    let (failure, failure_detail) = failure(run);
    RunLine {
        failure,
        failure_detail,
        steps,
        steps_detail: detail.join(" · "),
        target: target_line(run.target),
        target_count: target_ids(run.target).len() as u64,
    }
}

/// A node a run in progress is working on, as [`run_now`] reads it.
#[derive(Clone, Copy, Debug)]
pub struct ActiveNodeFacts<'a> {
    pub node_id: &'a str,
    /// Its step is executing; otherwise it waits (for a run slot or for its upstream steps).
    pub running: bool,
    /// For a partitioned node: keys with a result, and every key.
    pub keys: Option<(usize, usize)>,
}

/// A node whose step is executing now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActiveNode {
    pub node_id: String,
    /// `3,120 of 5,000 keys`; `null` for an unpartitioned node.
    pub keys: Option<String>,
    /// 0 to 1 of its keys done; `null` for an unpartitioned node.
    pub share: Option<f64>,
}

/// Keys done of the keys of the partitioned nodes in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeysDone {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub done: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
}

/// The nodes in flight of one group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GroupActivity {
    /// The group's id (`group:<file>:<name>`).
    pub id: String,
    /// The group's name.
    pub group: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub running: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queued: usize,
}

/// What a run in progress is working on now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunNow {
    /// The nodes executing, the one furthest from done first: what holds the run up.
    pub running: Vec<ActiveNode>,
    /// How many nodes the run has not started executing: they wait for their upstream steps,
    /// or, for a run in the queue, for its run slot.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queued: usize,
    /// Keys with a result of the partitioned nodes in flight; `null` when none is partitioned.
    pub keys: Option<KeysDone>,
    /// The groups of the nodes in flight, busiest first; empty without groups. A node is
    /// counted under the group the rollup puts it in ([`crate::rollup::node_groups`]).
    pub groups: Vec<GroupActivity>,
    /// The same in one line, as `barca history --status running` prints it under the run:
    /// `executing 3: orders, clean, report · 12 waiting · 240/900 keys`. The first
    /// [`NOW_NAMES`] nodes are named, by their short names.
    #[serde(default)]
    pub line: String,
}

/// How many executing nodes [`RunNow::line`] names before `+N more`.
pub const NOW_NAMES: usize = 5;

/// [`RunNow::line`].
fn now_line(running: &[ActiveNode], queued: usize, keys: Option<KeysDone>) -> String {
    let mut parts = Vec::new();
    if !running.is_empty() {
        let mut names: Vec<String> = running
            .iter()
            .take(NOW_NAMES)
            .map(|n| step_name(&n.node_id))
            .collect();
        if running.len() > NOW_NAMES {
            names.push(format!(
                "+{} more",
                count((running.len() - NOW_NAMES) as u64)
            ));
        }
        parts.push(format!(
            "executing {}: {}",
            count(running.len() as u64),
            names.join(", ")
        ));
    }
    if queued > 0 || running.is_empty() {
        parts.push(format!("{} waiting", count(queued as u64)));
    }
    if let Some(keys) = keys {
        parts.push(format!(
            "{}/{} keys",
            count(keys.done as u64),
            count(keys.total as u64)
        ));
    }
    parts.join(" · ")
}

/// What a run in progress is working on, from the nodes it has in flight.
pub fn run_now(active: &[ActiveNodeFacts<'_>], groups: &[NodeGroup]) -> RunNow {
    let mut done = 0;
    let mut total = 0;
    for node in active {
        if let Some((d, t)) = node.keys {
            done += d;
            total += t;
        }
    }
    let mut running: Vec<ActiveNode> = active
        .iter()
        .filter(|a| a.running)
        .map(|a| ActiveNode {
            node_id: a.node_id.to_string(),
            keys: a.keys.map(|(d, t)| {
                format!(
                    "{} of {} {}",
                    count(d as u64),
                    count(t as u64),
                    if t == 1 { "key" } else { "keys" }
                )
            }),
            share: a
                .keys
                .filter(|(_, t)| *t > 0)
                .map(|(d, t)| d as f64 / t as f64),
        })
        .collect();
    running.sort_by(|a, b| {
        a.share
            .unwrap_or(1.0)
            .total_cmp(&b.share.unwrap_or(1.0))
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    let group_of: HashMap<&str, &NodeGroup> = crate::rollup::node_groups(groups);
    let mut by_group: BTreeMap<&str, GroupActivity> = BTreeMap::new();
    for node in active {
        let Some(group) = group_of.get(node.node_id) else {
            continue;
        };
        let entry = by_group.entry(&group.id).or_insert_with(|| GroupActivity {
            id: group.id.clone(),
            group: group.name.clone(),
            running: 0,
            queued: 0,
        });
        if node.running {
            entry.running += 1;
        } else {
            entry.queued += 1;
        }
    }
    let mut groups: Vec<GroupActivity> = by_group.into_values().collect();
    groups.sort_by(|a, b| {
        (b.running + b.queued)
            .cmp(&(a.running + a.queued))
            .then_with(|| a.group.cmp(&b.group))
            .then_with(|| a.id.cmp(&b.id))
    });
    let queued = active.iter().filter(|a| !a.running).count();
    let keys = (total > 0).then_some(KeysDone { done, total });
    RunNow {
        line: now_line(&running, queued, keys),
        running,
        queued,
        keys,
        groups,
    }
}

/// Whether a run this process cannot follow (a `barca get` or `barca run` on this machine,
/// which streams its steps to nobody) covers `node`: every asset and sensor for a run of
/// everything (and the tasks for `barca run`), else its targets. Its upstream steps report
/// nothing, so only the targets count.
pub fn unfollowed_run_covers(
    command: &str,
    targets: &[String],
    node: &crate::views::NodeStatus,
) -> bool {
    if targets.is_empty() {
        command != "get" || node.kind != "task"
    } else {
        targets.contains(&node.id)
    }
}

/// Whether such a run is past `node`: it recorded a result of it since it started
/// (`started`: `YYYY-MM-DD HH:MM:SS`, UTC), and for a partitioned node every key has one.
pub fn unfollowed_run_finished(started: &str, node: &crate::views::NodeStatus) -> bool {
    node.last_materialization
        .as_ref()
        .is_some_and(|m| !started.is_empty() && m.created_at.as_str() >= started)
        && (!node.partitioned || node.partitions.as_ref().is_none_or(|p| p.cached >= p.total))
}

/// What a run this process cannot follow is working on: the nodes it covers that it is not
/// past ([`unfollowed_run_covers`], [`unfollowed_run_finished`]). Without worker activity the run is visible, but no node can be confirmed executing.
pub fn unfollowed_run_now(
    command: &str,
    targets: &[String],
    started: &str,
    nodes: &[crate::views::NodeStatus],
    groups: &[NodeGroup],
) -> RunNow {
    let active: Vec<ActiveNodeFacts<'_>> = nodes
        .iter()
        .filter(|n| unfollowed_run_covers(command, targets, n))
        .filter(|n| !unfollowed_run_finished(started, n))
        .map(|n| ActiveNodeFacts {
            node_id: &n.id,
            running: false,
            keys: n
                .partitions
                .as_ref()
                .filter(|_| n.partitioned)
                .map(|p| (p.cached.min(p.total), p.total)),
        })
        .collect();
    run_now(&active, groups)
}

// ─── the activity of the last day ───────────────────────────────────────────

/// How many hours [`RunActivity`] covers.
pub const ACTIVITY_HOURS: usize = 24;

/// Runs by status: the statuses of `barca history --status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StatusCounts {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub success: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub pending: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cancelled: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub interrupted: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub running: usize,
}

/// Runs by what started them: the values of `barca history --triggered-by`, and `unknown` for
/// runs recorded by a barca that did not record it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TriggerCounts {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub schedule: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub api: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub manual: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub client: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub unknown: usize,
}

/// What most of an hour's unsuccessful runs say went wrong.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LeadingFailure {
    /// The first failed step and its error, `orders: KeyError: 'region'` (a partitioned
    /// node's keys under its name), or how the runs ended when no step failed: `cancelled`.
    pub text: String,
    /// How many of the hour's runs it is said of.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub runs: usize,
}

/// The runs started in one hour.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActivityHour {
    /// The hour's start, RFC 3339 in UTC: `2026-10-22T08:00:00Z`. Hours are hours of UTC; a
    /// reader shows them in its own zone.
    pub at: String,
    /// The hour's end, the next one's start: with `at`, the `since` and `until` that list its
    /// runs (`barca history --since AT --until UNTIL`).
    #[serde(default)]
    pub until: String,
    /// Every run started in the hour.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub runs: usize,
    /// Of `runs`, the ones that succeeded.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub ok: usize,
    /// Of `runs`, the ones that ended otherwise: failed, error, cancelled or interrupted.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub not_successful: usize,
    /// Of `runs`, the ones still running.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub in_progress: usize,
    pub by_status: StatusCounts,
    pub by_trigger: TriggerCounts,
    /// The median time its finished runs took, in seconds; `null` when none finished.
    #[serde(default)]
    pub median_seconds: Option<f64>,
    /// What most of its unsuccessful runs say went wrong (the latest on a tie); `null` when
    /// none ended that way.
    #[serde(default)]
    pub failure: Option<LeadingFailure>,
}

impl ActivityHour {
    fn add(&mut self, status: &str, trigger: Option<TriggeredBy>, runs: usize) {
        self.runs += runs;
        let by_status = &mut self.by_status;
        match status {
            "success" => {
                by_status.success += runs;
                self.ok += runs;
            }
            "pending" => {
                by_status.pending += runs;
                self.in_progress += runs;
            }
            "running" => {
                by_status.running += runs;
                self.in_progress += runs;
            }
            other => {
                self.not_successful += runs;
                match other {
                    "failed" => by_status.failed += runs,
                    "cancelled" => by_status.cancelled += runs,
                    "interrupted" => by_status.interrupted += runs,
                    // `error`, and a status this barca does not know.
                    _ => by_status.failed += runs,
                }
            }
        }
        let by_trigger = &mut self.by_trigger;
        *match trigger {
            Some(TriggeredBy::Schedule) => &mut by_trigger.schedule,
            Some(TriggeredBy::Api) => &mut by_trigger.api,
            Some(TriggeredBy::Manual) => &mut by_trigger.manual,
            Some(TriggeredBy::Client) => &mut by_trigger.client,
            None => &mut by_trigger.unknown,
        } += runs;
    }
}

/// The runs started in the last [`ACTIVITY_HOURS`] hours, hour by hour: the activity strip of
/// the Runs page, `activity` of `GET /runs` and `barca history --activity`. Counted by the
/// database over every run of the window ([`crate::db::run_activity`]), never over the runs a
/// list happens to hold.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunActivity {
    /// The window's start: the start of the hour 23 hours before the current one (RFC 3339,
    /// UTC).
    pub since: String,
    /// The window's end: the end of the current hour, which is still filling.
    pub until: String,
    /// The triggers counted (`--triggered-by`); empty for every run, also the ones that
    /// recorded no trigger.
    pub triggered_by: Vec<TriggeredBy>,
    /// The triggers left out (`--not-triggered-by`); a run that recorded no trigger is
    /// counted.
    #[serde(default)]
    pub not_triggered_by: Vec<TriggeredBy>,
    /// The statuses counted (`--status`); empty for every status.
    #[serde(default)]
    pub statuses: Vec<String>,
    /// The text the runs counted hold in their run id, target or job (`--search`).
    #[serde(default)]
    pub search: Option<String>,
    /// One entry per hour, oldest first, the hours without runs included.
    pub hours: Vec<ActivityHour>,
    /// The window as a whole; its `at` is `since`.
    pub total: ActivityHour,
    /// With `not_triggered_by`: the window's runs it left out and the other filters keep, so
    /// what is hidden is said (`1,438 schedule runs, 3 not successful`). `null` without it.
    #[serde(default)]
    pub excluded: Option<ActivityHour>,
    /// The window in one line: `1,440 runs in the last 24 hours · 3 not successful · 1 in
    /// progress · median 1.2s · 1,400 schedule, 30 api, 10 manual`.
    pub summary: String,
}

/// What of a run filter the activity keeps: everything but its time window, which is the
/// activity's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityFilter {
    pub statuses: Vec<String>,
    pub triggered_by: Vec<TriggeredBy>,
    pub not_triggered_by: Vec<TriggeredBy>,
    /// Applied by the database ([`barca_db::db::run_activity`]); said back in the answer.
    pub search: Option<String>,
}

impl ActivityFilter {
    pub fn of(filter: &crate::history_filter::HistoryFilter) -> Self {
        ActivityFilter {
            statuses: filter.statuses.clone(),
            triggered_by: filter.triggered_by.clone(),
            not_triggered_by: filter.not_triggered_by.clone(),
            search: filter.search.clone(),
        }
    }

    fn status(&self, status: &str) -> bool {
        self.statuses.is_empty() || self.statuses.iter().any(|s| s == status)
    }

    fn included(&self, trigger: Option<TriggeredBy>) -> bool {
        self.triggered_by.is_empty() || trigger.is_some_and(|t| self.triggered_by.contains(&t))
    }

    fn excluded(&self, trigger: Option<TriggeredBy>) -> bool {
        trigger.is_some_and(|t| self.not_triggered_by.contains(&t))
    }
}

/// The start of `now`'s hour, [`ACTIVITY_HOURS`] - 1 hours earlier: where the window starts.
pub fn activity_since(now: chrono::DateTime<chrono::Utc>) -> chrono::NaiveDateTime {
    use chrono::Timelike;
    let hour = now
        .naive_utc()
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or_else(|| now.naive_utc());
    hour - chrono::Duration::hours(ACTIVITY_HOURS as i64 - 1)
}

/// A length of time as a run list writes it: `1.2s`, `3.4m`, `1.5h`.
pub fn seconds_text(seconds: f64) -> String {
    match seconds.max(0.0) {
        s if s < 60.0 => format!("{s:.1}s"),
        s if s < 3600.0 => format!("{:.1}m", s / 60.0),
        s => format!("{:.1}h", s / 3600.0),
    }
}

/// The median of `times`; `None` when empty.
fn median(times: &mut [f64]) -> Option<f64> {
    if times.is_empty() {
        return None;
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let mid = times.len() / 2;
    Some(if times.len() % 2 == 1 {
        times[mid]
    } else {
        (times[mid - 1] + times[mid]) / 2.0
    })
}

/// What an unsuccessful run says went wrong, as [`LeadingFailure::text`] groups runs by it.
fn failure_text(run: &barca_db::db::ActivityFailure) -> String {
    match &run.failure {
        Some(f) => format!("{}: {}", short_name(&f.node), line_of(&f.error)),
        None if run.status == "failed" => "failed: no failed step recorded".to_string(),
        None => run.status.clone(),
    }
}

/// What most of `texts` say (in the order the runs started): the latest on a tie.
fn leading(texts: &[String]) -> Option<LeadingFailure> {
    let mut seen: Vec<(&str, usize, usize)> = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        match seen.iter_mut().find(|(t, _, _)| *t == text.as_str()) {
            Some(entry) => {
                entry.1 += 1;
                entry.2 = i;
            }
            None => seen.push((text.as_str(), 1, i)),
        }
    }
    seen.into_iter()
        .max_by_key(|(_, runs, last)| (*runs, *last))
        .map(|(text, runs, _)| LeadingFailure {
            text: text.to_string(),
            runs,
        })
}

/// [`RunActivity`] at `now` from what the database read of the window, keeping the runs
/// `filter` keeps. A row outside the window is left out.
pub fn run_activity(
    facts: &barca_db::db::RunActivityFacts,
    filter: &ActivityFilter,
    now: chrono::DateTime<chrono::Utc>,
) -> RunActivity {
    let since = activity_since(now);
    let stamp = |t: chrono::NaiveDateTime| {
        t.and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let end = since + chrono::Duration::hours(ACTIVITY_HOURS as i64);
    let mut hours: Vec<ActivityHour> = (0..ACTIVITY_HOURS)
        .map(|i| ActivityHour {
            at: stamp(since + chrono::Duration::hours(i as i64)),
            until: stamp(since + chrono::Duration::hours(i as i64 + 1)),
            ..Default::default()
        })
        .collect();
    let whole = || ActivityHour {
        at: stamp(since),
        until: stamp(end),
        ..Default::default()
    };
    let mut total = whole();
    let mut excluded = whole();
    // The index of a row's hour in the window; `None` outside it.
    let index = |hour: &str| -> Option<usize> {
        let at =
            chrono::NaiveDateTime::parse_from_str(&format!("{hour}:00:00"), "%Y-%m-%d %H:%M:%S")
                .ok()?;
        let index = (at - since).num_hours();
        (at >= since && index < ACTIVITY_HOURS as i64).then_some(index as usize)
    };
    // Whether a run counts, and whether it is one `not_triggered_by` left out.
    let kept = |status: &str, trigger: Option<TriggeredBy>| -> Option<bool> {
        (filter.status(status) && filter.included(trigger)).then(|| !filter.excluded(trigger))
    };
    for row in &facts.counts {
        let (Some(i), Some(shown)) = (index(&row.hour), kept(&row.status, row.triggered_by)) else {
            continue;
        };
        if shown {
            hours[i].add(&row.status, row.triggered_by, row.runs);
            total.add(&row.status, row.triggered_by, row.runs);
        } else {
            excluded.add(&row.status, row.triggered_by, row.runs);
        }
    }
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); ACTIVITY_HOURS];
    let mut excluded_times = Vec::new();
    for run in &facts.durations {
        match (index(&run.hour), kept(&run.status, run.triggered_by)) {
            (Some(i), Some(true)) => times[i].push(run.seconds),
            (Some(_), Some(false)) => excluded_times.push(run.seconds),
            _ => {}
        }
    }
    let mut failures: Vec<Vec<String>> = vec![Vec::new(); ACTIVITY_HOURS];
    let mut excluded_failures = Vec::new();
    for run in &facts.failures {
        match (index(&run.hour), kept(&run.status, run.triggered_by)) {
            (Some(i), Some(true)) => failures[i].push(failure_text(run)),
            (Some(_), Some(false)) => excluded_failures.push(failure_text(run)),
            _ => {}
        }
    }
    excluded.median_seconds = median(&mut excluded_times);
    excluded.failure = leading(&excluded_failures);
    for (i, hour) in hours.iter_mut().enumerate() {
        hour.median_seconds = median(&mut times[i]);
        hour.failure = leading(&failures[i]);
    }
    total.median_seconds = median(&mut times.concat());
    total.failure = leading(&failures.concat());
    let mut summary = vec![format!(
        "{} {} in the last {ACTIVITY_HOURS} hours",
        count(total.runs as u64),
        if total.runs == 1 { "run" } else { "runs" }
    )];
    if total.not_successful > 0 {
        summary.push(format!(
            "{} not successful",
            count(total.not_successful as u64)
        ));
    }
    if total.in_progress > 0 {
        summary.push(format!("{} in progress", count(total.in_progress as u64)));
    }
    if let Some(median) = total.median_seconds {
        summary.push(format!("median {}", seconds_text(median)));
    }
    let by = total.by_trigger;
    let started: Vec<String> = [
        (by.schedule, "schedule"),
        (by.api, "api"),
        (by.manual, "manual"),
        (by.client, "client"),
        (by.unknown, "unknown"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, name)| format!("{} {name}", count(*n as u64)))
    .collect();
    if !started.is_empty() {
        summary.push(started.join(", "));
    }
    RunActivity {
        since: stamp(since),
        until: stamp(end),
        triggered_by: filter.triggered_by.clone(),
        not_triggered_by: filter.not_triggered_by.clone(),
        statuses: filter.statuses.clone(),
        search: filter.search.clone(),
        hours,
        total,
        excluded: (!filter.not_triggered_by.is_empty()).then_some(excluded),
        summary: summary.join(" · "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_slots_name_the_requested_run_using_the_extra_slot() {
        assert_eq!(queue_slots_line(1, 1), "1 of 1 running");
        assert_eq!(
            queue_slots_line(2, 1),
            "1 of 1 running, plus 1 requested run"
        );
    }

    fn facts(status: &'static str) -> RunLineFacts<'static> {
        RunLineFacts {
            status,
            steps_total: Some(12),
            steps_executed: 8,
            steps_cached: 3,
            steps_failed: Some(1),
            target: None,
            error: None,
            first_failure: None,
        }
    }

    #[test]
    fn a_successful_run_counts_its_steps_and_says_nothing_went_wrong() {
        let line = run_line(RunLineFacts {
            steps_failed: Some(0),
            ..facts("success")
        });
        assert_eq!(line.steps, "8 ok · 3 cached · 1 not reached");
        assert_eq!(
            line.steps_detail,
            "A step is one node, or one key of a partitioned node · 12 steps planned"
        );
        assert_eq!((line.failure, line.failure_detail), (None, None));
    }

    #[test]
    fn a_failed_run_says_where_it_stopped_and_why() {
        let line = run_line(RunLineFacts {
            first_failure: Some((
                "billing.py:orders",
                "KeyError: 'region'\n  File \"billing.py\"",
            )),
            ..facts("failed")
        });
        assert_eq!(line.steps, "7 ok · 1 failed · 3 cached · 1 not reached");
        assert_eq!(
            line.failure.as_deref(),
            Some("stopped at orders · KeyError: 'region'")
        );
        assert_eq!(
            line.failure_detail.as_deref(),
            Some("billing.py:orders: KeyError: 'region'\n  File \"billing.py\"")
        );
        assert!(line.steps_detail.ends_with("stopped at billing.py:orders"));
    }

    #[test]
    fn a_partitioned_failure_counts_its_keys() {
        let line = run_line(RunLineFacts {
            steps_failed: Some(7),
            first_failure: Some(("m.py:forecast[day=1]", "BatchKeyFailed: 7 keys")),
            ..facts("failed")
        });
        assert_eq!(
            line.failure.as_deref(),
            Some("forecast: 7 keys failed · BatchKeyFailed: 7 keys")
        );
    }

    #[test]
    fn a_run_that_did_not_finish_says_what_it_started() {
        let line = run_line(facts("cancelled"));
        assert_eq!(line.steps, "8 started · 3 cached · 1 not reached");
        assert_eq!(line.failure, None);
    }

    #[test]
    fn a_failed_run_with_no_failed_step_says_so_and_the_runs_own_error_leads() {
        let line = run_line(facts("failed"));
        assert_eq!(
            line.failure.as_deref(),
            Some("No failed step recorded: open the run")
        );
        let own = run_line(RunLineFacts {
            error: Some("worker pool exited\nmore"),
            ..facts("running")
        });
        assert_eq!(own.failure.as_deref(), Some("worker pool exited"));
        assert_eq!(
            own.failure_detail.as_deref(),
            Some("worker pool exited\nmore")
        );
    }

    fn group(id: &str, name: &str, members: &[&str]) -> NodeGroup {
        NodeGroup {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            members: members.iter().map(|m| m.to_string()).collect(),
            output: members.last().unwrap_or(&"").to_string(),
        }
    }

    #[test]
    fn nodes_in_flight_are_listed_furthest_from_done_first_and_counted_by_group() {
        let active = [
            ActiveNodeFacts {
                node_id: "p.py:a",
                running: true,
                keys: None,
            },
            ActiveNodeFacts {
                node_id: "p.py:b",
                running: true,
                keys: Some((3120, 5000)),
            },
            ActiveNodeFacts {
                node_id: "p.py:c",
                running: false,
                keys: Some((0, 1)),
            },
        ];
        // `p.py:b` is declared in two groups: it is counted once, under the first by id, as
        // the rollup places it.
        let groups = [
            group("group:p.py:ingest", "ingest", &["p.py:a", "p.py:b"]),
            group("group:p.py:model", "model", &["p.py:b", "p.py:c"]),
        ];
        let now = run_now(&active, &groups);
        assert_eq!(
            now.running
                .iter()
                .map(|n| (n.node_id.as_str(), n.keys.as_deref()))
                .collect::<Vec<_>>(),
            vec![("p.py:b", Some("3,120 of 5,000 keys")), ("p.py:a", None)]
        );
        assert_eq!(now.queued, 1);
        assert_eq!(now.line, "executing 2: b, a · 1 waiting · 3,120/5,001 keys");
        assert_eq!(
            now.keys,
            Some(KeysDone {
                done: 3120,
                total: 5001
            })
        );
        assert_eq!(
            now.groups
                .iter()
                .map(|g| (g.group.as_str(), g.running, g.queued))
                .collect::<Vec<_>>(),
            vec![("ingest", 2, 0), ("model", 0, 1)]
        );
    }

    #[test]
    fn the_line_names_the_first_executing_nodes_and_counts_the_rest() {
        let ids: Vec<String> = (0..7).map(|i| format!("p.py:n{i}")).collect();
        let active: Vec<ActiveNodeFacts<'_>> = ids
            .iter()
            .map(|id| ActiveNodeFacts {
                node_id: id,
                running: true,
                keys: None,
            })
            .collect();
        assert_eq!(
            run_now(&active, &[]).line,
            "executing 7: n0, n1, n2, n3, n4, +2 more"
        );
        // Nothing executing yet: the run waits for a slot.
        let waiting = [ActiveNodeFacts {
            node_id: "p.py:a",
            running: false,
            keys: None,
        }];
        assert_eq!(run_now(&waiting, &[]).line, "1 waiting");
        assert_eq!(run_now(&[], &[]).line, "0 waiting");
    }

    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn counted(
        hour: &str,
        status: &str,
        triggered_by: Option<TriggeredBy>,
        runs: usize,
    ) -> barca_db::db::ActivityRow {
        barca_db::db::ActivityRow {
            hour: hour.into(),
            status: status.into(),
            triggered_by,
            runs,
        }
    }

    #[test]
    fn activity_is_24_utc_hours_ending_with_the_current_one() {
        use TriggeredBy::{Api, Manual, Schedule};
        // 14:25 UTC: the window is 15:00 the day before up to 15:00 today.
        let now = at("2026-10-22T14:25:10Z");
        assert_eq!(
            activity_since(now).to_string(),
            "2026-10-21 15:00:00",
            "what the database is asked for"
        );
        let counts = [
            // The hour before the window (a `T`-form row sorts after the window's start).
            counted("2026-10-21 14", "success", Some(Schedule), 9),
            counted("2026-10-21 15", "success", Some(Schedule), 60),
            counted("2026-10-21 15", "failed", Some(Schedule), 2),
            counted("2026-10-22 00", "cancelled", Some(Api), 1),
            counted("2026-10-22 14", "running", Some(Manual), 1),
            counted("2026-10-22 14", "interrupted", None, 1),
            counted("2026-10-22 14", "error", Some(Api), 1),
            // A clock ahead of this one: past the window.
            counted("2026-10-22 15", "success", Some(Schedule), 5),
        ];
        let facts = barca_db::db::RunActivityFacts {
            counts: counts.to_vec(),
            durations: [
                ("2026-10-21 15", 1.0),
                ("2026-10-21 15", 3.0),
                ("2026-10-22 00", 90.0),
            ]
            .iter()
            .map(|(hour, seconds)| barca_db::db::ActivityDuration {
                hour: hour.to_string(),
                status: "success".into(),
                triggered_by: Some(Schedule),
                seconds: *seconds,
            })
            .collect(),
            failures: [
                (
                    "2026-10-21 15",
                    "failed",
                    Some(("p.py:orders[day=1]", "KeyError: 'region'")),
                ),
                (
                    "2026-10-21 15",
                    "failed",
                    Some(("p.py:orders[day=2]", "KeyError: 'region'")),
                ),
                ("2026-10-22 14", "interrupted", None),
                ("2026-10-22 14", "error", None),
            ]
            .iter()
            .map(|(hour, status, failure)| barca_db::db::ActivityFailure {
                hour: hour.to_string(),
                status: status.to_string(),
                triggered_by: Some(Schedule),
                failure: failure.map(|(node, error)| barca_db::db::RunFailure {
                    node: node.into(),
                    error: error.into(),
                }),
            })
            .collect(),
        };
        let activity = run_activity(&facts, &ActivityFilter::default(), now);
        assert_eq!(activity.since, "2026-10-21T15:00:00Z");
        assert_eq!(activity.until, "2026-10-22T15:00:00Z");
        assert_eq!(activity.hours.len(), ACTIVITY_HOURS);
        assert_eq!(activity.hours[0].at, "2026-10-21T15:00:00Z");
        assert_eq!(activity.hours[23].at, "2026-10-22T14:00:00Z");
        let first = &activity.hours[0];
        assert_eq!(
            (
                first.runs,
                first.ok,
                first.not_successful,
                first.in_progress
            ),
            (62, 60, 2, 0)
        );
        assert_eq!(first.by_trigger.schedule, 62);
        assert_eq!(first.until, "2026-10-21T16:00:00Z");
        // The median of its finished runs, and what most of its unsuccessful ones say: the
        // keys of one node count together.
        assert_eq!(first.median_seconds, Some(2.0));
        assert_eq!(
            first.failure,
            Some(LeadingFailure {
                text: "orders: KeyError: 'region'".into(),
                runs: 2
            })
        );
        // On a tie, the latest.
        assert_eq!(
            activity.hours[23].failure,
            Some(LeadingFailure {
                text: "error".into(),
                runs: 1
            })
        );
        assert_eq!(activity.hours[1].median_seconds, None);
        assert_eq!(activity.hours[1].failure, None);
        assert_eq!(activity.total.median_seconds, Some(3.0));
        // Midnight UTC is the tenth hour, whatever zone the server shows times in.
        assert_eq!(activity.hours[9].at, "2026-10-22T00:00:00Z");
        assert_eq!(activity.hours[9].by_status.cancelled, 1);
        let last = &activity.hours[23];
        assert_eq!(
            (last.runs, last.ok, last.not_successful, last.in_progress),
            (3, 0, 2, 1)
        );
        assert_eq!(
            last.by_status,
            StatusCounts {
                running: 1,
                interrupted: 1,
                failed: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            last.by_trigger,
            TriggerCounts {
                manual: 1,
                api: 1,
                unknown: 1,
                ..Default::default()
            }
        );
        assert_eq!(activity.total.runs, 66);
        assert_eq!(
            activity.hours.iter().map(|h| h.runs).sum::<usize>(),
            activity.total.runs
        );
        assert_eq!(
            activity.summary,
            "66 runs in the last 24 hours · 5 not successful · 1 in progress · median 3.0s · \
             62 schedule, 2 api, 1 manual, 1 unknown"
        );
        assert_eq!(activity.excluded, None);
        // By status: only those runs, their times and their failures.
        let failed = run_activity(
            &facts,
            &ActivityFilter {
                statuses: vec!["failed".into(), "error".into()],
                ..Default::default()
            },
            now,
        );
        assert_eq!(failed.total.runs, 3);
        assert_eq!(failed.total.median_seconds, None);
        assert_eq!(failed.statuses, ["failed", "error"]);
        assert_eq!(seconds_text(0.04), "0.0s");
        assert_eq!(seconds_text(90.0), "1.5m");
        assert_eq!(seconds_text(5400.0), "1.5h");
    }

    #[test]
    fn activity_without_the_schedules_runs_counts_the_rest() {
        use TriggeredBy::{Api, Client, Manual, Schedule};
        let now = at("2026-10-22T14:00:00Z");
        let counts = [
            counted("2026-10-22 13", "success", Some(Schedule), 1_440),
            counted("2026-10-22 13", "success", Some(Api), 1),
            counted("2026-10-22 13", "success", None, 4),
        ];
        let facts = barca_db::db::RunActivityFacts {
            counts: counts.to_vec(),
            ..Default::default()
        };
        let only = ActivityFilter {
            triggered_by: vec![Manual, Api, Client],
            ..Default::default()
        };
        let activity = run_activity(&facts, &only, now);
        assert_eq!(activity.triggered_by, vec![Manual, Api, Client]);
        assert_eq!(activity.total.runs, 1);
        assert_eq!(activity.summary, "1 run in the last 24 hours · 1 api");
        assert_eq!(
            run_activity(&facts, &ActivityFilter::default(), now).summary,
            "1,445 runs in the last 24 hours · 1,440 schedule, 1 api, 4 unknown"
        );
        assert_eq!(
            run_activity(&Default::default(), &ActivityFilter::default(), now).summary,
            "0 runs in the last 24 hours"
        );
        // #969: leaving the schedule's runs out keeps the runs that recorded no trigger, and
        // says what it left out.
        let hidden = run_activity(
            &facts,
            &ActivityFilter {
                not_triggered_by: vec![Schedule],
                ..Default::default()
            },
            now,
        );
        assert_eq!(hidden.total.runs, 5);
        assert_eq!(hidden.total.by_trigger.unknown, 4);
        assert_eq!(hidden.not_triggered_by, vec![Schedule]);
        let left_out = hidden.excluded.expect("what was left out");
        assert_eq!(
            (left_out.runs, left_out.by_trigger.schedule),
            (1_440, 1_440)
        );
        assert_eq!(
            hidden.summary,
            "5 runs in the last 24 hours · 1 api, 4 unknown"
        );
    }
}
