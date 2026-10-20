//! `--partition`: which keys of a partitioned target a `barca get` computes and returns.
//!
//! A selection names one or more dimensions and, for each, the values to keep:
//! `region=emea,amer` or, for compound keys, a JSON object
//! `{"region": "emea", "day": ["2026-10-13", "2026-10-14"]}`. A key is selected when every
//! named dimension has one of the listed values. Only the target's own steps are narrowed:
//! the rest of its cone is planned as before.

use crate::BarcaError;
use crate::dag::Dag;
use crate::dispatch::OutputRef;
use crate::model::PartitionKey;
use crate::planner::{ExecutionPlan, Phase};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The keys `--partition` selects: dimension → accepted values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PartitionSelection(pub BTreeMap<String, BTreeSet<String>>);

const USAGE: &str = "Use --partition region=emea,amer (one dimension, comma-separated values) or a \
     JSON object for compound keys: --partition '{\"region\": \"emea\", \"day\": \"2026-10-14\"}'";

impl PartitionSelection {
    /// Parse the `--partition` argument.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
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
        Ok(Self(dims))
    }

    /// Whether `key` is selected: each named dimension has one of its listed values.
    pub fn matches(&self, key: &PartitionKey) -> bool {
        self.0
            .iter()
            .all(|(dim, values)| key.0.get(dim).is_some_and(|v| values.contains(v)))
    }

    /// Drop the target steps' keys that are not selected. A step left with no key is removed,
    /// and so is a stream left with no step. A step whose keys are not known yet (a
    /// `partitions_from` placeholder) is kept: [`Self::narrow_phase`] runs again after it
    /// expands.
    pub fn narrow_phase(&self, phase: &mut Phase, targets: &[&str]) {
        for stream in &mut phase.streams {
            stream.steps.retain_mut(|step| {
                if !targets.contains(&step.step_id.base_id()) || step.partition_keys.is_empty() {
                    return true;
                }
                step.partition_keys.retain(|k| self.matches(k));
                !step.partition_keys.is_empty()
            });
        }
        phase.streams.retain(|s| !s.steps.is_empty());
    }

    /// [`Self::narrow_phase`] over a whole plan, keeping `total_steps` in step.
    pub fn narrow_plan(&self, plan: &mut ExecutionPlan, targets: &[&str]) {
        for phase in &mut plan.phases {
            self.narrow_phase(phase, targets);
        }
        plan.phases.retain(|p| !p.streams.is_empty());
        plan.total_steps = plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|st| st.partition_keys.len().max(1))
            .sum();
    }

    /// Check the selection against the targets before anything runs: a usage error when a
    /// target is not partitioned, lacks a named dimension, feeds another target of the same
    /// run, or (when its keys are known now) has none of the selected values.
    pub fn validate(
        &self,
        dag: &Dag,
        target_ids: &[&str],
        plan: &ExecutionPlan,
    ) -> Result<(), BarcaError> {
        if target_ids.is_empty() {
            return Err(BarcaError::Usage(
                "--partition selects keys of a target: name the partitioned asset, e.g. \
                 `barca get sales --partition region=emea`"
                    .to_string(),
            ));
        }
        for tid in target_ids {
            let short = crate::targets::short_name(tid);
            let Some(node) = dag.get_node(tid) else {
                continue;
            };
            let dims = &node.extracted.partitions;
            if dims.is_empty() {
                return Err(BarcaError::Usage(format!(
                    "--partition: '{short}' is not partitioned, so it has no keys to select.\n\
                     Drop --partition to get it."
                )));
            }
            for dim in self.0.keys() {
                if !dims.contains_key(dim) {
                    let mut names: Vec<&str> = dims.keys().map(String::as_str).collect();
                    names.sort_unstable();
                    return Err(BarcaError::Usage(format!(
                        "--partition: '{short}' has no dimension '{dim}'. Its dimensions: {}",
                        names.join(", ")
                    )));
                }
            }
            if let Some(other) = target_ids
                .iter()
                .find(|o| *o != tid && dag.subgraph(o).contains(tid))
            {
                return Err(BarcaError::Usage(format!(
                    "--partition: '{short}' is upstream of '{}' in this run, which reads every \
                     key of it. Get the selected keys in a separate command: \
                     barca get {short} --partition ...",
                    crate::targets::short_name(other)
                )));
            }
            // Keys known when the plan was made: every selected value must name one.
            let steps: Vec<_> = plan
                .phases
                .iter()
                .flat_map(|p| &p.streams)
                .flat_map(|s| &s.steps)
                .filter(|st| st.step_id.base_id() == *tid)
                .collect();
            if steps.is_empty() || steps.iter().any(|st| !st.pending_partitions.is_empty()) {
                continue;
            }
            let mut known: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
            for key in steps.iter().flat_map(|st| &st.partition_keys) {
                for (d, v) in &key.0 {
                    known.entry(d.as_str()).or_default().insert(v.as_str());
                }
            }
            for (dim, values) in &self.0 {
                let have = known.get(dim.as_str()).cloned().unwrap_or_default();
                if let Some(missing) = values.iter().find(|v| !have.contains(v.as_str())) {
                    let preview: Vec<&str> = have.iter().take(20).copied().collect();
                    return Err(BarcaError::Usage(format!(
                        "--partition: '{short}' has no key {dim}={missing}. Its {dim} values: {}{}",
                        preview.join(", "),
                        if have.len() > preview.len() {
                            format!(" (and {} more)", have.len() - preview.len())
                        } else {
                            String::new()
                        }
                    )));
                }
            }
        }
        Ok(())
    }
}

