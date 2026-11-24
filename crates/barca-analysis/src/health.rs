//! A node's health (`specs/behavior/health.md`): one word that says whether something needs
//! doing about it, the row of the rule table that decided it, what its recorded results say
//! about its data, and whether its data was measured at all.
//!
//! This is classification: it reads records and the graph, and writes nothing. Every surface
//! (`barca status --json`, `GET /nodes`, `GET /state`, `barca.nodes`) carries what
//! [`classify`] put on [`NodeStatus`]; none derives a word of its own.
//!
//! The words, most severe first:
//!
//! - `failing`: its own last attempt failed. A root cause.
//! - `blocked`: something upstream keeps it from being brought up to date.
//! - `suspect`: its last run succeeded but its recorded row counts look wrong.
//! - `behind`: out of date against its inputs, partly computed, out of sync, or late.
//! - `undecided`: barca cannot decide yet (a cutoff or sensor upstream has not reported).
//! - `never_run`: nothing recorded.
//! - `ok`: fresh, or a task or sensor whose last run succeeded.
//!
//! Health sits beside the cache state (`cache.state`) and never replaces it.

pub use crate::estimates::Evidence;
pub(crate) use crate::views::recorded_at;
pub use crate::views::{
    Cause, Check, Checks, DataCoverage, DataSignal, Health, HealthReason, LastCheck, NoEstimate,
    Root, RootKind, last_attempt_failed,
};
use crate::views::{NodeStatus, StatusResult, keys_failed, last_materialization_failed};
use barca_db::db::{self, NodeAge, TickTargetOutcome};
use barca_decide::schedule::Zone;
use barca_model::BarcaError;
use barca_model::cache_state::CacheState;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Why `node`, classified and attributed, has no estimate; `None` for a fresh node and one
/// that has an `expected_fresh_at`. The first that applies: its failure; what its root says
/// of its schedule (paused, not fired, missing); that barca cannot decide it; that ticks
/// missed it; then, for a node a tick or a run will reach, why that promises nothing.
pub fn no_estimate(node: &NodeStatus, evidence: Evidence<'_>) -> Option<NoEstimate> {
    let cause = node.cause?;
    if node.expected_fresh_at.is_some() {
        return None;
    }
    Some(match (cause, node.root.as_ref().map(|r| r.kind)) {
        (Cause::Failing, _) => NoEstimate::Failing,
        (_, Some(RootKind::SchedulePaused)) => NoEstimate::Paused,
        (_, Some(RootKind::SchedulerOff)) => NoEstimate::SchedulerOff,
        (_, Some(RootKind::NoSchedule)) | (Cause::NoRefreshPath, _) => NoEstimate::NoPath,
        (Cause::Undecided, _) => NoEstimate::Undecided,
        (Cause::Overdue, _) => NoEstimate::Missed,
        (Cause::Queued | Cause::CatchingUp, _) if !evidence.timing.queue_open => {
            NoEstimate::QueueStopped
        }
        (Cause::Queued | Cause::CatchingUp, _) if node.recheck_at.is_none() => {
            NoEstimate::NoSchedulerHere
        }
        (Cause::Queued | Cause::CatchingUp, _) => NoEstimate::NotPromised,
    })
}

/// Whether a failure upstream can hold the node: not failing itself, and neither `cached` nor
/// `always_runs`.
fn can_be_blocked(node: &NodeStatus) -> bool {
    !last_attempt_failed(node)
        && !matches!(
            node.cache.state,
            CacheState::Cached | CacheState::AlwaysRuns
        )
}

/// Each node a failing node upstream keeps from being brought up to date, with that failing
/// node (the nearest one): walking a node's inputs in order, an input whose last attempt failed
/// is the answer; an input that can itself be blocked passes the question to its own inputs.
/// The walk passes through `unknown` nodes, so a node waiting on a cutoff below a failure is
/// blocked by the failure. A pinned asset is never blocked and stops the walk.
pub fn blocked_by(nodes: &[NodeStatus], pinned: &HashSet<String>) -> HashMap<String, String> {
    let by_id: HashMap<&str, &NodeStatus> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut memo: HashMap<&str, Option<&str>> = HashMap::with_capacity(nodes.len());
    let mut visiting: HashSet<&str> = HashSet::new();
    fn cause_of<'a>(
        id: &'a str,
        by_id: &HashMap<&'a str, &'a NodeStatus>,
        pinned: &HashSet<String>,
        memo: &mut HashMap<&'a str, Option<&'a str>>,
        visiting: &mut HashSet<&'a str>,
    ) -> Option<&'a str> {
        if let Some(known) = memo.get(id) {
            return *known;
        }
        if !visiting.insert(id) {
            return None;
        }
        let mut found = None;
        for input in by_id.get(id).map_or(&[][..], |n| n.inputs.as_slice()) {
            let Some(up) = by_id.get(input.as_str()) else {
                continue;
            };
            if pinned.contains(input) {
                continue;
            }
            if last_attempt_failed(up) {
                found = Some(up.id.as_str());
            } else if can_be_blocked(up) {
                found = cause_of(up.id.as_str(), by_id, pinned, memo, visiting);
            }
            if found.is_some() {
                break;
            }
        }
        visiting.remove(id);
        memo.insert(id, found);
        found
    }
    let mut out = HashMap::new();
    // Rows come in dependency order, so each walk finds its inputs already answered.
    for node in nodes {
        if !can_be_blocked(node) || pinned.contains(&node.id) {
            continue;
        }
        if let Some(cause) = cause_of(node.id.as_str(), &by_id, pinned, &mut memo, &mut visiting) {
            out.insert(node.id.clone(), cause.to_string());
        }
    }
    out
}

/// The signals of `node`, from the ages of every node. `is_asset` says whether an input is an
/// asset (asked only for a node whose state is `unknown`). Nothing is inspected or guessed: a
/// count no worker recorded raises no signal. `checked`: a tick's verdict stands for the node
/// ([`verdict_stands`]), so it is not behind an input: that tick read the input's newest
/// result and found the node up to date.
pub fn signals(
    node: &NodeStatus,
    ages: &HashMap<String, NodeAge>,
    is_asset: impl Fn(&str) -> bool,
    checked: bool,
) -> Vec<DataSignal> {
    let mut out = Vec::new();
    if node.kind != "asset" {
        return out;
    }
    let Some(age) = ages.get(&node.id) else {
        return out;
    };
    if let Some(rows) = &age.rows {
        if rows.empty > 0 {
            out.push(DataSignal::Empty {
                empty_keys: rows.empty,
                keys: rows.counted,
                previous_empty_keys: rows.previous_empty,
            });
        }
        if let Some(change) = &rows.largest_change {
            out.push(DataSignal::RowsChanged {
                changed_keys: rows.changed,
                key: change.key.clone(),
                rows: change.rows,
                previous_rows: change.previous_rows,
            });
        }
    }
    if node.cache.state == CacheState::Unknown
        && !checked
        && let (Some(own), Some(at)) = (age.rows.as_ref(), age.last_success_at.as_ref())
    {
        let behind = node
            .inputs
            .iter()
            .filter(|input| is_asset(input))
            .filter_map(|input| {
                let theirs = ages.get(input)?;
                let newest = &theirs.rows.as_ref()?.newest;
                // Newer, and from another run: a run writes a node after its inputs, and a key
                // its run recorded late (the end-of-run ledger) is not newer data.
                let other_run = newest.1.is_none() || newest.1 != own.newest.1;
                (newest.0 > own.newest.0 && other_run)
                    .then(|| Some((newest.0, input, theirs.last_success_at.clone()?)))
                    .flatten()
            })
            .max_by_key(|(id, _, _)| *id);
        if let Some((_, input, input_at)) = behind {
            out.push(DataSignal::BehindInput {
                input: input.to_string(),
                input_success_at: input_at,
                last_success_at: at.clone(),
            });
        }
    }
    out
}

/// Whether an asset's latest successful result has a recorded row count (for a partitioned
/// asset, any key's); `None` for a task or sensor, which has no data to measure.
pub fn measured(node: &NodeStatus, ages: &HashMap<String, NodeAge>) -> Option<bool> {
    (node.kind == "asset").then(|| {
        ages.get(&node.id)
            .and_then(|age| age.rows.as_ref())
            .is_some_and(|rows| rows.counted > 0)
    })
}

