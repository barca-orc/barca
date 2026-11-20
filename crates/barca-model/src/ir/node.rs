//! One node of the graph: declaration, fingerprint, invocation, findings.

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::HashMap;

use crate::model::{
    BatchSpec, Freshness, NodeKind, PartitionSpec, SerializerKind, SinkDecl, ValueType,
};

// ─── Declaration ─────────────────────────────────────────────────────────────

/// What a node declares, in no language: everything the graph, the planner and the scheduler
/// read to decide what runs, when, and how its results are kept. A frontend fills it from
/// whatever its language's declarations look like (for Python, the decorator arguments).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDecl {
    /// The kind of node.
    pub kind: NodeKind,
    /// The name the declaration gives the node (`name=`), when it gives one: then it is the
    /// node's id, and people may write it for the node.
    pub explicit_name: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// Metadata tags.
    pub tags: HashMap<String, String>,
    /// Freshness policy.
    pub freshness: Freshness,
    /// `Schedule(..., max_failures=N)`: consecutive failed ticks before the schedule is skipped
    /// and reported `failing`. `None` when not written (the scheduler's default applies).
    pub max_failures: Option<u32>,
    /// `Schedule(..., timeout_seconds=N)`: how long a scheduled run of this node may take under
    /// `barca serve` (0: no limit). `None` when not written (the server's setting applies).
    pub schedule_timeout: Option<u64>,
    /// `Schedule(..., batch=V)`: the per-run `--batch` override a scheduled tick of this node
    /// runs with (as the flag takes it: `all`, `none`, `20`, `scored=all`).
    pub schedule_batch: Option<String>,
    /// Partition dimensions. A dimension taken from a partitioned upstream holds that
    /// upstream's spec (see [`Self::inherited_partitions`]).
    pub partitions: HashMap<String, PartitionSpec>,
    /// The `partitions_from` dimensions this node takes from a partitioned upstream:
    /// `partitions_from(cleaned)` where `cleaned` is itself `partitions_from(source)`. Its keys
    /// come from `source`, but a dict `source`'s values are passed only to the nodes that name
    /// it (`crate::keyed`). Not part of any hash.
    pub inherited_partitions: Vec<String>,
    /// `batch=` (RFC-0013): how the keys that run are grouped into calls. `None` when not
    /// declared (or `batch=None`): one key per call, and the step takes a single key.
    /// Scheduling only: not part of any hash.
    pub batch: Option<BatchSpec>,
    /// Sink declarations (from stacked `@sink` decorators). Typically 0–2 items.
    pub sinks: SmallVec<[SinkDecl; 2]>,
    /// Explicit artifact serializer override from `@asset(serializer="parquet")`.
    pub artifact_serializer: Option<SerializerKind>,
    /// Declared environment variables (`@asset(env=["NAME", ...])`), in declaration order.
    /// Their values are read at plan time and folded into the run hash; see [`crate::envdeps`].
    pub env: Vec<String>,
    /// Timeout in seconds.
    pub timeout_seconds: u32,
    /// Total number of attempts on failure (1 = no retry).
    pub retries: u32,
    /// Base backoff in seconds between attempts; delay = `retry_backoff_seconds * attempt`.
    pub retry_backoff_seconds: f64,
    /// `weight=`: how much of the pool's capacity the step takes while it runs, at least 1
    /// (#436). Not part of the run hash.
    pub weight: u32,
    /// `@asset(deterministic=False)`: the asset may return other bytes for the same code and
    /// inputs (a snapshot of an external system), so barca does not warn that it is not
    /// deterministic (#627). Not part of the run hash and no effect on caching.
    pub deterministic: bool,
    /// `@asset(cutoff=True)`: what reads it folds in the content hash of its output instead of
    /// its run hash, so a rerun that writes the same bytes leaves them cached (#628). Not part
    /// of its own run hash; see `barca_service::cache::is_cutoff`.
    pub cutoff: bool,
}

// ─── Fingerprint ─────────────────────────────────────────────────────────────

