//! `barca status` — one read-only view of every node: its definition, cache state, last
//! materialization and artifact shape.
//!
//! Nothing here is new logic. Cache state comes from the same decision `--dry-run` makes
//! ([`crate::execution::explain_dag`]), history from the metadata DB, and artifact shape from
//! `python -m barca._inspect`, which opens artifact files only: user code is never imported.
//! Nothing is written, and no `.barca` directory is created.

use crate::BarcaError;
use crate::cache::CachePolicy;
use crate::db;
use crate::results::StepReport;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResult {
    /// The target name, when status was scoped to one node's upstream cone (`null` for the
    /// whole DAG or for several targets).
    pub target: Option<String>,
    /// Every target the status was scoped to, in the order given (empty for the whole DAG).
    #[serde(default)]
    pub targets: Vec<String>,
    /// Every node in scope, in dependency order.
    pub nodes: Vec<NodeStatus>,
    /// Node counts per cache state.
    pub summary: StatusSummary,
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
    /// With a configured server (`barca status` answered by the server's ledger): whether this
    /// machine has a copy of the node's result (RFC-0007 section 4.1). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub local: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheStatus {
    /// `cached`, `unsynced`, `stale`, `never_run`, `partial`, `unknown` or `always_runs`.
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"cached\" | \"unsynced\" | \"stale\" | \"never_run\" | \"partial\" | \"unknown\" | \"always_runs\""
        )
    )]
    pub state: String,
    /// Machine-readable reason: `materialized`, `local_only`, `store_only`, `mismatch`,
    /// `not_in_ledger` (the CLI with a configured server; the server never answers it), `changed`,
    /// `upstream_stale`, `failed`, `artifact_missing`, `evicted`, `no_record`, `partitions_missing`,
    /// `partitions_unknown`, `sensor_output_unknown`, `task` or `sensor`.
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"materialized\" | \"local_only\" | \"store_only\" | \"mismatch\" | \"not_in_ledger\" | \"changed\" | \"upstream_stale\" | \"failed\" | \"artifact_missing\" | \"evicted\" | \"no_record\" | \"partitions_missing\" | \"partitions_unknown\" | \"sensor_output_unknown\" | \"task\" | \"sensor\""
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionState {
    pub total: usize,
    pub cached: usize,
    pub missing: usize,
    /// Keys without a cached result, capped at 20.
    pub missing_keys: Vec<String>,
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
    let mine = pending.iter().find(|p| {
        let base = p.node_id.split('[').next().unwrap_or(&p.node_id);
        base == node.id
            && (node.partitioned
                || node.cache.run_hash.is_none()
                || node.cache.run_hash.as_deref() == Some(p.run_hash.as_str()))
    });
    let Some(p) = mine else {
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
    Ok(
        status_with_membership(cfg, target_names, file_args, python, sample, shape)
            .await?
            .0,
    )
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
    let dag = crate::load::build_dag(file_args, python).await?;
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
async fn status_from_dag_with_membership(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    dag: &crate::dag::Dag,
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<(StatusResult, crate::cache::ExpandedMembership), BarcaError> {
    let (explained, membership) = crate::execution::explain_dag_with_membership(
        dag,
        cfg,
        target_names,
        CachePolicy::CacheAware,
        "status",
        None,
    )
    .await?;
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

    // Results computed here whose upload to the store is still pending (#390).
    let pending = if cfg.remote_artifacts() {
        crate::pending_uploads::list(&cfg.db_path).await?
    } else {
        Vec::new()
    };

    let mut nodes: Vec<NodeStatus> = Vec::new();
    let mut states: HashMap<String, String> = HashMap::new();
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
            });
        let mut cache = cache_status(report, &inputs, &states, &history);
        local_only(
            &mut cache,
            id,
            !node.extracted.partitions.is_empty(),
            &pending,
        );
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
            last_bytes_change: history.last_bytes_change,
            shape: None,
            env: node.extracted.env.clone(),
            local: None,
        });
    }

    // With a store, `cached` means this machine's copy, the store's object and the recorded
    // digest all agree (RFC-0009); anything else is `unsynced` with its reason.
    if cfg.remote_artifacts() && Path::new(&cfg.db_path).exists() {
        check_store_copies(python, cfg, &mut nodes).await;
    }

    if shape {
        read_shapes(python, cfg, &mut nodes, sample).await;
    }

    let summary = StatusSummary::of(&nodes);
    Ok((
        StatusResult {
            targets: explained.target_names(),
            target: explained.target,
            nodes,
            summary,
        },
        membership,
    ))
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
                    "this machine's copy is not the one recorded; get fetches the store's copy"
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
) {
    let layout = crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
    let Ok(cache) = db::CacheReader::for_config(cfg).await else {
        return;
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
        return;
    }
    let paths: Vec<&str> = wanted.iter().map(|(_, p, _, _)| p.as_str()).collect();
    let objects = match stat_objects(python, cfg, &paths).await {
        Ok(objects) if objects.len() == wanted.len() => objects,
        Ok(_) => return,
        Err(why) => {
            for (i, ..) in &wanted {
                let c = &mut nodes[*i].cache;
                c.detail = format!(
                    "{} (the artifact store could not be checked: {why})",
                    c.detail
                );
            }
            return;
        }
    };
    for ((i, store, local, recorded), object) in wanted.into_iter().zip(objects) {
        let here = std::fs::metadata(&local)
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| db::file_sha256(&local).ok().map(|sha| (m.len(), sha)));
        let copies = classify(
            here.as_ref().map(|(size, sha)| (*size, sha.as_str())),
            &object,
            recorded.as_deref(),
        );
        let at = crate::transfer::diagnostic_uri(&store);
        let c = &mut nodes[i].cache;
        let (state, reason, detail) = match copies {
            Copies::InSync => continue,
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
}

/// Ask the store about each of `paths` with one `python -m barca._state stat`.
pub(crate) async fn stat_objects(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    paths: &[&str],
) -> Result<Vec<StoreObject>, String> {
    use tokio::io::AsyncWriteExt;
    #[derive(Deserialize)]
    struct Reply {
        objects: Vec<StoreObject>,
    }
    let mut cmd = crate::backup::state_helper(python, cfg);
    cmd.arg("stat")
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child =
        crate::helper_proc::spawn(&mut cmd).map_err(|e| format!("{}: {e}", python.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let request = serde_json::json!({ "paths": paths }).to_string();
    stdin
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(60));
    let out = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("no answer within {}s", limit.as_secs()))?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(crate::helper_proc::helper_cause(&out.stderr));
    }
    serde_json::from_slice::<Reply>(&out.stdout)
        .map(|r| r.objects)
        .map_err(|e| e.to_string())
}