/// For each node, when the newest materialization of a node above it (its whole upstream
/// cone) was recorded, unix seconds; a node with nothing recorded above it is absent. Rows a
/// tick's run still in flight recorded do not count: that run's own verdict is on its way.
/// `nodes` are in dependency order.
fn newest_above(nodes: &[NodeStatus], evidence: Evidence<'_>) -> HashMap<String, i64> {
    // Each node's own newest row or anything above it, whichever is newer.
    let mut through: HashMap<&str, i64> = HashMap::with_capacity(nodes.len());
    let mut above: HashMap<String, i64> = HashMap::new();
    for node in nodes {
        let over = node
            .inputs
            .iter()
            .filter_map(|input| through.get(input.as_str()).copied())
            .max();
        if let Some(at) = over {
            above.insert(node.id.clone(), at);
        }
        let in_flight = evidence
            .ages
            .get(&node.id)
            .and_then(|age| age.last_run.as_ref())
            .is_some_and(|run| evidence.checks.in_flight.contains(&run.run_id));
        let own = node
            .last_materialization
            .as_ref()
            .filter(|_| !in_flight)
            .map(|m| recorded_at(&m.created_at));
        if let Some(at) = own.into_iter().chain(over).max() {
            through.insert(node.id.as_str(), at);
        }
    }
    above
}

/// For each node, when a node above it most recently recovered from failure (its whole upstream
/// cone) was recorded, unix seconds; a node with none above it is absent. What a missed tick
/// is counted against ([`crate::refresh_path::missed_ticks`]). `nodes` are in dependency
/// order.
fn latest_recovery_above(nodes: &[NodeStatus], evidence: Evidence<'_>) -> HashMap<String, i64> {
    let mut through: HashMap<&str, i64> = HashMap::with_capacity(nodes.len());
    let mut above: HashMap<String, i64> = HashMap::new();
    for node in nodes {
        let over = node
            .inputs
            .iter()
            .filter_map(|input| through.get(input.as_str()).copied())
            .max();
        if let Some(at) = over {
            above.insert(node.id.clone(), at);
        }
        let own = evidence
            .ages
            .get(&node.id)
            .and_then(|age| age.recovered_at.as_deref())
            .map(recorded_at)
            .filter(|at| *at != i64::MAX);
        if let Some(at) = own.into_iter().chain(over).max() {
            through.insert(node.id.as_str(), at);
        }
    }
    above
}

/// Whether the verdict of `check`, the newest finished tick that had `node` as a target,
/// still stands as of `now`: a run decided what the read path cannot, and nothing the read
/// path can see has changed since. All of:
///
/// - the tick found the node up to date or rebuilt it (a newer `failed` or `not_reached`
///   outcome is the newest check, and decides nothing);
/// - it is not expired ([`Check::trusted_until`]): the next tick of that schedule is not
///   overdue, and the schedule still exists;
/// - the node's definition is the one its newest successful result recorded
///   ([`Checks::redefined`]);
/// - no node above it has a materialization recorded after the tick's run ended
///   (`newest_above`), other than by a tick's run still in flight.
///
/// What it cannot see, and the expiry bounds: an edit to a node above it (shown on that
/// node), and a declared environment variable's new value.
pub fn verdict_stands(
    node: &NodeStatus,
    check: &Check,
    redefined: &HashSet<String>,
    newest_above: Option<i64>,
    now: i64,
) -> bool {
    matches!(
        check.last.outcome,
        TickTargetOutcome::UpToDate | TickTargetOutcome::Rebuilt
    ) && check.trusted_until.is_some_and(|until| now as f64 <= until)
        && !redefined.contains(&node.id)
        // Rows are recorded to the second: one in the second the run ended is the run's own.
        && newest_above.is_none_or(|at| at as f64 <= check.finished_at)
}

/// The health table of `specs/behavior/health.md`: the first matching row decides. `checked`:
/// a tick's verdict stands for the node ([`verdict_stands`]).
pub fn decide(
    node: &NodeStatus,
    blocked: bool,
    signals: &[DataSignal],
    late: bool,
    checked: bool,
) -> (Health, HealthReason) {
    use HealthReason as R;
    if last_materialization_failed(node) {
        return (Health::Failing, R::LastAttemptFailed);
    }
    if keys_failed(node) {
        return (Health::Failing, R::KeysFailed);
    }
    if blocked {
        return (Health::Blocked, R::UpstreamFailing);
    }
    if node.cache.state == CacheState::Blocked || node.cache.reason == "upstream_failed" {
        return (Health::Blocked, R::CacheBlocked);
    }
    if signals
        .iter()
        .any(|s| matches!(s, DataSignal::Empty { .. }))
    {
        return (Health::Suspect, R::Empty);
    }
    if signals
        .iter()
        .any(|s| matches!(s, DataSignal::RowsChanged { .. }))
    {
        return (Health::Suspect, R::RowsChanged);
    }
    if signals
        .iter()
        .any(|s| matches!(s, DataSignal::BehindInput { .. }))
    {
        return (Health::Behind, R::BehindInput);
    }
    if late {
        return (Health::Behind, R::ScheduleLate);
    }
    match node.cache.state {
        CacheState::Stale => (Health::Behind, R::Stale),
        CacheState::Partial => (Health::Behind, R::Partial),
        CacheState::Unsynced => (Health::Behind, R::Unsynced),
        // The read path cannot decide; a tick did, and its verdict stands (#964).
        CacheState::Unknown if checked => (Health::Ok, R::CheckedUpToDate),
        // Row 8.
        CacheState::Unknown => (Health::Undecided, R::Unknown),
        CacheState::NeverRun => (Health::NeverRun, R::NeverRun),
        CacheState::AlwaysRuns if node.last_materialization.is_none() => {
            (Health::NeverRun, R::NoSuccessfulRun)
        }
        CacheState::AlwaysRuns => (Health::Ok, R::LastRunSucceeded),
        CacheState::Cached => (Health::Ok, R::Cached),
        // Decided above (row 3); kept so the match stays exhaustive over cache states.
        CacheState::Blocked => (Health::Blocked, R::CacheBlocked),
    }
}

/// Row 6a of the health table: a node [`decide`] found up to date from its last result
/// (`cached`, a task or sensor whose last run succeeded, a tick's standing verdict) is
/// `behind` (`schedule_late`) when a slot of its refresh path's schedule has passed that no
/// scheduler served and none fires that schedule
/// ([`crate::refresh_path::PathSchedule::unserved`]): the result is current only because
/// nothing has looked since. Every other answer stands.
pub fn unserved(
    decided: (Health, HealthReason),
    node: &str,
    path: crate::refresh_path::PathEvidence<'_>,
) -> (Health, HealthReason) {
    use HealthReason as R;
    match decided {
        (Health::Ok, R::CheckedUpToDate | R::LastRunSucceeded | R::Cached)
            if crate::refresh_path::path_schedule(node, path)
                .is_some_and(|(_, schedule)| schedule.off() && schedule.off_since.is_some()) =>
        {
            (Health::Behind, R::ScheduleLate)
        }
        other => other,
    }
}

