//! `barca status` — one read-only view of every node: its definition, cache state, last
//! materialization and artifact shape.
//!
//! Nothing here is new logic. Cache state comes from the same decision `--dry-run` makes
//! ([`crate::execution::explain_dag`]), history from the metadata DB, and artifact shape from
//! `python -m barca._inspect`, which opens artifact files only: user code is never imported.
//! The ledger is read-only; bounded disposable inspection evidence is cached separately.

use crate::BarcaError;
use crate::cache::CachePolicy;
use crate::db;
use crate::results::StepReport;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResult {
    /// Project-wide maintained cache accounting, filled by status entry points only.
    #[serde(default)]
    pub disk_usage: crate::gc::DiskUsage,
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
    /// Per-key cache state of a partitioned node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<PartitionState>,
    /// The most recent execution (successful or failed); `null` if it never ran. Cache hits
    /// are not executions and do not appear here.
    pub last_materialization: Option<LastMaterialization>,
    /// Last recompute whose bytes differed for unchanged code and inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_bytes_change: Option<db::BytesChange>,
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
    #[cfg_attr(feature = "ts", ts(skip))]
    pub schedule: Option<ScheduleStatus>,
}

/// The state of one scheduled node's schedule. With a server it is what the server's scheduler
/// reports (`GET /schedule`, the numbers the web UI shows); on this machine there is no
/// scheduler, so it is read from the metadata DB, which holds what a `barca serve` of this
/// `.barca/` recorded (failures, pauses, the last tick), and `live` is false.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleStatus {
    /// The cron expression.
    pub cron: String,
    /// When the cron expression next matches (RFC 3339, UTC): in the server's `--timezone` from
    /// a server, in this machine's local time otherwise, as `barca list` computes it.
    pub next_fire: Option<String>,
    /// The last tick the scheduler fired (RFC 3339, UTC), if any.
    pub last_fired: Option<String>,
    /// Status of the run the last tick started, while the server remembers it (`null` after a
    /// restart, and always on this machine).
    pub last_status: Option<String>,
    /// Consecutive failed ticks.
    pub failures: u32,
    /// `Schedule(..., max_failures=N)`, default 5.
    pub max_failures: u32,
    /// `failures >= max_failures`: ticks are skipped until an explicit request or a change in
    /// the node's inputs.
    pub failing: bool,
    /// Held back by `POST /schedule/{id}/pause`.
    pub paused: bool,
    /// When a timed pause ends (RFC 3339, UTC); `null` for a pause until resumed.
    pub paused_until: Option<String>,
    /// Why the last due tick was skipped: `in_flight` or `paused` (server only).
    pub skip_reason: Option<String>,
    /// True when a running scheduler reported this (a `barca serve` that fires schedules);
    /// false when it was read from the metadata DB because no scheduler runs here.
    pub live: bool,
}

impl ScheduleStatus {
    /// A schedule that needs attention: paused, or skipped after `max_failures` failed ticks.
    pub fn needs_attention(&self) -> bool {
        self.paused || self.failing
    }

    /// The table's SCHEDULE cell: `*/5 * * * *; failing 3/3`, `0 6 * * *; paused`,
    /// `0 6 * * *; next 2026-10-23 06:00:00 UTC`.
    pub fn cell(&self) -> String {
        let state = if self.failing {
            format!("failing {}/{}", self.failures, self.max_failures)
        } else if self.paused {
            match &self.paused_until {
                Some(until) => format!("paused until {}", crate::report::utc_cell(until)),
                None => "paused".to_string(),
            }
        } else {
            let next = self
                .next_fire
                .as_deref()
                .map(|t| format!("next {}", crate::report::utc_cell(t)))
                .unwrap_or_else(|| "no next fire".to_string());
            match self.failures {
                0 => next,
                1 => format!("1 failure, {next}"),
                n => format!("{n} failures, {next}"),
            }
        };
        format!("{}; {state}", self.cron)
    }
}

/// Unix seconds as RFC 3339 UTC (`2026-10-22T06:00:00Z`).
pub fn rfc3339_secs(secs: i64) -> Option<String> {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(secs, 0)
        .single()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Give every scheduled node in `nodes` its [`ScheduleStatus`] from the metadata DB at
/// `db_path` (none yet: no failures, no pauses), with next fire times evaluated in `zone`.
pub async fn attach_schedules(
    nodes: &mut [NodeStatus],
    dag: &crate::dag::Dag,
    db_path: Option<&str>,
    zone: &crate::schedule::Zone,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), BarcaError> {
    let jobs = crate::schedule::jobs_from_dag(dag);
    if jobs.is_empty() {
        return Ok(());
    }
    let (fired, failures, pauses) = match db_path.filter(|p| Path::new(p).exists()) {
        Some(path) => (
            db::get_schedule_state(path).await?,
            db::get_schedule_failures(path).await?,
            db::get_schedule_pauses(path).await?,
        ),
        None => Default::default(),
    };
    for node in nodes.iter_mut() {
        let Some(job) = jobs.iter().find(|j| j.id == node.id) else {
            continue;
        };
        let failures = failures.get(&job.id).map_or(0, |f| f.failures);
        let pause = pauses
            .get(&job.id)
            .filter(|p| p.until.is_none_or(|until| until > now.timestamp()));
        node.schedule = Some(ScheduleStatus {
            cron: job.cron_str.clone(),
            next_fire: crate::schedule::next_fire(&job.cron, zone, now)
                .and_then(|t| rfc3339_secs(t.timestamp())),
            last_fired: fired.get(&job.id).and_then(|s| rfc3339_secs(*s)),
            last_status: None,
            failures,
            max_failures: job.max_failures,
            failing: failures >= job.max_failures,
            paused: pause.is_some(),
            paused_until: pause.and_then(|p| p.until).and_then(rfc3339_secs),
            skip_reason: None,
            live: false,
        });
    }
    Ok(())
}

/// The `cache.state` values, as JSON and `--state` spell them.
pub const STATES: &[&str] = &[
    "cached",
    "unsynced",
    "stale",
    "never_run",
    "partial",
    "blocked",
    "unknown",
    "always_runs",
];

/// Which nodes `barca status --state` / `--problems` (and `GET /nodes?state=&problems=`) keep.
/// The summary always counts every node in scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusFilter {
    /// Keep nodes whose `cache.state` is one of these (snake_case, as in JSON).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    /// Keep nodes that need attention ([`needs_attention`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub problems: bool,
    /// How many nodes passed, before any `--limit` (the table's note).
    #[serde(skip)]
    pub matched: usize,
}

