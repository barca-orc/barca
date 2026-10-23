//! `--partition`: which keys a run computes, across the whole planned cone.
//!
//! A selection names one or more dimensions and, for each, the values to keep:
//! `region=emea,amer` or, for compound keys, a JSON object
//! `{"region": "emea", "day": ["2026-10-13", "2026-10-14"]}`.
//!
//! [`PartitionSelection::apply`] narrows the plan of one run:
//!
//! - every planned partitioned node that has a named dimension keeps only the selected keys
//!   (it is *narrowed*), upstream and downstream of the targets alike;
//! - a node that reads a narrowed node across keys (a `collect()` fan-in, or an input whose
//!   keys do not line up with its own) is *skipped*, and so is everything below it: computed
//!   over some keys only it would be a different result;
//! - every other node is planned as without the flag.
//!
//! A narrowed key is computed exactly as a full run computes it, so it has the same run hash
//! and a later full run serves it from cache.

use crate::BarcaError;
use crate::dag::Dag;
use crate::dispatch::OutputRef;
use crate::model::PartitionKey;
use crate::planner::{ExecutionPlan, Phase};
use crate::targets::short_name;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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

    /// Whether `key` is selected: each named dimension that `key` has holds one of its listed
    /// values. Dimensions the key does not have are not checked, so a key with none of them
    /// matches; [`Self::apply`] narrows only nodes that have at least one.
    pub fn matches(&self, key: &PartitionKey) -> bool {
        self.0
            .iter()
            .all(|(dim, values)| key.0.get(dim).is_none_or(|v| values.contains(v)))
    }

    /// The planner's pre-filter for one node's static dimensions (dimension → its values): when
    /// the node is narrowed, keep only the selected values of each named dimension, so that
    /// the keys left out are never built. A dimension that would be left with no value is kept
    /// whole; [`Self::apply`] then removes the node's keys.
    pub fn prefilter(&self, dims: &mut HashMap<String, Vec<String>>) {
        if !self.0.keys().any(|d| dims.contains_key(d)) {
            return;
        }
        let filtered: Vec<(String, Vec<String>)> = self
            .0
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
    fn narrows(&self, dims: &HashMap<String, crate::model::PartitionSpec>) -> bool {
        self.0.keys().any(|d| dims.contains_key(d))
    }

    /// Narrow `plan` (the plan of one run, for `targets`) to the selection; see the module
    /// documentation. A usage error, before anything runs, when no planned node has a named
    /// dimension, when a selected value is a key of no planned node with that dimension (when
    /// those keys are known now), or when a target is a node the selection skips.
    pub fn apply(
        &self,
        dag: &Dag,
        plan: &mut ExecutionPlan,
        targets: &[&str],
    ) -> Result<Narrowing, BarcaError> {
        let planned_set: HashSet<&str> = plan.step_ids().collect();
        let planned: Vec<&str> = dag
            .topo_order()
            .into_iter()
            .filter(|id| planned_set.contains(id))
            .collect();
        let dims_of = |id: &str| dag.get_node(id).map(|n| &n.extracted.partitions);

        // Every named dimension must be a dimension of something this run plans.
        let cone_dims: BTreeSet<&str> = planned
            .iter()
            .filter_map(|id| dims_of(id))
            .flat_map(|d| d.keys().map(String::as_str))
            .collect();
        if let Some(dim) = self.0.keys().find(|d| !cone_dims.contains(d.as_str())) {
            let scope = match targets {
                [] => "this run".to_string(),
                [one] => format!("the cone of '{}'", short_name(one)),
                _ => "the cones of these targets".to_string(),
            };
            return Err(BarcaError::Usage(if cone_dims.is_empty() {
                format!(
                    "--partition: nothing in {scope} is partitioned, so there are no keys to \
                     select.\nDrop --partition."
                )
            } else {
                format!(
                    "--partition: no node in {scope} has dimension '{dim}'. Dimensions: {}",
                    cone_dims.iter().copied().collect::<Vec<_>>().join(", ")
                )
            }));
        }

        let narrowed: HashSet<&str> = planned
            .iter()
            .copied()
            .filter(|id| dims_of(id).is_some_and(|d| self.narrows(d)))
            .collect();

        // Skipped: a node reading a narrowed node across keys, and everything below one.
        let mut skipped: Vec<SkippedNode> = Vec::new();
        let mut skipped_ids: HashSet<&str> = HashSet::new();
        for id in &planned {
            let Some(node) = dag.get_node(id) else {
                continue;
            };
            let reads = node
                .resolved_inputs
                .values()
                .map(|u| (u.as_str(), false))
                .chain(node.resolved_collected.values().map(|u| (u.as_str(), true)));
            let mut fan_in: Option<&str> = None;
            let mut below: Option<&str> = None;
            for (up, collected) in reads {
                if skipped_ids.contains(up) {
                    below.get_or_insert(up);
                } else if narrowed.contains(up) {
                    let aligned = !collected && {
                        let mine: BTreeSet<&String> = node.extracted.partitions.keys().collect();
                        let theirs: BTreeSet<&String> =
                            dims_of(up).map(|d| d.keys().collect()).unwrap_or_default();
                        !mine.is_empty() && mine == theirs
                    };
                    if !aligned {
                        fan_in = Some(match fan_in {
                            Some(prev) if prev < up => prev,
                            _ => up,
                        });
                    }
                }
            }
            let short = short_name(id);
            let entry = match (fan_in, below) {
                (Some(up), _) => SkippedNode {
                    id: id.to_string(),
                    fan_in: true,
                    detail: format!(
                        "{FAN_IN_DETAIL} '{}', and --partition selects only some; run without \
                         --partition to update it",
                        short_name(up)
                    ),
                },
                (None, Some(up)) => SkippedNode {
                    id: id.to_string(),
                    fan_in: false,
                    detail: format!(
                        "depends on '{}', which --partition skips; run without --partition to \
                         update it",
                        short_name(up)
                    ),
                },
                (None, None) => continue,
            };
            if targets.contains(id) {
                let why = match (fan_in, below) {
                    (Some(up), _) => format!("reads every key of '{}'", short_name(up)),
                    (None, Some(up)) => {
                        format!("depends on '{}', which --partition skips", short_name(up))
                    }
                    (None, None) => unreachable!("a skipped node has a reason"),
                };
                return Err(BarcaError::Usage(format!(
                    "--partition: '{short}' {why}, so it cannot be computed for a selection of \
                     keys.\nGet '{short}' without --partition."
                )));
            }
            skipped_ids.insert(id);
            skipped.push(entry);
        }

        self.check_values(dag, plan, &narrowed)?;

        // Narrow: drop the keys that are not selected and the skipped nodes' steps.
        let mut counts: BTreeMap<String, Option<KeyCounts>> = narrowed
            .iter()
            .filter(|id| !skipped_ids.contains(*id))
            .map(|id| (id.to_string(), None))
            .collect();
        for phase in &mut plan.phases {
            for stream in &mut phase.streams {
                stream.steps.retain_mut(|step| {
                    let base = step.step_id.base_id();
                    if skipped_ids.contains(base) {
                        return false;
                    }
                    if !narrowed.contains(base) || step.partition_keys.is_empty() {
                        return true; // not narrowed, or keys known only once its source ran
                    }
                    step.partition_keys.retain(|k| self.matches(k));
                    let entry = counts
                        .entry(base.to_string())
                        .or_default()
                        .get_or_insert_with(KeyCounts::default);
                    entry.selected += step.partition_keys.len();
                    !step.partition_keys.is_empty()
                });
            }
            phase.streams.retain(|s| !s.steps.is_empty());
        }
        plan.phases.retain(|p| !p.streams.is_empty());
        // The planner built only the selected keys: what was left out is counted from the
        // declared keys.
        for (id, c) in counts.iter_mut() {
            if let Some(c) = c {
                c.skipped = static_key_count(dag, id).saturating_sub(c.selected);
            }
        }
        plan.total_steps = plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|st| st.partition_keys.len().max(1))
            .sum();
        Ok(Narrowing {
            selection: self.clone(),
            nodes: counts,
            skipped,
        })
    }

    /// Every selected value must be a key of some narrowed node with its dimension. Checked
    /// only when all those nodes' keys are known now: a `partitions_from` list's keys are
    /// filtered once its source ran ([`Narrowing::narrow_expanded`]).
    fn check_values(
        &self,
        dag: &Dag,
        plan: &ExecutionPlan,
        narrowed: &HashSet<&str>,
    ) -> Result<(), BarcaError> {
        let steps: Vec<&crate::planner::StreamStep> = plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .filter(|st| narrowed.contains(st.step_id.base_id()))
            .collect();
        for (dim, values) in &self.0 {
            let carriers = || {
                steps
                    .iter()
                    .filter(|st| st.partition_keys.iter().any(|k| k.0.contains_key(dim)))
            };
            let late = steps
                .iter()
                .any(|st| st.pending_partitions.contains_key(dim));
            if late {
                continue;
            }
            let mut found: HashSet<&str> = HashSet::new();
            for key in carriers().flat_map(|st| &st.partition_keys) {
                if let Some(v) = key.0.get(dim)
                    && values.contains(v)
                {
                    found.insert(v);
                }
            }
            let missing: Vec<&str> = values
                .iter()
                .map(String::as_str)
                .filter(|v| !found.contains(v))
                .collect();
            if missing.is_empty() {
                continue;
            }
            // The declared keys: the planner built only the selected ones.
            let have: BTreeSet<String> = narrowed
                .iter()
                .filter_map(|id| dag.get_node(id))
                .filter_map(|n| match n.extracted.partitions.get(dim) {
                    Some(crate::model::PartitionSpec::Static { values }) => Some(values),
                    _ => None,
                })
                .flatten()
                .map(value_string)
                .collect();
            let preview: Vec<&str> = have.iter().take(20).map(String::as_str).collect();
            return Err(BarcaError::Usage(format!(
                "--partition: no node in this run has the key {dim}={}{}. Its {dim} values: {}{}",
                missing[0],
                match missing.len() {
                    1 => String::new(),
                    n => format!(
                        " (nor {} other selected value{})",
                        n - 1,
                        if n == 2 { "" } else { "s" }
                    ),
                },
                preview.join(", "),
                if have.len() > preview.len() {
                    format!(" (and {} more)", have.len() - preview.len())
                } else {
                    String::new()
                }
            )));
        }
        Ok(())
    }
}

