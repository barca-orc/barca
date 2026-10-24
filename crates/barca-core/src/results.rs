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
    /// `--partition`: the selection, each narrowed node's selected and skipped key counts, and
    /// the nodes the selection did not run. Absent without `--partition`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub partition_selection: Option<PartitionSelectionReport>,
}

/// The most partition keys a run lists in `final_output` (#623). A partitioned target that
/// returns more, with no `--partition` selecting them, lists only the keys the run computed (at
/// most this many) and counts the others: `{"_barca_partitions": {...}}` (`barca docs
/// partitions`, "Many keys"). `--partition` returns every key it selects, however many.
pub const LISTED_KEYS: usize = 1000;

/// What `final_output` says of a partitioned target that returned more than [`LISTED_KEYS`]
/// keys: how many, how many this run computed, and those keys' artifacts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeySummary {
    /// How many keys the target returned.
    pub keys: usize,
    /// How many of them this run computed; the others were served from cache.
    pub computed: usize,
    /// The computed keys and their artifacts, sorted by key, at most [`LISTED_KEYS`].
    pub listed: Vec<(String, OutputRef)>,
}

impl KeySummary {
    /// The summary of `keys`, the sorted keys `node` returned, when there are more than
    /// [`LISTED_KEYS`] and no selection named them; `cached` holds the step ids (`node[key]`)
    /// served from cache in this run. `None` otherwise: every key is listed.
    pub fn of(
        node: &str,
        keys: &[(String, OutputRef)],
        selected: bool,
        cached: &std::collections::HashSet<String>,
    ) -> Option<Self> {
        if selected || keys.len() <= LISTED_KEYS {
            return None;
        }
        let mut id = String::with_capacity(node.len() + 32);
        let mut computed = 0;
        let mut listed = Vec::new();
        for (key, oref) in keys {
            id.clear();
            id.push_str(node);
            id.push('[');
            id.push_str(key);
            id.push(']');
            if !cached.contains(&id) {
                computed += 1;
                if listed.len() < LISTED_KEYS {
                    listed.push((key.clone(), oref.clone()));
                }
            }
        }
        Some(Self {
            keys: keys.len(),
            computed,
            listed,
        })
    }

    /// The keys to list: the summary's, or every key when there is no summary.
    pub fn listed<'a>(
        summary: Option<&'a KeySummary>,
        keys: Option<&'a [(String, OutputRef)]>,
    ) -> Option<&'a [(String, OutputRef)]> {
        match summary {
            Some(s) => Some(&s.listed),
            None => keys,
        }
    }

    /// The same counts without the listed artifacts (what a printed note needs).
    pub fn counts(&self) -> Self {
        Self {
            keys: self.keys,
            computed: self.computed,
            listed: Vec::new(),
        }
    }
}

/// `partition_selection` in the JSON results of a run, a dry run and `barca plan` under
/// `--partition` (see [`crate::selection`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionSelectionReport {
    /// Dimension → the selected values, sorted.
    pub selection: std::collections::BTreeMap<String, Vec<String>>,
    /// Each narrowed node (a planned partitioned node with a selected dimension), by id.
    pub nodes: Vec<SelectedNode>,
    /// The nodes not run: each reads a narrowed node across keys, or is below one. Each is also
    /// a `steps` entry with status `skipped` and reason `partition_selection`.
    pub skipped_nodes: Vec<String>,
}

/// One narrowed node of a [`PartitionSelectionReport`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SelectedNode {
    pub id: String,
    /// The keys kept; `null` while not known (a `partitions_from` list before its source ran).
    pub selected: Option<usize>,
    /// The keys left out; `null` while not known.
    pub skipped: Option<usize>,
}

/// What a run did not publish to the artifact store: the run JSON's `uploads` object, and the
/// dry run's. Every step succeeded; these results are recorded and kept on this machine, and
/// the next command that opens the store uploads them before its own work.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UploadsReport {
    /// How many results of this machine are waiting to be uploaded.
    pub pending: usize,
    /// Why: the last upload error, `null` when there is none.
    pub error: Option<String>,
    /// The nodes (partition keys included) whose results are pending, sorted.
    #[serde(default)]
    pub nodes: Vec<String>,
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

/// Read target objects directly through serde so their declared order survives a wire round trip.
fn deserialize_targets<'de, D, T>(deserializer: D) -> Result<Vec<(String, T)>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct TargetsVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for TargetsVisitor<T> {
        type Value = Vec<(String, T)>;
        fn expecting(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            fmt.write_str("an object keyed by target name")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut targets = Vec::new();
            let mut names = std::collections::HashSet::new();
            while let Some((name, value)) = map.next_entry::<String, T>()? {
                if !names.insert(name.clone()) {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate target '{name}'"
                    )));
                }
                targets.push((name, value));
            }
            Ok(targets)
        }
    }
    deserializer.deserialize_map(TargetsVisitor(std::marker::PhantomData))
}

impl MultiResult {
    /// True when any target failed.
    pub fn any_failed(&self) -> bool {
        self.targets.iter().any(|(_, t)| t.failed())
    }
}

/// How one target of a multi-target run ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    /// `success` or `failed`; under a scheduled tick also `blocked` (it reads a `Manual` node
    /// that has never been computed, so it was not run; not a failure of the tick).
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
    /// A partitioned target's returned keys and their artifacts (see [`GetResult::partitions`]);
    /// on a failed target, the keys that succeeded.
    #[serde(skip)]
    pub partitions: Option<Vec<(String, OutputRef)>>,
    /// See [`GetResult::key_summary`].
    #[serde(skip)]
    pub key_summary: Option<KeySummary>,
}