impl StatusFilter {
    /// Parse `--state a,b` (or `?state=a,b`): each one of [`STATES`], `never-run` spelled either
    /// way. An unknown state is an error naming the valid ones.
    pub fn parse_states(raw: &[String]) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        for state in raw.iter().flat_map(|s| s.split(',')) {
            let state = state.trim();
            if state.is_empty() {
                continue;
            }
            let snake = state.replace('-', "_");
            if !STATES.contains(&snake.as_str()) {
                let hint = if snake == "failed" {
                    " A failed node is `stale` or `never_run` with reason `failed`: --problems \
                     shows it."
                } else {
                    ""
                };
                return Err(format!(
                    "unknown state '{state}'\nUse one of {}.{hint}",
                    STATES.join(", ")
                ));
            }
            if !out.contains(&snake) {
                out.push(snake);
            }
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty() && !self.problems
    }

    /// Whether `node` passes: its state is one of `states` (when given) and, with `problems`,
    /// it needs attention.
    pub fn keep(&self, node: &NodeStatus) -> bool {
        (self.states.is_empty() || self.states.contains(&node.cache.state))
            && (!self.problems || needs_attention(node))
    }

    /// Drop the nodes that do not pass, and record the filter on the result. The summary is
    /// left as it was: it counts every node in scope.
    pub fn apply(&self, result: &mut StatusResult) {
        if self.is_empty() {
            return;
        }
        result.nodes.retain(|n| self.keep(n));
        result.filter = Some(StatusFilter {
            matched: result.nodes.len(),
            ..self.clone()
        });
    }

    /// The flags that select this filter, for the table's note: `--problems`, `--state stale`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.problems {
            parts.push("--problems".to_string());
        }
        if !self.states.is_empty() {
            parts.push(format!("--state {}", self.states.join(",")));
        }
        parts.join(" ")
    }
}

/// A node that is not fine (`--problems`): every state but `cached` and `always_runs`
/// (`unsynced`, `stale`, `never_run`, `partial`, `unknown`), a task or sensor whose last run
/// failed, and a node whose schedule is paused or failing.
pub fn needs_attention(node: &NodeStatus) -> bool {
    let state_ok = matches!(node.cache.state.as_str(), "cached" | "always_runs");
    let last_failed = node.cache.state == "always_runs"
        && node
            .last_materialization
            .as_ref()
            .is_some_and(|m| m.status == "failed");
    !state_ok
        || last_failed
        || node
            .schedule
            .as_ref()
            .is_some_and(ScheduleStatus::needs_attention)
}

/// The first line of a failure's message, at most `max` characters: `ZeroDivisionError:
/// division by zero` out of a traceback.
pub fn short_error(error: &str, max: usize) -> String {
    let lines: Vec<&str> = error
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // A Python traceback ends with the exception line; anything else starts with its message.
    let line = if lines.first().is_some_and(|l| l.starts_with("Traceback")) {
        lines.last().copied().unwrap_or_default()
    } else {
        lines.first().copied().unwrap_or_default()
    };
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let cut: String = line.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheStatus {
    /// `cached`, `unsynced`, `stale`, `never_run`, `partial`, `blocked`, `unknown` or
    /// `always_runs`.
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"cached\" | \"unsynced\" | \"stale\" | \"never_run\" | \"partial\" | \"blocked\" | \"unknown\" | \"always_runs\""
        )
    )]
    pub state: String,
    /// Machine-readable reason: `materialized`, `local_only`, `store_only`, `mismatch`,
    /// `not_in_ledger` (the CLI with a configured server; the server never answers it), `changed`,
    /// `upstream_stale`, `upstream_failed`, `failed`, `artifact_missing`, `evicted`, `no_record`,
    /// `partitions_missing`, `partitions_unknown`, `sensor_output_unknown`, `cutoff_pending`,
    /// `task` or `sensor`.
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"materialized\" | \"local_only\" | \"store_only\" | \"mismatch\" | \"not_in_ledger\" | \"changed\" | \"upstream_stale\" | \"upstream_failed\" | \"failed\" | \"artifact_missing\" | \"evicted\" | \"no_record\" | \"partitions_missing\" | \"partitions_unknown\" | \"sensor_output_unknown\" | \"cutoff_pending\" | \"task\" | \"sensor\""
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LastMaterialization {
    /// SHA-256 of the serialized artifact, absent for legacy materializations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// `success` or `failed`.
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
            match n.cache.state.as_str() {
                "cached" => summary.cached += 1,
                "unsynced" => summary.unsynced += 1,
                "stale" => summary.stale += 1,
                "never_run" => summary.never_run += 1,
                "partial" => summary.partial += 1,
                "blocked" => summary.blocked += 1,
                "always_runs" => summary.always_runs += 1,
                _ => summary.unknown += 1,
            }
        }
        summary
    }
}

/// One result this machine computed whose post to the server's ledger is pending: the step's
/// node id (with any partition suffix), its run hash, and why the post did not arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPost {
    pub node_id: String,
    pub run_hash: String,
    pub error: Option<String>,
}

/// A node the server's ledger does not hold a result for, but this machine computed one whose
/// post is pending, is `unsynced` with reason `not_in_ledger` (beside RFC-0009's `local_only`,
/// `store_only` and `mismatch`). `get` serves it from this machine; the next `get` or `run`
/// against the server posts it again. A node the ledger already has as `cached` is left alone,
/// and so is one whose current run hash differs from the pending one (the server's code moved
/// on). Returns true when `node` changed.
pub fn not_in_ledger(node: &mut NodeStatus, pending: &[PendingPost]) -> bool {
    if node.cache.state == "cached" || node.kind != "asset" {
        return false;
    }
    let Some(p) = pending_for(
        &node.id,
        node.partitioned,
        node.cache.run_hash.as_deref(),
        pending,
    ) else {
        return false;
    };
    node.cache.state = "unsynced".to_string();
    node.cache.reason = "not_in_ledger".to_string();
    node.cache.detail = format!(
        "computed here and not recorded on the server yet: {}; the next get or run against the \
         server posts it",
        p.error.as_deref().unwrap_or("its post is pending")
    );
    node.local = Some(true);
    true
}

/// The pending post of `node_id` that [`not_in_ledger`] counts: one of its steps (any key of a
/// partitioned node), at the run hash the server's code gives it now when that is known.
pub fn pending_for<'a>(
    node_id: &str,
    partitioned: bool,
    run_hash: Option<&str>,
    pending: &'a [PendingPost],
) -> Option<&'a PendingPost> {
    pending.iter().find(|p| {
        let base = p.node_id.split('[').next().unwrap_or(&p.node_id);
        base == node_id
            && (partitioned || run_hash.is_none() || run_hash == Some(p.run_hash.as_str()))
    })
}

