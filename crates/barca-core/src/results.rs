//! What a command returns: the typed results of `get`, `run`, `--dry-run`, `plan` and `list`.
//!
//! These are data only. Each derives serde so the CLI, the server and their tests serialize the
//! same shape; how a result is printed is the caller's job.

use crate::step_result::OutputRef;
use crate::warnings::PlanWarning;
use serde::{Deserialize, Serialize};

pub use crate::reports::{
    BatchExecution, BatchPlan, ExplainResult, ExplainSummary, KeySummary, PartitionSelectionReport,
    PartitionSummary, SelectedNode, StepReport, TargetOutcome, TargetPrediction, UploadsReport,
};
pub(crate) use crate::reports::{LISTED_KEYS, deserialize_targets};

/// The key of a summarized partitioned `final_output` (#623).
pub(crate) const KEY_SUMMARY: &str = "_barca_partitions";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GetResult {
    pub run_id: String,
    pub elapsed_seconds: f64,
    pub steps_executed: usize,
    pub phases: usize,
    pub final_output: Option<OutputRef>,
    /// What happened to each planned step in this run (ran / cached / partial, and why).
    #[serde(default)]
    pub steps: Vec<StepReport>,
    /// Plan-time warnings for the steps this run planned (`[]` when there are none).
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
    /// When the returned node is partitioned: each returned key (`dim=value,...`, sorted) and
    /// its artifact. Keys that failed or were not selected are absent. `final_output` is then
    /// the first of them, for callers that read one output (the HTTP API).
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub partitions: Option<Vec<(String, OutputRef)>>,
    /// Set when `partitions` holds more than [`LISTED_KEYS`] keys and no `--partition` selected
    /// them: `final_output` then summarizes them ([`KeySummary`]).
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub key_summary: Option<KeySummary>,
    /// Present when the check after the run found `.barca/` over its size limit (RFC-0012):
    /// what it evicted, and the size after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheReport>,
    /// Results computed here and not yet in the artifact store (#390). Absent when nothing is
    /// pending and nothing failed to publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub uploads: Option<UploadsReport>,
    /// Coordinator seconds spent recording results (handling each finished step and writing
    /// the metadata DB), when the run made a batched call. Absent otherwise. Experimental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub metadata_seconds: Option<f64>,
    /// `--partition`: the selection, each narrowed node's selected and skipped key counts, and
    /// the nodes the selection did not run. Absent without `--partition`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub partition_selection: Option<PartitionSelectionReport>,
}

/// The result of `barca get|run a,b` (several targets): one run over the union of the targets'
/// cones, with each target's outcome. A failed target does not stop the others.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiResult {
    pub run_id: String,
    pub elapsed_seconds: f64,
    pub steps_executed: usize,
    pub phases: usize,
    /// What happened to each planned step (shared upstream steps appear once).
    pub steps: Vec<StepReport>,
    /// Plan-time warnings for the steps this run planned (`[]` when there are none).
    pub warnings: Vec<PlanWarning>,
    /// Each target by the name it was given, in the order given (serialized as a map).
    #[serde(
        serialize_with = "serialize_targets",
        deserialize_with = "deserialize_targets"
    )]
    pub targets: Vec<(String, TargetOutcome)>,
    /// Present when the check after the run found `.barca/` over its size limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheReport>,
    /// See [`GetResult::uploads`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploads: Option<UploadsReport>,
    /// See [`GetResult::metadata_seconds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_seconds: Option<f64>,
    /// See [`GetResult::partition_selection`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_selection: Option<PartitionSelectionReport>,
}