/// The note [`Narrowing::note`] prints, from the skipped fan-ins (node ids) and how many nodes
/// are skipped in all.
fn note_of(fan_ins: &[&str], skipped: usize) -> Option<String> {
    if fan_ins.is_empty() {
        return None;
    }
    let names: Vec<&str> = fan_ins.iter().map(|id| short_name(id)).collect();
    let below = skipped.saturating_sub(fan_ins.len());
    Some(format!(
        "[barca] note: --partition skips {} that read{} every key: {}{}. Run without \
         --partition to update {}.",
        if names.len() == 1 {
            "a fan-in"
        } else {
            "fan-ins"
        },
        if names.len() == 1 { "s" } else { "" },
        names.join(", "),
        match (below, names.len()) {
            (0, _) => String::new(),
            (1, 1) => ", and 1 node below it".to_string(),
            (1, _) => ", and 1 node below them".to_string(),
            (n, 1) => format!(", and {n} nodes below it"),
            (n, _) => format!(", and {n} nodes below them"),
        },
        if skipped == 1 { "it" } else { "them" },
    ))
}

/// The note of a dry run, rebuilt from its result (a dry run on a server prints it on the
/// client): the skipped nodes whose step reads a narrowed node itself are the fan-ins.
pub fn explain_note(result: &crate::results::ExplainResult) -> Option<String> {
    let selection = result.partition_selection.as_ref()?;
    let fan_ins: Vec<&str> = selection
        .skipped_nodes
        .iter()
        .map(String::as_str)
        .filter(|id| {
            result.steps.iter().any(|st| {
                st.id == *id
                    && st
                        .detail
                        .as_deref()
                        .is_some_and(|d| d.starts_with(FAN_IN_DETAIL))
            })
        })
        .collect();
    note_of(&fan_ins, selection.skipped_nodes.len())
}

