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

use crate::BarcaError;
use crate::cache_state::CacheState;
use crate::db::{self, NodeAge, TickTargetOutcome};
use crate::schedule::Zone;
use crate::status::{NodeStatus, StatusResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A node's health word. Declared most severe first: this order sorts and ranks everywhere
/// (`Ord`, [`Health::ALL`]).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Its own last attempt failed.
    Failing,
    /// Something upstream keeps it from being brought up to date.
    Blocked,
    /// Its last run succeeded but its recorded row counts look wrong.
    Suspect,
    /// Out of date against its inputs, partly computed, out of sync, or late.
    Behind,
    /// barca cannot decide yet. Also what a status row that was never classified reads as:
    /// the most conservative word.
    #[default]
    Undecided,
    /// Nothing recorded.
    NeverRun,
    /// Fresh, or a task or sensor whose last run succeeded.
    Ok,
}

impl Health {
    /// Every word, most severe first.
    pub const ALL: [Health; 7] = [
        Health::Failing,
        Health::Blocked,
        Health::Suspect,
        Health::Behind,
        Health::Undecided,
        Health::NeverRun,
        Health::Ok,
    ];

    /// The value as JSON and `barca.nodes` spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Failing => "failing",
            Health::Blocked => "blocked",
            Health::Suspect => "suspect",
            Health::Behind => "behind",
            Health::Undecided => "undecided",
            Health::NeverRun => "never_run",
            Health::Ok => "ok",
        }
    }

    /// The display word: what the CLI prints and the UI shows.
    pub fn word(self) -> &'static str {
        match self {
            Health::Suspect => "suspect data",
            Health::NeverRun => "never run",
            other => other.as_str(),
        }
    }

    /// Whether the node is fresh: `ok` or `suspect` (a statement about data, not freshness).
    pub fn is_fresh(self) -> bool {
        matches!(self, Health::Ok | Health::Suspect)
    }
}

/// Which row of the health table decided a node's [`Health`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    /// `failing`: its last materialization failed.
    LastAttemptFailed,
    /// `failing`: its last materialization did not fail, but some of its keys did.
    KeysFailed,
    /// `blocked`: a failing node upstream ([`blocked_by`]).
    UpstreamFailing,
    /// `blocked`: its cache state says so (keys waiting on failed keys of an input).
    CacheBlocked,
    /// `suspect`: a latest result wrote no rows.
    Empty,
    /// `suspect`: a latest result's row count moved sharply.
    RowsChanged,
    /// `behind`: older than an input, and nothing else says so.
    BehindInput,
    /// `behind`: its schedule is late ([`crate::schedule_standing::standing`]).
    ScheduleLate,
    /// `behind`: `cache.state` is `stale`.
    Stale,
    /// `behind`: `cache.state` is `partial`.
    Partial,
    /// `behind`: `cache.state` is `unsynced`.
    Unsynced,
    /// `ok`: `cache.state` is `unknown`, and the newest tick that checked the node found it
    /// up to date or rebuilt it, with nothing seen to have changed since ([`verdict_stands`]).
    CheckedUpToDate,
    /// `undecided`: `cache.state` is `unknown`. Also the reason of an unclassified row.
    #[default]
    Unknown,
    /// `never_run`: `cache.state` is `never_run`.
    NeverRun,
    /// `never_run`: a task, sensor or `always` asset with no recorded run.
    NoSuccessfulRun,
    /// `ok`: a task, sensor or `always` asset whose last run did not fail.
    LastRunSucceeded,
    /// An explicitly Manual task has no result due before its first requested run.
    OnDemand,
    /// `ok`: `cache.state` is `cached`.
    Cached,
}

