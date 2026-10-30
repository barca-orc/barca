//! What is running now, whoever started it (#534): the `running` field of `GET /state` and
//! `GET /assets/{name}`, and `GET /runs?status=running`.
//!
//! Two sources. This server's runs in flight (`AppState::runs`: the scheduler's, `POST /runs`
//! and the older trigger routes, `barca get --server`), with their live step events. And the
//! CLI runs on the same `.barca/`: `running` rows of the metadata database whose owner is
//! alive ([`db::alive_running_runs_in`], the check the scheduler's overlap skip uses), read
//! from the snapshot the caller already took. A CLI run has no event stream here: its job's
//! nodes count as running until each records a materialization during the run.
//!
//! The engine emits `step_finished` but no `step_started`, so which nodes of a server run are
//! executing is derived: a node is `running` once its function has printed a line or every
//! upstream node in the run has finished, and `queued` before that (and while the run waits
//! for a run slot). A partitioned node's keys come from the cache state the same request
//! reads (`done`: keys with a result, `total`: every key) and from the run's own step events.

use crate::state::{AppState, RunStatus};
use barca_core::outcome::TriggeredBy;
use barca_core::status::NodeStatus;
use barca_core::{RunEvent, StepId, db};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// A run working on a node now.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeRunning {
    /// The run (`GET /runs/{run_id}`).
    pub run_id: String,
    /// What started the run; `null` for a CLI run recorded by a barca that did not record it.
    pub triggered_by: Option<TriggeredBy>,
    /// When the run started (RFC 3339, UTC).
    pub started_at: String,
    pub step: RunningStep,
    /// For a partitioned node: how many of its keys are done.
    pub partitions: Option<PartitionProgress>,
}

/// Where a node is in a run.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RunningStep {
    /// The run will get to it: it waits for a run slot or for its upstream steps.
    Queued,
    /// Its step is executing (for a CLI run: the run is going and the node has not finished).
    Running,
}

/// The keys of a partitioned node, while a run works on it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionProgress {
    /// Keys not done yet while the node's step is running; 0 while it is queued.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub running: usize,
    /// Keys with a result: cached, or finished by this run.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub done: usize,
    /// Every key of the node.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
}

/// One run in flight.
#[derive(Clone, Debug)]
pub(crate) struct InFlight {
    pub run_id: String,
    pub triggered_by: Option<TriggeredBy>,
    /// RFC 3339, UTC, as `GET /runs` gives it.
    pub started_at: String,
    /// `YYYY-MM-DD HH:MM:SS` UTC, comparable with a materialization's `created_at`.
    started_sql: String,
    pub command: String,
    /// The resolved target node ids; empty for a run of every asset and sensor.
    pub nodes: Vec<String>,
    /// Waiting for a run slot.
    pub pending: bool,
    /// The run's events so far; `None` for a CLI run (no stream reaches this server).
    pub progress: Option<ProgressSnapshot>,
}

/// Every run in flight: this server's (pending or running), then the CLI runs in the
/// snapshot at `read_path` whose owner is alive and that this server does not hold.
pub(crate) async fn in_flight(state: &AppState, read_path: Option<&str>) -> Vec<InFlight> {
    let mut runs: Vec<InFlight> = state
        .runs
        .iter()
        .filter(|r| matches!(r.status, RunStatus::Pending | RunStatus::Running))
        .map(|r| InFlight {
            run_id: r.handle.clone(),
            triggered_by: Some(r.trigger.triggered_by),
            started_at: crate::runs::timestamp(r.started_at),
            started_sql: chrono::DateTime::from_timestamp(r.started_at as i64, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default(),
            command: r.command.clone(),
            nodes: r.nodes.clone(),
            pending: r.status == RunStatus::Pending,
            progress: state.events.get(&r.handle).map(|c| c.progress()),
        })
        .collect();
    let Some(read_path) = read_path else {
        return runs;
    };
    // The server's own runs have rows too; they are the ones above (or finished).
    let held: HashSet<String> = state.runs.iter().map(|r| r.handle.clone()).collect();
    for alive in db::alive_running_runs_in(read_path, &state.config.resolved.db_path).await {
        if held.contains(&alive.run_id) {
            continue;
        }
        // A row without a job was written by a barca that recorded it only at the end.
        let nodes = match alive.job.as_str() {
            "" => continue,
            "all" => Vec::new(),
            ids => ids.split(',').map(str::to_string).collect(),
        };
        let record = db::get_run(read_path, &alive.run_id).await.ok().flatten();
        let started_sql = record
            .as_ref()
            .map(|r| r.started_at.clone())
            .unwrap_or_default();
        runs.push(InFlight {
            run_id: alive.run_id,
            triggered_by: record.as_ref().and_then(|r| r.triggered_by),
            started_at: crate::runs::durable_timestamp(&started_sql),
            started_sql,
            command: alive.command,
            nodes,
            pending: false,
            progress: None,
        });
    }
    runs
}

/// What one run's events say about its steps.
#[derive(Default)]
pub(crate) struct Progress {
    /// Nodes that printed a line or finished a step (any key).
    touched: HashSet<String>,
    /// Unpartitioned nodes whose step finished.
    finished: HashSet<String>,
    /// Partitioned nodes: the keys this run finished with a result.
    keys_done: HashMap<String, HashSet<String>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProgressSnapshot {
    pub touched: HashSet<String>,
    pub finished: HashSet<String>,
    pub keys_done: HashMap<String, usize>,
}
impl Progress {
    pub(crate) fn observe(&mut self, event: &RunEvent) {
        match event {
            RunEvent::Log { node_id, .. } => {
                self.touched
                    .insert(StepId::parse(node_id).base_id().to_string());
            }
            RunEvent::StepFinished { node_id, ok, .. } => {
                let id = StepId::parse(node_id);
                // A `parallel()` child is not a step of the plan.
                if id.partition.0.contains_key("_branch") {
                    return;
                }
                let base = id.base_id().to_string();
                self.touched.insert(base.clone());
                if id.partition.0.is_empty() {
                    self.finished.insert(base);
                } else if *ok {
                    self.keys_done.entry(base).or_default().insert(id.display());
                }
            }
            _ => {}
        }
    }
    pub(crate) fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            touched: self.touched.clone(),
            finished: self.finished.clone(),
            keys_done: self
                .keys_done
                .iter()
                .map(|(node, keys)| (node.clone(), keys.len()))
                .collect(),
        }
    }
}

