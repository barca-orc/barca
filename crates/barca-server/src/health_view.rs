//! Health assembly: what a status result is classified with, as this server holds it, and the
//! rows of `GET /state` built from it. Every read route that reports health reads it here, so
//! a node's health is the same word in all of them. CPU-heavy status reads use the blocking
//! pool so cheap routes remain responsive.

use crate::error::ApiError;
use crate::run_model::RunStatus;
use crate::schedule_view::{Schedules, published_ticks, recorded_queue, schedules};
use crate::state::AppState;
use crate::views::NodeState;
use barca_core::db;
use barca_core::health::{DURATION_WINDOW, Durations};
use std::time::Duration;

/// The body of `GET /state`, also warmed at startup.
pub(crate) async fn state_rows(state: &AppState) -> Result<Vec<NodeState>, ApiError> {
    state_rows_for_targets(state, &[]).await
}

pub(crate) async fn state_rows_for_targets(
    state: &AppState,
    targets: &[String],
) -> Result<Vec<NodeState>, ApiError> {
    let dag = state.reads.inspect_dag().await?;
    let db_state = state.reads.db_state();
    let snapshot = state.reads.snapshot_db().await?;
    // Read at most once per database state, whatever the scope (`read_cache`).
    let (status, durations) = if targets.is_empty() {
        // Together: after a change, each waits a moment for its new value (`read_cache`).
        let (status, durations) = tokio::join!(
            state.reads.whole_status(&dag, &db_state, &snapshot),
            state.reads.cached_durations(&dag, &db_state, {
                let (dag, snapshot) = (dag.clone(), snapshot.clone());
                move || async move {
                    let ids: Vec<String> =
                        dag.topo_order().into_iter().map(str::to_string).collect();
                    Ok(barca_core::health::durations(
                        db::recent_durations(&snapshot.path, &ids, DURATION_WINDOW).await?,
                    ))
                }
            })
        );
        (status?, durations?)
    } else {
        let status = state.reads.status_for_targets(&dag, &db_state, &snapshot, targets).await?;
        let ids = status
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let snapshot = snapshot.clone();
        let durations = crate::read_cache::off_the_workers(move || async move {
            Ok(barca_core::health::durations(
                db::recent_durations(&snapshot.path, &ids, DURATION_WINDOW).await?,
            ))
        })
        .await?;
        (status, std::sync::Arc::new(durations))
    };
    let facts = gather(
        state,
        &dag,
        &db_state,
        &snapshot.path,
        chrono::Utc::now(),
        durations,
    )
    .await?;
    let running = crate::running::by_node(&facts.runs, &status.nodes, &dag);
    Ok(rows(&status.nodes, &facts, running))
}

/// The run queue as the estimates read it ([`barca_core::health::Queue`]) as of `now` (unix
/// seconds): the `runs` in flight with where each is (its latest progress, or its estimated
/// start in the run queue: [`crate::queue_view::estimate`]) and the worker pool. A read-only
/// server holds no queue: it reads the one the serving server records. No read of its own
/// beyond the schedules' recorded ticks, which are kept per database state.
async fn queue_of(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    runs: &[crate::running::InFlight],
    now: i64,
    db_path: &str,
) -> Result<barca_core::health::Queue, ApiError> {
    if state.config.read_only {
        return Ok(barca_core::health::Queue::recorded(
            recorded_queue(state, dag, db_path, now).await?,
            None,
        ));
    }
    let facts = published_ticks(state).await.unwrap_or_default();
    let queue = crate::queue_view::estimate(state, &facts.ticks, now);
    let cfg = &state.config.resolved;
    let workers = barca_core::sizing(
        cfg.capacity,
        cfg.pool_size,
        state
            .runs.capacity
            .as_ref()
            .map(|c| (c.total() as usize, c.source())),
    )
    .pool_size;
    let runs = runs
        .iter()
        .map(|run| barca_core::estimates::PlannedRun {
            run_id: run.run_id.clone(),
            schedule: run.schedule_id.clone(),
            // A run's targets and everything above them; every node for a run of everything.
            nodes: (run.schedule_id.is_none() && !run.nodes.is_empty()).then(|| {
                let targets: Vec<&str> = run.nodes.iter().map(String::as_str).collect();
                dag.subgraph_many(&targets)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            }),
            running: !run.pending,
            start: if run.pending {
                queue.starts.get(&run.run_id).copied()
            } else {
                run.anchor()
            },
        })
        .collect();
    Ok(barca_core::health::Queue {
        runs,
        clear_at: queue.clear_at,
        open: queue.open,
        workers,
        max_runs: Some(state.runs.queue.max_runs),
    })
}

