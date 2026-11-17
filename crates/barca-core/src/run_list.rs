//! A run as a list shows it (#959): the words of one row of `GET /runs` and of
//! `barca history --json` ([`RunLine`]), and what a run in progress is working on now
//! ([`RunNow`], with `GET /runs?status=running`). Beside [`crate::run_explanation`], which
//! explains one run in full: both are computed here so the web UI and the CLI only render
//! them (`specs/behavior/health.md`, "One computation").
//!
//! This reads what a run recorded and writes nothing.

use crate::groups::NodeGroup;
use crate::rollup::headline;
use crate::run_explanation::count;
use crate::targets::short_name;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// One run in the words of a list row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunLine {
    /// What went wrong, in one line: `stopped at orders · KeyError: 'region'`, or for a
    /// partitioned node `demand_forecast: 7 keys failed · BatchKeyFailed: …`. `null` for a
    /// run with nothing wrong to say.
    pub failure: Option<String>,
    /// The same in full: the run's own error, or `<node id>: <error>` of its first failed
    /// step. `null` with `failure`.
    pub failure_detail: Option<String>,
    /// The run's steps by outcome: `7 ok · 1 failed · 0 cached`, or `3 started · 0 cached` for
    /// a run that did not finish. Steps are keys: a partitioned node counts one per key.
    pub steps: String,
    /// What `steps` counts, with what was planned and where the run stopped:
    /// `Steps are keys: a partitioned node counts one per key · 12 nodes planned · stopped at
    /// billing.py:orders`.
    pub steps_detail: String,
}

/// What [`run_line`] reads of a run.
#[derive(Clone, Copy, Debug)]
pub struct RunLineFacts<'a> {
    pub status: &'a str,
    pub steps_total: Option<i64>,
    pub steps_executed: i64,
    pub steps_cached: i64,
    /// Recorded steps that failed, each key counted; `None` when not recorded.
    pub steps_failed: Option<i64>,
    /// The run's own error (a run a server holds that failed outside a step).
    pub error: Option<&'a str>,
    /// The first failed step: its node id (with its key) and its error.
    pub first_failure: Option<(&'a str, &'a str)>,
}

impl<'a> From<&'a crate::db::RunRecord> for RunLineFacts<'a> {
    fn from(r: &'a crate::db::RunRecord) -> Self {
        RunLineFacts {
            status: &r.status,
            steps_total: r.steps_total,
            steps_executed: r.steps_executed,
            steps_cached: r.steps_cached,
            steps_failed: r.steps_failed,
            error: None,
            first_failure: r
                .failure
                .as_ref()
                .map(|f| (f.node.as_str(), f.error.as_str())),
        }
    }
}

/// A run that ended and did not succeed: failed, errored, cancelled or interrupted.
fn not_successful(status: &str) -> bool {
    matches!(status, "failed" | "error" | "cancelled" | "interrupted")
}

/// A step's short name, with its partition key: `pipeline.py:a[day=1]` is `a[day=1]`.
fn step_name(node_id: &str) -> String {
    match node_id.find('[') {
        Some(key) => format!("{}{}", short_name(node_id), &node_id[key..]),
        None => short_name(node_id).to_string(),
    }
}

fn line_of(error: &str) -> String {
    headline(error).unwrap_or_default()
}

/// `7 ok · 1 failed · 0 cached`. `executed` counts every step that ran, failed ones included.
fn outcome(executed: u64, cached: u64, failed: u64) -> String {
    let failed = failed.min(executed);
    let mut parts = vec![format!("{} ok", count(executed - failed))];
    if failed > 0 {
        parts.push(format!("{} failed", count(failed)));
    }
    parts.push(format!("{} cached", count(cached)));
    parts.join(" · ")
}

/// The row's failure line and its full text.
fn failure(run: RunLineFacts<'_>) -> (Option<String>, Option<String>) {
    let (text, detail) = match (run.error, run.first_failure) {
        (Some(error), _) => (line_of(error), error.to_string()),
        _ if !not_successful(run.status) => return (None, None),
        (None, Some((node, error))) => (
            format!("{}: {}", step_name(node), line_of(error)),
            format!("{node}: {error}"),
        ),
        (None, None) if run.status == "failed" => {
            let none = "No failed step recorded: open the run".to_string();
            (none.clone(), none)
        }
        (None, None) => return (None, None),
    };
    let Some((node, step_error)) = run.first_failure else {
        return (Some(text), Some(detail));
    };
    let why = line_of(run.error.unwrap_or(step_error));
    let line = match node.find('[') {
        None => format!("stopped at {} · {why}", step_name(node)),
        Some(_) => {
            let failed = run.steps_failed.unwrap_or(1).max(1) as u64;
            format!(
                "{}: {} {} failed · {why}",
                short_name(node),
                count(failed),
                if failed == 1 { "key" } else { "keys" }
            )
        }
    };
    (Some(line), Some(detail))
}

