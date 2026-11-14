//! What a command returns: the typed results of `get`, `run`, `--dry-run`, `plan` and `list`.
//!
//! These are data only. Each derives serde so the CLI, the server and their tests serialize the
//! same shape; how a result is printed is the caller's job.

use crate::dispatch::OutputRef;
use crate::warnings::PlanWarning;
use serde::{Deserialize, Serialize};

pub use crate::reports::{
    BatchExecution, BatchPlan, ExplainResult, ExplainSummary, KeySummary, PartitionSelectionReport,
    PartitionSummary, SelectedNode, StepReport, TargetOutcome, TargetPrediction, UploadsReport,
};
pub(crate) use crate::reports::{LISTED_KEYS, deserialize_targets};

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
    pub cache: Option<crate::gc::CacheReport>,
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
    pub cache: Option<crate::gc::CacheReport>,
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