/// Gather status for every node in scope (the targets' upstream cones, or the whole DAG when
/// `target_names` is empty). `sample` > 0 adds up to that many sample rows to json/parquet
/// shapes; `shape` = false skips the artifact reader entirely.
pub async fn status(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<StatusResult, BarcaError> {
    let dag = crate::load::build_dag(file_args, python).await?;
    let mut result =
        status_from_dag_with_membership(cfg, target_names, &dag, python, sample, shape)
            .await?
            .0;
    // No scheduler runs here: a schedule's state is what the metadata DB holds.
    attach_schedules(
        &mut result.nodes,
        &dag,
        Some(&cfg.db_path),
        &crate::schedule::Zone::Local,
        chrono::Utc::now(),
    )
    .await?;
    Ok(result)
}

/// SQL reuses this prediction instead of loading the DAG or interpreting keys again.
pub(crate) async fn status_with_membership(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<(StatusResult, crate::cache::ExpandedMembership), BarcaError> {
    let t0 = std::time::Instant::now();
    let dag = crate::load::build_dag(file_args, python).await?;
    trace_since("status: load", t0);
    status_from_dag_with_membership(cfg, target_names, &dag, python, sample, shape).await
}

/// Inspect a validated graph; shares cache decisions with strict status.
pub async fn status_from_dag(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    dag: &crate::dag::Dag,
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<StatusResult, BarcaError> {
    Ok(
        status_from_dag_with_membership(cfg, target_names, dag, python, sample, shape)
            .await?
            .0,
    )
}

/// Preserve the given graph and move the prediction's private membership to SQL.
pub(crate) async fn status_from_dag_with_membership(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    dag: &crate::dag::Dag,
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<(StatusResult, crate::cache::ExpandedMembership), BarcaError> {
    let t0 = std::time::Instant::now();
    let (explained, membership) = crate::execution::explain_dag_with_membership(
        dag,
        cfg,
        target_names,
        CachePolicy::CacheAware,
        "status",
        None,
        &crate::freshness::RunRules::default(),
        &[],
    )
    .await?;
    trace_since("status: cache decisions", t0);
    let t0 = std::time::Instant::now();
    let reports: HashMap<&str, &StepReport> =
        explained.steps.iter().map(|s| (s.id.as_str(), s)).collect();

    // Scope: the target's upstream cone (the same nodes the dry run planned), in topo order.
    let in_scope: HashSet<&str> = explained.steps.iter().map(|s| s.id.as_str()).collect();
    let ids: Vec<&str> = dag
        .topo_order()
        .into_iter()
        .filter(|id| target_names.is_empty() || in_scope.contains(id))
        .collect();

    // History, without creating a DB that does not exist yet.
    let histories = if Path::new(&cfg.db_path).exists() {
        // A database from an earlier release may lack a column format 1 added since; complete
        // it (a complete one is left untouched) before querying those columns.
        db::init_db(&cfg.db_path).await?;
        let owned: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        db::node_histories(&cfg.db_path, &owned).await?
    } else {
        HashMap::new()
    };

    trace_since("status: histories", t0);
    let t0 = std::time::Instant::now();
    // Results computed here whose upload to the store is still pending (#390).
    let pending = if cfg.remote_artifacts() {
        crate::pending_uploads::list(&cfg.db_path).await?
    } else {
        Vec::new()
    };

    let failed_keys = if Path::new(&cfg.db_path).exists() {
        db::failed_partition_keys(&cfg.db_path).await?
    } else {
        HashMap::new()
    };

    let mut nodes: Vec<NodeStatus> = Vec::new();
    let mut states: HashMap<String, String> = HashMap::new();
    // Each blocked node: how many of its keys wait, and on which failed node.
    let mut blocked: HashMap<String, (usize, String)> = HashMap::new();
    for id in ids {
        let Some(node) = dag.get_node(id) else {
            continue;
        };
        let mut inputs: Vec<String> = node
            .resolved_inputs
            .values()
            .chain(node.resolved_collected.values())
            .cloned()
            .collect();
        inputs.sort();
        inputs.dedup();
        let history = histories.get(id).cloned().unwrap_or_default();
        let report = reports.get(id).copied();
        let partitions = report
            .and_then(|r| r.partitions.as_ref())
            .map(|p| PartitionState {
                total: p.total,
                cached: p.cached,
                missing: p.will_run,
                missing_keys: p.will_run_keys.clone(),
                // A key that failed and is cached again through an older result is not missing.
                failed: failed_keys.get(id).map_or(0, |f| f.count.min(p.will_run)),
                failed_keys: if p.will_run > 0 {
                    failed_keys.get(id).map_or(Vec::new(), |f| f.keys.clone())
                } else {
                    Vec::new()
                },
                changed: p.changed.clone(),
            });
        let mut cache = cache_status(report, &inputs, &states, &history);
        local_only(
            &mut cache,
            id,
            !node.extracted.partitions.is_empty(),
            &pending,
        );
        // Keys that wait on the same keys of an input, which failed, are blocked (#764): a run
        // computes the node's other keys and skips these. Its own failed keys come first.
        if let Some(p) = partitions.as_ref().filter(|p| p.failed == 0)
            && !node.extracted.partitions.is_empty()
            && matches!(cache.state.as_str(), "stale" | "never_run" | "partial")
            && let Some((count, origin)) = blocked_by(dag, node, &failed_keys, &blocked, p.missing)
        {
            cache.state = "blocked".to_string();
            cache.reason = "upstream_failed".to_string();
            cache.detail = format!(
                "{count} of {} missing keys wait on '{}': the same keys of it failed",
                p.missing,
                short_name(&origin)
            );
            blocked.insert(id.to_string(), (count, origin));
        }
        states.insert(id.to_string(), cache.state.clone());

        let last_materialization = history.latest.map(|m| LastMaterialization {
            content_hash: m.content_hash,
            partition: partition_of(&m.node_id),
            status: m.status,
            created_at: m.created_at,
            elapsed_seconds: m.elapsed_seconds,
            run_hash: m.run_hash,
            artifact: m.artifact_path,
            format: m.artifact_format,
            size_bytes: m.artifact_size_bytes,
            error: m.error_message,
        });
        nodes.push(NodeStatus {
            id: id.to_string(),
            name: node
                .extracted
                .explicit_name
                .as_deref()
                .unwrap_or(node.function_name())
                .to_string(),
            kind: serde_json::to_value(node.kind())
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "unknown".to_string()),
            inputs,
            partitioned: !node.extracted.partitions.is_empty(),
            cache,
            partitions: partitions.filter(|_| !node.extracted.partitions.is_empty()),
            last_materialization,
            // Only an asset's new bytes under the same run hash mean it is not deterministic.
            last_bytes_change: history
                .last_bytes_change
                .filter(|_| crate::cache::checks_bytes(node)),
            shape: None,
            env: node.extracted.env.clone(),
            undeclared_env: node.extracted.undeclared_env(),
            local: None,
            schedule: None,
        });
    }

    // With a store, `cached` means this machine's copy, the store's object and the recorded
    // digest all agree (RFC-0009); anything else is `unsynced` with its reason.
    let verified_local = if cfg.remote_artifacts() && Path::new(&cfg.db_path).exists() {
        check_store_copies(python, cfg, &mut nodes).await
    } else {
        HashSet::new()
    };

    trace_since("status: store check", t0);
    let t0 = std::time::Instant::now();
    if shape {
        read_shapes(python, cfg, &mut nodes, sample, &verified_local).await;
    }
    trace_since("status: shapes", t0);

    let summary = StatusSummary::of(&nodes);
    Ok((
        StatusResult {
            disk_usage: Default::default(),
            targets: explained.target_names(),
            target: explained.target,
            nodes,
            summary,
            filter: None,
        },
        membership,
    ))
}

/// With `BARCA_TRACE_TIMING` set: one stderr line with how long `what` took since `t0`, so the
/// cost of each part of `barca status` (and of a dry run) can be read without a profiler.
pub(crate) fn trace_since(what: &str, t0: std::time::Instant) {
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        crate::errln!(
            "[timing] {what}: {:.1} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// How many of `node`'s `missing` keys wait on a failed key of the same name: the node reads an
/// input key for key (both have exactly the same dimension names and the input is not
/// collected) whose keys failed, or are blocked in turn. Names the node whose keys failed.
fn blocked_by(
    dag: &crate::dag::Dag,
    node: &crate::model::DagNode,
    failed_keys: &HashMap<String, db::FailedKeys>,
    blocked: &HashMap<String, (usize, String)>,
    missing: usize,
) -> Option<(usize, String)> {
    let mine: std::collections::BTreeSet<&String> = node.extracted.partitions.keys().collect();
    let mut reads: Vec<&String> = node.resolved_inputs.values().collect();
    reads.sort();
    reads.dedup();
    reads
        .into_iter()
        .filter(|up| {
            dag.get_node(up).is_some_and(|u| {
                let theirs: std::collections::BTreeSet<&String> =
                    u.extracted.partitions.keys().collect();
                theirs == mine
            })
        })
        .find_map(|up| match failed_keys.get(up) {
            Some(f) if f.count > 0 => Some((f.count, up.clone())),
            _ => blocked.get(up).cloned(),
        })
        .map(|(count, origin)| (count.min(missing), origin))
        .filter(|(count, _)| *count > 0)
}

/// A cached node whose result was computed here and whose upload is still pending is
/// `unsynced`, `local_only` (RFC-0009 section 4.1). A `get` serves it from this machine; the
/// next command that opens the store uploads it. A partitioned node is unsynced when any of
/// its keys is pending.
fn local_only(
    cache: &mut CacheStatus,
    id: &str,
    partitioned: bool,
    pending: &[crate::pending_uploads::PendingUpload],
) {
    if cache.state != "cached" {
        return;
    }
    let mine = pending.iter().find(|p| {
        let base = p.node_id.split('[').next().unwrap_or(&p.node_id);
        base == id && (partitioned || cache.run_hash.is_none() || p.run_hash == cache.run_hash)
    });
    let Some(p) = mine else {
        return;
    };
    cache.state = "unsynced".to_string();
    cache.reason = "local_only".to_string();
    cache.detail = format!(
        "computed here and not in the artifact store yet: {}; the next get or run uploads it",
        p.error.as_deref().unwrap_or("its upload is pending")
    );
}

/// Where a cached result's copies are, against what was recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Copies {
    /// This machine's copy, the store's object and the recorded digest agree.
    InSync,
    /// Here, not in the store.
    LocalOnly,
    /// In the store, not here.
    StoreOnly,
    /// Both are there and differ (or this machine's copy is not the recorded one): why.
    Mismatch(String),
    /// Neither copy is anywhere.
    Missing,
}

/// What the store said about one object (`python -m barca._state stat`). The server's ledger
/// checks a post's objects with the same answer.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StoreObject {
    pub exists: bool,
    pub size_bytes: Option<u64>,
    /// Only for a store that is a plain directory: object stores give no content checksum to
    /// compare.
    pub sha256: Option<String>,
}

/// Classify a result's copies. `local` is this machine's copy (size, SHA-256) when there is
/// one; `recorded` the digest its row was recorded with. Without a checksum from the store,
/// copies of the same size are taken to agree.
pub(crate) fn classify(
    local: Option<(u64, &str)>,
    store: &StoreObject,
    recorded: Option<&str>,
) -> Copies {
    match (local, store.exists) {
        (None, false) => Copies::Missing,
        (None, true) => Copies::StoreOnly,
        (Some(_), false) => Copies::LocalOnly,
        (Some((size, sha)), true) => {
            if recorded.is_some_and(|r| r != sha) {
                Copies::Mismatch(
                    "this machine's copy is not the one recorded; get fetches the store's copy and records \
                     it"
                        .to_string(),
                )
            } else if store.size_bytes.is_some_and(|s| s != size) {
                Copies::Mismatch(format!(
                    "this machine's copy ({size} bytes) and the store's ({} bytes) differ; get \
                     keeps this machine's copy and uploads it again",
                    store.size_bytes.unwrap_or_default()
                ))
            } else if store.sha256.as_deref().is_some_and(|s| s != sha) {
                Copies::Mismatch(
                    "this machine's copy and the store's have different bytes; get keeps this \
                     machine's copy and uploads it again"
                        .to_string(),
                )
            } else {
                Copies::InSync
            }
        }
    }
}

/// For every `cached` unpartitioned node: compare this machine's copy, the store's object (one
/// `python -m barca._state stat` for all of them) and the recorded digest, and turn the node
/// `unsynced` (`local_only`, `store_only`, `mismatch`) or `stale` (`artifact_missing`) when they
/// do not agree. A store that cannot be asked leaves the states as they are, with a note.
async fn check_store_copies(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
) -> HashSet<usize> {
    let mut verified_local = HashSet::new();
    let layout = crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
    let Ok(cache) = db::CacheReader::for_config(cfg).await else {
        return verified_local;
    };
    // (node index, store path, local path, recorded digest)
    let mut wanted: Vec<(usize, String, std::path::PathBuf, Option<String>)> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.cache.state != "cached" || n.partitioned {
            continue;
        }
        let Some(hash) = n.cache.run_hash.as_deref() else {
            continue;
        };
        let Some(row) = crate::cache::lookup_cached(&cache, &n.id, hash).await else {
            continue;
        };
        let Some(local) = layout.local_for(&row.path) else {
            continue;
        };
        wanted.push((i, row.path, local, row.content_hash));
    }
    drop(cache);
    if wanted.is_empty() {
        return verified_local;
    }
    let paths: Vec<&str> = wanted.iter().map(|(_, p, _, _)| p.as_str()).collect();
    let local_paths: Vec<_> = wanted
        .iter()
        .map(|(_, _, local, _)| local.to_string_lossy().into_owned())
        .collect();
    let evidence = match inspect_copies(python, cfg, &paths, &local_paths).await {
        Ok(evidence)
            if evidence.objects.len() == wanted.len() && evidence.locals.len() == wanted.len() =>
        {
            evidence
        }
        Ok(_) => return verified_local,
        Err(why) => {
            for (i, ..) in &wanted {
                let c = &mut nodes[*i].cache;
                c.detail = format!(
                    "{} (the artifact store could not be checked: {why})",
                    c.detail
                );
            }
            return verified_local;
        }
    };
    for (((i, store, _, recorded), object), here) in wanted
        .into_iter()
        .zip(evidence.objects)
        .zip(evidence.locals)
    {
        let copies = classify(
            here.as_ref()
                .map(|local| (local.size_bytes, local.sha256.as_str())),
            &object,
            recorded.as_deref(),
        );
        let at = crate::transfer::diagnostic_uri(&store);
        let c = &mut nodes[i].cache;
        let (state, reason, detail) = match copies {
            Copies::InSync => {
                verified_local.insert(i);
                continue;
            }
            Copies::LocalOnly => (
                "unsynced",
                "local_only",
                format!(
                    "computed here and not in the artifact store ({at}); the next get uploads it"
                ),
            ),
            Copies::StoreOnly => (
                "unsynced",
                "store_only",
                format!("in the artifact store ({at}), not on this machine; get fetches it"),
            ),
            Copies::Mismatch(why) => ("unsynced", "mismatch", format!("{why} ({at})")),
            Copies::Missing => (
                "stale",
                "artifact_missing",
                format!(
                    "the artifact is neither on this machine nor in the store ({at}); a run computes it again"
                ),
            ),
        };
        c.state = state.to_string();
        c.reason = reason.to_string();
        c.detail = detail;
        if state != "cached" && state != "unsynced" {
            c.artifact = None;
        }
    }
    verified_local
}

/// Disposable status evidence; publication and transfer checks remain strict.
#[derive(Deserialize)]
struct CopyEvidence {
    objects: Vec<StoreObject>,
    locals: Vec<Option<LocalEvidence>>,
}
#[derive(Deserialize)]
struct LocalEvidence {
    size_bytes: u64,
    sha256: String,
}
async fn inspect_copies(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    paths: &[&str],
    locals: &[String],
) -> Result<CopyEvidence, String> {
    let mut command = crate::backup::state_helper(python, cfg);
    command.arg("stat").kill_on_drop(true);
    let request = serde_json::json!({"paths": paths, "locals": locals, "inspection_cache": cfg.inspection_cache_path()});
    let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(60));
    use crate::helper_proc::JsonFailed;
    match crate::helper_proc::run_json(command, &request, Some(limit)).await {
        Ok(reply) => Ok(reply),
        Err(JsonFailed::Spawn(error) | JsonFailed::Io(error)) => Err(error.to_string()),
        Err(JsonFailed::TimedOut(limit)) => Err(format!("no answer within {}s", limit.as_secs())),
        Err(JsonFailed::Exited(stderr)) => Err(crate::helper_proc::helper_cause(&stderr)),
        Err(JsonFailed::Invalid(error)) => Err(error.to_string()),
    }
}

/// Ask the store about each of `paths` with one `python -m barca._state stat`.
pub(crate) async fn stat_objects(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    paths: &[&str],
) -> Result<Vec<StoreObject>, String> {
    use crate::helper_proc::JsonFailed;
    #[derive(Deserialize)]
    struct Reply {
        objects: Vec<StoreObject>,
    }
    let mut cmd = crate::backup::state_helper(python, cfg);
    cmd.arg("stat").kill_on_drop(true);
    let request = serde_json::json!({ "paths": paths });
    let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(60));
    match crate::helper_proc::run_json::<Reply>(cmd, &request, Some(limit)).await {
        Ok(reply) => Ok(reply.objects),
        Err(JsonFailed::Spawn(e)) => Err(format!("{}: {e}", python.display())),
        Err(JsonFailed::Io(e)) => Err(e.to_string()),
        Err(JsonFailed::TimedOut(limit)) => Err(format!("no answer within {}s", limit.as_secs())),
        Err(JsonFailed::Exited(stderr)) => Err(crate::helper_proc::helper_cause(&stderr)),
        Err(JsonFailed::Invalid(e)) => Err(e.to_string()),
    }
}

