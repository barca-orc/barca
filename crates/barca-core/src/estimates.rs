//! When a node went out of date and when it will be fresh again (`specs/behavior/health.md`,
//! "Times"; #962): `stale_since`, `expected_fresh_at` and `recheck_at` of every row of one
//! status result, and the estimated start of every run in the run queue.
//!
//! [`times`] is called by [`crate::health::classify`], once per status result, over rows it
//! has already classified. It reads no database: what it needs beyond the rows is
//! [`Timing`], which the caller gathers once for the scope (typical step durations, the runs
//! in flight, the run queue, the worker capacity, when each source file last changed).
//!
//! An estimate promises only what barca will do. A time that is not known is `None`, never a
//! guess, and `expected_fresh_at` is given only where a run will leave the node fresh: where
//! a tick can only look again (an undecided node, a node above it that no tick refreshes),
//! the node has `recheck_at` alone.

use crate::cache_state::CacheState;
use crate::health::{DataSignal, Evidence, Health, HealthReason, recorded_at};
use crate::status::NodeStatus;
use std::collections::{HashMap, HashSet};

/// Estimated times are rounded up to a multiple of this many seconds: an estimate is not
/// known to the second.
pub const ROUND_SECONDS: i64 = 5;

/// A run in flight, as an estimate reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlannedRun {
    pub run_id: String,
    /// The scheduled node whose tick started it; `None` for a run someone asked for.
    pub schedule: Option<String>,
    /// The nodes it computes when they are out of date: its targets and everything above
    /// them. `None` for a run of every node. Not read for a tick's run, whose nodes are its
    /// schedule's targets ([`crate::refresh_path::Paths`]).
    pub nodes: Option<HashSet<String>>,
    /// It is executing; false while it waits for a run slot.
    pub running: bool,
    /// When what it has left starts, unix seconds: for a running run its latest progress
    /// (its start, or the last step it finished); for a waiting run its estimated start
    /// ([`queue_starts`]). `None` when that is not known (a paused or draining queue).
    pub start: Option<i64>,
}

/// What the times are read from beyond the rows and [`Evidence`]: gathered once per status
/// result, never per node.
#[derive(Clone, Copy)]
pub struct Timing<'a> {
    /// Each node's typical step duration, seconds: the median of its recent successful runs
    /// (one key's, for a partitioned node). A node without samples takes the median of these.
    pub durations: &'a HashMap<String, f64>,
    /// The runs in flight: executing, or waiting in the run queue.
    pub runs: &'a [PlannedRun],
    /// When a run that joined the run queue now would start, unix seconds ([`queue_starts`]);
    /// `None` when a run slot is free.
    pub queue_clear_at: Option<i64>,
    /// The run queue starts runs: false while it is paused or draining, when no start can be
    /// estimated for a run that joins it.
    pub queue_open: bool,
    /// How many steps of one run execute at once (the worker pool).
    pub workers: usize,
    /// When each source file was last modified, unix seconds, by the file part of a node id.
    pub source_times: &'a HashMap<String, i64>,
    /// How many runs the server executes at once (`--max-runs`, `[server] max_runs`), where
    /// the answering process knows: what a `queue` root names. `None` when it does not (no
    /// server here and none configured: the default is one per core of the serving machine).
    pub max_runs: Option<usize>,
}

impl Timing<'static> {
    /// Nothing is known: no durations, no run in flight, no source times.
    pub fn none() -> Self {
        static DURATIONS: std::sync::LazyLock<HashMap<String, f64>> =
            std::sync::LazyLock::new(HashMap::new);
        static TIMES: std::sync::LazyLock<HashMap<String, i64>> =
            std::sync::LazyLock::new(HashMap::new);
        Self {
            durations: &DURATIONS,
            runs: &[],
            queue_clear_at: None,
            queue_open: true,
            workers: 1,
            source_times: &TIMES,
            max_runs: None,
        }
    }
}

/// The three times of one node, RFC 3339 UTC.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Times {
    pub stale_since: Option<String>,
    pub expected_fresh_at: Option<String>,
    pub recheck_at: Option<String>,
}

/// The source file of a node id: what is before its last `:`.
fn source_file(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(file, _)| file)
}

/// When each of `files` (the file part of node ids, relative to the working directory) was
/// last modified, unix seconds. A file that cannot be read is absent.
pub fn source_times<'a>(files: impl IntoIterator<Item = &'a str>) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for file in files {
        if out.contains_key(file) {
            continue;
        }
        let modified = std::fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        if let Some(at) = modified {
            out.insert(file.to_string(), at.as_secs() as i64);
        }
    }
    out
}

/// [`source_times`] of the files the nodes of `dag` are declared in.
pub fn source_times_of(dag: &crate::dag::Dag) -> HashMap<String, i64> {
    source_times(dag.topo_order().into_iter().map(source_file))
}

fn rfc3339(at: i64) -> Option<String> {
    crate::time::rfc3339_secs(at)
}

/// `at` rounded up to [`ROUND_SECONDS`], and never before `now`.
pub fn rounded(at: i64, now: i64) -> i64 {
    let at = at.max(now);
    at.div_euclid(ROUND_SECONDS) * ROUND_SECONDS
        + if at.rem_euclid(ROUND_SECONDS) == 0 {
            0
        } else {
            ROUND_SECONDS
        }
}

/// The median of the typical durations: what a node without samples takes. 0 with none.
pub(crate) fn project_median(durations: &HashMap<String, f64>) -> f64 {
    let values: Vec<f64> = durations.values().copied().collect();
    crate::schedule_standing::median(&values)
}