impl HealthReason {
    /// The value as JSON and `barca.nodes` spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            HealthReason::LastAttemptFailed => "last_attempt_failed",
            HealthReason::KeysFailed => "keys_failed",
            HealthReason::UpstreamFailing => "upstream_failing",
            HealthReason::CacheBlocked => "cache_blocked",
            HealthReason::Empty => "empty",
            HealthReason::RowsChanged => "rows_changed",
            HealthReason::BehindInput => "behind_input",
            HealthReason::ScheduleLate => "schedule_late",
            HealthReason::Stale => "stale",
            HealthReason::Partial => "partial",
            HealthReason::Unsynced => "unsynced",
            HealthReason::CheckedUpToDate => "checked_up_to_date",
            HealthReason::Unknown => "unknown",
            HealthReason::NeverRun => "never_run",
            HealthReason::NoSuccessfulRun => "no_successful_run",
            HealthReason::LastRunSucceeded => "last_run_succeeded",
            HealthReason::OnDemand => "on_demand",
            HealthReason::Cached => "cached",
        }
    }
}

/// One thing a node's recorded results say is worth a look. A node has any number of them, in
/// this order: `empty`, `rows_changed`, `behind_input`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataSignal {
    /// Its latest successful result wrote no rows: for a partitioned node, `empty_keys` of the
    /// `keys` whose latest result recorded a row count (1 of 1 for a node without keys).
    Empty {
        empty_keys: u32,
        keys: u32,
        /// How many were empty as of before its newest run (each key's latest success before
        /// that run); `null` when no key had a counted result before it.
        previous_empty_keys: Option<u32>,
    },
    /// A latest successful result's row count differs from the one before it (same node, same
    /// key) by more than half of it, or rows came after an empty result. `rows` and
    /// `previous_rows` are of `key`, the key whose count moved the most relative to before
    /// (`null` for a node without keys); `changed_keys` counts every key that moved.
    RowsChanged {
        changed_keys: u32,
        key: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        rows: i64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        previous_rows: i64,
    },
    /// Its latest successful result is older than the newest successful result of `input`, an
    /// asset it reads (typically a `collect()` fan-in after its input's keys ran again), and its
    /// cache state is `unknown`, so nothing else says it is behind. The input is the one with
    /// the newest result; times are UTC `YYYY-MM-DD HH:MM:SS`, as `last_success_at`.
    BehindInput {
        input: String,
        input_success_at: String,
        last_success_at: String,
    },
}

impl DataSignal {
    /// The signal's `kind`, as JSON spells it: what `barca.nodes.signals` lists.
    pub fn kind(&self) -> &'static str {
        match self {
            DataSignal::Empty { .. } => "empty",
            DataSignal::RowsChanged { .. } => "rows_changed",
            DataSignal::BehindInput { .. } => "behind_input",
        }
    }

    /// Whether it says the data looks wrong (`empty`, `rows_changed`), as opposed to old.
    pub fn is_suspect(&self) -> bool {
        !matches!(self, DataSignal::BehindInput { .. })
    }
}

/// Why a node is not fresh, in one word (`specs/behavior/health.md`, "Cause"): every node
/// that is not fresh has exactly one, a fresh node none. Declared in the order the display
/// lists them: what alarms first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// It failed, a failing node upstream holds it, or its schedule is held after
    /// `max_failures` failed ticks.
    Failing,
    /// Ticks reached it and did not leave it fresh.
    Overdue,
    /// Nothing scheduled will refresh it.
    NoRefreshPath,
    /// What would refresh it waits for a run slot.
    Queued,
    /// The read path cannot decide.
    Undecided,
    /// A run is computing it, or the next tick of its refresh path will.
    CatchingUp,
}

impl Cause {
    /// Every cause, in display order: what alarms first, progress last.
    pub const ALL: [Cause; 6] = [
        Cause::Failing,
        Cause::Overdue,
        Cause::NoRefreshPath,
        Cause::Queued,
        Cause::Undecided,
        Cause::CatchingUp,
    ];

