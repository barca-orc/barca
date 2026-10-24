//! Keyed partition sources: `partitions_from(source)` where `source` is unpartitioned and
//! returns a dict.
//!
//! The dict's keys are the partition keys. Each key of a consumer that names the source in its
//! own `partitions_from(...)` receives that key's value, under the parameter named after the
//! source (or the `inputs=` key that names it), and its run hash folds in that value's hash
//! and the source's run hash instead of the source's whole output. So one sensor call that
//! returns `{retailer: version}` for every retailer decides both the key set and which keys
//! changed: a key whose value is unchanged is served from cache, and so is everything
//! `partitions_from` chains below it.
//!
//! A consumer that takes the source's keys from a partitioned upstream
//! (`partitions_from(cleaned)` where `cleaned` is `partitions_from(source)`) gets the keys
//! only: what it reads, `cleaned`'s key, already carries the value in its run hash.
//!
//! **Value hash.** A value is hashed as canonical JSON: the value as barca reads it from the
//! source's JSON artifact, written back with object keys sorted at every level and no
//! whitespace ([`canonical_json`]). So reordering the keys of a nested dict, or of the source
//! dict itself, changes nothing. Integers outside 64 bits are read as floats.
//!
//! The source must be written as JSON. The worker checks a source's return value before
//! writing it (`python/barca/_worker.py`, `_check_partition_source`): a list or a dict, string
//! or integer keys (an integer is stringified, as JSON does), and JSON-serializable values; any
//! other value is a step error of the source naming the key or the type.

use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

/// One dict source a partitioned step expanded from.
#[derive(Debug)]
pub struct KeyedSource {
    /// The partition dimension its keys fill.
    pub dim: String,
    /// The source's node id.
    pub source_id: String,
    /// The source's JSON artifact, from which the worker reads each key's value.
    pub path: String,
    /// The parameter a key's value is passed as: the `inputs=` key that names the source, else
    /// the source's name. `None` for a consumer that takes the keys from a partitioned upstream
    /// (it gets no value, and the value is not folded into its run hash).
    pub param: Option<String>,
    /// Key -> hash of its value ([`value_hash`]).
    pub hashes: Arc<HashMap<String, String>>,
}

/// What a planned step has to do with keyed sources.
#[derive(Debug, Clone, Default)]
pub struct StepKeyed {
    /// Original key positions for a batched node, before worker bin-packing and cache
    /// filtering. Shared by its chunks so batch boundaries do not depend on pool size.
    pub order: Option<Arc<HashMap<String, usize>>>,
    /// The dict sources this step's keys were expanded from, once expanded.
    pub sources: Option<Arc<Vec<KeyedSource>>>,
    /// The step is an unpartitioned node that a `partitions_from(...)` reads: the worker checks
    /// what it returns is a list or a dict it can write as JSON.
    pub is_source: bool,
    /// Expanded from a source that returned no key (an empty list or dict): the step has
    /// nothing to run, and is kept only so what reads it finds it decided.
    pub no_keys: bool,
}

impl StepKeyed {
    pub(crate) fn remember_order(&mut self, keys: &[crate::PartitionKey]) {
        self.order = Some(Arc::new(
            keys.iter()
                .enumerate()
                .map(|(i, key)| (key.suffix(), i))
                .collect(),
        ));
    }

    /// The sources whose value this step receives (and whose value its run hash folds in).
    pub fn direct(&self) -> impl Iterator<Item = &KeyedSource> {
        self.sources
            .iter()
            .flat_map(|s| s.iter())
            .filter(|s| s.param.is_some())
    }
}

/// What a `partitions_from` source returned.
#[derive(Debug, PartialEq)]
pub enum SourceKeys {
    /// A list: its values are the keys (strings, and numbers in their JSON form).
    List(Vec<String>),
    /// A dict: its keys in artifact order, and each key's value hash.
    Keyed(Vec<String>, HashMap<String, String>),
}

/// Read a dict without sorting its keys. Values still use the same canonical hashes;
/// only scheduling order follows the producer's declaration (RFC-0013).
struct OrderedSource;
impl<'de> serde::de::Visitor<'de> for OrderedSource {
    type Value = SourceKeys;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a dict of partition key to value")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<SourceKeys, M::Error> {
        let mut keys = Vec::new();
        let mut hashes = HashMap::new();
        while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
            let hash = raw_value_hash(&raw).map_err(serde::de::Error::custom)?;
            if !hashes.contains_key(&key) {
                keys.push(key.clone());
            }
            hashes.insert(key, hash);
        }
        Ok(SourceKeys::Keyed(keys, hashes))
    }
}

