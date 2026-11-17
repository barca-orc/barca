//! A run that explains itself, in words (#956): what `GET /runs/{run_id}` and
//! `barca history RUN_ID` say about why a run executed the steps it did, who asked for it, how
//! many of its attempts were retries, why it was cancelled, what became of its targets and, while
//! it runs, how far it is. One implementation: the CLI prints it and the web UI renders it, and
//! neither derives it again.
//!
//! It is derived from what the run recorded ([`crate::run_explain::PlanReasons`],
//! [`crate::run_explain::Cancellation`], its counters and its per-node summary); nothing here is
//! recorded, so the wording can change without a migration. Experimental.

use crate::db::{RunNodeSummary, RunRecord, RunStep};
use crate::outcome::{RunView, TriggeredBy};
use crate::run_explain::{Cancellation, CancelledBy, PlanReason, PlanReasons, ReasonCategory};
use crate::step_status::StepStatus;
use crate::targets::short_name;
use serde::{Deserialize, Serialize};

/// How many nodes [`RunExplanation::slowest`] lists.
pub const SLOWEST_SHOWN: usize = 5;

/// How many reasons the headline of [`WhyRan`] names before `and N more reasons`.
const HEADLINE_REASONS: usize = 3;

/// What a run says about itself. Experimental.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunExplanation {
    /// The run's targets, one each: its job's node ids, else the targets as given. Empty for a
    /// run of every asset.
    pub targets: Vec<String>,
    /// What started the run and, when the server knows, who: `api · token:ci-bot`.
    pub requested_by: String,
    /// Steps tried and how many of the attempts were retries: `29 steps, no retries`,
    /// `19,399 steps, 62 retries`. `null` before the run recorded an attempt.
    pub attempts: Option<String>,
    /// The run's steps by outcome: `7 ok · 1 failed · 2 skipped · 0 cached`, or
    /// `3 started · 0 cached` for a run that did not finish.
    pub steps: String,
    /// What became of each target, one line each: `publish_plan did not run: upstream
    /// demand_forecast failed 7 keys`. Empty for a run of every asset, or when the run's plan
    /// could not be rebuilt.
    pub target_outcomes: Vec<String>,
    /// For a cancelled run: why, in words.
    pub cancellation: Option<CancellationNotice>,
    /// Why the run executed what it did, steps grouped by the reason they ran.
    pub why: WhyRan,
    /// For a run in progress: how far it is.
    pub progress: Option<RunProgress>,
    /// The nodes that took longest, slowest first, at most [`SLOWEST_SHOWN`].
    pub slowest: Vec<SlowNode>,
}

/// Why a cancelled run was cancelled, as a sentence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CancellationNotice {
    pub headline: String,
    pub detail: Option<String>,
    /// A run to open: the one that was doing the same work, else the first it waited behind.
    pub run: Option<String>,
}

/// Why a run executed what it did: its steps grouped by the reason they ran.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WhyRan {
    /// `29 steps ran: 27 sensors (always run), 2 never run`; with no `groups`, what there is to
    /// say instead (`Every step was served from cache.`).
    pub headline: String,
    /// By reason category, most steps first. Empty when the run decided to execute nothing, or
    /// recorded no reasons.
    pub groups: Vec<ReasonGroup>,
    /// How many `(category, subject)` reasons the run left out of its record.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub omitted: u64,
    /// The run is still deciding phases: these are the reasons so far.
    pub partial: bool,
}

/// The steps a run executed for one reason category.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ReasonGroup {
    pub category: ReasonCategory,
    /// The category in words: `code changed`, `sensors (always run)`.
    pub label: String,
    /// The steps executed for it. A step with several changed components counts under each of
    /// them, so this is at most the run's steps, and groups can add up to more.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps: u64,
    /// One row per subject (what changed, the refreshed node), or one row when the category
    /// says it all.
    pub rows: Vec<ReasonRow>,
}

/// One reason of a [`ReasonGroup`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ReasonRow {
    /// What changed, without the word the category already says (`billing.py:orders`, not
    /// `input billing.py:orders`); `null` when the category says it all.
    pub subject: Option<String>,
    /// `27 nodes` when each node ran one step, else `5,000 steps of 1 node`.
    pub counts: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub nodes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps: u64,
    /// Of the steps the run decided to execute, this reason's share (0 to 1).
    pub share: f64,
    /// The first node executed for it (a full node id), to link to.
    pub example: String,
    /// How to name that node beside the counts: `poll_inbox` when it is the only one,
    /// `e.g. poll_inbox` when it is one of several.
    pub example_text: String,
    /// The row in one line: `27 nodes, e.g. poll_inbox`, `29 nodes: helper billing.py:_n`.
    pub line: String,
}

/// How far a run in progress is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunProgress {
    /// Steps with a result (executed or cached).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub done: u64,
    /// The steps planned so far; `null` before the run recorded its plan.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub total: Option<u64>,
    /// 0 to 100, `null` without a total.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub percent: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub executed: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cached: u64,
    /// Seconds left at the rate the run has executed steps so far (cached steps take no time
    /// and are left out of the rate); `null` when it cannot be told yet.
    pub eta_seconds: Option<f64>,
    /// `eta_seconds` in words: `about 4m left`.
    pub eta: Option<String>,
}