/// How a skipped fan-in's step detail begins.
const FAN_IN_DETAIL: &str = "reads every key of";

/// A declared partition value as it appears in a key.
fn value_string(v: &crate::model::PartitionValue) -> String {
    match v {
        crate::model::PartitionValue::Str(s) => s.clone(),
        crate::model::PartitionValue::Int(i) => i.to_string(),
    }
}

/// How many keys a node with only static dimensions declares (the product of their sizes).
fn static_key_count(dag: &Dag, id: &str) -> usize {
    dag.get_node(id).map_or(0, |n| {
        n.extracted
            .partitions
            .values()
            .map(|spec| match spec {
                crate::model::PartitionSpec::Static { values } => values.len(),
                _ => 0,
            })
            .product()
    })
}

/// How many keys of one narrowed node a selection kept, and how many it left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct KeyCounts {
    pub selected: usize,
    pub skipped: usize,
}

/// A node a selection does not run, and why (a step report's `detail`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedNode {
    pub id: String,
    /// It reads a narrowed node across keys itself (a fan-in), rather than being below one.
    pub fan_in: bool,
    pub detail: String,
}

/// What [`PartitionSelection::apply`] did to one run's plan.
#[derive(Debug, Clone, Default)]
pub struct Narrowing {
    pub selection: PartitionSelection,
    /// The narrowed nodes, by id: the keys kept and left out, `None` while they are not known
    /// (the keys of a `partitions_from` list, before its source ran).
    pub nodes: BTreeMap<String, Option<KeyCounts>>,
    /// The nodes not run, in plan order.
    pub skipped: Vec<SkippedNode>,
}

