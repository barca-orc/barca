//! Types and utilities for partition expansion and input resolution.
//!
//! Execution (worker spawning, protocol parsing) lives in `io_loop`.

use crate::planner::{Phase, StreamStep, WorkerStream, expand_partition_combos};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Reference to a materialized artifact on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputRef {
    pub path: String,
    pub format: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
}

/// A structured failure reported by a worker for a single step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepError {
    pub error_type: String,
    pub message: String,
    /// User-relevant traceback (barca-internal frames filtered out).
    pub traceback: String,
    /// Number of attempts made before this failure became permanent.
    pub attempts: u32,
}

/// A permanently-failed step (retry budget exhausted), for the metadata DB.
#[derive(Debug, Clone, PartialEq)]
pub struct StepFailure {
    pub node_id: String,
    pub error: StepError,
}

/// The materialized output a `partitions_from` source name refers to.
fn find_partition_source<'a>(
    all_outputs: &'a HashMap<String, OutputRef>,
    source_name: &str,
) -> Option<&'a OutputRef> {
    all_outputs
        .iter()
        .find(|(k, _)| k.ends_with(&format!(":{source_name}")) || k.as_str() == source_name)
        .map(|(_, v)| v)
}

/// The partition-source outputs [`expand_pending_partitions`] will read from
/// disk for this phase (so callers can make sure they are local first).
pub fn partition_sources<'a>(
    phase: &Phase,
    all_outputs: &'a HashMap<String, OutputRef>,
) -> Vec<&'a OutputRef> {
    phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .flat_map(|st| st.pending_partitions.values())
        .filter_map(|source| find_partition_source(all_outputs, source))
        .collect()
}