    /// The value as JSON and `barca.nodes` spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Failing => "failing",
            Cause::Overdue => "overdue",
            Cause::NoRefreshPath => "no_refresh_path",
            Cause::Queued => "queued",
            Cause::Undecided => "undecided",
            Cause::CatchingUp => "catching_up",
        }
    }

    /// How far one node with this cause is from fresh, against 1 for a fresh node
    /// (`specs/behavior/health.md`, "Distance from fresh"): 6 where someone has to act
    /// (`failing`, `overdue`, `no_refresh_path`), 2 where the node will be fresh without
    /// anyone acting or barca cannot say (`queued`, `catching_up`, `undecided`).
    pub fn weight(self) -> usize {
        match self {
            Cause::Failing | Cause::Overdue | Cause::NoRefreshPath => 6,
            Cause::Queued | Cause::Undecided | Cause::CatchingUp => 2,
        }
    }

    /// The display word: what the CLI prints and the UI shows.
    pub fn word(self) -> &'static str {
        match self {
            Cause::NoRefreshPath => "no refresh path",
            Cause::CatchingUp => "catching up",
            other => other.as_str(),
        }
    }
}

/// Why a node that is not fresh has no `expected_fresh_at` (`specs/behavior/health.md`,
/// "Times"): the one reason a surface gives instead of a blank. [`no_estimate`] names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum NoEstimate {
    /// It failed, or a failing node upstream holds it: nothing clears it until that is fixed.
    Failing,
    /// Its schedule, or the node itself, is paused.
    Paused,
    /// No scheduler is seen firing its schedule.
    SchedulerOff,
    /// Nothing scheduled refreshes it.
    NoPath,
    /// barca cannot decide it until a run reports.
    Undecided,
    /// Ticks reached it and did not leave it fresh: its estimate already failed.
    Missed,
    /// The run queue is paused or draining: a tick's run would start nothing.
    QueueStopped,
    /// The answering process runs no scheduler, so it has no next tick to estimate from.
    NoSchedulerHere,
    /// A tick will look at it, and may find it up to date or leave it: a result above it
    /// may come out unchanged, or a node above it is read as it is.
    NotPromised,
}

impl NoEstimate {
    /// The value as JSON and `barca.nodes` spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            NoEstimate::Failing => "failing",
            NoEstimate::Paused => "paused",
            NoEstimate::SchedulerOff => "scheduler_off",
            NoEstimate::NoPath => "no_path",
            NoEstimate::Undecided => "undecided",
            NoEstimate::Missed => "missed",
            NoEstimate::QueueStopped => "queue_stopped",
            NoEstimate::NoSchedulerHere => "no_scheduler_here",
            NoEstimate::NotPromised => "not_promised",
        }
    }

    /// The reason in words, after "no estimate: ".
    pub fn word(self) -> &'static str {
        match self {
            NoEstimate::Failing => "until the failure is fixed",
            NoEstimate::Paused => "paused",
            NoEstimate::SchedulerOff => "no scheduler is firing its schedule",
            NoEstimate::NoPath => "no refresh path",
            NoEstimate::Undecided => "undecided until a run reports",
            NoEstimate::Missed => "ticks missed it",
            NoEstimate::QueueStopped => "the run queue is not starting runs",
            NoEstimate::NoSchedulerHere => "no scheduler runs here",
            NoEstimate::NotPromised => "a result above it may come out unchanged",
        }
    }
}

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

/// What kind of upstream fact a [`Root`] is (`specs/behavior/health.md`, "Root").
/// [`crate::causes::attribute`] names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// A sensor whose observation changed: `node` is the sensor.
    SensorValue,
    /// A node whose code changed (the function, a decorator, a helper).
    Code,
    /// A node whose declared environment variable changed.
    Env,
    /// A result that what reads it has not caught up with: `node` is the input that changed.
    Input,
    /// The topmost never-run node of a chain.
    NeverRun,
    /// The topmost not-fresh node no tick reaches: where a schedule would have to be added
    /// (or, for a `Manual` or pinned node, where someone has to run it).
    NoSchedule,
    /// A paused schedule (`node` is the scheduled node) or a paused node (`barca pause`).
    SchedulePaused,
    /// A schedule no running scheduler fires: `node` is the scheduled node.
    SchedulerOff,
    /// A failing node: the node itself, or the nearest failing node upstream ([`blocked_by`]);
    /// or a schedule held after `max_failures` failed ticks (`node` is the scheduled node).
    Failure,
    /// A schedule whose ticks wait for a run slot: `node` is the scheduled node.
    Queue,
    /// The topmost node the read path cannot decide, with nothing above it that explains it.
    Undecided,
}

