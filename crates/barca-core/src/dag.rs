//! DAG construction and query — builds a directed acyclic graph from extracted
//! nodes, validates constraints, and supports traversal operations.

use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

use crate::hash;
use crate::model::{DagNode, EdgeKind, ExtractedNode, NodeKind};

/// The constructed DAG — validated, acyclic, ready for plan generation.
#[derive(Debug)]
pub struct Dag {
    pub graph: DiGraph<DagNode, EdgeKind>,
    index: HashMap<String, NodeIndex>,
}

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("cycle detected in dependency graph")]
    CycleDetected,

    #[error("input '{param}' on node '{node}' references unknown upstream '{upstream}'")]
    UnresolvedInput {
        node: String,
        param: String,
        upstream: String,
    },

    #[error(
        "input '{param}' on node '{node}' is ambiguous: '{upstream}' is not defined in that \
         file, and more than one file defines it ({candidates}). Import it from the file you \
         mean (`from <module> import {upstream}`) or name it with asset_ref(\"<file>:{upstream}\")"
    )]
    AmbiguousInput {
        node: String,
        param: String,
        upstream: String,
        candidates: String,
    },

    #[error(
        "input '{param}' on node '{node}': '{name}' is imported from {module} ({file}), but no \
         @asset/@task/@sensor named '{name}' is defined there"
    )]
    UnresolvedImport {
        node: String,
        param: String,
        name: String,
        module: String,
        file: String,
    },

    #[error(
        "task '{task}' cannot be an input to {downstream_kind} '{downstream}' \
         (tasks are never cached, so this would poison caching)"
    )]
    TaskAsInput {
        task: String,
        downstream: String,
        downstream_kind: &'static str,
    },

    #[error("duplicate continuity key: '{key}' defined in both '{first}' and '{second}'")]
    DuplicateKey {
        key: String,
        first: String,
        second: String,
    },

    #[error("sensor '{sensor}' cannot have inputs")]
    SensorWithInputs { sensor: String },

    /// `inputs={"x": upstream}` where `upstream` is partitioned and the consumer is not (#189).
    /// This used to pass every partition as a list, a second spelling of `collect(upstream)`.
    #[error(
        "input '{param}' on '{node}' reads partitioned asset '{upstream}', but '{node}' is not \
         partitioned"
    )]
    PartitionedInputToUnpartitioned {
        node: String,
        param: String,
        upstream: String,
    },

    /// `partitions_from(upstream)` on a partitioned upstream that the consumer cannot mirror.
    #[error("'{node}': partitions_from({upstream}) {problem}")]
    PartitionsFrom {
        node: String,
        upstream: String,
        problem: String,
        fix: String,
    },
}

impl DagError {
    /// The fix, when this error has one more specific than "check each node's inputs".
    pub fn remediation(&self) -> Option<String> {
        match self {
            DagError::PartitionedInputToUnpartitioned {
                param, upstream, ..
            } => {
                let name = upstream.rsplit(':').next().unwrap_or(upstream);
                Some(format!(
                    "Use `inputs={{\"{param}\": collect({name})}}` to receive every partition of \
                     '{name}' as one list, or `partitions={{\"<key>\": partitions_from({name})}}` \
                     to run once per partition of '{name}' with that partition's output."
                ))
            }
            DagError::PartitionsFrom { fix, .. } => Some(fix.clone()),
            _ => None,
        }
    }
}

