//! Batched partitions (RFC-0013): `batch=` groups the keys of a partitioned `@asset` or
//! `@task` that need to run into fewer calls of its function.
//!
//! Batching is scheduling only. Each key keeps the run hash, artifact and materialization it
//! has without batching; the planner decides which keys run exactly as before, and the keys of
//! a node that run in a phase are then grouped into calls ([`group_keys`]). The coordinator
//! starts a call once every key of it is ready, and leases its keys to one worker together
//! (`crate::coordinator`); the worker calls the function once and reports each key on its
//! own (`python/barca/_worker.py`).
//!
//! This module holds the grouping of a run's keys into calls and the dry run's account of it.
//! Reading `batch=` from the source ([`spec::extract`]), the per-run override (`--batch`,
//! [`BatchOverride`]) and the Python evaluation of `batches(<expression>)` and
//! `batches_by(<function>)` are [`spec`]'s: loading a pipeline needs them.

use crate::model::BatchSpec;

pub(crate) mod spec;

pub(crate) use spec::evaluate_labels;
pub use spec::{BatchOverride, OverrideValue};

// ─── Grouping ────────────────────────────────────────────────────────────────

/// How one node's keys are grouped in a run: its `batch=`, or the run's override of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grouping {
    One,
    Size(usize),
    All,
    Groups {
        groups: Vec<Vec<String>>,
        strict: bool,
    },
    By {
        source_text: String,
        max_size: Option<usize>,
    },
}

impl Grouping {
    /// As the dry run says it: `batch=all`, `batch=20`, `batch=none`.
    pub fn describe(&self) -> String {
        match self {
            Grouping::One => "batch=none".to_string(),
            Grouping::Size(n) => format!("batch={n}"),
            Grouping::All => "batch=all".to_string(),
            Grouping::Groups { .. } => "batch=batches(...)".to_string(),
            Grouping::By { source_text, .. } => format!("batch=batches_by({source_text})"),
        }
    }
}

/// The grouping of `node` in a run with `over`: `None` for a node that does not declare
/// `batch=` (one key per call, and its function takes a single key).
pub(crate) fn effective(node: &crate::model::DagNode, over: &BatchOverride) -> Option<Grouping> {
    let spec = node.extracted.batch.as_ref()?;
    if let Some(v) = over.value_for(node) {
        return Some(match v {
            OverrideValue::One => Grouping::One,
            OverrideValue::Size(n) => Grouping::Size(n as usize),
            OverrideValue::All => Grouping::All,
        });
    }
    Some(match spec {
        BatchSpec::Size { size } => Grouping::Size(*size as usize),
        BatchSpec::All => Grouping::All,
        BatchSpec::Groups { groups, strict } => Grouping::Groups {
            groups: groups.clone(),
            strict: *strict,
        },
        // Evaluated when the pipeline was loaded; a node left with the expression failed then
        // and is not planned.
        BatchSpec::GroupsExpr { strict, .. } => Grouping::Groups {
            groups: Vec::new(),
            strict: *strict,
        },
        BatchSpec::By {
            source_text,
            max_size,
        } => Grouping::By {
            source_text: source_text.clone(),
            max_size: max_size.map(|n| n as usize),
        },
    })
}

/// Group `keys` (each a key's inner display form, `retailer=r1`, in key order) into calls,
/// as indexes into `keys`. `labels` is required for [`Grouping::By`] (one per key).
///
/// Linear in the keys given: the keys that run, not the keys declared.
pub(crate) fn group_keys(
    keys: &[String],
    grouping: &Grouping,
    labels: Option<&[String]>,
) -> Result<Vec<Vec<usize>>, String> {
    let n = keys.len();
    Ok(match grouping {
        Grouping::One => (0..n).map(|i| vec![i]).collect(),
        Grouping::Size(size) => (0..n)
            .collect::<Vec<_>>()
            .chunks((*size).max(1))
            .map(<[usize]>::to_vec)
            .collect(),
        Grouping::All => {
            if n == 0 {
                Vec::new()
            } else {
                vec![(0..n).collect()]
            }
        }
        Grouping::Groups { groups, strict } => {
            let mut group_of: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for (g, members) in groups.iter().enumerate() {
                for m in members {
                    group_of.insert(m.as_str(), g);
                }
            }
            let mut calls: Vec<Vec<usize>> = Vec::new();
            let mut call_of_group: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            let mut alone = Vec::new();
            for (i, key) in keys.iter().enumerate() {
                match group_of.get(key.as_str()) {
                    Some(g) => {
                        let c = *call_of_group.entry(*g).or_insert_with(|| {
                            calls.push(Vec::new());
                            calls.len() - 1
                        });
                        calls[c].push(i);
                    }
                    None if *strict => {
                        alone.push(key.clone());
                    }
                    None => calls.push(vec![i]),
                }
            }
            if !alone.is_empty() {
                let shown: Vec<&str> = alone.iter().take(5).map(String::as_str).collect();
                return Err(format!(
                    "batches(..., strict=True) names no group for the key{} {}{}",
                    if alone.len() == 1 { "" } else { "s" },
                    shown.join(", "),
                    if alone.len() > 5 {
                        format!(" and {} more", alone.len() - 5)
                    } else {
                        String::new()
                    }
                ));
            }
            calls
        }
        Grouping::By { max_size, .. } => {
            let labels = labels.ok_or("batches_by(...) labels were not evaluated")?;
            let mut calls: Vec<Vec<usize>> = Vec::new();
            let mut call_of: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for (i, label) in labels.iter().enumerate().take(n) {
                let c = *call_of.entry(label.as_str()).or_insert_with(|| {
                    calls.push(Vec::new());
                    calls.len() - 1
                });
                calls[c].push(i);
            }
            match max_size {
                Some(size) => calls
                    .into_iter()
                    .flat_map(|c| {
                        c.chunks((*size).max(1))
                            .map(<[usize]>::to_vec)
                            .collect::<Vec<_>>()
                    })
                    .collect(),
                None => calls,
            }
        }
    })
}

