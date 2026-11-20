//! What is running now, whoever started it (#534): the `running` field of `GET /state` and
//! `GET /assets/{name}`, and `GET /runs?status=running`.
//!
//! Two sources. This server's runs in flight (`Runs::registry`: the scheduler's, `POST /runs`
//! and the older trigger routes, `barca get --server`), with their live step events. And the
//! CLI runs on the same `.barca/`: `running` rows of the metadata database whose owner is
//! alive ([`db::alive_running_runs_in`], the check the scheduler's overlap skip uses), read
//! from the snapshot the caller already took. A CLI run has no event stream here: its job's
//! unfinished targets are visible without a confirmed worker assignment.
//!
//! Worker-front leases identify the nodes executing now. Other nodes retain their own state.
//! Cached steps are announced at the end of the run. Until then, cache state and completion
//! events identify finished nodes and partition keys.

use crate::progress::ProgressSnapshot;
use crate::run_model::RunStatus;
use crate::state::AppState;
use crate::views::{NodeRunning, PartitionProgress, RunningStep};
use barca_service::db;
use barca_service::outcome::TriggeredBy;
use barca_service::status::NodeStatus;
use std::collections::{HashMap, HashSet};

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
    from_queue: bool,
    /// The resolved target node ids; empty for a run of every asset and sensor.
    pub nodes: Vec<String>,
    /// Waiting for a run slot.
    pub pending: bool,
    /// The run's events so far; `None` for a CLI run (no stream reaches this server).
    pub progress: Option<ProgressSnapshot>,
    /// The scheduled node whose tick started it; `None` for a run someone asked for.
    pub schedule_id: Option<String>,
    /// It computes again what is cached (`RunState::refreshes`).
    pub refreshes: bool,
}

impl InFlight {
    /// When it started executing, unix seconds: its `RunStarted` event; for a CLI run, the
    /// start its row records. `None` while it waits for a run slot.
    pub(crate) fn started(&self) -> Option<i64> {
        if self.pending {
            return None;
        }
        self.progress
            .as_ref()
            .and_then(|p| p.started)
            .map(|at| at as i64)
            .or_else(|| barca_service::time::parse_rfc3339(&self.started_at))
    }

    /// Its latest progress, unix seconds: when it started executing, or the last step it
    /// finished. What it has left is counted from here ([`barca_service::estimates`]).
    pub(crate) fn anchor(&self) -> Option<i64> {
        let last = self
            .progress
            .as_ref()
            .and_then(|p| p.last_step)
            .map(|at| at as i64);
        self.started()
            .map(|started| last.map_or(started, |at| at.max(started)))
    }
}

/// Every run in flight: this server's (pending or running), then the CLI runs in the
/// snapshot at `read_path` whose owner is alive and that this server does not hold.
pub(crate) async fn in_flight(state: &AppState, read_path: Option<&str>) -> Vec<InFlight> {
    let mut runs: Vec<InFlight> = state
        .runs
        .registry
        .iter()
        .filter(|r| matches!(r.status, RunStatus::Pending | RunStatus::Running))
        .map(|r| InFlight {
            run_id: r.handle.clone(),
            triggered_by: Some(r.trigger.triggered_by),
            started_at: barca_service::time::rfc3339_millis(r.started_at),
            started_sql: chrono::DateTime::from_timestamp(r.started_at as i64, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default(),
            command: r.command.clone(),
            from_queue: true,
            nodes: r.nodes.clone(),
            pending: r.status == RunStatus::Pending,
            progress: state.runs.events.get(&r.handle).map(|c| {
                let mut p = c.progress();
                p.executing = state.runs.shared_steps.executing(&r.handle);
                p
            }),
            schedule_id: r.trigger.schedule_id.clone(),
            refreshes: r.refreshes,
        })
        .collect();
    let Some(read_path) = read_path else {
        return runs;
    };
    // The server's own runs have rows too; they are the ones above (or finished).
    let mut held: HashSet<String> = state
        .runs
        .registry
        .iter()
        .map(|r| r.handle.clone())
        .collect();
    if state.config.read_only
        && let Ok(recorded) = barca_service::liveness::recorded_runs(read_path).await
    {
        let stopped = !recorded.is_empty()
            && barca_service::liveness::recorded(read_path)
                .await
                .is_ok_and(|rec| rec.writer_stopped(chrono::Utc::now().timestamp()));
        for r in recorded {
            let at = r.started_at.or(r.queued_at).unwrap_or_default();
            held.insert(r.run_id.clone());
            if stopped {
                continue;
            }
            runs.push(InFlight {
                run_id: r.run_id,
                triggered_by: r.triggered_by,
                command: r.command,
                from_queue: true,
                nodes: r.targets,
                pending: !r.started,
                progress: None,
                schedule_id: r.schedule,
                refreshes: r.refreshes,
                started_at: barca_service::time::rfc3339_secs(at).unwrap_or_default(),
                started_sql: chrono::DateTime::from_timestamp(at, 0)
                    .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default(),
            });
        }
    }
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
            started_at: barca_service::time::sql_datetime_rfc3339(&started_sql),
            started_sql,
            command: alive.command,
            from_queue: false,
            nodes,
            pending: false,
            progress: None,
            schedule_id: None,
            refreshes: false,
        });
    }
    runs
}