/// Resolve `partitions_from(upstream)` where `upstream` is itself partitioned (#189).
///
/// The consumer takes the upstream's partition spec (so the same keys, static or resolved at
/// run time), and each key reads that key of the upstream under the upstream's name, unless an
/// `inputs=` entry already names the upstream (then it arrives under that parameter). Both
/// steps are then partitioned by the same dimension, so the coordinator wires each consumer key
/// to the producer key with the same partition key, as for any partition-aligned input.
///
/// `partitions_from(<unpartitioned asset>)` is left alone: its keys are the values of the list
/// that asset returns, read at dispatch time (`dispatch::expand_pending_partitions`).
fn resolve_partitions_from(nodes: &[ExtractedNode]) -> Result<Vec<ExtractedNode>, DagError> {
    use crate::model::{DeclaredInput, NodeRef, PartitionSpec};

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Todo,
        Active,
        Done,
    }

    fn resolve(
        i: usize,
        nodes: &mut [ExtractedNode],
        refs: &Resolver,
        state: &mut [State],
    ) -> Result<(), DagError> {
        if state[i] != State::Todo {
            return Ok(()); // done, or a cycle (reported by `Dag::build`)
        }
        state[i] = State::Active;
        let derived: Vec<(String, NodeRef)> = nodes[i]
            .partitions
            .iter()
            .filter_map(|(dim, spec)| match spec {
                PartitionSpec::DerivedFrom { source_ref } => {
                    Some((dim.clone(), source_ref.clone()))
                }
                _ => None,
            })
            .collect();
        for (dim, source_ref) in derived {
            let source = source_ref.resolution_name().to_string();
            let j = match refs.resolve_input(&nodes[i], &source, &source_ref) {
                Ok(j) => j,
                // Unknown: left for `Dag::build` (the source may be a plain list). Several
                // candidates or a bad import is an error now, as for any input.
                Err(DagError::UnresolvedInput { .. }) => continue,
                Err(e) => return Err(e),
            };
            resolve(j, nodes, refs, state)?;
            if nodes[j].partitions.is_empty() {
                continue; // keys come from the list the source returns
            }
            let node = nodes[i].continuity_key();
            let err = |problem: String, fix: String| DagError::PartitionsFrom {
                node: node.clone(),
                upstream: source.clone(),
                problem,
                fix,
            };
            if nodes[j].partitions.len() != 1 {
                let mut dims: Vec<&String> = nodes[j].partitions.keys().collect();
                dims.sort();
                return Err(err(
                    format!(
                        "is not supported: '{source}' has {} partition dimensions ({})",
                        dims.len(),
                        dims.iter()
                            .map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!(
                        "Declare the partitions of '{}' explicitly with partitions([...]).",
                        nodes[i].function_name
                    ),
                ));
            }
            let (src_dim, src_spec) = nodes[j]
                .partitions
                .iter()
                .next()
                .map(|(d, s)| (d.clone(), s.clone()))
                .expect("one dimension");
            if src_dim != dim {
                return Err(err(
                    format!(
                        "is declared under dimension '{dim}', but '{source}' is partitioned by \
                         '{src_dim}'"
                    ),
                    format!(
                        "Use the upstream's dimension name: \
                         `partitions={{\"{src_dim}\": partitions_from({source})}}`, with a \
                         parameter named '{src_dim}'."
                    ),
                ));
            }
            if nodes[i].partitions.len() != 1 {
                return Err(err(
                    "must be the asset's only partition dimension".to_string(),
                    format!(
                        "Remove the other dimensions from '{}', or declare its keys explicitly \
                         with partitions([...]).",
                        nodes[i].function_name
                    ),
                ));
            }
            let consumer = &mut nodes[i];
            consumer.partitions.insert(dim, src_spec);
            let file = consumer.source_file.clone();
            let named = consumer.inputs.iter().any(|inp| {
                !inp.collected && matches!(refs.resolve(&file, &inp.upstream), Ok(k) if k == j)
            });
            if !named {
                consumer.inputs.push(DeclaredInput {
                    param_name: source.clone(),
                    upstream: source_ref.clone(),
                    collected: false,
                });
            }
        }
        state[i] = State::Done;
        Ok(())
    }

    let mut out = nodes.to_vec();
    let refs = Resolver::new(&out);
    let mut state = vec![State::Todo; out.len()];
    for i in 0..out.len() {
        resolve(i, &mut out, &refs, &mut state)?;
    }
    Ok(out)
}

/// Resolves a reference written in one file (`inputs=`, `collect(...)`, `partitions_from(...)`)
/// to the node it means. In order:
///
/// - `asset_ref("file.py:name")`: the node with that id, else `name` in that file (root-relative,
///   then relative to the referencing file), else the one node named `name`.
/// - an imported name (`from m import name`, `m.name`): `name` in the file the import points at
///   (relative imports from the file's package; absolute ones from the file's directory, then
///   the root), else the one node named `name` (a re-export).
/// - a bare name: the function in the same file, else the one node with that name anywhere.
///
/// Several candidates and nothing more specific is an error, never a guess (#202): before,
/// the last file read silently won.
struct Resolver {
    ids: Vec<String>,
    by_id: HashMap<String, usize>,
    by_file: HashMap<(String, String), usize>,
    by_name: HashMap<String, Vec<usize>>,
}

enum Miss {
    NotFound,
    Ambiguous(Vec<String>),
    NotInModule { module: String, file: String },
}

