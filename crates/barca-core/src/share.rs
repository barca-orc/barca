//! Which targets can share one run.
//!
//! Running several targets as one run computes a step they have in common once instead of
//! once per target. That is the only thing sharing a run buys, and it has a price: the run's
//! phases are barriers, so a target in a later phase waits for every step of the earlier
//! phases, including steps only another target needs. [`shared_run_groups`] therefore puts
//! targets together only where there is something to gain and nothing to lose:
//!
//! 1. Targets are grouped when their cones overlap (they have at least one step in common),
//!    directly or through a third target. Targets with nothing in common stay apart.
//! 2. A group is kept only if its plan makes none of its targets wait for a step that target
//!    does not depend on. A target that would wait is taken out and runs on its own.
//!
//! A target alone in its group runs exactly as it would by itself.

use crate::commands;
use crate::dag::Dag;
use crate::planner::{ExecutionPlan, ResourceConfig};
use std::collections::{HashMap, HashSet};

/// Split `targets` (node ids) into the groups that should each be one run. Every target is in
/// exactly one group; groups, and the targets inside each, keep the order given. An id the DAG
/// does not know is a group of its own.
pub fn shared_run_groups(dag: &Dag, targets: &[String]) -> Vec<Vec<String>> {
    let position: HashMap<&str, usize> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut undecided = overlapping(dag, targets);
    while let Some(group) = undecided.pop() {
        match delayed_target(dag, &group) {
            // Sharing would hold this target back: it runs alone, and what is left is grouped
            // again, since it may have been the only thing connecting the others.
            Some(delayed) => {
                let rest: Vec<String> = group.into_iter().filter(|t| *t != delayed).collect();
                groups.push(vec![delayed]);
                undecided.extend(overlapping(dag, &rest));
            }
            None => groups.push(group),
        }
    }
    groups.sort_by_key(|group| position[group[0].as_str()]);
    groups
}

/// [`shared_run_groups`] for the DAG of `file_args`, read now.
pub async fn shared_run_groups_in(
    file_args: &[String],
    python: &std::path::Path,
    targets: &[String],
) -> Result<Vec<Vec<String>>, crate::BarcaError> {
    let dag = commands::build_dag(file_args, python).await?;
    Ok(shared_run_groups(&dag, targets))
}

/// Group `targets` by overlapping cones: two targets are together when they have a step in
/// common, or are each together with a third. Order is kept.
fn overlapping(dag: &Dag, targets: &[String]) -> Vec<Vec<String>> {
    let cones: Vec<HashSet<&str>> = targets
        .iter()
        .map(|t| dag.subgraph(t).into_iter().collect())
        .collect();
    // group_of[i]: index of the group target i is in. Merging relabels the later group.
    let mut group_of: Vec<usize> = (0..targets.len()).collect();
    for i in 0..targets.len() {
        for j in 0..i {
            if group_of[i] != group_of[j] && !cones[i].is_disjoint(&cones[j]) {
                let (keep, merge) = (group_of[j].min(group_of[i]), group_of[j].max(group_of[i]));
                for g in group_of.iter_mut().filter(|g| **g == merge) {
                    *g = keep;
                }
            }
        }
    }
    let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        match groups.iter_mut().find(|(g, _)| *g == group_of[i]) {
            Some((_, members)) => members.push(target.clone()),
            None => groups.push((group_of[i], vec![target.clone()])),
        }
    }
    groups.into_iter().map(|(_, members)| members).collect()
}

/// The first target of `group` that one run over the whole group would hold back, if any.
fn delayed_target(dag: &Dag, group: &[String]) -> Option<String> {
    if group.len() < 2 {
        return None;
    }
    let ids: Vec<&str> = group.iter().map(String::as_str).collect();
    let config = ResourceConfig {
        pool_size: commands::default_pool_size(),
        concurrency_groups: HashMap::new(),
    };
    let plan = commands::plan_for_targets(dag, &ids, &config, commands::MIXED_COMMAND);
    group
        .iter()
        .find(|target| waits_for_a_step_it_does_not_need(dag, &plan, target))
        .cloned()
}

