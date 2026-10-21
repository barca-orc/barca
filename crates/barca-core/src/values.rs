//! `barca get -o value`: read returned artifacts as JSON values.
//!
//! A json artifact is read here. Parquet, CSV and pickle artifacts are read by
//! `python -m barca._value` with the reader `barca.get()` uses (`barca._artifacts.deserialize`),
//! all in one process, and converted to JSON there: a table becomes a list of row objects.
//!
//! `barca get -o url` signs each returned artifact's store object through the same helper
//! ([`sign_urls`]).

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
        let answers =
            match crate::status::run_json_helper(helper(python, cfg), python, &request).await {
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

/// What `barca get -o value --json` returns as `final_output`: the value of the returned
/// artifact, or for a partitioned target an object of values keyed by partition key. An
/// artifact that cannot be read is returned as its pointer, with a warning, as `get` does.
/// `None` when the run returned nothing.
pub async fn returned_values(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
    single: Option<&OutputRef>,
    keys: Option<&[(String, OutputRef)]>,
) -> Option<Value> {
    let items: Vec<(Option<&str>, &OutputRef)> = match keys {
        Some(keys) => keys.iter().map(|(k, o)| (Some(k.as_str()), o)).collect(),
        None => single.into_iter().map(|o| (None, o)).collect(),
    };
    let refs: Vec<&OutputRef> = items.iter().map(|(_, o)| *o).collect();
    let values: Vec<Value> = read_values(python, cfg, &refs)
        .await
        .into_iter()
        .zip(&refs)
        .map(|(value, oref)| {
            value.unwrap_or_else(|e| {
                crate::errln!(
                    "[barca] warning: could not read the value of {}: {e}; its path is \
                     returned instead",
                    oref.path
                );
                crate::report::artifact_metadata(oref)
            })
        })
        .collect();
    match keys {
        Some(_) => Some(Value::Object(
            items
                .iter()
                .zip(values)
                .map(|((key, _), v)| (key.unwrap_or_default().to_string(), v))
                .collect(),
        )),
        None => values.into_iter().next(),
    }
}

/// `python -m barca._value`, with the resolved storage options (reads and signing alike).
fn helper(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
) -> tokio::process::Command {
    crate::helper_proc::python_module(python, "barca._value", cfg.storage_options_json.as_deref())
}

/// Version of the `sign` request to `barca._value` (`specs/worker-protocol.md`).
pub const SIGN_VERSION: u32 = 1;

/// One object to sign: its store URI and the name a browser downloads it as.
#[derive(Debug, Clone)]
pub struct SignRequest {
    pub uri: String,
    pub filename: String,
}

/// A signed URL and when it stops working (RFC 3339, UTC). The URL is a bearer credential:
/// print it to stdout and nowhere else.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub url: String,
    pub expires_at: String,
    /// Why the link ends sooner than asked (temporary credentials); never contains the URL.
    pub note: Option<String>,
}

/// Why one object could not be signed: `kind` is `usage`, `missing`, `credentials` or `store`
/// (see `barca._storage.SignError`).
#[derive(Debug, Clone, PartialEq)]
pub struct SignFailure {
    pub kind: String,
    pub message: String,
}

/// Sign each object for `expires_secs` seconds, in order, with one `barca._value` process.
pub async fn sign_urls(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
    objects: &[SignRequest],
    expires_secs: u64,
) -> Vec<Result<Signed, SignFailure>> {
    if objects.is_empty() {
        return Vec::new();
    }
    let request = serde_json::json!({
        "op": "sign",
        "version": SIGN_VERSION,
        "objects": objects
            .iter()
            .map(|o| serde_json::json!({
                "uri": o.uri,
                "expires": expires_secs,
                "filename": o.filename,
            }))
            .collect::<Vec<_>>(),
    });
    let failure = |message: String| SignFailure {
        kind: "store".to_string(),
        message,
    };
    match crate::status::run_json_helper(helper(python, cfg), python, &request).await {
        Ok(a) if a.len() == objects.len() => a.into_iter().map(signed).collect(),
        Ok(_) => vec![
            Err(failure(
                "the URL signer returned an unexpected result".into()
            ));
            objects.len()
        ],
        Err(e) => vec![Err(failure(format!("could not run the URL signer: {e}"))); objects.len()],
    }
}

/// `{"url", "expires_at"}` or `{"error", "kind"}` from `barca._value`.
fn signed(item: Value) -> Result<Signed, SignFailure> {
    let get = |k: &str| item.get(k).and_then(Value::as_str).map(str::to_string);
    match (get("url"), get("expires_at"), get("error")) {
        (_, _, Some(message)) => Err(SignFailure {
            kind: get("kind").unwrap_or_else(|| "store".to_string()),
            message,
        }),
        (Some(url), Some(expires_at), None) => Ok(Signed {
            url,
            expires_at,
            note: get("note"),
        }),
        _ => Err(SignFailure {
            kind: "store".to_string(),
            message: "the URL signer returned an unexpected result".to_string(),
        }),
    }
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

    #[test]
    fn signer_answers_are_urls_or_failures() {
        assert_eq!(
            signed(
                serde_json::json!({"url": "https://x/y?sig", "expires_at": "2026-10-20T10:00:00Z"})
            ),
            Ok(Signed {
                url: "https://x/y?sig".into(),
                expires_at: "2026-10-20T10:00:00Z".into(),
                note: None,
            })
        );
        assert_eq!(
            signed(serde_json::json!({"error": "s3://b/k is not in the store", "kind": "missing"})),
            Err(SignFailure {
                kind: "missing".into(),
                message: "s3://b/k is not in the store".into()
            })
        );
        assert_eq!(
            signed(serde_json::json!({"url": "u"})).unwrap_err().kind,
            "store"
        );
    }
}