fn cache(state: &str, reason: &str, detail: impl Into<String>) -> CacheStatus {
    CacheStatus {
        state: state.to_string(),
        reason: reason.to_string(),
        detail: detail.into(),
        run_hash: None,
        artifact: None,
        changed: None,
        change_causes: None,
    }
}

/// Translate the dry run's verdict for this node into a cache state, adding what the DB knows
/// (did it ever succeed, did the last attempt fail) and what its inputs' states are.
fn cache_status(
    report: Option<&StepReport>,
    inputs: &[String],
    states: &HashMap<String, String>,
    history: &db::NodeHistory,
) -> CacheStatus {
    let Some(r) = report else {
        return cache("unknown", "not_planned", "not part of the execution plan");
    };
    let action = r.action.as_deref().unwrap_or("unknown");
    let mut c = match (action, r.reason.as_deref()) {
        (_, Some("task")) => cache("always_runs", "task", "tasks always re-run"),
        (_, Some("sensor")) => cache("always_runs", "sensor", "sensors always re-run"),
        ("unknown", _) => cache(
            "unknown",
            r.reason.as_deref().unwrap_or("unknown"),
            r.detail.clone().unwrap_or_default(),
        ),
        ("cached", _) => cache(
            "cached",
            "materialized",
            "a successful materialization matches this code and these inputs",
        ),
        // The result is recorded, but its artifact is gone and a run would have to read it.
        ("run", Some("artifact_missing")) => cache(
            "stale",
            "artifact_missing",
            r.detail.clone().unwrap_or_default(),
        ),
        // The same, because the cache size limit deleted the file (RFC-0012).
        ("run", Some("evicted")) => cache("stale", "evicted", r.detail.clone().unwrap_or_default()),
        ("partial", _) => {
            let (cached, total) = r
                .partitions
                .as_ref()
                .map(|p| (p.cached, p.total))
                .unwrap_or_default();
            cache(
                "partial",
                "partitions_missing",
                format!("{cached} of {total} partition keys cached"),
            )
        }
        _ => not_cached(r, inputs, states, history),
    };
    // A consumer of a sensor is predicted from the sensor's last recorded output (#183); keep
    // that caveat from the dry run's detail.
    if let Some(note) = r
        .detail
        .as_deref()
        .and_then(|d| d.find("assumes sensor").map(|i| &d[i..]))
        && !c.detail.contains(note)
    {
        c.detail = format!("{}; {note}", c.detail);
    }
    c.run_hash = r.run_hash.clone();
    if c.state == "cached" {
        c.artifact = r.artifact.clone();
    }
    c
}

