//! Core domain model — every concept from the barca workflow specification.
//!
//! These types represent the *declared* state of a barca project as parsed
//! from Python source files. They are the input to DAG construction, execution
//! planning, and hashing.
//!
//! Allocation strategy:
//! - `SmallVec<[T; N]>` for small collections (inputs, sinks — usually ≤4 items, stays on stack)
//! - `String` everywhere else (ruff's AST already gives us owned Strings; no point converting)

use croner::parser::{CronParser, Seconds, Year};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

// ─── Node kinds ──────────────────────────────────────────────────────────────

/// The three kinds of node in a barca DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// `@asset` — produces and caches a value.
    Asset,
    /// `@sensor` — observes external state, returns `(updated: bool, output)`.
    Sensor,
    /// `@task` — never cached, always re-runs; may appear anywhere in the DAG.
    /// May depend on assets/sensors/tasks, but must not be upstream of an
    /// asset or sensor (that would poison caching).
    Task,
}

// ─── Freshness ───────────────────────────────────────────────────────────────

/// Freshness policy — determines when a node is eligible for execution.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Freshness {
    /// Auto-materializes whenever stale and all upstreams are fresh.
    Always,
    /// Only runs on explicit `barca assets refresh`.
    Manual,
    /// Runs when a cron tick has elapsed since last execution.
    Schedule(CronExpr),
}

impl Freshness {
    /// Default freshness for a given node kind.
    pub fn default_for(kind: NodeKind) -> Self {
        match kind {
            NodeKind::Asset | NodeKind::Task => Freshness::Always,
            NodeKind::Sensor => Freshness::Manual,
        }
    }
}

/// A validated cron expression. Accepts standard 5-field, minute-granular cron
/// (`minute hour dom month dow`) and 6-field cron with a leading seconds field
/// (`*/5 * * * * *` — every 5 seconds). Barca's scheduler evaluates at 1-second
/// resolution: a 6-field expression fires at its seconds cadence, while a 5-field
/// expression pins seconds to `0` and fires once per matching minute.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CronExpr(pub String);

impl CronExpr {
    /// Parse a cron string under Barca's uniform field policy: seconds are
    /// OPTIONAL (5-field minute-granular, or 6-field with a leading seconds
    /// field) and the year field is disallowed. This is the single source of
    /// truth for the cron grammar — validation, the scheduler, and the
    /// `/schedule` handler all parse through here, so they can never disagree on
    /// what a valid schedule is. (The 5-field-only restriction from issue #109
    /// was widened to allow seconds once the scheduler gained sub-minute ticks;
    /// an omitted seconds field is pinned to `0`, so 5-field crons are unchanged.)
    ///
    /// Returns a human-readable reason on failure.
    pub fn parse(s: &str) -> Result<croner::Cron, String> {
        CronParser::builder()
            .seconds(Seconds::Optional)
            .year(Year::Disallowed)
            .build()
            .parse(s.trim())
            .map_err(|e| e.to_string())
    }

    /// Parse this expression's stored string. Convenience over [`CronExpr::parse`].
    pub fn parsed(&self) -> Result<croner::Cron, String> {
        Self::parse(&self.0)
    }

    /// Validate a cron string without retaining the parsed schedule. Delegates to
    /// [`CronExpr::parse`] so validation and execution share one grammar.
    ///
    /// Returns a human-readable reason on failure.
    pub fn validate(s: &str) -> Result<(), String> {
        Self::parse(s).map(|_| ())
    }
}

// ─── Partition specification ─────────────────────────────────────────────────

/// How partitions are defined for an asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PartitionSpec {
    /// Inline static list: `partitions(["AAPL", "MSFT", "GOOG"])`.
    Static { values: Vec<PartitionValue> },
    /// Dynamic expression that needs Python evaluation at plan time.
    /// e.g., `partitions([f"p{i}" for i in range(100)])` or `partitions(get_tickers())`
    Dynamic { source_text: String },
    /// Derived from upstream asset: `partitions_from(tickers)`.
    /// Resolved at execution time — source asset must materialize first.
    DerivedFrom { source_ref: NodeRef },
}

/// A single partition value — scalar types allowed in partition keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PartitionValue {
    Str(String),
    Int(i64),
}

// ─── Partition key (runtime) ─────────────────────────────────────────────────

/// A partition coordinate — maps dimension names to values, deterministically ordered.
/// This is the *runtime* partition identity for a step, not the declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionKey(pub BTreeMap<String, String>);

impl PartitionKey {
    pub fn empty() -> Self {
        Self(BTreeMap::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The canonical suffix string: `"k1=v1,k2=v2"`. Empty string if no partitions.
    pub fn suffix(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Format as `"base[k1=v1,k2=v2]"` or just `"base"` if empty.
    pub fn display_id(&self, base: &str) -> String {
        if self.0.is_empty() {
            base.to_string()
        } else {
            format!("{base}[{}]", self.suffix())
        }
    }

    /// Parse a node_id string into (base, PartitionKey).
    /// `"test.py:foo[region=us,tier=1]"` → `("test.py:foo", PartitionKey({region: "us", tier: "1"}))`
    /// `"test.py:foo"` → `("test.py:foo", PartitionKey({}))`
    pub fn parse_from_id(id: &str) -> (&str, PartitionKey) {
        if let Some(bracket) = id.find('[') {
            let base = &id[..bracket];
            let inner = id[bracket + 1..].trim_end_matches(']');
            // Parse key=value pairs. Keys (dimension names) never contain `=` or `,`,
            // but values might. We detect pair boundaries by finding `,<ident>=` patterns:
            // a comma followed by text containing `=` before the next comma.
            let mut map = BTreeMap::new();
            let mut remaining = inner;
            while !remaining.is_empty() {
                let Some(eq_pos) = remaining.find('=') else {
                    break;
                };
                let key = &remaining[..eq_pos];
                let after_eq = &remaining[eq_pos + 1..];
                // Find where this value ends: at a `,` that's followed by another `key=`.
                let val_end = after_eq
                    .find(',')
                    .and_then(|comma| {
                        let rest = &after_eq[comma + 1..];
                        let next_eq = rest.find('=')?;
                        let next_comma = rest.find(',').unwrap_or(rest.len());
                        (next_eq < next_comma).then_some(comma)
                    })
                    .unwrap_or(after_eq.len());
                map.insert(key.to_string(), after_eq[..val_end].to_string());
                remaining = if val_end < after_eq.len() {
                    &after_eq[val_end + 1..]
                } else {
                    ""
                };
            }
            (base, PartitionKey(map))
        } else {
            (id, PartitionKey::empty())
        }
    }
}

impl From<HashMap<String, String>> for PartitionKey {
    fn from(map: HashMap<String, String>) -> Self {
        Self(map.into_iter().collect())
    }
}

impl fmt::Display for PartitionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.suffix())
    }
}

