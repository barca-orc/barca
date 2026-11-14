use super::*;
use barca_model::model::*;
use smallvec::SmallVec;

fn build_test_dag(specs: &[(&str, &[&str])]) -> Dag {
    build_dag_with_sensors(specs, &[])
}

/// Like `build_test_dag`, with the nodes named in `sensors` declared as `@sensor`.
fn build_dag_with_sensors(specs: &[(&str, &[&str])], sensors: &[&str]) -> Dag {
    build_dag_with(specs, sensors, &[])
}

/// Like `build_dag_with_sensors`, with the assets named in `manual` declared
/// `freshness=Manual`.
fn build_dag_with(specs: &[(&str, &[&str])], sensors: &[&str], manual: &[&str]) -> Dag {
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
                freshness: if sensor || manual.contains(name) {
                    Freshness::Manual
                } else {
                    Freshness::Always
                },
                max_failures: None,
                schedule_timeout: None,
                inputs,
                partitions: HashMap::new(),
                sinks: SmallVec::new(),
                timeout_seconds: 300,
                retries: 1,
                cutoff: false,
                weight: 1,
                deterministic: true,
                retry_backoff_seconds: 0.0,
                description: None,
                tags: HashMap::new(),
                is_unsafe: false,
                source_file: "test.py".to_string(),
                byte_offset: 0,
                line: 0,
                source_text: String::new(),
                cone_hash: String::new(),
                cone_parts: Vec::new(),
                artifact_serializer: None,
                param_types: HashMap::new(),
                return_type: None,
                parallel_calls: Vec::new(),
                env: Vec::new(),
                unused_inputs: Vec::new(),
                env_reads: Vec::new(),
                inherited_partitions: Vec::new(),
                batch: None,
                schedule_batch: None,
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
        max_failures: None,
        schedule_timeout: None,
        inputs: SmallVec::new(),
        partitions: HashMap::new(),
        sinks: SmallVec::new(),
        timeout_seconds: 300,
        retries: 1,
        cutoff: false,
        weight: 1,
        deterministic: true,
        retry_backoff_seconds: 0.0,
        description: None,
        tags: HashMap::new(),
        is_unsafe: false,
        source_file: "test.py".to_string(),
        byte_offset: 0,
        line: 0,
        source_text: String::new(),
        cone_hash: String::new(),
        cone_parts: Vec::new(),
        artifact_serializer: None,
        param_types: HashMap::new(),
        return_type: None,
        parallel_calls: Vec::new(),
        env: Vec::new(),
        unused_inputs: Vec::new(),
        env_reads: Vec::new(),
        inherited_partitions: Vec::new(),
        batch: None,
        schedule_batch: None,
    };
    extracted.sinks.push(SinkDecl {
        path: "exports/out.parquet".to_string(),
        serializer: Some(SerializerKind::Parquet),
        ..Default::default()
    });
    extracted.sinks.push(SinkDecl {
        path: "s3://bucket/model.pkl".to_string(),
        serializer: None,
        ..Default::default()
    });
    let dag = Dag::build(std::slice::from_ref(&extracted)).unwrap();
    let chain = Chain {
        nodes: vec!["test.py:a".to_string()],
    };
    let steps = chain_to_steps(&dag, &chain, None);
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
    let mut specs: Vec<(&str, Vec<&str>)> = vec![("a", vec![]), ("b", vec!["a"]), ("c", vec!["b"])];
    let d_names: Vec<String> = (0..10).map(|i| format!("d{i}")).collect();
    for name in &d_names {
        specs.push((name, vec!["c"]));
    }
    let d_refs: Vec<&str> = d_names.iter().map(|s| s.as_str()).collect();
    specs.push(("e", d_refs));
    let specs_ref: Vec<(&str, &[&str])> = specs.iter().map(|(n, d)| (*n, d.as_slice())).collect();

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
            max_failures: None,
            schedule_timeout: None,
            inputs: SmallVec::new(),
            partitions: HashMap::new(),
            sinks: SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            cutoff: false,
            weight: 1,
            deterministic: true,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            line: 0,
            source_text: String::new(),
            cone_hash: String::new(),
            cone_parts: Vec::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
            unused_inputs: Vec::new(),
            env_reads: Vec::new(),
            inherited_partitions: Vec::new(),
            batch: None,
            schedule_batch: None,
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
            let partition_map: HashMap<String, barca_model::model::PartitionSpec> = partitions
                .iter()
                .map(|(dim, vals)| {
                    (
                        dim.to_string(),
                        barca_model::model::PartitionSpec::Static {
                            values: vals
                                .iter()
                                .map(|v| barca_model::model::PartitionValue::Str(v.to_string()))
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
                max_failures: None,
                schedule_timeout: None,
                inputs,
                partitions: partition_map,
                sinks: SmallVec::new(),
                timeout_seconds: 300,
                retries: 1,
                cutoff: false,
                weight: 1,
                deterministic: true,
                retry_backoff_seconds: 0.0,
                description: None,
                tags: HashMap::new(),
                is_unsafe: false,
                source_file: "test.py".to_string(),
                byte_offset: 0,
                line: 0,
                source_text: String::new(),
                cone_hash: String::new(),
                cone_parts: Vec::new(),
                artifact_serializer: None,
                param_types: HashMap::new(),
                return_type: None,
                parallel_calls: Vec::new(),
                env: Vec::new(),
                unused_inputs: Vec::new(),
                env_reads: Vec::new(),
                inherited_partitions: Vec::new(),
                batch: None,
                schedule_batch: None,
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
    let all_steps: Vec<&StreamStep> = p.phases[0].streams.iter().flat_map(|s| &s.steps).collect();
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
    a_node_runs_in_an_earlier_phase_than_its_consumers(true);
}

/// #548: so does a `Manual` asset, whose output is folded into its consumers' run hashes
/// the same way (a refresh by hand gives it new bytes under the same run hash).
#[test]
fn a_manual_asset_runs_in_an_earlier_phase_than_its_consumers() {
    a_node_runs_in_an_earlier_phase_than_its_consumers(false);
}

/// `etag` in an earlier phase than everything that reads it: a sensor, or a `Manual` asset.
fn a_node_runs_in_an_earlier_phase_than_its_consumers(sensor: bool) {
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
            let dag = if sensor {
                build_dag_with_sensors(specs, &["etag"])
            } else {
                build_dag_with(specs, &[], &["etag"])
            };
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
        max_failures: None,
        schedule_timeout: None,
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
        cutoff: false,
        weight: 1,
        deterministic: true,
        retry_backoff_seconds: 0.0,
        description: None,
        tags: HashMap::new(),
        is_unsafe: false,
        source_file: "test.py".to_string(),
        byte_offset: 0,
        line: 0,
        source_text: String::new(),
        cone_hash: String::new(),
        cone_parts: Vec::new(),
        artifact_serializer: None,
        param_types: HashMap::new(),
        return_type: None,
        parallel_calls: Vec::new(),
        env: Vec::new(),
        unused_inputs: Vec::new(),
        env_reads: Vec::new(),
        inherited_partitions: Vec::new(),
        batch: None,
        schedule_batch: None,
    };
    let collector = ExtractedNode {
        kind: NodeKind::Asset,
        function_name: "sink".to_string(),
        explicit_name: None,
        freshness: Freshness::Always,
        max_failures: None,
        schedule_timeout: None,
        inputs: SmallVec::from_vec(vec![DeclaredInput {
            param_name: "data".to_string(),
            upstream: NodeRef::FunctionName("source".to_string()),
            collected: true,
        }]),
        partitions: HashMap::new(),
        sinks: SmallVec::new(),
        timeout_seconds: 300,
        retries: 1,
        cutoff: false,
        weight: 1,
        deterministic: true,
        retry_backoff_seconds: 0.0,
        description: None,
        tags: HashMap::new(),
        is_unsafe: false,
        source_file: "test.py".to_string(),
        byte_offset: 0,
        line: 0,
        source_text: String::new(),
        cone_hash: String::new(),
        cone_parts: Vec::new(),
        artifact_serializer: None,
        param_types: HashMap::new(),
        return_type: None,
        parallel_calls: Vec::new(),
        env: Vec::new(),
        unused_inputs: Vec::new(),
        env_reads: Vec::new(),
        inherited_partitions: Vec::new(),
        batch: None,
        schedule_batch: None,
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
        max_failures: None,
        schedule_timeout: None,
        inputs: SmallVec::new(),
        partitions: HashMap::new(),
        sinks: SmallVec::new(),
        timeout_seconds: 300,
        retries: 1,
        cutoff: false,
        weight: 1,
        deterministic: true,
        retry_backoff_seconds: 0.0,
        description: None,
        tags: HashMap::new(),
        is_unsafe: false,
        source_file: "test.py".to_string(),
        byte_offset: 0,
        line: 0,
        source_text: String::new(),
        cone_hash: String::new(),
        cone_parts: Vec::new(),
        artifact_serializer: None,
        param_types: HashMap::new(),
        return_type: None,
        parallel_calls: Vec::new(),
        env: Vec::new(),
        unused_inputs: Vec::new(),
        env_reads: Vec::new(),
        inherited_partitions: Vec::new(),
        batch: None,
        schedule_batch: None,
    };
    let fetch_prices = ExtractedNode {
        kind: NodeKind::Asset,
        function_name: "fetch_prices".to_string(),
        explicit_name: None,
        freshness: Freshness::Always,
        max_failures: None,
        schedule_timeout: None,
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
        cutoff: false,
        weight: 1,
        deterministic: true,
        retry_backoff_seconds: 0.0,
        description: None,
        tags: HashMap::new(),
        is_unsafe: false,
        source_file: "test.py".to_string(),
        byte_offset: 0,
        line: 0,
        source_text: String::new(),
        cone_hash: String::new(),
        cone_parts: Vec::new(),
        artifact_serializer: None,
        param_types: HashMap::new(),
        return_type: None,
        parallel_calls: Vec::new(),
        env: Vec::new(),
        unused_inputs: Vec::new(),
        env_reads: Vec::new(),
        inherited_partitions: Vec::new(),
        batch: None,
        schedule_batch: None,
    };
    let aggregate = ExtractedNode {
        kind: NodeKind::Asset,
        function_name: "aggregate".to_string(),
        explicit_name: None,
        freshness: Freshness::Always,
        max_failures: None,
        schedule_timeout: None,
        inputs: SmallVec::from_vec(vec![DeclaredInput {
            param_name: "reports".to_string(),
            upstream: NodeRef::FunctionName("fetch_prices".to_string()),
            collected: true,
        }]),
        partitions: HashMap::new(),
        sinks: SmallVec::new(),
        timeout_seconds: 300,
        retries: 1,
        cutoff: false,
        weight: 1,
        deterministic: true,
        retry_backoff_seconds: 0.0,
        description: None,
        tags: HashMap::new(),
        is_unsafe: false,
        source_file: "test.py".to_string(),
        byte_offset: 0,
        line: 0,
        source_text: String::new(),
        cone_hash: String::new(),
        cone_parts: Vec::new(),
        artifact_serializer: None,
        param_types: HashMap::new(),
        return_type: None,
        parallel_calls: Vec::new(),
        env: Vec::new(),
        unused_inputs: Vec::new(),
        env_reads: Vec::new(),
        inherited_partitions: Vec::new(),
        batch: None,
        schedule_batch: None,
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

/// #331, #325: a `collect()` fan-in is never in the same phase as the node it collects,
/// whatever the pool size and however many keys there are. With one worker, or with one
/// key, the producer's phase and the fan-in's phase are one stream each, and
/// `merge_single_stream_phases` used to fuse them: the fan-in then started with no
/// collected input (`TypeError: summary() missing 1 required positional argument`).
#[test]
fn a_fan_in_is_never_fused_into_the_phase_it_collects_from() {
    for keys in 1..=5usize {
        let listed: Vec<String> = (0..keys).map(|k| format!("\"k{k}\"")).collect();
        let source = format!(
            "@asset()\ndef base() -> int:\n    return 1\n\n\n\
                 @asset(inputs={{\"b\": base}}, partitions={{\"k\": partitions([{}])}})\n\
                 def part(k: str, b: int) -> dict:\n    return {{}}\n\n\n\
                 @asset(partitions={{\"k\": partitions_from(part)}})\n\
                 def per_key(k: str, part: dict) -> dict:\n    return part\n\n\n\
                 @asset(inputs={{\"parts\": collect(part)}})\n\
                 def summary(parts: list) -> int:\n    return len(parts)\n\n\n\
                 @asset(inputs={{\"s\": summary, \"rows\": collect(per_key)}})\n\
                 def report(s: int, rows: list) -> int:\n    return s\n",
            listed.join(", ")
        );
        let nodes = crate::parse::extract_nodes(&source, "t.py").unwrap();
        let dag = Dag::build(&nodes).unwrap();
        for pool_size in 1..=8usize {
            let config = ResourceConfig {
                pool_size,
                concurrency_groups: HashMap::new(),
            };
            let plan = plan_from_dag(&dag, &config);
            let phase_of = |name: &str| -> Vec<usize> {
                plan.phases
                    .iter()
                    .enumerate()
                    .filter(|(_, phase)| {
                        phase
                            .streams
                            .iter()
                            .flat_map(|s| &s.steps)
                            .any(|st| st.step_id.base_id() == format!("t.py:{name}"))
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            let context = format!("{keys} keys, pool of {pool_size}");
            for (fan_in, collected) in [
                ("summary", "part"),
                ("report", "per_key"),
                ("report", "summary"),
            ] {
                let consumer = phase_of(fan_in);
                let producer = phase_of(collected);
                assert_eq!(consumer.len(), 1, "{context}: {fan_in}");
                assert!(!producer.is_empty(), "{context}: {collected}");
                if collected != "summary" {
                    assert!(
                        producer.iter().all(|p| *p < consumer[0]),
                        "{context}: `{fan_in}` (phase {consumer:?}) collects `{collected}` \
                             (phase {producer:?}) and must be in a later phase"
                    );
                }
            }
            // Every step is planned exactly once per key.
            assert_eq!(plan.total_steps, 3 + 2 * keys, "{context}");
        }
    }
}
