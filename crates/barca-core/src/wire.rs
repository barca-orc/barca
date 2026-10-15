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
        frame["kwargs"] = encode_table(&kwargs, false);
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
                    if detail.get("json").is_some() {
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