/// The words of `run`'s row in a list of runs.
pub fn run_line(run: RunLineFacts<'_>) -> RunLine {
    let executed = run.steps_executed.max(0) as u64;
    let cached = run.steps_cached.max(0) as u64;
    let steps = if matches!(run.status, "cancelled" | "interrupted" | "error") {
        format!("{} started · {} cached", count(executed), count(cached))
    } else {
        outcome(
            executed,
            cached,
            run.steps_failed.unwrap_or(0).max(0) as u64,
        )
    };
    let mut detail = vec!["Steps are keys: a partitioned node counts one per key".to_string()];
    if let Some(total) = run.steps_total {
        let total = total.max(0) as u64;
        detail.push(format!(
            "{} {} planned",
            count(total),
            if total == 1 { "node" } else { "nodes" }
        ));
    }
    if let Some((node, _)) = run.first_failure {
        detail.push(format!("stopped at {node}"));
    }
    let (failure, failure_detail) = failure(run);
    RunLine {
        failure,
        failure_detail,
        steps,
        steps_detail: detail.join(" · "),
    }
}

/// A node a run in progress is working on, as [`run_now`] reads it.
#[derive(Clone, Copy, Debug)]
pub struct ActiveNodeFacts<'a> {
    pub node_id: &'a str,
    /// Its step is executing; otherwise it waits (for a run slot or for its upstream steps).
    pub running: bool,
    /// For a partitioned node: keys with a result, and every key.
    pub keys: Option<(usize, usize)>,
}

/// A node whose step is executing now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActiveNode {
    pub node_id: String,
    /// `3,120 of 5,000 keys`; `null` for an unpartitioned node.
    pub keys: Option<String>,
    /// 0 to 1 of its keys done; `null` for an unpartitioned node.
    pub share: Option<f64>,
}

/// Keys done of the keys of the partitioned nodes in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeysDone {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub done: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
}

/// The nodes in flight of one group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GroupActivity {
    /// The group's id (`group:<file>:<name>`).
    pub id: String,
    /// The group's name.
    pub group: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub running: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queued: usize,
}

/// What a run in progress is working on now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunNow {
    /// The nodes executing, the one furthest from done first: what holds the run up.
    pub running: Vec<ActiveNode>,
    /// How many nodes wait (for a run slot or for their upstream steps).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queued: usize,
    /// Keys with a result of the partitioned nodes in flight; `null` when none is partitioned.
    pub keys: Option<KeysDone>,
    /// The groups of the nodes in flight, busiest first; empty without groups. A node is
    /// counted under the group the rollup puts it in ([`crate::rollup::node_groups`]).
    pub groups: Vec<GroupActivity>,
}

