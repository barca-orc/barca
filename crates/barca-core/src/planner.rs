//! Execution planner — transforms a DAG into a phased, multi-stream execution plan.
//!
//! Flow: Dag → decompose() → Topology → plan() → ExecutionPlan
//!
//! 1. decompose: identify chains, fan-out points, fan-in points
//! 2. plan: assign chains to phases and streams based on topology + resource config

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::dag::Dag;
use crate::model::{PartitionKey, StepId};

/// Resource configuration — the knobs the user controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceConfig {
    /// Max concurrent workers (default: cpu_count).
    pub pool_size: usize,
    /// Per-group concurrency limits from `tags={"concurrency_group": "..."}`.
    pub concurrency_groups: HashMap<String, usize>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Output types
// ═══════════════════════════════════════════════════════════════════════════════

/// A complete execution plan — phased, with worker stream assignments.
#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub phases: Vec<Phase>,
    pub total_steps: usize,
}

/// A phase — all streams execute concurrently; all must complete before next phase.
#[derive(Debug, Clone)]
pub struct Phase {
    pub reason: PhaseReason,
    pub streams: Vec<WorkerStream>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseReason {
    Initial,
    FanIn { node_id: String },
}

/// A worker stream — ordered steps for one Python process.
#[derive(Debug, Clone)]
pub struct WorkerStream {
    pub stream_id: String,
    pub steps: Vec<StreamStep>,
}

/// A step within a stream.
#[derive(Debug, Clone)]
pub struct StreamStep {
    /// Step identity: base node ID (unpartitioned — workers expand partition_keys).
    pub step_id: StepId,
    /// The kind of node (asset, sensor, task).
    pub kind: crate::model::NodeKind,
    pub function_name: Arc<str>,
    pub source_file: Arc<str>,
    pub inputs: HashMap<String, String>,
    /// Pending partition resolution: dimension → source_node_id.
    /// The dispatch loop reads the source output and expands into N steps.
    pub pending_partitions: HashMap<String, String>,
    /// Explicit artifact serializer from `@asset(serializer="parquet")`.
    pub serializer: Option<Arc<str>>,
    /// Sink outputs from stacked `@sink(...)` decorators.
    pub sinks: Vec<crate::model::SinkDecl>,
    /// Pre-dispatch run hashes: display node id → run_hash (one entry for
    /// unpartitioned steps, one per partition otherwise). Populated by the
    /// executor's cache pass; empty in freshly planned steps.
    pub run_hashes: HashMap<String, String>,
    /// Timeout in seconds for this step's execution.
    pub timeout_seconds: u32,
    /// Total number of attempts on failure (1 = no retry). Rust-side only.
    pub retries: u32,
    /// Base backoff in seconds between attempts; delay = `retry_backoff_seconds * attempt`.
    pub retry_backoff_seconds: f64,
    /// Late-expanded partition keys: workers loop over these internally.
    /// Empty for unpartitioned nodes. Enables 1M+ partitions without 1M structs.
    pub partition_keys: Vec<PartitionKey>,
    /// Parameter frame types from function annotations (param → loader).
    pub param_types: HashMap<String, crate::model::ValueType>,
    /// Return frame type from the function's return annotation.
    pub return_type: Option<crate::model::ValueType>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Intermediate representation: Topology
// ═══════════════════════════════════════════════════════════════════════════════

/// The decomposed structure of a DAG — chains and their relationships.
/// This is the intermediate between raw DAG and execution plan.
#[derive(Debug, Clone)]
pub struct Topology {
    /// All chains (maximal sequential paths). Every node is in exactly one chain.
    pub chains: Vec<Chain>,
    /// Which chains each chain depends on.
    pub chain_deps: HashMap<usize, HashSet<usize>>,
    /// Node → chain index lookup.
    pub node_to_chain: HashMap<String, usize>,
}

/// A chain: maximal path where each internal node has single predecessor and successor.
#[derive(Debug, Clone)]
pub struct Chain {
    /// Ordered node IDs (head to tail).
    pub nodes: Vec<String>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Step 1: Decompose DAG → Topology
// ═══════════════════════════════════════════════════════════════════════════════

/// Decompose a DAG into its topological structure (chains + relationships).
pub fn decompose(dag: &Dag) -> Topology {
    let chains = detect_chains(dag);

    // Build node → chain lookup.
    let mut node_to_chain: HashMap<String, usize> = HashMap::new();
    for (idx, chain) in chains.iter().enumerate() {
        for node_id in &chain.nodes {
            node_to_chain.insert(node_id.clone(), idx);
        }
    }

    // Compute inter-chain dependencies.
    let mut chain_deps: HashMap<usize, HashSet<usize>> = HashMap::new();
    for (idx, chain) in chains.iter().enumerate() {
        let mut deps = HashSet::new();
        for node_id in &chain.nodes {
            for upstream_id in dag.upstream(node_id) {
                let upstream_chain = node_to_chain[upstream_id];
                if upstream_chain != idx {
                    deps.insert(upstream_chain);
                }
            }
        }
        chain_deps.insert(idx, deps);
    }

    Topology {
        chains,
        chain_deps,
        node_to_chain,
    }
}

/// True if the node declares partitions, static or dynamic (`partitions_from`).
fn is_partitioned(dag: &Dag, id: &str) -> bool {
    dag.get_node(id)
        .is_some_and(|n| !n.extracted.partitions.is_empty())
}

fn is_sensor(dag: &Dag, id: &str) -> bool {
    dag.get_node(id)
        .is_some_and(|n| n.kind() == crate::model::NodeKind::Sensor)
}

/// Detect all maximal chains. Every node belongs to exactly one chain.
fn detect_chains(dag: &Dag) -> Vec<Chain> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut chains = Vec::new();
    let topo = dag.topo_order();

    for node_id in topo {
        if visited.contains(node_id) {
            continue;
        }

        let mut nodes = vec![node_id.to_string()];
        visited.insert(node_id.to_string());

        // Extend forward: follow single-succ + single-pred execution edges.
        // PartitionSource edges are excluded — they force phase breaks.
        let mut current = node_id.to_string();
        loop {
            let succs = dag.execution_downstream(&current);
            if succs.len() != 1 {
                break;
            }
            let next = succs[0];
            if dag.execution_upstream(next).len() != 1 {
                break;
            }
            if visited.contains(next) {
                break;
            }
            // Never fuse across a partitioned/unpartitioned boundary (#170). `build_phases`
            // splits a partitioned step into per-chunk work units, so an unpartitioned
            // neighbour fused into the same chain either lands in a sibling work unit of the
            // same phase (decided, hashed and dispatched with no ordering against the keys
            // that read it) or is copied into every chunk (run once per chunk). As its own
            // chain it gets a phase of its own, before its consumers.
            if is_partitioned(dag, &current) != is_partitioned(dag, next) {
                break;
            }
            // A sensor ends its chain (#183): its consumers' run hashes include the sensor's
            // output, so they are decided in a later phase, after the sensor has run.
            if is_sensor(dag, &current) {
                break;
            }
            nodes.push(next.to_string());
            visited.insert(next.to_string());
            current = next.to_string();
        }

        chains.push(Chain { nodes });
    }

