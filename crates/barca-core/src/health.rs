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
use crate::db::{self, NodeAge};
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
    /// `undecided`: `cache.state` is `unknown`. Also the reason of an unclassified row.
    #[default]
    Unknown,
    /// `never_run`: `cache.state` is `never_run`.
    NeverRun,
    /// `never_run`: a task, sensor or `always` asset with no recorded run.
    NoSuccessfulRun,
    /// `ok`: a task, sensor or `always` asset whose last run did not fail.
    LastRunSucceeded,
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
            HealthReason::Unknown => "unknown",
            HealthReason::NeverRun => "never_run",
            HealthReason::NoSuccessfulRun => "no_successful_run",
            HealthReason::LastRunSucceeded => "last_run_succeeded",
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

/// What kind of upstream fact a [`Root`] is.
///
/// SEAM (#963): only `failure` is derived today. The change, schedule and queue kinds of
/// `specs/behavior/health.md` ("Root") are added here, and [`classify`] names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// A failing node: the node itself, or the nearest failing node upstream ([`blocked_by`]).
    Failure,
}

impl RootKind {
    /// The value as JSON spells it, and the prefix of a root's id.
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::Failure => "failure",
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
    /// The node the fact is about: for a failure, the failing node.
    pub node: String,
}

impl Root {
    /// The failure root of `node`: what a failing node and every node it blocks carry.
    pub fn failure(node: &str) -> Self {
        Root {
            id: format!("{}:{node}", RootKind::Failure.as_str()),
            kind: RootKind::Failure,
            node: node.to_string(),
        }
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

/// What classification reads beyond the status rows themselves.
#[derive(Clone, Copy)]
pub struct Evidence<'a> {
    /// Every node's age ([`db::node_ages`]): row counts and when each result was written.
    pub ages: &'a HashMap<String, NodeAge>,
    /// Pinned assets (`barca pin`): never blocked, and a failure above one does not pass it.
    pub pinned: &'a HashSet<String>,
    /// Scheduled nodes whose schedule is late ([`crate::schedule_standing::late_schedules`]).
    pub late: &'a HashSet<String>,
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
/// count no worker recorded raises no signal.
pub fn signals(
    node: &NodeStatus,
    ages: &HashMap<String, NodeAge>,
    is_asset: impl Fn(&str) -> bool,
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

/// The health table of `specs/behavior/health.md`: the first matching row decides.
pub fn decide(
    node: &NodeStatus,
    blocked: bool,
    signals: &[DataSignal],
    late: bool,
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
        // Row 8. SEAM (#964): a row before this one answers `ok`, `checked_up_to_date`, when
        // the newest finished tick that checked the node found it up to date.
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

/// Fill `health`, `health_reason`, `signals`, `measured` and `root` on every row of `nodes` (one
/// status result: a scope with its upstream cone, in dependency order).
pub fn classify(nodes: &mut [NodeStatus], evidence: Evidence<'_>) {
    let assets: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == "asset")
        .map(|n| n.id.as_str())
        .collect();
    let mut blocked = blocked_by(nodes, evidence.pinned);
    let decided: Vec<_> = nodes
        .iter()
        .map(|node| {
            let signals = signals(node, evidence.ages, |id| assets.contains(id));
            let by = blocked.remove(&node.id);
            let (health, reason) = decide(
                node,
                by.is_some(),
                &signals,
                evidence.late.contains(&node.id),
            );
            // SEAM (#963): the root of every other cause is named here.
            let root = match health {
                Health::Failing => Some(Root::failure(&node.id)),
                _ => by.as_deref().map(Root::failure),
            };
            (health, reason, signals, measured(node, evidence.ages), root)
        })
        .collect();
    for (node, (health, reason, signals, measured, root)) in nodes.iter_mut().zip(decided) {
        node.health = health;
        node.health_reason = reason;
        node.signals = signals;
        node.measured = measured;
        node.root = root;
    }
}

/// [`classify`] every row of `result` and count its data coverage, over every node in scope:
/// call it before a filter cuts `nodes`.
pub fn attach(result: &mut StatusResult, evidence: Evidence<'_>) {
    classify(&mut result.nodes, evidence);
    result.data_coverage = DataCoverage::of(&result.nodes);
}

/// [`attach`] with the evidence the metadata DB at `db_path` holds (none when it does not
/// exist yet): what `barca status` and `barca sql` read on this machine. Cron is evaluated in
/// `zone`; a paused schedule is not late.
pub async fn attach_from_db(
    result: &mut StatusResult,
    dag: &crate::dag::Dag,
    db_path: &str,
    zone: &Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), BarcaError> {
    let (ages, pinned, pauses) = if Path::new(db_path).exists() {
        (
            db::node_ages(db_path).await?,
            crate::pins::list(db_path)
                .await?
                .into_iter()
                .map(|pin| pin.node_id)
                .collect(),
            db::get_schedule_pauses(db_path).await?,
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
    let jobs = crate::schedule::jobs_from_dag(dag);
    let ids: Vec<String> = jobs.iter().map(|job| job.id.clone()).collect();
    let ticks = if Path::new(db_path).exists() {
        crate::schedule_standing::recorded(db_path, &ids).await?
    } else {
        Default::default()
    };
    let late = crate::schedule_standing::late_schedules(&jobs, &ticks, zone, now, &paused);
    attach(
        result,
        Evidence {
            ages: &ages,
            pinned: &pinned,
            late: &late,
        },
    );
    Ok(())
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
            decide(n, blocked, signals, late)
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
            signals(&a, &ages, |_| true),
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
        assert!(signals(&a, &quiet, |_| true).is_empty());
        let mut task = node("p.py:a", &[], CacheState::AlwaysRuns);
        task.kind = "task".into();
        assert!(signals(&task, &ages, |_| true).is_empty());
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
                is_asset
            ),
            vec![DataSignal::BehindInput {
                input: "p.py:keys".into(),
                input_success_at: "2026-11-16 09:00:00".into(),
                last_success_at: "2026-11-14 10:00:00".into(),
            }]
        );
        // A state that already says what to do, or that it is current, raises nothing.
        for state in [CacheState::Stale, CacheState::Cached, CacheState::Partial] {
            assert!(signals(&node("p.py:all", &inputs, state), &ages, is_asset).is_empty());
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
                is_asset
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
            filter: None,
        };
        let (_, pinned, late) = none();
        attach(
            &mut result,
            Evidence {
                ages: &ages,
                pinned: &pinned,
                late: &late,
            },
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
}
