//! Protocol v3 metadata tables; missing cells are distinct from explicit nulls.
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub(crate) fn encode_table(items: &[Value], hoist: bool) -> Value {
    let keys: BTreeSet<String> = items
        .iter()
        .flat_map(|item| item.as_object().expect("metadata object").keys().cloned())
        .collect();
    let header: Map<String, Value> = keys
        .iter()
        .filter(|key| {
            hoist
                && !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.get(*key).is_some() && item.get(*key) == items[0].get(*key))
        })
        .map(|key| (key.clone(), items[0][key].clone()))
        .collect();
    let dims: Vec<_> = keys
        .into_iter()
        .filter(|key| !header.contains_key(key))
        .collect();
    let rows: Vec<Vec<Value>> = items
        .iter()
        .map(|item| {
            dims.iter()
                .map(|key| item.get(key).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    let missing: Vec<Vec<usize>> = items
        .iter()
        .map(|item| {
            dims.iter()
                .enumerate()
                .filter_map(|(i, key)| item.get(key).is_none().then_some(i))
                .collect()
        })
        .collect();
    let mut table = json!({"header":header,"dims":dims,"rows":rows});
    if missing.iter().any(|row| !row.is_empty()) {
        table["missing"] = json!(missing);
    }
    table
}

pub(crate) fn decode_table(table: &Value) -> Result<Vec<Value>, String> {
    let header = table
        .get("header")
        .and_then(Value::as_object)
        .ok_or("table needs header")?;
    let dims = table
        .get("dims")
        .and_then(Value::as_array)
        .ok_or("table needs dims")?;
    let rows = table
        .get("rows")
        .and_then(Value::as_array)
        .ok_or("table needs rows")?;
    let keys: Vec<&str> = dims
        .iter()
        .map(|dim| dim.as_str().ok_or("dimensions must be strings"))
        .collect::<Result<_, _>>()?;
    if keys.iter().copied().collect::<BTreeSet<_>>().len() != keys.len()
        || keys.iter().any(|key| header.contains_key(*key))
    {
        return Err("duplicate or overlapping dimensions".into());
    }
    let missing = table
        .get("missing")
        .map(|v| v.as_array().ok_or("missing masks must be arrays"))
        .transpose()?;
    if missing.is_some_and(|m| m.len() != rows.len()) {
        return Err("missing masks must match rows".into());
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let row = row.as_array().ok_or("row must be an array")?;
            if row.len() != keys.len() {
                return Err("row width must match dimensions".into());
            }
            let absent: Vec<usize> = match missing {
                Some(masks) => masks[index]
                    .as_array()
                    .ok_or("missing mask must be an array")?
                    .iter()
                    .map(|v| {
                        v.as_u64()
                            .and_then(|n| usize::try_from(n).ok())
                            .filter(|n| *n < keys.len())
                            .ok_or("invalid missing column")
                    })
                    .collect::<Result<_, _>>()?,
                None => Vec::new(),
            };
            if absent.iter().collect::<BTreeSet<_>>().len() != absent.len() {
                return Err("duplicate missing column".into());
            }
            let mut item = header.clone();
            for (column, key) in keys.iter().enumerate() {
                if !absent.contains(&column) {
                    item.insert((*key).to_string(), row[column].clone());
                }
            }
            Ok(Value::Object(item))
        })
        .collect()
}

pub(crate) fn normalize_step(step: &Value) -> Value {
    let mut item = step.as_object().expect("step object").clone();
    for key in [
        "inputs",
        "direct_args",
        "direct_kwargs",
        "param_types",
        "sinks",
    ] {
        if item.get(key).is_some_and(|v| {
            v.as_object().is_some_and(Map::is_empty) || v.as_array().is_some_and(Vec::is_empty)
        }) {
            item.remove(key);
        }
    }
    for key in ["serializer", "return_type", "branch_path"] {
        if item.get(key) == Some(&Value::Null) {
            item.remove(key);
        }
    }
    if item.get("branch") == Some(&json!(false)) {
        item.remove("branch");
    }
    if item.get("timeout_seconds") == Some(&json!(0)) {
        item.remove("timeout_seconds");
    }
    Value::Object(item)
}

pub(crate) fn encode_steps(steps: &[Value]) -> Value {
    let mut items: Vec<Value> = steps.iter().map(normalize_step).collect();
    let kwargs: Vec<Value> = items
        .iter_mut()
        .map(|item| {
            item.as_object_mut()
                .unwrap()
                .remove("direct_kwargs")
                .unwrap_or(json!({}))
        })
        .collect();
    let mut frame = json!({"type":"execute_batch", "items":encode_table(&items,true)});
    if kwargs
        .iter()
        .any(|v| v.as_object().is_some_and(|v| !v.is_empty()))
    {
        frame["kwargs"] = encode_table(&kwargs, true);
    }
    frame
}

pub(crate) fn decode_results(frame: &Value) -> Result<Vec<crate::protocol::WorkerMessage>, String> {
    let details = decode_table(frame.get("details").ok_or("results need details")?)?;
    let array = |name| {
        frame
            .get(name)
            .and_then(Value::as_array)
            .filter(|v| v.len() == details.len())
            .ok_or_else(|| format!("result {name} must match rows"))
    };
    let ids = array("node_ids")?;
    let statuses = array("statuses")?;
    let elapsed = array("elapsed_seconds")?;
    details
        .into_iter()
        .enumerate()
        .map(|(index, mut detail)| {
            let status = statuses[index]
                .as_str()
                .ok_or("result status must be a string")?;
            if !ids[index].is_string() || !(elapsed[index].is_null() || elapsed[index].is_number())
            {
                return Err("invalid result identity/elapsed".into());
            }
            let item = match status {
                "step_completed" => {
                    if detail.get("json").is_some()
                        || detail
                            .get("path")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                    {
                        return Err("artifact data must travel by reference".into());
                    }
                    if !elapsed[index].is_null() {
                        detail["elapsed_seconds"] = elapsed[index].clone();
                    }
                    json!({"type":status,"node_id":ids[index],"artifact":detail})
                }
                "step_error" | "blocked" => {
                    detail["type"] = json!(status);
                    detail["node_id"] = ids[index].clone();
                    if status == "step_error" {
                        detail["elapsed"] = elapsed[index].clone();
                    }
                    detail
                }
                _ => return Err("unsupported result status".into()),
            };
            serde_json::from_value(item).map_err(|e| format!("invalid result: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn tables_distinguish_missing_null_and_reject_malformed_shapes() {
        let items = vec![json!({"a":null,"b":true}), json!({"b":1})];
        assert_eq!(decode_table(&encode_table(&items, true)).unwrap(), items);
        for malformed in [
            json!({"dims":[],"rows":[]}),
            json!({"header":{},"dims":["a","a"],"rows":[[1,2]]}),
            json!({"header":{"a":1},"dims":["a"],"rows":[[2]]}),
            json!({"header":{},"dims":["a"],"rows":[[]]}),
            json!({"header":{},"dims":["a"],"rows":[[1]],"missing":[[0,0]]}),
            json!({"header":{},"dims":["a"],"rows":[[1]],"missing":[[2]]}),
        ] {
            assert!(decode_table(&malformed).is_err(), "{malformed}");
        }
        let result = json!({"type":"results","node_ids":["n"],"statuses":["step_completed"],"elapsed_seconds":[0.1],"details":encode_table(&[json!({"path":"/a.json","format":"json","size_bytes":1})],true)});
        assert!(decode_results(&result).is_ok());
        for (key, value) in [
            ("node_ids", json!([])),
            ("statuses", json!(["invented"])),
            ("elapsed_seconds", json!([true])),
            (
                "details",
                encode_table(
                    &[json!({"path":"","format":"json","size_bytes":1,"json":"1"})],
                    true,
                ),
            ),
            ("details", encode_table(&[json!({"format":"json"})], true)),
        ] {
            let mut invalid = result.clone();
            invalid[key] = value;
            assert!(decode_results(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn ten_thousand_steps_and_results_round_trip_through_real_python_stdlib() {
        let steps: Vec<_> = (0..10_000).map(|i| json!({
            "node_id":format!("pipeline.py:orders[region=r{i},day=2026-10-14]"),
            "function_name":"orders","source_file":"/project/pipeline.py","kind":"asset",
            "serializer":"parquet","timeout_seconds":0,"branch":false,"inputs":{},
            "direct_args":[],"direct_kwargs":{"region":format!("r{i}"),"day":"2026-10-14", "nullable":null},
            "param_types":{},"sinks":[],"return_type":null,"branch_path":null
        })).collect();
        let command = encode_steps(&steps);
        let plain_bytes = serde_json::to_vec(&json!({"type":"execute_batch","steps":steps}))
            .unwrap()
            .len();
        let compact_bytes = serde_json::to_vec(&command).unwrap().len();
        assert!(
            compact_bytes * 5 < plain_bytes,
            "compact={compact_bytes}, plain={plain_bytes}"
        );
        let script = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location('wire', sys.argv[1])
wire = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wire)
command = json.load(sys.stdin)
steps = wire.decode_steps(command)
assert len(steps) == 10000
assert steps[0]['direct_kwargs']['nullable'] is None
assert 'timeout_seconds' not in steps[0] and 'sinks' not in steps[0]
# JSON booleans and numbers must never be hoisted as the same value.
assert wire.decode_table(wire.encode_table([{'v': True}, {'v': 1}])) == [{'v': True}, {'v': 1}]
assert wire.decode_table(wire.encode_table([{'v': None}, {}])) == [{'v': None}, {}]
messages = []
for i, step in enumerate(steps):
    if i % 997 == 0:
        message = {'type':'step_error','node_id':step['node_id'],'error_type':'ValueError','message':'broken','traceback':'line\nnext','elapsed':0.2}
    elif i % 991 == 0:
        message = {'type':'blocked','node_id':step['node_id'],'reason':'sensor'}
    else:
        artifact = {'path':f'/artifacts/{i}.parquet','format':'parquet','size_bytes':i+12,'elapsed_seconds':i/10000}
        if i % 17 == 0: artifact['upload_snapshot'] = f'/staging/upload-{i}'
        if i % 29 == 0: artifact['content_hash'] = 'a'*64
        message = {'type':'step_completed','node_id':step['node_id'],'artifact':artifact}
    messages.append(message)
encoded = wire.encode_results(messages)
assert wire.decode_results(encoded) == messages
for mutate in [lambda x: x.update(node_ids=[]), lambda x: x.update(statuses=['unknown']*len(messages))]:
    bad = dict(encoded)
    mutate(bad)
    try: wire.decode_results(bad)
    except ValueError: pass
    else: raise AssertionError('malformed results accepted')
json.dump({'steps':steps,'results':encoded}, sys.stdout)
"#;
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/barca/_wire.py");
        let mut python_command = Command::new("python3");
        python_command
            .args(["-S", "-c", script])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::helper_proc::spawn_std(&mut python_command).unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&command).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
        let expected: Vec<_> = steps.iter().map(normalize_step).collect();
        assert_eq!(answer["steps"], json!(expected));
        let results = decode_results(&answer["results"]).unwrap();
        assert_eq!(results.len(), 10_000);
        assert!(
            matches!(&results[0], crate::protocol::WorkerMessage::StepError { error_type, .. } if error_type=="ValueError")
        );
        let crate::protocol::WorkerMessage::StepCompleted { artifact, .. } = &results[17] else {
            panic!("completion")
        };
        assert_eq!(
            artifact.upload_snapshot.as_deref(),
            Some("/staging/upload-17")
        );
        crate::errln!("10k execution: {plain_bytes} bytes plain, {compact_bytes} compact");
    }
}