fn norm_path(p: &std::path::Path) -> String {
    crate::config::normalize_lexically(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_dir(file: &str) -> std::path::PathBuf {
    std::path::Path::new(file)
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default()
}

impl Resolver {
    fn new(nodes: &[ExtractedNode]) -> Self {
        let mut r = Resolver {
            ids: Vec::with_capacity(nodes.len()),
            by_id: HashMap::new(),
            by_file: HashMap::new(),
            by_name: HashMap::new(),
        };
        for (i, n) in nodes.iter().enumerate() {
            let id = n.continuity_key();
            r.by_id.insert(id.clone(), i);
            r.ids.push(id);
            r.by_file.insert(
                (
                    norm_path(std::path::Path::new(&n.source_file)),
                    n.function_name.clone(),
                ),
                i,
            );
            r.by_name
                .entry(n.function_name.clone())
                .or_default()
                .push(i);
        }
        r
    }

    fn in_file(&self, file: &std::path::Path, name: &str) -> Option<usize> {
        self.by_file
            .get(&(norm_path(file), name.to_string()))
            .copied()
    }

    fn unique(&self, name: &str) -> Result<usize, Miss> {
        match self.by_name.get(name).map(Vec::as_slice) {
            Some([one]) => Ok(*one),
            Some(many) if !many.is_empty() => Err(Miss::Ambiguous(
                many.iter().map(|&i| self.ids[i].clone()).collect(),
            )),
            _ => Err(Miss::NotFound),
        }
    }

    /// Candidate files for `module` imported from `from_file`, most specific first.
    fn module_files(from_file: &str, module: &str) -> Vec<std::path::PathBuf> {
        let level = module.chars().take_while(|c| *c == '.').count();
        let rest = module[level..].replace('.', "/");
        let mut bases = Vec::new();
        if level > 0 {
            let mut base = file_dir(from_file);
            for _ in 1..level {
                base = base.join("..");
            }
            bases.push(base);
        } else {
            bases.push(file_dir(from_file));
            bases.push(std::path::PathBuf::new());
        }
        let mut out = Vec::new();
        for base in bases {
            if rest.is_empty() {
                out.push(base.join("__init__.py"));
            } else {
                out.push(base.join(format!("{rest}.py")));
                out.push(base.join(&rest).join("__init__.py"));
            }
        }
        out
    }

    fn resolve(&self, from_file: &str, r: &crate::model::NodeRef) -> Result<usize, Miss> {
        use crate::model::NodeRef;
        match r {
            NodeRef::FunctionName(name) => self
                .in_file(std::path::Path::new(from_file), name)
                .map_or_else(|| self.unique(name), Ok),
            NodeRef::Canonical(s) => {
                if let Some(&i) = self.by_id.get(s) {
                    return Ok(i);
                }
                let Some((path, name)) = s.rsplit_once(':') else {
                    return self.unique(s);
                };
                let rel = file_dir(from_file).join(path);
                self.in_file(std::path::Path::new(path), name)
                    .or_else(|| self.in_file(&rel, name))
                    .map_or_else(|| self.unique(name), Ok)
            }
            NodeRef::Imported { module, name } => {
                let files = Self::module_files(from_file, module);
                if let Some(i) = files.iter().find_map(|f| self.in_file(f, name)) {
                    return Ok(i);
                }
                match self.unique(name) {
                    Err(Miss::NotFound) => Err(Miss::NotInModule {
                        module: module.clone(),
                        file: norm_path(&files[0]),
                    }),
                    other => other,
                }
            }
        }
    }

    /// `resolve`, as the DagError for input `param` of `node`.
    fn resolve_input(
        &self,
        node: &ExtractedNode,
        param: &str,
        r: &crate::model::NodeRef,
    ) -> Result<usize, DagError> {
        self.resolve(&node.source_file, r).map_err(|miss| {
            let node_id = node.continuity_key();
            let upstream = match r {
                crate::model::NodeRef::Canonical(s) => s.clone(),
                other => other.resolution_name().to_string(),
            };
            match miss {
                Miss::NotFound => DagError::UnresolvedInput {
                    node: node_id,
                    param: param.to_string(),
                    upstream,
                },
                Miss::Ambiguous(ids) => DagError::AmbiguousInput {
                    node: node_id,
                    param: param.to_string(),
                    upstream,
                    candidates: ids.join(", "),
                },
                Miss::NotInModule { module, file } => DagError::UnresolvedImport {
                    node: node_id,
                    param: param.to_string(),
                    name: upstream,
                    module,
                    file,
                },
            }
        })
    }
}

impl Dag {
    /// Build a DAG from extracted nodes. Validates all constraints.
    pub fn build(nodes: &[ExtractedNode]) -> Result<Self, DagError> {
        let resolved = resolve_partitions_from(nodes)?;
        let nodes = resolved.as_slice();
        let mut graph = DiGraph::new();
        let mut index: HashMap<String, NodeIndex> = HashMap::new();
        let refs = Resolver::new(nodes);

        // First pass: add all nodes, check for duplicate keys.
        for node in nodes {
            let id = node.continuity_key();

            if let Some(existing_idx) = index.get(&id) {
                let existing: &DagNode = &graph[*existing_idx];
                return Err(DagError::DuplicateKey {
                    key: id,
                    first: existing.source_file().to_string(),
                    second: node.source_file.clone(),
                });
            }

            // Validate: sensors cannot have inputs.
            if node.kind == NodeKind::Sensor && !node.inputs.is_empty() {
                return Err(DagError::SensorWithInputs { sensor: id.clone() });
            }

            // The definition hash: what determines the node's result. `source_text` is the
            // function from `def` on plus the decorator parts that count, in canonical form
            // (`crate::definition::RULES` is the list: inputs, serializer, sinks, ...), and
            // `cone_hash` is the helper code both reach. The kind is the one thing added here.
            let metadata = serde_json::json!({ "kind": node.kind }).to_string();
            let def_hash = hash::definition_hash(&node.source_text, &node.cone_hash, &metadata);

            let dag_node = DagNode {
                id: id.clone(),
                extracted: node.clone(),
                resolved_inputs: HashMap::new(),
                resolved_collected: HashMap::new(),
                definition_hash: def_hash,
            };

            let idx = graph.add_node(dag_node);
            index.insert(id, idx);
        }

        // Second pass: add edges, resolve inputs.
        for node in nodes {
            let downstream_key = node.continuity_key();
            let downstream_idx = index[&downstream_key];

            for input in &node.inputs {
                let upstream_key =
                    &refs.ids[refs.resolve_input(node, &input.param_name, &input.upstream)?];

                let upstream_idx = index[upstream_key.as_str()];

                // Validate: tasks cannot be an input to an asset or sensor.
                // (Tasks always re-run and never cache, so feeding a task's output
                // into a cacheable node would make that node perpetually stale.)
                if graph[upstream_idx].kind() == NodeKind::Task {
                    let downstream_kind = match node.kind {
                        NodeKind::Asset => Some("asset"),
                        NodeKind::Sensor => Some("sensor"),
                        NodeKind::Task => None,
                    };
                    if let Some(downstream_kind) = downstream_kind {
                        return Err(DagError::TaskAsInput {
                            task: upstream_key.clone(),
                            downstream: downstream_key.clone(),
                            downstream_kind,
                        });
                    }
                }

                // A partitioned upstream read whole by an unpartitioned consumer: say which of
                // the two meanings was intended instead of guessing one (#189).
                if !input.collected
                    && node.partitions.is_empty()
                    && !graph[upstream_idx].extracted.partitions.is_empty()
                {
                    return Err(DagError::PartitionedInputToUnpartitioned {
                        node: downstream_key.clone(),
                        param: input.param_name.clone(),
                        upstream: upstream_key.clone(),
                    });
                }

                let edge_kind = if input.collected {
                    EdgeKind::Collect
                } else {
                    EdgeKind::Direct
                };
                graph.add_edge(upstream_idx, downstream_idx, edge_kind);

                // Record the resolved mapping on the node.
                if input.collected {
                    graph[downstream_idx]
                        .resolved_collected
                        .insert(input.param_name.clone(), upstream_key.clone());
                } else {
                    graph[downstream_idx]
                        .resolved_inputs
                        .insert(input.param_name.clone(), upstream_key.clone());
                }
            }

            // Add partition_source edges for partitions_from.
            for spec in node.partitions.values() {
                if let crate::model::PartitionSpec::DerivedFrom { source_ref } = spec
                    && let Ok(j) = refs.resolve(&node.source_file, source_ref)
                {
                    let source_idx = index[refs.ids[j].as_str()];
                    graph.add_edge(source_idx, downstream_idx, EdgeKind::PartitionSource);
                }
            }
        }

        let dag = Dag { graph, index };

        // Verify acyclicity.
        if toposort(&dag.graph, None).is_err() {
            return Err(DagError::CycleDetected);
        }

        Ok(dag)
    }

    /// Get the subgraph of all nodes upstream of (and including) target.
    /// Returns node IDs in topological order (dependencies first).
    pub fn subgraph(&self, target_id: &str) -> Vec<&str> {
        self.subgraph_many(&[target_id])
    }

    /// The union of the subgraphs of several targets: every node upstream of (and including)
    /// any of them, each once, in topological order. Unknown ids are ignored.
    pub fn subgraph_many(&self, target_ids: &[&str]) -> Vec<&str> {
        // BFS backwards from the targets to find all ancestors.
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        for id in target_ids {
            if let Some(&idx) = self.index.get(*id)
                && visited.insert(idx)
            {
                queue.push_back(idx);
            }
        }
        if queue.is_empty() {
            return vec![];
        }

        while let Some(idx) = queue.pop_front() {
            for pred in self.graph.neighbors_directed(idx, Direction::Incoming) {
                if visited.insert(pred) {
                    queue.push_back(pred);
                }
            }
        }

        // Return in topo order (filtered to subgraph).
        let sorted = toposort(&self.graph, None).expect("verified acyclic");
        sorted
            .into_iter()
            .filter(|idx| visited.contains(idx))
            .map(|idx| self.graph[idx].id.as_str())
            .collect()
    }

    /// Topologically sorted node IDs.
    pub fn topo_order(&self) -> Vec<&str> {
        let sorted = toposort(&self.graph, None).expect("verified acyclic");
        sorted
            .iter()
            .map(|idx| self.graph[*idx].id.as_str())
            .collect()
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: &str) -> Option<&DagNode> {
        self.index.get(id).map(|idx| &self.graph[*idx])
    }

    /// Get upstream node IDs.
    pub fn upstream(&self, id: &str) -> Vec<&str> {
        let Some(&idx) = self.index.get(id) else {
            return vec![];
        };
        self.graph
            .neighbors_directed(idx, Direction::Incoming)
            .map(|pred| self.graph[pred].id.as_str())
            .collect()
    }

    /// Get downstream node IDs.
    pub fn downstream(&self, id: &str) -> Vec<&str> {
        let Some(&idx) = self.index.get(id) else {
            return vec![];
        };
        self.graph
            .neighbors_directed(idx, Direction::Outgoing)
            .map(|succ| self.graph[succ].id.as_str())
            .collect()
    }

    /// Get upstream node IDs, excluding PartitionSource and Collect edges.
    /// Used by the planner for chain detection — partition source deps should
    /// force phase breaks, not chain bundling (and pass no data). Collect
    /// (fan-in) deps must also force a phase break: a `collect()` consumer
    /// has to wait for *every* partition of its upstream to finish, which a
    /// fused single-succ/single-pred chain cannot guarantee (see #97).
    pub fn execution_upstream(&self, id: &str) -> Vec<&str> {
        let Some(&idx) = self.index.get(id) else {
            return vec![];
        };
        let mut result = Vec::new();
        for edge in self.graph.edges_directed(idx, Direction::Incoming) {
            if !matches!(
                *edge.weight(),
                EdgeKind::PartitionSource | EdgeKind::Collect
            ) {
                let source_idx = edge.source();
                result.push(self.graph[source_idx].id.as_str());
            }
        }
        result
    }

    /// Get downstream node IDs, excluding PartitionSource and Collect edges.
    pub fn execution_downstream(&self, id: &str) -> Vec<&str> {
        let Some(&idx) = self.index.get(id) else {
            return vec![];
        };
        let mut result = Vec::new();
        for edge in self.graph.edges_directed(idx, Direction::Outgoing) {
            if !matches!(
                *edge.weight(),
                EdgeKind::PartitionSource | EdgeKind::Collect
            ) {
                let target_idx = edge.target();
                result.push(self.graph[target_idx].id.as_str());
            }
        }
        result
    }

    /// Node count.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Edge count.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> ExtractedNode {
        ExtractedNode {
            kind: NodeKind::Asset,
            function_name: name.to_string(),
            explicit_name: None,
            freshness: crate::model::Freshness::Always,
            inputs: smallvec::SmallVec::new(),
            partitions: HashMap::new(),
            sinks: smallvec::SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: "def a(): return 1".to_string(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
            unused_inputs: Vec::new(),
        }
    }

    fn build_files(files: &[(&str, &str)]) -> Result<Dag, DagError> {
        let mut nodes = Vec::new();
        for (path, src) in files {
            nodes.extend(crate::parse::extract_nodes(src, path).unwrap());
        }
        Dag::build(&nodes)
    }

    fn input_of(dag: &Dag, node: &str, param: &str) -> String {
        dag.get_node(node)
            .unwrap_or_else(|| panic!("no node {node}"))
            .resolved_inputs
            .get(param)
            .unwrap_or_else(|| panic!("{node} has no input {param}"))
            .clone()
    }

    const SRC_ASSET: &str =
        "from barca import asset\n\n@asset()\ndef src() -> int:\n    return 1\n";

    #[test]
    fn a_name_defined_in_the_same_file_wins_over_other_files() {
        for order in [["a.py", "b.py"], ["b.py", "a.py"]] {
            let a = format!(
                "{SRC_ASSET}\n@asset(inputs={{\"s\": src}})\ndef out(s: int) -> int:\n    return s\n"
            );
            let files: Vec<(&str, &str)> = order
                .iter()
                .map(|f| {
                    if *f == "a.py" {
                        ("a.py", a.as_str())
                    } else {
                        ("b.py", SRC_ASSET)
                    }
                })
                .collect();
            let dag = build_files(&files).unwrap();
            assert_eq!(
                input_of(&dag, "a.py:out", "s"),
                "a.py:src",
                "order {order:?}"
            );
        }
    }

    #[test]
    fn an_imported_name_resolves_to_the_module_it_is_imported_from() {
        let cases = [
            (
                "pipelines/reconcile.py",
                "from pipelines.sources import src\n",
                "src",
            ),
            (
                "pipelines/reconcile.py",
                "from .sources import src\n",
                "src",
            ),
            ("pipelines/reconcile.py", "from sources import src\n", "src"),
            (
                "pipelines/reconcile.py",
                "from pipelines.sources import src as up\n",
                "up",
            ),
            (
                "pipelines/reconcile.py",
                "import pipelines.sources as s\n",
                "s.src",
            ),
            (
                "pipelines/reconcile.py",
                "import pipelines.sources\n",
                "pipelines.sources.src",
            ),
            (
                "pipelines/deep/reconcile.py",
                "from ..sources import src\n",
                "src",
            ),
        ];
        for (path, import, expr) in cases {
            let consumer = format!(
                "from barca import asset\n{import}\n@asset(inputs={{\"s\": {expr}}})\ndef out(s: int) -> int:\n    return s\n"
            );
            // A decoy with the same function name elsewhere must not be picked.
            let dag = build_files(&[
                ("other/sources.py", SRC_ASSET),
                ("pipelines/sources.py", SRC_ASSET),
                (path, consumer.as_str()),
            ])
            .unwrap_or_else(|e| panic!("{import}: {e}"));
            assert_eq!(
                input_of(&dag, &format!("{path}:out"), "s"),
                "pipelines/sources.py:src",
                "{import}"
            );
        }
    }

    #[test]
    fn collect_of_an_imported_attribute_resolves() {
        let up = "from barca import asset, partitions\n\n@asset(partitions={\"k\": partitions([\"a\", \"b\"])})\ndef src(k: str) -> int:\n    return 1\n";
        let consumer = "from barca import asset, collect\nimport up\n\n@asset(inputs={\"s\": collect(up.src)})\ndef total(s: list) -> int:\n    return sum(s)\n";
        let dag = build_files(&[("up.py", up), ("c.py", consumer)]).unwrap();
        let n = dag.get_node("c.py:total").unwrap();
        assert_eq!(
            n.resolved_collected.get("s").map(String::as_str),
            Some("up.py:src")
        );
    }

    #[test]
    fn an_imported_name_that_is_not_a_node_there_is_an_error() {
        let helper = "def src():\n    return 1\n";
        let consumer = "from barca import asset\nfrom helpers import src\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let err = build_files(&[("helpers.py", helper), ("c.py", consumer)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'src' is imported from helpers"), "{msg}");
        assert!(msg.contains("no @asset/@task/@sensor named 'src'"), "{msg}");
    }

    #[test]
    fn a_reexported_name_falls_back_to_the_unique_node_with_that_name() {
        // `from pkg import src` where pkg/__init__.py re-exports it from pkg/impl.py.
        let consumer = "from barca import asset\nfrom pkg import src\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let dag = build_files(&[("pkg/impl.py", SRC_ASSET), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:out", "s"), "pkg/impl.py:src");
    }

    #[test]
    fn a_bare_name_defined_in_several_other_files_is_ambiguous() {
        let consumer = "from barca import asset\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let err = build_files(&[("a.py", SRC_ASSET), ("b.py", SRC_ASSET), ("c.py", consumer)])
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("a.py:src") && msg.contains("b.py:src"),
            "{msg}"
        );
        assert!(msg.contains("asset_ref"), "{msg}");
    }

    #[test]
    fn a_bare_name_defined_once_elsewhere_still_resolves() {
        let consumer = "from barca import asset\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let dag = build_files(&[("a.py", SRC_ASSET), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:out", "s"), "a.py:src");
    }

    #[test]
    fn asset_ref_picks_the_named_file() {
        let consumer = "from barca import asset, asset_ref\n\n@asset(inputs={\"s\": asset_ref(\"b.py:src\")})\ndef out(s: int) -> int:\n    return s\n";
        let dag =
            build_files(&[("a.py", SRC_ASSET), ("b.py", SRC_ASSET), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:out", "s"), "b.py:src");
        // A path relative to the referencing file's directory also works.
        let nested = "from barca import asset, asset_ref\n\n@asset(inputs={\"s\": asset_ref(\"b.py:src\")})\ndef out(s: int) -> int:\n    return s\n";
        let dag = build_files(&[
            ("p/b.py", SRC_ASSET),
            ("q/b.py", SRC_ASSET),
            ("p/c.py", nested),
        ])
        .unwrap();
        assert_eq!(input_of(&dag, "p/c.py:out", "s"), "p/b.py:src");
    }

    #[test]
    fn partitions_from_an_imported_asset_resolves_to_that_file() {
        let up = "from barca import asset, partitions\n\n@asset(partitions={\"k\": partitions([\"a\", \"b\"])})\ndef src(k: str) -> int:\n    return 1\n";
        let consumer = "from barca import asset, partitions_from\nfrom up import src\n\n@asset(partitions={\"k\": partitions_from(src)})\ndef per(k: str, src: int) -> int:\n    return src\n";
        let dag = build_files(&[("decoy.py", up), ("up.py", up), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:per", "src"), "up.py:src");
    }

    fn build_src(src: &str) -> Result<Dag, DagError> {
        Dag::build(&crate::parse::extract_nodes(src, "t.py").unwrap())
    }

    const SALES: &str = "from barca import asset, collect, partitions, partitions_from\n\n\
@asset(partitions={\"region\": partitions([\"emea\", \"amer\"])})\n\
def sales(region: str) -> dict:\n    return {}\n\n";

    #[test]
    fn partitions_from_a_partitioned_asset_takes_its_keys_and_reads_it_per_key() {
        let dag = build_src(&format!(
            "{SALES}@asset(partitions={{\"region\": partitions_from(sales)}})\n\
def margin(region: str, sales: dict) -> dict:\n    return sales\n\n\
@asset(partitions={{\"region\": partitions_from(margin)}})\n\
def pct(region: str, margin: dict) -> dict:\n    return margin\n"
        ))
        .unwrap();
        let sales = &dag.get_node("t.py:sales").unwrap().extracted.partitions;
        for (id, upstream) in [("t.py:margin", "sales"), ("t.py:pct", "margin")] {
            let n = dag.get_node(id).unwrap();
            assert_eq!(&n.extracted.partitions, sales, "{id} has the keys of sales");
            assert_eq!(
                n.resolved_inputs.get(upstream).map(String::as_str),
                Some(format!("t.py:{upstream}").as_str()),
                "{id} reads {upstream} under its name"
            );
        }
    }

    #[test]
    fn partitions_from_an_inputs_entry_names_the_parameter() {
        let dag = build_src(&format!(
            "{SALES}@asset(inputs={{\"s\": sales}}, partitions={{\"region\": partitions_from(sales)}})\n\
def margin(region: str, s: dict) -> dict:\n    return s\n"
        ))
        .unwrap();
        let n = dag.get_node("t.py:margin").unwrap();
        assert_eq!(n.resolved_inputs.len(), 1);
        assert_eq!(n.resolved_inputs["s"], "t.py:sales");
    }

    #[test]
    fn partitions_from_a_list_asset_is_unchanged() {
        let dag = build_src(
            "from barca import asset, partitions_from\n\n\
@asset()\ndef keys() -> list:\n    return []\n\n\
@asset(partitions={\"k\": partitions_from(keys)})\ndef part(k: str) -> str:\n    return k\n",
        )
        .unwrap();
        let n = dag.get_node("t.py:part").unwrap();
        assert!(n.resolved_inputs.is_empty());
        assert!(matches!(
            n.extracted.partitions["k"],
            crate::model::PartitionSpec::DerivedFrom { .. }
        ));
    }

    #[test]
    fn partitions_from_under_another_dimension_name_is_an_error() {
        let err = build_src(&format!(
            "{SALES}@asset(partitions={{\"r\": partitions_from(sales)}})\n\
def margin(r: str, sales: dict) -> dict:\n    return sales\n"
        ))
        .unwrap_err();
        assert!(matches!(err, DagError::PartitionsFrom { .. }), "{err}");
        assert!(err.to_string().contains("'region'"), "{err}");
        assert!(
            err.remediation()
                .unwrap()
                .contains("partitions_from(sales)")
        );
    }

    #[test]
    fn partitions_from_needs_one_dimension_on_both_sides() {
        let several = build_src(
            "from barca import asset, partitions, partitions_from\n\n\
@asset(partitions={\"a\": partitions([\"x\"]), \"b\": partitions([\"y\"])})\n\
def grid(a: str, b: str) -> str:\n    return a\n\n\
@asset(partitions={\"a\": partitions_from(grid)})\n\
def down(a: str, grid: str) -> str:\n    return grid\n",
        )
        .unwrap_err();
        assert!(
            several
                .to_string()
                .contains("2 partition dimensions (a, b)"),
            "{several}"
        );
        let mixed = build_src(&format!(
            "{SALES}@asset(partitions={{\"region\": partitions_from(sales), \"tier\": partitions([\"1\"])}})\n\
def margin(region: str, tier: str, sales: dict) -> dict:\n    return sales\n"
        ))
        .unwrap_err();
        assert!(
            mixed.to_string().contains("only partition dimension"),
            "{mixed}"
        );
    }

    #[test]
    fn partitioned_input_to_unpartitioned_consumer_is_an_error() {
        let err = build_src(&format!(
            "{SALES}@asset(inputs={{\"xs\": sales}})\ndef total(xs: list) -> int:\n    return 0\n"
        ))
        .unwrap_err();
        assert!(
            matches!(err, DagError::PartitionedInputToUnpartitioned { .. }),
            "{err}"
        );
        let fix = err.remediation().unwrap();
        assert!(
            fix.contains("collect(sales)") && fix.contains("partitions_from(sales)"),
            "{fix}"
        );
        // collect() is the way to read every partition.
        build_src(&format!(
            "{SALES}@asset(inputs={{\"xs\": collect(sales)}})\ndef total(xs: list) -> int:\n    return 0\n"
        ))
        .unwrap();
    }

    #[test]
    fn subgraph_many_is_the_union_of_cones_with_shared_upstream_once() {
        let src = "from barca import asset\n\n\
@asset()\ndef src() -> int:\n    return 1\n\n\
@asset(inputs={\"s\": src})\ndef a(s: int) -> int:\n    return s\n\n\
@asset(inputs={\"s\": src})\ndef b(s: int) -> int:\n    return s\n\n\
@asset()\ndef other() -> int:\n    return 2\n";
        let nodes = crate::parse::extract_nodes(src, "t.py").unwrap();
        let dag = Dag::build(&nodes).unwrap();
        let union = dag.subgraph_many(&["t.py:a", "t.py:b"]);
        assert_eq!(union.len(), 3);
        assert_eq!(union[0], "t.py:src", "dependencies come first");
        assert!(union.contains(&"t.py:a") && union.contains(&"t.py:b"));
        assert!(!union.contains(&"t.py:other"));
        assert_eq!(dag.subgraph("t.py:a"), dag.subgraph_many(&["t.py:a"]));
        assert!(dag.subgraph_many(&["t.py:nope"]).is_empty());
    }

    /// The definition hash of `a` in a one-file pipeline, through the parser.
    fn definition_hash_of(source: &str) -> String {
        let nodes = crate::parse::extract_nodes(source, "test.py").unwrap();
        let dag = Dag::build(&nodes).unwrap();
        dag.get_node("test.py:a").unwrap().definition_hash.clone()
    }

    const A: &str = "def a():\n    return 1\n";

    // Cached steps never reach a worker, so a new or edited sink has to change the hash, or it
    // would silently not be written.
    #[test]
    fn definition_hash_changes_when_sink_added() {
        assert_ne!(
            definition_hash_of(&format!("@asset()\n{A}")),
            definition_hash_of(&format!("@asset()\n@sink(\"exports/a.parquet\")\n{A}")),
        );
    }

    #[test]
    fn definition_hash_changes_when_sink_edited() {
        let hash = |sink: &str| definition_hash_of(&format!("@asset()\n@sink({sink})\n{A}"));
        assert_ne!(
            hash("\"exports/a.parquet\""),
            hash("\"exports/a.parquet\", serializer=\"pickle\""),
        );
        assert_ne!(hash("\"exports/a.parquet\""), hash("\"exports/b.parquet\""));
    }

    #[test]
    fn definition_hash_changes_when_serializer_changed() {
        assert_ne!(
            definition_hash_of(&format!("@asset()\n{A}")),
            definition_hash_of(&format!("@asset(serializer=\"parquet\")\n{A}")),
        );
    }

    /// The definition hash is over `source_text` (the canonical definition, see
    /// `crate::definition`), the cone and the kind. The fields the parser reads from arguments
    /// that do not count (`freshness`, `retries`, `description`, ...) are not part of it.
    #[test]
    fn definition_hash_ignores_what_does_not_count() {
        let plain = node("a");
        let mut other = node("a");
        other.freshness = crate::model::Freshness::Manual;
        other.retries = 5;
        other.retry_backoff_seconds = 2.0;
        other.timeout_seconds = 10;
        other.description = Some("d".to_string());
        other.tags.insert("team".to_string(), "a".to_string());
        other.env.push("HOME".to_string());
        let d1 = Dag::build(std::slice::from_ref(&plain)).unwrap();
        let d2 = Dag::build(std::slice::from_ref(&other)).unwrap();
        assert_eq!(
            d1.get_node("test.py:a").unwrap().definition_hash,
            d2.get_node("test.py:a").unwrap().definition_hash,
        );

        let mut sensor = node("a");
        sensor.kind = NodeKind::Sensor;
        let d3 = Dag::build(std::slice::from_ref(&sensor)).unwrap();
        assert_ne!(
            d1.get_node("test.py:a").unwrap().definition_hash,
            d3.get_node("test.py:a").unwrap().definition_hash,
            "the kind counts"
        );
    }

    /// Issue #283, end to end through the parser: decorator edits that do not change the
    /// result leave the definition hash alone; edits that can change it do not.
    #[test]
    fn definition_hash_covers_the_result_and_nothing_else() {
        let base = definition_hash_of(&format!(
            "@asset(partitions={{\"region\": partitions([\"us\", \"eu\"])}})\n{A}"
        ));
        for same in [
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\", \"apac\"])})\n",
            "@asset(partitions={\"region\": partitions([\"eu\"])})\n",
            "@asset(partitions={\"region\": partitions([\"eu\", \"us\"])})\n",
            "@asset(partitions={\"region\": partitions(REGIONS)})\n",
            "@asset(partitions={\"region\": partitions(regions())})\n",
            "@asset(partitions={\"region\": partitions([r for r in REGIONS])})\n",
            "@asset(\n    partitions = {'region': partitions(['us', 'eu',]),},  # keys\n)\n",
            "@asset(description=\"d\", partitions={\"region\": partitions([\"us\", \"eu\"])})\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, tags={\"a\": \"b\"}, retries=3)\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, freshness=Manual)\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, timeout_seconds=5, retry_backoff=1.5)\n",
        ] {
            assert_eq!(base, definition_hash_of(&format!("{same}{A}")), "{same}");
        }
        for different in [
            "@asset(partitions={\"area\": partitions([\"us\", \"eu\"])})\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, serializer=\"pickle\")\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})\n@sink(\"a.json\")\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})\n@functools.cache\n",
            "@asset()\n",
        ] {
            assert_ne!(
                base,
                definition_hash_of(&format!("{different}{A}")),
                "{different}"
            );
        }
        assert_ne!(
            base,
            definition_hash_of(
                "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})\ndef a():\n    return 2\n"
            ),
            "the body counts"
        );
    }
}