fn serialize_targets<S: serde::Serializer>(
    targets: &[(String, TargetOutcome)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(targets.iter().map(|(k, v)| (k, v)))
}

impl MultiResult {
    /// True when any target failed.
    pub(crate) fn any_failed(&self) -> bool {
        self.targets.iter().any(|(_, t)| t.failed())
    }
}

pub(crate) const PLAN_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanResult {
    /// Version of this plan document, independent of the package/worker/DB versions.
    pub plan_version: u32,
    pub total_steps: usize,
    /// Worker processes a run starts at most: `BARCA_POOL_SIZE`, `[execution] pool_size`, or
    /// the cores, bounded by the capacity when one is set (#628).
    #[serde(default)]
    pub pool_size: usize,
    /// Where `pool_size` came from: `BARCA_POOL_SIZE`, `[execution] pool_size`, `cores`, or the
    /// capacity setting that bounded the cores (`[execution] capacity`, `BARCA_CAPACITY`,
    /// `[server] capacity`, `--capacity`).
    #[serde(default)]
    pub pool_source: String,
    /// The most the `weight=` of the steps running at once may add up to (#436).
    #[serde(default)]
    pub capacity: usize,
    /// Where `capacity` came from: `BARCA_CAPACITY`, `[execution] capacity` or `pool size`.
    #[serde(default)]
    pub capacity_source: String,
    /// The planned nodes whose `weight=` is above 1, by node id (`{}` when there are none).
    #[serde(default)]
    pub weights: std::collections::BTreeMap<String, u32>,
    pub phases: Vec<PlanPhase>,
    /// Plan-time warnings for the planned steps (`[]` when there are none).
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
    /// See [`GetResult::partition_selection`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub partition_selection: Option<PartitionSelectionReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanPhase {
    /// Why this phase starts: `{"type": "initial"}`, or `{"type": "fan_in", "node_id": ...}`
    /// when it waits for a node that gathers several upstream results.
    pub reason: PlanPhaseReason,
    pub streams: Vec<PlanStream>,
}

/// [`crate::planner::PhaseReason`] as `barca plan` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum PlanPhaseReason {
    Initial,
    FanIn { node_id: String },
}