impl RootKind {
    /// The value as JSON spells it, and the prefix of a root's id.
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::SensorValue => "sensor_value",
            RootKind::Code => "code",
            RootKind::Env => "env",
            RootKind::Input => "input",
            RootKind::NeverRun => "never_run",
            RootKind::NoSchedule => "no_schedule",
            RootKind::SchedulePaused => "schedule_paused",
            RootKind::SchedulerOff => "scheduler_off",
            RootKind::Failure => "failure",
            RootKind::Queue => "queue",
            RootKind::Undecided => "undecided",
        }
    }

    /// The display word: what the CLI prints and the UI shows before the root's node.
    pub fn word(self) -> &'static str {
        match self {
            RootKind::SensorValue => "sensor value",
            RootKind::Code => "code change",
            RootKind::Env => "environment change",
            RootKind::Input => "input change",
            RootKind::NeverRun => "never run",
            RootKind::NoSchedule => "no schedule",
            RootKind::SchedulePaused => "paused",
            RootKind::SchedulerOff => "scheduler off",
            RootKind::Failure => "failure",
            RootKind::Queue => "run queue",
            RootKind::Undecided => "undecided",
        }
    }
}

/// The upstream fact that explains why a node is not fresh: `<kind>:<node id>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Root {
    /// `<kind>:<node id>`, e.g. `failure:pipeline.py:orders`.
    pub id: String,
    pub kind: RootKind,
    /// The node the fact is about: the failing node, the sensor whose observation changed,
    /// the node whose code changed, the scheduled node of a paused schedule.
    pub node: String,
}

impl Root {
    /// The root of `kind` at `node`.
    pub fn of(kind: RootKind, node: &str) -> Self {
        Root {
            id: format!("{}:{node}", kind.as_str()),
            kind,
            node: node.to_string(),
        }
    }

    /// The failure root of `node`: what a failing node and every node it blocks carry.
    pub fn failure(node: &str) -> Self {
        Self::of(RootKind::Failure, node)
    }
}

/// How much of a scope's data was measured: `7 of 556 measured`. An unmeasured asset raised no
/// signal because nothing was counted, so it is "not measured", never implied ok.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DataCoverage {
    /// Assets in scope (tasks and sensors are nodes, not assets).
    pub assets: usize,
    /// Of them, those whose latest successful result has a recorded row count.
    pub measured: usize,
    /// Of the measured ones, those with an `empty` or `rows_changed` signal.
    pub suspect: usize,
}

impl DataCoverage {
    /// The coverage of `nodes`, which [`classify`] has filled.
    pub fn of(nodes: &[NodeStatus]) -> Self {
        let mut out = DataCoverage::default();
        for node in nodes.iter().filter(|n| n.kind == "asset") {
            out.assets += 1;
            if node.measured == Some(true) {
                out.measured += 1;
            }
            if node.signals.iter().any(DataSignal::is_suspect) {
                out.suspect += 1;
            }
        }
        out
    }

    /// `7 of 556 assets measured`, then `, 2 suspect` when some are.
    pub fn line(&self) -> String {
        let suspect = match self.suspect {
            0 => String::new(),
            n => format!(", {n} suspect"),
        };
        format!(
            "{} of {} {} measured{suspect}",
            self.measured,
            self.assets,
            if self.assets == 1 { "asset" } else { "assets" }
        )
    }
}

/// `last_check`: what the newest finished tick that had the node as a target did with it
/// (the tick record, `specs/behavior/health.md`). A record, whatever its age: whether it still
/// decides the node's health is [`verdict_stands`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LastCheck {
    /// When that tick's run ended (RFC 3339, UTC).
    pub at: String,
    /// What the run did with the node: `rebuilt` (its step executed: for a sensor, it was
    /// polled, whatever it returned; for a task, it fired), `up_to_date` (a cache hit),
    /// `failed`, or `not_reached` (the run ended before it decided the node).
    #[cfg_attr(
        feature = "ts",
        ts(type = "\"rebuilt\" | \"up_to_date\" | \"failed\" | \"not_reached\"")
    )]
    pub outcome: TickTargetOutcome,
    /// The scheduled node whose tick it was.
    pub schedule: String,
}