/// What identifies a node's definition, supplied by the frontend and opaque here: barca
/// compares the hash and the digests for equality, folds the hash into run hashes, and shows
/// the component names; it never recomputes either or reads meaning into a name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Content hash of the definition: changes when anything that determines the node's result
    /// changes (for Python: the function body, helpers, constants, the decorator arguments
    /// that count, and the kind).
    pub definition_hash: String,
    /// The definition's named components and a short digest of each (for Python: `code`,
    /// `decorator ...`, `helper ...`, `kind`; see `barca_service::changes`), recorded with each
    /// materialization to say which one changed (#558). Not part of any hash.
    pub components: Vec<(String, String)>,
}

// ─── Invocation ──────────────────────────────────────────────────────────────

/// How a worker runs the step. This is the frontend's: the graph and the planner carry it to
/// the worker and read nothing else from it. One variant today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "language", rename_all = "snake_case")]
pub enum Invocation {
    /// A Python function, imported by file path and called by name.
    Python(PythonFunction),
}

impl Invocation {
    /// The Python function to call. Every caller of this assumes a Python step: when a second
    /// variant exists, this returns an `Option` and each caller decides what it does without one.
    pub fn python(&self) -> &PythonFunction {
        match self {
            Invocation::Python(function) => function,
        }
    }
}

/// A step that is a Python function: where it is, and how its tabular values are handed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonFunction {
    /// Source file (repo-relative path), imported by the worker.
    pub source_file: String,
    /// The Python function name.
    pub function_name: String,
    /// 1-based line of the definition's first decorator.
    pub line: usize,
    /// Parameter types from function annotations (param → frame loader).
    pub param_types: HashMap<String, ValueType>,
    /// Return type from the function's return annotation (frame writer/loader).
    pub return_type: Option<ValueType>,
}

// ─── Findings ────────────────────────────────────────────────────────────────

/// What the frontend's analysis of the step's code noticed. Reported as plan warnings; part of
/// no hash and no decision. A frontend that does no such analysis leaves both empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeFindings {
    /// Data inputs the step's code never uses.
    pub unused_inputs: Vec<String>,
    /// Environment variables the step and the helper code it reaches read, sorted; a name built
    /// at run time is a `*` pattern. Those not in [`NodeDecl::env`] are reported.
    pub env_reads: Vec<String>,
}

// ─── Node ────────────────────────────────────────────────────────────────────

/// A node of the graph. Inputs are already resolved to node ids: which node a reference in
/// the source means is the frontend's question, answered before the graph exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    /// Stable identity (continuity key).
    pub id: String,
    /// What the node declares.
    pub decl: NodeDecl,
    /// Resolved inputs: param_name → upstream_node_id.
    pub resolved_inputs: HashMap<String, String>,
    /// Collected inputs (fan-in): param_name → upstream_node_id.
    pub resolved_collected: HashMap<String, String>,
    /// The definition's hash and components, as the frontend computed them.
    pub fingerprint: Fingerprint,
    /// How a worker runs the step.
    pub invoke: Invocation,
    /// What the frontend's analysis noticed.
    pub findings: NodeFindings,
}

impl DagNode {
    /// Whether new bytes under an unchanged run hash mean this node is not deterministic: only
    /// for an asset that is not `Manual` and not declared `@asset(deterministic=False)`
    /// (#548, #627; `barca_service::cache::checks_bytes` has the reasons). A property of the
    /// declaration alone, so the ledger's post and the cache decision read the same answer.
    pub fn checks_bytes(&self) -> bool {
        self.decl.kind == NodeKind::Asset
            && self.decl.freshness != Freshness::Manual
            && self.decl.deterministic
    }

    /// The environment variables the step reads without declaring them in `env=[...]`, sorted.
    pub fn undeclared_env(&self) -> Vec<String> {
        let reads = self.findings.env_reads.iter().cloned().collect();
        crate::envdeps::undeclared(&reads, &self.decl.env)
    }

    pub fn kind(&self) -> NodeKind {
        self.decl.kind
    }
    pub fn function_name(&self) -> &str {
        &self.invoke.python().function_name
    }
    pub fn source_file(&self) -> &str {
        &self.invoke.python().source_file
    }
}

// ─── Edge kinds ──────────────────────────────────────────────────────────────

/// How two nodes are related in the DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Direct 1:1 dependency (partition-aligned if both partitioned).
    Direct,
    /// Fan-in: downstream consumes all partitions of upstream via `collect()`.
    Collect,
    /// Partition source: upstream defines the partition universe for downstream.
    PartitionSource,
}
