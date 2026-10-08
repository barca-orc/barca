//! What a command returns: the typed results of `get`, `run`, `--dry-run`, `plan` and `list`.
//!
//! These are data only. Each derives serde so the CLI, the server and their tests serialize the
//! same shape; how a result is printed is the caller's job.

use crate::dispatch::OutputRef;
use crate::warnings::PlanWarning;
use serde::{Deserialize, Serialize};

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
}

/// The result of `barca get|run a,b` (several targets): one run over the union of the targets'
/// cones, with each target's outcome. A failed target does not stop the others.
#[derive(Debug, Clone, Serialize)]
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
    #[serde(serialize_with = "serialize_targets")]
    pub targets: Vec<(String, TargetOutcome)>,
}

fn serialize_targets<S: serde::Serializer>(
    targets: &[(String, TargetOutcome)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(targets.iter().map(|(k, v)| (k, v)))
}

impl MultiResult {
    /// True when any target failed.
    pub fn any_failed(&self) -> bool {
        self.targets.iter().any(|(_, t)| t.status != "success")
    }
}

/// How one target of a multi-target run ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    /// `success` or `failed`.
    pub status: String,
    /// The target's output, when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_output: Option<OutputRef>,
    /// The error of the step that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The step that failed: the target itself, or a step upstream of it (`failed_node`, the
    /// same key a failed single-target run uses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_node: Option<String>,
}

/// How a step was (or, in a dry run, will be) treated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StepReport {
    pub id: String,
    /// `asset`, `task` or `sensor`.
    pub kind: String,
    /// Dry run only: `cached`, `run`, `partial` (some partition keys cached) or `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Real run only: `ran`, `cached`, `partial`, or `failed` (in a failed run's result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why the step runs: `task`, `sensor`, `refresh`, `refresh_cascade`, `refresh_all`,
    /// `not_materialized`, `artifact_missing`, `partitions_unknown` or `sensor_output_unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The reason in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_hash: Option<String>,
    /// The cached artifact, when the step is served from cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// Something to know about the step, in words: it was served from cache although an asset
    /// it depends on was refreshed in the same run, or `artifact_mismatch` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// `true` when the artifact store's copy of this step's result, or of an input the step
    /// read in this run, does not have the hash recorded for it. The store's copy was used,
    /// and `warning` says which and how to recompute it. Absent otherwise (never `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_mismatch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<PartitionSummary>,
    /// Declared env values the step used (`@asset(env=[...])`): name -> value, `null` when unset,
    /// `"<redacted>"` for secret-looking names. Absent when the node declares no env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionSummary {
    pub total: usize,
    pub cached: usize,
    pub will_run: usize,
    /// The keys that will (or did) run, capped at 20.
    pub will_run_keys: Vec<String>,
}

/// What `--dry-run` reports: the same decisions a real run would make, without making them.
///
/// Serialized with `target` (one target, or null for the whole file) or, when several targets
/// were given, `targets` in its place: an object keyed by target name in the order given, like a
/// real multi-target run, each `{"summary": {...}}` counted over that target's cone.
#[derive(Debug, Clone)]
pub struct ExplainResult {
    pub dry_run: bool,
    pub command: String,
    pub target: Option<String>,
    /// Every target with its predicted summary when more than one was given; empty otherwise.
    pub targets: Vec<(String, TargetPrediction)>,
    pub steps: Vec<StepReport>,
    pub summary: ExplainSummary,
    /// Plan-time warnings for the steps the command would plan: the list the real run reports.
    pub warnings: Vec<PlanWarning>,
}

/// One target of a multi-target dry run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetPrediction {
    /// The dry-run summary counted over this target's cone only (shared upstream steps count
    /// for every target that needs them).
    pub summary: ExplainSummary,
}

impl ExplainResult {
    /// The target names in scope: the several given, the one given, or none (whole file).
    pub fn target_names(&self) -> Vec<String> {
        if self.targets.is_empty() {
            self.target.iter().cloned().collect()
        } else {
            self.targets.iter().map(|(n, _)| n.clone()).collect()
        }
    }
}

impl Serialize for ExplainResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        struct Targets<'a>(&'a [(String, TargetPrediction)]);
        impl Serialize for Targets<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_map(self.0.iter().map(|(k, v)| (k, v)))
            }
        }
        let mut s = serializer.serialize_struct("ExplainResult", 6)?;
        s.serialize_field("dry_run", &self.dry_run)?;
        s.serialize_field("command", &self.command)?;
        if self.targets.len() > 1 {
            s.serialize_field("targets", &Targets(&self.targets))?;
        } else {
            s.serialize_field("target", &self.target)?;
        }
        s.serialize_field("steps", &self.steps)?;
        s.serialize_field("summary", &self.summary)?;
        s.serialize_field("warnings", &self.warnings)?;
        s.end()
    }
}

impl ExplainSummary {
    /// Add one (merged) dry-run step line: a partitioned line counts its keys.
    pub(crate) fn add(&mut self, r: &StepReport) {
        match (r.action.as_deref(), &r.partitions) {
            (Some("unknown"), _) => self.unknown += 1,
            (_, Some(p)) => {
                self.cached += p.cached;
                self.will_run += p.will_run;
            }
            (Some("cached"), None) => self.cached += 1,
            _ => self.will_run += 1,
        }
    }
}

/// Counted in steps: each partition key is one step.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplainSummary {
    pub will_run: usize,
    pub cached: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanResult {
    pub total_steps: usize,
    pub phases: Vec<PlanPhase>,
    /// Plan-time warnings for the planned steps (`[]` when there are none).
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
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
}