/// Fill `health`, `health_reason`, `signals`, `measured`, `last_check`, `refresh_path`,
/// `cause`, `root`, `gap_seconds` ([`crate::causes`]) and the times (`stale_since`, `expected_fresh_at`, `recheck_at`:
/// [`crate::estimates::times`]) on every row of `nodes` (one status result: a scope with its upstream cone,
/// in dependency order).
pub fn classify(nodes: &mut [NodeStatus], evidence: Evidence<'_>) {
    let assets: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == "asset")
        .map(|n| n.id.as_str())
        .collect();
    let mut blocked = blocked_by(nodes, evidence.pinned);
    // Only a node the read path cannot decide asks whether a tick's verdict stands.
    let above = if nodes.iter().any(|n| {
        n.cache.state == CacheState::Unknown && evidence.checks.by_node.contains_key(&n.id)
    }) {
        newest_above(nodes, evidence)
    } else {
        HashMap::new()
    };
    // Only a node a tick missed asks what has succeeded above it since.
    let succeeded_above = if nodes
        .iter()
        .any(|n| evidence.checks.misses.contains_key(&n.id))
    {
        latest_recovery_above(nodes, evidence)
    } else {
        HashMap::new()
    };
    let decided: Vec<_> = nodes
        .iter()
        .map(|node| {
            let check = evidence.checks.by_node.get(&node.id);
            let checked = node.cache.state == CacheState::Unknown
                && check.is_some_and(|check| {
                    verdict_stands(
                        node,
                        check,
                        &evidence.checks.redefined,
                        above.get(&node.id).copied(),
                        evidence.now,
                    )
                });
            let signals = signals(node, evidence.ages, |id| assets.contains(id), checked);
            let by = blocked.remove(&node.id);
            let (health, reason) = unserved(
                decide(
                    node,
                    by.is_some(),
                    &signals,
                    evidence.late.contains(&node.id),
                    checked,
                ),
                &node.id,
                evidence.path,
            );
            let (health, reason) = if health == Health::NeverRun
                && node.kind == "task"
                && evidence.path.paths.is_manual(&node.id)
            {
                (Health::Ok, HealthReason::OnDemand)
            } else {
                (health, reason)
            };
            // The failure root; every other cause's root is named below
            // ([`crate::causes::attribute`]), once the paths and the times are known.
            let root = match health {
                Health::Failing => Some(Root::failure(&node.id)),
                _ => by.as_deref().map(Root::failure),
            };
            let last_check = check.map(|check| check.last.clone());
            // A fresh node was missed by no tick; else the misses since its newest success.
            let missed = match evidence.checks.misses.get(&node.id) {
                Some(misses) if !health.is_fresh() => crate::refresh_path::missed_ticks(
                    &node.id,
                    evidence.path.paths,
                    misses,
                    evidence
                        .ages
                        .get(&node.id)
                        .and_then(|age| age.last_success_at.as_deref())
                        .map(recorded_at),
                    succeeded_above.get(&node.id).copied(),
                ),
                _ => 0,
            };
            (
                health,
                reason,
                signals,
                measured(node, evidence.ages),
                root,
                last_check,
                crate::refresh_path::refresh_path(&node.id, missed, evidence.path),
            )
        })
        .collect();
    for (node, (health, reason, signals, measured, root, last_check, refresh_path)) in
        nodes.iter_mut().zip(decided)
    {
        node.health = health;
        node.health_reason = reason;
        node.signals = signals;
        node.measured = measured;
        node.root = root;
        node.last_check = last_check;
        node.refresh_path = refresh_path;
    }
    // The times read what is classified above: one pass over the rows, no read.
    let times = crate::estimates::times(nodes, evidence);
    for (node, times) in nodes.iter_mut().zip(times) {
        node.stale_since = times.stale_since;
        node.expected_fresh_at = times.expected_fresh_at;
        node.recheck_at = times.recheck_at;
    }
    // The cause, the root and the gap read all of the above: one more pass, no read.
    let attributed = crate::causes::attribute(nodes, evidence);
    let gap_at = crate::causes::gap_instant(evidence.now);
    for (node, (cause, root)) in nodes.iter_mut().zip(attributed) {
        node.cause = cause;
        node.root = root;
        node.gap_seconds = crate::causes::gap_seconds(node, gap_at);
        node.no_estimate = no_estimate(node, evidence);
        node.last_attempt = crate::views::LastAttempt::of(node);
        node.typical_run_seconds =
            crate::estimates::node_run_seconds(node, evidence.timing.durations);
    }
}

/// [`classify`] every row of `result`, count its data coverage and sum its rollup's root
/// entry as of `evidence.now`, over every node in scope: call it before a filter cuts
/// `nodes`. `running`: the nodes a run is working on.
pub fn attach(result: &mut StatusResult, evidence: Evidence<'_>, running: &HashSet<String>) {
    classify(&mut result.nodes, evidence);
    result.data_coverage = DataCoverage::of(&result.nodes);
    result.as_of = barca_model::time::rfc3339_secs(evidence.now);
    let roots = crate::rollup::roots(&result.nodes, evidence, false);
    result.rollup = Some(crate::rollup::total(
        &result.nodes,
        running,
        &crate::rollup::root_actions(&roots),
    ));
}

/// Successful materializations considered for a node's typical durations.
pub const DURATION_WINDOW: usize = 20;

/// Median and p95 wall time over a node's most recent successful materializations.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Durations {
    pub median_seconds: f64,
    pub p95_seconds: f64,
    /// How many materializations these are computed from (at most 20).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub samples: usize,
}

/// [`Durations`] of each node's recent successful runs ([`db::recent_durations`], at most
/// [`DURATION_WINDOW`] of them, partition keys folded into their node); a node without one
/// has no entry. The median is the one every typical time is
/// ([`crate::schedule_standing::median`]: the middle value, or the mean of the two middle
/// values of an even count); p95 is the nearest rank.
pub fn durations(recent: HashMap<String, Vec<f64>>) -> HashMap<String, Durations> {
    recent
        .into_iter()
        .filter_map(|(id, mut recent)| {
            if recent.is_empty() {
                return None;
            }
            recent.sort_by(f64::total_cmp);
            // Nearest rank over the sorted values.
            let rank = (0.95 * recent.len() as f64).ceil() as usize;
            Some((
                id,
                Durations {
                    median_seconds: crate::schedule_standing::median(&recent),
                    p95_seconds: recent[rank.clamp(1, recent.len()) - 1],
                    samples: recent.len(),
                },
            ))
        })
        .collect()
}

/// The run queue as the estimates read it: the runs in flight and when one that joined now
/// would start. A server fills it from the queue it holds; `barca status` and a read-only
/// server from the one the serving server records ([`Queue::recorded`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Queue {
    /// The runs in flight: executing, or waiting ([`crate::estimates::Timing::runs`]).
    pub runs: Vec<crate::estimates::PlannedRun>,
    pub clear_at: Option<i64>,
    /// The queue starts runs: neither paused nor draining, and its server is up.
    pub open: bool,
    /// How many steps of one run execute at once.
    pub workers: usize,
    /// How many runs execute at once, where the host knows.
    pub max_runs: Option<usize>,
}

impl Queue {
    /// The queue a serving server recorded ([`recorded_queue`]); `max_runs` is what the host
    /// has configured, read when the server recorded none.
    pub fn recorded(timing: crate::liveness::RecordedTiming, max_runs: Option<usize>) -> Self {
        Self {
            runs: timing.runs,
            clear_at: timing.queue_clear_at,
            open: timing.queue_open,
            workers: timing.workers.unwrap_or(1),
            max_runs: timing.max_runs.or(max_runs),
        }
    }
}

/// What the queue of the database at `db_path` records, as the estimates read it
/// ([`crate::liveness::recorded_timing`]): its runs, its pause, and whether its server is
/// gone (`recorded`, `observed`: [`crate::liveness`]). `ticks` are the schedules' recorded
/// ticks. A database that does not exist records an open, empty queue.
pub async fn recorded_queue(
    db_path: &str,
    dag: &barca_model::dag::Dag,
    ticks: &HashMap<String, crate::schedule_standing::Ticks>,
    recorded: &crate::liveness::Recorded,
    observed: &crate::liveness::Observed,
    now: i64,
) -> Result<crate::liveness::RecordedTiming, BarcaError> {
    let (runs, paused) = if Path::new(db_path).exists() {
        (
            crate::liveness::recorded_runs(db_path).await?,
            barca_store::run_queue::paused_at(db_path).await?.is_some(),
        )
    } else {
        Default::default()
    };
    Ok(crate::liveness::recorded_timing(
        &runs,
        ticks,
        recorded,
        paused,
        observed.server_gone(recorded),
        dag,
        now,
    ))
}

/// Everything health, schedule standing and the estimates are computed from, as of
/// `schedules.now`: plain data, filled by the host answering. `barca status` fills it from
/// the metadata database ([`attach_from_db`]); a server from its read cache, its scheduler
/// and its run queue. The hosts differ only in what they put here: [`Inputs::assemble`] is the
/// one computation over it.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// Every node's age ([`db::node_ages`]).
    pub ages: &'a HashMap<String, NodeAge>,
    /// Pinned assets (`barca pin`).
    pub pinned: &'a HashSet<String>,
    /// Paused nodes (`barca pause`).
    pub paused: &'a HashSet<String>,
    /// What the newest ticks found ([`checks`]).
    pub checks: &'a Checks,
    /// Which schedules reach each node ([`crate::refresh_path::Paths::of`]).
    pub paths: &'a crate::refresh_path::Paths,
    /// The schedules: ticks, failures, pauses, the scheduler here or the one on record, and
    /// the instant everything is read at.
    pub schedules: crate::refresh_path::ScheduleInputs<'a>,
    /// Typical durations ([`durations`]) of the nodes in scope; empty where no estimate
    /// needs them.
    pub durations: &'a HashMap<String, Durations>,
    pub queue: &'a Queue,
    /// When each source file changed ([`crate::estimates::source_times_of`]).
    pub source_times: &'a HashMap<String, i64>,
}