/// How long a run takes to compute `node`, seconds: its typical duration, once per key that
/// has no result for a partitioned node.
pub(crate) fn work(node: &NodeStatus, durations: &HashMap<String, f64>, default: f64) -> f64 {
    let one = durations.get(&node.id).copied().unwrap_or(default).max(0.0);
    let keys = node
        .partitions
        .as_ref()
        .filter(|_| node.partitioned)
        .map_or(1, |p| p.total.saturating_sub(p.cached).max(1));
    one * keys as f64
}

/// When `input`'s result first changed after `own` (unix seconds: when the node reading it
/// last succeeded): the earliest of its recorded value changes ([`crate::db::NodeAge::changed_at`])
/// that is newer. A sensor polled again with the same observation, and a rerun that wrote
/// the same bytes, add a result and no change, so this does not move while the reader stays
/// out of date. Where no change is on record (rows that recorded no digest), the input's
/// newest result, when that is newer. `None` when nothing of the input is newer than `own`.
fn changed_after(input: &str, own: i64, evidence: Evidence<'_>) -> Option<i64> {
    let age = evidence.ages.get(input)?;
    let newer = |at: &str| Some(recorded_at(at)).filter(|at| *at != i64::MAX && *at > own);
    if age.changed_at.is_empty() {
        return age.last_success_at.as_deref().and_then(newer);
    }
    let first = age.changed_at.partition_point(|at| recorded_at(at) <= own);
    age.changed_at.get(first).and_then(|at| newer(at))
}

/// When `node` itself went out of date, from its own evidence: unix seconds, `None` when no
/// record holds the time.
fn own_stale_since(node: &NodeStatus, evidence: Evidence<'_>) -> Option<i64> {
    let age = evidence.ages.get(&node.id);
    let last_success = age
        .and_then(|a| a.last_success_at.as_deref())
        .map(recorded_at)
        .filter(|at| *at != i64::MAX);
    match node.health_reason {
        // A failure: the first failed attempt after its last clean run.
        HealthReason::LastAttemptFailed | HealthReason::KeysFailed => age
            .and_then(|a| a.failing_since.as_deref())
            .or(node
                .last_materialization
                .as_ref()
                .filter(|m| m.status == "failed")
                .map(|m| m.created_at.as_str()))
            .map(recorded_at)
            .filter(|at| *at != i64::MAX),
        // Held by something above: the time is that node's.
        HealthReason::UpstreamFailing | HealthReason::CacheBlocked => None,
        // A late schedule: the first slot no tick fired for.
        HealthReason::ScheduleLate => evidence
            .path
            .schedules
            .get(&node.id)
            .and_then(|s| s.standing.as_ref())
            .and_then(|s| s.late_since),
        // An input's newer result: when that input first changed after the node's own.
        HealthReason::BehindInput => node
            .signals
            .iter()
            .filter_map(|signal| match signal {
                DataSignal::BehindInput {
                    input,
                    input_success_at,
                    ..
                } => last_success
                    .and_then(|own| changed_after(input, own, evidence))
                    .or(Some(recorded_at(input_success_at)).filter(|at| *at != i64::MAX)),
                _ => None,
            })
            .min(),
        HealthReason::Stale | HealthReason::NeverRun | HealthReason::NoSuccessfulRun => {
            let causes = node.cache.change_causes.as_deref().unwrap_or(&[]);
            let has = |cause: &str| causes.iter().any(|c| c == cause);
            let mut candidates: Vec<i64> = Vec::new();
            // An input (a sensor's observation, an asset's result) that changed after the
            // node's own newest result: the first such change, which stays put however often
            // the input is polled or rewritten with the same value afterwards.
            if let Some(own) = last_success.filter(|_| has("inputs_changed")) {
                candidates.extend(
                    node.inputs
                        .iter()
                        .filter_map(|input| changed_after(input, own, evidence)),
                );
            }
            // Its code changed, or it was never run: no record holds when the definition was
            // first seen, so the source file's modification time stands for it, when that is
            // not before the node's newest result (results are recorded to the second). It is never before the change (a later edit
            // elsewhere in the file moves it forward), so the node is out of date for at
            // least that long.
            let never_ran = last_success.is_none();
            if (has("code_changed") || never_ran)
                && let Some(modified) = evidence
                    .timing
                    .source_times
                    .get(source_file(&node.id))
                    .copied()
                    .filter(|at| last_success.is_none_or(|own| *at >= own))
            {
                candidates.push(modified);
            }
            candidates.into_iter().min()
        }
        // No record says when a key was added or a copy went missing; a fresh or undecided
        // node has no time.
        _ => None,
    }
}

/// `stale_since` of every row, unix seconds: its own evidence ([`own_stale_since`]) or, when
/// earlier, that of a node above it that is not fresh (a node out of date because of what is
/// above it has been so since that went out of date). `None` for a fresh or undecided node
/// and where no record holds a time. `nodes` are in dependency order.
fn stale_since(nodes: &[NodeStatus], evidence: Evidence<'_>) -> Vec<Option<i64>> {
    let mut through: HashMap<&str, Option<i64>> = HashMap::with_capacity(nodes.len());
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        if node.health.is_fresh() {
            out.push(None);
            continue;
        }
        let above = node
            .inputs
            .iter()
            .filter_map(|input| through.get(input.as_str()).copied().flatten())
            .min();
        let own = own_stale_since(node, evidence);
        let since = match node.health {
            // A failure is dated by its own first failed attempt.
            Health::Failing => own,
            _ => own.into_iter().chain(above).min(),
        }
        .map(|at| at.min(evidence.now));
        through.insert(node.id.as_str(), since);
        out.push(
            (node.health != Health::Undecided)
                .then_some(since)
                .flatten(),
        );
    }
    out
}

