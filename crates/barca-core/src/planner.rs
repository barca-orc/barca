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

impl ExecutionPlan {
    /// The node id of every planned step, in plan order (a partitioned step once per stream).
    pub fn step_ids(&self) -> impl Iterator<Item = &str> {
        self.phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|step| step.step_id.base_id())
    }
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
    /// `weight=`: how much of the pool's capacity the step takes while it runs (#436).
    pub weight: u32,
    /// Base backoff in seconds between attempts; delay = `retry_backoff_seconds * attempt`.
    pub retry_backoff_seconds: f64,
    /// Late-expanded partition keys: workers loop over these internally.
    /// Empty for unpartitioned nodes. Enables 1M+ partitions without 1M structs.
    pub partition_keys: Vec<PartitionKey>,
    /// Parameter frame types from function annotations (param → loader).
    pub param_types: HashMap<String, crate::model::ValueType>,
    /// Return frame type from the function's return annotation.
    pub return_type: Option<crate::model::ValueType>,
    /// Keyed `partitions_from` sources (`crate::keyed`): the dict sources the keys were
    /// expanded from, and whether this step is itself a source a `partitions_from` reads.
    pub keyed: crate::keyed::StepKeyed,
}

impl StreamStep {
    /// This step with `keys` in place of its partition keys, without copying its own (a
    /// partitioned step can hold 200k keys).
    pub fn with_keys(&self, keys: Vec<PartitionKey>) -> StreamStep {
        StreamStep {
            step_id: self.step_id.clone(),
            kind: self.kind,
            function_name: self.function_name.clone(),
            source_file: self.source_file.clone(),
            inputs: self.inputs.clone(),
            pending_partitions: self.pending_partitions.clone(),
            serializer: self.serializer.clone(),
            sinks: self.sinks.clone(),
            run_hashes: HashMap::new(),
            timeout_seconds: self.timeout_seconds,
            retries: self.retries,
            weight: self.weight,
            retry_backoff_seconds: self.retry_backoff_seconds,
            partition_keys: keys,
            param_types: self.param_types.clone(),
            return_type: self.return_type,
            keyed: self.keyed.clone(),
        }
    }
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
    decompose_ordered(dag, dag.topo_order())
}