impl<'a> Inputs<'a> {
    /// Where every schedule stands and what classification reads, computed once per answer.
    pub fn assemble(self) -> Assembled<'a> {
        let schedules = crate::refresh_path::schedule_views(self.schedules);
        Assembled {
            late: schedules
                .iter()
                .filter(|(_, schedule)| schedule.standing.late)
                .map(|(id, _)| id.clone())
                .collect(),
            path_schedules: crate::refresh_path::path_schedules(&schedules),
            typical: self
                .durations
                .iter()
                .map(|(id, d)| (id.clone(), d.median_seconds))
                .collect(),
            schedules,
            inputs: self,
        }
    }
}

/// [`Inputs`] with what is derived from them once per answer: each schedule's view, the late
/// ones and each node's typical duration. Every host classifies, attaches and sums through
/// it.
pub struct Assembled<'a> {
    inputs: Inputs<'a>,
    /// Every schedule, by scheduled node id ([`crate::refresh_path::schedule_views`]).
    pub schedules: HashMap<String, crate::refresh_path::ScheduleView>,
    late: HashSet<String>,
    path_schedules: crate::refresh_path::PathSchedules,
    typical: HashMap<String, f64>,
}

impl Assembled<'_> {
    /// What [`classify`], [`attach`] and [`crate::rollup::build`] read.
    pub fn evidence(&self) -> Evidence<'_> {
        let inputs = &self.inputs;
        Evidence {
            ages: inputs.ages,
            pinned: inputs.pinned,
            late: &self.late,
            checks: inputs.checks,
            path: crate::refresh_path::PathEvidence {
                paths: inputs.paths,
                schedules: &self.path_schedules,
                paused: inputs.paused,
                pinned: inputs.pinned,
            },
            timing: crate::estimates::Timing {
                durations: &self.typical,
                runs: &inputs.queue.runs,
                queue_clear_at: inputs.queue.clear_at,
                queue_open: inputs.queue.open,
                workers: inputs.queue.workers,
                source_times: inputs.source_times,
                max_runs: inputs.queue.max_runs,
            },
            now: inputs.schedules.now.timestamp(),
        }
    }

    /// Each scheduled node of `nodes` gets its `schedule` block.
    pub fn attach_schedules(&self, nodes: &mut [NodeStatus]) {
        for node in nodes.iter_mut() {
            if let Some(schedule) = self.schedules.get(&node.id) {
                node.schedule = Some(schedule.block.clone());
            }
        }
    }

    /// [`attach`] with this evidence.
    pub fn attach(&self, result: &mut StatusResult, running: &HashSet<String>) {
        attach(result, self.evidence(), running);
    }

    /// The rollup of `result`, which [`Assembled::attach`] has filled, as of `as_of`
    /// ([`crate::rollup::build`]). `server`: a server answers.
    pub fn rollup(
        &self,
        result: &StatusResult,
        groups: &[barca_model::ir::NodeGroup],
        running: &HashSet<String>,
        as_of: chrono::DateTime<chrono::Utc>,
        server: bool,
    ) -> crate::rollup::Rollup {
        crate::rollup::build(
            &result.nodes,
            groups,
            running,
            as_of,
            self.evidence(),
            server,
        )
    }
}

/// [`attach`] with the evidence the metadata DB of `cfg` holds (none when it does not exist
/// yet): what `barca status` and `barca sql` read on this machine. Cron is evaluated in
/// `zone`; a paused schedule is not late.
pub async fn attach_from_db(
    result: &mut StatusResult,
    dag: &barca_model::dag::Dag,
    cfg: &barca_model::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), BarcaError> {
    from_db(result, dag, cfg, zone, now, false, false)
        .await
        .map(|_| ())
}

/// [`attach_from_db`], with each scheduled node's `schedule` block from the same read
/// (`barca status`).
pub async fn attach_from_db_with_schedules(
    result: &mut StatusResult,
    dag: &barca_model::dag::Dag,
    cfg: &barca_model::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), BarcaError> {
    from_db(result, dag, cfg, zone, now, false, true)
        .await
        .map(|_| ())
}

