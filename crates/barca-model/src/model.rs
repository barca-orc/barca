//! Core domain model — every concept from the barca workflow specification.
//!
//! These types are the values a declaration is made of (kinds, freshness, partitions, sinks,
//! batches) and the identities of steps. The node that holds them is [`crate::ir::DagNode`];
//! what the Python parser produces for a decorated function is
//! `barca_python::extracted::ExtractedNode`.
//!
//! Allocation strategy:
//! - `SmallVec<[T; N]>` for small collections (inputs, sinks — usually ≤4 items, stays on stack)
//! - `String` everywhere else (ruff's AST already gives us owned Strings; no point converting)

use croner::parser::{CronParser, Seconds, Year};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

pub use crate::ir::{
    DagNode, EdgeKind, Fingerprint, Invocation, NodeDecl, NodeFindings, PythonFunction,
};

// ─── Node kinds ──────────────────────────────────────────────────────────────

/// The three kinds of node in a barca DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", content = "value")]
pub enum Freshness {
    /// Brought up to date at each tick of a scheduled node above it (RFC-0008).
    Always,
    /// Refreshed by explicit request only; what is below it reads its current result.
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
/// (`0/5 * * * * *` — every 5 seconds). Barca's scheduler evaluates at 1-second
/// resolution: a 6-field expression fires at its seconds cadence, while a 5-field
/// expression pins seconds to `0` and fires once per matching minute.
//
// Keep `*/` out of this doc comment: ts-rs copies it into a JSDoc block in the
// generated UI bindings, where `*/` would terminate the comment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
    /// `pl.LazyFrame`: the worker hands the step a scan, so only what its query uses is read.
    PolarsLazy,
    PyArrow,
    /// `pyarrow.dataset.Dataset`: a dataset over the artifact's file, read lazily.
    PyArrowDataset,
    DuckDB,
}

impl ValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            ValueType::Pandas => "pandas",
            ValueType::Polars => "polars",
            ValueType::PolarsLazy => "polars_lazy",
            ValueType::PyArrow => "pyarrow",
            ValueType::PyArrowDataset => "pyarrow_dataset",
            ValueType::DuckDB => "duckdb",
        }
    }

    /// True for types whose reader fetches only what the step's query uses.
    pub fn is_lazy(self) -> bool {
        matches!(
            self,
            ValueType::PolarsLazy | ValueType::DuckDB | ValueType::PyArrowDataset
        )
    }
}

// ─── Input references ────────────────────────────────────────────────────────

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkDecl {
    /// Output path (local, s3://, gs://, etc.). A path ending in `/` is a dataset sink.
    pub path: String,
    /// Serializer kind override.
    pub serializer: Option<SerializerKind>,
    /// `partition_by=[...]`: columns of the returned table that add Hive levels below the
    /// asset's own dimensions (dataset sinks only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partition_by: Vec<String>,
    /// `rows_per_file=`: the most rows one file of a dataset sink holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_per_file: Option<u64>,
    /// The asset's partition dimensions in declared order: the Hive levels of a dataset sink.
    /// Filled from the `@asset` decorator, not written in `@sink`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dims: Vec<String>,
}

impl SinkDecl {
    /// A path ending in `/` is a dataset prefix, written as a Hive-partitioned dataset.
    pub fn is_dataset(&self) -> bool {
        self.path.ends_with('/')
    }
}

/// Supported serializer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializerKind {
    Json,
    Parquet,
    Csv,
    Pickle,
    Text,
    Yaml,
}

/// `batch=` of a partitioned `@asset` or `@task` (RFC-0013): how the keys that need to run
/// are grouped into calls of the function, which then receives the list of its call's keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BatchSpec {
    /// `batch=N`: at most N keys per call, in key order.
    Size { size: u32 },
    /// `batch="all"`: every key that needs to run, in one call.
    All,
    /// `batches([[...], [...]])`: the groups named, each as the keys' inner display form
    /// (`retailer=r1`, or `month=2026-10,retailer=r1`). A key in no group runs alone, or is an
    /// error with `strict`.
    Groups {
        groups: Vec<Vec<String>>,
        #[serde(default)]
        strict: bool,
    },
    /// `batches(<expression>)`: evaluated when the pipeline is loaded, into [`Self::Groups`].
    GroupsExpr {
        source_text: String,
        #[serde(default)]
        strict: bool,
    },
    /// `batches_by(<function>, max_size=N)`: keys whose label is the same share a call,
    /// split into calls of at most `max_size`. The function is evaluated by Python when the
    /// keys that run are known.
    By {
        source_text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_size: Option<u32>,
    },
}

impl BatchSpec {
    /// How the decorator wrote it, for messages and the dry run (`batch=all`, `batch=20`).
    pub fn describe(&self) -> String {
        match self {
            BatchSpec::Size { size } => format!("batch={size}"),
            BatchSpec::All => "batch=all".to_string(),
            BatchSpec::Groups { .. } | BatchSpec::GroupsExpr { .. } => {
                "batch=batches(...)".to_string()
            }
            BatchSpec::By { source_text, .. } => format!("batch=batches_by({source_text})"),
        }
    }
}

/// A node kind as reports name it (`asset`, `task`, `sensor`), `unknown` for no node.
pub fn kind_str(kind: Option<NodeKind>) -> String {
    match kind {
        Some(NodeKind::Asset) => "asset",
        Some(NodeKind::Task) => "task",
        Some(NodeKind::Sensor) => "sensor",
        None => "unknown",
    }
    .to_string()
}