impl LastCheck {
    /// The outcome in words, after "checked 3m ago, ": what the CLI prints and the UI shows.
    pub fn outcome_word(outcome: TickTargetOutcome) -> &'static str {
        match outcome {
            TickTargetOutcome::Rebuilt => "rebuilt",
            TickTargetOutcome::UpToDate => "up to date",
            TickTargetOutcome::Failed => "failed",
            TickTargetOutcome::NotReached => "not reached",
        }
    }

    /// `checked 3m ago, up to date`, as of `now` (unix seconds).
    pub fn line(&self, now: i64) -> String {
        let ago = chrono::DateTime::parse_from_rfc3339(&self.at)
            .map(|at| crate::report::ago(now - at.timestamp()))
            .unwrap_or_else(|_| self.at.clone());
        format!("checked {ago}, {}", Self::outcome_word(self.outcome))
    }
}

/// A node's [`LastCheck`] with what [`verdict_stands`] reads of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub last: LastCheck,
    /// When the tick's run ended, unix seconds.
    pub finished_at: f64,
    /// Until when its verdict can decide the node's health (unix seconds): one interval of
    /// its schedule after its run ended, plus the time that tick took from its slot to its
    /// end (when the next tick's verdict is due), plus the grace a late schedule gets
    /// (`crate::schedule_standing`). `None` when its schedule is no longer a scheduled node of the
    /// graph: nothing will check the node again.
    pub trusted_until: Option<f64>,
}

/// What the tick record says about the nodes of a graph ([`checks`]): the same for every
/// request until the metadata database or the graph changes.
#[derive(Clone, Debug, Default)]
pub struct Checks {
    /// Each node's newest check.
    pub by_node: HashMap<String, Check>,
    /// Assets whose definition (code, counted decorator parts, helpers) is not the one their
    /// newest successful result recorded, or cannot be compared with it: edited since.
    pub redefined: HashSet<String>,
    /// The runs of fired ticks still in flight: what they record is not a change since a
    /// check, their own verdict is on its way.
    pub in_flight: HashSet<String>,
    /// Each node's finished ticks that did not leave it fresh, back to the last one that did
    /// ([`db::LatestTicks::misses`]): what `refresh_path.missed_ticks` counts.
    pub misses: HashMap<String, Vec<(String, f64)>>,
}

/// What classification reads beyond the status rows themselves.
#[derive(Clone, Copy)]
pub struct Evidence<'a> {
    /// Every node's age ([`db::node_ages`]): row counts and when each result was written.
    pub ages: &'a HashMap<String, NodeAge>,
    /// Pinned assets (`barca pin`): never blocked, and a failure above one does not pass it.
    pub pinned: &'a HashSet<String>,
    /// Scheduled nodes whose schedule is late ([`crate::schedule_standing::late_schedules`]).
    pub late: &'a HashSet<String>,
    /// What the newest ticks found ([`checks`]).
    pub checks: &'a Checks,
    /// What a refresh path is read from ([`crate::refresh_path`]): the graph's reach, each
    /// schedule now, the paused nodes and the pinned assets.
    pub path: crate::refresh_path::PathEvidence<'a>,
    /// What the times are estimated from ([`crate::estimates`]): typical durations, the runs
    /// in flight, the run queue, the worker pool, when each source file changed.
    pub timing: crate::estimates::Timing<'a>,
    /// The instant of classification, unix seconds: a check's verdict expires.
    pub now: i64,
}

impl Evidence<'static> {
    /// No record, no schedule, no run: what a result classified from its rows alone reads.
    pub fn none(now: i64) -> Self {
        static AGES: std::sync::LazyLock<HashMap<String, NodeAge>> =
            std::sync::LazyLock::new(HashMap::new);
        static NODES: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        static CHECKS: std::sync::LazyLock<Checks> = std::sync::LazyLock::new(Checks::default);
        Self {
            ages: &AGES,
            pinned: &NODES,
            late: &NODES,
            checks: &CHECKS,
            path: crate::refresh_path::PathEvidence::none(),
            timing: crate::estimates::Timing::none(),
            now,
        }
    }
}