impl From<&crate::planner::PhaseReason> for PlanPhaseReason {
    fn from(r: &crate::planner::PhaseReason) -> Self {
        match r {
            crate::planner::PhaseReason::Initial => PlanPhaseReason::Initial,
            crate::planner::PhaseReason::FanIn { node_id } => PlanPhaseReason::FanIn {
                node_id: node_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanStream {
    pub stream_id: String,
    pub steps: Vec<String>,
}

/// Lightweight summary of a single DAG node, for the server's `/assets` listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetSummary {
    /// Stable node id (continuity key), e.g. `pipeline.py:fetch`.
    pub id: String,
    /// Node kind: asset, sensor, or task.
    pub kind: crate::NodeKind,
    /// Freshness policy (always / manual / schedule).
    pub freshness: crate::Freshness,
    /// Upstream node ids this node depends on (direct + collected), sorted.
    pub inputs: Vec<String>,
    /// Declared environment variable names (`@asset(env=[...])`), in declaration order.
    #[serde(default)]
    pub env: Vec<String>,
    /// Source file of the definition, as in the id.
    #[serde(default)]
    pub file: String,
    /// 1-based line of the definition's first decorator.
    #[serde(default)]
    pub line: usize,
}

/// The `cache` object of a `get`/`run` result: present when the check after the run found the
/// cache over its limit (with nothing evicted when nothing more could go).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheReport {
    /// Bytes under `.barca/` after eviction.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size_bytes: u64,
    /// The limit, in bytes.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub limit_bytes: u64,
    /// Files (and leftover directories) deleted.
    pub evicted_files: usize,
    /// Bytes deleted.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub evicted_bytes: u64,
}

/// Describe a previously validated graph without reading/importing source again.
pub fn list_assets_from_dag(dag: &crate::dag::Dag) -> Vec<AssetSummary> {
    dag.topo_order()
        .into_iter()
        .filter_map(|id| dag.get_node(id))
        .map(|node| {
            let mut inputs: Vec<String> = node
                .resolved_inputs
                .values()
                .chain(node.resolved_collected.values())
                .cloned()
                .collect();
            inputs.sort();
            inputs.dedup();
            AssetSummary {
                id: node.id.clone(),
                kind: node.kind(),
                freshness: node.extracted.freshness.clone(),
                inputs,
                env: node.extracted.env.clone(),
                file: node.extracted.source_file.clone(),
                line: node.extracted.line,
            }
        })
        .collect()
}

/// Select the first twenty keys in lexical order, without retaining every key.
/// Taking a preview of each chunk and then previewing their union has the same result
/// as previewing all keys, independently of worker chunk boundaries.
pub(crate) fn key_preview(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut preview = Vec::new();
    for key in keys {
        let pos = preview.binary_search(&key).unwrap_or_else(|pos| pos);
        if pos < 20 {
            preview.insert(pos, key);
            preview.truncate(20);
        }
    }
    preview
}

/// A `barca sql` query's result: column names in order, one object per row, and how many rows the query
/// returns in all (`truncated` when only the first `limit` are in `rows`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlResult {
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub total: u64,
    pub truncated: bool,
    /// Notes for stderr (stale views, renamed views); not part of the JSON result.
    #[serde(skip)]
    pub notes: Vec<String>,
}

// ─── the JSON form of a result's output ─────────────────────────────────────────

/// The pointer a result gives for an artifact instead of its value.
pub fn artifact_metadata(oref: &OutputRef) -> serde_json::Value {
    serde_json::json!({
        "_barca_artifact": {
            "path": oref.path,
            "format": oref.format,
            "size_bytes": oref.size_bytes,
        }
    })
}

/// What `barca get --json` returns by default (#404): the artifact pointer, or for a
/// partitioned target an object of pointers keyed by partition key ([`keyed_output`]). `None`
/// when the run returned nothing (a `get` of every asset).
pub fn pointer_output(
    single: Option<&OutputRef>,
    keys: Option<&[(String, OutputRef)]>,
    summary: Option<&KeySummary>,
) -> Option<serde_json::Value> {
    output_with(single, keys, summary, artifact_metadata)
}

/// A result's `final_output` with each listed artifact as `read` gives it: its pointer
/// ([`pointer_output`]) or its value (`values::read_task_output`).
pub(crate) fn output_with(
    single: Option<&OutputRef>,
    keys: Option<&[(String, OutputRef)]>,
    summary: Option<&KeySummary>,
    read: impl Fn(&OutputRef) -> serde_json::Value,
) -> Option<serde_json::Value> {
    match KeySummary::listed(summary, keys) {
        Some(keys) => Some(keyed_output(
            keys.iter()
                .map(|(key, oref)| (key.clone(), read(oref)))
                .collect(),
            summary,
        )),
        None => single.map(read),
    }
}

/// A partitioned target's `final_output`: `entries` (key to pointer or value) as they are, or,
/// with a summary (more than [`LISTED_KEYS`] keys), `{"_barca_partitions":
/// {"keys", "computed", "cached", "outputs", "truncated", "hint"}}`, where `outputs` holds the
/// entries of the keys the run computed.
pub fn keyed_output(
    entries: serde_json::Map<String, serde_json::Value>,
    summary: Option<&KeySummary>,
) -> serde_json::Value {
    let Some(s) = summary else {
        return serde_json::Value::Object(entries);
    };
    let truncated = entries.len() < s.computed;
    serde_json::json!({
        KEY_SUMMARY: {
            "keys": s.keys,
            "computed": s.computed,
            "cached": s.keys - s.computed,
            "outputs": entries,
            "truncated": truncated,
            "hint": key_summary_hint(s),
        }
    })
}

/// Why a partitioned output is summarized, and how to see the keys it leaves out.
fn key_summary_hint(s: &KeySummary) -> String {
    format!(
        "{} keys, more than {}: outputs lists only the keys this run computed{}; select keys \
         with --partition to list their paths, or read every key with barca sql",
        s.keys,
        LISTED_KEYS,
        if s.computed > LISTED_KEYS {
            format!(" (the first {})", LISTED_KEYS)
        } else {
            String::new()
        }
    )
}

/// The counts of a summarized partitioned `final_output` ([`keyed_output`]); `None` for any
/// other value.
pub(crate) fn parse_key_summary(output: &serde_json::Value) -> Option<KeySummary> {
    let s = output.get(KEY_SUMMARY)?;
    let n = |k: &str| {
        s.get(k)
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize)
    };
    Some(KeySummary {
        keys: n("keys")?,
        computed: n("computed")?,
        listed: Vec::new(),
    })
}

