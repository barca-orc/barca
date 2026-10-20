//! `barca get -o value`: read returned artifacts as JSON values.
//!
//! A json artifact is read here. Parquet, CSV and pickle artifacts are read by
//! `python -m barca._value` with the reader `barca.get()` uses (`barca._artifacts.deserialize`),
//! all in one process, and converted to JSON there: a table becomes a list of row objects.

use crate::dispatch::OutputRef;
use serde_json::Value;

/// Each artifact's value, in order, or why it could not be read.
pub async fn read_values(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
    refs: &[&OutputRef],
) -> Vec<Result<Value, String>> {
    let mut out: Vec<Option<Result<Value, String>>> = refs
        .iter()
        .map(|r| (r.format == "json").then(|| read_json(&r.path)))
        .collect();
    let others: Vec<usize> = (0..refs.len()).filter(|i| out[*i].is_none()).collect();
    if !others.is_empty() {
        let request = serde_json::json!({
            "artifacts": others
                .iter()
                .map(|i| serde_json::json!({"path": refs[*i].path, "format": refs[*i].format}))
                .collect::<Vec<_>>(),
        });
        let reader = crate::helper_proc::python_module(
            python,
            "barca._value",
            cfg.storage_options_json.as_deref(),
        );
        let answers = match crate::status::run_json_helper(reader, python, &request).await {
            Ok(a) if a.len() == others.len() => a.into_iter().map(answer).collect(),
            Ok(_) => vec![
                Err("the value reader returned an unexpected result".to_string());
                others.len()
            ],
            Err(e) => vec![Err(format!("could not run the value reader: {e}")); others.len()],
        };
        for (i, answer) in others.into_iter().zip(answers) {
            out[i] = Some(answer);
        }
    }
    out.into_iter()
        .map(|v| v.expect("every artifact read"))
        .collect()
}

fn read_json(path: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))
}

/// `{"value": ...}` or `{"error": "..."}` from `barca._value`.
fn answer(item: Value) -> Result<Value, String> {
    match item {
        Value::Object(mut obj) => match (obj.remove("value"), obj.remove("error")) {
            (_, Some(Value::String(e))) => Err(e),
            (Some(v), _) => Ok(v),
            _ => Err("the value reader returned an unexpected result".to_string()),
        },
        _ => Err("the value reader returned an unexpected result".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_answers_are_values_or_errors() {
        assert_eq!(
            answer(serde_json::json!({"value": [1, 2]})),
            Ok(serde_json::json!([1, 2]))
        );
        assert_eq!(answer(serde_json::json!({"value": null})), Ok(Value::Null));
        assert_eq!(
            answer(serde_json::json!({"error": "ImportError: pyarrow"})),
            Err("ImportError: pyarrow".to_string())
        );
        assert!(answer(serde_json::json!(3)).is_err());
    }
}