/// The keys of a partitioned node that a run returns, sorted by key (`dim=value,...`), with
/// their artifacts. `None` when the node is not partitioned. Keys outside `selection` are
/// left out, and so are keys with no output (a failed or skipped key).
pub fn partition_outputs(
    dag: &Dag,
    node_id: &str,
    all_outputs: &HashMap<String, OutputRef>,
    selection: Option<&PartitionSelection>,
) -> Option<Vec<(String, OutputRef)>> {
    let node = dag.get_node(node_id)?;
    if node.extracted.partitions.is_empty() {
        return None;
    }
    let prefix = format!("{node_id}[");
    let mut keyed: Vec<(String, OutputRef)> = all_outputs
        .iter()
        .filter(|(id, _)| id.starts_with(&prefix))
        .filter_map(|(id, oref)| {
            let step = crate::StepId::parse(id);
            if step.partition.0.contains_key("_branch") {
                return None;
            }
            if selection.is_some_and(|s| !s.matches(&step.partition)) {
                return None;
            }
            Some((step.partition.suffix(), oref.clone()))
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Some(keyed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pairs: &[(&str, &str)]) -> PartitionKey {
        PartitionKey(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn parses_one_dimension_with_several_values() {
        let s = PartitionSelection::parse("region=emea,amer").unwrap();
        assert!(s.matches(&key(&[("region", "emea")])));
        assert!(s.matches(&key(&[("region", "amer")])));
        assert!(!s.matches(&key(&[("region", "apac")])));
    }

    #[test]
    fn a_json_object_selects_compound_keys() {
        let s = PartitionSelection::parse(r#"{"region": "emea", "day": ["d1", "d2"]}"#).unwrap();
        assert!(s.matches(&key(&[("region", "emea"), ("day", "d2")])));
        assert!(!s.matches(&key(&[("region", "emea"), ("day", "d3")])));
        assert!(!s.matches(&key(&[("region", "amer"), ("day", "d1")])));
        // A dimension the key does not have never matches.
        assert!(!s.matches(&key(&[("region", "emea")])));
        let n = PartitionSelection::parse(r#"{"n": 3}"#).unwrap();
        assert!(n.matches(&key(&[("n", "3")])));
    }

    #[test]
    fn malformed_selections_name_the_accepted_forms() {
        for bad in [
            "region",
            "=emea",
            "region=",
            "{\"region\": null}",
            "[1]",
            "{}",
        ] {
            let err = PartitionSelection::parse(bad).unwrap_err();
            assert!(err.contains("--partition"), "{bad}: {err}");
            assert!(err.contains("region=emea,amer"), "{bad}: {err}");
        }
    }
}
