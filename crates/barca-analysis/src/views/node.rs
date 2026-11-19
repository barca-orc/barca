//! A node's status row and the result that carries the rows of one scope: what `barca status
//! --json`, `GET /nodes` and `GET /state` serve. [`crate::status`] fills the cache half of a
//! row, [`crate::health`] the rest.

use super::ScheduleStatus;
use barca_model::cache_state::CacheState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResult {
    /// Project-wide maintained cache accounting, filled by status entry points only.
    #[serde(default)]
    pub disk_usage: barca_store::gc::DiskUsage,
    /// The target name, when status was scoped to one node's upstream cone (`null` for the
    /// whole DAG or for several targets).
    pub target: Option<String>,
    /// Every target the status was scoped to, in the order given (empty for the whole DAG).
    #[serde(default)]
    pub targets: Vec<String>,
    /// Every node in scope, in dependency order.
    pub nodes: Vec<NodeStatus>,
    /// Node counts per cache state, for every node in scope: a filter does not change them.
    pub summary: StatusSummary,
    /// How many assets in scope had their data measured (a recorded row count), and how many of
    /// those look wrong: `7 of 556 measured`. For every node in scope, like `summary`.
    #[serde(default)]
    pub data_coverage: super::DataCoverage,
    /// The instant the nodes were classified at (RFC 3339, UTC): what every derived time and
    /// `gap_seconds` are as of. `null` on a result that was not classified.
    #[serde(default)]
    pub as_of: Option<String>,
    /// The scope summed: the root entry of `barca status --rollup` (counts by health and by
    /// cause in nodes and in assets, `gap_seconds`, `unbounded`, `fresh_by`), for every node
    /// in scope, like `summary`. Its `children` are empty here: the tree is the rollup's.
    #[serde(default)]
    pub rollup: Option<super::RollupEntry>,
    /// `--state` / `--problems`: which nodes `nodes` keeps. Absent without a filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<StatusFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeStatus {
    /// Node id, e.g. `pipeline.py:clean`.
    pub id: String,
    /// The declared name, or function name when no explicit name is set.
    pub name: String,
    /// `asset`, `task` or `sensor`.
    #[cfg_attr(feature = "ts", ts(type = "\"asset\" | \"task\" | \"sensor\""))]
    pub kind: String,
    /// Upstream node ids (direct inputs and `collect(...)` inputs), sorted.
    pub inputs: Vec<String>,
    pub partitioned: bool,
    pub cache: CacheStatus,
    /// Whether something needs doing about it, in one word, most severe first: `failing`,
    /// `blocked`, `suspect`, `behind`, `undecided`, `never_run`, `ok` (`barca docs
    /// status-fields`, "Health"). Beside `cache.state`, never in place of it.
    #[serde(default)]
    pub health: super::Health,
    /// Which rule decided `health`.
    #[serde(default)]
    pub health_reason: super::HealthReason,
    /// What its recorded results say about its data (`empty`, `rows_changed`,
    /// `behind_input`); empty when nothing does, which for an unmeasured asset says nothing.
    #[serde(default)]
    pub signals: Vec<super::DataSignal>,
    /// Whether its latest successful result has a recorded row count (for a partitioned asset,
    /// any key's); `null` for a task or sensor. `false` is "not measured", not ok.
    #[serde(default)]
    pub measured: Option<bool>,
    /// Why it is not fresh, in one word: `catching_up`, `queued`, `no_refresh_path`,
    /// `overdue`, `failing` or `undecided` (`barca docs status-fields`, "Cause"); `null`
    /// exactly when it is fresh (`health` is `ok` or `suspect`).
    #[serde(default)]
    pub cause: Option<super::Cause>,
    /// The upstream fact that explains why it is not fresh (`barca docs status-fields`,
    /// "Root"): the sensor whose observation changed, the node whose code changed, the
    /// failing node, the missing or paused schedule. `null` exactly when it is fresh. Every
    /// node with the same `root.id` is held by the same thing (`barca status --rollup`).
    #[serde(default)]
    pub root: Option<super::Root>,
    /// The node's freshness gap, seconds: how long it has been out of date (`stale_since` to
    /// now) plus how long until it is expected fresh (now to `expected_fresh_at`), read at
    /// the start of the current minute. 0 for a fresh or undecided node; a term whose time
    /// is `null` counts 0.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub gap_seconds: i64,
    /// What the newest finished tick that had the node as a target did with it, and when
    /// (`barca docs status-fields`, "Last check"); `null` when no retained tick did. When the
    /// node's own state is `unknown`, a check that found it up to date decides its health
    /// (`health_reason: "checked_up_to_date"`).
    #[serde(default)]
    pub last_check: Option<super::LastCheck>,
    /// The schedule whose tick brings the node up to date, with its next tick and the ticks
    /// that already missed the node (`barca docs status-fields`, "Refresh path"); `null` when
    /// no tick has the node as a target, or the node is paused or pinned.
    #[serde(default)]
    pub refresh_path: Option<super::RefreshPath>,
    /// Since when the node has been out of date (RFC 3339, UTC): the earliest recorded time
    /// from which it has been continuously not fresh (`barca docs status-fields`, "Times").
    /// `null` for a fresh or undecided node and where no record holds the time.
    #[serde(default)]
    pub stale_since: Option<String>,
    /// When the node is expected to be fresh (RFC 3339, UTC), an estimate rounded up to 5
    /// seconds: the start of the run that computes it plus the typical durations of the
    /// steps before it. `null` for a fresh node and where no run is known to leave it fresh
    /// (failing, blocked, undecided, missed by a tick, no refresh path).
    #[serde(default)]
    pub expected_fresh_at: Option<String>,
    /// When a tick next looks at the node (RFC 3339, UTC): the next tick of its refresh path.
    /// A node with this and no `expected_fresh_at` will be rechecked, not necessarily fresh.
    /// `null` when it has no refresh path or the path has no next tick.
    #[serde(default)]
    pub recheck_at: Option<String>,
    /// Why a node that is not fresh has no `expected_fresh_at`, in one word (`barca docs
    /// status-fields`, "Times"): `failing`, `paused`, `scheduler_off`, `no_path`,
    /// `undecided`, `missed`, `queue_stopped`, `no_scheduler_here` or `not_promised`. `null`
    /// for a fresh node and one that has an estimate.
    #[serde(default)]
    pub no_estimate: Option<super::NoEstimate>,
    /// How its newest attempt went, in one word (`barca docs status-fields`, "Last attempt"):
    /// `failed` when its last materialization failed or any of its keys last failed
    /// (`partitions.failed`), `never_run` when no execution is recorded, else `success`. A
    /// fact beside `cache.state` and `health`: a blocked or stale node whose last run went
    /// well reads `success`, and a cache hit is not an attempt.
    #[serde(default)]
    pub last_attempt: super::LastAttempt,
    /// How long one run of the node typically takes, seconds (`barca docs status-fields`,
    /// "Typical run time"): the median of its recent successful executions, times its number
    /// of keys for a partitioned node (durations are recorded per key, and a run of the node
    /// computes each key). An estimate of a run over every key, not of the next run, which
    /// computes only the keys that need it. Rounded to a millisecond. `null` when it has no
    /// successful execution on record.
    #[serde(default)]
    pub typical_run_seconds: Option<f64>,
    /// Per-key cache state of a partitioned node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<PartitionState>,
    /// The most recent execution (successful or failed); `null` if it never ran. Cache hits
    /// are not executions and do not appear here.
    pub last_materialization: Option<LastMaterialization>,
    /// Last recompute whose bytes differed for unchanged code and inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_bytes_change: Option<barca_db::db::BytesChange>,
    /// Shape of `last_materialization`'s artifact (`null` when there is none).
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub shape: Option<serde_json::Value>,
    /// Environment variables the node declares with `env=[...]` (empty when none); their values
    /// are part of its run hash.
    #[serde(default)]
    pub env: Vec<String>,
    /// Environment variables the node's code reads without declaring them in `env=[...]`
    /// (`barca docs cache`, "Environment variables"), sorted; not part of its run hash. Absent
    /// when there are none. Experimental (#574).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undeclared_env: Vec<String>,
    /// With a configured server (`barca status` answered by the server's ledger): whether this
    /// machine has a copy of the node's result (RFC-0007 section 4.1). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub local: Option<bool>,
    /// A node with `freshness=Schedule(...)`: its schedule's state (`barca status`, "Schedules").
    /// Absent for a node that is not scheduled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub schedule: Option<ScheduleStatus>,
}

