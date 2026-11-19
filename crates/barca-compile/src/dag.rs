//! The graph: nodes ([`DagNode`]), the edges between them and the groups over them, validated
//! and acyclic, with the traversals planning needs.
//!
//! This is the intermediate representation's container. It is built from nodes whose inputs
//! are already resolved to node ids ([`Dag::from_parts`]) and knows nothing about where they
//! came from: the Python frontend's last step ([`crate::link`]) is its only producer today.
//! [`DagParts`] is the same graph as plain data.

use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use barca_model::ir::{DagNode, EdgeKind, NodeGroup, NodeKind};

pub use barca_model::dag_error::DagError;

/// The constructed DAG — validated, acyclic, ready for plan generation.
#[derive(Debug, Clone)]
pub struct Dag {
    pub groups: Vec<NodeGroup>,
    pub graph: DiGraph<DagNode, EdgeKind>,
    index: HashMap<String, NodeIndex>,
    names: HashMap<String, Vec<NodeIndex>>,
    /// Nodes that are in the graph but cannot run, by id, each with the whole message a
    /// command that plans it stops with ([`Dag::check_imports`]). The frontend knows why (for
    /// Python: the node's file imports a project module that does not parse, #550); the graph
    /// only refuses.
    pub cannot_run: HashMap<String, String>,
    /// Not part of the graph: a by-product of loading it, kept here until what `load` returns
    /// has a place for it (`docs/engineering/ir.md`, "What is left"). Every project file
    /// loading looked at besides the pipeline files: the helper modules it read, and the paths
    /// it probed and found missing (a module created there later changes what an import
    /// resolves to). With the pipeline files, these are the files whose edit can change this
    /// graph's run hashes; `barca serve` keeps a graph until one of them changes. Dynamic
    /// partition keys are not covered: they can depend on anything.
    pub files_read: Vec<std::path::PathBuf>,
}

/// One edge of a [`DagParts`]: positions in its `nodes`, upstream first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
}

/// A graph as plain data: what a frontend supplies and what [`Dag::from_parts`] validates.
/// The order of `nodes` and of `edges` is kept by the graph (plans list steps in an order
/// derived from both), so a graph taken apart with [`Dag::to_parts`] and put back plans the
/// same. The derives are internal; this is not a file format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DagParts {
    pub nodes: Vec<DagNode>,
    pub edges: Vec<Edge>,
    pub groups: Vec<NodeGroup>,
    /// See [`Dag::cannot_run`].
    pub cannot_run: HashMap<String, String>,
}

/// The rules of one data edge (an input or a `collect()`), which hold in any language.
/// `param` is called only to word an error.
pub(crate) fn check_data_edge<'a>(
    upstream: (&str, NodeKind, bool),
    downstream: (&str, NodeKind, bool),
    collected: bool,
    param: impl FnOnce() -> &'a str,
) -> Result<(), DagError> {
    let (upstream_id, upstream_kind, upstream_partitioned) = upstream;
    let (downstream_id, downstream_kind, downstream_partitioned) = downstream;
    // Tasks cannot be an input to an asset or sensor. (Tasks always re-run and never cache, so
    // feeding a task's output into a cacheable node would make that node perpetually stale.)
    if upstream_kind == NodeKind::Task {
        let downstream_kind = match downstream_kind {
            NodeKind::Asset => Some("asset"),
            NodeKind::Sensor => Some("sensor"),
            NodeKind::Task => None,
        };
        if let Some(downstream_kind) = downstream_kind {
            return Err(DagError::TaskAsInput {
                task: upstream_id.to_string(),
                downstream: downstream_id.to_string(),
                downstream_kind,
            });
        }
    }
    // A partitioned upstream read whole by an unpartitioned consumer: say which of the two
    // meanings was intended instead of guessing one (#189).
    if !collected && !downstream_partitioned && upstream_partitioned {
        return Err(DagError::PartitionedInputToUnpartitioned {
            node: downstream_id.to_string(),
            param: param().to_string(),
            upstream: upstream_id.to_string(),
        });
    }
    Ok(())
}