/// A node the dry run would execute: `stale` if it ever succeeded, else `never_run`, with the
/// most specific reason the metadata can support.
fn not_cached(
    r: &StepReport,
    inputs: &[String],
    states: &HashMap<String, String>,
    history: &db::NodeHistory,
) -> CacheStatus {
    let state = if history.ever_succeeded {
        "stale"
    } else {
        "never_run"
    };
    let upstream_stale = inputs.iter().find(|up| {
        let base = up.split('[').next().unwrap_or(up);
        states.get(base).is_some_and(|s| {
            matches!(
                s.as_str(),
                "stale" | "never_run" | "partial" | "blocked" | "unknown"
            )
        })
    });
    // The last attempt at exactly this code and these inputs failed.
    let failed_here = history
        .latest
        .as_ref()
        .filter(|m| m.status == "failed" && (r.run_hash.is_none() || m.run_hash == r.run_hash));
    if let Some(m) = failed_here {
        let msg = m.error_message.as_deref().unwrap_or("no message");
        let mut c = cache(state, "failed", format!("last attempt failed: {msg}"));
        if history.ever_succeeded {
            c.changed = r.changed.clone();
            c.change_causes = causes(r.changed.as_deref(), false);
        }
        return c;
    }
    if !history.ever_succeeded {
        return cache(state, "no_record", "no successful materialization recorded");
    }
    if let Some(up) = upstream_stale {
        let mut c = cache(
            state,
            "upstream_stale",
            format!(
                "upstream '{}' is not cached, so this node's inputs will change",
                short_name(up)
            ),
        );
        c.changed = r.changed.clone();
        c.change_causes = causes(r.changed.as_deref(), true);
        return c;
    }
    let mut c = match &r.changed {
        Some(changed) => cache(
            state,
            "changed",
            format!("changed: {}", crate::changes::summary(changed)),
        ),
        None => cache(
            state,
            "changed",
            "code or upstream outputs changed since the last materialization (the run hash differs)",
        ),
    };
    c.changed = r.changed.clone();
    c.change_causes = causes(r.changed.as_deref(), false);
    c
}