/// Read a source's JSON artifact text: a list of keys, or a dict of key -> value. Anything else
/// is an error in words naming the types a source may return.
pub fn read_source(text: &str) -> Result<SourceKeys, String> {
    match text.trim_start().as_bytes().first() {
        Some(b'[') => {
            let values: Vec<Value> =
                serde_json::from_str(text).map_err(|e| format!("its JSON cannot be read: {e}"))?;
            Ok(SourceKeys::List(
                values
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect(),
            ))
        }
        Some(b'{') => {
            let mut parser = serde_json::Deserializer::from_str(text);
            let source = serde::Deserializer::deserialize_map(&mut parser, OrderedSource)
                .map_err(|e| format!("its JSON cannot be read: {e}"))?;
            parser
                .end()
                .map_err(|e| format!("its JSON cannot be read: {e}"))?;
            Ok(source)
        }
        _ => {
            let what = match serde_json::from_str::<Value>(text) {
                Ok(Value::Null) => "None",
                Ok(Value::Bool(_)) => "a bool",
                Ok(Value::Number(_)) => "a number",
                Ok(Value::String(_)) => "a string",
                Ok(_) | Err(_) => "a value barca cannot read",
            };
            Err(format!(
                "returned {what}; a partitions_from source must return a list of keys or a \
                 dict of key -> value"
            ))
        }
    }
}

fn raw_value_hash(raw: &RawValue) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(raw.get()).map_err(|e| format!("its JSON cannot be read: {e}"))?;
    Ok(value_hash(&value))
}

/// The hash of one key's value: SHA-256 of its canonical JSON ([`canonical_json`]), hex.
pub fn value_hash(value: &Value) -> String {
    let mut text = String::new();
    canonical_json(value, &mut text);
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// `value` as JSON with object keys sorted at every level and no whitespace.
pub fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                canonical_json(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(v, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap_or_default()),
    }
}

/// What a key's value contributes to its run hash: the source's run hash and the value's hash.
/// The source's run hash covers its code, as for any input: editing the sensor re-runs every
/// key, as it re-runs everything that reads a sensor (`barca docs cache`).
pub fn token(source_run_hash: &str, value_hash: &str) -> String {
    format!("{source_run_hash}+value:{value_hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(text: &str) -> (Vec<String>, HashMap<String, String>) {
        match read_source(text).unwrap() {
            SourceKeys::Keyed(keys, hashes) => (keys, hashes),
            other => panic!("not keyed: {other:?}"),
        }
    }

    #[test]
    fn a_list_gives_its_values_as_keys() {
        assert_eq!(
            read_source(r#"["a", 2, 1.5, null, {"x": 1}]"#).unwrap(),
            SourceKeys::List(vec!["a".into(), "2".into(), "1.5".into()])
        );
    }

    #[test]
    fn a_dict_preserves_its_key_order_and_one_hash_per_value() {
        let (keys, hashes) = keyed(r#"{"r2": "b", "r1": "a", "r3": "a"}"#);
        assert_eq!(keys, ["r2", "r1", "r3"]);
        assert_eq!(hashes["r1"], hashes["r3"], "equal values hash alike");
        assert_ne!(hashes["r1"], hashes["r2"]);
    }

    #[test]
    fn a_keys_hash_depends_on_its_own_value_only() {
        let (_, before) = keyed(r#"{"r1": "a", "r2": "b"}"#);
        let (_, after) = keyed(r#"{"r1": "a", "r2": "c", "r9": "new"}"#);
        assert_eq!(before["r1"], after["r1"]);
        assert_ne!(before["r2"], after["r2"]);
    }

    #[test]
    fn the_hash_is_canonical_json() {
        // Key order and whitespace do not count, at any level.
        let (_, a) = keyed(r#"{"r1": {"b": 1, "a": [1, {"y": 2, "x": 3}]}}"#);
        let (_, b) = keyed(r#"{"r1":{"a":[1,{"x":3,"y":2}],"b":1}}"#);
        assert_eq!(a["r1"], b["r1"]);
        let mut text = String::new();
        canonical_json(
            &serde_json::json!({"b": 1, "a": [true, null, "é"]}),
            &mut text,
        );
        assert_eq!(text, r#"{"a":[true,null,"é"],"b":1}"#);
        // The hash is SHA-256 of that text.
        assert_eq!(
            value_hash(&serde_json::json!("a")),
            format!("{:x}", Sha256::digest(br#""a""#))
        );
    }

    #[test]
    fn types_and_values_are_told_apart() {
        let (_, h) = keyed(r#"{"int": 1, "str": "1", "float": 1.5, "list": [1], "null": null}"#);
        let mut all: Vec<&String> = h.values().collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn anything_else_names_the_types_a_source_may_return() {
        for (text, what) in [
            ("null", "None"),
            ("3", "a number"),
            ("\"k\"", "a string"),
            ("true", "a bool"),
        ] {
            let err = read_source(text).unwrap_err();
            assert!(err.contains(what), "{err}");
            assert!(
                err.contains("a list of keys or a dict of key -> value"),
                "{err}"
            );
        }
    }

    #[test]
    fn the_token_carries_the_source_run_hash_and_the_value() {
        assert_eq!(token("abc", "def"), "abc+value:def");
        assert_ne!(token("abc", "def"), token("abd", "def"));
    }
}