/// The `GET /state` rows: each node's status with its durations, its schedule, run and data
/// signals, classified with `facts`.
fn rows(
    nodes: &[barca_core::status::NodeStatus],
    facts: &HealthFacts,
    mut running: std::collections::HashMap<String, crate::views::NodeRunning>,
) -> Vec<NodeState> {
    // Health, signals and coverage are classified here, per answer: lateness moves with the
    // clock, so it is never kept with the status result. Late is the schedule's standing
    // (`barca_core::schedule_standing::standing`), the one `GET /schedule` reports.
    let health = facts.inputs().assemble();
    let mut nodes = nodes.to_vec();
    barca_core::health::classify(&mut nodes, health.evidence());
    nodes
        .into_iter()
        .map(|mut n| {
            let schedule = health.schedules.get(&n.id);
            let slot = schedule.map(|s| &s.standing);
            let age = facts.ages.get(&n.id).cloned().unwrap_or_default();
            n.schedule = schedule.map(|s| s.block.clone());
            NodeState {
                durations: facts.durations.get(&n.id).cloned(),
                next_run: slot.and_then(|s| s.next_fire),
                prev_run: slot.and_then(|s| s.prev_fire),
                late_since: slot.and_then(|s| s.late_since),
                running: running.remove(&n.id),
                status: n,
                last_success_at: age.last_success_at,
                failing_since: age.failing_since,
                consecutive_failures: age.consecutive_failures,
                last_run: age.last_run,
            }
        })
        .collect()
}

/// Health, data signals, data coverage and each scheduled node's `schedule` block of `result`
/// (`barca_core::health`), for every node in scope: called before a filter cuts `nodes`.
/// Lateness is each schedule's standing ([`schedules`]): read on the server's clock and in
/// its `--timezone`, never for a paused schedule, and the value `GET /schedule` and the
/// `schedule` block of `GET /state` report. Classified as of `now`.
pub(crate) async fn attach_health_at(
    state: &AppState,
    result: &mut barca_core::status::StatusResult,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<std::collections::HashSet<String>, ApiError> {
    let facts = health_facts(state, dag, db_state, db_path, now).await?;
    let health = facts.inputs().assemble();
    let running =
        crate::running::executing(&crate::running::by_node(&facts.runs, &result.nodes, dag));
    health.attach_schedules(&mut result.nodes);
    health.attach(result, &running);
    Ok(running)
}

/// What a status result is classified with (`barca_core::health::Inputs`), as this server
/// holds it: the records kept per database state, the graph's reach kept per graph, the
/// schedules, the run queue and the runs in flight read per answer.
pub(crate) struct HealthFacts {
    ages: std::sync::Arc<std::collections::HashMap<String, db::NodeAge>>,
    pinned: std::sync::Arc<std::collections::HashSet<String>>,
    checks: std::sync::Arc<barca_core::health::Checks>,
    paused: std::sync::Arc<std::collections::HashSet<String>>,
    paths: std::sync::Arc<barca_core::refresh_path::Paths>,
    schedules: Schedules,
    durations: std::sync::Arc<std::collections::HashMap<String, Durations>>,
    queue: barca_core::health::Queue,
    source_times: crate::read_cache::SourceTimes,
    pub(crate) runs: Vec<crate::running::InFlight>,
}

impl HealthFacts {
    /// The inputs of the one computation every host shares
    /// ([`barca_core::health::Inputs::assemble`]).
    pub(crate) fn inputs(&self) -> barca_core::health::Inputs<'_> {
        barca_core::health::Inputs {
            ages: &self.ages,
            pinned: &self.pinned,
            paused: &self.paused,
            checks: &self.checks,
            paths: &self.paths,
            schedules: self.schedules.inputs(),
            durations: &self.durations,
            queue: &self.queue,
            source_times: &self.source_times,
        }
    }
}

