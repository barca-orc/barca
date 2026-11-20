//! What the Python parser produces for one decorated function, before any graph exists.
//!
//! This is the frontend's own value: flat, in the order the parser reads the decorator, with
//! what only the frontend needs (references as written, the text and cone the definition hash
//! covers). [`crate::link`] turns a project's extracted nodes into the graph's
//! [`barca_model::ir::DagNode`]s; nothing after that reads this type.

use barca_model::model::{
    BatchSpec, Freshness, NodeKind, NodeRef, PartitionSpec, SerializerKind, SinkDecl, ValueType,
};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::HashMap;

// ─── Input references ────────────────────────────────────────────────────────

/// A declared input to a node — maps a function parameter to an upstream node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredInput {
    /// The parameter name in the function signature.
    pub param_name: String,
    /// Reference to the upstream node.
    pub upstream: NodeRef,
    /// Whether this input uses `collect()` (fan-in from all partitions).
    pub collected: bool,
}

// ─── Parallel calls ──────────────────────────────────────────────────────────

/// A parallel work item extracted from a `parallel()` call in a task body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelCall {
    /// Function references in this parallel() call. Each is a @task function.
    /// Empty if the call is fully dynamic (can't be resolved statically).
    pub static_refs: Vec<NodeRef>,
    /// Whether this parallel() call has dynamic (non-static) arguments
    /// that need runtime expansion (like generators or splat of variables).
    pub is_dynamic: bool,
}

// ─── Extracted node ──────────────────────────────────────────────────────────

/// A fully extracted node from Python source — the output of parsing.
/// Contains all declared metadata needed for DAG construction and planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedNode {
    /// The kind of node.
    pub kind: NodeKind,
    /// The Python function name.
    pub function_name: String,
    /// Explicit `name=` override for continuity key.
    pub explicit_name: Option<String>,
    /// Freshness policy.
    pub freshness: Freshness,
    /// `Schedule(..., max_failures=N)`: consecutive failed ticks before the schedule is skipped
    /// and reported `failing`. `None` when not written (the scheduler's default applies).
    #[serde(default)]
    pub max_failures: Option<u32>,
    /// `Schedule(..., timeout_seconds=N)`: how long a scheduled run of this node may take under
    /// `barca serve` (0: no limit). `None` when not written (the server's setting applies).
    #[serde(default)]
    pub schedule_timeout: Option<u64>,
    /// Declared inputs (parameter → upstream mapping). Typically 0–4 items.
    pub inputs: SmallVec<[DeclaredInput; 4]>,
    /// Partition dimensions.
    pub partitions: HashMap<String, PartitionSpec>,
    /// Sink declarations (from stacked `@sink` decorators). Typically 0–2 items.
    pub sinks: SmallVec<[SinkDecl; 2]>,
    /// Timeout in seconds.
    pub timeout_seconds: u32,
    /// Total number of attempts on failure (1 = no retry).
    pub retries: u32,
    /// `weight=`: how much of the pool's capacity the step takes while it runs, at least 1
    /// (#436). Not part of the run hash.
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// `@asset(deterministic=False)`: the asset may return other bytes for the same code and
    /// inputs (a snapshot of an external system), so barca does not warn that it is not
    /// deterministic (#627). Not part of the run hash and no effect on caching.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub deterministic: bool,
    /// `@asset(cutoff=True)`: what reads it folds in the content hash of its output instead of
    /// its run hash, so a rerun that writes the same bytes leaves them cached (#628). Not part
    /// of its own run hash; see `barca_service::cache::is_cutoff`.
    #[serde(default)]
    pub cutoff: bool,
    /// Base backoff in seconds between attempts; delay = `retry_backoff_seconds * attempt`.
    pub retry_backoff_seconds: f64,
    /// Human-readable description.
    pub description: Option<String>,
    /// Metadata tags.
    pub tags: HashMap<String, String>,
    /// Whether this function is marked `@unsafe`.
    pub is_unsafe: bool,
    /// Source file (repo-relative path).
    pub source_file: String,
    /// Byte offset of the function definition in source.
    pub byte_offset: usize,
    /// 1-based line of that offset: the first decorator of the definition.
    #[serde(default)]
    pub line: usize,
    /// The text the definition hash covers: the decorator parts that count, in canonical form,
    /// then the function from `def` on as written (`crate::definition`).
    pub source_text: String,
    /// Hash of the dependency cone: source text of all helpers, constants,
    /// and imports that this function references (transitively).
    /// Includes same-file definitions. Cross-file deps tracked by source_file content.
    pub cone_hash: String,
    /// A short digest of each definition in the cone, by label
    /// (`barca_service::cone::cone_hash_and_parts`). Recorded with each materialization to say which
    /// helper changed (#558); not part of any hash, and not serialized.
    #[serde(skip)]
    pub cone_parts: Vec<(String, String)>,
    /// Explicit artifact serializer override from `@asset(serializer="parquet")`.
    pub artifact_serializer: Option<SerializerKind>,
    /// Parameter types from function annotations (param → frame loader).
    pub param_types: HashMap<String, ValueType>,
    /// Return type from the function's return annotation (frame writer/loader).
    pub return_type: Option<ValueType>,
    /// Parallel calls found in this task's function body.
    /// Only populated for `@task` nodes. Empty for assets/sensors.
    pub parallel_calls: Vec<ParallelCall>,
    /// Declared environment variables (`@asset(env=["NAME", ...])`), in declaration order.
    /// Their values are read at plan time and folded into the run hash; see [`barca_model::envdeps`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Data inputs the function body never uses, found by static analysis while parsing
    /// (see `barca_service::unused_inputs`). Reported as plan warnings; not part of any hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unused_inputs: Vec<String>,
    /// Environment variables the function and the helper code it reaches read, found by static
    /// analysis (see `barca_service::env_reads`), sorted; a name built at run time is a `*` pattern.
    /// Those not in [`Self::env`] are reported; not part of any hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_reads: Vec<String>,
    /// The `partitions_from` dimensions this node takes from a partitioned upstream (set when
    /// the DAG is built, `crate::link`): `partitions_from(cleaned)` where `cleaned` is itself
    /// `partitions_from(source)`. Its keys come from `source`, but a dict `source`'s values are
    /// passed only to the nodes that name it (`barca_model::keyed`). Not part of any hash.
    #[serde(skip)]
    pub inherited_partitions: Vec<String>,
    /// `batch=` (RFC-0013): how the keys that run are grouped into calls. `None` when not
    /// declared (or `batch=None`): one key per call, and the function takes a single key.
    /// Scheduling only: not part of any hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchSpec>,
    /// `Schedule(..., batch=V)`: the per-run `--batch` override a scheduled tick of this node
    /// runs with (as the flag takes it: `all`, `none`, `20`, `scored=all`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_batch: Option<String>,
}

fn default_weight() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl ExtractedNode {
    /// The environment variables the step reads without declaring them in `env=[...]`, sorted.
    pub fn undeclared_env(&self) -> Vec<String> {
        let reads = self.env_reads.iter().cloned().collect();
        barca_model::envdeps::undeclared(&reads, &self.env)
    }

    /// The continuity key — stable identity for this node.
    pub fn continuity_key(&self) -> String {
        if let Some(ref name) = self.explicit_name {
            name.clone()
        } else {
            format!("{}:{}", self.source_file, self.function_name)
        }
    }
}