/// Whether a run that computes `node` leaves it fresh, as far as the node itself says: it is
/// out of date in a way a run decides. Not a failing or blocked node (its cause is the
/// failure), and not one the read path cannot decide (`unknown`: the run may find it up to
/// date or leave it).
fn promisable(node: &NodeStatus) -> bool {
    matches!(node.health, Health::Behind | Health::NeverRun)
        && node.cache.state != CacheState::Unknown
}

/// For each node of `nodes` that `member` says a run computes and that is not fresh: how
/// long after the run starts it is expected to be done, seconds; `None` where the run may
/// not leave it fresh (it or a node above it is not [`promisable`], or a node above it that
/// is not fresh is not computed by the run).
///
/// The model: a step starts when its inputs are done and a worker is free. Steps are taken in
/// the order they become ready on the longest chain of typical durations above them; a step
/// also waits until the work taken before it, divided over `workers`, is done. With enough
/// workers that is the longest chain; with one worker it is the sum of the work before it.
/// Nodes that run on every tick (`always_runs`: a sensor's poll, a task) count as work.
fn finish_after<'a>(
    nodes: &'a [NodeStatus],
    member: impl Fn(&NodeStatus) -> bool,
    timing: Timing<'_>,
    default: f64,
) -> HashMap<&'a str, Option<f64>> {
    struct Step<'a> {
        node: &'a NodeStatus,
        work: f64,
        /// Done on the longest chain alone, seconds after the start.
        ready: f64,
        ok: bool,
    }
    let fresh: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.health.is_fresh())
        .map(|n| n.id.as_str())
        .collect();
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut steps: Vec<Step<'a>> = Vec::new();
    let mut chain_done: Vec<f64> = Vec::new();
    for node in nodes {
        let runs = !fresh.contains(node.id.as_str()) || node.cache.state == CacheState::AlwaysRuns;
        if !runs || !member(node) {
            continue;
        }
        let mut ready: f64 = 0.0;
        // A node that is fresh and runs anyway (a sensor's poll) is work, and promises
        // nothing itself.
        let mut ok = fresh.contains(node.id.as_str()) || promisable(node);
        for input in &node.inputs {
            match index.get(input.as_str()) {
                Some(&at) => {
                    ready = ready.max(chain_done[at]);
                    ok &= steps[at].ok;
                }
                // Not computed by this run: fine when it is fresh, else the node stays out
                // of date against it.
                None => ok &= fresh.contains(input.as_str()),
            }
        }
        let work = work(node, timing.durations, default);
        index.insert(node.id.as_str(), steps.len());
        chain_done.push(ready + work);
        steps.push(Step {
            node,
            work,
            ready,
            ok,
        });
    }
    // The order steps are taken in: as they become ready, then as the rows are.
    let mut order: Vec<usize> = (0..steps.len()).collect();
    order.sort_by(|a, b| steps[*a].ready.total_cmp(&steps[*b].ready).then(a.cmp(b)));
    let workers = timing.workers.max(1) as f64;
    let mut before = vec![0.0; steps.len()];
    let mut taken = 0.0;
    for at in order {
        before[at] = taken / workers;
        taken += steps[at].work;
    }
    let mut done = vec![0.0_f64; steps.len()];
    let mut out = HashMap::with_capacity(steps.len());
    for (at, step) in steps.iter().enumerate() {
        let inputs = step
            .node
            .inputs
            .iter()
            .filter_map(|input| index.get(input.as_str()))
            .map(|&input| done[input])
            .fold(0.0, f64::max);
        done[at] = inputs.max(before[at]) + step.work;
        if !fresh.contains(step.node.id.as_str()) {
            out.insert(step.node.id.as_str(), step.ok.then_some(done[at]));
        }
    }
    out
}