/// A node and the time its steps took in one run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SlowNode {
    pub node_id: String,
    /// Summed over its keys, which may have overlapped.
    pub seconds: f64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub keys: u64,
}

/// What [`explain`] reads of a run: the same fields of a recorded run ([`RunRecord`]) and of
/// one `GET /runs/{run_id}` shows ([`RunView`]).
#[derive(Clone, Copy, Debug)]
pub struct RunFacts<'a> {
    pub status: &'a str,
    pub target: Option<&'a str>,
    pub job: Option<&'a str>,
    pub triggered_by: Option<TriggeredBy>,
    pub principal: Option<&'a str>,
    pub attempts: Option<i64>,
    pub retries: Option<i64>,
    pub steps_total: Option<i64>,
    pub steps_executed: i64,
    pub steps_cached: i64,
    /// As recorded (`YYYY-MM-DD HH:MM:SS`, UTC) or RFC 3339.
    pub started_at: &'a str,
    pub plan_reasons: Option<&'a PlanReasons>,
    pub cancellation: Option<&'a Cancellation>,
}

impl<'a> From<&'a RunRecord> for RunFacts<'a> {
    fn from(r: &'a RunRecord) -> Self {
        RunFacts {
            status: &r.status,
            target: r.target.as_deref(),
            job: r.job.as_deref(),
            triggered_by: r.triggered_by,
            principal: r.principal.as_deref(),
            attempts: r.attempts,
            retries: r.retries,
            steps_total: r.steps_total,
            steps_executed: r.steps_executed,
            steps_cached: r.steps_cached,
            started_at: &r.started_at,
            plan_reasons: r.plan_reasons.as_ref(),
            cancellation: r.cancellation.as_ref(),
        }
    }
}

impl<'a> From<&'a RunView> for RunFacts<'a> {
    fn from(r: &'a RunView) -> Self {
        RunFacts {
            status: &r.status,
            target: r.target.as_deref(),
            job: r.job.as_deref(),
            triggered_by: r.triggered_by,
            principal: r.principal.as_deref(),
            attempts: r.attempts,
            retries: r.retries,
            steps_total: r.steps_total,
            steps_executed: r.steps_executed,
            steps_cached: r.steps_cached,
            started_at: &r.started_at,
            plan_reasons: r.plan_reasons.as_ref(),
            cancellation: r.cancellation.as_ref(),
        }
    }
}

/// A run's explanation. `nodes` is its per-node summary (`steps_page.nodes`), `steps` the steps
/// listed, and `now` the time in unix seconds, which only a run in progress reads (its ETA).
pub fn explain(
    run: RunFacts<'_>,
    nodes: &[RunNodeSummary],
    steps: &[RunStep],
    now: f64,
) -> RunExplanation {
    let live = matches!(run.status, "pending" | "running");
    RunExplanation {
        targets: targets(run),
        requested_by: requested_by(run.triggered_by, run.principal),
        attempts: attempts_label(run.attempts, run.retries),
        steps: steps_label(run, nodes),
        target_outcomes: target_outcomes(run, nodes),
        cancellation: run.cancellation.map(cancellation_notice),
        why: why_ran(
            run.plan_reasons,
            live,
            run.steps_executed.max(0) as u64,
            // A run cancelled in the queue decided nothing: its cancellation says so.
            run.cancellation.is_some_and(|c| !c.started),
        ),
        progress: live.then(|| progress(run, now)),
        slowest: slowest_nodes(nodes, steps, SLOWEST_SHOWN),
    }
}

/// `n` with thousands separators: `19,461`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1 step`, `5,000 steps`.
fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

fn steps_of(n: u64) -> String {
    plural(n, "step", "steps")
}

fn nodes_of(n: u64) -> String {
    plural(n, "node", "nodes")
}

fn keys_of(n: u64) -> String {
    plural(n, "key", "keys")
}