// ─── Step identity ──────────────────────────────────────────────────────────

/// A step identity — base node ID + optional partition coordinate.
/// Separates node identity from partition coordinates at the type level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepId {
    /// The base node identity (continuity key), e.g. `"test.py:fetch"`.
    pub base: Arc<str>,
    /// The partition coordinate for this step. Empty for non-partitioned steps.
    pub partition: PartitionKey,
}

impl StepId {
    pub fn new(base: impl Into<Arc<str>>, partition: PartitionKey) -> Self {
        Self {
            base: base.into(),
            partition,
        }
    }

    pub fn unpartitioned(base: impl Into<Arc<str>>) -> Self {
        Self {
            base: base.into(),
            partition: PartitionKey::empty(),
        }
    }

    pub fn base_id(&self) -> &str {
        &self.base
    }

    /// The display string: `"base[k1=v1]"` or just `"base"`.
    pub fn display(&self) -> String {
        self.partition.display_id(&self.base)
    }

    /// Parse from a display string.
    pub fn parse(id: &str) -> Self {
        let (base, partition) = PartitionKey::parse_from_id(id);
        Self {
            base: Arc::from(base),
            partition,
        }
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}

// ─── Value types (from function annotations) ─────────────────────────────────

/// Tabular value type declared on a function parameter or return annotation.
/// When absent on a parameter, workers default to pandas for parquet reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Pandas,
    Polars,
    PyArrow,
    DuckDB,
}

impl ValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            ValueType::Pandas => "pandas",
            ValueType::Polars => "polars",
            ValueType::PyArrow => "pyarrow",
            ValueType::DuckDB => "duckdb",
        }
    }
}

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

/// A reference to another node — either a function name (local) or a canonical path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum NodeRef {
    /// Direct function reference: just the function name (resolved during DAG build).
    FunctionName(String),
    /// Canonical asset reference: `asset_ref("module/file.py:function_name")`.
    Canonical(String),
    /// A name bound by an import in the referencing file: `from <module> import <name>`, or
    /// `<module>.<name>` after `import <module>`. `module` keeps leading dots for relative
    /// imports. Resolved against the importing file's location during DAG build.
    Imported { module: String, name: String },
}

impl NodeRef {
    /// The name used for resolution (function name or the name part of canonical).
    pub fn resolution_name(&self) -> &str {
        match self {
            NodeRef::FunctionName(name) => name,
            NodeRef::Canonical(path) => path.rsplit(':').next().unwrap_or(path),
            NodeRef::Imported { name, .. } => name,
        }
    }
}

// ─── Sink declaration ────────────────────────────────────────────────────────

/// A `@sink` decorator stacked on an `@asset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkDecl {
    /// Output path (local, s3://, gs://, etc.).
    pub path: String,
    /// Serializer kind override.
    pub serializer: Option<SerializerKind>,
}

/// Supported serializer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializerKind {
    Json,
    Parquet,
    Pickle,
    Text,
    Yaml,
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
    /// The raw source text of the function (for hashing).
    pub source_text: String,
    /// Hash of the dependency cone: source text of all helpers, constants,
    /// and imports that this function references (transitively).
    /// Includes same-file definitions. Cross-file deps tracked by source_file content.
    pub cone_hash: String,
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
    /// Their values are read at plan time and folded into the run hash; see [`crate::envdeps`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

impl ExtractedNode {
    /// The continuity key — stable identity for this node.
    pub fn continuity_key(&self) -> String {
        if let Some(ref name) = self.explicit_name {
            name.clone()
        } else {
            format!("{}:{}", self.source_file, self.function_name)
        }
    }
}

// ─── DAG node (enriched after construction) ──────────────────────────────────

/// A node in the constructed DAG. Wraps the original `ExtractedNode` and adds
/// resolved dependency information (upstream node IDs, not raw function names).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    /// Stable identity (continuity key).
    pub id: String,
    /// The original parsed node (all declared metadata).
    pub extracted: ExtractedNode,
    /// Resolved inputs: param_name → upstream_node_id.
    pub resolved_inputs: HashMap<String, String>,
    /// Collected inputs (fan-in): param_name → upstream_node_id.
    pub resolved_collected: HashMap<String, String>,
    /// Content hash of this node's definition (source + metadata).
    /// Changes when the function body, helpers, constants, or decorator args change.
    pub definition_hash: String,
}

impl DagNode {
    // Convenience accessors that forward to the extracted node.
    pub fn kind(&self) -> NodeKind {
        self.extracted.kind
    }
    pub fn function_name(&self) -> &str {
        &self.extracted.function_name
    }
    pub fn source_file(&self) -> &str {
        &self.extracted.source_file
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