/// The times of every row of `nodes`, which [`crate::health::classify`] has given their
/// health, signals and refresh path: one status result, in dependency order.
///
/// - `stale_since`: [`stale_since`].
/// - `recheck_at`: the next tick of the node's refresh path, for every node that has one.
/// - `expected_fresh_at`, for a node that is not fresh, not failing, blocked or undecided,
///   and that a run will leave fresh ([`finish_after`]): the start of that run plus the
///   node's place in it. The run is, first that applies: a running run that computes the
///   node (from its latest progress); a waiting run that computes it (from its estimated
///   start), unless a tick already missed the node; the next tick of its refresh path (the
///   slot plus the schedule's typical queue wait, or when the run queue clears if later),
///   unless a tick already missed it.
pub fn times(nodes: &[NodeStatus], evidence: Evidence<'_>) -> Vec<Times> {
    let timing = evidence.timing;
    let now = evidence.now;
    let since = stale_since(nodes, evidence);
    let mut out: Vec<Times> = since
        .into_iter()
        .map(|since| Times {
            stale_since: since.and_then(rfc3339),
            ..Times::default()
        })
        .collect();
    let at: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    // The schedule of each node's path, and its next tick.
    let paths: Vec<Option<(&str, &crate::refresh_path::PathSchedule)>> = nodes
        .iter()
        .map(|n| crate::refresh_path::path_schedule(&n.id, evidence.path))
        .collect();
    for (times, path) in out.iter_mut().zip(&paths) {
        times.recheck_at = path.and_then(|(_, s)| s.next_tick).and_then(rfc3339);
    }
    if nodes.iter().all(|n| n.health.is_fresh()) {
        return out;
    }
    let default = project_median(timing.durations);
    let mut expected: Vec<Option<i64>> = vec![None; nodes.len()];
    let give = |finish: &HashMap<&str, Option<f64>>,
                start: i64,
                wanted: &dyn Fn(usize) -> bool,
                expected: &mut Vec<Option<i64>>| {
        for (id, after) in finish {
            let (Some(&i), Some(after)) = (at.get(id), after) else {
                continue;
            };
            if expected[i].is_none() && wanted(i) {
                expected[i] = Some(start + after.ceil() as i64);
            }
        }
    };
    // Runs in flight first: running ones, then waiting ones in the order they start.
    let mut runs: Vec<&PlannedRun> = timing.runs.iter().filter(|r| r.start.is_some()).collect();
    runs.sort_by_key(|r| (!r.running, r.start));
    // A node a tick's run reads as it is and does not compute: paused, pinned or `Manual`.
    // No estimate is given for one and none passes through one that is not fresh
    // ([`finish_after`]), whether the tick is the next one or already has a run in flight.
    let held = |id: &str| {
        evidence.path.paused.contains(id)
            || evidence.path.pinned.contains(id)
            || evidence.path.paths.is_manual(id)
    };
    for run in runs {
        let member = |n: &NodeStatus| match (&run.schedule, &run.nodes) {
            (Some(schedule), _) => {
                evidence
                    .path
                    .paths
                    .schedules(&n.id)
                    .iter()
                    .any(|s| s == schedule)
                    && !held(&n.id)
            }
            (None, Some(planned)) => planned.contains(&n.id),
            (None, None) => true,
        };
        let finish = finish_after(nodes, member, timing, default);
        // A waiting run does not clear a miss: the node's estimate already failed.
        let missed = |i: usize| {
            nodes[i]
                .refresh_path
                .as_ref()
                .is_some_and(|p| p.missed_ticks > 0)
        };
        give(
            &finish,
            run.start.unwrap_or(now),
            &|i| run.running || !missed(i),
            &mut expected,
        );
    }
    // Then the next tick of each node's path.
    let mut schedules: Vec<&str> = paths
        .iter()
        .zip(&expected)
        .zip(nodes)
        .filter(|((_, got), n)| got.is_none() && !n.health.is_fresh())
        .filter_map(|((path, _), _)| {
            path.filter(|(_, s)| s.next_tick.is_some())
                .map(|(id, _)| id)
        })
        .collect();
    schedules.sort_unstable();
    schedules.dedup();
    if !timing.queue_open {
        // A tick's run would join a queue that starts nothing: no start to estimate from.
        schedules.clear();
    }
    for schedule in schedules {
        let Some(state) = evidence.path.schedules.get(schedule) else {
            continue;
        };
        let Some(tick) = state.next_tick else {
            continue;
        };
        let wait = state
            .standing
            .as_ref()
            .map_or(0.0, |s| s.typical_queue_wait_seconds);
        let start = (tick + wait.ceil() as i64).max(timing.queue_clear_at.unwrap_or(i64::MIN));
        let member = |n: &NodeStatus| {
            evidence
                .path
                .paths
                .schedules(&n.id)
                .iter()
                .any(|s| s == schedule)
                && !held(&n.id)
        };
        let finish = finish_after(nodes, member, timing, default);
        give(
            &finish,
            start,
            &|i| {
                paths[i].is_some_and(|(id, _)| id == schedule)
                    && nodes[i]
                        .refresh_path
                        .as_ref()
                        .is_some_and(|p| p.missed_ticks == 0)
            },
            &mut expected,
        );
    }
    for (times, at) in out.iter_mut().zip(expected) {
        times.expected_fresh_at = at.map(|at| rounded(at, now)).and_then(rfc3339);
    }
    out
}

/// `behind_seconds` of the freshness gap: how long the node has been out of date as of `now`
/// (unix seconds); 0 when `stale_since` is `null`.
pub fn behind_seconds(node: &NodeStatus, now: i64) -> i64 {
    node.stale_since
        .as_deref()
        .and_then(crate::time::parse_rfc3339)
        .map_or(0, |since| (now - since).max(0))
}

/// `ahead_seconds` of the freshness gap: how long until the node is expected fresh as of `now`
/// (unix seconds); 0 when `expected_fresh_at` is `null`.
pub fn ahead_seconds(node: &NodeStatus, now: i64) -> i64 {
    node.expected_fresh_at
        .as_deref()
        .and_then(crate::time::parse_rfc3339)
        .map_or(0, |at| (at - now).max(0))
}

/// A time for a table cell as of `now` (unix seconds): `15:05 UTC`, with the day when it is
/// not today's. `up`: an estimate, shown to the minute rounded up (never earlier than
/// estimated).
pub(crate) fn clock(at: &str, now: i64, up: bool) -> Option<String> {
    let at = crate::time::parse_rfc3339(at)?;
    let at = if up {
        at + (60 - at.rem_euclid(60)) % 60
    } else {
        at
    };
    let at = chrono::DateTime::from_timestamp(at, 0)?;
    let today =
        chrono::DateTime::from_timestamp(now, 0).is_some_and(|n| n.date_naive() == at.date_naive());
    Some(
        at.format(if today {
            "%H:%M UTC"
        } else {
            "%Y-%m-%d %H:%M UTC"
        })
        .to_string(),
    )
}

/// The node's estimate for a table cell, as of `now` (unix seconds): `fresh ~15:05 UTC`
/// (`expected_fresh_at`), `overdue 16h` (ticks missed it: how long it has been out of date),
/// `rechecked 15:05 UTC` (`recheck_at` only). `None` for a fresh node and one with no time to
/// show. "Fresh" is said only of an estimate, "rechecked" only of a tick's next look.
pub fn line(node: &NodeStatus, now: i64) -> Option<String> {
    if node.health.is_fresh() {
        return None;
    }
    if let Some(at) = node
        .expected_fresh_at
        .as_deref()
        .and_then(|at| clock(at, now, true))
    {
        return Some(format!("fresh ~{at}"));
    }
    if node
        .refresh_path
        .as_ref()
        .is_some_and(|p| p.missed_ticks > 0)
        && let Some(since) = node
            .stale_since
            .as_deref()
            .and_then(crate::time::parse_rfc3339)
    {
        return Some(format!("overdue {}", crate::report::span(now - since)));
    }
    node.recheck_at
        .as_deref()
        .and_then(|at| clock(at, now, false))
        .map(|at| format!("rechecked {at}"))
}

