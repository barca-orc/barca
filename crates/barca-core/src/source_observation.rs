//! Compact producer observations. Full snapshots stay in artifacts and their workers;
//! freshness proofs retain counts and digests, and execution receives affected keys only.
use serde::{
    Deserialize, Serialize,
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Source {
    path: String,
    pub count: usize,
    content_hash: String,
    keyed: bool,
    pub membership_hash: String,
    #[serde(skip)]
    delta: Option<Delta>,
}
impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.count == other.count
            && self.content_hash == other.content_hash
            && self.keyed == other.keyed
            && self.membership_hash == other.membership_hash
    }
}
#[derive(Clone)]
struct Delta {
    previous_hash: String,
    order: Vec<String>,
    hashes: HashMap<String, String>,
    removed: HashSet<String>,
}
#[derive(Deserialize)]
struct Observation {
    count: usize,
    keyed: bool,
    membership_hash: String,
    previous_hash: Option<String>,
    #[serde(default)]
    changed: HashMap<String, Value>,
    #[serde(default)]
    changed_order: Vec<String>,
    #[serde(default)]
    removed: HashSet<String>,
}
impl Source {
    pub(crate) fn matches(&self, hash: Option<&str>) -> bool {
        hash == Some(self.content_hash.as_str())
    }
    pub(crate) fn valid(&self) -> bool {
        self.content_hash.len() == 64 && self.membership_hash.len() == 64
    }
    pub(crate) fn observed(path: &str, content_hash: &str, wire: &Value) -> Result<Self, String> {
        let observation: Observation =
            serde_json::from_value(wire.clone()).map_err(|e| e.to_string())?;
        if observation.changed.len() > observation.count
            || observation
                .changed_order
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != observation.changed.len()
            || observation
                .removed
                .iter()
                .any(|k| observation.changed.contains_key(k))
            || observation.changed_order.len() != observation.changed.len()
            || observation
                .changed_order
                .iter()
                .any(|k| !observation.changed.contains_key(k))
        {
            return Err("source observation has inconsistent changed keys".into());
        }
        let delta = observation.previous_hash.map(|previous_hash| Delta {
            previous_hash,
            order: observation.changed_order,
            hashes: observation
                .changed
                .into_iter()
                .map(|(k, v)| (k, crate::keyed::value_hash(&v)))
                .collect(),
            removed: observation.removed,
        });
        let source = Self {
            path: path.into(),
            count: observation.count,
            content_hash: content_hash.into(),
            keyed: observation.keyed,
            membership_hash: observation.membership_hash,
            delta,
        };
        if !source.valid() {
            return Err("source observation has invalid digests".into());
        }
        Ok(source)
    }
    pub(crate) fn read(path: &str, content_hash: &str) -> Result<Self, String> {
        let scan = scan(path, None, false, true)?;
        Ok(Self {
            path: path.into(),
            count: scan.count,
            content_hash: content_hash.into(),
            keyed: scan.keyed,
            membership_hash: scan.membership_hash,
            delta: None,
        })
    }
    pub(crate) fn changes(&self, old: &Self) -> Option<HashSet<String>> {
        if !self.keyed || !old.keyed {
            return None;
        }
        if self.content_hash == old.content_hash {
            return Some(HashSet::new());
        }
        let delta = self
            .delta
            .as_ref()
            .filter(|d| d.previous_hash == old.content_hash)?;
        Some(delta.hashes.keys().chain(&delta.removed).cloned().collect())
    }
    pub(crate) fn keys(&self) -> Result<Vec<String>, String> {
        Ok(scan(&self.path, None, true, false)?.keys)
    }
    fn known(&self, selected: &HashSet<String>) -> Option<&Delta> {
        self.delta.as_ref().filter(|d| {
            selected
                .iter()
                .all(|k| d.hashes.contains_key(k) || d.removed.contains(k))
        })
    }
    pub(crate) fn active(&self, selected: &HashSet<String>) -> Result<HashSet<String>, String> {
        if let Some(delta) = self.known(selected) {
            return Ok(selected
                .iter()
                .filter(|k| delta.hashes.contains_key(*k))
                .cloned()
                .collect());
        }
        Ok(scan(&self.path, Some(selected), false, false)?
            .keys
            .into_iter()
            .collect())
    }
    pub(crate) fn project(
        &self,
        selected: &HashSet<String>,
    ) -> Result<crate::keyed::SourceKeys, String> {
        let (keys, hashes) = if let Some(delta) = self.known(selected) {
            let keys: Vec<_> = delta
                .order
                .iter()
                .filter(|k| selected.contains(*k))
                .cloned()
                .collect();
            let hashes = keys
                .iter()
                .filter_map(|k| Some((k.clone(), delta.hashes.get(k)?.clone())))
                .collect();
            (keys, hashes)
        } else {
            let read = scan(&self.path, Some(selected), false, false)?;
            (read.keys, read.hashes)
        };
        Ok(if self.keyed {
            crate::keyed::SourceKeys::Keyed(keys, hashes)
        } else {
            crate::keyed::SourceKeys::List(keys)
        })
    }
    pub(crate) fn hash(&self, key: &str) -> Option<String> {
        if let Some(hash) = self.delta.as_ref().and_then(|d| d.hashes.get(key)) {
            return Some(hash.clone());
        }
        scan(&self.path, Some(&HashSet::from([key.into()])), false, false)
            .ok()?
            .hashes
            .remove(key)
    }
}
#[derive(Default)]
struct Read {
    count: usize,
    keyed: bool,
    membership_hash: String,
    keys: Vec<String>,
    hashes: HashMap<String, String>,
}
struct Scan<'a> {
    selected: Option<&'a HashSet<String>>,
    all: bool,
    summary: bool,
}
impl Scan<'_> {
    fn key(&self, key: &str, result: &mut Read, digest: &mut Sha256) -> bool {
        result.count += 1;
        if self.summary {
            digest.update((key.len() as u64).to_be_bytes());
            digest.update(key.as_bytes());
        }
        let wanted = self.all || self.selected.is_some_and(|s| s.contains(key));
        if wanted {
            result.keys.push(key.to_owned());
        }
        wanted
    }
}
impl<'de> Visitor<'de> for Scan<'_> {
    type Value = Read;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a list of keys or a dict of key to value")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Read, M::Error> {
        let mut result = Read {
            keyed: true,
            ..Default::default()
        };
        let mut digest = Sha256::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.key(&key, &mut result, &mut digest) && self.selected.is_some() {
                let value = map.next_value::<Value>()?;
                result.hashes.insert(key, crate::keyed::value_hash(&value));
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        result.membership_hash = format!("{:x}", digest.finalize());
        Ok(result)
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Read, S::Error> {
        let mut result = Read::default();
        let mut digest = Sha256::new();
        while let Some(value) = seq.next_element::<Value>()? {
            match value {
                Value::String(key) => {
                    self.key(&key, &mut result, &mut digest);
                }
                Value::Number(key) => {
                    self.key(&key.to_string(), &mut result, &mut digest);
                }
                _ => {}
            }
        }
        result.membership_hash = format!("{:x}", digest.finalize());
        Ok(result)
    }
}
fn scan(
    path: &str,
    selected: Option<&HashSet<String>>,
    all: bool,
    summary: bool,
) -> Result<Read, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut parser =
        serde_json::Deserializer::from_reader(std::io::BufReader::with_capacity(64 * 1024, file));
    let result = serde::Deserializer::deserialize_any(
        &mut parser,
        Scan {
            selected,
            all,
            summary,
        },
    )
    .map_err(|e| e.to_string())?;
    parser.end().map_err(|e| e.to_string())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn artifact(text: &str) -> (tempfile::NamedTempFile, String) {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), text).unwrap();
        (file, format!("{:x}", Sha256::digest(text.as_bytes())))
    }
    #[test]
    fn compact_proofs_do_not_contain_members_or_value_fingerprints() {
        let text = serde_json::to_string(
            &(0..10000)
                .map(|i| (format!("k{i}"), 1))
                .collect::<HashMap<_, _>>(),
        )
        .unwrap();
        let (file, hash) = artifact(&text);
        let source = Source::read(file.path().to_str().unwrap(), &hash).unwrap();
        assert_eq!(source.count, 10000);
        assert!(serde_json::to_string(&source).unwrap().len() < 500);
        let selected = HashSet::from(["k9999".into(), "k5".into()]);
        let crate::keyed::SourceKeys::Keyed(keys, hashes) = source.project(&selected).unwrap()
        else {
            panic!()
        };
        assert_eq!(keys.len(), 2);
        assert_eq!(hashes.len(), 2);
        assert_eq!(
            hashes["k5"],
            crate::keyed::value_hash(&serde_json::json!(1))
        );
    }
    #[test]
    fn deltas_require_matching_previous_evidence_and_keep_producer_order() {
        let (file, hash) = artifact(r#"{"z":1,"a":2}"#);
        let old = Source::read(file.path().to_str().unwrap(), &hash).unwrap();
        let wire = serde_json::json!({"count":2,"keyed":true,"membership_hash":"a".repeat(64),
            "previous_hash":hash,"changed":{"z":2,"new":{"b":1,"a":2}},"changed_order":["z","new"],"removed":["a"]});
        let current = Source::observed("not-read", &"b".repeat(64), &wire).unwrap();
        assert_eq!(
            current.changes(&old).unwrap(),
            HashSet::from(["z".into(), "new".into(), "a".into()])
        );
        let selected = HashSet::from(["z".into(), "new".into(), "a".into()]);
        assert_eq!(
            current.active(&selected).unwrap(),
            HashSet::from(["z".into(), "new".into()])
        );
        let crate::keyed::SourceKeys::Keyed(keys, hashes) = current.project(&selected).unwrap()
        else {
            panic!()
        };
        assert_eq!(keys, ["z", "new"]);
        assert_eq!(
            hashes["new"],
            crate::keyed::value_hash(&serde_json::json!({"a":2,"b":1}))
        );
        let restored: Source =
            serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
        assert!(restored.changes(&old).is_none());
        let mut mismatched = wire.clone();
        mismatched["previous_hash"] = Value::String("c".repeat(64));
        assert!(
            Source::observed("unused", &"b".repeat(64), &mismatched)
                .unwrap()
                .changes(&old)
                .is_none()
        );
        mismatched["changed_order"] = serde_json::json!(["z", "z"]);
        assert!(Source::observed("unused", &"b".repeat(64), &mismatched).is_err());
    }
    #[test]
    fn selected_reads_keep_canonical_json_fingerprints() {
        let text = r#"{"b":{"z":1,"a":2},"a":1.0,"zero":-0.0}"#;
        let (file, hash) = artifact(text);
        let source = Source::read(file.path().to_str().unwrap(), &hash).unwrap();
        let crate::keyed::SourceKeys::Keyed(keys, hashes) =
            crate::keyed::read_source(text).unwrap()
        else {
            panic!()
        };
        assert_eq!(source.keys().unwrap(), keys);
        for key in keys {
            assert_eq!(source.hash(&key).unwrap(), hashes[&key]);
        }
    }
}