/// The run's targets as a list: its job's node ids, else the targets as given.
fn targets(run: RunFacts<'_>) -> Vec<String> {
    let ids = match run.job {
        Some(job) if job != "all" => Some(job),
        Some(_) => None,
        None => run.target,
    };
    ids.map(|ids| {
        ids.split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != "all")
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// `api · token:ci-bot`: what started the run and, when the server knows, who.
fn requested_by(triggered_by: Option<TriggeredBy>, principal: Option<&str>) -> String {
    let what = triggered_by.map_or("unknown", TriggeredBy::as_str);
    match principal {
        Some(principal) => format!("{what} · {principal}"),
        None => what.to_string(),
    }
}

/// Steps tried, and the retries apart from them: a step's first attempt is not a retry, so a
/// run of 29 steps that retried nothing is `29 steps, no retries`, not 29 attempts. A run that
/// did not record its retries (before 0.43) says only its attempts.
fn attempts_label(attempts: Option<i64>, retries: Option<i64>) -> Option<String> {
    let attempts = attempts.filter(|n| *n > 0)? as u64;
    Some(match retries.map(|n| n.max(0) as u64) {
        None => plural(attempts, "attempt", "attempts"),
        Some(0) => format!("{}, no retries", steps_of(attempts)),
        Some(retries) => format!(
            "{}, {}",
            steps_of(attempts.saturating_sub(retries)),
            plural(retries, "retry", "retries")
        ),
    })
}

/// `7 ok · 1 failed · 2 skipped · 0 cached`. `executed` counts every step that ran, failed
/// ones included, so ok is `executed - failed`.
fn outcome_label(executed: u64, cached: u64, failed: u64, skipped: u64) -> String {
    let failed = failed.min(executed);
    let mut parts = vec![format!("{} ok", count(executed - failed))];
    if failed > 0 {
        parts.push(format!("{} failed", count(failed)));
    }
    if skipped > 0 {
        parts.push(format!("{} skipped", count(skipped)));
    }
    parts.push(format!("{} cached", count(cached)));
    parts.join(" · ")
}

/// A run's steps by outcome: failed keys and skipped nodes from its per-node summary. A run
/// that did not finish says what it started, since what it did not reach is not an outcome.
fn steps_label(run: RunFacts<'_>, nodes: &[RunNodeSummary]) -> String {
    let executed = run.steps_executed.max(0) as u64;
    let cached = run.steps_cached.max(0) as u64;
    if matches!(run.status, "cancelled" | "interrupted" | "error") {
        return format!("{} started · {} cached", count(executed), count(cached));
    }
    let failed: usize = nodes.iter().map(|n| n.failed).sum();
    let skipped = nodes
        .iter()
        .filter(|n| StepStatus::Skipped.is(n.status.as_deref()))
        .count();
    outcome_label(executed, cached, failed as u64, skipped as u64)
}

/// What became of each target, from the run's planned nodes.
fn target_outcomes(run: RunFacts<'_>, nodes: &[RunNodeSummary]) -> Vec<String> {
    if !nodes.iter().any(|n| n.status.is_some()) {
        return Vec::new();
    }
    targets(run)
        .iter()
        .filter_map(|id| {
            let name = short_name(id);
            let node = nodes
                .iter()
                .find(|n| n.node_id == *id)
                .or_else(|| nodes.iter().find(|n| short_name(&n.node_id) == name))?;
            let keys = node.keys as u64;
            let status = node.status.as_deref();
            Some(if StepStatus::Skipped.is(status) {
                match &node.blocked_by {
                    None => format!("{name} did not run: the run stopped before it"),
                    Some(blocked_by) => {
                        let failed = nodes
                            .iter()
                            .find(|n| n.node_id == *blocked_by)
                            .filter(|up| up.keys > 1)
                            .map(|up| format!(" {}", keys_of(up.failed as u64)))
                            .unwrap_or_default();
                        format!(
                            "{name} did not run: upstream {} failed{failed}",
                            short_name(blocked_by)
                        )
                    }
                }
            } else if StepStatus::Cached.is(status) {
                format!("{name} was cached: nothing to do")
            } else if node.failed > 0 && keys > 1 {
                format!(
                    "{name} failed: {} of {}",
                    count(node.failed as u64),
                    keys_of(keys)
                )
            } else if node.failed > 0 {
                format!("{name} failed")
            } else if keys > 1 {
                format!("{name} ran: {}", keys_of(keys))
            } else {
                format!("{name} ran")
            })
        })
        .collect()
}

/// Why a cancelled run was cancelled, as the run's panel and `barca history RUN_ID` say it.
pub fn cancellation_notice(c: &Cancellation) -> CancellationNotice {
    let how = match (c.by, &c.principal) {
        (CancelledBy::Request, Some(principal)) => format!("Cancelled by {principal}"),
        (CancelledBy::Request, None) => "Cancelled".to_string(),
        (CancelledBy::Shutdown, _) => "Cancelled by the server stopping".to_string(),
    };
    if c.started {
        return CancellationNotice {
            headline: format!("{how} while it ran."),
            detail: Some("Steps that finished before the cancel are cached.".to_string()),
            run: None,
        };
    }
    if let Some(same) = &c.same_as {
        return CancellationNotice {
            headline: format!("Not started: the same target was already running as run {same}."),
            detail: Some(format!(
                "{how} while it waited in the queue; it executed nothing."
            )),
            run: Some(same.clone()),
        };
    }
    let behind: Vec<String> = c
        .waiting_behind
        .iter()
        .map(|r| format!("run {r}"))
        .collect();
    CancellationNotice {
        headline: "Not started: cancelled while it waited in the queue.".to_string(),
        detail: Some(if behind.is_empty() {
            format!("{how}.")
        } else {
            format!("It waited behind {}. {how}.", behind.join(", "))
        }),
        run: c.waiting_behind.first().cloned(),
    }
}

/// A reason category in words, as a heading over the steps that ran for it.
pub fn reason_label(category: ReasonCategory) -> &'static str {
    match category {
        ReasonCategory::NeverRun => "never run",
        ReasonCategory::NewKeys => "new partition keys",
        ReasonCategory::Failed => "retrying a failed attempt",
        ReasonCategory::CodeChanged => "code changed",
        ReasonCategory::InputsChanged => "input changed",
        ReasonCategory::EnvChanged => "environment changed",
        ReasonCategory::Changed => "changed (what was not recorded)",
        ReasonCategory::Refresh => "named in --refresh",
        ReasonCategory::RefreshCascade => "downstream of a refresh",
        ReasonCategory::RefreshAll => "--refresh-all",
        ReasonCategory::ArtifactMissing => "artifact missing",
        ReasonCategory::Evicted => "artifact evicted",
        ReasonCategory::Task => "tasks (always run)",
        ReasonCategory::Sensor => "sensors (always run)",
        ReasonCategory::NotExamined => "not examined",
    }
}

/// `steps` steps of a category as a clause of the headline: `27 sensors (always run)`,
/// `5,000 new partition keys`, `2 never run`.
fn reason_clause(category: ReasonCategory, steps: u64) -> String {
    let one = steps == 1;
    match category {
        ReasonCategory::NewKeys if one => "1 new partition key".to_string(),
        ReasonCategory::Task if one => "1 task (always runs)".to_string(),
        ReasonCategory::Sensor if one => "1 sensor (always runs)".to_string(),
        ReasonCategory::Changed => format!("{} changed", count(steps)),
        _ => format!("{} {}", count(steps), reason_label(category)),
    }
}

/// What a reason's subject names, without the component word the category already says.
fn subject_text(reason: &PlanReason) -> Option<String> {
    let subject = reason.subject.as_deref()?;
    Some(
        match reason.category {
            // `input a.py:x` is `a.py:x`; `sensor` and `value` keep their word.
            ReasonCategory::InputsChanged => subject.strip_prefix("input ").unwrap_or(subject),
            ReasonCategory::EnvChanged => subject.strip_prefix("env ").unwrap_or(subject),
            _ => subject,
        }
        .to_string(),
    )
}

/// One reason as a row. Its counts name what was counted: nodes when each ran one step (27
/// sensors are `27 nodes`, never one sensor of 27 steps), else steps of nodes.
fn reason_row(reason: &PlanReason, run_steps: u64) -> ReasonRow {
    let counts = if reason.steps == reason.nodes {
        nodes_of(reason.nodes)
    } else {
        format!("{} of {}", steps_of(reason.steps), nodes_of(reason.nodes))
    };
    let name = short_name(&reason.example);
    let example_text = if reason.nodes > 1 {
        format!("e.g. {name}")
    } else {
        name.to_string()
    };
    let subject = subject_text(reason);
    let line = match &subject {
        Some(subject) => format!("{counts}: {subject}"),
        None if reason.nodes > 1 => format!("{counts}, {example_text}"),
        None => format!("{counts}: {example_text}"),
    };
    ReasonRow {
        subject,
        counts,
        nodes: reason.nodes,
        steps: reason.steps,
        share: (reason.steps as f64 / run_steps.max(1) as f64).min(1.0),
        example: reason.example.clone(),
        example_text,
        line,
    }
}

/// A run's recorded reasons grouped by category, most steps first. `live`: the run is still
/// deciding phases. `executed`: the steps it executed, for a run that recorded no reasons.
/// `never_started`: it was cancelled in the queue.
fn why_ran(plan: Option<&PlanReasons>, live: bool, executed: u64, never_started: bool) -> WhyRan {
    let Some(plan) = plan.filter(|p| p.steps > 0) else {
        let headline = if never_started {
            "It was cancelled before it decided what to run.".to_string()
        } else if live {
            "No step decided to run yet.".to_string()
        } else if executed > 0 {
            format!(
                "{} ran, but this run recorded no reasons (barca records them from 0.43).",
                steps_of(executed)
            )
        } else {
            "Every step was served from cache.".to_string()
        };
        return WhyRan {
            headline,
            groups: Vec::new(),
            omitted: 0,
            partial: live,
        };
    };
    let mut groups: Vec<ReasonGroup> = Vec::new();
    for reason in &plan.reasons {
        let row = reason_row(reason, plan.steps);
        match groups.iter_mut().find(|g| g.category == reason.category) {
            Some(group) => {
                group.steps = (group.steps + reason.steps).min(plan.steps);
                group.rows.push(row);
            }
            None => groups.push(ReasonGroup {
                category: reason.category,
                label: reason_label(reason.category).to_string(),
                steps: reason.steps.min(plan.steps),
                rows: vec![row],
            }),
        }
    }
    // Stable: equal groups keep the order the run recorded their first reason in.
    groups.sort_by_key(|g| std::cmp::Reverse(g.steps));
    let mut clauses: Vec<String> = groups
        .iter()
        .take(HEADLINE_REASONS)
        .map(|g| reason_clause(g.category, g.steps))
        .collect();
    let more = groups.len().saturating_sub(HEADLINE_REASONS);
    if more > 0 {
        clauses.push(format!(
            "and {}",
            plural(more as u64, "more reason", "more reasons")
        ));
    }
    let verb = if live { "decided to run so far" } else { "ran" };
    WhyRan {
        headline: format!("{} {verb}: {}", steps_of(plan.steps), clauses.join(", ")),
        groups,
        omitted: plan.omitted,
        partial: live,
    }
}

/// `about 4m left` for `seconds` to go.
fn left_label(seconds: f64) -> String {
    if seconds < 60.0 {
        return "under a minute left".to_string();
    }
    if seconds < 3600.0 {
        return format!("about {}m left", (seconds / 60.0).round() as u64);
    }
    let hours = (seconds / 3600.0).floor() as u64;
    let minutes = ((seconds % 3600.0) / 60.0).round() as u64;
    format!("about {hours}h {minutes}m left")
}

/// How far a run in progress is at `now` (unix seconds). The ETA is the steps left at the rate
/// it has executed steps so far.
fn progress(run: RunFacts<'_>, now: f64) -> RunProgress {
    let executed = run.steps_executed.max(0) as u64;
    let cached = run.steps_cached.max(0) as u64;
    let done = executed + cached;
    let total = run.steps_total.map(|n| n.max(0) as u64);
    let percent = total.filter(|t| *t > 0).map(|t| (done * 100 / t).min(100));
    let started = crate::time::parse_db_utc(run.started_at)
        .or_else(|| crate::time::parse_rfc3339(run.started_at));
    let elapsed = started.map_or(0.0, |s| (now - s as f64).max(0.0));
    let left = total.map_or(0, |t| t.saturating_sub(done));
    let eta_seconds = (executed > 0 && elapsed > 0.0 && left > 0)
        .then(|| left as f64 / (executed as f64 / elapsed));
    RunProgress {
        done,
        total,
        percent,
        executed,
        cached,
        eta_seconds,
        eta: eta_seconds.map(left_label),
    }
}

/// The nodes that took longest: by node over every step when the run has a per-node summary,
/// else the listed steps.
fn slowest_nodes(nodes: &[RunNodeSummary], steps: &[RunStep], n: usize) -> Vec<SlowNode> {
    let mut rows: Vec<SlowNode> = if nodes.is_empty() {
        steps
            .iter()
            .filter_map(|s| {
                Some(SlowNode {
                    node_id: s.node_id.clone(),
                    seconds: s.elapsed_seconds?,
                    keys: 1,
                })
            })
            .collect()
    } else {
        nodes
            .iter()
            .map(|s| SlowNode {
                node_id: s.node_id.clone(),
                seconds: s.elapsed_seconds,
                keys: s.keys as u64,
            })
            .collect()
    };
    rows.retain(|r| r.seconds > 0.0);
    rows.sort_by(|a, b| {
        b.seconds
            .total_cmp(&a.seconds)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    rows.truncate(n);
    rows
}

/// The explanation as the lines `barca history RUN_ID` prints after the run's counts: what
/// became of each target, then `why:` and each reason group with at most `rows` rows.
pub fn why_lines(why: &WhyRan, rows: usize) -> Vec<String> {
    let mut out = vec![format!("why: {}", why.headline)];
    for group in &why.groups {
        out.push(format!("  {}", group.label));
        for row in group.rows.iter().take(rows) {
            out.push(format!("    {}", row.line));
        }
        if group.rows.len() > rows {
            out.push(format!(
                "    and {} more (--json lists them)",
                group.rows.len() - rows
            ));
        }
    }
    if why.omitted > 0 {
        out.push(format!(
            "  not listed: {}",
            plural(why.omitted, "smaller reason", "smaller reasons")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1794830400.0;

    fn facts<'a>() -> RunFacts<'a> {
        RunFacts {
            status: "success",
            target: None,
            job: None,
            triggered_by: None,
            principal: None,
            attempts: None,
            retries: None,
            steps_total: None,
            steps_executed: 0,
            steps_cached: 0,
            started_at: "",
            plan_reasons: None,
            cancellation: None,
        }
    }

    fn reason(
        category: ReasonCategory,
        subject: Option<&str>,
        nodes: u64,
        steps: u64,
        example: &str,
    ) -> PlanReason {
        PlanReason {
            category,
            subject: subject.map(str::to_string),
            nodes,
            steps,
            example: example.to_string(),
        }
    }

    fn plan() -> PlanReasons {
        PlanReasons {
            nodes: 469,
            steps: 5_492,
            omitted: 2,
            reasons: vec![
                reason(
                    ReasonCategory::NewKeys,
                    None,
                    1,
                    5_000,
                    "billing.py:invoices_by_account",
                ),
                reason(
                    ReasonCategory::CodeChanged,
                    Some("helper billing.py:_n"),
                    29,
                    29,
                    "billing.py:dunning_05",
                ),
                reason(
                    ReasonCategory::InputsChanged,
                    Some("input billing.py:dunning_05"),
                    3,
                    3,
                    "billing.py:marts_01",
                ),
                reason(
                    ReasonCategory::CodeChanged,
                    Some("code"),
                    2,
                    2,
                    "crm.py:owners_00",
                ),
            ],
        }
    }

    fn node(id: &str, keys: usize, failed: usize, seconds: f64) -> RunNodeSummary {
        RunNodeSummary {
            node_id: id.into(),
            keys,
            failed,
            elapsed_seconds: seconds,
            calls: 0,
            status: None,
            blocked_by: None,
        }
    }

    fn planned(id: &str, status: &str, blocked_by: Option<&str>) -> RunNodeSummary {
        RunNodeSummary {
            status: Some(status.into()),
            blocked_by: blocked_by.map(str::to_string),
            ..node(id, 0, 0, 0.0)
        }
    }

    fn step(id: &str, seconds: f64) -> RunStep {
        RunStep {
            node_id: id.into(),
            status: "success".into(),
            elapsed_seconds: Some(seconds),
            ..Default::default()
        }
    }

    #[test]
    fn steps_are_grouped_by_the_reason_they_ran_most_steps_first() {
        let plan = plan();
        let why = why_ran(Some(&plan), false, 5_492, false);
        assert_eq!(
            why.headline,
            "5,492 steps ran: 5,000 new partition keys, 31 code changed, 3 input changed"
        );
        let categories: Vec<_> = why.groups.iter().map(|g| g.category).collect();
        assert_eq!(
            categories,
            [
                ReasonCategory::NewKeys,
                ReasonCategory::CodeChanged,
                ReasonCategory::InputsChanged
            ]
        );
        let subjects: Vec<_> = why.groups[1]
            .rows
            .iter()
            .map(|r| r.subject.as_deref())
            .collect();
        assert_eq!(subjects, [Some("helper billing.py:_n"), Some("code")]);
        // The input's word is the category's; the subject names the node.
        assert_eq!(
            why.groups[2].rows[0].subject.as_deref(),
            Some("billing.py:dunning_05")
        );
        let keys = &why.groups[0].rows[0];
        assert_eq!(keys.counts, "5,000 steps of 1 node");
        assert_eq!(keys.line, "5,000 steps of 1 node: invoices_by_account");
        assert!((keys.share - 5_000.0 / 5_492.0).abs() < 1e-9);
        assert_eq!(why.groups[1].rows[0].line, "29 nodes: helper billing.py:_n");
        assert_eq!(why.omitted, 2);
        assert!(!why.partial);
    }

    /// The audit of 2026-11-17: 29 steps, 27 of them sensors, read as `29 steps of 29 nodes
    /// were not cache hits` and as one sensor of 27 steps.
    #[test]
    fn a_group_of_sensors_is_a_count_of_nodes_with_an_example() {
        let plan = PlanReasons {
            nodes: 29,
            steps: 29,
            omitted: 0,
            reasons: vec![
                reason(ReasonCategory::Sensor, None, 27, 27, "feeds.py:poll_inbox"),
                reason(ReasonCategory::NeverRun, None, 2, 2, "feeds.py:orders"),
            ],
        };
        let why = why_ran(Some(&plan), false, 29, false);
        assert_eq!(
            why.headline,
            "29 steps ran: 27 sensors (always run), 2 never run"
        );
        assert_eq!(why.groups[0].label, "sensors (always run)");
        let row = &why.groups[0].rows[0];
        assert_eq!(row.counts, "27 nodes");
        assert_eq!(row.example_text, "e.g. poll_inbox");
        assert_eq!(row.line, "27 nodes, e.g. poll_inbox");
        assert_eq!(
            why_lines(&why, 8),
            [
                "why: 29 steps ran: 27 sensors (always run), 2 never run",
                "  sensors (always run)",
                "    27 nodes, e.g. poll_inbox",
                "  never run",
                "    2 nodes, e.g. orders",
            ]
        );
        let one = PlanReasons {
            nodes: 1,
            steps: 1,
            omitted: 0,
            reasons: vec![reason(
                ReasonCategory::Sensor,
                None,
                1,
                1,
                "feeds.py:poll_inbox",
            )],
        };
        let why = why_ran(Some(&one), false, 1, false);
        assert_eq!(why.headline, "1 step ran: 1 sensor (always runs)");
        assert_eq!(why.groups[0].rows[0].line, "1 node: poll_inbox");
    }

    #[test]
    fn a_step_counted_under_several_subjects_never_exceeds_the_runs_steps() {
        let plan = PlanReasons {
            nodes: 2,
            steps: 2,
            omitted: 0,
            reasons: vec![
                reason(ReasonCategory::CodeChanged, Some("code"), 2, 2, "a.py:x"),
                reason(
                    ReasonCategory::CodeChanged,
                    Some("helper a.py:f"),
                    2,
                    2,
                    "a.py:x",
                ),
            ],
        };
        let why = why_ran(Some(&plan), false, 2, false);
        assert_eq!(why.groups[0].steps, 2);
        assert_eq!(why.headline, "2 steps ran: 2 code changed");
    }

    #[test]
    fn many_groups_are_summed_up_and_long_groups_cut_in_the_lines() {
        let categories = [
            ReasonCategory::NeverRun,
            ReasonCategory::Failed,
            ReasonCategory::Evicted,
            ReasonCategory::Task,
            ReasonCategory::RefreshAll,
        ];
        let plan = PlanReasons {
            nodes: 5,
            steps: 15,
            omitted: 1,
            reasons: categories
                .iter()
                .enumerate()
                .map(|(i, c)| reason(*c, None, 1, 5 - i as u64, "a.py:x"))
                .collect(),
        };
        let why = why_ran(Some(&plan), true, 0, false);
        assert_eq!(
            why.headline,
            "15 steps decided to run so far: 5 never run, 4 retrying a failed attempt, \
             3 artifact evicted, and 2 more reasons"
        );
        assert!(why.partial);
        assert_eq!(
            why_lines(&why, 8).last().unwrap(),
            "  not listed: 1 smaller reason"
        );
        let subjects = PlanReasons {
            nodes: 3,
            steps: 3,
            omitted: 0,
            reasons: (0..3)
                .map(|i| {
                    reason(
                        ReasonCategory::EnvChanged,
                        Some(&format!("env VAR_{i}")),
                        1,
                        1,
                        "a.py:x",
                    )
                })
                .collect(),
        };
        let lines = why_lines(&why_ran(Some(&subjects), false, 3, false), 2);
        assert_eq!(
            lines[1..],
            [
                "  environment changed",
                "    1 node: VAR_0",
                "    1 node: VAR_1",
                "    and 1 more (--json lists them)"
            ]
        );
    }

    #[test]
    fn a_run_that_decided_nothing_says_so_in_a_sentence() {
        let none = |live, executed, never| why_ran(None, live, executed, never);
        assert_eq!(none(true, 0, false).headline, "No step decided to run yet.");
        assert_eq!(
            none(false, 0, false).headline,
            "Every step was served from cache."
        );
        assert_eq!(
            none(false, 4_976, false).headline,
            "4,976 steps ran, but this run recorded no reasons (barca records them from 0.43)."
        );
        assert_eq!(
            none(false, 0, true).headline,
            "It was cancelled before it decided what to run."
        );
        let empty = PlanReasons::default();
        assert!(why_ran(Some(&empty), false, 0, false).groups.is_empty());
    }

    #[test]
    fn every_category_has_its_own_words() {
        let all = [
            ReasonCategory::NeverRun,
            ReasonCategory::NewKeys,
            ReasonCategory::Failed,
            ReasonCategory::CodeChanged,
            ReasonCategory::InputsChanged,
            ReasonCategory::EnvChanged,
            ReasonCategory::Changed,
            ReasonCategory::Refresh,
            ReasonCategory::RefreshCascade,
            ReasonCategory::RefreshAll,
            ReasonCategory::ArtifactMissing,
            ReasonCategory::Evicted,
            ReasonCategory::Task,
            ReasonCategory::Sensor,
            ReasonCategory::NotExamined,
        ];
        let labels: std::collections::HashSet<_> = all.iter().map(|c| reason_label(*c)).collect();
        assert_eq!(labels.len(), all.len());
        let clauses: std::collections::HashSet<_> =
            all.iter().map(|c| reason_clause(*c, 1)).collect();
        assert_eq!(clauses.len(), all.len());
    }

    #[test]
    fn the_token_that_asked_for_a_run_is_named() {
        assert_eq!(
            requested_by(Some(TriggeredBy::Api), Some("token:ci-bot")),
            "api · token:ci-bot"
        );
        assert_eq!(requested_by(Some(TriggeredBy::Schedule), None), "schedule");
        assert_eq!(requested_by(None, None), "unknown");
    }

    /// The audit of 2026-11-17: 29 steps with no retries read `29 attempts`.
    #[test]
    fn retries_are_counted_apart_from_first_attempts() {
        assert_eq!(
            attempts_label(Some(29), Some(0)).as_deref(),
            Some("29 steps, no retries")
        );
        assert_eq!(
            attempts_label(Some(19_461), Some(62)).as_deref(),
            Some("19,399 steps, 62 retries")
        );
        assert_eq!(
            attempts_label(Some(3), Some(1)).as_deref(),
            Some("2 steps, 1 retry")
        );
        assert_eq!(
            attempts_label(Some(1), Some(0)).as_deref(),
            Some("1 step, no retries")
        );
        // A run recorded before barca counted retries.
        assert_eq!(attempts_label(Some(3), None).as_deref(), Some("3 attempts"));
        assert_eq!(attempts_label(None, None), None);
        assert_eq!(attempts_label(Some(0), Some(0)), None);
    }

    #[test]
    fn a_run_cancelled_in_the_queue_never_started_and_says_what_it_waited_for() {
        let cancel = |by, principal: Option<&str>, started, behind: &[&str], same: Option<&str>| {
            cancellation_notice(&Cancellation {
                by,
                principal: principal.map(str::to_string),
                started,
                waiting_behind: behind.iter().map(|r| r.to_string()).collect(),
                same_as: same.map(str::to_string),
            })
        };
        assert_eq!(
            cancel(
                CancelledBy::Request,
                Some("token:ops"),
                false,
                &["a", "b"],
                Some("b")
            ),
            CancellationNotice {
                headline: "Not started: the same target was already running as run b.".into(),
                detail: Some(
                    "Cancelled by token:ops while it waited in the queue; it executed nothing."
                        .into()
                ),
                run: Some("b".into()),
            }
        );
        let behind = cancel(CancelledBy::Request, None, false, &["a"], None);
        assert_eq!(
            behind.headline,
            "Not started: cancelled while it waited in the queue."
        );
        assert_eq!(
            behind.detail.as_deref(),
            Some("It waited behind run a. Cancelled.")
        );
        assert_eq!(behind.run.as_deref(), Some("a"));
        let stopped = cancel(CancelledBy::Shutdown, None, true, &[], None);
        assert_eq!(
            stopped.headline,
            "Cancelled by the server stopping while it ran."
        );
        assert_eq!(stopped.run, None);
    }

    #[test]
    fn targets_are_a_list_the_jobs_node_ids_when_recorded() {
        let of = |target, job| {
            targets(RunFacts {
                target,
                job,
                ..facts()
            })
        };
        assert_eq!(
            of(Some("orders, daily_report"), None),
            ["orders", "daily_report"]
        );
        assert_eq!(
            of(Some("orders"), Some("p.py:daily_report,p.py:orders")),
            ["p.py:daily_report", "p.py:orders"]
        );
        assert!(of(None, Some("all")).is_empty());
        assert!(of(None, None).is_empty());
    }

    #[test]
    fn steps_are_counted_by_outcome_and_a_run_that_did_not_finish_says_what_it_started() {
        assert_eq!(
            outcome_label(8, 0, 1, 2),
            "7 ok · 1 failed · 2 skipped · 0 cached"
        );
        assert_eq!(outcome_label(0, 9_236, 0, 0), "0 ok · 9,236 cached");
        let run = RunFacts {
            steps_executed: 3,
            steps_cached: 1,
            ..facts()
        };
        let nodes = [
            RunNodeSummary {
                failed: 1,
                ..planned("a", "ran", None)
            },
            planned("b", "ran", None),
            planned("c", "skipped", None),
        ];
        assert_eq!(
            steps_label(run, &nodes),
            "2 ok · 1 failed · 1 skipped · 1 cached"
        );
        let cancelled = RunFacts {
            status: "cancelled",
            steps_executed: 1,
            ..facts()
        };
        assert_eq!(steps_label(cancelled, &[]), "1 started · 0 cached");
    }

    #[test]
    fn what_became_of_each_target_is_one_line() {
        let forecast = RunNodeSummary {
            status: Some("ran".into()),
            ..node("p.py:demand_forecast", 3_077, 7, 2.0)
        };
        let nodes = vec![
            planned("p.py:retailer_accounts", "cached", None),
            forecast.clone(),
            planned("p.py:promo_plan", "skipped", Some("p.py:demand_forecast")),
            planned("p.py:publish_plan", "skipped", Some("p.py:demand_forecast")),
        ];
        let of = |target, job, nodes: &[RunNodeSummary]| {
            target_outcomes(
                RunFacts {
                    target,
                    job,
                    ..facts()
                },
                nodes,
            )
        };
        assert_eq!(
            of(Some("publish_plan"), Some("p.py:publish_plan"), &nodes),
            ["publish_plan did not run: upstream demand_forecast failed 7 keys"]
        );
        assert_eq!(
            of(
                Some("demand_forecast"),
                Some("p.py:demand_forecast"),
                &nodes
            ),
            ["demand_forecast failed: 7 of 3,077 keys"]
        );
        assert_eq!(
            of(Some("retailer_accounts"), None, &nodes),
            ["retailer_accounts was cached: nothing to do"]
        );
        let mut late = nodes.clone();
        late.push(planned("p.py:late", "skipped", None));
        assert_eq!(
            of(Some("late"), Some("p.py:late"), &late),
            ["late did not run: the run stopped before it"]
        );
        // The server could not rebuild the plan: the nodes carry no status.
        let unplanned = [RunNodeSummary {
            status: None,
            ..forecast
        }];
        assert!(of(Some("publish_plan"), Some("p.py:publish_plan"), &unplanned).is_empty());
        assert!(of(None, Some("all"), &nodes).is_empty());
    }

    #[test]
    fn a_run_in_progress_says_how_far_and_an_eta_from_the_rate_so_far() {
        let run = RunFacts {
            status: "running",
            started_at: "2026-11-16 11:50:00",
            steps_total: Some(23_278),
            steps_executed: 9_000,
            ..facts()
        };
        let p = progress(run, NOW);
        assert_eq!(
            (p.done, p.total, p.percent),
            (9_000, Some(23_278), Some(38))
        );
        // 14,278 steps left at 15 a second.
        assert_eq!(p.eta.as_deref(), Some("about 16m left"));
        let rfc = RunFacts {
            started_at: "2026-11-16T11:50:00.000Z",
            ..run
        };
        assert_eq!(progress(rfc, NOW).eta_seconds, p.eta_seconds);
        // No ETA before a step executed; cached steps are done.
        let cached = RunFacts {
            status: "running",
            started_at: "2026-11-16 11:59:55",
            steps_total: Some(10),
            steps_cached: 4,
            ..facts()
        };
        let p = progress(cached, NOW);
        assert_eq!((p.percent, p.eta), (Some(40), None));
        assert_eq!(left_label(30.0), "under a minute left");
        assert_eq!(left_label(2.0 * 3600.0 + 300.0), "about 2h 5m left");
    }

    #[test]
    fn only_a_run_in_progress_has_progress() {
        let live = explain(
            RunFacts {
                status: "running",
                ..facts()
            },
            &[],
            &[],
            NOW,
        );
        assert!(live.progress.is_some());
        assert!(live.why.partial);
        let done = explain(facts(), &[], &[], NOW);
        assert_eq!(done.progress, None);
        assert_eq!(done.requested_by, "unknown");
        assert_eq!(done.steps, "0 ok · 0 cached");
    }

    #[test]
    fn the_slowest_nodes_are_ranked_by_their_summed_time_the_summary_first() {
        let nodes = [
            node("a", 10, 0, 3.0),
            node("b", 1, 0, 9.0),
            node("c", 1, 0, 0.0),
        ];
        let steps: Vec<RunStep> = (0..7)
            .map(|i| step(&format!("p.py:n{i}"), i as f64 + 1.0))
            .collect();
        let ids = |rows: Vec<SlowNode>| rows.into_iter().map(|r| r.node_id).collect::<Vec<_>>();
        assert_eq!(ids(slowest_nodes(&nodes, &steps, 5)), ["b", "a"]);
        assert_eq!(ids(slowest_nodes(&[], &steps, 2)), ["p.py:n6", "p.py:n5"]);
    }

    #[test]
    fn counts_are_grouped_in_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(19_461), "19,461");
        assert_eq!(count(1_234_567), "1,234,567");
    }
}