/// A run of the run queue, as [`queue_starts`] reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueRun {
    pub run_id: String,
    /// When it started executing, unix seconds; `None` while it waits.
    pub started_at: Option<i64>,
    /// How long such a run typically takes, seconds.
    pub typical_seconds: f64,
}

/// When each waiting run of the run queue is expected to start, unix seconds, and when a run
/// that joined now would. `runs`: the running runs, then the waiting ones in the order they
/// start. Each of `max_runs` slots is free when its run ends (its start plus its typical
/// time, or `now` when that has passed); a waiting run takes the slot that frees first. The
/// extra slot a run someone asked for may take past scheduled work is not modelled.
pub fn queue_starts(runs: &[QueueRun], max_runs: usize, now: i64) -> (HashMap<String, i64>, i64) {
    let mut slots: Vec<i64> = runs
        .iter()
        .filter_map(|run| {
            run.started_at
                .map(|at| (at + run.typical_seconds.ceil() as i64).max(now))
        })
        .collect();
    while slots.len() < max_runs.max(1) {
        slots.push(now);
    }
    let mut starts = HashMap::new();
    for run in runs.iter().filter(|run| run.started_at.is_none()) {
        let (slot, free) = slots
            .iter()
            .copied()
            .enumerate()
            .min_by_key(|(_, free)| *free)
            .expect("at least one slot");
        starts.insert(run.run_id.clone(), free);
        slots[slot] = free + run.typical_seconds.ceil() as i64;
    }
    let clear = slots.into_iter().min().unwrap_or(now);
    (starts, clear)
}