    chains
}

// ═══════════════════════════════════════════════════════════════════════════════
// Step 2: Plan — Topology × ResourceConfig → ExecutionPlan
// ═══════════════════════════════════════════════════════════════════════════════

/// Generate an execution plan from a topology and resource config.
pub fn plan(dag: &Dag, topology: &Topology, config: &ResourceConfig) -> ExecutionPlan {
    let phase_assignment = assign_phases(topology);
    let phases = build_phases(dag, topology, &phase_assignment, config);
    let phases = merge_single_stream_phases(phases);

    let total_steps: usize = phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .map(|st| {
            if st.partition_keys.is_empty() {
                1
            } else {
                st.partition_keys.len()
            }
        })
        .sum();

    ExecutionPlan {
        phases,
        total_steps,
    }
}

/// Merge consecutive phases that each have exactly 1 stream into a single phase.
/// This avoids spawning a new process for each sequential phase when there's
/// no parallelism opportunity.
/// True if any step in the phase has an unresolved dynamic partition source
/// (`partitions_from`) — its real step count/keys are only known once
/// `dispatch::expand_pending_partitions` runs at dispatch time.
fn phase_has_pending(phase: &Phase) -> bool {
    phase
        .streams
        .iter()
        .any(|s| s.steps.iter().any(|st| !st.pending_partitions.is_empty()))
}

/// True if a step in `phase` reads a sensor that runs in `prev`. Such a step's run hash includes
/// the sensor's output (#183), so it must stay in a later phase: cache decisions for a phase are
/// made before any of its steps execute.
fn reads_sensor_of(phase: &Phase, prev: &Phase) -> bool {
    let sensors: HashSet<&str> = prev
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .filter(|st| st.kind == crate::model::NodeKind::Sensor)
        .map(|st| st.step_id.base_id())
        .collect();
    !sensors.is_empty()
        && phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .flat_map(|st| st.inputs.values())
            .any(|up| sensors.contains(up.split('[').next().unwrap_or(up)))
}

fn merge_single_stream_phases(phases: Vec<Phase>) -> Vec<Phase> {
    let mut merged: Vec<Phase> = Vec::new();

    for phase in phases {
        // Don't merge if this phase has steps with pending partition resolution,
        // and don't merge INTO a phase that does either — a step fused into a
        // still-pending phase would ride along in the same dispatch phase as
        // partitions that don't exist yet at plan time (see #97: this bit the
        // same collect()-fan-in bug via a second path, `partitions_from`).
        let has_pending = phase_has_pending(&phase);
        let can_merge = !has_pending
            && phase.streams.len() == 1
            && merged.last().is_some_and(|prev: &Phase| {
                prev.streams.len() == 1
                    && !phase_has_pending(prev)
                    && !reads_sensor_of(&phase, prev)
            });

        if can_merge {
            // Append this phase's single stream's steps to the previous phase's single stream.
            let prev = merged.last_mut().unwrap();
            let new_steps = phase.streams.into_iter().next().unwrap().steps;
            prev.streams[0].steps.extend(new_steps);
        } else {
            merged.push(phase);
        }
    }

    merged
}

/// Assign each chain to a phase number based on inter-chain dependencies.
/// chain_idx → phase_number
fn assign_phases(topology: &Topology) -> HashMap<usize, usize> {
    let chain_order = topo_sort_indices(topology.chains.len(), &topology.chain_deps);
    let mut chain_phase: HashMap<usize, usize> = HashMap::new();

    for &chain_idx in &chain_order {
        let deps = &topology.chain_deps[&chain_idx];
        let phase = if deps.is_empty() {
            0
        } else {
            deps.iter()
                .map(|d| chain_phase.get(d).copied().unwrap_or(0) + 1)
                .max()
                .unwrap_or(0)
        };
        chain_phase.insert(chain_idx, phase);
    }

    chain_phase
}

/// Build Phase structs with stream assignments.
fn build_phases(
    dag: &Dag,
    topology: &Topology,
    phase_assignment: &HashMap<usize, usize>,
    config: &ResourceConfig,
) -> Vec<Phase> {
    let max_phase = phase_assignment.values().copied().max().unwrap_or(0);

    let mut phases = Vec::new();
    for phase_num in 0..=max_phase {
        // Chains in this phase.
        let mut chains_in_phase: Vec<usize> = phase_assignment
            .iter()
            .filter(|&(_, &p)| p == phase_num)
            .map(|(&c, _)| c)
            .collect();
        chains_in_phase.sort(); // determinism

        let reason = if phase_num == 0 {
            PhaseReason::Initial
        } else {
            let first_chain = chains_in_phase.first().copied().unwrap_or(0);
            PhaseReason::FanIn {
                node_id: topology.chains[first_chain].nodes[0].clone(),
            }
        };

        // Convert chains to step lists. With late partition expansion, a partitioned
        // node is ONE StreamStep with N partition_keys. We split the partition_keys
        // across streams so each worker process handles a subset.
        let mut work_units: Vec<Vec<StreamStep>> = Vec::new();
        for &chain_idx in &chains_in_phase {
            let steps = chain_to_steps(dag, &topology.chains[chain_idx]);
            let has_partitioned = steps.iter().any(|s| !s.partition_keys.is_empty());

            if !has_partitioned {
                // No partitions — keep chain as one work unit.
                work_units.push(steps);
            } else {
                // Split partitioned steps across streams while preserving chain order.
                // When a chain has multiple partitioned nodes (e.g., fetch→transform),
                // we must keep them together so each stream executes the full chain
                // for its partition slice.
                let mut unpartitioned: Vec<StreamStep> = Vec::new();
                let partitioned: Vec<&StreamStep> = steps
                    .iter()
                    .filter(|s| !s.partition_keys.is_empty())
                    .collect();

                if partitioned.len() <= 1 {
                    // Single partitioned node — split its partition_keys across streams.
                    for step in steps {
                        if step.partition_keys.is_empty() {
                            unpartitioned.push(step);
                        } else {
                            let total_pks = step.partition_keys.len();
                            let chunk_size = total_pks.div_ceil(config.pool_size);
                            for chunk in step.partition_keys.chunks(chunk_size) {
                                work_units.push(vec![StreamStep {
                                    step_id: step.step_id.clone(),
                                    kind: step.kind,
                                    function_name: Arc::clone(&step.function_name),
                                    source_file: Arc::clone(&step.source_file),
                                    inputs: step.inputs.clone(),
                                    pending_partitions: step.pending_partitions.clone(),
                                    serializer: step.serializer.clone(),
                                    sinks: step.sinks.clone(),
                                    run_hashes: step.run_hashes.clone(),
                                    timeout_seconds: step.timeout_seconds,
                                    retries: step.retries,
                                    retry_backoff_seconds: step.retry_backoff_seconds,
                                    partition_keys: chunk.to_vec(),
                                    param_types: step.param_types.clone(),
                                    return_type: step.return_type,
                                }]);
                            }
                        }
                    }
                } else {
                    // Multiple partitioned nodes in the same chain — split by partition
                    // chunk but keep the chain together per chunk. Use the first
                    // partitioned node's partition count to determine chunk size.
                    let total_pks = partitioned[0].partition_keys.len();
                    let chunk_size = total_pks.div_ceil(config.pool_size);

                    for chunk_start in (0..total_pks).step_by(chunk_size) {
                        let chunk_end = (chunk_start + chunk_size).min(total_pks);
                        let mut chain_chunk: Vec<StreamStep> = Vec::new();
                        for step in &steps {
                            if step.partition_keys.is_empty() {
                                // Unpartitioned steps in a mixed chain run in every chunk.
                                chain_chunk.push(step.clone());
                            } else {
                                let pks = if step.partition_keys.len() == total_pks {
                                    step.partition_keys[chunk_start..chunk_end].to_vec()
                                } else {
                                    // Different partition counts — include all (fallback).
                                    step.partition_keys.clone()
                                };
                                chain_chunk.push(StreamStep {
                                    step_id: step.step_id.clone(),
                                    kind: step.kind,
                                    function_name: Arc::clone(&step.function_name),
                                    source_file: Arc::clone(&step.source_file),
                                    inputs: step.inputs.clone(),
                                    pending_partitions: step.pending_partitions.clone(),
                                    serializer: step.serializer.clone(),
                                    sinks: step.sinks.clone(),
                                    run_hashes: step.run_hashes.clone(),
                                    timeout_seconds: step.timeout_seconds,
                                    retries: step.retries,
                                    retry_backoff_seconds: step.retry_backoff_seconds,
                                    partition_keys: pks,
                                    param_types: step.param_types.clone(),
                                    return_type: step.return_type,
                                });
                            }
                        }
                        work_units.push(chain_chunk);
                    }
                }

                if !unpartitioned.is_empty() {
                    work_units.push(unpartitioned);
                }
            }
        }

        // Assign to streams via greedy bin-packing.
        let num_streams = work_units.len().min(config.pool_size).max(1);
        let streams = bin_pack_to_streams(work_units, num_streams, phase_num);

        phases.push(Phase { reason, streams });
    }

    phases
}

/// Convert a chain's nodes into StreamSteps, expanding static partitions.
fn chain_to_steps(dag: &Dag, chain: &Chain) -> Vec<StreamStep> {
    let mut steps = Vec::new();

    for node_id in &chain.nodes {
        let node = dag.get_node(node_id).unwrap();
        let mut inputs = node.resolved_inputs.clone();
        for (k, v) in &node.resolved_collected {
            inputs.insert(k.clone(), v.clone());
        }

        // Check for static partitions — expand into N steps.
        let static_partitions = collect_static_partitions(&node.extracted.partitions);
        // Check for derived partitions — mark for resolution at dispatch time.
        let derived_partitions = collect_derived_partitions(&node.extracted.partitions);
        let serializer: Option<Arc<str>> = node
            .extracted
            .artifact_serializer
            .map(|s| Arc::from(format!("{s:?}").to_lowercase().as_str()));
        let sinks: Vec<crate::model::SinkDecl> = node.extracted.sinks.to_vec();
        let timeout_seconds = node.extracted.timeout_seconds;
        let retries = node.extracted.retries;
        let retry_backoff_seconds = node.extracted.retry_backoff_seconds;
        let function_name: Arc<str> = Arc::from(node.function_name());
        let source_file: Arc<str> = Arc::from(node.source_file());
        let param_types = node.extracted.param_types.clone();
        let return_type = node.extracted.return_type;

        if !static_partitions.is_empty() {
            let combos = expand_partition_combos(&static_partitions);
            let pks: Vec<PartitionKey> = combos.into_iter().map(PartitionKey::from).collect();
            steps.push(StreamStep {
                step_id: StepId::unpartitioned(node_id.as_str()),
                kind: node.kind(),
                function_name,
                source_file,
                inputs,
                pending_partitions: HashMap::new(),
                serializer,
                sinks,
                run_hashes: HashMap::new(),
                timeout_seconds,
                retries,
                retry_backoff_seconds,
                partition_keys: pks,
                param_types: param_types.clone(),
                return_type,
            });
        } else if !derived_partitions.is_empty() {
            steps.push(StreamStep {
                step_id: StepId::unpartitioned(node_id.as_str()),
                kind: node.kind(),
                function_name,
                source_file,
                inputs,
                pending_partitions: derived_partitions,
                serializer: serializer.clone(),
                sinks,
                run_hashes: HashMap::new(),
                timeout_seconds,
                retries,
                retry_backoff_seconds,
                partition_keys: Vec::new(),
                param_types: param_types.clone(),
                return_type,
            });
        } else {
            steps.push(StreamStep {
                step_id: StepId::unpartitioned(node_id.as_str()),
                kind: node.kind(),
                function_name,
                source_file,
                inputs,
                pending_partitions: HashMap::new(),
                serializer,
                sinks,
                run_hashes: HashMap::new(),
                timeout_seconds,
                retries,
                retry_backoff_seconds,
                partition_keys: Vec::new(),
                param_types,
                return_type,
            });
        }
    }

    steps
}

/// Collect DerivedFrom partition specs: dimension → source_node_id.
fn collect_derived_partitions(
    specs: &HashMap<String, crate::model::PartitionSpec>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for (dim, spec) in specs {
        if let crate::model::PartitionSpec::DerivedFrom { source_ref } = spec {
            result.insert(dim.clone(), source_ref.resolution_name().to_string());
        }
    }
    result
}

/// Collect only static partition specs (already resolved values).
/// Dynamic and DerivedFrom specs are handled at dispatch time.
fn collect_static_partitions(
    specs: &HashMap<String, crate::model::PartitionSpec>,
) -> HashMap<String, Vec<String>> {
    let mut result = HashMap::new();
    for (dim, spec) in specs {
        if let crate::model::PartitionSpec::Static { values } = spec {
            let vals: Vec<String> = values
                .iter()
                .map(|v| match v {
                    crate::model::PartitionValue::Str(s) => s.clone(),
                    crate::model::PartitionValue::Int(i) => i.to_string(),
                })
                .collect();
            if !vals.is_empty() {
                result.insert(dim.clone(), vals);
            }
        }
    }
    result
}

/// Expand partition dimensions into all combinations.
/// For single dimension {"ticker": ["A","B","C"]} → [{"ticker":"A"}, {"ticker":"B"}, {"ticker":"C"}]
/// For multiple dimensions, produces the cartesian product.
pub fn expand_partition_combos(
    dims: &HashMap<String, Vec<String>>,
) -> Vec<HashMap<String, String>> {
    let mut combos = vec![HashMap::new()];
    for (dim, values) in dims {
        let mut new_combos = Vec::new();
        for combo in &combos {
            for val in values {
                let mut c = combo.clone();
                c.insert(dim.clone(), val.clone());
                new_combos.push(c);
            }
        }
        combos = new_combos;
    }
    combos
}

/// Weight of a work unit: sum of max(1, partition_keys.len()) for each step.
fn work_unit_weight(steps: &[StreamStep]) -> usize {
    steps
        .iter()
        .map(|st| {
            if st.partition_keys.is_empty() {
                1
            } else {
                st.partition_keys.len()
            }
        })
        .sum()
}

/// Distribute chains across N streams using greedy bin-packing (heaviest-first).
fn bin_pack_to_streams(
    mut chain_steps: Vec<Vec<StreamStep>>,
    num_streams: usize,
    phase_idx: usize,
) -> Vec<WorkerStream> {
    // Sort work units by weight, heaviest first.
    chain_steps.sort_by_key(|s| std::cmp::Reverse(work_unit_weight(s)));

    let mut streams: Vec<Vec<StreamStep>> = vec![Vec::new(); num_streams];
    let mut stream_sizes: Vec<usize> = vec![0; num_streams];

    for steps in chain_steps {
        // Assign to stream with least weight.
        let weight = work_unit_weight(&steps);
        let target = stream_sizes
            .iter()
            .enumerate()
            .min_by_key(|&(_, &size)| size)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        stream_sizes[target] += weight;
        streams[target].extend(steps);
    }

    streams
        .into_iter()
        .enumerate()
        .filter(|(_, s)| !s.is_empty())
        .map(|(idx, steps)| WorkerStream {
            stream_id: format!("p{phase_idx}-w{idx}"),
            steps,
        })
        .collect()
}

/// Topological sort of indices by dependency map.
fn topo_sort_indices(count: usize, deps: &HashMap<usize, HashSet<usize>>) -> Vec<usize> {
    let mut in_degree = vec![0usize; count];
    let mut successors = vec![Vec::new(); count];

    for (&node, node_deps) in deps {
        in_degree[node] = node_deps.len();
        for &dep in node_deps {
            successors[dep].push(node);
        }
    }

    let mut queue: VecDeque<usize> = (0..count).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(count);

    while let Some(node) = queue.pop_front() {
        order.push(node);
        for &succ in &successors[node] {
            in_degree[succ] -= 1;
            if in_degree[succ] == 0 {
                queue.push_back(succ);
            }
        }
    }

    order
}

// ═══════════════════════════════════════════════════════════════════════════════
// Convenience: full pipeline
// ═══════════════════════════════════════════════════════════════════════════════

/// Full pipeline: DAG → decompose → plan.
pub fn plan_from_dag(dag: &Dag, config: &ResourceConfig) -> ExecutionPlan {
    let topology = decompose(dag);
    plan(dag, &topology, config)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use smallvec::SmallVec;

    fn build_test_dag(specs: &[(&str, &[&str])]) -> Dag {
        build_dag_with_sensors(specs, &[])
    }

    /// Like `build_test_dag`, with the nodes named in `sensors` declared as `@sensor`.
    fn build_dag_with_sensors(specs: &[(&str, &[&str])], sensors: &[&str]) -> Dag {
        let extracted: Vec<ExtractedNode> = specs
            .iter()
            .map(|(name, deps)| {
                let inputs: SmallVec<[DeclaredInput; 4]> = deps
                    .iter()
                    .map(|&dep| DeclaredInput {
                        param_name: dep.to_string(),
                        upstream: NodeRef::FunctionName(dep.to_string()),
                        collected: false,
                    })
                    .collect();
                let sensor = sensors.contains(name);
                ExtractedNode {
                    kind: if sensor {
                        NodeKind::Sensor
                    } else {
                        NodeKind::Asset
                    },
                    function_name: name.to_string(),
                    explicit_name: None,
                    freshness: if sensor {
                        Freshness::Manual
                    } else {
                        Freshness::Always
                    },
                    inputs,
                    partitions: HashMap::new(),
                    sinks: SmallVec::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    description: None,
                    tags: HashMap::new(),
                    is_unsafe: false,
                    source_file: "test.py".to_string(),
                    byte_offset: 0,
                    source_text: String::new(),
                    cone_hash: String::new(),
                    artifact_serializer: None,
                    param_types: HashMap::new(),
                    return_type: None,
                    parallel_calls: Vec::new(),
                    env: Vec::new(),
                }
            })
            .collect();
        Dag::build(&extracted).unwrap()
    }

    fn cfg(pool_size: usize) -> ResourceConfig {
        ResourceConfig {
            pool_size,
            concurrency_groups: HashMap::new(),
        }
    }

    /// Helper: get step display IDs from a stream.
    fn step_ids(stream: &WorkerStream) -> Vec<String> {
        stream.steps.iter().map(|s| s.step_id.display()).collect()
    }

    /// Helper: total steps across all streams in a phase.
    fn phase_step_count(phase: &Phase) -> usize {
        phase.streams.iter().map(|s| s.steps.len()).sum()
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DECOMPOSITION: DAG shape → chains
    //
    // A chain is a maximal sequence where each node has single pred + succ.
    // Chains are the unit of vertical bundling (one Python process).
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn chain_to_steps_carries_sinks() {
        let mut extracted = ExtractedNode {
            kind: NodeKind::Asset,
            function_name: "a".to_string(),
            explicit_name: None,
            freshness: Freshness::Always,
            inputs: SmallVec::new(),
            partitions: HashMap::new(),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
        };
        extracted.sinks.push(SinkDecl {
            path: "exports/out.parquet".to_string(),
            serializer: Some(SerializerKind::Parquet),
        });
        extracted.sinks.push(SinkDecl {
            path: "s3://bucket/model.pkl".to_string(),
            serializer: None,
        });
        let dag = Dag::build(std::slice::from_ref(&extracted)).unwrap();
        let chain = Chain {
            nodes: vec!["test.py:a".to_string()],
        };
        let steps = chain_to_steps(&dag, &chain);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].sinks.len(), 2);
        assert_eq!(steps[0].sinks[0].path, "exports/out.parquet");
        assert_eq!(steps[0].sinks[0].serializer, Some(SerializerKind::Parquet));
        assert_eq!(steps[0].sinks[1].path, "s3://bucket/model.pkl");
        assert_eq!(steps[0].sinks[1].serializer, None);
    }