/// `last_attempt_failed`: its last materialization failed, or (a partitioned node's last
/// materialization is one key) some of its keys did.
pub fn last_attempt_failed(node: &NodeStatus) -> bool {
    last_materialization_failed(node) || keys_failed(node)
}

fn last_materialization_failed(node: &NodeStatus) -> bool {
    node.last_materialization
        .as_ref()
        .is_some_and(|m| m.status == "failed")
}

fn keys_failed(node: &NodeStatus) -> bool {
    node.partitions.as_ref().is_some_and(|p| p.failed > 0)
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

/// When `created_at` (UTC `YYYY-MM-DD HH:MM:SS`, as materializations record it) was, unix
/// seconds. A time that cannot be read is the end of time: newer than any check.
pub(crate) fn recorded_at(created_at: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .map_or(i64::MAX, |at| at.and_utc().timestamp())
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
            let (health, reason) = decide(
                node,
                by.is_some(),
                &signals,
                evidence.late.contains(&node.id),
                checked,
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
    }
}

/// [`classify`] every row of `result`, count its data coverage and sum its rollup's root
/// entry as of `evidence.now`, over every node in scope: call it before a filter cuts
/// `nodes`. `running`: the nodes a run is working on.
pub fn attach(result: &mut StatusResult, evidence: Evidence<'_>, running: &HashSet<String>) {
    classify(&mut result.nodes, evidence);
    result.data_coverage = DataCoverage::of(&result.nodes);
    result.as_of = crate::time::rfc3339_secs(evidence.now);
    let roots = crate::rollup::roots(&result.nodes, evidence, false);
    result.rollup = Some(crate::rollup::total(
        &result.nodes,
        running,
        &crate::rollup::root_actions(&roots),
    ));
}

/// [`attach`] with the evidence the metadata DB of `cfg` holds (none when it does not exist
/// yet): what `barca status` and `barca sql` read on this machine. Cron is evaluated in
/// `zone`; a paused schedule is not late.
pub async fn attach_from_db(
    result: &mut StatusResult,
    dag: &crate::dag::Dag,
    cfg: &crate::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), BarcaError> {
    from_db(result, dag, cfg, zone, now, false)
        .await
        .map(|_| ())
}

/// [`attach_from_db`], then the rollup of the rows (`barca status --rollup`): no run is
/// tracked here, so `running` is 0 everywhere.
pub async fn rollup_from_db(
    result: &mut StatusResult,
    dag: &crate::dag::Dag,
    cfg: &crate::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::rollup::Rollup, BarcaError> {
    Ok(from_db(result, dag, cfg, zone, now, true)
        .await?
        .expect("the rollup was asked for"))
}

async fn from_db(
    result: &mut StatusResult,
    dag: &crate::dag::Dag,
    cfg: &crate::config::ResolvedConfig,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
    rollup: bool,
) -> Result<Option<crate::rollup::Rollup>, BarcaError> {
    let db_path = cfg.db_path.as_str();
    let jobs = crate::schedule::jobs_from_dag(dag);
    let ids: Vec<String> = jobs.iter().map(|job| job.id.clone()).collect();
    let (ages, pinned, paused_nodes, pauses, failures, ticks, checks) =
        if Path::new(db_path).exists() {
            let ages = db::node_ages(db_path).await?;
            let checks = checks(db_path, dag, &ages, zone).await?;
            (
                ages,
                crate::pins::list(db_path)
                    .await?
                    .into_iter()
                    .map(|pin| pin.node_id)
                    .collect::<HashSet<String>>(),
                crate::pauses::list(db_path)
                    .await?
                    .into_iter()
                    .map(|pause| pause.id)
                    .collect::<HashSet<String>>(),
                db::get_schedule_pauses(db_path).await?,
                db::get_schedule_failures(db_path).await?,
                crate::schedule_standing::recorded(db_path, &ids).await?,
                checks,
            )
        } else {
            Default::default()
        };
    let paused = |id: &str| {
        pauses
            .get(id)
            .is_some_and(|p| p.until.is_none_or(|until| until > now.timestamp()))
    };
    // Row 6: late is the schedule's standing, the one definition (#958).
    let late = crate::schedule_standing::late_schedules(&jobs, &ticks, zone, now, &paused);
    // The scheduler's walk, once per command (#961); no scheduler runs here, so no schedule
    // is live.
    let paths = crate::refresh_path::Paths::of(dag);
    let schedules =
        crate::refresh_path::recorded_schedules(&jobs, &ticks, &failures, &pauses, zone, now);
    // No scheduler and no run queue here: only `stale_since` can be told, from the records
    // and the source files' modification times. The typical durations are read only for the
    // rollup, whose roots say what clearing them costs.
    let source_times = crate::estimates::source_times_of(dag);
    let durations: HashMap<String, f64> = if rollup && Path::new(db_path).exists() {
        let ids: Vec<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
        db::recent_durations(db_path, &ids, 20)
            .await?
            .into_iter()
            .filter(|(_, recent)| !recent.is_empty())
            .map(|(id, recent)| (id, crate::schedule_standing::median(&recent)))
            .collect()
    } else {
        HashMap::new()
    };
    let evidence = Evidence {
        ages: &ages,
        pinned: &pinned,
        late: &late,
        checks: &checks,
        path: crate::refresh_path::PathEvidence {
            paths: &paths,
            schedules: &schedules,
            paused: &paused_nodes,
            pinned: &pinned,
        },
        timing: crate::estimates::Timing {
            durations: &durations,
            source_times: &source_times,
            max_runs: cfg.server_max_runs,
            ..crate::estimates::Timing::none()
        },
        now: now.timestamp(),
    };
    let running = HashSet::new();
    attach(result, evidence, &running);
    Ok(rollup
        .then(|| crate::rollup::build(&result.nodes, &dag.groups, &running, now, evidence, false)))
}

/// What the tick record at `db_path` says about the nodes of `dag` ([`Checks`]): two bounded
/// reads for the whole graph, never one per node. Cron is evaluated in `zone`. It depends on
/// the database, the graph and the ages read from that database, not on the clock, so a
/// server keeps it until one of them changes.
pub async fn checks(
    db_path: &str,
    dag: &crate::dag::Dag,
    ages: &HashMap<String, NodeAge>,
    zone: &Zone,
) -> Result<Checks, BarcaError> {
    let ticks = db::latest_ticks(db_path).await?;
    if ticks.outcomes.is_empty() {
        return Ok(Checks::default());
    }
    let misses = ticks.misses;
    let crons: HashMap<String, _> =
        crate::schedule::jobs_from_summaries(crate::queries::list_assets_from_dag(dag))
            .into_iter()
            .map(|job| (job.id, job.cron))
            .collect();
    let mut by_node = HashMap::with_capacity(ticks.outcomes.len());
    // The newest successful row of each asset a tick's verdict could decide.
    let mut newest: HashMap<i64, &crate::model::DagNode> = HashMap::new();
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
            let next =
                crate::schedule::next_fire(cron, zone, chrono::DateTime::from_timestamp(slot, 0)?)?;
            let interval = next.timestamp() - slot;
            // The grace a late schedule gets (`crate::schedule_standing`).
            let grace = crate::schedule_standing::grace_seconds(Some(slot), Some(next.timestamp()));
            Some(finished_at + (finished_at - slot as f64).max(0.0) + (interval + grace) as f64)
        });
        if matches!(
            outcome.outcome,
            TickTargetOutcome::UpToDate | TickTargetOutcome::Rebuilt
        ) && node.kind() == crate::NodeKind::Asset
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
            same_definition(recorded, &node.definition_hash, &node.components)
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
    if let Some(definition) = recorded.get(crate::changes::DEFINITION_KEY) {
        return definition == definition_hash;
    }
    // The components of a run hash that are not its definition's (`crate::changes`).
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
    use crate::db::{RowChange, RowFacts};
    use crate::status::{LastMaterialization, PartitionState};

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
                    last_run: Some(crate::db::RunTotals {
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