/// The error a multi-target run ends with: its first failed target, with the run so far.
pub fn first_multi_failure(result: &MultiResult) -> Option<crate::BarcaError> {
    // The first failed target becomes the run's error: exit 1 with the error envelope.
    if let Some((name, t)) = result.targets.iter().find(|(_, t)| t.failed()) {
        return Some(crate::BarcaError::WorkerFailed(Box::new(
            crate::FailedStep {
                node: t.failed_node.clone().unwrap_or_else(|| name.clone()),
                message: t.error.clone().unwrap_or_else(|| "unknown error".into()),
                phase: t.failed_phase,
                artifact_dir: None,
                run: Some(Box::new(crate::PartialRun {
                    run_id: result.run_id.clone(),
                    elapsed_seconds: result.elapsed_seconds,
                    steps_executed: result.steps_executed,
                    phases: result.phases,
                    steps: result.steps.clone(),
                    warnings: result.warnings.clone(),
                    partitions: t.partitions.clone(),
                    key_summary: t.key_summary.clone(),
                })),
            },
        )));
    }
    None
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use serde_json::json;

    fn roundtrip<T: Serialize + serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        let decoded: T = serde_json::from_value(value.clone()).unwrap();
        let encoded = serde_json::to_string(&decoded).unwrap();
        let restored: T = serde_json::from_str(&encoded).unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), value);
        restored
    }

    #[test]
    fn successful_result_preserves_artifact_steps_and_warnings() {
        let result: GetResult = roundtrip(json!({
            "run_id": "run-1", "elapsed_seconds": 1.25, "steps_executed": 1, "phases": 2,
            "final_output": {"path": ".barca/artifacts/z.json", "format": "json", "size_bytes": 12},
            "steps": [{"id": "p.py:z", "kind": "asset", "status": "ran", "reason": "not_materialized",
                "env": {"TOKEN": "<redacted>", "OPTIONAL": null}}],
            "warnings": [{"kind": "unused_input", "node": "p.py:z", "param": "a", "message": "unused a"}]
        }));
        assert_eq!(result.final_output.unwrap().path, ".barca/artifacts/z.json");
        assert_eq!(result.steps[0].env.as_ref().unwrap()["OPTIONAL"], None);
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn multi_result_keeps_target_order_and_failure_details() {
        let wire = r#"{"run_id":"multi","elapsed_seconds":2.0,"steps_executed":1,"phases":1,"steps":[],"warnings":[],"targets":{"z":{"status":"success","final_output":{"path":"z.json","format":"json","size_bytes":3}},"a":{"status":"failed","error":"ValueError: bad","failed_node":"p.py:a"}}}"#;
        let result: MultiResult = serde_json::from_str(wire).unwrap();
        assert_eq!(
            result
                .targets
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert!(result.any_failed());
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(encoded.find("\"z\":").unwrap() < encoded.find("\"a\":").unwrap());
        let restored: MultiResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.targets[1].1.failed_node.as_deref(), Some("p.py:a"));
        assert_eq!(serde_json::to_string(&restored).unwrap(), encoded);
    }

    #[test]
    fn failed_step_keeps_the_partial_run_and_complete_traceback() {
        let failed: crate::FailedStep = roundtrip(json!({
            "node": "p.py:a", "message": "ValueError: bad\n  File \"p.py\", line 4, in a\n    raise ValueError('bad')",
            "artifact_dir": "s3://bucket/p.py--a", "run": {
                "run_id": "partial", "elapsed_seconds": 0.5, "steps_executed": 2, "phases": 1,
                "steps": [{"id":"p.py:z", "kind":"asset", "status":"ran"}, {"id":"p.py:a", "kind":"task", "status":"failed"}],
                "warnings": []
            }
        }));
        assert_eq!(failed.summary(), "ValueError: bad");
        assert!(failed.traceback().unwrap().contains("line 4"));
        let partial = failed.run.unwrap();
        let restored: crate::PartialRun =
            serde_json::from_str(&serde_json::to_string(&partial).unwrap()).unwrap();
        assert_eq!(restored.steps[0].status.as_deref(), Some("ran"));
        assert_eq!(restored.steps[1].status.as_deref(), Some("failed"));
        assert_eq!(restored.steps_executed, 2);
        let no_run: crate::FailedStep = roundtrip(
            json!({"node":"p.py:a", "message":"SystemExit: 1", "artifact_dir":null, "run":null}),
        );
        assert!(no_run.run.is_none());
    }

    #[test]
    fn explanations_roundtrip_whole_file_single_target_and_ordered_multiple_targets() {
        for target in [None, Some("p.py:z")] {
            let result: ExplainResult = roundtrip(json!({
                "dry_run": true, "command": "get", "target": target, "capacity": 4,
                "steps": [{"id":"p.py:z", "kind":"asset", "action":"run", "reason":"not_materialized"}],
                "summary": {"will_run":1,"cached":0,"unknown":0}, "warnings":[]
            }));
            assert_eq!(result.target.as_deref(), target);
            assert!(result.targets.is_empty());
        }
        let wire = r#"{"dry_run":true,"command":"get","targets":{"z":{"summary":{"will_run":1,"cached":0,"unknown":0}},"a":{"summary":{"will_run":0,"cached":2,"unknown":1}}},"steps":[],"summary":{"will_run":1,"cached":2,"unknown":1},"warnings":[]}"#;
        let result: ExplainResult = serde_json::from_str(wire).unwrap();
        assert_eq!(result.target_names(), ["z", "a"]);
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(encoded.find("\"z\":").unwrap() < encoded.find("\"a\":").unwrap());
        assert!(!encoded.contains("\"target\":"));
        let restored: ExplainResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.targets[1].1.summary.unknown, 1);
        assert_eq!(serde_json::to_string(&restored).unwrap(), encoded);
    }

    #[test]
    fn target_map_rejects_duplicate_names_instead_of_losing_an_outcome() {
        let wire = r#"{"run_id":"multi","elapsed_seconds":0.0,"steps_executed":0,"phases":0,"steps":[],"warnings":[],"targets":{"z":{"status":"success"},"z":{"status":"failed"}}}"#;
        assert!(
            serde_json::from_str::<MultiResult>(wire)
                .unwrap_err()
                .to_string()
                .contains("duplicate target 'z'")
        );
    }
}

