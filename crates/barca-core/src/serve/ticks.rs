//! What the run of a fired tick did with each of the tick's targets, read from the run's own
//! step results at its end (#960). It records, it does not classify: a step that executed is
//! `rebuilt`, a cache hit `up_to_date`, a failed step `failed`, and a target the run has no
//! such result for `not_reached`. Health, causes and estimates are computed later, from the
//! record ([`crate::db::ticks_for_schedule`], [`crate::db::tick_outcomes_for_node`]).

use crate::db::TickTargetOutcome;
use crate::step_status::StepStatus;
use serde::Deserialize;
use std::collections::HashMap;

/// The two parts of a run's outcome (`runs.outcome_json`) that say what happened to a step;
/// everything else is skipped without being built.
#[derive(Deserialize, Default)]
struct Body {
    #[serde(default)]
    steps: Vec<Step>,
    #[serde(default)]
    targets: HashMap<String, Target>,
}

#[derive(Deserialize)]
struct Step {
    id: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct Target {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    failed_node: Option<String>,
}

/// The node of a step id: `pipeline.py:orders` of `pipeline.py:orders[day=1]`.
fn base(id: &str) -> &str {
    id.split('[').next().unwrap_or(id)
}

/// Each of `targets` (node ids, the tick's order kept) with what the run did with it, from the
/// run's outcome JSON. A run that reports no steps (cancelled in the queue, refused, timed out
/// before it planned) reached none of them.
///
/// A step reported for several partition keys counts once, by its worst key: failed, then
/// rebuilt, then up to date. A sensor that was polled executed, so it is `rebuilt` whether or
/// not its observation changed: the step results do not say which.
pub fn target_outcomes(targets: &[String], outcome_json: &str) -> Vec<(String, TickTargetOutcome)> {
    use TickTargetOutcome::*;
    let body: Body = serde_json::from_str(outcome_json).unwrap_or_default();
    let rank = |outcome: TickTargetOutcome| match outcome {
        Failed => 3,
        Rebuilt => 2,
        UpToDate => 1,
        NotReached => 0,
    };
    // Keys borrow from `body`.
    fn note<'a>(
        seen: &mut HashMap<&'a str, TickTargetOutcome>,
        node: &'a str,
        outcome: TickTargetOutcome,
        rank: impl Fn(TickTargetOutcome) -> u8,
    ) {
        let slot = seen.entry(node).or_insert(NotReached);
        if rank(outcome) > rank(*slot) {
            *slot = outcome;
        }
    }
    let mut seen: HashMap<&str, TickTargetOutcome> = HashMap::new();
    for step in &body.steps {
        let outcome = match step.status.as_deref().and_then(StepStatus::parse) {
            Some(StepStatus::Ran | StepStatus::Partial | StepStatus::Success) => Rebuilt,
            Some(StepStatus::Cached) => UpToDate,
            Some(StepStatus::Failed) => Failed,
            Some(StepStatus::Skipped | StepStatus::Blocked) | None => NotReached,
        };
        note(&mut seen, base(&step.id), outcome, rank);
    }
    // A multi-target run names each failed target's failing step.
    for target in body.targets.values() {
        if StepStatus::Failed.is(target.status.as_deref())
            && let Some(node) = &target.failed_node
        {
            note(&mut seen, base(node), Failed, rank);
        }
    }
    targets
        .iter()
        .map(|node| {
            let outcome = seen.get(node.as_str()).copied().unwrap_or(NotReached);
            (node.clone(), outcome)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TickTargetOutcome::*;

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| format!("pipeline.py:{n}")).collect()
    }

    fn outcomes(names: &[&str], json: &str) -> Vec<TickTargetOutcome> {
        target_outcomes(&ids(names), json)
            .into_iter()
            .map(|(_, outcome)| outcome)
            .collect()
    }

    #[test]
    fn each_step_status_is_one_outcome() {
        let json = r#"{"status":"failed","steps":[
            {"id":"pipeline.py:inbox","kind":"sensor","status":"ran"},
            {"id":"pipeline.py:orders","kind":"asset","status":"cached"},
            {"id":"pipeline.py:report","kind":"asset","status":"failed"},
            {"id":"pipeline.py:export","kind":"asset","status":"skipped","reason":"upstream_failed"},
            {"id":"pipeline.py:upstream_only","kind":"asset","status":"ran"}
        ]}"#;
        assert_eq!(
            outcomes(
                &["inbox", "orders", "report", "export", "never_planned"],
                json
            ),
            vec![Rebuilt, UpToDate, Failed, NotReached, NotReached],
            "a step that is not a target is not recorded, a target with no step is not reached"
        );
        let got = target_outcomes(&ids(&["orders", "inbox"]), json);
        assert_eq!(got[0].0, "pipeline.py:orders", "the tick's order is kept");
    }

    #[test]
    fn partition_keys_count_once_by_their_worst() {
        let json = r#"{"steps":[
            {"id":"pipeline.py:daily[day=1]","status":"cached"},
            {"id":"pipeline.py:daily[day=2]","status":"ran"},
            {"id":"pipeline.py:weekly[week=1]","status":"ran"},
            {"id":"pipeline.py:weekly[week=2]","status":"failed"},
            {"id":"pipeline.py:merged","status":"partial"}
        ]}"#;
        assert_eq!(
            outcomes(&["daily", "weekly", "merged"], json),
            vec![Rebuilt, Failed, Rebuilt]
        );
    }

    #[test]
    fn a_failed_target_names_its_failing_step() {
        let json = r#"{"status":"failed","steps":[{"id":"pipeline.py:a","status":"ran"}],
            "targets":{"a":{"status":"success"},"b":{"status":"failed","failed_node":"pipeline.py:b"}}}"#;
        assert_eq!(outcomes(&["a", "b"], json), vec![Rebuilt, Failed]);
    }

    #[test]
    fn a_run_without_steps_reached_nothing() {
        for json in [
            r#"{"status":"cancelled","envelope":{"error":"run cancelled"}}"#,
            "not json",
            "",
        ] {
            assert_eq!(outcomes(&["a", "b"], json), vec![NotReached, NotReached]);
        }
    }
}
