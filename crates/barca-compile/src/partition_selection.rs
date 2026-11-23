//! The value of `--partition`: which keys it names and whether a key is among them. A leaf, so
//! the planner ([`crate::planner`]) can expand only the selected keys without depending on
//! [`crate::selection`], which narrows the plan it produces and re-exports this type.

use barca_model::model::PartitionKey;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The keys `--partition` selects: a rectangular dimension filter or exact combinations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PartitionSelection {
    pub dimensions: BTreeMap<String, BTreeSet<String>>,
    pub(crate) exact_keys: Option<Vec<BTreeMap<String, String>>>,
}

const USAGE: &str = "Use --partition region=emea,amer (one dimension, comma-separated values) or a \
     JSON object for compound keys: --partition '{\"region\": \"emea\", \"day\": \"2026-10-14\"}'";

impl PartitionSelection {
    /// Parse the `--partition` argument.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.starts_with('[') {
            return Self::parse_exact(raw).map_err(|error| format!("{error}\n{USAGE}"));
        }
        let mut dims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if raw.starts_with('{') {
            let parsed: serde_json::Value = serde_json::from_str(raw)
                .map_err(|e| format!("--partition: not a JSON object ({e}).\n{USAGE}"))?;
            let serde_json::Value::Object(obj) = parsed else {
                return Err(format!("--partition: not a JSON object.\n{USAGE}"));
            };
            for (dim, value) in obj {
                let values: Vec<serde_json::Value> = match value {
                    serde_json::Value::Array(items) => items,
                    other => vec![other],
                };
                let entry = dims.entry(dim.clone()).or_default();
                for v in values {
                    entry.insert(match v {
                        serde_json::Value::String(s) => s,
                        serde_json::Value::Number(n) => n.to_string(),
                        other => {
                            return Err(format!(
                                "--partition: '{dim}' must be a string, a number or a list of \
                                 them, not {other}.\n{USAGE}"
                            ));
                        }
                    });
                }
            }
        } else {
            let Some((dim, values)) = raw.split_once('=') else {
                return Err(format!(
                    "--partition: expected DIMENSION=VALUE[,VALUE...], got '{raw}'.\n{USAGE}"
                ));
            };
            let entry = dims.entry(dim.trim().to_string()).or_default();
            for v in values.split(',').map(str::trim).filter(|v| !v.is_empty()) {
                entry.insert(v.to_string());
            }
        }
        if let Some((dim, _)) = dims
            .iter()
            .find(|(dim, values)| dim.is_empty() || values.is_empty())
        {
            return Err(if dim.is_empty() {
                format!("--partition: a dimension name is missing.\n{USAGE}")
            } else {
                format!("--partition: no value given for '{dim}'.\n{USAGE}")
            });
        }
        if dims.is_empty() {
            return Err(format!("--partition: nothing selected.\n{USAGE}"));
        }
        Ok(Self {
            dimensions: dims,
            exact_keys: None,
        })
    }

    /// Exact coordinate tuples, when this selection is not a rectangular filter.
    /// Readers can enumerate a sparse selection without expanding its cross-product.
    pub fn exact_keys(&self) -> Option<&[BTreeMap<String, String>]> {
        self.exact_keys.as_deref()
    }

    /// Preserve tuples in queued requests; legacy rectangular selections remain objects.
    pub fn to_json(&self) -> serde_json::Value {
        match &self.exact_keys {
            Some(keys) => serde_json::json!(keys),
            None => serde_json::json!(self.dimensions),
        }
    }

    fn parse_exact(raw: &str) -> Result<Self, String> {
        let entries: Vec<serde_json::Value> = serde_json::from_str(raw)
            .map_err(|e| format!("--partition: not a JSON key list ({e})."))?;
        if entries.is_empty() {
            return Err("--partition: nothing selected.".into());
        }
        let mut dimensions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut keys = BTreeSet::new();
        let mut names = None;
        for entry in entries {
            let serde_json::Value::Object(obj) = entry else {
                return Err("--partition: each exact key must be a JSON object.".into());
            };
            if obj.is_empty() || obj.keys().any(|d| d.is_empty()) {
                return Err("--partition: each exact key needs named dimensions.".into());
            }
            let current: BTreeSet<String> = obj.keys().cloned().collect();
            if names.as_ref().is_some_and(|expected| expected != &current) {
                return Err("--partition: exact keys must name the same dimensions.".into());
            }
            names = Some(current);
            let mut key = BTreeMap::new();
            for (dim, value) in obj {
                let value = match value {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => {
                        return Err(format!(
                            "--partition: exact key '{dim}' must be a string or number."
                        ));
                    }
                };
                dimensions
                    .entry(dim.clone())
                    .or_default()
                    .insert(value.clone());
                key.insert(dim, value);
            }
            keys.insert(key);
        }
        Ok(Self {
            dimensions,
            exact_keys: Some(keys.into_iter().collect()),
        })
    }

    /// Whether `key` is selected: each named dimension that `key` has holds one of its listed
    /// values. Dimensions the key does not have are not checked, so a key with none of them
    /// matches; [`Self::apply`] narrows only nodes that have at least one.
    pub fn matches(&self, key: &PartitionKey) -> bool {
        match &self.exact_keys {
            // Parsed keys are sorted and have identical dimension names. Full keys use a
            // logarithmic lookup, even for thousands of scattered failures.
            Some(keys) if self.dimensions.keys().all(|d| key.0.contains_key(d)) => keys
                .binary_search_by(|selected| {
                    selected
                        .iter()
                        .map(|(d, v)| v.cmp(&key.0[d]))
                        .find(|order| !order.is_eq())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .is_ok(),
            Some(keys) => keys.iter().any(|selected| {
                selected
                    .iter()
                    .all(|(dim, value)| key.0.get(dim).is_none_or(|v| v == value))
            }),
            None => self
                .dimensions
                .iter()
                .all(|(dim, values)| key.0.get(dim).is_none_or(|v| values.contains(v))),
        }
    }

    /// The planner's pre-filter for one node's static dimensions (dimension → its values): when
    /// the node is narrowed, keep only the selected values of each named dimension, so that
    /// the keys left out are never built. A dimension that would be left with no value is kept
    /// whole; [`Self::apply`] then removes the node's keys.
    pub fn prefilter(&self, dims: &mut HashMap<String, Vec<String>>) {
        if !self.dimensions.keys().any(|d| dims.contains_key(d)) {
            return;
        }
        let filtered: Vec<(String, Vec<String>)> = self
            .dimensions
            .iter()
            .filter_map(|(dim, selected)| {
                let values = dims.get(dim)?;
                Some((
                    dim.clone(),
                    values
                        .iter()
                        .filter(|v| selected.contains(*v))
                        .cloned()
                        .collect(),
                ))
            })
            .collect();
        if filtered.iter().any(|(_, kept)| kept.is_empty()) {
            return;
        }
        dims.extend(filtered);
    }

    /// Whether a node with these dimensions is narrowed: it has a named dimension.
    pub(crate) fn narrows(
        &self,
        dims: &HashMap<String, barca_model::model::PartitionSpec>,
    ) -> bool {
        self.dimensions.keys().any(|d| dims.contains_key(d))
    }
}