/// What a run in progress is working on, from the nodes it has in flight.
pub fn run_now(active: &[ActiveNodeFacts<'_>], groups: &[NodeGroup]) -> RunNow {
    let mut done = 0;
    let mut total = 0;
    for node in active {
        if let Some((d, t)) = node.keys {
            done += d;
            total += t;
        }
    }
    let mut running: Vec<ActiveNode> = active
        .iter()
        .filter(|a| a.running)
        .map(|a| ActiveNode {
            node_id: a.node_id.to_string(),
            keys: a.keys.map(|(d, t)| {
                format!(
                    "{} of {} {}",
                    count(d as u64),
                    count(t as u64),
                    if t == 1 { "key" } else { "keys" }
                )
            }),
            share: a
                .keys
                .filter(|(_, t)| *t > 0)
                .map(|(d, t)| d as f64 / t as f64),
        })
        .collect();
    running.sort_by(|a, b| {
        a.share
            .unwrap_or(1.0)
            .total_cmp(&b.share.unwrap_or(1.0))
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    let group_of: HashMap<&str, &NodeGroup> = crate::rollup::node_groups(groups);
    let mut by_group: BTreeMap<&str, GroupActivity> = BTreeMap::new();
    for node in active {
        let Some(group) = group_of.get(node.node_id) else {
            continue;
        };
        let entry = by_group.entry(&group.id).or_insert_with(|| GroupActivity {
            id: group.id.clone(),
            group: group.name.clone(),
            running: 0,
            queued: 0,
        });
        if node.running {
            entry.running += 1;
        } else {
            entry.queued += 1;
        }
    }
    let mut groups: Vec<GroupActivity> = by_group.into_values().collect();
    groups.sort_by(|a, b| {
        (b.running + b.queued)
            .cmp(&(a.running + a.queued))
            .then_with(|| a.group.cmp(&b.group))
            .then_with(|| a.id.cmp(&b.id))
    });
    RunNow {
        running,
        queued: active.iter().filter(|a| !a.running).count(),
        keys: (total > 0).then_some(KeysDone { done, total }),
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(status: &'static str) -> RunLineFacts<'static> {
        RunLineFacts {
            status,
            steps_total: Some(12),
            steps_executed: 8,
            steps_cached: 3,
            steps_failed: Some(1),
            error: None,
            first_failure: None,
        }
    }

    #[test]
    fn a_successful_run_counts_its_steps_and_says_nothing_went_wrong() {
        let line = run_line(RunLineFacts {
            steps_failed: Some(0),
            ..facts("success")
        });
        assert_eq!(line.steps, "8 ok · 3 cached");
        assert_eq!(
            line.steps_detail,
            "Steps are keys: a partitioned node counts one per key · 12 nodes planned"
        );
        assert_eq!((line.failure, line.failure_detail), (None, None));
    }

    #[test]
    fn a_failed_run_says_where_it_stopped_and_why() {
        let line = run_line(RunLineFacts {
            first_failure: Some((
                "billing.py:orders",
                "KeyError: 'region'\n  File \"billing.py\"",
            )),
            ..facts("failed")
        });
        assert_eq!(line.steps, "7 ok · 1 failed · 3 cached");
        assert_eq!(
            line.failure.as_deref(),
            Some("stopped at orders · KeyError: 'region'")
        );
        assert_eq!(
            line.failure_detail.as_deref(),
            Some("billing.py:orders: KeyError: 'region'\n  File \"billing.py\"")
        );
        assert!(line.steps_detail.ends_with("stopped at billing.py:orders"));
    }

    #[test]
    fn a_partitioned_failure_counts_its_keys() {
        let line = run_line(RunLineFacts {
            steps_failed: Some(7),
            first_failure: Some(("m.py:forecast[day=1]", "BatchKeyFailed: 7 keys")),
            ..facts("failed")
        });
        assert_eq!(
            line.failure.as_deref(),
            Some("forecast: 7 keys failed · BatchKeyFailed: 7 keys")
        );
    }

    #[test]
    fn a_run_that_did_not_finish_says_what_it_started() {
        let line = run_line(facts("cancelled"));
        assert_eq!(line.steps, "8 started · 3 cached");
        assert_eq!(line.failure, None);
    }

    #[test]
    fn a_failed_run_with_no_failed_step_says_so_and_the_runs_own_error_leads() {
        let line = run_line(facts("failed"));
        assert_eq!(
            line.failure.as_deref(),
            Some("No failed step recorded: open the run")
        );
        let own = run_line(RunLineFacts {
            error: Some("worker pool exited\nmore"),
            ..facts("running")
        });
        assert_eq!(own.failure.as_deref(), Some("worker pool exited"));
        assert_eq!(
            own.failure_detail.as_deref(),
            Some("worker pool exited\nmore")
        );
    }

    fn group(id: &str, name: &str, members: &[&str]) -> NodeGroup {
        NodeGroup {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            members: members.iter().map(|m| m.to_string()).collect(),
            output: members.last().unwrap_or(&"").to_string(),
        }
    }

    #[test]
    fn nodes_in_flight_are_listed_furthest_from_done_first_and_counted_by_group() {
        let active = [
            ActiveNodeFacts {
                node_id: "p.py:a",
                running: true,
                keys: None,
            },
            ActiveNodeFacts {
                node_id: "p.py:b",
                running: true,
                keys: Some((3120, 5000)),
            },
            ActiveNodeFacts {
                node_id: "p.py:c",
                running: false,
                keys: Some((0, 1)),
            },
        ];
        // `p.py:b` is declared in two groups: it is counted once, under the first by id, as
        // the rollup places it.
        let groups = [
            group("group:p.py:ingest", "ingest", &["p.py:a", "p.py:b"]),
            group("group:p.py:model", "model", &["p.py:b", "p.py:c"]),
        ];
        let now = run_now(&active, &groups);
        assert_eq!(
            now.running
                .iter()
                .map(|n| (n.node_id.as_str(), n.keys.as_deref()))
                .collect::<Vec<_>>(),
            vec![("p.py:b", Some("3,120 of 5,000 keys")), ("p.py:a", None)]
        );
        assert_eq!(now.queued, 1);
        assert_eq!(
            now.keys,
            Some(KeysDone {
                done: 3120,
                total: 5001
            })
        );
        assert_eq!(
            now.groups
                .iter()
                .map(|g| (g.group.as_str(), g.running, g.queued))
                .collect::<Vec<_>>(),
            vec![("ingest", 2, 0), ("model", 0, 1)]
        );
    }
}