/// The nodes `run` works on: every asset and sensor for a run of everything, else the
/// targets and (for a server run, whose upstream steps report events) everything upstream of
/// them. A CLI run reports none of its steps, so only its targets count.
fn covered<'a>(
    run: &InFlight,
    nodes: &'a [NodeStatus],
    dag: &barca_service::dag::Dag,
) -> Vec<&'a str> {
    // A CLI run: the rule `barca history --status running` applies on its own machine.
    if !run.from_queue {
        return nodes
            .iter()
            .filter(|n| barca_service::run_list::unfollowed_run_covers(&run.command, &run.nodes, n))
            .map(|n| n.id.as_str())
            .collect();
    }
    if run.nodes.is_empty() {
        return nodes
            .iter()
            .filter(|n| run.command != "get" || n.kind != "task")
            .map(|n| n.id.as_str())
            .collect();
    }
    let targets: Vec<&str> = run.nodes.iter().map(String::as_str).collect();
    let wanted: HashSet<&str> = dag.subgraph_many(&targets).into_iter().collect();
    nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| wanted.contains(id))
        .collect()
}

/// The nodes a run is executing now: the only ones "N running" counts, anywhere.
pub(crate) fn executing(by_node: &HashMap<String, NodeRunning>) -> HashSet<String> {
    by_node
        .iter()
        .filter(|(_, r)| r.step == RunningStep::Running)
        .map(|(id, _)| id.clone())
        .collect()
}

/// How far along a step is, for choosing among several runs with one node.
fn rank(step: RunningStep) -> u8 {
    match step {
        RunningStep::Running => 0,
        RunningStep::Waiting => 1,
        RunningStep::Queued => 2,
    }
}

/// The `running` entry of every node a run in `runs` has in it. When several runs have one
/// node, a running step wins over a waiting one and that over a queued one, then the oldest
/// run.
pub(crate) fn by_node(
    runs: &[InFlight],
    nodes: &[NodeStatus],
    dag: &barca_service::dag::Dag,
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
            // Served from cache: nothing reports it until the run ends.
            if run.from_queue
                && !run.refreshes
                && !run.pending
                && n.cache.state == barca_service::cache_state::CacheState::Cached
                && progress.is_none_or(|p| !p.touched.contains(id))
            {
                return true;
            }
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
                None => barca_service::run_list::unfollowed_run_finished(&run.started_sql, n),
            }
        };
        for id in covered {
            if finished(id) && !progress.is_some_and(|p| p.executing.contains_key(id)) {
                continue;
            }
            let n = by_id[id];
            let step = match &progress {
                _ if run.pending => RunningStep::Queued,
                Some(p) if p.executing.contains_key(id) => RunningStep::Running,
                _ => RunningStep::Waiting,
            };
            let partitions = n.partitioned.then(|| {
                let (done, total) = keys(n);
                PartitionProgress {
                    running: if step == RunningStep::Running {
                        progress
                            .and_then(|p| p.executing.get(id))
                            .copied()
                            .unwrap_or(0)
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
                label: step.label().to_string(),
                partitions,
            };
            let better = match out.get(id) {
                None => true,
                Some(old) => {
                    rank(entry.step) < rank(old.step)
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