/// [`attach_from_db`], then the rollup of the rows (`barca status --rollup`): no run is
/// tracked here, so `running` is 0 everywhere.
pub async fn rollup_from_db(
    result: &mut StatusResult,
    dag: &barca_model::dag::Dag,
    cfg: &barca_model::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::rollup::Rollup, BarcaError> {
    Ok(from_db(result, dag, cfg, zone, now, true, false)
        .await?
        .expect("the rollup was asked for"))
}

/// Fill [`Inputs`] from the metadata DB of `cfg` and attach: this host's half. No scheduler
/// and no run queue run here, so the schedules and the queue are what the serving server
/// recorded (`crate::liveness`), in the zone it recorded.
async fn from_db(
    result: &mut StatusResult,
    dag: &barca_model::dag::Dag,
    cfg: &barca_model::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
    rollup: bool,
    schedules: bool,
) -> Result<Option<crate::rollup::Rollup>, BarcaError> {
    let db_path = cfg.db_path.as_str();
    let exists = Path::new(db_path).exists();
    let jobs = barca_decide::schedule::jobs_from_dag(dag);
    let ids: Vec<String> = jobs.iter().map(|job| job.id.clone()).collect();
    let (seen, ages, pinned, paused, pauses, failures, ticks) = if exists {
        (
            crate::liveness::recorded(db_path).await?,
            db::node_ages(db_path).await?,
            barca_store::pins::list(db_path)
                .await?
                .into_iter()
                .map(|pin| pin.node_id)
                .collect::<HashSet<String>>(),
            barca_store::pauses::list(db_path)
                .await?
                .into_iter()
                .map(|pause| pause.id)
                .collect::<HashSet<String>>(),
            db::get_schedule_pauses(db_path).await?,
            db::get_schedule_failures(db_path).await?,
            crate::schedule_standing::recorded(db_path, &ids).await?,
        )
    } else {
        Default::default()
    };
    let recorded_zone = seen.zone();
    let zone = recorded_zone.as_ref().unwrap_or(zone);
    let observed = crate::liveness::observe(&jobs, zone, now, &seen);
    let checks = match exists {
        true => checks(db_path, dag, &ages, zone).await?,
        false => Checks::default(),
    };
    // The serving server's run slots, when it recorded them; else what is configured here.
    let queue = Queue::recorded(
        recorded_queue(db_path, dag, &ticks, &seen, &observed, now.timestamp()).await?,
        cfg.server_max_runs,
    );
    // The typical durations: what each row's `typical_run_seconds` is, what an estimate is
    // made of where a scheduler is seen firing, and what the rollup's roots cost.
    let durations = if exists {
        let ids: Vec<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
        durations(db::recent_durations(db_path, &ids, DURATION_WINDOW).await?)
    } else {
        HashMap::new()
    };
    // The scheduler's walk, once per command (#961).
    let paths = crate::refresh_path::Paths::of(dag);
    let source_times = crate::estimates::source_times_of(dag);
    // No scheduler runs here: a schedule is live when the records show one firing it (#1039).
    let firing = HashMap::new();
    let health = Inputs {
        ages: &ages,
        pinned: &pinned,
        paused: &paused,
        checks: &checks,
        paths: &paths,
        schedules: crate::refresh_path::ScheduleInputs {
            jobs: &jobs,
            zone,
            now,
            ticks: &ticks,
            failures: &failures,
            pauses: &pauses,
            firing: &firing,
            observed: &observed,
        },
        durations: &durations,
        queue: &queue,
        source_times: &source_times,
    }
    .assemble();
    if schedules {
        health.attach_schedules(&mut result.nodes);
    }
    let running = HashSet::new();
    health.attach(result, &running);
    Ok(rollup.then(|| health.rollup(result, &dag.groups, &running, now, false)))
}

/// What the tick record at `db_path` says about the nodes of `dag` ([`Checks`]): two bounded
/// reads for the whole graph, never one per node. Cron is evaluated in `zone`. It depends on
/// the database, the graph and the ages read from that database, not on the clock, so a
/// server keeps it until one of them changes.
pub async fn checks(
    db_path: &str,
    dag: &barca_model::dag::Dag,
    ages: &HashMap<String, NodeAge>,
    zone: &Zone,
) -> Result<Checks, BarcaError> {
    let ticks = db::latest_ticks(db_path).await?;
    if ticks.outcomes.is_empty() {
        return Ok(Checks::default());
    }
    let misses = ticks.misses;
    let crons: HashMap<String, _> = barca_decide::schedule::jobs_from_summaries(
        barca_store::results::list_assets_from_dag(dag),
    )
    .into_iter()
    .map(|job| (job.id, job.cron))
    .collect();
    let mut by_node = HashMap::with_capacity(ticks.outcomes.len());
    // The newest successful row of each asset a tick's verdict could decide.
    let mut newest: HashMap<i64, &barca_model::model::DagNode> = HashMap::new();
    let mut redefined = HashSet::new();
    for (id, outcome) in ticks.outcomes {
        let (Some(node), Some(finished_at)) = (dag.get_node(&id), outcome.finished_at) else {
            continue;
        };
        let Some(at) = chrono::DateTime::from_timestamp(finished_at as i64, 0) else {
            continue;
        };
        let slot = outcome.scheduled_at;
        let trusted_until = crons.get(&outcome.schedule_id).and_then(|cron| {
            let next = barca_decide::schedule::next_fire(
                cron,
                zone,
                chrono::DateTime::from_timestamp(slot, 0)?,
            )?;
            let interval = next.timestamp() - slot;
            // The grace a late schedule gets (`crate::schedule_standing`).
            let grace = crate::schedule_standing::grace_seconds(Some(slot), Some(next.timestamp()));
            Some(finished_at + (finished_at - slot as f64).max(0.0) + (interval + grace) as f64)
        });
        if matches!(
            outcome.outcome,
            TickTargetOutcome::UpToDate | TickTargetOutcome::Rebuilt
        ) && node.kind() == barca_model::NodeKind::Asset
        {
            match ages.get(&id).and_then(|age| age.rows.as_ref()) {
                Some(rows) => {
                    newest.insert(rows.newest.0, node);
                }
                // No successful result on record: nothing to compare its definition with.
                None => {
                    redefined.insert(id.clone());
                }
            }
        }
        by_node.insert(
            id,
            Check {
                last: LastCheck {
                    at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    outcome: outcome.outcome,
                    schedule: outcome.schedule_id,
                },
                finished_at,
                trusted_until,
            },
        );
    }
    let rows: Vec<i64> = newest.keys().copied().collect();
    let recorded = db::components_of_rows(db_path, &rows).await?;
    for (row, node) in newest {
        if !recorded.get(&row).is_some_and(|recorded| {
            same_definition(
                recorded,
                &node.fingerprint.definition_hash,
                &node.fingerprint.components,
            )
        }) {
            redefined.insert(node.id.clone());
        }
    }
    Ok(Checks {
        by_node,
        redefined,
        in_flight: ticks.in_flight.into_iter().collect(),
        misses,
    })
}

/// Whether `recorded` (the run-hash components a materialization recorded, as JSON) names the
/// definition a node has now (`definition_hash`, `components`): its definition hash (rows
/// since 0.37.1), or, for an older row that lists every digest, the same definition components. A row that cannot be read does
/// not.
fn same_definition(recorded: &str, definition_hash: &str, components: &[(String, String)]) -> bool {
    let Ok(recorded) = serde_json::from_str::<HashMap<String, String>>(recorded) else {
        return false;
    };
    if let Some(definition) = recorded.get(barca_decide::changes::DEFINITION_KEY) {
        return definition == definition_hash;
    }
    // The components of a run hash that are not its definition's (`barca_decide::changes`).
    let own = |key: &str| {
        ["input ", "sensor ", "value ", "env "]
            .iter()
            .any(|prefix| key.starts_with(prefix))
    };
    let then: HashMap<&str, &str> = recorded
        .iter()
        .filter(|(key, _)| !own(key))
        .map(|(key, digest)| (key.as_str(), digest.as_str()))
        .collect();
    let now: HashMap<&str, &str> = components
        .iter()
        .map(|(key, digest)| (key.as_str(), digest.as_str()))
        .collect();
    !then.is_empty() && then == now
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{LastMaterialization, PartitionState};
    use barca_db::db::{RowChange, RowFacts};

    fn node(id: &str, inputs: &[&str], state: CacheState) -> NodeStatus {
        let mut n: NodeStatus = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "kind": "asset", "inputs": inputs, "partitioned": false,
            "cache": {"state": "unknown", "reason": "", "detail": ""},
            "last_materialization": null, "shape": null, "env": [],
        }))
        .unwrap();
        n.cache.state = state;
        n
    }

    fn last(status: &str) -> Option<LastMaterialization> {
        Some(LastMaterialization {
            skip_reason: None,
            content_hash: None,
            status: status.to_string(),
            created_at: "2026-11-16 10:00:00".to_string(),
            elapsed_seconds: None,
            run_hash: None,
            artifact: None,
            format: None,
            size_bytes: None,
            rows: None,
            partition: None,
            error: (status == "failed").then(|| "boom".to_string()),
        })
    }

    fn failed(mut n: NodeStatus) -> NodeStatus {
        n.last_materialization = last("failed");
        n
    }

    fn keys(mut n: NodeStatus, total: usize, missing: usize, failed: usize) -> NodeStatus {
        n.partitioned = true;
        n.partitions = Some(PartitionState {
            total,
            cached: total - missing,
            missing,
            missing_keys: Vec::new(),
            failed,
            failed_keys: Vec::new(),
            changed: None,
            empty: 0,
            blocked: 0,
            blocked_on: None,
            behind: missing - failed,
        });
        n
    }

    #[test]
    fn the_last_attempt_is_a_fact_of_the_row_whatever_its_health() {
        use crate::views::LastAttempt;
        let word = |mut n: NodeStatus| {
            classify(std::slice::from_mut(&mut n), Evidence::none(0));
            n.last_attempt
        };
        // No execution on record: a task that always runs reads `never_run`, as an asset does.
        assert_eq!(
            word(node("p.py:t", &[], CacheState::AlwaysRuns)),
            LastAttempt::NeverRun
        );
        assert_eq!(
            word(node("p.py:a", &[], CacheState::NeverRun)),
            LastAttempt::NeverRun
        );
        // It ran, and went well: `success`, though the node is stale now.
        let mut stale = node("p.py:a", &[], CacheState::Stale);
        stale.last_materialization = last("success");
        assert_eq!(word(stale), LastAttempt::Success);
        assert_eq!(
            word(failed(node("p.py:a", &[], CacheState::Stale))),
            LastAttempt::Failed
        );
        // A partitioned node's last materialization is one key: a failed key elsewhere counts.
        let mut some = keys(node("p.py:k", &[], CacheState::Partial), 4, 2, 1);
        some.last_materialization = last("success");
        assert_eq!(word(some), LastAttempt::Failed);
        let mut none = keys(node("p.py:k", &[], CacheState::Partial), 4, 2, 0);
        none.last_materialization = last("success");
        assert_eq!(word(none), LastAttempt::Success);
    }

    fn age(newest: (i64, &str), at: &str, rows: RowFacts) -> NodeAge {
        NodeAge {
            last_success_at: Some(at.to_string()),
            rows: Some(RowFacts {
                newest: (newest.0, Some(newest.1.to_string())),
                ..rows
            }),
            ..NodeAge::default()
        }
    }

    fn none() -> (HashMap<String, NodeAge>, HashSet<String>, HashSet<String>) {
        Default::default()
    }

    fn classified(mut nodes: Vec<NodeStatus>) -> Vec<NodeStatus> {
        let (ages, pinned, late) = none();
        classify(
            &mut nodes,
            Evidence {
                ages: &ages,
                pinned: &pinned,
                late: &late,
                checks: &Checks::default(),
                path: crate::refresh_path::PathEvidence::none(),
                timing: crate::estimates::Timing::none(),
                now: 0,
            },
        );
        nodes
    }

    /// The fixture of the TypeScript tests this module replaces: a failing source, a chain
    /// below it, and one node of each resting state.
    fn fixture() -> Vec<NodeStatus> {
        vec![
            failed(keys(
                node("a.py:raw", &[], CacheState::Partial),
                1205,
                100,
                100,
            )),
            node("a.py:clean", &["a.py:raw"], CacheState::Unknown),
            node("a.py:report", &["a.py:clean"], CacheState::Stale),
            {
                let mut n = node("b.py:orders", &[], CacheState::Cached);
                n.last_materialization = last("success");
                n
            },
            node("b.py:new", &[], CacheState::NeverRun),
            node("b.py:old", &[], CacheState::Stale),
        ]
    }

    fn word(nodes: &[NodeStatus], id: &str) -> (Health, HealthReason) {
        let n = nodes.iter().find(|n| n.id == id).unwrap();
        (n.health, n.health_reason)
    }

    #[test]
    fn the_twelve_rows_each_decide_a_health_and_name_themselves() {
        use HealthReason as R;
        let ok = last("success");
        let row = |n: &NodeStatus, blocked, signals: &[DataSignal], late| {
            decide(n, blocked, signals, late, false)
        };
        let empty = DataSignal::Empty {
            empty_keys: 1,
            keys: 1,
            previous_empty_keys: None,
        };
        let changed = DataSignal::RowsChanged {
            changed_keys: 1,
            key: None,
            rows: 3,
            previous_rows: 300,
        };
        let behind = DataSignal::BehindInput {
            input: "p.py:in".into(),
            input_success_at: "2026-11-16 09:00:00".into(),
            last_success_at: "2026-11-14 10:00:00".into(),
        };
        let cached = {
            let mut n = node("p.py:a", &[], CacheState::Cached);
            n.last_materialization = ok.clone();
            n
        };
        // 1: its own failure outranks everything, a failed key included.
        assert_eq!(
            row(
                &failed(cached.clone()),
                true,
                std::slice::from_ref(&empty),
                true
            ),
            (Health::Failing, R::LastAttemptFailed)
        );
        assert_eq!(
            row(&keys(cached.clone(), 10, 2, 2), false, &[], false),
            (Health::Failing, R::KeysFailed)
        );
        // 2, 3: a failing node upstream, then the cache state's own word for it.
        let stale = node("p.py:a", &[], CacheState::Stale);
        assert_eq!(
            row(&stale, true, std::slice::from_ref(&empty), true),
            (Health::Blocked, R::UpstreamFailing)
        );
        let blocked = node("p.py:a", &[], CacheState::Blocked);
        assert_eq!(
            row(&blocked, false, &[], false),
            (Health::Blocked, R::CacheBlocked)
        );
        let mut waits = node("p.py:a", &[], CacheState::Stale);
        waits.cache.reason = "upstream_failed".into();
        assert_eq!(
            row(&waits, false, &[], false),
            (Health::Blocked, R::CacheBlocked)
        );
        // 4: suspect data, `empty` before `rows_changed`.
        assert_eq!(
            row(&cached, false, &[empty.clone(), changed.clone()], true),
            (Health::Suspect, R::Empty)
        );
        assert_eq!(
            row(&cached, false, &[changed, behind.clone()], true),
            (Health::Suspect, R::RowsChanged)
        );
        // 5, 6: behind an input, then a late schedule.
        let unknown = node("p.py:a", &[], CacheState::Unknown);
        assert_eq!(
            row(&unknown, false, &[behind], true),
            (Health::Behind, R::BehindInput)
        );
        assert_eq!(
            row(&cached, false, &[], true),
            (Health::Behind, R::ScheduleLate)
        );
        // 7 to 12: the cache state.
        for (state, expected) in [
            (CacheState::Stale, (Health::Behind, R::Stale)),
            (CacheState::Partial, (Health::Behind, R::Partial)),
            (CacheState::Unsynced, (Health::Behind, R::Unsynced)),
            (CacheState::Unknown, (Health::Undecided, R::Unknown)),
            (CacheState::NeverRun, (Health::NeverRun, R::NeverRun)),
            (
                CacheState::AlwaysRuns,
                (Health::NeverRun, R::NoSuccessfulRun),
            ),
            (CacheState::Cached, (Health::Ok, R::Cached)),
        ] {
            assert_eq!(
                row(&node("p.py:a", &[], state), false, &[], false),
                expected
            );
        }
        let mut task = node("t.py:t", &[], CacheState::AlwaysRuns);
        task.kind = "task".into();
        task.last_materialization = ok;
        assert_eq!(
            row(&task, false, &[], false),
            (Health::Ok, R::LastRunSucceeded)
        );
    }

    #[test]
    fn every_cache_state_has_a_health_without_evidence() {
        for state in CacheState::ALL {
            let nodes = classified(vec![node("p.py:a", &[], state)]);
            assert_ne!(
                (state, nodes[0].health),
                (CacheState::Cached, Health::Undecided)
            );
            assert_eq!(nodes[0].measured, Some(false));
            assert!(nodes[0].signals.is_empty());
        }
    }

    #[test]
    fn a_nodes_health_is_one_word_from_its_state_and_the_facts_around_it() {
        let nodes = classified(fixture());
        assert_eq!(
            word(&nodes, "a.py:raw"),
            (Health::Failing, HealthReason::LastAttemptFailed)
        );
        // Blocked passes through an `unknown` node to the node below it.
        assert_eq!(
            word(&nodes, "a.py:clean"),
            (Health::Blocked, HealthReason::UpstreamFailing)
        );
        assert_eq!(
            word(&nodes, "a.py:report"),
            (Health::Blocked, HealthReason::UpstreamFailing)
        );
        assert_eq!(
            word(&nodes, "b.py:orders"),
            (Health::Ok, HealthReason::Cached)
        );
        assert_eq!(
            word(&nodes, "b.py:new"),
            (Health::NeverRun, HealthReason::NeverRun)
        );
        assert_eq!(
            word(&nodes, "b.py:old"),
            (Health::Behind, HealthReason::Stale)
        );
        // On its own, the `unknown` node is undecided.
        let alone = classified(vec![node("a.py:clean", &[], CacheState::Unknown)]);
        assert_eq!(
            word(&alone, "a.py:clean"),
            (Health::Undecided, HealthReason::Unknown)
        );
        // Most severe first: the order sorts.
        assert_eq!(
            [Health::Ok, Health::Behind, Health::Undecided]
                .into_iter()
                .min(),
            Some(Health::Behind)
        );
        assert!(Health::ALL.is_sorted());
    }

    #[test]
    fn blocked_names_the_nearest_failing_node_and_a_pin_stops_it() {
        let nodes = fixture();
        assert_eq!(
            blocked_by(&nodes, &HashSet::new()),
            HashMap::from([
                ("a.py:clean".to_string(), "a.py:raw".to_string()),
                ("a.py:report".to_string(), "a.py:raw".to_string()),
            ])
        );
        // A pinned asset is never blocked, and the failure above it does not pass it.
        assert_eq!(
            blocked_by(&nodes, &HashSet::from(["a.py:clean".to_string()])),
            HashMap::new()
        );
        // A fresh node between them stops it too; a failing node is failing, not blocked.
        let mut fresh = fixture();
        fresh[1].cache.state = CacheState::Cached;
        assert_eq!(blocked_by(&fresh, &HashSet::new()), HashMap::new());
        let mut both = fixture();
        both[2] = failed(both[2].clone());
        assert_eq!(
            blocked_by(&both, &HashSet::new()),
            HashMap::from([("a.py:clean".to_string(), "a.py:raw".to_string())])
        );
        // The first failing input, in the order `inputs` lists them; a cycle ends.
        let two = vec![
            failed(node("p.py:x", &[], CacheState::Stale)),
            failed(node("p.py:y", &[], CacheState::Stale)),
            node("p.py:z", &["p.py:x", "p.py:y"], CacheState::Stale),
        ];
        assert_eq!(blocked_by(&two, &HashSet::new())["p.py:z"], "p.py:x");
        let cycle = vec![
            node("p.py:a", &["p.py:b"], CacheState::Stale),
            node("p.py:b", &["p.py:a"], CacheState::Stale),
        ];
        assert_eq!(blocked_by(&cycle, &HashSet::new()), HashMap::new());
    }

    #[test]
    fn empty_and_changed_counts_become_signals_with_both_numbers() {
        let ages = HashMap::from([(
            "p.py:a".to_string(),
            age(
                (5, "r2"),
                "2026-11-16 10:00:00",
                RowFacts {
                    partitioned: true,
                    counted: 10,
                    empty: 3,
                    previous_empty: Some(0),
                    changed: 4,
                    largest_change: Some(RowChange {
                        key: Some("k=a".into()),
                        rows: 0,
                        previous_rows: 120,
                    }),
                    ..RowFacts::default()
                },
            ),
        )]);
        let a = node("p.py:a", &[], CacheState::Cached);
        assert_eq!(
            signals(&a, &ages, |_| true, false),
            vec![
                DataSignal::Empty {
                    empty_keys: 3,
                    keys: 10,
                    previous_empty_keys: Some(0)
                },
                DataSignal::RowsChanged {
                    changed_keys: 4,
                    key: Some("k=a".into()),
                    rows: 0,
                    previous_rows: 120
                },
            ]
        );
        // Facts without an empty key or a change say nothing; a task says nothing.
        let quiet = HashMap::from([(
            "p.py:a".to_string(),
            age((5, "r2"), "2026-11-16 10:00:00", RowFacts::default()),
        )]);
        assert!(signals(&a, &quiet, |_| true, false).is_empty());
        let mut task = node("p.py:a", &[], CacheState::AlwaysRuns);
        task.kind = "task".into();
        assert!(signals(&task, &ages, |_| true, false).is_empty());
    }

    #[test]
    fn an_unknown_fan_in_older_than_its_input_is_behind_it() {
        let ages = HashMap::from([
            (
                "p.py:all".to_string(),
                age((10, "r1"), "2026-11-14 10:00:00", RowFacts::default()),
            ),
            (
                "p.py:keys".to_string(),
                age((40, "r3"), "2026-11-16 09:00:00", RowFacts::default()),
            ),
            (
                "p.py:old".to_string(),
                age((4, "r0"), "2026-11-13 09:00:00", RowFacts::default()),
            ),
            (
                "p.py:sensor".to_string(),
                age((50, "r4"), "2026-11-16 09:30:00", RowFacts::default()),
            ),
        ]);
        let inputs = ["p.py:keys", "p.py:old", "p.py:sensor"];
        let is_asset = |id: &str| id != "p.py:sensor";
        assert_eq!(
            signals(
                &node("p.py:all", &inputs, CacheState::Unknown),
                &ages,
                is_asset,
                false
            ),
            vec![DataSignal::BehindInput {
                input: "p.py:keys".into(),
                input_success_at: "2026-11-16 09:00:00".into(),
                last_success_at: "2026-11-14 10:00:00".into(),
            }]
        );
        // A state that already says what to do, or that it is current, raises nothing.
        for state in [CacheState::Stale, CacheState::Cached, CacheState::Partial] {
            assert!(signals(&node("p.py:all", &inputs, state), &ages, is_asset, false).is_empty());
        }
        // An input written later in the same run is not newer data.
        let same_run = HashMap::from([
            (
                "p.py:all".to_string(),
                age((10, "r1"), "2026-11-14 10:00:00", RowFacts::default()),
            ),
            (
                "p.py:keys".to_string(),
                age((12, "r1"), "2026-11-14 10:00:01", RowFacts::default()),
            ),
        ]);
        assert!(
            signals(
                &node("p.py:all", &inputs, CacheState::Unknown),
                &same_run,
                is_asset,
                false
            )
            .is_empty()
        );
    }

    #[test]
    fn coverage_counts_what_was_measured_and_an_unmeasured_asset_is_not_ok_for_data() {
        let counted = |empty| RowFacts {
            counted: 1,
            empty,
            ..RowFacts::default()
        };
        let ages = HashMap::from([
            (
                "p.py:a".to_string(),
                age((1, "r1"), "2026-11-16 10:00:00", counted(0)),
            ),
            (
                "p.py:b".to_string(),
                age((2, "r1"), "2026-11-16 10:00:00", counted(1)),
            ),
            // A result no worker counted: recorded, not measured.
            (
                "p.py:c".to_string(),
                age((3, "r1"), "2026-11-16 10:00:00", RowFacts::default()),
            ),
        ]);
        let mut task = node("p.py:t", &[], CacheState::AlwaysRuns);
        task.kind = "task".into();
        let mut result = StatusResult {
            disk_usage: Default::default(),
            target: None,
            targets: Vec::new(),
            nodes: vec![
                node("p.py:a", &[], CacheState::Cached),
                node("p.py:b", &[], CacheState::Cached),
                node("p.py:c", &[], CacheState::Cached),
                node("p.py:d", &[], CacheState::NeverRun),
                task,
            ],
            summary: Default::default(),
            data_coverage: Default::default(),
            as_of: None,
            rollup: None,
            filter: None,
        };
        let (_, pinned, late) = none();
        attach(
            &mut result,
            Evidence {
                ages: &ages,
                pinned: &pinned,
                late: &late,
                checks: &Checks::default(),
                path: crate::refresh_path::PathEvidence::none(),
                timing: crate::estimates::Timing::none(),
                now: 0,
            },
            &HashSet::new(),
        );
        assert_eq!(
            result.data_coverage,
            DataCoverage {
                assets: 4,
                measured: 2,
                suspect: 1
            }
        );
        assert_eq!(
            result.data_coverage.line(),
            "2 of 4 assets measured, 1 suspect"
        );
        let measured: Vec<Option<bool>> = result.nodes.iter().map(|n| n.measured).collect();
        assert_eq!(
            measured,
            [Some(true), Some(true), Some(false), Some(false), None]
        );
        assert_eq!(result.nodes[1].health, Health::Suspect);
        // 7 of 556: the unmeasured ones are counted as assets and nothing else.
        let many = DataCoverage {
            assets: 556,
            measured: 7,
            suspect: 0,
        };
        assert_eq!(many.line(), "7 of 556 assets measured");
    }

    #[test]
    fn the_enum_words_are_the_ones_the_wire_carries() {
        for health in Health::ALL {
            assert_eq!(
                serde_json::to_value(health).unwrap(),
                serde_json::json!(health.as_str())
            );
        }
        for reason in [
            HealthReason::LastAttemptFailed,
            HealthReason::KeysFailed,
            HealthReason::UpstreamFailing,
            HealthReason::CacheBlocked,
            HealthReason::Empty,
            HealthReason::RowsChanged,
            HealthReason::BehindInput,
            HealthReason::ScheduleLate,
            HealthReason::Stale,
            HealthReason::Partial,
            HealthReason::Unsynced,
            HealthReason::Unknown,
            HealthReason::NeverRun,
            HealthReason::NoSuccessfulRun,
            HealthReason::LastRunSucceeded,
            HealthReason::Cached,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::json!(reason.as_str())
            );
        }
        assert_eq!(Health::Suspect.word(), "suspect data");
        assert_eq!(Health::NeverRun.word(), "never run");
        assert!(Health::Suspect.is_fresh() && !Health::Behind.is_fresh());
    }

    /// A check of `id` that ended at `finished_at`, trusted for ten minutes after.
    fn check(outcome: TickTargetOutcome, finished_at: i64) -> Check {
        Check {
            last: LastCheck {
                at: chrono::DateTime::from_timestamp(finished_at, 0)
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                outcome,
                schedule: "p.py:clock".to_string(),
            },
            finished_at: finished_at as f64 + 0.5,
            trusted_until: Some(finished_at as f64 + 600.0),
        }
    }

    /// A sensor every tick polls, a cutoff asset below it the read path says would run, and
    /// a report below that it cannot decide: the audit case. Rows recorded at `window_at`
    /// (the cutoff asset's newest) and long ago (the report's last success).
    fn undecided_below_a_cutoff(window_at: &str) -> Vec<NodeStatus> {
        let at = |mut n: NodeStatus, created_at: &str| {
            n.last_materialization = last("success");
            n.last_materialization.as_mut().unwrap().created_at = created_at.to_string();
            n
        };
        let mut clock = at(node("p.py:clock", &[], CacheState::AlwaysRuns), window_at);
        clock.kind = "sensor".into();
        vec![
            clock,
            at(
                node("p.py:window", &["p.py:clock"], CacheState::Stale),
                window_at,
            ),
            at(
                node("p.py:report", &["p.py:window"], CacheState::Unknown),
                "2026-11-10 08:00:00",
            ),
        ]
    }

    // 2026-11-16 10:00:00 UTC.
    const TICK_ENDED: i64 = 1_794_823_200;

    fn classified_with(mut nodes: Vec<NodeStatus>, checks: &Checks, now: i64) -> Vec<NodeStatus> {
        let (ages, pinned, late) = none();
        classify(
            &mut nodes,
            Evidence {
                ages: &ages,
                pinned: &pinned,
                late: &late,
                checks,
                path: crate::refresh_path::PathEvidence::none(),
                timing: crate::estimates::Timing::none(),
                now,
            },
        );
        nodes
    }

    fn checked(outcome: TickTargetOutcome) -> Checks {
        Checks {
            by_node: HashMap::from([
                ("p.py:report".to_string(), check(outcome, TICK_ENDED)),
                (
                    "p.py:clock".to_string(),
                    check(TickTargetOutcome::Rebuilt, TICK_ENDED),
                ),
            ]),
            ..Checks::default()
        }
    }

    #[test]
    fn a_node_the_read_path_cannot_decide_takes_the_verdict_of_the_tick_that_checked_it() {
        use TickTargetOutcome::*;
        assert_eq!(
            chrono::DateTime::from_timestamp(TICK_ENDED, 0)
                .unwrap()
                .to_rfc3339(),
            "2026-11-16T10:00:00+00:00"
        );
        let report = |nodes: &[NodeStatus]| word(nodes, "p.py:report");
        // The tick polled the sensor and rebuilt the cutoff asset a second before it ended.
        let rows = || undecided_below_a_cutoff("2026-11-16 09:59:59");
        let now = TICK_ENDED + 180;

        // No tick record: as before.
        let none = classified_with(rows(), &Checks::default(), now);
        assert_eq!(report(&none), (Health::Undecided, HealthReason::Unknown));
        assert_eq!(none[2].last_check, None);

        // Found up to date, or rebuilt: ok, and every node says what its last check did.
        for outcome in [UpToDate, Rebuilt] {
            let got = classified_with(rows(), &checked(outcome), now);
            assert_eq!(report(&got), (Health::Ok, HealthReason::CheckedUpToDate));
            let last = got[2].last_check.as_ref().unwrap();
            assert_eq!(
                (last.at.as_str(), last.outcome, last.schedule.as_str()),
                ("2026-11-16T10:00:00Z", outcome, "p.py:clock")
            );
            // A sensor's check is carried too, and decides nothing: it is not undecided.
            assert_eq!(got[0].last_check.as_ref().unwrap().outcome, Rebuilt);
            assert_eq!(got[0].health_reason, HealthReason::LastRunSucceeded);
            // The cutoff asset is decided by the read path: its check changes nothing.
            assert_eq!(
                word(&got, "p.py:window"),
                (Health::Behind, HealthReason::Stale)
            );
        }
        assert_eq!(
            checked(UpToDate).by_node["p.py:report"].last.line(now),
            "checked 3m ago, up to date"
        );
        assert_eq!(
            checked(Rebuilt).by_node["p.py:report"]
                .last
                .line(now + 3600),
            "checked 1h ago, rebuilt"
        );

        // A newer check that failed, or did not reach it, decides nothing.
        for outcome in [Failed, NotReached] {
            let got = classified_with(rows(), &checked(outcome), now);
            assert_eq!(report(&got), (Health::Undecided, HealthReason::Unknown));
            assert_eq!(got[2].last_check.as_ref().unwrap().outcome, outcome);
        }

        // Something above it was recorded after the tick ended: changed since.
        let later = classified_with(
            undecided_below_a_cutoff("2026-11-16 10:01:00"),
            &checked(UpToDate),
            now,
        );
        assert_eq!(report(&later), (Health::Undecided, HealthReason::Unknown));
        // Unless a tick's run still in flight recorded it: its verdict is on its way.
        let ages = HashMap::from(["p.py:clock", "p.py:window"].map(|id| {
            (
                id.to_string(),
                NodeAge {
                    last_run: Some(barca_db::db::RunTotals {
                        run_id: "run-next".to_string(),
                        elapsed_seconds: None,
                        keys: 1,
                        calls: 0,
                        failed: 0,
                    }),
                    ..NodeAge::default()
                },
            )
        }));
        let in_flight = Checks {
            in_flight: HashSet::from(["run-next".to_string()]),
            ..checked(UpToDate)
        };
        let (pinned, late) = (HashSet::new(), HashSet::new());
        let mut running = undecided_below_a_cutoff("2026-11-16 10:01:00");
        classify(
            &mut running,
            Evidence {
                ages: &ages,
                pinned: &pinned,
                late: &late,
                checks: &in_flight,
                path: crate::refresh_path::PathEvidence::none(),
                timing: crate::estimates::Timing::none(),
                now,
            },
        );
        assert_eq!(
            report(&running),
            (Health::Ok, HealthReason::CheckedUpToDate)
        );

        // Its definition is not the one its last success recorded: edited since.
        let edited = Checks {
            redefined: HashSet::from(["p.py:report".to_string()]),
            ..checked(UpToDate)
        };
        assert_eq!(
            report(&classified_with(rows(), &edited, now)),
            (Health::Undecided, HealthReason::Unknown)
        );

        // The verdict expires, and one of a schedule that is gone never stands.
        let expired = classified_with(rows(), &checked(UpToDate), TICK_ENDED + 601);
        assert_eq!(report(&expired), (Health::Undecided, HealthReason::Unknown));
        assert!(expired[2].last_check.is_some());
        let mut gone = checked(UpToDate);
        gone.by_node.get_mut("p.py:report").unwrap().trusted_until = None;
        assert_eq!(
            report(&classified_with(rows(), &gone, now)),
            (Health::Undecided, HealthReason::Unknown)
        );

        // Only `unknown` asks: a failure, or a state the read path decided, keeps its row.
        let mut failing = rows();
        failing[2].last_materialization = last("failed");
        assert_eq!(
            report(&classified_with(failing, &checked(UpToDate), now)).0,
            Health::Failing
        );
        let mut stale = rows();
        stale[2].cache.state = CacheState::Stale;
        assert_eq!(
            report(&classified_with(stale, &checked(UpToDate), now)),
            (Health::Behind, HealthReason::Stale)
        );
    }

    /// A node whose verdict stands is not behind an input the tick rebuilt: the tick read
    /// that result and found the node up to date.
    #[test]
    fn a_standing_verdict_silences_behind_input() {
        let ages = HashMap::from([
            (
                "p.py:window".to_string(),
                age((9, "r9"), "2026-11-16 09:59:59", RowFacts::default()),
            ),
            (
                "p.py:report".to_string(),
                age((4, "r1"), "2026-11-10 08:00:00", RowFacts::default()),
            ),
        ]);
        let (pinned, late) = (HashSet::new(), HashSet::new());
        let run = |checks: &Checks| {
            let mut nodes = undecided_below_a_cutoff("2026-11-16 09:59:59");
            classify(
                &mut nodes,
                Evidence {
                    ages: &ages,
                    pinned: &pinned,
                    late: &late,
                    checks,
                    path: crate::refresh_path::PathEvidence::none(),
                    timing: crate::estimates::Timing::none(),
                    now: TICK_ENDED + 180,
                },
            );
            nodes.remove(2)
        };
        let before = run(&Checks::default());
        assert_eq!(
            (before.health, before.health_reason),
            (Health::Behind, HealthReason::BehindInput)
        );
        let after = run(&checked(TickTargetOutcome::UpToDate));
        assert_eq!(
            (after.health, after.health_reason),
            (Health::Ok, HealthReason::CheckedUpToDate)
        );
        assert!(after.signals.is_empty());
    }

    #[test]
    fn a_recorded_definition_is_compared_by_hash_or_by_its_components() {
        let components = vec![
            ("code".to_string(), "c1".to_string()),
            ("kind".to_string(), "k1".to_string()),
        ];
        let same = |recorded: &str| same_definition(recorded, "abc", &components);
        assert!(same(r#"{"@definition":"abc","input p.py:x":"i"}"#));
        assert!(!same(r#"{"@definition":"abd"}"#));
        // A row from before definitions were named lists every digest.
        assert!(same(r#"{"code":"c1","kind":"k1","input p.py:x":"i"}"#));
        assert!(!same(r#"{"code":"c2","kind":"k1"}"#));
        assert!(!same(r#"{"code":"c1","kind":"k1","helper p.py:f":"h"}"#));
        assert!(!same(r#"{"input p.py:x":"i"}"#));
        assert!(!same(""));
    }
}