    #[test]
    fn decompose_single_node() {
        // DAG: [a]
        // Chains: [a]
        let dag = build_test_dag(&[("a", &[])]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 1);
        assert_eq!(topo.chains[0].nodes, vec!["test.py:a"]);
    }

    #[test]
    fn decompose_linear_chain() {
        // DAG: a → b → c
        // Chains: [a, b, c] — one maximal chain
        let dag = build_test_dag(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 1);
        assert_eq!(
            topo.chains[0].nodes,
            vec!["test.py:a", "test.py:b", "test.py:c"]
        );
    }

    #[test]
    fn decompose_independent_nodes() {
        // DAG: a   b   c   (no edges)
        // Chains: [a], [b], [c] — each is its own chain
        let dag = build_test_dag(&[("a", &[]), ("b", &[]), ("c", &[])]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 3);
        for chain in &topo.chains {
            assert_eq!(chain.nodes.len(), 1);
        }
    }

    #[test]
    fn decompose_fan_out() {
        // DAG: a → [b, c, d]
        // a has 3 successors → chain breaks
        // Chains: [a], [b], [c], [d]
        let dag = build_test_dag(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["a"])]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 4);
    }

    #[test]
    fn decompose_fan_in() {
        // DAG: [a, b, c] → d
        // d has 3 predecessors → can't be chained with any
        // Chains: [a], [b], [c], [d]
        let dag = build_test_dag(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &["a", "b", "c"])]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 4);
    }

    #[test]
    fn decompose_diamond() {
        // DAG: a → [b, c] → d
        // a has 2 succs → break. d has 2 preds → break.
        // Chains: [a], [b], [c], [d]
        let dag = build_test_dag(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 4);
    }

    #[test]
    fn decompose_chain_then_fan_out() {
        // DAG: a → b → c → [d, e, f]
        // a→b→c is a chain (each has 1 pred, 1 succ until c which has 3 succs)
        // Chains: [a, b, c], [d], [e], [f]
        let dag = build_test_dag(&[
            ("a", &[]),
            ("b", &["a"]),
            ("c", &["b"]),
            ("d", &["c"]),
            ("e", &["c"]),
            ("f", &["c"]),
        ]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 4);
        assert_eq!(topo.chains[0].nodes.len(), 3); // [a, b, c]
    }

    #[test]
    fn decompose_parallel_chains() {
        // DAG: a→b   c→d   e→f   (3 independent chains)
        // Chains: [a, b], [c, d], [e, f]
        let dag = build_test_dag(&[
            ("a", &[]),
            ("b", &["a"]),
            ("c", &[]),
            ("d", &["c"]),
            ("e", &[]),
            ("f", &["e"]),
        ]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 3);
        for chain in &topo.chains {
            assert_eq!(chain.nodes.len(), 2);
        }
    }

    #[test]
    fn decompose_spaceflights() {
        // DAG:
        //   raw_s→prep_s ──┐
        //   raw_c→prep_c ──├→ master→split → train
        //   raw_r→prep_r ──┘              ↘ eval ← train
        //
        // Chains: [raw_s,prep_s], [raw_c,prep_c], [raw_r,prep_r],
        //         [master,split], [train], [eval]
        // (split has 2 succs → breaks; eval has 2 preds → breaks)
        let dag = build_test_dag(&[
            ("raw_s", &[]),
            ("raw_c", &[]),
            ("raw_r", &[]),
            ("prep_s", &["raw_s"]),
            ("prep_c", &["raw_c"]),
            ("prep_r", &["raw_r"]),
            ("master", &["prep_s", "prep_c", "prep_r"]),
            ("split", &["master"]),
            ("train", &["split"]),
            ("eval", &["train", "split"]),
        ]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 6);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // PLANNING: Topology × ResourceConfig → ExecutionPlan
    //
    // The planner assigns chains to phases (barriers at fan-in points)
    // and streams (one Python process per stream).
    //
    // Rules:
    // - Chains with no cross-chain deps → same phase (parallel)
    // - Chain depends on chains from multiple phases → new phase (barrier)
    // - Consecutive single-stream phases merge (avoid redundant process spawn)
    // - Chains distributed across pool_size streams via bin-packing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn plan_single_node() {
        // a → 1 phase, 1 stream, 1 step
        let dag = build_test_dag(&[("a", &[])]);
        let p = plan_from_dag(&dag, &cfg(10));
        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0].streams.len(), 1);
        assert_eq!(p.phases[0].streams[0].steps.len(), 1);
    }

    #[test]
    fn plan_linear_chain_is_one_phase_one_stream() {
        // a → b → c → d → e
        // All in one chain → 1 phase, 1 stream, 5 steps in order
        let dag = build_test_dag(&[
            ("a", &[]),
            ("b", &["a"]),
            ("c", &["b"]),
            ("d", &["c"]),
            ("e", &["d"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));
        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0].streams.len(), 1);
        assert_eq!(p.phases[0].streams[0].steps.len(), 5);
        let ids = step_ids(&p.phases[0].streams[0]);
        assert_eq!(
            ids,
            vec![
                "test.py:a",
                "test.py:b",
                "test.py:c",
                "test.py:d",
                "test.py:e"
            ]
        );
    }

    #[test]
    fn plan_independent_nodes_fan_out_to_streams() {
        // 10 independent nodes, pool_size=3
        // → 1 phase, 3 streams, ~3-4 steps each
        let dag = build_test_dag(&[
            ("a", &[]),
            ("b", &[]),
            ("c", &[]),
            ("d", &[]),
            ("e", &[]),
            ("f", &[]),
            ("g", &[]),
            ("h", &[]),
            ("i", &[]),
            ("j", &[]),
        ]);
        let p = plan_from_dag(&dag, &cfg(3));
        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0].streams.len(), 3);
        assert_eq!(p.total_steps, 10);
        for stream in &p.phases[0].streams {
            assert!(stream.steps.len() >= 3 && stream.steps.len() <= 4);
        }
    }

    #[test]
    fn plan_diamond_creates_phase_barrier() {
        // a → [b, c] → d
        //
        // Phase 0: [a] (1 stream)
        // Phase 1: [b], [c] (2 streams, parallel)
        // Phase 2: [d] (1 stream, barrier — needs both b and c)
        //
        // But phases 0 and 2 are single-stream → merge kicks in:
        // Phase 0: [a] merged with... wait, phase 1 has 2 streams so no merge.
        // Final: 3 phases.
        let dag = build_test_dag(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])]);
        let p = plan_from_dag(&dag, &cfg(10));
        assert_eq!(p.phases.len(), 3);
        assert_eq!(phase_step_count(&p.phases[0]), 1); // a
        assert_eq!(phase_step_count(&p.phases[1]), 2); // b, c
        assert_eq!(phase_step_count(&p.phases[2]), 1); // d
    }

    #[test]
    fn plan_chain_then_fan_out_then_fan_in() {
        // a → b → c → [d0..d9] → e
        //
        // Decomposition: chain [a,b,c], then 10 single-node chains [d0]..[d9], then [e]
        //
        // Phase 0: 1 stream [a, b, c]
        // Phase 1: up to 5 streams with 10 d-nodes distributed
        // Phase 2: 1 stream [e]
        //
        // Phase 2 is single-stream but phase 1 has multiple → no merge.
        let mut specs: Vec<(&str, Vec<&str>)> =
            vec![("a", vec![]), ("b", vec!["a"]), ("c", vec!["b"])];
        let d_names: Vec<String> = (0..10).map(|i| format!("d{i}")).collect();
        for name in &d_names {
            specs.push((name, vec!["c"]));
        }
        let d_refs: Vec<&str> = d_names.iter().map(|s| s.as_str()).collect();
        specs.push(("e", d_refs));
        let specs_ref: Vec<(&str, &[&str])> =
            specs.iter().map(|(n, d)| (*n, d.as_slice())).collect();

        let dag = build_test_dag(&specs_ref);
        let p = plan_from_dag(&dag, &cfg(5));

        assert_eq!(p.phases[0].streams.len(), 1);
        assert_eq!(p.phases[0].streams[0].steps.len(), 3); // [a, b, c]
        assert_eq!(phase_step_count(&p.phases[1]), 10); // d0..d9
        assert!(p.phases[1].streams.len() <= 5); // distributed
        assert_eq!(phase_step_count(p.phases.last().unwrap()), 1); // [e]
    }

    #[test]
    fn plan_parallel_chains_in_one_phase() {
        // a→b   c→d   e→f   (3 independent 2-step chains)
        //
        // All chains have no cross-deps → 1 phase, 3 streams
        let dag = build_test_dag(&[
            ("a", &[]),
            ("b", &["a"]),
            ("c", &[]),
            ("d", &["c"]),
            ("e", &[]),
            ("f", &["e"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));
        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0].streams.len(), 3);
        for stream in &p.phases[0].streams {
            assert_eq!(stream.steps.len(), 2);
        }
    }

    #[test]
    fn plan_spaceflights_diamond() {
        // raw_s→prep_s ──┐
        // raw_c→prep_c ──├→ master→split→train
        // raw_r→prep_r ──┘              ↘ eval
        //
        // Phase 0: 3 streams [raw_s,prep_s], [raw_c,prep_c], [raw_r,prep_r]
        // Phase 1: 1 stream [master,split,train,eval] (merged single-stream phases)
        let dag = build_test_dag(&[
            ("raw_s", &[]),
            ("raw_c", &[]),
            ("raw_r", &[]),
            ("prep_s", &["raw_s"]),
            ("prep_c", &["raw_c"]),
            ("prep_r", &["raw_r"]),
            ("master", &["prep_s", "prep_c", "prep_r"]),
            ("split", &["master"]),
            ("train", &["split"]),
            ("eval", &["train", "split"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));

        // Phase 0: 3 parallel source→prep chains
        assert_eq!(p.phases[0].streams.len(), 3);
        for stream in &p.phases[0].streams {
            assert_eq!(stream.steps.len(), 2);
        }
        // Remaining: master→split, train, eval = 4 steps (merged into 1 stream)
        let remaining: usize = p.phases[1..].iter().map(phase_step_count).sum();
        assert_eq!(remaining, 4);
        assert_eq!(p.total_steps, 10);
    }

    #[test]
    fn plan_wide_fan_in() {
        // [a, b, c, d, e, f, g, h, i, j] → merge
        //
        // Phase 0: 10 independent nodes → distributed across pool_size streams
        // Phase 1: [merge] → 1 stream (merged with nothing since phase 0 is multi-stream)
        let dag = build_test_dag(&[
            ("a", &[]),
            ("b", &[]),
            ("c", &[]),
            ("d", &[]),
            ("e", &[]),
            ("f", &[]),
            ("g", &[]),
            ("h", &[]),
            ("i", &[]),
            ("j", &[]),
            ("merge", &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(5));
        assert_eq!(p.phases[0].streams.len(), 5); // 10 nodes across 5 streams
        assert_eq!(phase_step_count(&p.phases[0]), 10);
        assert_eq!(phase_step_count(p.phases.last().unwrap()), 1); // merge
        assert_eq!(p.total_steps, 11);
    }

    #[test]
    fn plan_consecutive_single_stream_phases_merge() {
        // a → b → c where b has 2 preds conceptually... actually let's
        // test directly: a → [b] → c (b is single chain, c depends only on b)
        // With just a→b→c this is one chain, one phase. So test with a diamond
        // that collapses to single-stream phases:
        //
        // a → b → c → d (all single-pred/single-succ)
        // This is one chain, so it's trivially 1 phase.
        //
        // Better test: the merge optimization.
        // Phase 0: [a] (1 stream), Phase 1: [b] (1 stream) → merge into 1 phase.
        // This happens when chains are in separate phases but both single-stream.
        //
        // Use spaceflights: phases after the fan-in (master,split,train,eval)
        // are all single-stream and should merge.
        let dag = build_test_dag(&[
            ("raw_s", &[]),
            ("prep_s", &["raw_s"]),
            ("master", &["prep_s"]),
            ("split", &["master"]),
            ("train", &["split"]),
            ("eval", &["train"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));
        // Everything is one chain [raw_s, prep_s, master, split, train, eval]
        // → 1 phase, 1 stream, 6 steps
        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0].streams[0].steps.len(), 6);
    }

    #[test]
    fn plan_pool_size_one_forces_sequential() {
        // 5 independent nodes, pool_size=1
        // → 1 phase, 1 stream containing all 5
        let dag = build_test_dag(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[]), ("e", &[])]);
        let p = plan_from_dag(&dag, &cfg(1));
        for phase in &p.phases {
            assert_eq!(phase.streams.len(), 1);
        }
        assert_eq!(p.total_steps, 5);
    }

    #[test]
    fn plan_500_independent_across_16_workers() {
        // Simulates fan_out_500 benchmark
        let extracted: Vec<ExtractedNode> = (0..500)
            .map(|i| ExtractedNode {
                kind: NodeKind::Asset,
                function_name: format!("n{i:03}"),
                explicit_name: None,
                freshness: Freshness::Always,
                inputs: SmallVec::new(),
                partitions: HashMap::new(),
                sinks: SmallVec::new(),
                timeout_seconds: 300,
                retries: 1,
                retry_backoff_seconds: 0.0,
                description: None,
                tags: HashMap::new(),
                is_unsafe: false,
                source_file: "test.py".to_string(),
                byte_offset: 0,
                source_text: String::new(),
                cone_hash: String::new(),
                artifact_serializer: None,
                param_types: HashMap::new(),
                return_type: None,
                parallel_calls: Vec::new(),
                env: Vec::new(),
            })
            .collect();
        let dag = Dag::build(&extracted).unwrap();
        let p = plan_from_dag(&dag, &cfg(16));

        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0].streams.len(), 16);
        assert_eq!(p.total_steps, 500);
        // Each stream gets ~31-32 steps
        for stream in &p.phases[0].streams {
            assert!(stream.steps.len() >= 31 && stream.steps.len() <= 32);
        }
    }

    #[test]
    fn plan_wide_layers_with_sync_barriers() {
        // Layer 0: [a0, a1, a2] (independent)
        // Sync: merge0 depends on all of layer 0
        // Layer 1: [b0, b1, b2] depend on merge0
        // Sync: merge1 depends on all of layer 1
        //
        // Phase 0: [a0], [a1], [a2] → 3 streams
        // Phase 1: [merge0] → 1 stream (barrier)
        // Phase 2: [b0], [b1], [b2] → 3 streams
        // Phase 3: [merge1] → 1 stream (barrier)
        //
        // But phases 1+2 can't merge (1 is single-stream, 2 is multi-stream — no merge)
        // Phases 3 is single-stream, preceded by multi-stream — no merge.
        let dag = build_test_dag(&[
            ("a0", &[]),
            ("a1", &[]),
            ("a2", &[]),
            ("merge0", &["a0", "a1", "a2"]),
            ("b0", &["merge0"]),
            ("b1", &["merge0"]),
            ("b2", &["merge0"]),
            ("merge1", &["b0", "b1", "b2"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));

        assert_eq!(phase_step_count(&p.phases[0]), 3); // a0, a1, a2
        assert_eq!(p.total_steps, 8);
        // Should have barriers at merge points
        assert!(p.phases.len() >= 3); // at least: layer0, merge0+layer1, merge1
    }

    #[test]
    fn plan_deep_diamond_benchmark() {
        // 5 parallel 3-step chains → merge → 2-step tail
        // src_0→prep_0→feat_0 ──┐
        // src_1→prep_1→feat_1 ──├→ merge → transform → output
        // ...                   ┘
        //
        // Phase 0: 5 parallel streams, each [src, prep, feat]
        // Phase 1: [merge, transform, output] (merged single-stream)
        let dag = build_test_dag(&[
            ("s0", &[]),
            ("s1", &[]),
            ("s2", &[]),
            ("s3", &[]),
            ("s4", &[]),
            ("p0", &["s0"]),
            ("p1", &["s1"]),
            ("p2", &["s2"]),
            ("p3", &["s3"]),
            ("p4", &["s4"]),
            ("f0", &["p0"]),
            ("f1", &["p1"]),
            ("f2", &["p2"]),
            ("f3", &["p3"]),
            ("f4", &["p4"]),
            ("merge", &["f0", "f1", "f2", "f3", "f4"]),
            ("transform", &["merge"]),
            ("output", &["transform"]),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));

        // Phase 0: 5 streams of 3 steps each
        assert_eq!(p.phases[0].streams.len(), 5);
        for stream in &p.phases[0].streams {
            assert_eq!(stream.steps.len(), 3);
        }
        // Remaining: merge + transform + output = 3 steps
        let remaining: usize = p.phases[1..].iter().map(phase_step_count).sum();
        assert_eq!(remaining, 3);
        assert_eq!(p.total_steps, 18);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // PARTITION EXPANSION
    // ═══════════════════════════════════════════════════════════════════════

    #[allow(clippy::type_complexity)]
    fn build_partitioned_dag(specs: &[(&str, &[&str], &[(&str, &[&str])])]) -> Dag {
        let extracted: Vec<ExtractedNode> = specs
            .iter()
            .map(|(name, deps, partitions)| {
                let inputs: SmallVec<[DeclaredInput; 4]> = deps
                    .iter()
                    .map(|&dep| DeclaredInput {
                        param_name: dep.to_string(),
                        upstream: NodeRef::FunctionName(dep.to_string()),
                        collected: false,
                    })
                    .collect();
                let partition_map: HashMap<String, crate::model::PartitionSpec> = partitions
                    .iter()
                    .map(|(dim, vals)| {
                        (
                            dim.to_string(),
                            crate::model::PartitionSpec::Static {
                                values: vals
                                    .iter()
                                    .map(|v| crate::model::PartitionValue::Str(v.to_string()))
                                    .collect(),
                            },
                        )
                    })
                    .collect();
                ExtractedNode {
                    kind: NodeKind::Asset,
                    function_name: name.to_string(),
                    explicit_name: None,
                    freshness: Freshness::Always,
                    inputs,
                    partitions: partition_map,
                    sinks: SmallVec::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    description: None,
                    tags: HashMap::new(),
                    is_unsafe: false,
                    source_file: "test.py".to_string(),
                    byte_offset: 0,
                    source_text: String::new(),
                    cone_hash: String::new(),
                    artifact_serializer: None,
                    param_types: HashMap::new(),
                    return_type: None,
                    parallel_calls: Vec::new(),
                    env: Vec::new(),
                }
            })
            .collect();
        Dag::build(&extracted).unwrap()
    }

    #[test]
    fn plan_static_partitions_late_expansion() {
        // fetch_prices with 3 partitions → 1 StreamStep with 3 partition_keys
        // (late expansion: workers loop internally)
        let dag = build_partitioned_dag(&[(
            "fetch_prices",
            &[],
            &[("ticker", &["AAPL", "MSFT", "GOOG"])],
        )]);
        let p = plan_from_dag(&dag, &cfg(10));

        assert_eq!(p.total_steps, 3);
        assert_eq!(p.phases.len(), 1);

        // Late expansion: planner produces fewer StreamSteps, each with partition_keys
        let all_steps: Vec<&StreamStep> =
            p.phases[0].streams.iter().flat_map(|s| &s.steps).collect();
        // With pool_size=10 and 3 partitions, chunk_size=1 → 3 work units → 3 streams
        assert_eq!(all_steps.len(), 3);

        // Each step has partition_keys — step_id is unpartitioned
        let mut all_tickers: Vec<String> = all_steps
            .iter()
            .flat_map(|s| {
                s.partition_keys
                    .iter()
                    .map(|pk| pk.0.get("ticker").unwrap().clone())
            })
            .collect();
        all_tickers.sort();
        assert_eq!(all_tickers, vec!["AAPL", "GOOG", "MSFT"]);

        // step_id.base should be unpartitioned
        for step in &all_steps {
            assert_eq!(step.step_id.base_id(), "test.py:fetch_prices");
            assert!(step.step_id.partition.is_empty());
        }
    }

    #[test]
    fn plan_partitioned_steps_distributed_across_streams() {
        // 100 partition values, pool_size=10 → split into 10 chunks of 10

        let ticker_strings: Vec<String> = (0..100).map(|i| format!("T{i:03}")).collect();
        let ticker_refs: Vec<&str> = ticker_strings.iter().map(|s| s.as_str()).collect();

        let dag = build_partitioned_dag(&[("fetch", &[], &[("ticker", &ticker_refs)])]);
        let p = plan_from_dag(&dag, &cfg(10));

        assert_eq!(p.total_steps, 100);
        assert_eq!(p.phases[0].streams.len(), 10);
        // Each stream has 1 StreamStep with 10 partition_keys
        for stream in &p.phases[0].streams {
            assert_eq!(stream.steps.len(), 1);
            assert_eq!(stream.steps[0].partition_keys.len(), 10);
        }
    }

    #[test]
    fn plan_chain_with_partitioned_node() {
        // source → partitioned_fetch(3 partitions) → aggregate
        // source is non-partitioned, fetch expands to 3 steps
        let dag = build_partitioned_dag(&[
            ("source", &[], &[]),
            (
                "fetch",
                &["source"],
                &[("ticker", &["AAPL", "MSFT", "GOOG"])],
            ),
        ]);
        let p = plan_from_dag(&dag, &cfg(10));

        // source = 1 step, fetch = 3 partition_keys = 4 total
        assert_eq!(p.total_steps, 4);
    }

    /// (phase index, stream index, step index) of every step with this base id.
    fn positions(p: &ExecutionPlan, base: &str) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for (pi, phase) in p.phases.iter().enumerate() {
            for (si, stream) in phase.streams.iter().enumerate() {
                for (ti, step) in stream.steps.iter().enumerate() {
                    if step.step_id.base_id() == base {
                        out.push((pi, si, ti));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn unpartitioned_producer_runs_before_its_partitioned_consumer() {
        // #170: multiplier → leaf(k) is a single-succ/single-pred edge, so it used to be fused
        // into one chain; build_phases then split the partitioned step into per-chunk work units
        // and pushed `multiplier` into a separate work unit of the SAME phase. Run-hash decisions
        // walk streams in order, so leaf's keys were hashed before multiplier's hash existed.
        // The producer must come first: an earlier phase, or earlier in the same stream.
        for pool in [1, 2, 10] {
            let dag = build_partitioned_dag(&[
                ("multiplier", &[], &[]),
                ("leaf", &["multiplier"], &[("k", &["a", "b", "c"])]),
            ]);
            let p = plan_from_dag(&dag, &cfg(pool));
            assert_eq!(p.total_steps, 4, "pool={pool}");
            let producer = positions(&p, "test.py:multiplier");
            assert_eq!(producer.len(), 1, "pool={pool}: multiplier runs once");
            let (mp, ms, mt) = producer[0];
            for (lp, ls, lt) in positions(&p, "test.py:leaf") {
                assert!(
                    mp < lp || (mp == lp && ms == ls && mt < lt),
                    "pool={pool}: multiplier at {:?} does not precede leaf at {:?}",
                    (mp, ms, mt),
                    (lp, ls, lt)
                );
            }
        }
    }

    /// #183: a consumer's run hash includes the output of the sensors it reads, and cache
    /// decisions are made per phase before the phase executes. A sensor must therefore finish in
    /// an EARLIER phase than its consumers; the same stream is not enough. Single-stream phases
    /// are normally merged, which must not pull a sensor's consumer into the sensor's phase.
    #[test]
    fn a_sensor_runs_in_an_earlier_phase_than_its_consumers() {
        let shapes: Vec<Vec<(&str, &[&str])>> = vec![
            vec![
                ("etag", &[]),
                ("bronze", &["etag"]),
                ("silver", &["bronze"]),
            ],
            vec![
                ("etag", &[]),
                ("bronze", &["etag"]),
                ("other", &["etag"]),
                ("plain", &[]),
                ("joined", &["bronze", "plain"]),
            ],
            vec![("plain", &[]), ("etag", &[]), ("mix", &["plain", "etag"])],
        ];
        for specs in &shapes {
            for pool in [1, 2, 10] {
                let dag = build_dag_with_sensors(specs, &["etag"]);
                let p = plan_from_dag(&dag, &cfg(pool));
                let sensor_phase = positions(&p, "test.py:etag")[0].0;
                for consumer in dag.downstream("test.py:etag") {
                    for (phase, _, _) in positions(&p, consumer) {
                        assert!(
                            sensor_phase < phase,
                            "pool={pool}: sensor in phase {sensor_phase}, consumer {consumer} \
                             in phase {phase}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_sensor_runs_before_its_partitioned_consumer() {
        let mut dag_specs = build_partitioned_dag(&[
            ("etag", &[], &[]),
            ("leaf", &["etag"], &[("k", &["a", "b", "c"])]),
        ]);
        // build_partitioned_dag makes assets; rebuild with `etag` as a sensor.
        let mut extracted: Vec<ExtractedNode> = dag_specs
            .topo_order()
            .iter()
            .map(|id| dag_specs.get_node(id).unwrap().extracted.clone())
            .collect();
        for n in &mut extracted {
            if n.function_name == "etag" {
                n.kind = NodeKind::Sensor;
                n.freshness = Freshness::Manual;
            }
        }
        dag_specs = Dag::build(&extracted).unwrap();
        for pool in [1, 2, 10] {
            let p = plan_from_dag(&dag_specs, &cfg(pool));
            let sensor_phase = positions(&p, "test.py:etag")[0].0;
            for (phase, _, _) in positions(&p, "test.py:leaf") {
                assert!(sensor_phase < phase, "pool={pool}");
            }
        }
    }

    #[test]
    fn unpartitioned_producer_of_partitioned_chain_runs_once() {
        // a → p1(k) → p2(k): the unpartitioned head used to be copied into every chunk of the
        // partitioned chain (so it ran once per chunk). It must run exactly once, first.
        let dag = build_partitioned_dag(&[
            ("a", &[], &[]),
            ("p1", &["a"], &[("k", &["x", "y", "z", "w"])]),
            ("p2", &["p1"], &[("k", &["x", "y", "z", "w"])]),
        ]);
        let p = plan_from_dag(&dag, &cfg(4));
        assert_eq!(p.total_steps, 9);
        let producer = positions(&p, "test.py:a");
        assert_eq!(producer.len(), 1, "a must be planned once: {producer:?}");
        let first_partitioned_phase = positions(&p, "test.py:p1")
            .into_iter()
            .map(|(phase, _, _)| phase)
            .min()
            .unwrap();
        assert!(producer[0].0 < first_partitioned_phase);
    }

    #[test]
    fn plan_non_partitioned_remains_single_step() {
        // Regular asset without partitions → 1 step, no partition_keys
        let dag = build_partitioned_dag(&[("simple", &[], &[])]);
        let p = plan_from_dag(&dag, &cfg(10));

        assert_eq!(p.total_steps, 1);
        assert!(p.phases[0].streams[0].steps[0].partition_keys.is_empty());
        assert!(p.phases[0].streams[0].steps[0].step_id.partition.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // FAN-IN (collect()) — regression coverage for #97
    //
    // A collect()-consuming node must never be chain-fused with its
    // partitioned producer: fusion would let the consumer run in the same
    // phase as the still-executing partition chunks (see build_phases'
    // "unpartitioned steps run in every chunk / as their own late work
    // unit" handling), racing ahead of data it depends on. It must land in
    // its own chain, and — via cross-chain deps — a strictly later phase.
    // ═══════════════════════════════════════════════════════════════════════

    fn build_collect_dag(producer_partitions: &[&str]) -> Dag {
        let producer = ExtractedNode {
            kind: NodeKind::Asset,
            function_name: "source".to_string(),
            explicit_name: None,
            freshness: Freshness::Always,
            inputs: SmallVec::new(),
            partitions: HashMap::from([(
                "key".to_string(),
                PartitionSpec::Static {
                    values: producer_partitions
                        .iter()
                        .map(|v| PartitionValue::Str(v.to_string()))
                        .collect(),
                },
            )]),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
        };
        let collector = ExtractedNode {
            kind: NodeKind::Asset,
            function_name: "sink".to_string(),
            explicit_name: None,
            freshness: Freshness::Always,
            inputs: SmallVec::from_vec(vec![DeclaredInput {
                param_name: "data".to_string(),
                upstream: NodeRef::FunctionName("source".to_string()),
                collected: true,
            }]),
            partitions: HashMap::new(),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
        };
        Dag::build(&[producer, collector]).unwrap()
    }

    #[test]
    fn decompose_breaks_chain_at_collect_edge() {
        // source (2 partitions) --collect()--> sink
        // Even though source has exactly one successor and sink exactly one
        // predecessor, the Collect edge must not be chain-fused.
        let dag = build_collect_dag(&["a", "b"]);
        let topo = decompose(&dag);
        assert_eq!(topo.chains.len(), 2);
        for chain in &topo.chains {
            assert_eq!(chain.nodes.len(), 1);
        }
    }

    #[test]
    fn plan_collect_fan_in_creates_phase_barrier() {
        // source (2 partitions) --collect()--> sink
        // Phase 0: source's 2 partition_keys. Phase 1: sink, gated on phase 0.
        let dag = build_collect_dag(&["a", "b"]);
        let p = plan_from_dag(&dag, &cfg(10));

        assert_eq!(p.phases.len(), 2);
        assert_eq!(p.phases[0].reason, PhaseReason::Initial);
        let phase0_steps: Vec<&StreamStep> =
            p.phases[0].streams.iter().flat_map(|s| &s.steps).collect();
        // pool_size=10 with 2 partitions → chunk_size 1 → one StreamStep per key.
        let phase0_pk_count: usize = phase0_steps.iter().map(|s| s.partition_keys.len()).sum();
        assert!(
            phase0_steps
                .iter()
                .all(|s| s.step_id.base_id() == "test.py:source")
        );
        assert_eq!(phase0_pk_count, 2);

        let phase1_steps: Vec<&StreamStep> =
            p.phases[1].streams.iter().flat_map(|s| &s.steps).collect();
        assert_eq!(phase1_steps.len(), 1);
        assert_eq!(phase1_steps[0].step_id.base_id(), "test.py:sink");
        assert!(
            matches!(&p.phases[1].reason, PhaseReason::FanIn { node_id } if node_id == "test.py:sink")
        );
    }

    /// Regression test for a second manifestation of #97, found while fixing
    /// it: `merge_single_stream_phases` only checked whether the phase *being
    /// merged* had unresolved dynamic partitions (`partitions_from`), not
    /// whether the phase it would merge *into* did. A collect() consumer of a
    /// `partitions_from`-partitioned producer is its own single-stream phase
    /// and would get silently folded into the still-pending producer's phase,
    /// landing in the exact same "runs before its data exists" bug as the
    /// static-partition case, just via phase merging instead of chain fusion.
    #[test]
    fn plan_dynamic_partition_collect_not_merged_into_pending_phase() {
        let tickers = ExtractedNode {
            kind: NodeKind::Asset,
            function_name: "tickers".to_string(),
            explicit_name: None,
            freshness: Freshness::Always,
            inputs: SmallVec::new(),
            partitions: HashMap::new(),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
        };
        let fetch_prices = ExtractedNode {
            kind: NodeKind::Asset,
            function_name: "fetch_prices".to_string(),
            explicit_name: None,
            freshness: Freshness::Always,
            inputs: SmallVec::new(),
            partitions: HashMap::from([(
                "ticker".to_string(),
                PartitionSpec::DerivedFrom {
                    source_ref: NodeRef::FunctionName("tickers".to_string()),
                },
            )]),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
        };
        let aggregate = ExtractedNode {
            kind: NodeKind::Asset,
            function_name: "aggregate".to_string(),
            explicit_name: None,
            freshness: Freshness::Always,
            inputs: SmallVec::from_vec(vec![DeclaredInput {
                param_name: "reports".to_string(),
                upstream: NodeRef::FunctionName("fetch_prices".to_string()),
                collected: true,
            }]),
            partitions: HashMap::new(),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
        };
        let dag = Dag::build(&[tickers, fetch_prices, aggregate]).unwrap();
        let p = plan_from_dag(&dag, &cfg(10));

        // tickers, fetch_prices (pending), and aggregate must land in three
        // distinct phases — merging any two would let aggregate dispatch
        // before fetch_prices' real (dynamically-resolved) partitions exist.
        assert_eq!(p.phases.len(), 3);
        let base_ids_in = |phase: &Phase| -> Vec<String> {
            phase
                .streams
                .iter()
                .flat_map(|s| &s.steps)
                .map(|st| st.step_id.base_id().to_string())
                .collect()
        };
        assert_eq!(base_ids_in(&p.phases[0]), vec!["test.py:tickers"]);
        assert_eq!(base_ids_in(&p.phases[1]), vec!["test.py:fetch_prices"]);
        assert_eq!(base_ids_in(&p.phases[2]), vec!["test.py:aggregate"]);
        assert!(
            !p.phases[1].streams[0].steps[0]
                .pending_partitions
                .is_empty()
        );
    }
}