impl Narrowing {
    /// Narrow a phase whose `partitions_from` keys were just expanded. A narrowed node whose
    /// expanded keys include none of the selected ones gets a warning: its keys were not known
    /// when the command started, so this is the first point to say so.
    pub fn narrow_expanded(&mut self, phase: &mut Phase) {
        let mut seen: BTreeMap<String, KeyCounts> = BTreeMap::new();
        for stream in &mut phase.streams {
            stream.steps.retain_mut(|step| {
                let base = step.step_id.base_id();
                // Only steps whose keys are not counted yet: those just expanded.
                if step.partition_keys.is_empty() || !matches!(self.nodes.get(base), Some(None)) {
                    return true;
                }
                let total = step.partition_keys.len();
                step.partition_keys.retain(|k| self.selection.matches(k));
                let entry = seen.entry(base.to_string()).or_default();
                entry.selected += step.partition_keys.len();
                entry.skipped += total - step.partition_keys.len();
                !step.partition_keys.is_empty()
            });
        }
        phase.streams.retain(|s| !s.steps.is_empty());
        for (id, counts) in seen {
            if counts.selected == 0 {
                crate::run_line!(
                    Always,
                    "[barca] warning: --partition: '{}' has none of the selected keys; nothing \
                     of it is computed",
                    short_name(&id)
                );
            }
            self.nodes.insert(id, Some(counts));
        }
    }

    /// The step reports of the skipped nodes: status `skipped` (action `skipped` in a dry run,
    /// whose steps have an action), reason `partition_selection`.
    pub fn skipped_reports(&self, dag: &Dag, dry_run: bool) -> Vec<crate::results::StepReport> {
        self.skipped
            .iter()
            .map(|s| crate::results::StepReport {
                id: s.id.clone(),
                kind: crate::report::kind_str(dag.get_node(&s.id).map(|n| n.kind())),
                status: (!dry_run).then(|| "skipped".to_string()),
                action: dry_run.then(|| "skipped".to_string()),
                reason: Some("partition_selection".to_string()),
                detail: Some(s.detail.clone()),
                ..Default::default()
            })
            .collect()
    }

