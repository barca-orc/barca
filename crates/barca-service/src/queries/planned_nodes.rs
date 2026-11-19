//! What became of each node of a finished run ([`planned_run_nodes`]): the run's recorded steps
//! (`db::run_node_summaries`) set against the plan rebuilt from today's sources.

use crate::dag::Dag;
use crate::db::RunNodeSummary;
use crate::planner::ResourceConfig;
use crate::step_status::StepStatus;
use crate::targets;

/// Every node a finished run planned, in plan order, each with its [`RunNodeSummary::status`]:
/// `recorded` (the nodes with steps, from [`db::run_node_summaries`]) completed with the planned
/// nodes that recorded none. Such a node is `skipped` when a node upstream of it failed
/// (`blocked_by`) or when the run broke off before its phase (a single-target run stops at the
/// phase of its first failure; a run of several targets goes on), else `cached`. None when the
/// plan rebuilt from today's sources does not have `steps_total` steps: the sources changed
/// since the run, so nothing can be said about what it skipped.
pub fn planned_run_nodes(
    dag: &Dag,
    command: &str,
    target_ids: &[&str],
    status: &str,
    steps_total: Option<i64>,
    recorded: Vec<RunNodeSummary>,
) -> Option<Vec<RunNodeSummary>> {
    use std::collections::{HashMap, HashSet};
    // An interrupted call may have started without recording a result. Absence of a
    // step is not evidence of a cache hit when the run did not finish normally.
    if !matches!(status, "success" | "failed") {
        return None;
    }
    let config = ResourceConfig {
        pool_size: std::thread::available_parallelism().map_or(4, |n| n.get()),
        concurrency_groups: HashMap::new(),
    };
    let plan = targets::plan_for_targets(dag, target_ids, &config, command, None);
    if plan.phases.is_empty() || steps_total != Some(plan.total_steps as i64) {
        return None;
    }
    let mut phase_of: HashMap<&str, usize> = HashMap::new();
    let mut planned: Vec<&str> = Vec::new();
    for (i, phase) in plan.phases.iter().enumerate() {
        for step in phase.streams.iter().flat_map(|s| &s.steps) {
            if !phase_of.contains_key(step.step_id.base_id()) {
                phase_of.insert(step.step_id.base_id(), i);
                planned.push(step.step_id.base_id());
            }
        }
    }
    let mut by_id: HashMap<String, RunNodeSummary> = recorded
        .into_iter()
        .map(|n| (n.node_id.clone(), n))
        .collect();
    let failed: HashSet<String> = by_id
        .values()
        .filter(|n| n.failed > 0)
        .map(|n| n.node_id.clone())
        .collect();
    // A single-target run breaks off after the phase of its first failure; later phases were
    // never decided. A successful run, or one of several targets, reached every phase.
    let reached = if status == "success" || target_ids.len() > 1 {
        usize::MAX
    } else {
        by_id
            .keys()
            .filter_map(|id| phase_of.get(id.as_str()).copied())
            .max()
            .unwrap_or(0)
    };
    let mut nodes = Vec::with_capacity(planned.len());
    for id in planned {
        if let Some(mut node) = by_id.remove(id) {
            node.status = Some(StepStatus::Ran.to_string());
            nodes.push(node);
            continue;
        }
        let blocked_by = targets::blocking_failure(dag, id, &failed);
        let skipped = blocked_by.is_some() || phase_of.get(id).copied().unwrap_or(0) > reached;
        nodes.push(RunNodeSummary {
            node_id: id.to_string(),
            status: Some(
                if skipped {
                    StepStatus::Skipped
                } else {
                    StepStatus::Cached
                }
                .to_string(),
            ),
            blocked_by: blocked_by.map(str::to_string),
            ..Default::default()
        });
    }
    nodes.extend(by_id.into_values());
    Some(nodes)
}

#[cfg(test)]
mod tests {
    use super::{RunNodeSummary, planned_run_nodes};

    const SRC: &str = "from barca import asset\n\
@asset()\ndef accounts(): return 1\n\
@asset()\ndef prices(): return 1\n\
@asset(inputs={'accounts': accounts})\ndef forecast(accounts): return 1\n\
@asset(inputs={'forecast': forecast, 'prices': prices})\ndef plan(forecast, prices): return 1\n\
@asset(inputs={'plan': plan})\ndef publish(plan): return 1\n\
@asset()\ndef unrelated(): return 1\n";

    fn dag() -> crate::dag::Dag {
        crate::dag::Dag::build(&crate::parse::extract_nodes(SRC, "p.py").unwrap()).unwrap()
    }

    fn ran(node_id: &str, keys: usize, failed: usize) -> RunNodeSummary {
        RunNodeSummary {
            node_id: node_id.into(),
            keys,
            failed,
            ..Default::default()
        }
    }

    fn outcomes(nodes: &[RunNodeSummary]) -> Vec<(&str, &str, Option<&str>)> {
        nodes
            .iter()
            .map(|n| {
                (
                    n.node_id.as_str(),
                    n.status.as_deref().unwrap(),
                    n.blocked_by.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn a_failure_skips_what_depends_on_it_and_the_rest_was_cached() {
        let nodes = planned_run_nodes(
            &dag(),
            "get",
            &["p.py:publish"],
            "failed",
            Some(5),
            vec![ran("p.py:forecast", 3, 1)],
        )
        .unwrap();
        let mut sorted = outcomes(&nodes);
        sorted.sort();
        assert_eq!(
            sorted,
            [
                ("p.py:accounts", "cached", None),
                ("p.py:forecast", "ran", None),
                ("p.py:plan", "skipped", Some("p.py:forecast")),
                ("p.py:prices", "cached", None),
                ("p.py:publish", "skipped", Some("p.py:forecast")),
            ]
        );
        // In plan order: a node after what it depends on.
        let at = |id: &str| nodes.iter().position(|n| n.node_id == id).unwrap();
        assert!(at("p.py:accounts") < at("p.py:forecast"));
        assert!(at("p.py:forecast") < at("p.py:plan") && at("p.py:plan") < at("p.py:publish"));
    }

    #[test]
    fn a_successful_run_cached_every_node_without_steps() {
        let nodes = planned_run_nodes(
            &dag(),
            "get",
            &["p.py:publish"],
            "success",
            Some(5),
            vec![ran("p.py:publish", 1, 0)],
        )
        .unwrap();
        let statuses: Vec<&str> = nodes.iter().map(|n| n.status.as_deref().unwrap()).collect();
        assert_eq!(statuses, ["cached", "cached", "cached", "cached", "ran"]);
    }

    #[test]
    fn a_run_whose_plan_changed_says_nothing() {
        // Today's sources plan 5 nodes for `publish`; the run recorded 4.
        let dag = dag();
        assert!(
            planned_run_nodes(&dag, "get", &["p.py:publish"], "failed", Some(4), vec![]).is_none()
        );
        assert!(
            planned_run_nodes(&dag, "get", &["p.py:publish"], "failed", None, vec![]).is_none()
        );
    }

    #[test]
    fn interrupted_runs_do_not_infer_cache_hits_from_missing_steps() {
        for status in ["cancelled", "interrupted", "error"] {
            assert!(
                planned_run_nodes(&dag(), "get", &["p.py:publish"], status, Some(5), vec![])
                    .is_none()
            );
        }
    }

    #[test]
    fn everything_plans_the_gettable_graph() {
        let nodes = planned_run_nodes(&dag(), "get", &[], "success", Some(6), vec![]).unwrap();
        assert_eq!(nodes.len(), 6);
        assert!(nodes.iter().all(|n| n.status.as_deref() == Some("cached")));
    }
}
