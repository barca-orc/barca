//! The report values below the command layer: what a step, a target, a dry run and a run's
//! uploads and partition selection report. Data only, like the rest of `barca_core::results`,
//! which re-exports them; they live here so the planner, the engine and the metadata DB can
//! return them.

use crate::step_result::{ErrorPhase, OutputRef};
use crate::step_status::StepStatus;
use serde::{Deserialize, Serialize};

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
    /// When present, only these combinations are selected; `selection` summarizes their values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_keys: Option<Vec<std::collections::BTreeMap<String, String>>>,
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

/// A plan-time warning about one step, found by static analysis (no user code is imported).
///
/// `get`, `run`, their `--dry-run` and `plan` report the same list for the same plan: one line
/// each on stderr and the `warnings` array in JSON output (`barca docs contract`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanWarning {
    /// What was found: `unused_input` or `undeclared_env`.
    pub kind: String,
    /// The step's node id (`file.py:name`).
    pub node: String,
    /// The parameter concerned (`unused_input`), or the environment variable (`undeclared_env`).
    pub param: String,
    /// The warning in words, with what to do about it.
    pub message: String,
}

/// Read target objects directly through serde so their declared order survives a wire round trip.
pub fn deserialize_targets<'de, D, T>(deserializer: D) -> Result<Vec<(String, T)>, D::Error>
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
    /// Where `failed_node` failed, when its worker said so: carried to the run's error
    /// envelope (`phase`), not into the run JSON.
    #[serde(skip)]
    pub failed_phase: Option<ErrorPhase>,
    /// A partitioned target's returned keys and their artifacts (see `barca_core::results::GetResult::partitions`);
    /// on a failed target, the keys that succeeded.
    #[serde(skip)]
    pub partitions: Option<Vec<(String, OutputRef)>>,
    /// See `barca_core::results::GetResult::key_summary`.
    #[serde(skip)]
    pub key_summary: Option<KeySummary>,
}

impl TargetOutcome {
    /// Whether this target failed: neither `success` nor `blocked`.
    pub fn failed(&self) -> bool {
        !StepStatus::Success.is(Some(&self.status)) && !StepStatus::Blocked.is(Some(&self.status))
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
    /// Real run only: `ran`, `cached`, `partial`, `skipped` or `failed` (in a failed run's
    /// result): a [`StepStatus`] spelling.
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
    /// A real run of a node whose keys ran in batched calls (RFC-0013): how many calls made how
    /// many keys, and where their time went. Absent otherwise. Experimental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub batch_execution: Option<BatchExecution>,
}

/// A node's batched calls in one run, summed over the calls that wrote at least one key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BatchExecution {
    pub calls: usize,
    /// The keys those calls wrote.
    pub keys: usize,
    pub import_seconds: f64,
    /// Reading input artifacts. A lazy input is read in `compute_seconds` instead.
    pub load_seconds: f64,
    /// The function call. A returned lazy relation is computed in `split_seconds` instead.
    pub compute_seconds: f64,
    /// Dividing the result into keys.
    pub split_seconds: f64,
    /// Writing and hashing each key's artifact and sinks, in the worker.
    pub write_seconds: f64,
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
    /// See `barca_core::results::GetResult::partition_selection`.
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
    pub fn add(&mut self, r: &StepReport) {
        match (r.action.as_deref(), &r.partitions) {
            (Some("unknown"), _) => self.unknown += 1,
            (_, Some(p)) => {
                self.cached += p.cached;
                self.will_run += p.will_run;
            }
            (action, None) if StepStatus::Cached.is(action) => self.cached += 1,
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