impl TargetOutcome {
    /// Whether this target failed: neither `success` nor `blocked`.
    pub fn failed(&self) -> bool {
        self.status != "success" && self.status != "blocked"
    }
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
    /// `not_materialized`, `artifact_missing`, `evicted`, `partitions_unknown`,
    /// `sensor_output_unknown` or `cutoff_pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The reason in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Dry run, a step with no cached result: what changed in its run hash since its last
    /// successful materialization (`code`, `decorator argument NAME`, `decorator NAME`,
    /// `helper FILE:NAME`, `input ID`, `sensor ID`, `env NAME`, `kind`). Absent when that
    /// materialization recorded no components (before 0.29.0) or there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<Vec<String>>,
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
    /// Active elapsed seconds across this step's attempts and executed partition keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
    /// Maximum attempts on an executed partition key; absent on cache hits/dry runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    /// Slow module imports included in elapsed_seconds, summed across attempts/keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_seconds: Option<f64>,
    /// This recompute produced different bytes for the same node and run hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<PartitionSummary>,
    /// Declared env values the step used (`@asset(env=[...])`): name -> value, `null` when unset,
    /// `"<redacted>"` for secret-looking names. Absent when the node declares no env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
    /// Environment variables the step's code reads without declaring them in `env=[...]`
    /// (`barca docs cache`, "Environment variables"), sorted; a name built at run time is a `*`
    /// pattern. Not part of the run hash. Absent when there are none. Experimental (#574).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undeclared_env: Option<Vec<String>>,
    /// Dry run only: the node's `weight=` when it is above 1 (#436). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
    /// Dry run of a node with `batch=` (RFC-0013): how the keys that will run are grouped into
    /// calls. Absent for a node without `batch=`, or when no key will run. Experimental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub batch: Option<BatchPlan>,
}

/// How a dry run groups a batched node's keys into calls (RFC-0013).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BatchPlan {
    /// The grouping in effect, as written: `batch=all`, `batch=20`, `batch=none`,
    /// `batch=batches(...)`, `batch=batches_by(fn)`.
    pub grouping: String,
    /// The keys that will run.
    pub keys: usize,
    /// How many calls they make; `null` when the dry run cannot tell (`batches(...)` or
    /// `batches_by(...)` over more keys than it lists).
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub calls: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionSummary {
    pub total: usize,
    pub cached: usize,
    pub will_run: usize,
    /// The keys that will (or did) run, capped at 20.
    pub will_run_keys: Vec<String>,
    /// Dry run: for each key of `will_run_keys` that has an earlier successful
    /// materialization, what changed in its run hash since then (the components `changed` on
    /// the step lists). A key absent from it has no earlier result (a new key), or one
    /// recorded before 0.29.0. Absent when no key has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub changed: Option<std::collections::BTreeMap<String, Vec<String>>>,
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
    /// Results of an earlier run still waiting to be uploaded; the real run uploads them
    /// before its own work. Absent when there are none.
    pub uploads: Option<UploadsReport>,
    /// The most the `weight=` of the steps running at once may add up to (#436).
    pub capacity: usize,
    /// See [`GetResult::partition_selection`].
    pub partition_selection: Option<PartitionSelectionReport>,
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
        let mut s = serializer.serialize_struct("ExplainResult", 9)?;
        s.serialize_field("dry_run", &self.dry_run)?;
        s.serialize_field("command", &self.command)?;
        if self.targets.len() > 1 {
            s.serialize_field("targets", &Targets(&self.targets))?;
        } else {
            s.serialize_field("target", &self.target)?;
        }
        s.serialize_field("capacity", &self.capacity)?;
        s.serialize_field("steps", &self.steps)?;
        s.serialize_field("summary", &self.summary)?;
        s.serialize_field("warnings", &self.warnings)?;
        if let Some(uploads) = &self.uploads {
            s.serialize_field("uploads", uploads)?;
        }
        if let Some(selection) = &self.partition_selection {
            s.serialize_field("partition_selection", selection)?;
        }
        s.end()
    }
}

impl<'de> Deserialize<'de> for ExplainResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireExplain {
            dry_run: bool,
            command: String,
            #[serde(default)]
            target: Option<String>,
            #[serde(default, deserialize_with = "deserialize_targets")]
            targets: Vec<(String, TargetPrediction)>,
            steps: Vec<StepReport>,
            summary: ExplainSummary,
            #[serde(default)]
            warnings: Vec<PlanWarning>,
            #[serde(default)]
            uploads: Option<UploadsReport>,
            #[serde(default)]
            capacity: usize,
            #[serde(default)]
            partition_selection: Option<PartitionSelectionReport>,
        }
        let wire = WireExplain::deserialize(deserializer)?;
        if !wire.targets.is_empty() && (wire.targets.len() < 2 || wire.target.is_some()) {
            return Err(serde::de::Error::custom(
                "an explanation has one target or multiple targets, never both",
            ));
        }
        Ok(Self {
            dry_run: wire.dry_run,
            command: wire.command,
            target: wire.target,
            targets: wire.targets,
            steps: wire.steps,
            summary: wire.summary,
            warnings: wire.warnings,
            uploads: wire.uploads,
            capacity: wire.capacity,
            partition_selection: wire.partition_selection,
        })
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

pub const PLAN_VERSION: u32 = 1;

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
