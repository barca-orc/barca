//! Types and utilities for partition expansion and input resolution.
//!
//! Execution (worker spawning, protocol parsing) lives in `io_loop`.

use crate::planner::{Phase, StreamStep, WorkerStream, expand_partition_combos};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Reference to a materialized artifact on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct OutputRef {
    pub path: String,
    pub format: String,
    // serde_json emits u64/i64 as JSON numbers; map to `number`, not bigint.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub elapsed_seconds: Option<f64>,
    /// SHA-256 of the artifact's bytes, when one was recorded. Never sent to
    /// workers or printed: it is what a local copy is checked against.
    #[serde(default, skip_serializing)]
    pub content_hash: Option<String>,
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
    /// When the first attempt started, as Unix nanoseconds. `None` when the failure was not
    /// timed (an artifact upload that failed after the step itself succeeded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix_ns: Option<u64>,
    /// Wall seconds from the first attempt's start to the permanent failure, retries and
    /// backoff included. A step that timed out reports about its timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
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

/// What [`expand_pending_partitions`] made of a phase.
#[derive(Debug, Default)]
pub struct Expansion {
    /// Complete membership counts before an execution-time key filter.
    pub(crate) key_counts: HashMap<String, usize>,
    /// The phase with every step that had pending partitions expanded into its keys; `None`
    /// when no step had any.
    pub phase: Option<Phase>,
    /// Steps whose `partitions_from` source could not be read for keys, as (base id, message).
    /// They are left out of `phase`, and the caller reports them as failed steps.
    pub failures: Vec<(String, String)>,
}

/// The node id of the `partitions_from` source `source_name` of `node` (a `PartitionSource`
/// predecessor with that name), else the first recorded output named so.
fn source_id_of(dag: &crate::dag::Dag, node: &str, source_name: &str) -> Option<String> {
    dag.partition_sources(node)
        .into_iter()
        .find(|id| crate::targets::short_name(id) == source_name)
        .map(str::to_string)
}

/// Expand steps with pending_partitions using materialized source outputs.
///
/// A source that returned a list gives its values as keys; one that returned a dict gives its
/// keys, and each key's value is passed to the steps that name the source
/// ([`crate::keyed`]). A step whose source cannot be read is left out and listed in
/// [`Expansion::failures`].
pub fn expand_pending_partitions(
    dag: &crate::dag::Dag,
    phase: &Phase,
    all_outputs: &HashMap<String, OutputRef>,
    pool_size: usize,
) -> Expansion {
    expand_pending_partitions_filtered(dag, phase, all_outputs, pool_size, |_, _| {})
}

pub(crate) fn expand_pending_partitions_filtered(
    dag: &crate::dag::Dag,
    phase: &Phase,
    all_outputs: &HashMap<String, OutputRef>,
    pool_size: usize,
    filter: impl Fn(&str, &mut HashMap<String, Vec<String>>),
) -> Expansion {
    use crate::keyed::{KeyedSource, SourceKeys};
    use std::sync::Arc;

    let has_pending = phase
        .streams
        .iter()
        .any(|s| s.steps.iter().any(|st| !st.pending_partitions.is_empty()));

    if !has_pending {
        return Expansion::default();
    }

    let mut key_counts = HashMap::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut all_expanded_steps: Vec<StreamStep> = Vec::new();
    let mut passthrough_steps: Vec<StreamStep> = Vec::new();
    // Each source is read once per phase, however many steps (and worker chunks) it feeds:
    // source id -> its artifact and keys, or why it cannot be read.
    type Read = Result<
        (
            String,
            Arc<Vec<String>>,
            Option<Arc<HashMap<String, String>>>,
        ),
        String,
    >;
    let mut read: HashMap<String, Read> = HashMap::new();
    // A step's chunks are expanded once: (base id) -> the expanded step.
    let mut seen: HashSet<String> = HashSet::new();

    for stream in &phase.streams {
        for step in &stream.steps {
            if step.pending_partitions.is_empty() {
                passthrough_steps.push(step.clone());
                continue;
            }
            let base = step.step_id.base_id().to_string();
            if !seen.insert(base.clone()) {
                continue;
            }
            let inherited = dag
                .get_node(&base)
                .map(|n| n.extracted.inherited_partitions.clone())
                .unwrap_or_default();

            let mut dim_values: HashMap<String, Vec<String>> = HashMap::new();
            let mut keyed: Vec<KeyedSource> = Vec::new();
            let mut inputs = step.inputs.clone();
            let mut failure: Option<String> = None;
            let mut dims: Vec<(&String, &String)> = step.pending_partitions.iter().collect();
            dims.sort();
            for (dim, source_name) in dims {
                let source_id = source_id_of(dag, &base, source_name).or_else(|| {
                    all_outputs
                        .keys()
                        .find(|k| k.ends_with(&format!(":{source_name}")) || *k == source_name)
                        .cloned()
                });
                let Some(source_id) = source_id else {
                    failure = Some(format!(
                        "partitions_from source '{source_name}' has no output to read keys from"
                    ));
                    break;
                };
                let entry = read.entry(source_id.clone()).or_insert_with(|| {
                    let oref = all_outputs.get(&source_id).ok_or_else(|| {
                        format!(
                            "partitions_from source '{source_name}' has no output to read keys \
                             from"
                        )
                    })?;
                    if oref.format != "json" {
                        return Err(format!(
                            "partitions_from source '{source_name}' was written as {}, not JSON; \
                             a source must return a list of keys or a dict of key -> value that \
                             JSON can write",
                            oref.format
                        ));
                    }
                    // Always local: a store-backed cache hit is fetched before expansion.
                    let text = std::fs::read_to_string(&oref.path).map_err(|e| {
                        format!(
                            "partitions_from source '{source_name}': cannot read {}: {e}",
                            oref.path
                        )
                    })?;
                    match crate::keyed::read_source(&text) {
                        Ok(SourceKeys::List(keys)) => Ok((oref.path.clone(), Arc::new(keys), None)),
                        Ok(SourceKeys::Keyed(keys, hashes)) => {
                            Ok((oref.path.clone(), Arc::new(keys), Some(Arc::new(hashes))))
                        }
                        Err(why) => Err(format!("partitions_from source '{source_name}' {why}")),
                    }
                });
                match entry {
                    Err(why) => {
                        failure = Some(why.clone());
                        break;
                    }
                    Ok((path, keys, hashes)) => {
                        dim_values.insert(dim.clone(), keys.as_ref().clone());
                        let Some(hashes) = hashes else { continue };
                        let param = if inherited.contains(dim) {
                            None
                        } else {
                            // The `inputs=` key that names the source receives the key's value
                            // in place of the whole dict; else the source's name.
                            let named: Option<String> = inputs
                                .iter()
                                .find(|(_, up)| **up == source_id)
                                .map(|(p, _)| p.clone());
                            match named {
                                Some(p) => {
                                    inputs.remove(&p);
                                    Some(p)
                                }
                                None => Some(source_name.clone()),
                            }
                        };
                        keyed.push(KeyedSource {
                            dim: dim.clone(),
                            source_id: source_id.clone(),
                            path: path.clone(),
                            param,
                            hashes: hashes.clone(),
                        });
                    }
                }
            }
            if let Some(why) = failure {
                failures.push((base, why));
                continue;
            }

            key_counts.insert(
                base.clone(),
                dim_values
                    .values()
                    .fold(1usize, |n, values| n.saturating_mul(values.len())),
            );
            filter(&base, &mut dim_values);
            let combos = expand_partition_combos(&dim_values);
            let pks: Vec<crate::PartitionKey> =
                combos.into_iter().map(crate::PartitionKey::from).collect();
            let no_keys = pks.is_empty();
            let mut step_keyed = crate::keyed::StepKeyed {
                sources: (!keyed.is_empty()).then(|| Arc::new(keyed)),
                is_source: step.keyed.is_source,
                no_keys,
                order: None,
            };
            if dag
                .get_node(&base)
                .is_some_and(|n| n.extracted.batch.is_some())
            {
                step_keyed.remember_order(&pks);
            }
            all_expanded_steps.push(StreamStep {
                step_id: crate::StepId::unpartitioned(step.step_id.base.clone()),
                kind: step.kind,
                function_name: step.function_name.clone(),
                source_file: step.source_file.clone(),
                inputs,
                pending_partitions: HashMap::new(),
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
                keyed: step_keyed,
            });
        }
    }

    // Build work units: expanded steps (with partition_keys) + passthrough steps.
    let mut work_units: Vec<Vec<StreamStep>> = Vec::new();
    if !passthrough_steps.is_empty() {
        work_units.push(passthrough_steps);
    }
    // Split each expanded step's partition_keys across streams.
    for mut step in all_expanded_steps {
        let total_pks = step.partition_keys.len();
        if total_pks == 0 {
            work_units.push(vec![step]);
        } else {
            let chunk_size = total_pks.div_ceil(pool_size);
            // The keys move into their chunks: no second copy of 200k keys.
            let mut keys = std::mem::take(&mut step.partition_keys).into_iter();
            loop {
                let chunk: Vec<crate::PartitionKey> = keys.by_ref().take(chunk_size).collect();
                if chunk.is_empty() {
                    break;
                }
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
                    weight: step.weight,
                    retry_backoff_seconds: step.retry_backoff_seconds,
                    partition_keys: chunk,
                    param_types: step.param_types.clone(),
                    return_type: step.return_type,
                    keyed: step.keyed.clone(),
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

    Expansion {
        key_counts,
        phase: Some(Phase {
            reason: phase.reason.clone(),
            streams: worker_streams,
        }),
        failures,
    }
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
    // Nodes this phase produces: a reader in the same phase gets their keys at dispatch time,
    // never a fan-in of what is cached (a `collect()` runs in a later phase than what it reads).
    let in_phase: HashSet<&str> = phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|st| st.step_id.base_id())
        .collect();

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
                if in_phase.contains(upstream_id.as_str()) {
                    continue;
                }
                let prefix = format!("{upstream_id}[");
                let mut collected: Vec<OutputRef> = all_outputs
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .map(|(_, v)| v.clone())
                    .collect();
                // An unpartitioned reader of a node with no key at all (its `partitions_from`
                // source returned none) gets an empty list.
                if !collected.is_empty() || step.partition_keys.is_empty() && suffix.is_empty() {
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

/// Upstream ids that every consumer in this phase reads through a lazy type
/// (`duckdb.DuckDBPyRelation`, `pl.LazyFrame`). One eager consumer needs the
/// whole artifact, so its upstream is left out.
///
/// The ids are node ids, as in [`StreamStep::inputs`]: a reader names a partitioned upstream
/// by its base id (`f:up`), never by one partition (`f:up[k=a]`), whether it reads one
/// partition per key or all of them through `collect()`. So readers of the same upstream
/// always name it the same way, and comparing ids is enough.
pub fn lazily_read_inputs(phase: &Phase) -> HashSet<String> {
    let mut lazy = HashSet::new();
    let mut eager = HashSet::new();
    for step in phase.streams.iter().flat_map(|s| &s.steps) {
        for (param, upstream_id) in &step.inputs {
            if step.param_types.get(param).is_some_and(|t| t.is_lazy()) {
                lazy.insert(upstream_id.clone());
            } else {
                eager.insert(upstream_id.clone());
            }
        }
    }
    lazy.retain(|id| !eager.contains(id));
    lazy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ValueType;
    use crate::planner::PhaseReason;
    use crate::{NodeKind, PartitionKey, StepId};
    use std::sync::Arc;

    /// A DAG without nodes: sources are found by name among the outputs.
    fn no_dag() -> crate::dag::Dag {
        crate::dag::Dag::build(&[]).unwrap()
    }

    fn test_output_ref(path: &str, format: &str) -> OutputRef {
        OutputRef {
            path: path.to_string(),
            format: format.to_string(),
            size_bytes: 100,
            elapsed_seconds: None,
            content_hash: None,
        }
    }

    fn reader_step(id: &str, param: &str, upstream: &str, ty: Option<ValueType>) -> StreamStep {
        StreamStep {
            step_id: StepId::unpartitioned(id),
            kind: NodeKind::Asset,
            function_name: Arc::from(id),
            source_file: Arc::from("f"),
            inputs: HashMap::from([(param.to_string(), upstream.to_string())]),
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            weight: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: vec![],
            param_types: ty
                .map(|t| HashMap::from([(param.to_string(), t)]))
                .unwrap_or_default(),
            return_type: None,
            keyed: Default::default(),
        }
    }

    #[test]
    fn lazily_read_inputs_needs_every_consumer_lazy() {
        let phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![
                    reader_step("f:b", "x", "f:all_lazy", Some(ValueType::DuckDB)),
                    reader_step("f:c", "x", "f:all_lazy", Some(ValueType::PolarsLazy)),
                    reader_step("f:d", "x", "f:mixed", Some(ValueType::DuckDB)),
                    reader_step("f:e", "x", "f:mixed", Some(ValueType::Pandas)),
                    reader_step("f:g", "x", "f:untyped", None),
                ],
            }],
        };
        assert_eq!(
            lazily_read_inputs(&phase),
            HashSet::from(["f:all_lazy".to_string()])
        );
    }

    #[test]
    fn readers_of_a_partitioned_upstream_name_it_by_base_id_so_one_eager_reader_wins() {
        // What the planner emits for a partitioned `f:up` read per key by a lazy step and an
        // eager step (or by an eager `collect()`): every reader's input is the base id.
        let per_key = |id: &str, ty: ValueType| {
            let mut step = reader_step(id, "x", "f:up", Some(ty));
            step.partition_keys = ["a", "b"]
                .iter()
                .map(|k| PartitionKey([("k".to_string(), k.to_string())].into_iter().collect()))
                .collect();
            step
        };
        let phase = |steps: Vec<StreamStep>| Phase {
            reason: PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps,
            }],
        };
        let lazy_only = phase(vec![per_key("f:lazy", ValueType::DuckDB)]);
        assert_eq!(
            lazily_read_inputs(&lazy_only),
            HashSet::from(["f:up".to_string()])
        );
        let mixed = phase(vec![
            per_key("f:lazy", ValueType::DuckDB),
            per_key("f:eager", ValueType::Pandas),
        ]);
        assert!(lazily_read_inputs(&mixed).is_empty());
        let eager_fan_in = phase(vec![
            per_key("f:lazy", ValueType::PolarsLazy),
            reader_step("f:all", "xs", "f:up", Some(ValueType::Pandas)),
        ]);
        assert!(lazily_read_inputs(&eager_fan_in).is_empty());
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
                }],
            }],
        };
        assert!(
            expand_pending_partitions(&no_dag(), &phase, &HashMap::<String, OutputRef>::new(), 4)
                .phase
                .is_none()
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                content_hash: None,
            },
        );

        let expanded = expand_pending_partitions(&no_dag(), &phase, &outputs, 4)
            .phase
            .unwrap();
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
            ..Default::default()
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
                }],
            }],
        };
        let mut outputs = HashMap::new();
        outputs.insert(
            "f:get_regions".to_string(),
            test_output_ref(&artifact_path.to_string_lossy(), "json"),
        );

        let expanded = expand_pending_partitions(&no_dag(), &phase, &outputs, 4)
            .phase
            .unwrap();
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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

        let expanded = expand_pending_partitions(&no_dag(), &phase, &outputs, 4)
            .phase
            .unwrap();
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                content_hash: None,
            },
        );

        let expanded = expand_pending_partitions(&no_dag(), &phase, &outputs, 4)
            .phase
            .unwrap();
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                content_hash: None,
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
                    weight: 1,
                    retry_backoff_seconds: 0.0,
                    partition_keys: vec![],
                    param_types: HashMap::new(),
                    return_type: None,
                    keyed: Default::default(),
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
                content_hash: None,
            },
        );

        let provided = build_provided_inputs(&phase, &all_outputs);
        match &provided["f:a[t=X]"] {
            ProvidedInput::Single(oref) => assert_eq!(oref.format, "parquet"),
            _ => panic!("expected Single"),
        }
    }
}