#[cfg(test)]
mod key_summary_tests {
    use super::*;

    fn oref(key: &str) -> OutputRef {
        OutputRef {
            path: format!("{key}.json"),
            format: "json".into(),
            size_bytes: 1,
            elapsed_seconds: None,
            content_hash: None,
        }
    }

    #[test]
    fn a_target_of_many_keys_lists_only_the_keys_it_computed() {
        let keys: Vec<(String, OutputRef)> = (0..LISTED_KEYS + 1)
            .map(|i| {
                let k = format!("k={i:05}");
                let o = oref(&k);
                (k, o)
            })
            .collect();
        // Every key but two was served from cache.
        let cached: std::collections::HashSet<String> = keys
            .iter()
            .filter(|(k, _)| k != "k=00007" && k != "k=00900")
            .map(|(k, _)| format!("p.py:t[{k}]"))
            .collect();
        let s = KeySummary::of("p.py:t", &keys, false, &cached).unwrap();
        assert_eq!((s.keys, s.computed), (LISTED_KEYS + 1, 2));
        let listed: Vec<&str> = s.listed.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(listed, ["k=00007", "k=00900"]);
        // At the limit, or selected with --partition, every key is listed.
        assert!(KeySummary::of("p.py:t", &keys[..LISTED_KEYS], false, &cached).is_none());
        assert!(KeySummary::of("p.py:t", &keys, true, &cached).is_none());
        // A cold run computed them all: the first LISTED_KEYS are listed, all are counted.
        let none = std::collections::HashSet::new();
        let cold = KeySummary::of("p.py:t", &keys, false, &none).unwrap();
        assert_eq!(
            (cold.computed, cold.listed.len()),
            (LISTED_KEYS + 1, LISTED_KEYS)
        );
    }
}

#[cfg(test)]
mod multi_result_tests {
    use super::*;

    fn outcome(status: &str) -> TargetOutcome {
        TargetOutcome {
            status: status.to_string(),
            final_output: None,
            error: None,
            failed_node: None,
            failed_phase: None,
            partitions: None,
            key_summary: None,
        }
    }

    #[test]
    fn multi_result_targets_serialize_as_a_map_in_the_order_given() {
        let r = MultiResult {
            run_id: "r".to_string(),
            elapsed_seconds: 0.0,
            steps_executed: 0,
            phases: 0,
            steps: Vec::new(),
            warnings: Vec::new(),
            targets: vec![
                ("zeta".to_string(), outcome("success")),
                ("alpha".to_string(), outcome("failed")),
            ],
            cache: None,
            uploads: None,
            metadata_seconds: None,
            partition_selection: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            s.find("\"zeta\"").unwrap() < s.find("\"alpha\"").unwrap(),
            "{s}"
        );
        assert!(r.any_failed());
    }
}
