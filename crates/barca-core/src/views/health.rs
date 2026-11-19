//! The words of a node's health (`specs/behavior/health.md`) and what a tick's check recorded:
//! plain data every read model of the analysis layer shares. [`crate::health`] decides them.

use super::NodeStatus;
use crate::db::TickTargetOutcome;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
    /// `blocked`: a failing node upstream ([`crate::health::blocked_by`]).
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
    /// up to date or rebuilt it, with nothing seen to have changed since ([`crate::health::verdict_stands`]).
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
/// "Times"): the one reason a surface gives instead of a blank. [`crate::health::no_estimate`] names it.
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
    /// No scheduler fires the schedules of the nodes it holds: one root for the project
    /// ([`Root::scheduler_off`]), about no node.
    SchedulerOff,
    /// A failing node: the node itself, or the nearest failing node upstream ([`crate::health::blocked_by`]);
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

/// The upstream fact that explains why a node is not fresh: `<kind>:<node id>`, or
/// `scheduler_off` alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Root {
    /// `<kind>:<node id>`, e.g. `failure:pipeline.py:orders`.
    pub id: String,
    pub kind: RootKind,
    /// The node the fact is about: the failing node, the sensor whose observation changed,
    /// the node whose code changed, the scheduled node of a paused schedule. Empty for
    /// `scheduler_off`, which is about the scheduler.
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

    /// The one root of every node whose refresh path's schedule no scheduler fires: the
    /// scheduler is not running (or never ran, or does not serve that schedule). Its id is
    /// `scheduler_off` and it is about no node (`node` is empty): one entry for the project,
    /// however many schedules are unfired.
    pub fn scheduler_off() -> Self {
        Root {
            id: RootKind::SchedulerOff.as_str().to_string(),
            kind: RootKind::SchedulerOff,
            node: String::new(),
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
    /// The coverage of `nodes`, which [`crate::health::classify`] has filled.
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
/// decides the node's health is [`crate::health::verdict_stands`].
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
            .map(|at| crate::text::ago(now - at.timestamp()))
            .unwrap_or_else(|_| self.at.clone());
        format!("checked {ago}, {}", Self::outcome_word(self.outcome))
    }
}

/// A node's [`LastCheck`] with what [`crate::health::verdict_stands`] reads of it.
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

/// What the tick record says about the nodes of a graph ([`crate::health::checks`]): the same for every
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

/// `last_attempt_failed`: its last materialization failed, or (a partitioned node's last
/// materialization is one key) some of its keys did.
pub fn last_attempt_failed(node: &NodeStatus) -> bool {
    last_materialization_failed(node) || keys_failed(node)
}

pub(crate) fn last_materialization_failed(node: &NodeStatus) -> bool {
    node.last_materialization
        .as_ref()
        .is_some_and(|m| m.status == "failed")
}

pub(crate) fn keys_failed(node: &NodeStatus) -> bool {
    node.partitions.as_ref().is_some_and(|p| p.failed > 0)
}

/// When `created_at` (UTC `YYYY-MM-DD HH:MM:SS`, as materializations record it) was, unix
/// seconds. A time that cannot be read is the end of time: newer than any check.
pub(crate) fn recorded_at(created_at: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .map_or(i64::MAX, |at| at.and_utc().timestamp())
}