fn decompose_ordered(dag: &Dag, order: Vec<&str>) -> Topology {
    let chains = detect_chains(dag, &order);

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
                let Some(&upstream_chain) = node_to_chain.get(upstream_id) else {
                    continue;
                };
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

/// Detect all maximal chains. Every node belongs to exactly one chain.
fn detect_chains(dag: &Dag, topo: &[&str]) -> Vec<Chain> {
    let allowed: HashSet<&str> = topo.iter().copied().collect();
    let mut successors: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut predecessors: HashMap<&str, usize> = HashMap::new();
    for &id in topo {
        for up in dag
            .execution_upstream(id)
            .into_iter()
            .filter(|up| allowed.contains(up))
        {
            successors.entry(up).or_default().push(id);
            *predecessors.entry(id).or_default() += 1;
        }
    }
    let mut visited: HashSet<String> = HashSet::new();
    let mut chains = Vec::new();
    for &node_id in topo {
        if visited.contains(node_id) {
            continue;
        }

        let mut nodes = vec![node_id.to_string()];
        visited.insert(node_id.to_string());

        // Extend forward: follow single-succ + single-pred execution edges.
        // PartitionSource edges are excluded — they force phase breaks.
        let mut current = node_id.to_string();
        loop {
            let succs = successors
                .get(current.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default();
            if succs.len() != 1 {
                break;
            }
            let next = succs[0];
            if predecessors.get(next).copied().unwrap_or(0) != 1 {
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
            // A sensor or a `Manual` asset ends its chain (#183, #548): its consumers' run
            // hashes include its output, so they are decided in a later phase, after it ran.
            if crate::cache::folds_output(dag, &current) {
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
    plan_narrowed(dag, topology, config, None)
}

/// [`plan`], expanding only the selected keys of a static dimension that `selection` names
/// (`--partition`), so a few keys out of many are never all built. A pre-filter only:
/// [`crate::selection::PartitionSelection::apply`] narrows the plan and decides what is skipped.
pub fn plan_narrowed(
    dag: &Dag,
    topology: &Topology,
    config: &ResourceConfig,
    selection: Option<&crate::selection::PartitionSelection>,
) -> ExecutionPlan {
    let phase_assignment = assign_phases(topology);
    let phases = build_phases(dag, topology, &phase_assignment, config, selection);
    let phases = merge_single_stream_phases(dag, phases);

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

/// True if a step in `phase` reads a sensor or a `Manual` asset that runs in `prev`. Such a
/// step's run hash includes that output (#183, #548), so it must stay in a later phase: cache
/// decisions for a phase are made before any of its steps execute.
fn reads_sensor_of(dag: &Dag, phase: &Phase, prev: &Phase) -> bool {
    let sensors: HashSet<&str> = prev
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .filter(|st| crate::cache::folds_output(dag, st.step_id.base_id()))
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

/// True if a step in `phase` collects (`collect(upstream)`) a node that runs in `prev`. A
/// fan-in is handed the outputs of every partition of its upstream when its phase starts, so
/// the upstream's phase has to be over by then: fused into it, the fan-in would start with no
/// collected input at all (#331, #325).
fn collects_from(dag: &Dag, phase: &Phase, prev: &Phase) -> bool {
    let produced: HashSet<&str> = prev
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|st| st.step_id.base_id())
        .collect();
    phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .filter_map(|st| dag.get_node(st.step_id.base_id()))
        .flat_map(|node| node.resolved_collected.values())
        .any(|up| produced.contains(up.as_str()))
}

fn merge_single_stream_phases(dag: &Dag, phases: Vec<Phase>) -> Vec<Phase> {
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
                    && !reads_sensor_of(dag, &phase, prev)
                    && !collects_from(dag, &phase, prev)
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
    selection: Option<&crate::selection::PartitionSelection>,
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
            let steps = chain_to_steps(dag, &topology.chains[chain_idx], selection);
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
                                    weight: step.weight,
                                    retry_backoff_seconds: step.retry_backoff_seconds,
                                    partition_keys: chunk.to_vec(),
                                    param_types: step.param_types.clone(),
                                    return_type: step.return_type,
                                    keyed: step.keyed.clone(),
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
                                    weight: step.weight,
                                    retry_backoff_seconds: step.retry_backoff_seconds,
                                    partition_keys: pks,
                                    param_types: step.param_types.clone(),
                                    return_type: step.return_type,
                                    keyed: step.keyed.clone(),
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
fn chain_to_steps(
    dag: &Dag,
    chain: &Chain,
    selection: Option<&crate::selection::PartitionSelection>,
) -> Vec<StreamStep> {
    let mut steps = Vec::new();

    for node_id in &chain.nodes {
        let node = dag.get_node(node_id).unwrap();
        let mut inputs = node.resolved_inputs.clone();
        for (k, v) in &node.resolved_collected {
            inputs.insert(k.clone(), v.clone());
        }

        // Check for static partitions — expand into N steps.
        let mut static_partitions = collect_static_partitions(&node.extracted.partitions);
        if let Some(selection) = selection {
            selection.prefilter(&mut static_partitions);
        }
        // Check for derived partitions — mark for resolution at dispatch time.
        let derived_partitions = collect_derived_partitions(&node.extracted.partitions);
        let serializer: Option<Arc<str>> = node
            .extracted
            .artifact_serializer
            .map(|s| Arc::from(format!("{s:?}").to_lowercase().as_str()));
        let sinks: Vec<crate::model::SinkDecl> = node.extracted.sinks.to_vec();
        let timeout_seconds = node.extracted.timeout_seconds;
        let retries = node.extracted.retries;
        let weight = node.extracted.weight;
        let retry_backoff_seconds = node.extracted.retry_backoff_seconds;
        let function_name: Arc<str> = Arc::from(node.function_name());
        let source_file: Arc<str> = Arc::from(node.source_file());
        let param_types = node.extracted.param_types.clone();
        let return_type = node.extracted.return_type;
        // An unpartitioned node a `partitions_from(...)` reads: the worker checks what it
        // returns can be read for keys (a list, or a dict with JSON values).
        let mut keyed = crate::keyed::StepKeyed {
            order: None,
            sources: None,
            is_source: node.extracted.partitions.is_empty() && dag.feeds_partitions(node_id),
            no_keys: false,
        };

        if !static_partitions.is_empty() {
            let combos = expand_partition_combos(&static_partitions);
            let pks: Vec<PartitionKey> = combos.into_iter().map(PartitionKey::from).collect();
            if node.extracted.batch.is_some() {
                keyed.remember_order(&pks);
            }
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
                weight,
                retry_backoff_seconds,
                partition_keys: pks,
                param_types: param_types.clone(),
                return_type,
                keyed: keyed.clone(),
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
                weight,
                retry_backoff_seconds,
                partition_keys: Vec::new(),
                param_types: param_types.clone(),
                return_type,
                keyed: keyed.clone(),
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
                weight,
                retry_backoff_seconds,
                partition_keys: Vec::new(),
                param_types,
                return_type,
                keyed: keyed.clone(),
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
    let mut ordered: Vec<_> = dims.iter().collect();
    ordered.sort_by_key(|(dim, _)| *dim);
    for (dim, values) in ordered {
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
    plan_from_dag_narrowed(dag, config, None)
}

/// [`plan_from_dag`] with the [`plan_narrowed`] pre-filter.
pub fn plan_from_dag_narrowed(
    dag: &Dag,
    config: &ResourceConfig,
    selection: Option<&crate::selection::PartitionSelection>,
) -> ExecutionPlan {
    let topology = decompose(dag);
    plan_narrowed(dag, &topology, config, selection)
}

/// Plan only a dependency-closed set of nodes; unrelated partition sets are never expanded.
pub(crate) fn plan_for_nodes(
    dag: &Dag,
    nodes: &[&str],
    config: &ResourceConfig,
    selection: Option<&crate::selection::PartitionSelection>,
) -> ExecutionPlan {
    let topology = decompose_ordered(dag, dag.order_for(nodes.iter().copied()));
    plan_narrowed(dag, &topology, config, selection)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests;