impl Dag {
    fn name_index(graph: &DiGraph<DagNode, EdgeKind>) -> HashMap<String, Vec<NodeIndex>> {
        let mut names: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        for idx in graph.node_indices() {
            names
                .entry(crate::names::short_name(&graph[idx].id).to_owned())
                .or_default()
                .push(idx);
        }
        names
    }

    pub(crate) fn named(&self, name: &str) -> Vec<&DagNode> {
        self.names
            .get(crate::names::short_name(name))
            .into_iter()
            .flatten()
            .map(|&idx| &self.graph[idx])
            .filter(|node| crate::names::target_name_matches(&node.id, name))
            .collect()
    }

    /// Topologically order only these nodes, visiting their incoming edges once.
    pub fn order_for<'a, 'b>(&'a self, ids: impl IntoIterator<Item = &'b str>) -> Vec<&'a str> {
        use std::collections::{BTreeSet, HashSet};
        let nodes: HashSet<_> = ids
            .into_iter()
            .filter_map(|id| self.index.get(id).copied())
            .collect();
        let mut remaining = HashMap::new();
        let mut dependents: HashMap<NodeIndex, Vec<NodeIndex>> = HashMap::new();
        let mut ready = BTreeSet::new();
        for &idx in &nodes {
            let mut count = 0;
            for edge in self.graph.edges_directed(idx, Direction::Incoming) {
                if nodes.contains(&edge.source()) {
                    count += 1;
                    dependents.entry(edge.source()).or_default().push(idx);
                }
            }
            remaining.insert(idx, count);
            if count == 0 {
                ready.insert(idx);
            }
        }
        let mut ordered = Vec::with_capacity(nodes.len());
        while let Some(idx) = ready.pop_last() {
            ordered.push(self.graph[idx].id.as_str());
            for next in dependents.get(&idx).into_iter().flatten() {
                let count = remaining.get_mut(next).unwrap();
                *count -= 1;
                if *count == 0 {
                    ready.insert(*next);
                }
            }
        }
        debug_assert_eq!(ordered.len(), nodes.len());
        ordered
    }

    /// Build the graph from nodes and edges, checking every rule that does not depend on the
    /// language the nodes were written in: ids are unique, an edge joins two of the nodes, a
    /// sensor has no inputs, a task feeds no asset or sensor, a partitioned node is read whole
    /// only through `collect()`, and there is no cycle.
    pub fn from_parts(parts: DagParts) -> Result<Self, DagError> {
        let DagParts {
            nodes,
            edges,
            groups,
            cannot_run,
        } = parts;
        let mut graph = DiGraph::with_capacity(nodes.len(), edges.len());
        let mut index: HashMap<String, NodeIndex> = HashMap::with_capacity(nodes.len());
        for node in nodes {
            if let Some(existing) = index.get(&node.id) {
                let existing: &DagNode = &graph[*existing];
                return Err(DagError::DuplicateKey {
                    key: node.id.clone(),
                    first: existing.source_file().to_string(),
                    second: node.source_file().to_string(),
                });
            }
            if node.decl.kind == NodeKind::Sensor
                && !(node.resolved_inputs.is_empty() && node.resolved_collected.is_empty())
            {
                return Err(DagError::SensorWithInputs { sensor: node.id });
            }
            let id = node.id.clone();
            let idx = graph.add_node(node);
            index.insert(id, idx);
        }
        for edge in edges {
            if edge.from >= graph.node_count() || edge.to >= graph.node_count() {
                return Err(DagError::Malformed(format!(
                    "an edge joins nodes {} and {} of {}",
                    edge.from,
                    edge.to,
                    graph.node_count()
                )));
            }
            let (from, to) = (NodeIndex::new(edge.from), NodeIndex::new(edge.to));
            let collected = match edge.kind {
                EdgeKind::Direct => Some(false),
                EdgeKind::Collect => Some(true),
                EdgeKind::PartitionSource => None,
            };
            if let Some(collected) = collected {
                let (upstream, downstream): (&DagNode, &DagNode) = (&graph[from], &graph[to]);
                check_data_edge(
                    (
                        &upstream.id,
                        upstream.decl.kind,
                        !upstream.decl.partitions.is_empty(),
                    ),
                    (
                        &downstream.id,
                        downstream.decl.kind,
                        !downstream.decl.partitions.is_empty(),
                    ),
                    collected,
                    || {
                        downstream
                            .resolved_inputs
                            .iter()
                            .find(|(_, id)| **id == upstream.id)
                            .map_or("", |(param, _)| param.as_str())
                    },
                )?;
            }
            graph.add_edge(from, to, edge.kind);
        }

        let names = Self::name_index(&graph);
        let dag = Dag {
            graph,
            index,
            names,
            groups,
            cannot_run,
            files_read: Vec::new(),
        };

        // Verify acyclicity.
        if toposort(&dag.graph, None).is_err() {
            return Err(DagError::CycleDetected);
        }

        Ok(dag)
    }

    /// The graph as plain data: its nodes and edges in the order it holds them, so that
    /// [`Dag::from_parts`] gives back a graph that plans identically.
    pub fn to_parts(&self) -> DagParts {
        DagParts {
            nodes: self.graph.node_weights().cloned().collect(),
            edges: self
                .graph
                .edge_references()
                .map(|e| Edge {
                    from: e.source().index(),
                    to: e.target().index(),
                    kind: *e.weight(),
                })
                .collect(),
            groups: self.groups.clone(),
            cannot_run: self.cannot_run.clone(),
        }
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

        self.order_for(visited.into_iter().map(|idx| self.graph[idx].id.as_str()))
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

    /// Refuse to plan a step that cannot run ([`Dag::cannot_run`]): the worker would fail on
    /// it after the steps before it had run. Only the steps given are looked at, so a command
    /// that plans none of them is not affected. The first such step, in the order given, is
    /// named.
    pub fn check_imports<'a>(
        &self,
        step_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), barca_model::BarcaError> {
        if self.cannot_run.is_empty() {
            return Ok(());
        }
        for id in step_ids {
            if let Some(why) = self.cannot_run.get(id) {
                return Err(barca_model::BarcaError::Parse(why.clone()));
            }
        }
        Ok(())
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

    /// The nodes whose output `id`'s `partitions_from(...)` reads for its keys.
    pub fn partition_sources(&self, id: &str) -> Vec<&str> {
        let Some(&idx) = self.index.get(id) else {
            return vec![];
        };
        self.graph
            .edges_directed(idx, Direction::Incoming)
            .filter(|e| *e.weight() == EdgeKind::PartitionSource)
            .map(|e| self.graph[e.source()].id.as_str())
            .collect()
    }

    /// Whether a `partitions_from(...)` reads `id`'s output for its keys.
    pub fn feeds_partitions(&self, id: &str) -> bool {
        let Some(&idx) = self.index.get(id) else {
            return false;
        };
        self.graph
            .edges_directed(idx, Direction::Outgoing)
            .any(|e| *e.weight() == EdgeKind::PartitionSource)
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

    /// Node count.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Edge count.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

/// Whether the output of `id` (a base id) is folded into its consumers' run hashes, not only
/// its run hash: a sensor (#183), and a `Manual` asset (#548). A `Manual` asset is read as it
/// is and refreshed by hand with the same code and inputs, so its run hash stays the same when
/// a refresh gives new bytes; its output is what tells what reads it that it changed.
///
/// And an `@asset(cutoff=True)` (#628, `barca_service::cache::is_cutoff`), whose output replaces its run hash.
/// Either way the node ends its chain in the planner and its consumers are decided in a later
/// phase, once its output is known.
pub fn folds_output(dag: &Dag, id: &str) -> bool {
    dag.get_node(id).is_some_and(|n| match n.kind() {
        barca_model::NodeKind::Sensor => true,
        barca_model::NodeKind::Asset => {
            n.decl.freshness == barca_model::Freshness::Manual || n.decl.cutoff
        }
        barca_model::NodeKind::Task => false,
    })
}