/// Expand steps with pending_partitions using materialized source outputs.
/// Returns None if no expansion needed, or a new Phase with expanded steps.
pub fn expand_pending_partitions(
    phase: &Phase,
    all_outputs: &HashMap<String, OutputRef>,
    pool_size: usize,
) -> Option<Phase> {
    let has_pending = phase
        .streams
        .iter()
        .any(|s| s.steps.iter().any(|st| !st.pending_partitions.is_empty()));

    if !has_pending {
        return None;
    }

    let mut all_expanded_steps: Vec<StreamStep> = Vec::new();
    let mut passthrough_steps: Vec<StreamStep> = Vec::new();

    for stream in &phase.streams {
        for step in &stream.steps {
            if step.pending_partitions.is_empty() {
                passthrough_steps.push(step.clone());
                continue;
            }

            let mut dim_values: HashMap<String, Vec<String>> = HashMap::new();
            for (dim, source_name) in &step.pending_partitions {
                if let Some(oref) = find_partition_source(all_outputs, source_name) {
                    if oref.format != "json" {
                        eprintln!(
                            "[barca] Error: partition source '{}' must be JSON format, got '{}'",
                            source_name, oref.format
                        );
                        continue;
                    }
                    // Read the JSON artifact file from disk (always local: a
                    // store-backed cache hit is fetched before expansion).
                    let json_str = match std::fs::read_to_string(&oref.path) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!(
                                "[barca] Error: failed to read partition artifact '{}': {e}",
                                oref.path
                            );
                            continue;
                        }
                    };
                    let parsed: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let values: Vec<String> = match parsed {
                        serde_json::Value::Array(arr) => arr
                            .iter()
                            .filter_map(|v| match v {
                                serde_json::Value::String(s) => Some(s.clone()),
                                serde_json::Value::Number(n) => Some(n.to_string()),
                                _ => None,
                            })
                            .collect(),
                        _ => {
                            eprintln!(
                                "[barca] Warning: partition source '{}' did not return an array",
                                source_name
                            );
                            continue;
                        }
                    };
                    dim_values.insert(dim.clone(), values);
                } else {
                    eprintln!(
                        "[barca] Warning: partition source '{}' not found in outputs",
                        source_name
                    );
                }
            }

            if dim_values.is_empty() {
                passthrough_steps.push(step.clone());
                continue;
            }

            let combos = expand_partition_combos(&dim_values);
            let pks: Vec<crate::PartitionKey> =
                combos.into_iter().map(crate::PartitionKey::from).collect();
            all_expanded_steps.push(StreamStep {
                step_id: crate::StepId::unpartitioned(step.step_id.base.clone()),
                kind: step.kind,
                function_name: step.function_name.clone(),
                source_file: step.source_file.clone(),
                inputs: step.inputs.clone(),
                pending_partitions: HashMap::new(),
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

    // Build work units: expanded steps (with partition_keys) + passthrough steps.
    let mut work_units: Vec<Vec<StreamStep>> = Vec::new();
    if !passthrough_steps.is_empty() {
        work_units.push(passthrough_steps);
    }
    // Split each expanded step's partition_keys across streams.
    for step in all_expanded_steps {
        let total_pks = step.partition_keys.len();
        if total_pks == 0 {
            work_units.push(vec![step]);
        } else {
            let chunk_size = total_pks.div_ceil(pool_size);
            for chunk in step.partition_keys.chunks(chunk_size) {
                work_units.push(vec![StreamStep {
                    step_id: step.step_id.clone(),
                    kind: step.kind,
                    function_name: step.function_name.clone(),
                    source_file: step.source_file.clone(),
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

    // Distribute work units across streams via bin-packing.
    let num_streams = work_units.len().min(pool_size).max(1);
    let mut streams: Vec<Vec<StreamStep>> = vec![Vec::new(); num_streams];
    let mut sizes: Vec<usize> = vec![0; num_streams];

    for unit in work_units {
        let weight: usize = unit
            .iter()
            .map(|st| {
                if st.partition_keys.is_empty() {
                    1
                } else {
                    st.partition_keys.len()
                }
            })
            .sum();
        let target = sizes
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| *s)
            .map(|(i, _)| i)
            .unwrap_or(0);
        sizes[target] += weight;
        streams[target].extend(unit);
    }

    let worker_streams: Vec<WorkerStream> = streams
        .into_iter()
        .enumerate()
        .filter(|(_, s)| !s.is_empty())
        .map(|(i, steps)| WorkerStream {
            stream_id: format!("expanded-w{i}"),
            steps,
        })
        .collect();

    Some(Phase {
        reason: phase.reason.clone(),
        streams: worker_streams,
    })
}

/// A provided input — either a single artifact or a collected list of partition artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProvidedInput {
    Single(OutputRef),
    Collected(Vec<OutputRef>),
}

/// Determine what values need to be provided to workers in this phase.
///
/// Only data edges (`Direct`/`Collect`, surfaced via `step.inputs`) produce
/// provided inputs. `After` edges carry no data — they never appear in
/// `step.inputs`, so an after-ordered step receives no artifact from them.
pub fn build_provided_inputs(
    phase: &Phase,
    all_outputs: &HashMap<String, OutputRef>,
) -> HashMap<String, ProvidedInput> {
    let mut provided: HashMap<String, ProvidedInput> = HashMap::new();

    for stream in &phase.streams {
        for step in &stream.steps {
            let suffix = if step.step_id.partition.is_empty() {
                String::new()
            } else {
                format!("[{}]", step.step_id.partition.suffix())
            };

            for upstream_id in step.inputs.values() {
                // Direct base match.
                if let Some(output) = all_outputs.get(upstream_id) {
                    provided
                        .entry(upstream_id.clone())
                        .or_insert_with(|| ProvidedInput::Single(output.clone()));
                    continue;
                }
                // Partition-aligned match (old-style: step_id has partition suffix).
                if !suffix.is_empty() {
                    let aligned_id = format!("{upstream_id}{suffix}");
                    if let Some(output) = all_outputs.get(&aligned_id) {
                        provided
                            .entry(aligned_id)
                            .or_insert_with(|| ProvidedInput::Single(output.clone()));
                        continue;
                    }
                }
                // Late expansion: step has partition_keys — provide all aligned upstream
                // outputs so the worker can resolve per-partition inputs.
                if !step.partition_keys.is_empty() {
                    let mut found_any = false;
                    for pk in &step.partition_keys {
                        let pk_suffix = format!("[{}]", pk.suffix());
                        let aligned_id = format!("{upstream_id}{pk_suffix}");
                        if let Some(output) = all_outputs.get(&aligned_id) {
                            provided
                                .entry(aligned_id)
                                .or_insert_with(|| ProvidedInput::Single(output.clone()));
                            found_any = true;
                        }
                    }
                    if found_any {
                        continue;
                    }
                }
                // Fan-in collect(): upstream is partitioned but consumer is unpartitioned.
                // Gather all partition-suffixed outputs for this base id.
                let prefix = format!("{upstream_id}[");
                let mut collected: Vec<OutputRef> = all_outputs
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .map(|(_, v)| v.clone())
                    .collect();
                if !collected.is_empty() {
                    collected.sort_by(|a, b| a.path.cmp(&b.path));
                    provided
                        .entry(upstream_id.clone())
                        .or_insert(ProvidedInput::Collected(collected));
                }
            }
        }
    }

    provided
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::PhaseReason;
    use crate::{NodeKind, PartitionKey, StepId};
    use std::sync::Arc;

    fn test_output_ref(path: &str, format: &str) -> OutputRef {
        OutputRef {
            path: path.to_string(),
            format: format.to_string(),
            size_bytes: 100,
            elapsed_seconds: None,
        }
    }

    #[test]
    fn build_provided_inputs_direct_match() {
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:b"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("b"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::from([("a_val".to_string(), "f:a".to_string())]),
                    pending_partitions: HashMap::new(),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut all_outputs = HashMap::new();
        all_outputs.insert("f:a".to_string(), test_output_ref("f--a.json", "json"));

        let provided = build_provided_inputs(&phase, &all_outputs);
        match &provided["f:a"] {
            ProvidedInput::Single(oref) => assert_eq!(oref.path, "f--a.json"),
            other => panic!("expected Single, got {other:?}"),
        }
    }

    #[test]
    fn build_provided_inputs_partition_aligned() {
        let pk = PartitionKey::from(HashMap::from([("t".to_string(), "X".to_string())]));
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::new("f:b", pk),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("b"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::from([("a_val".to_string(), "f:a".to_string())]),
                    pending_partitions: HashMap::new(),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut all_outputs = HashMap::new();
        all_outputs.insert(
            "f:a[t=X]".to_string(),
            test_output_ref("f--a_t_X.json", "json"),
        );

        let provided = build_provided_inputs(&phase, &all_outputs);
        match &provided["f:a[t=X]"] {
            ProvidedInput::Single(oref) => assert_eq!(oref.path, "f--a_t_X.json"),
            other => panic!("expected Single, got {other:?}"),
        }
    }

    #[test]
    fn expand_pending_partitions_no_pending_returns_none() {
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:a"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("a"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::new(),
                    pending_partitions: HashMap::new(),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        assert!(
            expand_pending_partitions(&phase, &HashMap::<String, OutputRef>::new(), 4).is_none()
        );
    }

    #[test]
    fn expand_pending_partitions_expands_derived() {
        // Create a temporary JSON artifact file containing partition values.
        let dir = tempfile::tempdir().unwrap();
        let artifact_path = dir.path().join("regions.json");
        std::fs::write(&artifact_path, r#"["us","eu"]"#).unwrap();

        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:transform"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("transform"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::new(),
                    pending_partitions: HashMap::from([(
                        "region".to_string(),
                        "get_regions".to_string(),
                    )]),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut outputs = HashMap::new();
        outputs.insert(
            "f:get_regions".to_string(),
            OutputRef {
                path: artifact_path.to_string_lossy().to_string(),
                format: "json".to_string(),
                size_bytes: 12,
                elapsed_seconds: None,
            },
        );

        let expanded = expand_pending_partitions(&phase, &outputs, 4).unwrap();
        // Late expansion: expanded steps have partition_keys, not individual step_ids.
        let all_pks: Vec<String> = expanded
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .flat_map(|st| {
                st.partition_keys
                    .iter()
                    .map(|pk| pk.display_id(&st.step_id.base))
            })
            .collect();
        assert!(all_pks.contains(&"f:transform[region=eu]".to_string()));
        assert!(all_pks.contains(&"f:transform[region=us]".to_string()));
        assert_eq!(all_pks.len(), 2);
    }

    #[test]
    fn expand_pending_partitions_carries_sinks() {
        let dir = tempfile::tempdir().unwrap();
        let artifact_path = dir.path().join("regions.json");
        std::fs::write(&artifact_path, r#"["us","eu"]"#).unwrap();

        let sink = crate::model::SinkDecl {
            path: "exports/out.parquet".to_string(),
            serializer: Some(crate::model::SerializerKind::Parquet),
        };
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:transform"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("transform"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::new(),
                    pending_partitions: HashMap::from([(
                        "region".to_string(),
                        "get_regions".to_string(),
                    )]),
                    serializer: None,
                    sinks: vec![sink.clone()],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut outputs = HashMap::new();
        outputs.insert(
            "f:get_regions".to_string(),
            test_output_ref(&artifact_path.to_string_lossy(), "json"),
        );

        let expanded = expand_pending_partitions(&phase, &outputs, 4).unwrap();
        let expanded_steps: Vec<&StreamStep> = expanded
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .filter(|st| !st.partition_keys.is_empty())
            .collect();
        assert!(!expanded_steps.is_empty());
        for st in expanded_steps {
            assert_eq!(st.sinks, vec![sink.clone()]);
        }
    }

    fn pending_phase(source: &str) -> Phase {
        Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:transform"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("transform"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::new(),
                    pending_partitions: HashMap::from([("region".to_string(), source.to_string())]),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        }
    }

    #[test]
    fn partition_sources_finds_the_outputs_expansion_will_read() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "f:get_regions".to_string(),
            test_output_ref("/w/a/regions.json", "json"),
        );
        outputs.insert(
            "f:other".to_string(),
            test_output_ref("/w/a/o.json", "json"),
        );
        let found = partition_sources(&pending_phase("get_regions"), &outputs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/w/a/regions.json");
        assert!(partition_sources(&pending_phase("missing"), &outputs).is_empty());
    }

    #[test]
    fn expand_pending_partitions_unreadable_source_does_not_expand() {
        // A partition source whose file can't be read falls through as
        // passthrough (no expansion, loud error).
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:transform"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("transform"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::new(),
                    pending_partitions: HashMap::from([(
                        "region".to_string(),
                        "get_regions".to_string(),
                    )]),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut outputs = HashMap::new();
        outputs.insert(
            "f:get_regions".to_string(),
            test_output_ref(
                "abfss://cont@acct.dfs.core.windows.net/arts/regions.json",
                "json",
            ),
        );

        let expanded = expand_pending_partitions(&phase, &outputs, 4).unwrap();
        for st in expanded.streams.iter().flat_map(|s| &s.steps) {
            assert!(
                st.partition_keys.is_empty(),
                "remote source must not expand"
            );
        }
    }

    #[test]
    fn expand_pending_partitions_reads_json_artifact() {
        // Create a temporary JSON artifact file containing partition values.
        let dir = tempfile::tempdir().unwrap();
        let artifact_path = dir.path().join("regions.json");
        std::fs::write(&artifact_path, r#"["us","eu","ap"]"#).unwrap();

        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:transform"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("transform"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::new(),
                    pending_partitions: HashMap::from([(
                        "region".to_string(),
                        "get_regions".to_string(),
                    )]),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "f:get_regions".to_string(),
            OutputRef {
                path: artifact_path.to_string_lossy().to_string(),
                format: "json".to_string(),
                size_bytes: 14,
                elapsed_seconds: None,
            },
        );

        let expanded = expand_pending_partitions(&phase, &outputs, 4).unwrap();
        // Late expansion: expanded steps have partition_keys, not individual step_ids.
        let all_pks: Vec<String> = expanded
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .flat_map(|st| {
                st.partition_keys
                    .iter()
                    .map(|pk| pk.display_id(&st.step_id.base))
            })
            .collect();
        assert!(all_pks.contains(&"f:transform[region=us]".to_string()));
        assert!(all_pks.contains(&"f:transform[region=eu]".to_string()));
        assert!(all_pks.contains(&"f:transform[region=ap]".to_string()));
        assert_eq!(all_pks.len(), 3);
    }

    #[test]
    fn build_provided_inputs_with_output_ref_direct() {
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::unpartitioned("f:b"),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("b"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::from([("data".to_string(), "f:a".to_string())]),
                    pending_partitions: HashMap::new(),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut all_outputs: HashMap<String, OutputRef> = HashMap::new();
        all_outputs.insert(
            "f:a".to_string(),
            OutputRef {
                path: ".barca/artifacts/f--a.json".to_string(),
                format: "json".to_string(),
                size_bytes: 100,
                elapsed_seconds: None,
            },
        );

        let provided = build_provided_inputs(&phase, &all_outputs);
        match &provided["f:a"] {
            ProvidedInput::Single(oref) => assert_eq!(oref.path, ".barca/artifacts/f--a.json"),
            _ => panic!("expected Single"),
        }
    }

    #[test]
    fn build_provided_inputs_with_output_ref_partition_aligned() {
        let pk = PartitionKey::from(HashMap::from([("t".to_string(), "X".to_string())]));
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![StreamStep {
                    step_id: StepId::new("f:b", pk),
                    kind: NodeKind::Asset,
                    function_name: Arc::from("b"),
                    source_file: Arc::from("f"),
                    inputs: HashMap::from([("data".to_string(), "f:a".to_string())]),
                    pending_partitions: HashMap::new(),
                    serializer: None,
                    sinks: vec![],
                    run_hashes: HashMap::new(),
                    timeout_seconds: 300,
                    retries: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                }],
            }],
        };
        let mut all_outputs: HashMap<String, OutputRef> = HashMap::new();
        all_outputs.insert(
            "f:a[t=X]".to_string(),
            OutputRef {
                path: ".barca/artifacts/f--a_t_X.parquet".to_string(),
                format: "parquet".to_string(),
                size_bytes: 5000,
                elapsed_seconds: None,
            },
        );

        let provided = build_provided_inputs(&phase, &all_outputs);
        match &provided["f:a[t=X]"] {
            ProvidedInput::Single(oref) => assert_eq!(oref.format, "parquet"),
            _ => panic!("expected Single"),
        }
    }
}