/// Which nodes `barca status --state` / `--problems` (and `GET /nodes?state=&problems=`) keep.
/// The summary always counts every node in scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusFilter {
    /// Keep nodes whose `cache.state` is one of these (snake_case, as in JSON).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<CacheState>,
    /// Keep nodes that need attention ([`needs_attention`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub problems: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// How many nodes passed, before any `--limit` (the table's note).
    #[serde(skip)]
    pub matched: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheStatus {
    /// `cached`, `unsynced`, `stale`, `never_run`, `partial`, `blocked`, `unknown` or
    /// `always_runs`.
    pub state: CacheState,
    /// Machine-readable reason: `materialized`, `local_only`, `store_only`, `mismatch`,
    /// `not_in_ledger` (the CLI with a configured server; the server never answers it), `changed`,
    /// `upstream_stale`, `upstream_failed`, `failed`, `artifact_missing`, `evicted`, `no_record`,
    /// `partitions_missing`, `partitions_unknown`, `sensor_output_unknown`, `cutoff_pending`,
    /// `not_planned` (the dry run status is computed from has no step for it), `task` or
    /// `sensor`: [`CACHE_REASONS`].
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"materialized\" | \"local_only\" | \"store_only\" | \"mismatch\" | \"not_in_ledger\" | \"changed\" | \"upstream_stale\" | \"upstream_failed\" | \"failed\" | \"artifact_missing\" | \"evicted\" | \"no_record\" | \"partitions_missing\" | \"partitions_unknown\" | \"sensor_output_unknown\" | \"cutoff_pending\" | \"not_planned\" | \"task\" | \"sensor\""
        )
    )]
    pub reason: String,
    /// The reason in words.
    pub detail: String,
    /// The run hash this code and these inputs hash to now (the cache key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_hash: Option<String>,
    /// The artifact a `get` would serve from cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// A `stale` or failed node: what changed in its run hash since its last successful materialization
    /// (`code`, `decorator argument NAME`, `decorator NAME`, `helper FILE:NAME`, `input ID`,
    /// `sensor ID`, `env NAME`, `kind`). Absent when that materialization recorded no components
    /// (before 0.29.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<Vec<String>>,
    /// What moved the run hash, by cause: any of `code_changed`, `inputs_changed`, `env_changed`
    /// (from `changed`) and `upstream_stale` (an input is not cached yet). Set on `stale` nodes
    /// and on a failed node that once succeeded; absent when the cause is not recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(
            optional,
            type = "Array<\"code_changed\" | \"inputs_changed\" | \"env_changed\" | \"upstream_stale\">"
        )
    )]
    pub change_causes: Option<Vec<String>>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionState {
    pub total: usize,
    pub cached: usize,
    pub missing: usize,
    /// Keys without a cached result, capped at 20.
    pub missing_keys: Vec<String>,
    /// How many of `missing` last failed, rather than never having run or going stale.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub failed: usize,
    /// The first of them in key order, capped at 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub failed_keys: Vec<String>,
    /// For each key of `missing_keys` with an earlier successful result, what changed in its
    /// run hash since then (`value ID` when a dict `partitions_from` source returned a new value
    /// for it). A new key is absent. Absent when no key has an earlier result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub changed: Option<std::collections::BTreeMap<String, Vec<String>>>,
    /// Keys whose latest successful result wrote no rows (its row count was recorded and is 0).
    /// Absent when none is.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub empty: usize,
    /// How many of `missing` wait on the same keys of an input, which failed (`barca docs
    /// partitions`, "Failed keys block their own keys"): a run skips them. Absent when none
    /// does; the node's `cache.state` is then `blocked`.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub blocked: usize,
    /// The node whose failed keys they wait on, by id. Absent when no key is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub blocked_on: Option<String>,
    /// How many of `missing` neither failed nor are blocked: stale, or never run. A run
    /// computes them. `failed`, `blocked` and `behind` add up to `missing`.
    #[serde(default)]
    pub behind: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LastMaterialization {
    /// SHA-256 of the serialized artifact, absent for legacy materializations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// `success` or `failed` (the generated `MaterializationStatus` is the vocabulary; a row
    /// recorded by another release is served as it is).
    pub status: String,
    /// When it was recorded (UTC, `YYYY-MM-DD HH:MM:SS`).
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub run_hash: Option<String>,
    pub artifact: Option<String>,
    /// `json`, `pickle` or `parquet`.
    pub format: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
    /// Rows of a successful tabular result, as its worker counted them: a frame's length or a
    /// parquet file's footer. `null` when not counted (not a frame, a failed attempt, a CSV of a
    /// lazy frame or relation, a row recorded by an older barca).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub rows: Option<i64>,
    /// For a partitioned node: which key this was (e.g. `k=a`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    /// For a failed attempt: the error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusSummary {
    pub cached: usize,
    pub stale: usize,
    pub never_run: usize,
    pub partial: usize,
    /// Partitioned nodes some of whose keys wait on the same keys of an input, which failed
    /// (#764).
    #[serde(default)]
    pub blocked: usize,
    pub unknown: usize,
    pub always_runs: usize,
    /// Cached, but not (yet) where it should be: this machine's copy and the artifact store's
    /// do not agree (RFC-0009: `local_only`, `store_only`, `mismatch`), or, with a server, a
    /// result whose ledger post is pending (`not_in_ledger`).
    #[serde(default)]
    pub unsynced: usize,
}

impl StatusSummary {
    /// Node counts per cache state.
    pub fn of(nodes: &[NodeStatus]) -> Self {
        let mut summary = StatusSummary::default();
        for n in nodes {
            match n.cache.state {
                CacheState::Cached => summary.cached += 1,
                CacheState::Unsynced => summary.unsynced += 1,
                CacheState::Stale => summary.stale += 1,
                CacheState::NeverRun => summary.never_run += 1,
                CacheState::Partial => summary.partial += 1,
                CacheState::Blocked => summary.blocked += 1,
                CacheState::AlwaysRuns => summary.always_runs += 1,
                CacheState::Unknown => summary.unknown += 1,
            }
        }
        summary
    }
}
