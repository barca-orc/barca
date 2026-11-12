//! The `step` frame a worker is handed for one task: its inputs, partition keys, batch and
//! parallel-group context, built from the coordinator's item.

use crate::coordinator::{Coordinator, Item};
use std::collections::HashMap;

pub(crate) fn build_step_json(item: &Item, coord: &Coordinator) -> serde_json::Value {
    // Start with dag_inputs from spec (cross-phase provided inputs), as plain
    // artifact-path strings.
    let mut inputs: serde_json::Map<String, serde_json::Value> = item
        .spec
        .dag_inputs
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    // Fill in-phase upstream artifacts: for params that don't have a value yet
    // (and aren't a fan-in collected param, filled below), look up the
    // coordinator's outputs using the upstream_inputs mapping.
    for (param, upstream_node_id) in &item.spec.upstream_inputs {
        if item.spec.collected_inputs.contains_key(param) {
            continue;
        }
        if inputs.get(param).is_some_and(|v| v.as_str() != Some("")) {
            continue;
        }
        if let Some(artifact) = coord.output_for_display(upstream_node_id) {
            let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() {
                inputs.insert(param.clone(), serde_json::Value::String(path.to_string()));
            }
        }
    }

    // Fan-in (`collect()`) params: every partition artifact of the upstream,
    // sent as a `{"_collected": true, "artifacts": [...]}` marker — the same
    // shape batch mode's `_resolve_input` already understands — so the worker
    // deserializes the full list instead of a single artifact.
    for (param, orefs) in &item.spec.collected_inputs {
        let artifacts: Vec<serde_json::Value> = orefs
            .iter()
            .map(|oref| serde_json::json!({"path": oref.path, "format": oref.format}))
            .collect();
        inputs.insert(
            param.clone(),
            serde_json::json!({"_collected": true, "artifacts": artifacts}),
        );
    }

    // A dict `partitions_from` source's value for this key: the worker reads `key` of the
    // source's artifact, and passes it when the function takes the parameter.
    for (param, (path, key)) in &item.spec.keyed_inputs {
        inputs.insert(
            param.clone(),
            serde_json::json!({"_keyed": true, "path": path, "key": key}),
        );
    }

    // A key of a batched call (RFC-0013): which inputs come from the same key of a partitioned
    // upstream, so the call passes them as a dict of key to value (or a lazy table).
    let aligned: Vec<&String> = if coord.call_id(item.id).is_some() {
        let mut a: Vec<&String> = item
            .spec
            .upstream_inputs
            .iter()
            .filter(|(p, id)| !p.starts_with('_') && id.contains('['))
            .map(|(p, _)| p)
            .collect();
        a.sort();
        a
    } else {
        Vec::new()
    };
    let mut step = serde_json::json!({
        "node_id": item.step_id.display(),
        "function_name": item.spec.function_name,
        "source_file": item.spec.source_file,
        "kind": &item.spec.kind,
        "inputs": inputs,
        "timeout_seconds": item.spec.timeout_seconds,
        // `weight=`, so `barca.on_step_start` callbacks can size the step (memory, threads).
        "weight": item.spec.weight,
        "direct_args": item.spec.direct_args,
        "direct_kwargs": item.spec.direct_kwargs,
        "serializer": item.spec.serializer.as_deref(),
        "sinks": item.spec.sinks,
        "run_hash": item.spec.run_hash,
        "param_types": item
            .spec
            .param_types
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str()))
            .collect::<HashMap<String, &str>>(),
        "return_type": item.spec.return_type.map(|t| t.as_str()),
        // A `parallel()` branch: its result goes back to the step that called it.
        "branch": item.group.is_some(),
        // Read by a `partitions_from(...)` for keys: the worker checks what it returns.
        "partition_source": item.spec.partition_source,
        "previous_sensor": item.spec.previous_sensor.as_ref().map(|output| serde_json::json!({
            "path": output.path, "format": output.format, "size_bytes": output.size_bytes,
            "content_hash": output.content_hash,
        })),
        // Where the branch writes that result, without the extension: the worker adds the one
        // for the format it picks.
        "branch_path": item
            .group
            .and_then(|g| coord.group(g).result_dir.as_ref())
            .map(|dir| dir.join(item.id.0.to_string())),
    });
    if coord.call_id(item.id).is_some() {
        step["aligned_inputs"] = serde_json::json!(aligned);
    }
    step
}