fn cache(state: &str, reason: &str, detail: impl Into<String>) -> CacheStatus {
    CacheStatus {
        state: state.to_string(),
        reason: reason.to_string(),
        detail: detail.into(),
        run_hash: None,
        artifact: None,
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
        states
            .get(base)
            .is_some_and(|s| matches!(s.as_str(), "stale" | "never_run" | "partial" | "unknown"))
    });
    // The last attempt at exactly this code and these inputs failed.
    let failed_here = history
        .latest
        .as_ref()
        .filter(|m| m.status == "failed" && (r.run_hash.is_none() || m.run_hash == r.run_hash));
    if let Some(m) = failed_here {
        let msg = m.error_message.as_deref().unwrap_or("no message");
        return cache(state, "failed", format!("last attempt failed: {msg}"));
    }
    if !history.ever_succeeded {
        return cache(state, "no_record", "no successful materialization recorded");
    }
    if let Some(up) = upstream_stale {
        return cache(
            state,
            "upstream_stale",
            format!(
                "upstream '{}' is not cached, so this node's inputs will change",
                short_name(up)
            ),
        );
    }
    cache(
        state,
        "changed",
        "code or upstream outputs changed since the last materialization (the run hash differs)",
    )
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
) {
    read_shapes_inner(python, cfg, nodes, sample, false).await;
}

/// Inspect artifact schemas, including JSON object field and list element types.
/// This richer on-demand view leaves the CLI's existing shape contract unchanged.
pub async fn read_schemas(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
) {
    read_shapes_inner(python, cfg, nodes, 0, true).await;
}

async fn read_shapes_inner(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
    sample: usize,
    fields: bool,
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
    mut cmd: tokio::process::Command,
    python: &Path,
    request: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    use tokio::io::AsyncWriteExt;
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child =
        crate::helper_proc::spawn(&mut cmd).map_err(|e| format!("{}: {e}", python.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin
        .write_all(request.to_string().as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err
            .lines()
            .last()
            .unwrap_or("exited with an error")
            .to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
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
            },
            partitions: None,
            last_materialization: None,
            last_bytes_change: None,
            shape: None,
            env: vec![],
            local: Some(false),
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
}