/// Whether `target` would wait, in `plan`, for a step outside its cone. A phase starts only
/// when the one before it has ended, so the target waits for every step of the phases before
/// the one its own step is in.
fn waits_for_a_step_it_does_not_need(dag: &Dag, plan: &ExecutionPlan, target: &str) -> bool {
    let steps_of = |phase: &crate::planner::Phase| -> Vec<String> {
        phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .map(|st| st.step_id.base_id().to_string())
            .collect()
    };
    let Some(own_phase) = plan
        .phases
        .iter()
        .rposition(|phase| steps_of(phase).iter().any(|id| id == target))
    else {
        return false;
    };
    let cone: HashSet<&str> = dag.subgraph(target).into_iter().collect();
    plan.phases[..own_phase]
        .iter()
        .flat_map(steps_of)
        .any(|id| !cone.contains(id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dag(source: &str) -> Dag {
        let nodes = crate::parse::extract_nodes(source, "p.py").unwrap();
        Dag::build(&nodes).unwrap()
    }

    fn groups(dag: &Dag, targets: &[&str]) -> Vec<Vec<String>> {
        let targets: Vec<String> = targets.iter().map(|t| format!("p.py:{t}")).collect();
        shared_run_groups(dag, &targets)
            .into_iter()
            .map(|g| g.iter().map(|t| t.replace("p.py:", "")).collect())
            .collect()
    }

    const SHARED_UPSTREAM: &str = r#"
from barca import asset, sensor, task


@sensor()
def version() -> tuple[bool, str]:
    return True, "v1"


@asset(inputs={"version": version})
def tracked(version: str) -> dict:
    return {}


@task(inputs={"tracked": tracked})
def report(tracked: dict) -> None:
    pass


@asset()
def base() -> dict:
    return {}


@asset(inputs={"base": base})
def left(base: dict) -> dict:
    return {}


@task(inputs={"base": base})
def right(base: dict) -> None:
    pass


@asset()
def alone() -> dict:
    return {}


@task()
def other() -> None:
    pass
"#;

    #[test]
    fn targets_with_a_step_in_common_share_a_run() {
        let dag = dag(SHARED_UPSTREAM);
        // The issue's case (#253): a scheduled asset that a scheduled task reads.
        assert_eq!(
            groups(&dag, &["tracked", "report"]),
            [["tracked", "report"]]
        );
        // Two targets reading the same upstream.
        assert_eq!(groups(&dag, &["left", "right"]), [["left", "right"]]);
        // A target and its own upstream.
        assert_eq!(groups(&dag, &["base", "right"]), [["base", "right"]]);
    }

    #[test]
    fn targets_with_nothing_in_common_run_apart() {
        let dag = dag(SHARED_UPSTREAM);
        assert_eq!(groups(&dag, &["alone", "other"]), [["alone"], ["other"]]);
        // Mixed: each group keeps the order the targets were given in.
        assert_eq!(
            groups(
                &dag,
                &["other", "left", "report", "alone", "right", "tracked"]
            ),
            vec![
                vec!["other"],
                vec!["left", "right"],
                vec!["report", "tracked"],
                vec!["alone"],
            ]
        );
    }

    #[test]
    fn one_target_or_none_needs_no_grouping() {
        let dag = dag(SHARED_UPSTREAM);
        assert_eq!(groups(&dag, &["left"]), [["left"]]);
        assert!(groups(&dag, &[]).is_empty());
    }

    #[test]
    fn a_target_the_dag_does_not_know_is_on_its_own() {
        let dag = dag(SHARED_UPSTREAM);
        assert_eq!(
            groups(&dag, &["left", "gone", "right"]),
            vec![vec!["left", "right"], vec!["gone"]]
        );
    }

    /// `quick` reads only `shared`. `joined` reads `shared` and `slow_root`, so its run plans
    /// `slow_root` in the first phase, and `quick` (whose step is in the second) would wait
    /// for it.
    const ONE_WOULD_WAIT: &str = r#"
from barca import asset, task


@asset()
def shared() -> dict:
    return {}


@asset()
def slow_root() -> dict:
    return {}


@asset(inputs={"shared": shared})
def quick(shared: dict) -> dict:
    return {}


@asset(inputs={"shared": shared})
def quick_too(shared: dict) -> dict:
    return {}


@task(inputs={"shared": shared, "slow_root": slow_root})
def joined(shared: dict, slow_root: dict) -> None:
    pass
"#;

    #[test]
    fn a_target_that_would_wait_for_a_step_it_does_not_need_runs_alone() {
        let dag = dag(ONE_WOULD_WAIT);
        // The premise: in one run over both, `slow_root` is planned in a phase before `quick`.
        let config = ResourceConfig {
            pool_size: 4,
            concurrency_groups: HashMap::new(),
        };
        let both = ["p.py:quick", "p.py:joined"];
        let plan = commands::plan_for_targets(&dag, &both, &config, commands::MIXED_COMMAND);
        assert!(waits_for_a_step_it_does_not_need(&dag, &plan, "p.py:quick"));
        assert!(!waits_for_a_step_it_does_not_need(
            &dag,
            &plan,
            "p.py:joined"
        ));

        assert_eq!(groups(&dag, &["quick", "joined"]), [["quick"], ["joined"]]);
    }

    #[test]
    fn the_targets_left_after_one_is_taken_out_are_grouped_again() {
        let dag = dag(ONE_WOULD_WAIT);
        // Every target taken out leaves the rest to be judged again: no group that is kept
        // holds a target back.
        let all = groups(&dag, &["quick", "quick_too", "joined"]);
        let flat: Vec<&String> = all.iter().flatten().collect();
        assert_eq!(
            flat.len(),
            3,
            "every target is in exactly one group: {all:?}"
        );
        for group in &all {
            let ids: Vec<String> = group.iter().map(|t| format!("p.py:{t}")).collect();
            assert_eq!(delayed_target(&dag, &ids), None, "{group:?} in {all:?}");
        }
        // `joined` cannot be with the quick ones; whether they share is up to their plan.
        let with_joined = all.iter().find(|g| g.contains(&"joined".to_string()));
        assert_eq!(with_joined, Some(&vec!["joined".to_string()]));
    }
}