    /// The one stderr note naming the skipped fan-ins, or `None` when nothing is skipped.
    pub fn note(&self) -> Option<String> {
        let fan_ins: Vec<&str> = self
            .skipped
            .iter()
            .filter(|s| s.fan_in)
            .map(|s| s.id.as_str())
            .collect();
        note_of(&fan_ins, self.skipped.len())
    }

    /// The `partition_selection` object of the JSON results.
    pub fn report(&self) -> crate::results::PartitionSelectionReport {
        crate::results::PartitionSelectionReport {
            selection: self
                .selection
                .0
                .iter()
                .map(|(d, vs)| (d.clone(), vs.iter().cloned().collect()))
                .collect(),
            nodes: self
                .nodes
                .iter()
                .map(|(id, c)| crate::results::SelectedNode {
                    id: id.clone(),
                    selected: c.map(|c| c.selected),
                    skipped: c.map(|c| c.skipped),
                })
                .collect(),
            skipped_nodes: self.skipped.iter().map(|s| s.id.clone()).collect(),
        }
    }

    /// A usage error when `--refresh` names a node the selection skips.
    pub fn check_refresh(&self, names: &[String]) -> Result<(), BarcaError> {
        for name in names {
            if let Some(s) = self
                .skipped
                .iter()
                .find(|s| crate::targets::refresh_name_matches(&s.id, name))
            {
                return Err(BarcaError::Usage(format!(
                    "--refresh {name}: --partition skips '{}': it {}.",
                    short_name(&s.id),
                    s.detail
                )));
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
            // Without a selection the key is the id's bracketed suffix as it is: no parse
            // for each of (say) 200k keys.
            if selection.is_none() {
                let suffix = id.get(prefix.len()..id.len().saturating_sub(1))?;
                let branch = suffix.starts_with("_branch=") || suffix.contains(",_branch=");
                return (!branch).then(|| (suffix.to_string(), oref.clone()));
            }
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
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
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

    use crate::planner::{ResourceConfig, plan_from_dag_narrowed};

    const SRC: &str = r#"
from barca import asset, collect, partitions, partitions_from, task

@asset()
def rate():
    return 2

@asset(inputs={"rate": rate}, partitions={"retailer": partitions(["r1", "r2", "r3", "r4"])})
def orders(retailer, rate):
    return 1

@asset(partitions={"retailer": partitions_from(orders)})
def revenue(retailer, orders):
    return 1

@asset(partitions={"retailer": partitions(["r1", "r2", "r9"])})
def returns(retailer):
    return 1

@asset(partitions={"day": partitions(["d1", "d2"])})
def calendar(day):
    return 1

@asset(inputs={"calendar": calendar}, partitions={"retailer": partitions_from(revenue)})
def daily(retailer, revenue, calendar):
    return 1

@asset(inputs={"all": collect(revenue)})
def total(all):
    return 1

@asset(inputs={"total": total})
def report(total):
    return 1

@asset()
def names():
    return ["r1", "r2"]

@asset(partitions={"retailer": partitions_from(names)})
def late(retailer):
    return 1

@task(partitions={"retailer": partitions_from(orders)})
def notify(retailer, orders):
    return None
"#;

    fn dag() -> Dag {
        Dag::build(&crate::parse::extract_nodes(SRC, "p.py").unwrap()).unwrap()
    }

    fn plan_of(dag: &Dag, sel: Option<&PartitionSelection>) -> ExecutionPlan {
        let config = ResourceConfig {
            pool_size: 2,
            concurrency_groups: Default::default(),
        };
        plan_from_dag_narrowed(dag, &config, sel)
    }

    /// Every planned step's display id, one per key, sorted.
    fn planned(plan: &ExecutionPlan) -> Vec<String> {
        let mut ids: Vec<String> = plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .flat_map(|st| {
                if st.partition_keys.is_empty() {
                    vec![st.step_id.display()]
                } else {
                    st.partition_keys
                        .iter()
                        .map(|k| k.display_id(st.step_id.base_id()))
                        .collect()
                }
            })
            .collect();
        ids.sort();
        ids
    }

    fn apply(raw: &str, targets: &[&str]) -> Result<(ExecutionPlan, Narrowing), BarcaError> {
        let dag = dag();
        let sel = PartitionSelection::parse(raw).unwrap();
        let mut plan = plan_of(&dag, Some(&sel));
        if !targets.is_empty() {
            plan = crate::queries::filter_plan_to_subgraph(plan, &dag.subgraph_many(targets));
        }
        let n = sel.apply(&dag, &mut plan, targets)?;
        Ok((plan, n))
    }

    #[test]
    fn a_selection_narrows_every_node_with_its_dimension() {
        let (plan, n) = apply("retailer=r1,r2", &[]).unwrap();
        let ids = planned(&plan);
        for id in [
            "p.py:orders[retailer=r1]",
            "p.py:orders[retailer=r2]",
            "p.py:revenue[retailer=r2]",
            "p.py:returns[retailer=r1]",
            "p.py:daily[retailer=r1]",
            "p.py:late",
            "p.py:notify[retailer=r2]",
        ] {
            assert!(ids.iter().any(|i| i == id), "{id} in {ids:?}");
        }
        assert!(
            !ids.iter().any(|i| i.contains("r3") || i.contains("r9")),
            "{ids:?}"
        );
        // The upstream on another dimension keeps every key; an unpartitioned input runs.
        assert!(ids.contains(&"p.py:calendar[day=d1]".to_string()));
        assert!(ids.contains(&"p.py:calendar[day=d2]".to_string()));
        assert!(ids.contains(&"p.py:rate".to_string()));
        assert_eq!(
            n.nodes.get("p.py:orders").copied().flatten(),
            Some(KeyCounts {
                selected: 2,
                skipped: 2
            })
        );
        assert_eq!(
            n.nodes.get("p.py:returns").copied().flatten(),
            Some(KeyCounts {
                selected: 2,
                skipped: 1
            })
        );
        // Keys of a list known only after it ran: counted later.
        assert_eq!(n.nodes.get("p.py:late"), Some(&None));
        assert!(!n.nodes.contains_key("p.py:calendar"));
        assert_eq!(plan.total_steps, ids.len());
    }

    #[test]
    fn a_fan_in_over_selected_keys_and_what_is_below_it_are_skipped() {
        let (plan, n) = apply("retailer=r1", &[]).unwrap();
        let ids = planned(&plan);
        assert!(!ids.iter().any(|i| i == "p.py:total" || i == "p.py:report"));
        let skipped: Vec<(&str, bool)> = n
            .skipped
            .iter()
            .map(|s| (s.id.as_str(), s.fan_in))
            .collect();
        assert_eq!(skipped, [("p.py:total", true), ("p.py:report", false)]);
        assert!(n.skipped[0].detail.contains("reads every key of 'revenue'"));
        assert!(n.skipped[1].detail.contains("depends on 'total'"));
        let note = n.note().unwrap();
        assert_eq!(
            note,
            "[barca] note: --partition skips a fan-in that reads every key: total, and 1 node \
             below it. Run without --partition to update them."
        );
        let reports = n.skipped_reports(&dag(), false);
        assert_eq!(reports[0].reason.as_deref(), Some("partition_selection"));
        assert_eq!(reports[0].status.as_deref(), Some("skipped"));
    }

    #[test]
    fn a_node_reading_selected_keys_across_dimensions_is_a_fan_in() {
        // `daily` (retailer) reads `calendar` (day): selecting days makes it read some days only.
        let (plan, n) = apply("day=d1", &[]).unwrap();
        let ids = planned(&plan);
        assert!(ids.contains(&"p.py:calendar[day=d1]".to_string()));
        assert!(n.skipped.iter().any(|s| s.id == "p.py:daily" && s.fan_in));
        // Nothing on the other dimension is narrowed.
        assert_eq!(
            ids.iter().filter(|i| i.starts_with("p.py:orders[")).count(),
            4
        );
    }

    #[test]
    fn a_target_the_selection_skips_is_a_usage_error() {
        let err = apply("retailer=r1", &["p.py:total"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'total' reads every key of 'revenue'"),
            "{err}"
        );
        let err = apply("retailer=r1", &["p.py:report"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("depends on 'total'"), "{err}");
    }

    #[test]
    fn unknown_values_and_dimensions_are_usage_errors() {
        // `late`'s keys are not known yet, so the value may be one of them: filtered later.
        assert!(apply("retailer=r7", &[]).is_ok());
        let err = apply("region=emea", &[]).unwrap_err().to_string();
        assert!(
            err.contains("dimension 'region'") && err.contains("day, retailer"),
            "{err}"
        );
        // Within the cone of `revenue` the keys are all known.
        let err = apply("retailer=r7", &["p.py:revenue"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("retailer=r7") && err.contains("r1, r2, r3, r4"),
            "{err}"
        );
        // A value of another node with the dimension is a key of this run.
        assert!(apply("retailer=r9", &["p.py:revenue"]).is_err());
        let (plan, _) = apply("retailer=r9", &[]).unwrap();
        assert_eq!(
            planned(&plan)
                .iter()
                .filter(|i| i.contains("retailer="))
                .collect::<Vec<_>>(),
            ["p.py:returns[retailer=r9]"]
        );
    }

    #[test]
    fn the_planner_builds_only_the_selected_keys() {
        let dag = dag();
        let sel = PartitionSelection::parse("retailer=r2").unwrap();
        let plan = plan_of(&dag, Some(&sel));
        let orders: Vec<String> = planned(&plan)
            .into_iter()
            .filter(|i| i.starts_with("p.py:orders["))
            .collect();
        assert_eq!(orders, ["p.py:orders[retailer=r2]"]);
        // A node with none of the selected values is built whole, and `apply` removes it.
        let sel = PartitionSelection::parse("retailer=r9").unwrap();
        let mut plan = plan_of(&dag, Some(&sel));
        assert_eq!(
            planned(&plan)
                .iter()
                .filter(|i| i.starts_with("p.py:orders["))
                .count(),
            4
        );
        let n = sel.apply(&dag, &mut plan, &[]).unwrap();
        assert!(!planned(&plan).iter().any(|i| i.starts_with("p.py:orders[")));
        assert_eq!(
            n.nodes.get("p.py:orders").copied().flatten(),
            Some(KeyCounts {
                selected: 0,
                skipped: 4
            })
        );
    }

    #[test]
    fn keys_known_late_are_narrowed_once_expanded() {
        let mut n = Narrowing {
            selection: PartitionSelection::parse("retailer=r2").unwrap(),
            nodes: BTreeMap::from([("p.py:late".to_string(), None)]),
            skipped: Vec::new(),
        };
        let dag = dag();
        let plan = plan_of(&dag, None);
        let mut phase = plan
            .phases
            .iter()
            .find(|p| {
                p.streams
                    .iter()
                    .flat_map(|s| &s.steps)
                    .any(|s| s.step_id.base_id() == "p.py:late")
            })
            .unwrap()
            .clone();
        for st in phase.streams.iter_mut().flat_map(|s| &mut s.steps) {
            if st.step_id.base_id() == "p.py:late" {
                st.pending_partitions.clear();
                st.partition_keys = vec![key(&[("retailer", "r1")]), key(&[("retailer", "r2")])];
            }
        }
        n.narrow_expanded(&mut phase);
        let late: Vec<&PartitionKey> = phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .filter(|s| s.step_id.base_id() == "p.py:late")
            .flat_map(|s| &s.partition_keys)
            .collect();
        assert_eq!(late, [&key(&[("retailer", "r2")])]);
        assert_eq!(
            n.nodes["p.py:late"],
            Some(KeyCounts {
                selected: 1,
                skipped: 1
            })
        );
    }

    #[test]
    fn refresh_of_a_skipped_node_is_a_usage_error() {
        let (_, n) = apply("retailer=r1", &[]).unwrap();
        assert!(n.check_refresh(&["orders".to_string()]).is_ok());
        let err = n.check_refresh(&["total".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--partition skips 'total'"),
            "{err}"
        );
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
        // A dimension the key does not have is not checked: the node's own dimensions are.
        assert!(s.matches(&key(&[("region", "emea")])));
        assert!(!s.matches(&key(&[("region", "amer")])));
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