fn causes(changed: Option<&[String]>, upstream_stale: bool) -> Option<Vec<String>> {
    let mut out = changed.map(crate::changes::causes).unwrap_or_default();
    if upstream_stale {
        out.push("upstream_stale".to_string());
    }
    (!out.is_empty()).then_some(out)
}

/// `pipeline.py:p[k=a]` -> `k=a`.
fn partition_of(node_id: &str) -> Option<String> {
    let start = node_id.find('[')?;
    Some(node_id[start + 1..].trim_end_matches(']').to_string())
}

fn short_name(node_id: &str) -> &str {
    let base = node_id.split('[').next().unwrap_or(node_id);
    base.rsplit(':').next().unwrap_or(base)
}

/// Fill `shape` for every node whose last materialization produced an artifact, with one call
/// to `python -m barca._inspect`. A remote artifact is read through the same fsspec filesystem
/// the workers use, so the reader gets the same storage options. A failure to run the reader is
/// reported in each shape's `note` rather than failing the command.
async fn read_shapes(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
    sample: usize,
    verified_local: &HashSet<usize>,
) {
    read_shapes_inner(python, cfg, nodes, sample, false, verified_local).await;
}

/// Inspect artifact schemas, including JSON object field and list element types.
/// This richer on-demand view leaves the CLI's existing shape contract unchanged.
pub async fn read_schemas(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
    sample: usize,
) {
    let verified_local = if cfg.remote_artifacts() && Path::new(&cfg.db_path).exists() {
        check_store_copies(python, cfg, nodes).await
    } else {
        HashSet::new()
    };
    read_shapes_inner(python, cfg, nodes, sample, true, &verified_local).await;
}

async fn read_shapes_inner(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
    sample: usize,
    fields: bool,
    verified_local: &HashSet<usize>,
) {
    // A remote-off cache hit can select an older local row than the historical latest
    // materialization. Keep the selected artifact's metadata together rather than read
    // its bytes with the latest row's (possibly different) format.
    let cache = if cfg.remote_off
        && nodes.iter().any(|n| {
            n.cache.artifact.as_ref().is_some_and(|path| {
                n.last_materialization
                    .as_ref()
                    .and_then(|m| m.artifact.as_ref())
                    != Some(path)
            })
        }) {
        db::CacheReader::for_config(cfg).await.ok()
    } else {
        None
    };
    let mut wanted: Vec<(usize, String, String)> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let Some(m) = n.last_materialization.as_ref() else {
            continue;
        };
        if m.status != "success" {
            continue;
        }
        let path = if cfg.remote_off {
            n.cache.artifact.as_ref().or(m.artifact.as_ref())
        } else {
            m.artifact.as_ref()
        };
        let Some(path) = path.filter(|path| cfg.allows_artifact(path)) else {
            continue;
        };
        if cfg.remote_off && m.artifact.as_ref() != Some(path) {
            let (Some(cache), Some(hash)) = (&cache, &n.cache.run_hash) else {
                continue;
            };
            let Some(selected) = crate::cache::lookup_cached(cache, &n.id, hash).await else {
                continue;
            };
            // A concurrent write may have changed which result would now be selected.
            // Do not attach unrelated metadata to the previously reported artifact.
            if selected.path == *path {
                wanted.push((i, selected.path, selected.format));
            }
        } else if verified_local.contains(&i)
            && n.cache.state == "cached"
            && n.cache.artifact.as_deref() == Some(path.as_str())
        {
            // Use the selected, verified local copy instead of reopening remote payloads.
            let layout =
                crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
            let local = layout.local_for(path).filter(|path| path.is_file());
            wanted.push((
                i,
                local.map_or_else(|| path.clone(), |path| path.to_string_lossy().into_owned()),
                m.format.clone().unwrap_or_default(),
            ));
        } else if n.cache.state == "unsynced" && n.cache.reason != "store_only" {
            // Not in the store, or not the same there (#390, RFC-0009): read this machine's
            // copy. A `store_only` result is read from the store like a cached one.
            let layout =
                crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
            let local = layout
                .local_for(path)
                .map_or_else(|| path.clone(), |p| p.to_string_lossy().into_owned());
            wanted.push((i, local, m.format.clone().unwrap_or_default()));
        } else {
            wanted.push((i, path.clone(), m.format.clone().unwrap_or_default()));
        }
    }
    // The inspector only reads artifact files; release the database before launching it.
    drop(cache);
    if wanted.is_empty() {
        return;
    }
    let request = serde_json::json!({
        "sample": sample,
        "fields": fields,
        "inspection_cache": cfg.inspection_cache_path(),
        "artifacts": wanted
            .iter()
            .map(|(_, path, format)| serde_json::json!({"path": path, "format": format}))
            .collect::<Vec<_>>(),
    });
    let inspector = crate::helper_proc::python_module(
        python,
        "barca._inspect",
        cfg.storage_options_json.as_deref(),
    );
    let shapes = match run_json_helper(inspector, python, &request).await {
        Ok(shapes) if shapes.len() == wanted.len() => shapes,
        Ok(_) => vec![note("the shape reader returned an unexpected result"); wanted.len()],
        Err(e) => vec![note(&format!("could not run the shape reader: {e}")); wanted.len()],
    };
    for ((i, _, _), shape) in wanted.into_iter().zip(shapes) {
        nodes[i].shape = Some(shape);
    }
}

fn note(msg: &str) -> serde_json::Value {
    serde_json::json!({ "note": msg })
}