/// The nodes `run` works on: every asset and sensor for a run of everything, else the
/// targets and (for a server run, whose upstream steps report events) everything upstream of
/// them. A CLI run reports none of its steps, so only its targets count.
fn covered<'a>(
    run: &InFlight,
    nodes: &'a [NodeStatus],
    dag: &barca_core::dag::Dag,
) -> Vec<&'a str> {
    if run.nodes.is_empty() {
        return nodes
            .iter()
            .filter(|n| run.command != "get" || n.kind != "task")
            .map(|n| n.id.as_str())
            .collect();
    }
    let wanted: HashSet<&str> = if run.progress.is_some() {
        let targets: Vec<&str> = run.nodes.iter().map(String::as_str).collect();
        dag.subgraph_many(&targets).into_iter().collect()
    } else {
        run.nodes.iter().map(String::as_str).collect()
    };
    nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| wanted.contains(id))
        .collect()
}

/// The `running` entry of every node a run in `runs` is working on now. When several runs
/// work on one node, a running step wins over a queued one, then the oldest run.
pub(crate) fn by_node(
    runs: &[InFlight],
    nodes: &[NodeStatus],
    dag: &barca_core::dag::Dag,
) -> HashMap<String, NodeRunning> {
    let by_id: HashMap<&str, &NodeStatus> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut out: HashMap<String, NodeRunning> = HashMap::new();
    for run in runs {
        let covered = covered(run, nodes, dag);
        let in_run: HashSet<&str> = covered.iter().copied().collect();
        let progress = run.progress.as_ref();
        // Keys done of a partitioned node: its cached keys, or this run's own, whichever is
        // further along (the cache state is read from a snapshot that may lag the events).
        let keys = |n: &NodeStatus| {
            let total = n.partitions.as_ref().map_or(0, |p| p.total);
            let cached = n.partitions.as_ref().map_or(0, |p| p.cached);
            let mine = progress
                .as_ref()
                .and_then(|p| p.keys_done.get(&n.id))
                .copied()
                .unwrap_or(0);
            (cached.max(mine).min(total), total)
        };
        let finished = |id: &str| -> bool {
            let Some(n) = by_id.get(id) else {
                return true;
            };
            match &progress {
                // A partitioned step reports each key it runs, not the keys served from cache:
                // it is finished once its downstream steps have started, once this run has
                // run every key, or once every key has a result and this run ran some of them
                // (a node whose keys were all cached reports nothing, and counts as running
                // until the run gets past it).
                Some(p) if n.partitioned => {
                    let (done, total) = keys(n);
                    let mine = p.keys_done.get(id).copied().unwrap_or(0);
                    (total > 0 && (mine >= total || (mine > 0 && done >= total)))
                        || dag
                            .downstream(id)
                            .iter()
                            .any(|d| in_run.contains(d) && p.touched.contains(*d))
                }
                Some(p) => p.finished.contains(id),
                // A CLI run: finished once it recorded a materialization of the node.
                None => {
                    n.last_materialization.as_ref().is_some_and(|m| {
                        !run.started_sql.is_empty() && m.created_at >= run.started_sql
                    }) && (!n.partitioned || {
                        let (done, total) = keys(n);
                        done >= total
                    })
                }
            }
        };
        for id in covered {
            if finished(id) {
                continue;
            }
            let n = by_id[id];
            let step = match &progress {
                _ if run.pending => RunningStep::Queued,
                None => RunningStep::Running,
                Some(p) if p.touched.contains(id) => RunningStep::Running,
                Some(_) => {
                    let ready = n
                        .inputs
                        .iter()
                        .filter(|up| in_run.contains(up.as_str()))
                        .all(|up| finished(up));
                    if ready {
                        RunningStep::Running
                    } else {
                        RunningStep::Queued
                    }
                }
            };
            let partitions = n.partitioned.then(|| {
                let (done, total) = keys(n);
                PartitionProgress {
                    running: if step == RunningStep::Running {
                        total - done
                    } else {
                        0
                    },
                    done,
                    total,
                }
            });
            let entry = NodeRunning {
                run_id: run.run_id.clone(),
                triggered_by: run.triggered_by,
                started_at: run.started_at.clone(),
                step,
                partitions,
            };
            let better = match out.get(id) {
                None => true,
                Some(old) => {
                    (entry.step == RunningStep::Running && old.step == RunningStep::Queued)
                        || (entry.step == old.step && entry.started_at < old.started_at)
                }
            };
            if better {
                out.insert(id.to_string(), entry);
            }
        }
    }
    out
}
