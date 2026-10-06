//! `barca status` — one read-only view of every node: its definition, cache state, last
//! materialization and artifact shape.
//!
//! Nothing here is new logic. Cache state comes from the same decision `--dry-run` makes
//! ([`commands::explain_dag`]), history from the metadata DB, and artifact shape from
//! `python -m barca._inspect`, which opens artifact files only: user code is never imported.
//! Nothing is written, and no `.barca` directory is created.

use crate::BarcaError;
use crate::commands::{self, CachePolicy, StepReport};
use crate::db;
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
    /// The function name (what `get`, `run` and `--refresh` accept).
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
    /// Shape of `last_materialization`'s artifact (`null` when there is none).
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub shape: Option<serde_json::Value>,
    /// Environment variables the node declares with `env=[...]` (empty when none); their values
    /// are part of its run hash.
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheStatus {
    /// `cached`, `stale`, `never_run`, `partial`, `unknown` or `always_runs`.
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"cached\" | \"stale\" | \"never_run\" | \"partial\" | \"unknown\" | \"always_runs\""
        )
    )]
    pub state: String,
    /// Machine-readable reason: `materialized`, `changed`, `upstream_stale`, `failed`,
    /// `no_record`, `partitions_missing`, `partitions_unknown`, `sensor_output_unknown`, `task`
    /// or `sensor`.
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "\"materialized\" | \"changed\" | \"upstream_stale\" | \"failed\" | \"no_record\" | \"partitions_missing\" | \"partitions_unknown\" | \"sensor_output_unknown\" | \"task\" | \"sensor\""
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
    let dag = commands::build_dag(file_args, python).await?;
    let explained = commands::explain_dag(
        &dag,
        cfg,
        target_names,
        python,
        CachePolicy::CacheAware,
        false,
        "status",
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
        let owned: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        db::node_histories(&cfg.db_path, &owned).await?
    } else {
        HashMap::new()
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
        let cache = cache_status(report, &inputs, &states, &history);
        states.insert(id.to_string(), cache.state.clone());

        let last_materialization = history.latest.map(|m| LastMaterialization {
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
            name: node.function_name().to_string(),
            kind: serde_json::to_value(node.kind())
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "unknown".to_string()),
            inputs,
            partitioned: !node.extracted.partitions.is_empty(),
            cache,
            partitions: partitions.filter(|_| !node.extracted.partitions.is_empty()),
            last_materialization,
            shape: None,
            env: node.extracted.env.clone(),
        });
    }

    if shape {
        read_shapes(python, cfg, &mut nodes, sample).await;
    }

    let mut summary = StatusSummary::default();
    for n in &nodes {
        match n.cache.state.as_str() {
            "cached" => summary.cached += 1,
            "stale" => summary.stale += 1,
            "never_run" => summary.never_run += 1,
            "partial" => summary.partial += 1,
            "always_runs" => summary.always_runs += 1,
            _ => summary.unknown += 1,
        }
    }
    Ok(StatusResult {
        targets: explained.target_names(),
        target: explained.target,
        nodes,
        summary,
    })
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
    let wanted: Vec<(usize, String, String)> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let m = n.last_materialization.as_ref()?;
            if m.status != "success" {
                return None;
            }
            Some((i, m.artifact.clone()?, m.format.clone().unwrap_or_default()))
        })
        .collect();
    if wanted.is_empty() {
        return;
    }
    let request = serde_json::json!({
        "sample": sample,
        "artifacts": wanted
            .iter()
            .map(|(_, path, format)| serde_json::json!({"path": path, "format": format}))
            .collect::<Vec<_>>(),
    });
    let shapes = match run_inspector(python, cfg, &request).await {
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

async fn run_inspector(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    request: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(python);
    cmd.args(["-m", "barca._inspect"]);
    // The same options workers get (`[remote.storage_options.*]` merged with the environment).
    if let Some(ref opts) = cfg.storage_options_json {
        cmd.env("BARCA_STORAGE_OPTIONS", opts);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", python.display()))?;
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
            latest: latest_status.map(|s| db::MaterializationRecord {
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
    fn partition_suffix() {
        assert_eq!(partition_of("p.py:x[k=a]").as_deref(), Some("k=a"));
        assert_eq!(partition_of("p.py:x"), None);
    }
}