/// Run a Python helper that reads one JSON document on stdin and answers with one JSON array
/// on stdout (`barca._inspect`, `barca._value`). Its last stderr line is the error. `cmd` is
/// built by `helper_proc::python_module`, so it carries the resolved storage options.
pub(crate) async fn run_json_helper(
    cmd: tokio::process::Command,
    python: &Path,
    request: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    use crate::helper_proc::JsonFailed;
    crate::helper_proc::run_json(cmd, request, None)
        .await
        .map_err(|failed| match failed {
            JsonFailed::Spawn(e) => format!("{}: {e}", python.display()),
            JsonFailed::Io(e) => e.to_string(),
            JsonFailed::TimedOut(limit) => format!("no answer within {}s", limit.as_secs()),
            JsonFailed::Exited(stderr) => crate::helper_proc::last_line(&stderr),
            JsonFailed::Invalid(e) => e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(state: &str, run_hash: Option<&str>) -> NodeStatus {
        NodeStatus {
            id: "p.py:a".into(),
            name: "a".into(),
            kind: "asset".into(),
            inputs: vec![],
            partitioned: false,
            cache: CacheStatus {
                state: state.into(),
                reason: "no_record".into(),
                detail: String::new(),
                run_hash: run_hash.map(String::from),
                artifact: None,
                changed: None,
                change_causes: None,
            },
            partitions: None,
            last_materialization: None,
            last_bytes_change: None,
            shape: None,
            env: vec![],
            undeclared_env: vec![],
            local: Some(false),
            schedule: None,
        }
    }

    #[test]
    fn a_pending_post_is_unsynced_not_in_ledger_unless_the_ledger_has_it() {
        let pending = vec![PendingPost {
            node_id: "p.py:a".into(),
            run_hash: "h1".into(),
            error: None,
        }];
        let mut n = node("never_run", Some("h1"));
        assert!(not_in_ledger(&mut n, &pending));
        assert_eq!(
            (n.cache.state.as_str(), n.cache.reason.as_str()),
            ("unsynced", "not_in_ledger")
        );
        assert_eq!(n.local, Some(true));
        assert_eq!(StatusSummary::of(std::slice::from_ref(&n)).unsynced, 1);
        // The ledger already has a result, or the server's code hashes differently.
        assert!(!not_in_ledger(&mut node("cached", Some("h1")), &pending));
        assert!(!not_in_ledger(&mut node("stale", Some("h2")), &pending));
        assert!(!not_in_ledger(&mut node("never_run", Some("h1")), &[]));
    }

    fn report(action: &str, reason: Option<&str>) -> StepReport {
        StepReport {
            id: "p.py:a".into(),
            kind: "asset".into(),
            action: Some(action.into()),
            reason: reason.map(String::from),
            run_hash: Some("h1".into()),
            ..Default::default()
        }
    }

    fn history(latest_status: Option<&str>, ever: bool) -> db::NodeHistory {
        db::NodeHistory {
            last_bytes_change: None,
            latest: latest_status.map(|s| db::MaterializationRecord {
                content_hash: None,
                node_id: "p.py:a".into(),
                run_hash: Some("h1".into()),
                artifact_path: None,
                artifact_format: None,
                artifact_size_bytes: None,
                elapsed_seconds: None,
                status: s.into(),
                error_message: Some("boom".into()),
                created_at: String::new(),
            }),
            ever_succeeded: ever,
        }
    }

    #[test]
    fn states_and_reasons() {
        let none = HashMap::new();
        let h = history(None, false);
        let c = cache_status(
            Some(&report("run", Some("not_materialized"))),
            &[],
            &none,
            &h,
        );
        assert_eq!(
            (c.state.as_str(), c.reason.as_str()),
            ("never_run", "no_record")
        );

        let h = history(Some("success"), true);
        let c = cache_status(
            Some(&report("run", Some("not_materialized"))),
            &[],
            &none,
            &h,
        );
        assert_eq!((c.state.as_str(), c.reason.as_str()), ("stale", "changed"));

        // The split by cause, for a stale node and for a failed one (#742).
        let mut changed = report("run", Some("not_materialized"));
        changed.changed = Some(vec!["input p.py:b".into()]);
        changed.run_hash = None;
        let c = cache_status(Some(&changed), &[], &none, &h);
        assert_eq!(c.change_causes.unwrap(), ["inputs_changed"]);
        let c = cache_status(
            Some(&changed),
            &["p.py:b".to_string()],
            &HashMap::from([("p.py:b".to_string(), "stale".to_string())]),
            &h,
        );
        assert_eq!(c.reason, "upstream_stale");
        assert_eq!(
            c.change_causes.unwrap(),
            ["inputs_changed", "upstream_stale"]
        );
        let mut failed = history(Some("failed"), true);
        failed.latest.as_mut().unwrap().error_message = Some("boom".into());
        let c = cache_status(Some(&changed), &[], &none, &failed);
        assert_eq!(c.reason, "failed");
        assert_eq!(c.change_causes.unwrap(), ["inputs_changed"]);
        // A node that never succeeded has nothing to compare with.
        assert!(c.detail.contains("boom"));

        // Its result is recorded and its run hash is unchanged, but the artifact is gone and a
        // run needs it: that is not "changed" (#252).
        let mut missing = report("run", Some("artifact_missing"));
        missing.detail = Some("the artifact file is missing".into());
        let c = cache_status(Some(&missing), &[], &none, &h);
        assert_eq!(
            (c.state.as_str(), c.reason.as_str(), c.detail.as_str()),
            ("stale", "artifact_missing", "the artifact file is missing")
        );

        let up: HashMap<String, String> = [("p.py:u".to_string(), "stale".to_string())].into();
        let c = cache_status(
            Some(&report("run", Some("not_materialized"))),
            &["p.py:u".to_string()],
            &up,
            &h,
        );
        assert_eq!(
            (c.state.as_str(), c.reason.as_str()),
            ("stale", "upstream_stale")
        );

        let h = history(Some("failed"), false);
        let c = cache_status(
            Some(&report("run", Some("not_materialized"))),
            &[],
            &none,
            &h,
        );
        assert_eq!(
            (c.state.as_str(), c.reason.as_str()),
            ("never_run", "failed")
        );
        assert!(c.detail.contains("boom"));

        let c = cache_status(Some(&report("run", Some("task"))), &[], &none, &h);
        assert_eq!(c.state, "always_runs");
        let c = cache_status(Some(&report("cached", None)), &[], &none, &h);
        assert_eq!(c.state, "cached");
    }

    #[test]
    fn copies_here_in_the_store_and_on_record() {
        let there = |size: u64, sha: Option<&str>| StoreObject {
            exists: true,
            size_bytes: Some(size),
            sha256: sha.map(str::to_string),
        };
        let gone = StoreObject::default();
        assert_eq!(classify(None, &gone, Some("d")), Copies::Missing);
        assert_eq!(
            classify(None, &there(3, None), Some("d")),
            Copies::StoreOnly
        );
        assert_eq!(
            classify(Some((3, "d")), &gone, Some("d")),
            Copies::LocalOnly
        );
        // Agree: same size (an object store has no checksum to compare) or same digest.
        assert_eq!(
            classify(Some((3, "d")), &there(3, None), Some("d")),
            Copies::InSync
        );
        assert_eq!(
            classify(Some((3, "d")), &there(3, Some("d")), None),
            Copies::InSync
        );
        // Differ: by size, by digest where the store gives one, or this copy is not the
        // recorded one.
        for (local, store, recorded) in [
            ((3, "d"), there(4, None), Some("d")),
            ((3, "d"), there(3, Some("e")), Some("d")),
            ((3, "e"), there(3, None), Some("d")),
        ] {
            assert!(
                matches!(classify(Some(local), &store, recorded), Copies::Mismatch(_)),
                "{local:?} {store:?} {recorded:?}"
            );
        }
    }

    #[test]
    fn partition_suffix() {
        assert_eq!(partition_of("p.py:x[k=a]").as_deref(), Some("k=a"));
        assert_eq!(partition_of("p.py:x"), None);
    }

    fn failed(mut n: NodeStatus, error: &str) -> NodeStatus {
        n.last_materialization = Some(LastMaterialization {
            content_hash: None,
            status: "failed".into(),
            created_at: "2026-10-22 06:00:00".into(),
            elapsed_seconds: None,
            run_hash: None,
            artifact: None,
            format: None,
            size_bytes: None,
            partition: None,
            error: Some(error.into()),
        });
        n
    }

    #[test]
    fn problems_are_every_state_but_cached_and_always_runs() {
        for state in STATES {
            let fine = matches!(*state, "cached" | "always_runs");
            assert_eq!(needs_attention(&node(state, None)), !fine, "{state}");
        }
    }

    #[test]
    fn a_task_whose_last_run_failed_and_a_paused_or_failing_schedule_are_problems() {
        assert!(needs_attention(&failed(node("always_runs", None), "boom")));
        let mut scheduled = node("cached", None);
        scheduled.schedule = Some(ScheduleStatus {
            cron: "0 6 * * *".into(),
            max_failures: 5,
            failures: 2,
            ..Default::default()
        });
        assert!(!needs_attention(&scheduled), "failures below max_failures");
        scheduled.schedule.as_mut().unwrap().paused = true;
        assert!(needs_attention(&scheduled));
        let schedule = scheduled.schedule.as_mut().unwrap();
        schedule.paused = false;
        schedule.failing = true;
        assert!(needs_attention(&scheduled));
    }

    #[test]
    fn a_filter_keeps_matching_nodes_and_leaves_the_summary_alone() {
        let nodes = vec![
            node("cached", None),
            node("stale", None),
            node("never_run", None),
        ];
        let mut result = StatusResult {
            disk_usage: Default::default(),
            target: None,
            targets: vec![],
            summary: StatusSummary::of(&nodes),
            nodes,
            filter: None,
        };
        let summary = result.summary.clone();
        StatusFilter {
            states: StatusFilter::parse_states(&["never-run,stale".into()]).unwrap(),
            ..Default::default()
        }
        .apply(&mut result);
        assert_eq!(result.nodes.len(), 2);
        assert_eq!(result.summary, summary);
        let filter = result.filter.as_ref().unwrap();
        assert_eq!(filter.states, ["never_run", "stale"]);
        assert_eq!(filter.matched, 2);
        assert_eq!(filter.describe(), "--state never_run,stale");
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(
            json["filter"],
            serde_json::json!({"states": ["never_run", "stale"]})
        );
    }

    #[test]
    fn an_unknown_state_names_the_valid_ones_and_failed_points_at_problems() {
        let err = StatusFilter::parse_states(&["bogus".into()]).unwrap_err();
        assert!(
            err.contains("unknown state 'bogus'\nUse one of cached,"),
            "{err}"
        );
        let err = StatusFilter::parse_states(&["failed".into()]).unwrap_err();
        assert!(err.contains("--problems"), "{err}");
    }

    #[test]
    fn short_error_takes_the_exception_line() {
        assert_eq!(
            short_error(
                "ZeroDivisionError: division by zero\n  File \"p.py\", line 3",
                80
            ),
            "ZeroDivisionError: division by zero"
        );
        assert_eq!(
            short_error(
                "Traceback (most recent call last):\n  File \"p.py\"\nValueError: bad",
                80
            ),
            "ValueError: bad"
        );
        assert_eq!(short_error(&"x".repeat(100), 10), "xxxxxxx...");
    }

    #[test]
    fn schedule_cells() {
        let mut s = ScheduleStatus {
            cron: "*/5 * * * *".into(),
            next_fire: Some("2026-10-23T06:00:00Z".into()),
            max_failures: 3,
            ..Default::default()
        };
        assert_eq!(s.cell(), "*/5 * * * *; next 2026-10-23 06:00:00 UTC");
        s.failures = 1;
        assert_eq!(
            s.cell(),
            "*/5 * * * *; 1 failure, next 2026-10-23 06:00:00 UTC"
        );
        s.paused = true;
        s.paused_until = Some("2026-10-24T00:00:00Z".into());
        assert_eq!(
            s.cell(),
            "*/5 * * * *; paused until 2026-10-24 00:00:00 UTC"
        );
        s.failures = 3;
        s.failing = true;
        assert_eq!(s.cell(), "*/5 * * * *; failing 3/3");
    }

    #[test]
    fn the_table_shows_the_error_the_schedule_and_the_filter() {
        let mut task = failed(
            node("always_runs", None),
            "RuntimeError: 503\n  File \"p.py\"",
        );
        task.name = "sync".into();
        task.kind = "task".into();
        task.cache.reason = "task".into();
        task.schedule = Some(ScheduleStatus {
            cron: "* * * * *".into(),
            failures: 2,
            max_failures: 2,
            failing: true,
            ..Default::default()
        });
        let mut asset = failed(
            node("never_run", None),
            "ZeroDivisionError: division by zero",
        );
        asset.cache.reason = "failed".into();
        let nodes = vec![task, asset, node("cached", None)];
        let mut result = StatusResult {
            disk_usage: Default::default(),
            target: None,
            targets: vec![],
            summary: StatusSummary::of(&nodes),
            nodes,
            filter: None,
        };
        StatusFilter {
            problems: true,
            ..Default::default()
        }
        .apply(&mut result);
        let table = crate::report::render_status_table(&result);
        assert!(table.contains("SCHEDULE"), "{table}");
        assert!(
            table.contains("last run failed: RuntimeError: 503"),
            "{table}"
        );
        assert!(table.contains("* * * * *; failing 2/2"), "{table}");
        assert!(
            table.contains("failed: ZeroDivisionError: division by zero"),
            "{table}"
        );
        assert!(
            table.contains("2 of 3 nodes match --problems; the counts below are for every node"),
            "{table}"
        );
        assert!(table.contains("1 cached, 0 stale, 1 never run"), "{table}");

        result.nodes.clear();
        let table = crate::report::render_status_table(&result);
        assert!(table.contains("No node matches --problems."), "{table}");
    }
}