/// How long a run that `schedule`'s tick started typically takes, seconds: the median of its
/// newest ticks' successful runs ([`crate::schedule_standing::Ticks::run_seconds`]). For a
/// run no schedule started, and a schedule with none recorded: the median of the schedules
/// that have one; 0 when none has.
pub fn typical_run_seconds(
    ticks: &HashMap<String, crate::schedule_standing::Ticks>,
    schedule: Option<&str>,
) -> f64 {
    use crate::schedule_standing::median;
    let of = |ticks: &crate::schedule_standing::Ticks| {
        (!ticks.run_seconds.is_empty()).then(|| median(&ticks.run_seconds))
    };
    schedule
        .and_then(|id| ticks.get(id))
        .and_then(of)
        .unwrap_or_else(|| {
            let known: Vec<f64> = ticks.values().filter_map(of).collect();
            median(&known)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::Dag;
    use crate::health::{Checks, classify};
    use crate::refresh_path::{PathEvidence, PathSchedule, PathSchedules, Paths};
    use crate::schedule::{Zone, jobs_from_dag};
    use crate::status::ScheduleStatus;

    /// 2026-11-17T14:57:00Z.
    const NOW: i64 = 1_794_927_420;
    /// The next slot of `*/5`.
    const TICK: i64 = NOW + 180;

    const PIPELINE: &str = r#"
from barca import asset, sensor, Always, Manual, Schedule

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox(): ...

@asset(inputs={"inbox": inbox}, freshness=Always)
def orders(inbox): ...

@asset(inputs={"orders": orders}, freshness=Always)
def clean(orders): ...

@asset(inputs={"orders": orders}, freshness=Always)
def side(orders): ...

@asset(inputs={"clean": clean, "side": side}, freshness=Always)
def report(clean, side): ...

@asset(inputs={"orders": orders}, freshness=Manual)
def reviewed(orders): ...

@asset(inputs={"reviewed": reviewed}, freshness=Always)
def summary(reviewed): ...

@asset(freshness=Always)
def loose(): ...
"#;

    fn dag() -> Dag {
        Dag::build(&crate::parse::extract_nodes(PIPELINE, "p.py").unwrap()).expect("valid dag")
    }

    fn id(name: &str) -> String {
        format!("p.py:{name}")
    }

    /// Every schedule firing as of `NOW`, its typical queue wait `wait` seconds.
    fn firing(dag: &Dag, wait: f64) -> PathSchedules {
        let now = chrono::DateTime::from_timestamp(NOW, 0).unwrap();
        jobs_from_dag(dag)
            .iter()
            .map(|job| {
                let ticks = crate::schedule_standing::Ticks {
                    queue_waits: vec![wait],
                    ..Default::default()
                };
                let standing = crate::schedule_standing::standing(
                    &job.cron,
                    &Zone::Utc,
                    now,
                    false,
                    Some(&ticks),
                );
                let mut block = ScheduleStatus::of(job, standing.clone(), (0, 5), None);
                block.live = true;
                (
                    job.id.clone(),
                    PathSchedule::of(job, &Zone::Utc, &standing, &block),
                )
            })
            .collect()
    }

    /// The rows of the graph in dependency order, each in `state` (by name; `cached`
    /// otherwise), a sensor `always_runs`.
    fn rows(dag: &Dag, states: &[(&str, &str)]) -> Vec<NodeStatus> {
        dag.topo_order()
            .into_iter()
            .map(|node_id| {
                let name = crate::targets::short_name(node_id);
                let node = dag.get_node(node_id).unwrap();
                let sensor = node.kind() == crate::NodeKind::Sensor;
                let state = states
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map_or(if sensor { "always_runs" } else { "cached" }, |(_, s)| s);
                let mut inputs: Vec<&str> = dag.upstream(node_id);
                inputs.sort_unstable();
                serde_json::from_value(serde_json::json!({
                    "id": node_id, "name": name,
                    "kind": if sensor { "sensor" } else { "asset" },
                    "inputs": inputs, "partitioned": false,
                    "cache": {"state": state, "reason": "", "detail": ""},
                    "last_materialization": if sensor {
                        serde_json::json!({"status": "success", "created_at": "2026-11-17 14:55:00"})
                    } else {
                        serde_json::Value::Null
                    },
                    "shape": null, "env": [],
                }))
                .unwrap()
            })
            .collect()
    }

    struct World {
        dag: Dag,
        paths: Paths,
        schedules: PathSchedules,
        ages: HashMap<String, crate::db::NodeAge>,
        checks: Checks,
        durations: HashMap<String, f64>,
        runs: Vec<PlannedRun>,
        queue_clear_at: Option<i64>,
        queue_open: bool,
        workers: usize,
        source_times: HashMap<String, i64>,
    }

    impl World {
        fn new() -> Self {
            let dag = dag();
            Self {
                paths: Paths::of(&dag),
                schedules: firing(&dag, 0.0),
                dag,
                ages: HashMap::new(),
                checks: Checks::default(),
                // Every step takes 10 seconds, the sensor's poll 2.
                durations: ["orders", "clean", "side", "report", "reviewed", "summary"]
                    .iter()
                    .map(|name| (id(name), 10.0))
                    .chain([(id("inbox"), 2.0)])
                    .collect(),
                runs: Vec::new(),
                queue_clear_at: None,
                queue_open: true,
                workers: 4,
                source_times: HashMap::new(),
            }
        }

        fn classified(&self, states: &[(&str, &str)]) -> Vec<NodeStatus> {
            let mut nodes = rows(&self.dag, states);
            self.classify(&mut nodes);
            nodes
        }

        fn classify(&self, nodes: &mut [NodeStatus]) {
            let none = HashSet::new();
            classify(
                nodes,
                Evidence {
                    ages: &self.ages,
                    pinned: &none,
                    late: &none,
                    checks: &self.checks,
                    path: PathEvidence {
                        paths: &self.paths,
                        schedules: &self.schedules,
                        paused: &none,
                        pinned: &none,
                    },
                    timing: Timing {
                        durations: &self.durations,
                        runs: &self.runs,
                        queue_clear_at: self.queue_clear_at,
                        queue_open: self.queue_open,
                        workers: self.workers,
                        max_runs: None,
                        source_times: &self.source_times,
                    },
                    now: NOW,
                },
            );
        }
    }

    fn of<'a>(nodes: &'a [NodeStatus], name: &str) -> &'a NodeStatus {
        nodes.iter().find(|n| n.name == name).expect("a row")
    }

    /// `expected_fresh_at` of `name`, seconds after the tick.
    fn fresh_after_tick(nodes: &[NodeStatus], name: &str) -> Option<i64> {
        of(nodes, name)
            .expected_fresh_at
            .as_deref()
            .and_then(crate::time::parse_rfc3339)
            .map(|at| at - TICK)
    }

    const ALL_STALE: [(&str, &str); 4] = [
        ("orders", "stale"),
        ("clean", "stale"),
        ("side", "stale"),
        ("report", "stale"),
    ];

    #[test]
    fn a_node_is_expected_fresh_after_the_next_tick_and_the_steps_before_it() {
        let world = World::new();
        let nodes = world.classified(&ALL_STALE);
        // The tick polls the sensor (2s), then the chain: 10s a step, the two branches at once.
        assert_eq!(fresh_after_tick(&nodes, "orders"), Some(15));
        assert_eq!(fresh_after_tick(&nodes, "clean"), Some(25));
        assert_eq!(fresh_after_tick(&nodes, "side"), Some(25));
        assert_eq!(fresh_after_tick(&nodes, "report"), Some(35));
        // Every node with a path is rechecked at its next tick, fresh or not.
        let tick = crate::time::rfc3339_secs(TICK);
        assert_eq!(of(&nodes, "report").recheck_at, tick);
        assert_eq!(of(&nodes, "inbox").recheck_at, tick);
        // A fresh node has no estimate, and one no tick reaches has neither time.
        assert_eq!(of(&nodes, "inbox").expected_fresh_at, None);
        let loose = world.classified(&[("loose", "stale")]);
        assert_eq!(of(&loose, "loose").expected_fresh_at, None);
        assert_eq!(of(&loose, "loose").recheck_at, None);
    }

    #[test]
    fn branches_run_side_by_side_only_as_far_as_the_workers_reach() {
        let mut world = World::new();
        world.workers = 1;
        let nodes = world.classified(&ALL_STALE);
        // One worker: the sensor, then one step after another.
        assert_eq!(fresh_after_tick(&nodes, "orders"), Some(15));
        let mut branches = [
            fresh_after_tick(&nodes, "clean"),
            fresh_after_tick(&nodes, "side"),
        ];
        branches.sort();
        assert_eq!(branches, [Some(25), Some(35)]);
        assert_eq!(fresh_after_tick(&nodes, "report"), Some(45));
    }

    #[test]
    fn the_start_is_the_tick_plus_the_typical_queue_wait_or_when_the_queue_clears() {
        let mut world = World::new();
        world.schedules = firing(&world.dag, 60.0);
        let nodes = world.classified(&[("orders", "stale")]);
        assert_eq!(fresh_after_tick(&nodes, "orders"), Some(75));
        // The runs ahead take until five minutes after the tick.
        world.queue_clear_at = Some(TICK + 300);
        let nodes = world.classified(&[("orders", "stale")]);
        assert_eq!(fresh_after_tick(&nodes, "orders"), Some(315));
        // A queue that starts nothing gives no start to estimate from.
        world.queue_open = false;
        let nodes = world.classified(&[("orders", "stale")]);
        assert_eq!(of(&nodes, "orders").expected_fresh_at, None);
        assert!(of(&nodes, "orders").recheck_at.is_some());
    }

    #[test]
    fn a_node_without_samples_takes_the_projects_median_and_a_key_counts_each_time() {
        let mut world = World::new();
        world.durations.remove(&id("orders"));
        let mut nodes = rows(&world.dag, &[("orders", "partial")]);
        let orders = nodes.iter_mut().find(|n| n.name == "orders").unwrap();
        orders.partitioned = true;
        orders.partitions = Some(
            serde_json::from_value(
                serde_json::json!({"total": 5, "cached": 2, "missing": 3, "missing_keys": []}),
            )
            .unwrap(),
        );
        world.classify(&mut nodes);
        // Three keys to compute at the project's median of 10s, after the 2s poll.
        assert_eq!(fresh_after_tick(&nodes, "orders"), Some(35));
    }

    #[test]
    fn only_a_recheck_where_a_tick_cannot_promise_fresh() {
        let mut world = World::new();
        // Undecided, and what is below an undecided node.
        let nodes = world.classified(&[("orders", "unknown"), ("clean", "stale")]);
        for name in ["orders", "clean"] {
            assert_eq!(of(&nodes, name).expected_fresh_at, None, "{name}");
            assert!(of(&nodes, name).recheck_at.is_some(), "{name}");
        }
        // Undecided has no `stale_since` either.
        assert_eq!(of(&nodes, "orders").stale_since, None);
        // Below a `Manual` node that is out of date: the tick reads it as it is.
        let nodes = world.classified(&[("reviewed", "stale"), ("summary", "stale")]);
        assert_eq!(
            of(&nodes, "reviewed").recheck_at,
            None,
            "no tick reaches it"
        );
        assert_eq!(of(&nodes, "summary").expected_fresh_at, None);
        assert!(of(&nodes, "summary").recheck_at.is_some());
        // A failing node, and what it blocks: the cause is the failure.
        let mut nodes = rows(&world.dag, &[("orders", "stale"), ("clean", "stale")]);
        nodes
            .iter_mut()
            .find(|n| n.name == "orders")
            .unwrap()
            .last_materialization = Some(
            serde_json::from_value(
                serde_json::json!({"status": "failed", "created_at": "2026-11-17 14:00:00"}),
            )
            .unwrap(),
        );
        world.classify(&mut nodes);
        for name in ["orders", "clean"] {
            assert_eq!(of(&nodes, name).expected_fresh_at, None, "{name}");
        }
        // A tick already missed it: its estimate failed.
        world.checks.misses = HashMap::from([(id("orders"), vec![(id("inbox"), 400.0)])]);
        let nodes = world.classified(&[("orders", "stale"), ("clean", "stale")]);
        assert_eq!(of(&nodes, "orders").expected_fresh_at, None);
        assert_eq!(
            fresh_after_tick(&nodes, "clean"),
            Some(25),
            "the miss is the node's own"
        );
    }

    #[test]
    fn a_run_in_flight_is_the_start_before_the_next_tick() {
        let mut world = World::new();
        // The tick's run is executing; its last step ended 5 seconds ago.
        world.runs = vec![PlannedRun {
            run_id: "r1".into(),
            schedule: Some(id("inbox")),
            nodes: None,
            running: true,
            start: Some(NOW - 5),
        }];
        let nodes = world.classified(&[("clean", "stale"), ("report", "stale")]);
        let after = |name: &str| {
            of(&nodes, name)
                .expected_fresh_at
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .map(|at| at - NOW)
        };
        // The poll (2s) and the step (10s) from the anchor, rounded up to 5 seconds.
        assert_eq!(after("clean"), Some(10));
        assert_eq!(after("report"), Some(20));
        // A run someone asked for waits in the queue: from its estimated start.
        world.runs = vec![PlannedRun {
            run_id: "r2".into(),
            schedule: None,
            nodes: Some([id("orders"), id("clean")].into()),
            running: false,
            start: Some(NOW + 60),
        }];
        let nodes = world.classified(&[("clean", "stale"), ("report", "stale")]);
        let after = |name: &str| {
            of(&nodes, name)
                .expected_fresh_at
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .map(|at| at - NOW)
        };
        assert_eq!(after("clean"), Some(70));
        // `report` is not in that run: the next tick computes it.
        assert_eq!(fresh_after_tick(&nodes, "report"), Some(25));
        // An estimate that has passed is never in the past.
        world.runs = vec![PlannedRun {
            run_id: "r3".into(),
            schedule: Some(id("inbox")),
            nodes: None,
            running: true,
            start: Some(NOW - 600),
        }];
        let nodes = world.classified(&[("clean", "stale")]);
        assert_eq!(
            of(&nodes, "clean").expected_fresh_at,
            crate::time::rfc3339_secs(NOW)
        );
    }

    #[test]
    fn stale_since_is_the_earliest_record_of_what_made_it_stale() {
        let mut world = World::new();
        let at = |time: &str| crate::db::NodeAge {
            last_success_at: Some(time.to_string()),
            ..Default::default()
        };
        // The sensor observed a new value at 14:55; `orders` last succeeded at 14:00.
        world.ages = HashMap::from([
            (id("inbox"), at("2026-11-17 14:55:00")),
            (id("orders"), at("2026-11-17 14:00:00")),
            (id("clean"), at("2026-11-17 14:00:10")),
        ]);
        let mut nodes = rows(&world.dag, &[("orders", "stale"), ("clean", "stale")]);
        let causes = |nodes: &mut [NodeStatus], name: &str, causes: &[&str]| {
            nodes
                .iter_mut()
                .find(|n| n.name == name)
                .unwrap()
                .cache
                .change_causes = Some(causes.iter().map(|c| c.to_string()).collect());
        };
        causes(&mut nodes, "orders", &["inputs_changed"]);
        causes(&mut nodes, "clean", &["upstream_stale"]);
        world.classify(&mut nodes);
        let since = Some("2026-11-17T14:55:00Z".to_string());
        assert_eq!(of(&nodes, "orders").stale_since, since);
        // What is below has been out of date as long.
        assert_eq!(of(&nodes, "clean").stale_since, since);
        assert_eq!(behind_seconds(of(&nodes, "clean"), NOW), 120);
        assert_eq!(ahead_seconds(of(&nodes, "clean"), NOW), 205);
        // A code change: the source file's modification time, when after its newest result.
        let mut nodes = rows(&world.dag, &[("orders", "stale")]);
        causes(&mut nodes, "orders", &["code_changed"]);
        world.classify(&mut nodes);
        assert_eq!(of(&nodes, "orders").stale_since, None, "no record");
        world.source_times = HashMap::from([("p.py".to_string(), NOW - 30)]);
        world.classify(&mut nodes);
        assert_eq!(
            of(&nodes, "orders").stale_since,
            crate::time::rfc3339_secs(NOW - 30)
        );
        // A file last modified before the node's newest result does not date the change.
        world.source_times = HashMap::from([("p.py".to_string(), 1_000)]);
        world.classify(&mut nodes);
        assert_eq!(of(&nodes, "orders").stale_since, None);
        // An environment change has no record at all.
        causes(&mut nodes, "orders", &["env_changed"]);
        world.source_times = HashMap::from([("p.py".to_string(), NOW - 30)]);
        world.classify(&mut nodes);
        assert_eq!(of(&nodes, "orders").stale_since, None);
        // Never run: since the file was last modified.
        let nodes = world.classified(&[("loose", "never_run")]);
        assert_eq!(
            of(&nodes, "loose").stale_since,
            crate::time::rfc3339_secs(NOW - 30)
        );
        // A failure: its first failed attempt.
        world.ages.insert(
            id("orders"),
            crate::db::NodeAge {
                failing_since: Some("2026-11-17 13:00:00".to_string()),
                ..Default::default()
            },
        );
        let mut nodes = rows(&world.dag, &[("orders", "stale"), ("clean", "stale")]);
        nodes
            .iter_mut()
            .find(|n| n.name == "orders")
            .unwrap()
            .last_materialization = Some(
            serde_json::from_value(
                serde_json::json!({"status": "failed", "created_at": "2026-11-17 14:30:00"}),
            )
            .unwrap(),
        );
        world.classify(&mut nodes);
        let since = Some("2026-11-17T13:00:00Z".to_string());
        assert_eq!(of(&nodes, "orders").stale_since, since);
        assert_eq!(of(&nodes, "clean").stale_since, since, "what it blocks");
    }

    #[test]
    fn the_line_keeps_fresh_and_rechecked_apart() {
        let world = World::new();
        let mut nodes = world.classified(&[("orders", "stale"), ("clean", "unknown")]);
        assert_eq!(
            line(of(&nodes, "orders"), NOW).as_deref(),
            Some("fresh ~15:01 UTC")
        );
        assert_eq!(
            line(of(&nodes, "clean"), NOW).as_deref(),
            Some("rechecked 15:00 UTC")
        );
        assert_eq!(line(of(&nodes, "inbox"), NOW), None, "fresh");
        // Missed by a tick, out of date for 16 hours.
        let orders = nodes.iter_mut().find(|n| n.name == "orders").unwrap();
        orders.expected_fresh_at = None;
        orders.stale_since = crate::time::rfc3339_secs(NOW - 16 * 3600 - 5);
        orders.refresh_path.as_mut().unwrap().missed_ticks = 2;
        assert_eq!(
            line(of(&nodes, "orders"), NOW).as_deref(),
            Some("overdue 16h")
        );
    }

    #[test]
    fn a_waiting_run_starts_when_a_slot_frees() {
        let run = |id: &str, started_at: Option<i64>, typical: f64| QueueRun {
            run_id: id.into(),
            started_at,
            typical_seconds: typical,
        };
        // One slot: a run 20s into its typical 60s, two waiting behind it.
        let runs = [
            run("a", Some(NOW - 20), 60.0),
            run("b", None, 30.0),
            run("c", None, 30.0),
        ];
        let (starts, clear) = queue_starts(&runs, 1, NOW);
        assert_eq!(starts["b"], NOW + 40);
        assert_eq!(starts["c"], NOW + 70);
        assert_eq!(clear, NOW + 100);
        // Two slots: `b` starts at once, `c` when the first of them ends.
        let (starts, clear) = queue_starts(&runs, 2, NOW);
        assert_eq!(starts["b"], NOW);
        assert_eq!(starts["c"], NOW + 30);
        assert_eq!(clear, NOW + 40);
        // A run past its typical time frees its slot no earlier than now.
        let (starts, _) = queue_starts(
            &[run("a", Some(NOW - 600), 60.0), run("b", None, 1.0)],
            1,
            NOW,
        );
        assert_eq!(starts["b"], NOW);
        // A typical run time: the schedule's own, else the median of the others, else 0.
        let ticks = HashMap::from([(
            "s".to_string(),
            crate::schedule_standing::Ticks {
                run_seconds: vec![10.0, 30.0, 20.0],
                ..Default::default()
            },
        )]);
        assert_eq!(typical_run_seconds(&ticks, Some("s")), 20.0);
        assert_eq!(typical_run_seconds(&ticks, Some("other")), 20.0);
        assert_eq!(typical_run_seconds(&ticks, None), 20.0);
        assert_eq!(typical_run_seconds(&HashMap::new(), None), 0.0);
    }
}
