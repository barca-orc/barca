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

/// Who shares a run, and who was kept out of one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sharing {
    /// The groups that should each be one run. Every target is in exactly one; groups, and
    /// the targets inside each, keep the order given.
    pub groups: Vec<Vec<String>>,
    /// The targets that have a step in common with others and still run alone, with why.
    pub left_out: Vec<LeftOut>,
}

/// A target kept out of a shared run because the run would hold it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub target: String,
    /// The targets it would have shared the run with.
    pub with: Vec<String>,
    /// A step the shared run plans ahead of the target, though the target does not need it.
    pub waits_for: String,
}

/// Split `targets` (node ids) into the groups that should each be one run. An id the DAG does
/// not know is a group of its own.
pub fn shared_run_groups(dag: &Dag, targets: &[String]) -> Sharing {
    let position: HashMap<&str, usize> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    let mut sharing = Sharing::default();
    let mut undecided = overlapping(dag, targets);
    while let Some(group) = undecided.pop() {
        match delayed_target(dag, &group) {
            // Sharing would hold this target back: it runs alone, and what is left is grouped
            // again, since it may have been the only thing connecting the others.
            Some((target, waits_for)) => {
                let rest: Vec<String> = group.into_iter().filter(|t| *t != target).collect();
                sharing.groups.push(vec![target.clone()]);
                undecided.extend(overlapping(dag, &rest));
                sharing.left_out.push(LeftOut {
                    target,
                    with: rest,
                    waits_for,
                });
            }
            None => sharing.groups.push(group),
        }
    }
    sharing
        .groups
        .sort_by_key(|group| position[group[0].as_str()]);
    sharing
        .left_out
        .sort_by_key(|out| position[out.target.as_str()]);
    sharing
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

/// The first target of `group` that one run over the whole group would hold back, with a
/// step it would wait for, if there is one.
fn delayed_target(dag: &Dag, group: &[String]) -> Option<(String, String)> {
    if group.len() < 2 {
        return None;
    }
    let ids: Vec<&str> = group.iter().map(String::as_str).collect();
    let config = ResourceConfig {
        pool_size: commands::default_pool_size(),
        concurrency_groups: HashMap::new(),
    };
    let plan = commands::plan_for_targets(dag, &ids, &config, commands::MIXED_COMMAND);
    group.iter().find_map(|target| {
        step_it_waits_for_without_needing(dag, &plan, target).map(|step| (target.clone(), step))
    })
}

/// A step outside `target`'s cone that `target` would wait for in `plan`, if there is one.
///
/// A phase starts only when the one before it has ended, so a target waits for every step of
/// the phases before the one its own step is in. Inside a phase it waits for nothing but its
/// own inputs: a run over independent targets gives every chain its own stream
/// (`planner::unpack_streams`) and leases a worker one node at a time. So this is the whole
/// condition, read off the plan: no estimate is involved.
fn step_it_waits_for_without_needing(
    dag: &Dag,
    plan: &ExecutionPlan,
    target: &str,
) -> Option<String> {
    let steps_of = |phase: &crate::planner::Phase| -> Vec<String> {
        phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .map(|st| st.step_id.base_id().to_string())
            .collect()
    };
    let own_phase = plan
        .phases
        .iter()
        .rposition(|phase| steps_of(phase).iter().any(|id| id == target))?;
    let cone: HashSet<&str> = dag.subgraph(target).into_iter().collect();
    plan.phases[..own_phase]
        .iter()
        .flat_map(steps_of)
        .find(|id| !cone.contains(id.as_str()))
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
            .groups
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
        let waits_for = |target| step_it_waits_for_without_needing(&dag, &plan, target);
        assert_eq!(waits_for("p.py:quick").as_deref(), Some("p.py:slow_root"));
        assert_eq!(waits_for("p.py:joined"), None);

        assert_eq!(groups(&dag, &["quick", "joined"]), [["quick"], ["joined"]]);
        // And the reason is reported, so the scheduler can say why.
        let targets = ["p.py:quick".to_string(), "p.py:joined".to_string()];
        assert_eq!(
            shared_run_groups(&dag, &targets).left_out,
            [LeftOut {
                target: "p.py:quick".to_string(),
                with: vec!["p.py:joined".to_string()],
                waits_for: "p.py:slow_root".to_string(),
            }]
        );
    }

    /// Two jobs behind one sensor. `tracked` reads only the sensor. `publish` also reads
    /// `model`, a root of its own, which the shared plan puts in the first phase next to the
    /// sensor: `tracked`, in the second phase, would wait for it.
    const SENSOR_AND_AN_EXTRA_ROOT: &str = r#"
from barca import asset, sensor, task


@sensor()
def version() -> tuple[bool, str]:
    return True, "v1"


@asset(inputs={"version": version})
def tracked(version: str) -> dict:
    return {}


@asset(inputs={"version": version})
def feed(version: str) -> dict:
    return {}


@asset()
def model() -> dict:
    return {}


@task(inputs={"feed": feed, "model": model})
def publish(feed: dict, model: dict) -> None:
    pass


@task(inputs={"feed": feed})
def announce(feed: dict) -> None:
    pass
"#;

    #[test]
    fn jobs_that_read_one_sensor_side_by_side_share_a_run() {
        let dag = dag(SENSOR_AND_AN_EXTRA_ROOT);
        // Both read the sensor directly: neither has anything planned ahead of it but the sensor.
        assert_eq!(groups(&dag, &["tracked", "feed"]), [["tracked", "feed"]]);
        // A job and the job downstream of it: everything ahead of `announce` is its own upstream.
        assert_eq!(groups(&dag, &["feed", "announce"]), [["feed", "announce"]]);
    }

    #[test]
    fn a_job_further_from_the_shared_step_does_not_share_with_a_nearer_one() {
        let dag = dag(SENSOR_AND_AN_EXTRA_ROOT);
        // `announce` is two steps below the sensor, `tracked` one. In one run `announce` would
        // be in the phase after `tracked` and wait for it, though it does not read it.
        let targets = ["p.py:tracked".to_string(), "p.py:announce".to_string()];
        let sharing = shared_run_groups(&dag, &targets);
        assert_eq!(
            sharing.groups,
            [
                vec!["p.py:tracked".to_string()],
                vec!["p.py:announce".to_string()]
            ]
        );
        assert_eq!(
            sharing.left_out,
            [LeftOut {
                target: "p.py:announce".to_string(),
                with: vec!["p.py:tracked".to_string()],
                waits_for: "p.py:tracked".to_string(),
            }]
        );
    }

    #[test]
    fn a_job_behind_a_shared_sensor_does_not_share_with_one_that_has_another_root() {
        let dag = dag(SENSOR_AND_AN_EXTRA_ROOT);
        let targets = ["p.py:tracked".to_string(), "p.py:publish".to_string()];
        let sharing = shared_run_groups(&dag, &targets);
        // Refused: the sensor is polled by each job's own run, as before runs were shared.
        assert_eq!(
            sharing.groups,
            [
                vec!["p.py:tracked".to_string()],
                vec!["p.py:publish".to_string()]
            ]
        );
        assert_eq!(
            sharing.left_out,
            [LeftOut {
                target: "p.py:tracked".to_string(),
                with: vec!["p.py:publish".to_string()],
                waits_for: "p.py:model".to_string(),
            }]
        );
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