// ─── Dry run ─────────────────────────────────────────────────────────────────

/// How many calls `n` keys make under `grouping`, when that follows from the count alone.
fn calls_for(n: usize, grouping: &Grouping) -> Option<usize> {
    match grouping {
        Grouping::One => Some(n),
        Grouping::Size(size) => Some(n.div_ceil((*size).max(1))),
        Grouping::All => Some(usize::from(n > 0)),
        _ => None,
    }
}

/// "3 keys in 1 call (batch=all)".
pub(crate) fn grouping_line(plan: &crate::results::BatchPlan) -> String {
    let keys = format!("{} key{}", plan.keys, if plan.keys == 1 { "" } else { "s" });
    match plan.calls {
        Some(c) => format!(
            "{keys} in {c} call{} ({})",
            if c == 1 { "" } else { "s" },
            plan.grouping
        ),
        None => format!("{keys} in calls by {}", plan.grouping),
    }
}

/// Add the grouping of each batched node's keys that will run to its dry-run report: `batch`,
/// and the line at the end of `detail`.
pub(crate) fn annotate(
    dag: &crate::dag::Dag,
    steps: &mut [crate::results::StepReport],
    over: &BatchOverride,
    python: &std::path::Path,
) -> Result<(), crate::BarcaError> {
    for step in steps.iter_mut() {
        let Some(node) = dag.get_node(&step.id) else {
            continue;
        };
        let Some(grouping) = effective(node, over) else {
            continue;
        };
        let Some(parts) = step.partitions.as_ref() else {
            continue;
        };
        let n = parts.will_run;
        if n == 0 {
            continue;
        }
        let mut calls = calls_for(n, &grouping);
        if calls.is_none() && parts.will_run_keys.len() == n {
            let labels = match &grouping {
                Grouping::By { source_text, .. } => evaluate_labels(
                    python,
                    &node.extracted.source_file,
                    source_text,
                    &parts.will_run_keys,
                )
                .map(Some)
                .map_err(|e| crate::BarcaError::Usage(format!("{}: {e}", step.id)))?,
                _ => None,
            };
            calls = Some(
                group_keys(&parts.will_run_keys, &grouping, labels.as_deref())
                    .map_err(|e| crate::BarcaError::Usage(format!("{}: {e}", step.id)))?
                    .len(),
            );
        }
        let plan = crate::results::BatchPlan {
            grouping: grouping.describe(),
            keys: n,
            calls,
        };
        let line = grouping_line(&plan);
        step.detail = Some(match step.detail.take() {
            Some(d) if !d.is_empty() => format!("{d}; {line}"),
            _ => line,
        });
        step.batch = Some(plan);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("r=k{i}")).collect()
    }

    #[test]
    fn sizes_chunk_in_key_order() {
        assert_eq!(
            group_keys(&keys(5), &Grouping::Size(2), None).unwrap(),
            vec![vec![0, 1], vec![2, 3], vec![4]]
        );
        assert_eq!(
            group_keys(&keys(3), &Grouping::All, None).unwrap(),
            vec![vec![0, 1, 2]]
        );
        assert!(group_keys(&[], &Grouping::All, None).unwrap().is_empty());
        assert_eq!(
            group_keys(&keys(2), &Grouping::One, None).unwrap(),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn explicit_groups_keep_together_and_strays_run_alone_or_fail() {
        let g = Grouping::Groups {
            groups: vec![vec!["r=k2".into(), "r=k0".into()], vec!["r=k9".into()]],
            strict: false,
        };
        assert_eq!(
            group_keys(&keys(3), &g, None).unwrap(),
            vec![vec![0, 2], vec![1]]
        );
        let strict = Grouping::Groups {
            groups: vec![vec!["r=k0".into()]],
            strict: true,
        };
        let err = group_keys(&keys(2), &strict, None).unwrap_err();
        assert!(err.contains("r=k1"), "{err}");
    }

    #[test]
    fn labels_group_then_split_by_max_size() {
        let labels: Vec<String> = ["a", "b", "a", "a", "b"].map(String::from).to_vec();
        let g = Grouping::By {
            source_text: "f".into(),
            max_size: Some(2),
        };
        assert_eq!(
            group_keys(&keys(5), &g, Some(&labels)).unwrap(),
            vec![vec![0, 2], vec![3], vec![1, 4]]
        );
    }

    #[test]
    fn overrides_parse_and_name_nodes() {
        let o = BatchOverride::parse(&["all".into(), "scored=3".into(), "x=none".into()]).unwrap();
        assert_eq!(o.all, Some(OverrideValue::All));
        assert_eq!(
            o.nodes,
            vec![
                ("scored".to_string(), OverrideValue::Size(3)),
                ("x".to_string(), OverrideValue::One)
            ]
        );
        assert_eq!(o.to_flags(), vec!["all", "scored=3", "x=none"]);
        assert!(BatchOverride::parse(&["0".into()]).is_err());
        assert!(BatchOverride::parse(&["many".into()]).is_err());
        assert!(BatchOverride::parse(&["all".into(), "3".into()]).is_err());
    }
}