/// [`HealthFacts`] as of `now`, with every node's typical duration: kept per database state,
/// what `GET /state` reads.
pub(crate) async fn health_facts(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<HealthFacts, ApiError> {
    let durations = state
        .reads.cached_durations(dag, db_state, {
            let (dag, db_path) = (dag.clone(), db_path.to_string());
            move || async move {
                let ids: Vec<String> = dag.topo_order().into_iter().map(str::to_string).collect();
                Ok(barca_core::health::durations(
                    db::recent_durations(&db_path, &ids, DURATION_WINDOW).await?,
                ))
            }
        })
        .await?;
    gather(state, dag, db_state, db_path, now, durations).await
}

/// [`HealthFacts`] as of `now` over typical `durations` already read for the scope: each
/// record at most once per database state (`read_cache`), the schedules and the runs in
/// flight per answer.
async fn gather(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
    durations: std::sync::Arc<std::collections::HashMap<String, Durations>>,
) -> Result<HealthFacts, ApiError> {
    let ages = state.reads.cached_ages(db_state, db_path).await?;
    let pinned = state.reads.cached_pinned(db_state, db_path).await?;
    let checks = state.reads.cached_checks(dag, db_state, db_path, &ages).await?;
    let paused = state.reads.cached_paused_nodes(db_state, db_path).await?;
    let paths = state.reads.cached_paths(dag).await?;
    let runs = crate::running::in_flight(state, Some(db_path)).await;
    let schedules = schedules(state, dag, db_state, db_path, now).await?;
    let queue = queue_of(state, dag, &runs, now.timestamp(), db_path).await?;
    Ok(HealthFacts {
        ages,
        pinned,
        checks,
        paused,
        paths,
        schedules,
        durations,
        queue,
        source_times: state.reads.cached_source_times(dag),
        runs,
    })
}

/// How often [`keep_warm`] looks for a change.
const WARM_EVERY: Duration = Duration::from_secs(1);

/// [`keep_warm`] works while a client asked for `GET /state` within this long (the web UI asks
/// every 10 seconds while it is open).
const WARM_WHILE_POLLED: Duration = Duration::from_secs(60);

/// How long the metadata DB must have been still before [`keep_warm`] computes for it: a run
/// writes it many times a second, and its polls compute what they need.
const WARM_AFTER_STILL: Duration = Duration::from_secs(1);

/// `barca serve`'s background task: compute `GET /state` once the server is up (and its
/// database still), and again
/// whenever the sources or the database changed and have settled, while someone is polling and
/// no run of this server is going, so a poll finds it computed instead of waiting for it. With
/// nothing changed, a check is a few `stat` calls.
pub(crate) async fn keep_warm(state: AppState) {
    // The first time without waiting for a client, once start-up's own writes have settled.
    let mut started = false;
    loop {
        tokio::time::sleep(WARM_EVERY).await;
        if state.lifecycle.is_draining() || state.lifecycle.shutdown.is_cancelled() {
            return;
        }
        let busy = state.runs.registry.iter().any(|r| {
            matches!(
                r.status,
                RunStatus::Pending | RunStatus::Running
            )
        });
        if busy
            || (started && !state.reads.cache.polled_within(WARM_WHILE_POLLED))
            || ((started || !state.config.read_only)
                && !state
                    .reads
                    .db_state()
                    .still_for(WARM_AFTER_STILL))
        {
            continue;
        }
        started = true;
        let _ = state_rows(&state).await;
    }
}

