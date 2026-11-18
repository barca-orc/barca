//! Inspection endpoint handlers. CPU-heavy status and history reads and snapshot migrations
//! use the blocking pool so cheap routes remain responsive. Runs are in [`crate::runs`].

use crate::error::ApiError;
use crate::state::{AppState, Durations, NodeState};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use barca_core::queries;
use barca_core::results::PlanResult;
use barca_core::{BarcaError, db};
use serde_json::{Value, json};

/// `GET /health` — liveness (or `draining`), version, the API version a client checks, the
/// environment and artifact store this server uses, whether it is read-only, whether it runs
/// the scheduler (and whether it is still running), why the metadata DB cannot be used if it
/// cannot, and the cache size against its limit. No core work, and no wait on a read.
pub async fn health(State(state): State<AppState>) -> Json<Value> {
    let cfg = &state.config.resolved;
    Json(json!({
        // `draining` once the server was told to stop: it finishes its runs and starts none.
        "status": if state.is_draining() { "draining" } else { "ok" },
        // When this server started: a client that reconnects tells a restart by it.
        "started_at": state.started,
        // The run queue: paused or not, how many runs wait and how many execute.
        "queue": crate::queue::health(&state),
        "version": env!("CARGO_PKG_VERSION"),
        "api_version": barca_core::outcome::API_VERSION,
        "capabilities": { "event_resume": 1 },
        "env": cfg.env,
        // The artifact store this server writes to, so a client can refuse a mismatched
        // `[remote]`; null when artifacts stay on this machine.
        "store_uri": cfg.remote_artifacts().then(|| cfg.artifact_root.clone()),
        "read_only": state.config.read_only,
        "load_errors": *state.load_errors.read().unwrap(),
        // Whether this server fires `Schedule(...)` nodes: the same rule `serve`
        // uses to start the scheduler (on unless --no-schedule or --read-only).
        "scheduler": state.config.schedule && !state.config.read_only,
        // `scheduler`, and false once the scheduler stopped on an error.
        "scheduler_enabled": state.scheduler_enabled(),
        // Why the metadata DB cannot be used, while it cannot; null otherwise.
        "metadata_error": *state.metadata_error.read().unwrap(),
        // `.barca/` against its size limit, as last measured after a run or on the minute
        // checkpoint (RFC-0012); null before the first measurement or with the limit off.
        "cache": barca_core::gc::last_measured()
            .filter(|_| !state.config.read_only)
            .map(|(size, limit)| json!({"size_bytes": size, "limit_bytes": limit})),
    }))
}

/// A metadata DB a read-only request may query: the real one read in place
/// ([`db::ReadView`]) when this release uses it as it is; otherwise a private
/// snapshot of it, or — when there is no DB yet — an empty scratch DB, so the
/// absence is preserved. A copy has the schema ensured on it, never on the
/// original. Dropping it deletes the copy.
pub(crate) struct SnapshotDb {
    _view: Option<db::ReadView>,
    _snapshot: Option<db::DbSnapshot>,
    _scratch: Option<tempfile::TempDir>,
    pub(crate) path: String,
}

/// The shared snapshot of the metadata DB ([`AppState::read_snapshot`]): a copy taken when
/// the database last changed, shared by every request until it changes again.
pub(crate) async fn snapshot_db(state: &AppState) -> Result<std::sync::Arc<SnapshotDb>, ApiError> {
    Ok(state.note_metadata(state.read_snapshot().await)?)
}

/// Read a compatible WAL database in place, including on a `--read-only` server.
/// SQLite can create `-wal` and `-shm`; the reader never migrates or checkpoints the DB.
pub(crate) async fn snapshot_of(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    db::directory_check(db_path)?;
    if let Some(view) = db::ReadView::open(db_path).await? {
        return Ok(SnapshotDb {
            path: view.path().to_string(),
            _view: Some(view),
            _snapshot: None,
            _scratch: None,
        });
    }
    let db_path = db_path.to_owned();
    // Schema/index additions on the private copy can also take seconds on a large DB.
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(snapshot_copy(&db_path))
    })
    .await
    .map_err(|e| BarcaError::Db(format!("snapshot task failed: {e}")))?
}

async fn snapshot_copy(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    let (snapshot, scratch, path) = match db::DbSnapshot::take(db_path).await? {
        Some(s) => {
            let path = s.path().to_string();
            (Some(s), None, path)
        }
        None => {
            let dir = tempfile::tempdir()
                .map_err(|e| BarcaError::Db(format!("failed to create scratch dir: {e}")))?;
            let path = dir.path().join("metadata.db").display().to_string();
            (None, Some(dir), path)
        }
    };
    db::init_db_as(&path, db_path).await?;
    Ok(SnapshotDb {
        _view: None,
        _snapshot: snapshot,
        _scratch: scratch,
        path,
    })
}

/// Experimental target selection for `GET /state`.
#[derive(Default, serde::Deserialize)]
pub struct StateQuery {
    /// Experimental: comma-separated target names, full ids or group ids (`group:FILE:NAME`,
    /// every node in the group); absent/empty with no `files` means all nodes.
    pub targets: Option<String>,
    /// Experimental: comma-separated source files, as in node ids; adds every node they define.
    pub files: Option<String>,
    #[serde(default)]
    pub now: bool,
}

/// `GET /state` — every node's cache state (would `barca get` reuse it?), latest
/// attempt, typical durations, and next scheduled run. Read-only by
/// construction: the cache check reads the DB through read-only connections ([`SnapshotDb`]).
///
/// The cache states and durations are kept until the sources or the database change
/// ([`crate::read_cache`]); what runs now and the next fire times are read for every request.
pub async fn state(
    State(state): State<AppState>,
    Query(query): Query<StateQuery>,
) -> Result<Json<Vec<NodeState>>, ApiError> {
    state.reads.polled();
    let targets = scope_targets(&state, &query).await?;
    Ok(Json(state_rows_for_targets(&state, &targets).await?))
}

/// The targets a `GET /state` or `GET /state/rollup` query names, sorted: empty for every node.
async fn scope_targets(state: &AppState, query: &StateQuery) -> Result<Vec<String>, ApiError> {
    let split = |list: &Option<String>| -> Vec<String> {
        list.iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (targets, files) = (split(&query.targets), split(&query.files));
    // A group or a file stands for its nodes, so a client scopes a view of thousands of
    // nodes with one short query instead of a URL listing every id.
    let mut targets = if targets.iter().any(|t| t.starts_with("group:")) || !files.is_empty() {
        expand_targets(&*state.inspect_dag().await?, targets, &files)?
    } else {
        targets
    };
    targets.sort();
    targets.dedup();
    Ok(targets)
}

/// `GET /state/rollup` — the rows of `GET /state` summed into one tree (root, group, file,
/// node), each level the exact sum of its children, in nodes and in assets
/// ([`barca_core::rollup`]). It takes the scope of `GET /state` and sums the status result
/// that route's rows are built from ([`crate::read_cache`]): no status is computed for it
/// alone. Health is classified per answer, on the server's clock, exactly as `GET /state`
/// classifies it, so a node's health is the same word in both. `as_of` is the start of the
/// current minute (a schedule's lateness moves by the minute), so an unchanged poll within a
/// minute is a `304`.
pub async fn rollup(
    State(state): State<AppState>,
    Query(query): Query<StateQuery>,
) -> Result<Json<barca_core::rollup::Rollup>, ApiError> {
    state.reads.polled();
    let targets = scope_targets(&state, &query).await?;
    let dag = state.inspect_dag().await?;
    let db_state = state.db_state();
    let snapshot = snapshot_db(&state).await?;
    let status = status_for_targets(&state, &dag, &db_state, &snapshot, &targets).await?;
    let mut result = (*status).clone();
    let now = chrono::Utc::now();
    let as_of = if query.now {
        now
    } else {
        chrono::DateTime::from_timestamp(now.timestamp() / 60 * 60, 0).unwrap_or(now)
    };
    let facts = health_facts(&state, &dag, &db_state, &snapshot.path, as_of).await?;
    let running = crate::running::by_node(&facts.runs, &result.nodes, &dag)
        .into_keys()
        .collect();
    barca_core::health::attach(&mut result, facts.evidence(), &running);
    let mut rollup = barca_core::rollup::build(
        &result.nodes,
        &dag.groups,
        &running,
        as_of,
        facts.evidence(),
        true,
    );
    // Named by the targets alone, as `barca status --rollup` names it locally: a rollup of
    // whole files keeps the root's `all`.
    let scope: Vec<_> = query
        .targets
        .as_deref()
        .into_iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !scope.is_empty() {
        rollup.entries[0].label = scope.join(", ");
    }
    Ok(Json(rollup))
}

/// `targets` with each group id replaced by the nodes in it (nested groups included), and
/// every node of each of `files` added. An unknown group or file is `404`.
fn expand_targets(
    dag: &barca_core::dag::Dag,
    targets: Vec<String>,
    files: &[String],
) -> Result<Vec<String>, ApiError> {
    for target in &targets {
        if target.starts_with("group:") && !dag.groups.iter().any(|g| g.id == *target) {
            return Err(ApiError::NotFound(format!(
                "no group {target}\nGET /groups lists them."
            )));
        }
    }
    let mut out = barca_core::status::group_targets(dag, &targets)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    for file in files {
        let before = out.len();
        out.extend(
            dag.topo_order()
                .into_iter()
                .filter(|id| id.rsplit_once(':').is_some_and(|(f, _)| f == file))
                .map(str::to_owned),
        );
        if out.len() == before {
            return Err(ApiError::NotFound(format!(
                "no nodes in {file}\nGET /assets lists every node id, whose file is before its last `:`."
            )));
        }
    }
    Ok(out)
}

/// The body of `GET /state`, also warmed at startup.
pub(crate) async fn state_rows(state: &AppState) -> Result<Vec<NodeState>, ApiError> {
    state_rows_for_targets(state, &[]).await
}

async fn state_rows_for_targets(
    state: &AppState,
    targets: &[String],
) -> Result<Vec<NodeState>, ApiError> {
    let dag = state.inspect_dag().await?;
    let db_state = state.db_state();
    let snapshot = snapshot_db(state).await?;
    // Read at most once per database state, whatever the scope (`read_cache`).
    let ages = state.cached_ages(&db_state, &snapshot.path).await?;
    let pinned = state.cached_pinned(&db_state, &snapshot.path).await?;
    let checks = state
        .cached_checks(&dag, &db_state, &snapshot.path, &ages)
        .await?;
    let paused = state.cached_paused_nodes(&db_state, &snapshot.path).await?;
    let paths = state.cached_paths(&dag).await?;
    let (status, durations) = if targets.is_empty() {
        // Together: after a change, each waits a moment for its new value (`read_cache`).
        let (status, durations) = tokio::join!(
            whole_status(state, &dag, &db_state, &snapshot),
            state.cached_durations(&dag, &db_state, {
                let (dag, snapshot) = (dag.clone(), snapshot.clone());
                move || async move {
                    let ids: Vec<String> =
                        dag.topo_order().into_iter().map(str::to_string).collect();
                    Ok(durations_from(
                        db::recent_durations(&snapshot.path, &ids, DURATION_WINDOW).await?,
                    ))
                }
            })
        );
        (status?, durations?)
    } else {
        let status = status_for_targets(state, &dag, &db_state, &snapshot, targets).await?;
        let ids = status
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let snapshot = snapshot.clone();
        let durations = crate::read_cache::off_the_workers(move || async move {
            Ok(durations_from(
                db::recent_durations(&snapshot.path, &ids, DURATION_WINDOW).await?,
            ))
        })
        .await?;
        (status, std::sync::Arc::new(durations))
    };
    let runs = crate::running::in_flight(state, Some(&snapshot.path)).await;
    let running = crate::running::by_node(&runs, &status.nodes, &dag);
    let now = chrono::Utc::now();
    let schedules = live_schedules(state, &dag, &db_state, &snapshot.path, now).await?;
    let timings = timings(state, &dag, &durations, &runs, now.timestamp()).await;
    Ok(rows(
        &status.nodes,
        &durations,
        (&ages, &pinned, &checks, &paused, &paths),
        (&schedules, &timings),
        now.timestamp(),
        running,
    ))
}

/// `barca status` of every node without artifact shapes, against `snapshot` of the database in
/// state `db_state`: what `GET /state` and the `running` fields are built from, kept like every
/// status result.
pub(crate) async fn whole_status(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
) -> Result<std::sync::Arc<barca_core::status::StatusResult>, ApiError> {
    status_for_targets(state, dag, db_state, snapshot, &[]).await
}

async fn status_for_targets(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
    targets: &[String],
) -> Result<std::sync::Arc<barca_core::status::StatusResult>, ApiError> {
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let python = state.config.python.clone();
    let (graph, snapshot, names) = (dag.clone(), snapshot.clone(), targets.to_vec());
    Ok(state
        .cached_status(
            dag,
            db_state,
            (targets.to_vec(), 0, false),
            move || async move {
                let _snapshot = snapshot;
                barca_core::status::status_from_dag(&cfg, &names, &graph, &python, 0, false).await
            },
        )
        .await?)
}

/// Where one schedule stands, its `schedule` block, and what a refresh path through it says
/// (its next tick, paused, live), as this server reports them.
pub(crate) type LiveSchedule = (
    barca_core::schedule_standing::Standing,
    barca_core::status::ScheduleStatus,
    barca_core::refresh_path::PathSchedule,
);

/// Every scheduled node of `dag` as of `now`: where its schedule stands
/// ([`barca_core::schedule_standing::standing`], in the server's cron zone, over the ticks the
/// database in state `db_state` records) and its `schedule` block. `GET /state` and
/// `GET /nodes` (`barca status`) both read this, and `GET /schedule` the same standing
/// ([`schedule_at`]), so they say the same of a schedule. While the scheduler runs, its
/// failure count, the last skip and `live` are the scheduler's.
pub(crate) async fn live_schedules(
    state: &AppState,
    dag: &barca_core::dag::Dag,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<std::collections::HashMap<String, LiveSchedule>, ApiError> {
    let jobs = barca_core::schedule::jobs_from_dag(dag);
    if jobs.is_empty() {
        return Ok(Default::default());
    }
    crate::pause::pauses(state).await?;
    let ids = jobs.iter().map(|j| j.id.clone()).collect();
    let facts = state.cached_ticks(db_state, db_path, ids).await?;
    let zone = crate::scheduler::zone_of(&state.config);
    let published = state.schedule.read().map(|g| g.clone()).unwrap_or_default();
    Ok(jobs
        .iter()
        .map(|job| {
            let pause = crate::pause::active(state, &job.id, now.timestamp());
            let live = published.iter().find(|p| p.id == job.id);
            let failed = match live {
                Some(p) => (p.failures, p.max_failures),
                None => (
                    facts.failures.get(&job.id).map_or(0, |f| f.failures),
                    job.max_failures,
                ),
            };
            // Paused, or held after `max_failures` failed ticks: no tick is expected of it.
            let standing = barca_core::schedule_standing::standing(
                &job.cron,
                &zone,
                now,
                pause.is_some() || failed.0 >= failed.1,
                facts.ticks.get(&job.id),
            );
            let mut block = barca_core::status::ScheduleStatus::of(
                job,
                standing.clone(),
                failed,
                pause.as_ref(),
            );
            block.skip_reason = live.and_then(|p| p.skip_reason).map(str::to_string);
            block.live = live.is_some();
            // A refresh path through it: live when this server's scheduler fires it.
            let path = barca_core::refresh_path::PathSchedule::of(job, &zone, &standing, &block);
            (job.id.clone(), (standing, block, path))
        })
        .collect())
}

/// The recorded ticks of the schedules the scheduler has published, read once per database
/// state and shared with `GET /state` ([`AppState::cached_ticks`]).
pub(crate) async fn published_ticks(
    state: &AppState,
) -> Result<std::sync::Arc<crate::read_cache::ScheduleFacts>, ApiError> {
    let ids: Vec<String> = state
        .schedule
        .read()
        .map(|jobs| jobs.iter().map(|j| j.id.clone()).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let db_state = state.db_state();
    let snapshot = snapshot_db(state).await?;
    Ok(state.cached_ticks(&db_state, &snapshot.path, ids).await?)
}

/// What health reads of the metadata database, each kept per database state (`read_cache`):
/// every node's age, the pinned assets, what the newest ticks found and the paused nodes; and
/// of the graph, kept as long as it is: which schedules reach each node.
type Records<'a> = (
    &'a std::collections::HashMap<String, db::NodeAge>,
    &'a std::collections::HashSet<String>,
    &'a barca_core::health::Checks,
    &'a std::collections::HashSet<String>,
    &'a barca_core::refresh_path::Paths,
);

/// What the times of a status result are estimated from (`barca_core::estimates::Timing`), as
/// this server holds it: gathered once per answer, never per node.
pub(crate) struct Timings {
    durations: std::collections::HashMap<String, f64>,
    runs: Vec<barca_core::estimates::PlannedRun>,
    queue_clear_at: Option<i64>,
    queue_open: bool,
    workers: usize,
    source_times: crate::read_cache::SourceTimes,
    max_runs: usize,
}

impl Timings {
    fn evidence(&self) -> barca_core::estimates::Timing<'_> {
        barca_core::estimates::Timing {
            durations: &self.durations,
            runs: &self.runs,
            queue_clear_at: self.queue_clear_at,
            queue_open: self.queue_open,
            workers: self.workers,
            source_times: &self.source_times,
            max_runs: Some(self.max_runs),
        }
    }
}

/// [`Timings`] as of `now` (unix seconds): the typical `durations` already read for the
/// scope, the `runs` in flight with where each is (its latest progress, or its estimated
/// start in the run queue: [`crate::queue::estimate`]), the worker pool, and when each source
/// file changed. No read of its own beyond the schedules' recorded ticks, which are kept per
/// database state.
async fn timings(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    durations: &std::collections::HashMap<String, Durations>,
    runs: &[crate::running::InFlight],
    now: i64,
) -> Timings {
    let facts = published_ticks(state).await.unwrap_or_default();
    let queue = crate::queue::estimate(state, &facts.ticks, now);
    let cfg = &state.config.resolved;
    let workers = barca_core::sizing(
        cfg.capacity,
        cfg.pool_size,
        state
            .capacity
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
    Timings {
        durations: durations
            .iter()
            .map(|(id, d)| (id.clone(), d.median_seconds))
            .collect(),
        runs,
        queue_clear_at: queue.clear_at,
        queue_open: queue.open,
        workers,
        source_times: state.cached_source_times(dag),
        max_runs: state.queue.max_runs,
    }
}

/// Each schedule of [`live_schedules`] as a refresh path reads it.
fn path_schedules(
    schedules: &std::collections::HashMap<String, LiveSchedule>,
) -> barca_core::refresh_path::PathSchedules {
    schedules
        .iter()
        .map(|(id, (_, _, path))| (id.clone(), path.clone()))
        .collect()
}

/// The `GET /state` rows: each node's status with its durations, its schedule
/// ([`live_schedules`]), run and data signals, as of `now` (unix seconds).
fn rows(
    nodes: &[barca_core::status::NodeStatus],
    durations: &std::collections::HashMap<String, Durations>,
    records: Records<'_>,
    (schedules, timings): (&std::collections::HashMap<String, LiveSchedule>, &Timings),
    now: i64,
    mut running: std::collections::HashMap<String, crate::running::NodeRunning>,
) -> Vec<NodeState> {
    let (ages, pinned, checks, paused, paths) = records;
    let path_schedules = path_schedules(schedules);
    // Health, signals and coverage are classified here, per answer: lateness moves with the
    // clock, so it is never kept with the status result. Late is the schedule's standing
    // (`barca_core::schedule_standing::standing`), the one `GET /schedule` reports.
    let late = late_of(schedules);
    let mut nodes = nodes.to_vec();
    barca_core::health::classify(
        &mut nodes,
        barca_core::health::Evidence {
            ages,
            pinned,
            late: &late,
            checks,
            path: barca_core::refresh_path::PathEvidence {
                paths,
                schedules: &path_schedules,
                paused,
                pinned,
            },
            timing: timings.evidence(),
            now,
        },
    );
    nodes
        .into_iter()
        .map(|mut n| {
            let schedule = schedules.get(&n.id);
            let slot = schedule.map(|(standing, _, _)| standing);
            let age = ages.get(&n.id).cloned().unwrap_or_default();
            n.schedule = schedule.map(|(_, block, _)| block.clone());
            NodeState {
                durations: durations.get(&n.id).cloned(),
                next_run: slot.and_then(|s| s.next_fire),
                prev_run: slot.and_then(|s| s.prev_fire),
                late_since: slot.and_then(|s| s.late_since),
                running: running.remove(&n.id),
                status: n,
                last_success_at: age.last_success_at,
                failing_since: age.failing_since,
                consecutive_failures: age.consecutive_failures,
                last_run: age.last_run.map(Into::into),
            }
        })
        .collect()
}

/// Successful materializations considered for typical durations.
const DURATION_WINDOW: usize = 20;

/// Median and p95 of each node's recent successful runs ([`db::recent_durations`], at most
/// [`DURATION_WINDOW`] of them, partition keys folded into their node).
fn durations_from(
    recent: std::collections::HashMap<String, Vec<f64>>,
) -> std::collections::HashMap<String, Durations> {
    recent
        .into_iter()
        .filter_map(|(id, mut recent)| {
            if recent.is_empty() {
                return None;
            }
            recent.sort_by(f64::total_cmp);
            Some((
                id,
                Durations {
                    median_seconds: percentile(&recent, 0.5),
                    p95_seconds: percentile(&recent, 0.95),
                    samples: recent.len(),
                },
            ))
        })
        .collect()
}

/// Nearest-rank percentile over sorted values.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// `GET /plan` — execution plan for the server's files (cache-aware).
pub async fn plan(State(state): State<AppState>) -> Result<Json<PlanResult>, ApiError> {
    let dag = state.loaded_dag().await?;
    let cfg = &state.config.resolved;
    let sizing = barca_core::sizing(
        cfg.capacity,
        cfg.pool_size,
        state
            .capacity
            .as_ref()
            .map(|c| (c.total() as usize, c.source())),
    );
    Ok(Json(queries::plan_from_dag(&dag, sizing)))
}

/// `GET /assets` — every node as `barca list --json` shows it: `freshness` a lowercase
/// word, the cron expression in `schedule` and the next fire time (this machine's local
/// time) in `next_fire` for a scheduled node.
pub async fn assets(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let dag = state.loaded_dag().await?;
    let result = queries::list_assets_from_dag(&dag);
    let next_fires = next_fires(&dag);
    let nodes = result
        .iter()
        .map(|a| barca_core::report::list_node_json(a, next_fires.get(&a.id)))
        .collect();
    Ok(Json(nodes))
}

/// Each scheduled node's next fire time in this machine's local time, as `barca list` shows.
pub(crate) fn next_fires(dag: &barca_core::dag::Dag) -> std::collections::HashMap<String, String> {
    barca_core::schedule::describe_schedule_from_dag(Some(dag), &barca_core::schedule::Zone::Local)
        .into_iter()
        .filter_map(|j| j.next_fire_local.map(|t| (j.id, t)))
        .collect()
}

/// The most rows `GET /assets/{name}/schema?sample=N` returns per artifact.
const MAX_SCHEMA_SAMPLE: usize = 20;

#[derive(serde::Deserialize)]
pub struct SchemaQuery {
    /// First rows of each json/csv/parquet artifact, in `shape.sample`.
    sample: Option<usize>,
}

/// `GET /assets/{name}/schema` — inspect the selected node and its direct inputs.
/// Reads artifact shapes on demand rather than on every `/state` poll. Uses the
/// same safe inspector as `barca status`: parquet footers, JSON, pickle opcodes.
pub async fn asset_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<SchemaQuery>,
) -> Result<Json<Vec<barca_core::status::NodeStatus>>, ApiError> {
    let dag = state.inspect_dag().await?;
    let summaries = queries::list_assets_from_dag(&dag);
    let matches: Vec<_> = summaries
        .iter()
        .filter(|s| s.id == name || s.id.ends_with(&format!(":{name}")))
        .collect();
    let summary = match matches.len() {
        0 => return Err(ApiError::NotFound(format!("asset '{name}' not found"))),
        1 => matches[0],
        _ => return Err(ApiError::Conflict(format!("'{name}' is ambiguous"))),
    };
    let id = summary.id.clone();
    let inputs = &summary.inputs;
    let db_state = state.db_state();
    let snapshot = snapshot_db(&state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let mut nodes = whole_status(&state, &dag, &db_state, &snapshot)
        .await?
        .nodes
        .clone();
    nodes.retain(|n| n.id == id || inputs.contains(&n.id));
    barca_core::status::read_schemas(
        &state.config.python,
        &cfg,
        &mut nodes,
        query.sample.unwrap_or(0).min(MAX_SCHEMA_SAMPLE),
    )
    .await;
    Ok(Json(nodes))
}

/// `GET /assets/{name}` — one node as `barca list --json` shows it, with its
/// `barca stats --json` body.
pub async fn asset_detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.loaded_dag().await?;
    let dag = state.loaded_dag().await?;
    let summaries = queries::list_assets_from_dag(&dag);

    // Exact match first, then colon-prefixed match. No unbounded ends_with.
    let matches: Vec<_> = summaries
        .iter()
        .filter(|s| s.id == name || s.id.ends_with(&format!(":{name}")))
        .collect();

    let summary = match matches.len() {
        0 => return Err(ApiError::NotFound(format!("asset '{name}' not found"))),
        1 => matches[0].clone(),
        n => {
            let ids: Vec<_> = matches.iter().map(|s| s.id.as_str()).collect();
            return Err(ApiError::Conflict(format!(
                "'{name}' is ambiguous — matches {n} assets: {}",
                ids.join(", ")
            )));
        }
    };

    let db_state = state.db_state();
    let snap = snapshot_db(&state).await?;
    let stats = db::get_asset_stats(&snap.path, &summary.id).await?;
    let root = std::env::current_dir().unwrap_or_default();
    let failure = db::failure_facts(&snap.path, &summary.id, &root).await?;

    let next_fire = next_fires(&dag).remove(&summary.id);
    let running = running_node(&state, &db_state, &snap, &summary.id).await?;
    Ok(Json(json!({
        "asset": barca_core::report::list_node_json(&summary, next_fire.as_ref()),
        "stats": barca_core::report::stats_json(&stats),
        "running": running,
        "failure": failure,
    })))
}

/// The run working on node `id` now ([`crate::running`]), read from `snapshot` of the database
/// in state `db_state`. The cache state it needs is computed only while some run is in flight.
async fn running_node(
    state: &AppState,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<SnapshotDb>,
    id: &str,
) -> Result<Option<crate::running::NodeRunning>, ApiError> {
    let runs = crate::running::in_flight(state, Some(&snapshot.path)).await;
    if runs.is_empty() {
        return Ok(None);
    }
    let dag = state.inspect_dag().await?;
    let status = whole_status(state, &dag, db_state, snapshot).await?;
    Ok(crate::running::by_node(&runs, &status.nodes, &dag).remove(id))
}

/// `GET /schedule` — list scheduled jobs with next fire time and last run status.
/// Reads the scheduler's published registry; volatile fields (next fire, live
/// status) are computed per request.
pub async fn schedule(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    crate::pause::pauses(&state).await?;
    let facts = published_ticks(&state).await?;
    Ok(Json(schedule_at(&state, chrono::Utc::now(), &facts.ticks)))
}

/// Each scheduled node's `schedule` in `GET /nodes` (`barca status`): [`live_schedules`], the
/// block `GET /state` carries, so the numbers are the ones the web UI shows.
pub(crate) async fn attach_live_schedules(
    state: &AppState,
    nodes: &mut [barca_core::status::NodeStatus],
    dag: &barca_core::dag::Dag,
    db_path: &str,
) -> Result<(), ApiError> {
    let db_state = state.db_state();
    let mut schedules = live_schedules(state, dag, &db_state, db_path, chrono::Utc::now()).await?;
    for node in nodes.iter_mut() {
        if let Some((_, block, _)) = schedules.remove(&node.id) {
            node.schedule = Some(block);
        }
    }
    Ok(())
}

/// Health, data signals and data coverage of `result` (`barca_core::health`), for every node
/// in scope: called before a filter cuts `nodes`. Lateness is each schedule's standing
/// ([`live_schedules`]): read on the server's clock and in its `--timezone`, never for a
/// paused schedule, and the value `GET /schedule` and the `schedule` block report.
pub(crate) async fn attach_health(
    state: &AppState,
    result: &mut barca_core::status::StatusResult,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
) -> Result<std::collections::HashSet<String>, ApiError> {
    attach_health_at(state, result, dag, db_state, db_path, chrono::Utc::now()).await
}

/// [`attach_health`] as of `now`.
pub(crate) async fn attach_health_at(
    state: &AppState,
    result: &mut barca_core::status::StatusResult,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<std::collections::HashSet<String>, ApiError> {
    let facts = health_facts(state, dag, db_state, db_path, now).await?;
    let running = crate::running::by_node(&facts.runs, &result.nodes, dag)
        .into_keys()
        .collect();
    barca_core::health::attach(result, facts.evidence(), &running);
    Ok(running)
}

/// What a status result is classified with (`barca_core::health::Evidence`), as this server
/// holds it as of `now`: the records kept per database state, the graph's reach kept per
/// graph, each schedule's standing and the runs in flight read per answer.
pub(crate) struct HealthFacts {
    ages: std::sync::Arc<std::collections::HashMap<String, db::NodeAge>>,
    pinned: std::sync::Arc<std::collections::HashSet<String>>,
    checks: std::sync::Arc<barca_core::health::Checks>,
    paused: std::sync::Arc<std::collections::HashSet<String>>,
    paths: std::sync::Arc<barca_core::refresh_path::Paths>,
    late: std::collections::HashSet<String>,
    path_schedules: barca_core::refresh_path::PathSchedules,
    timings: Timings,
    pub(crate) runs: Vec<crate::running::InFlight>,
    now: i64,
}

impl HealthFacts {
    pub(crate) fn evidence(&self) -> barca_core::health::Evidence<'_> {
        barca_core::health::Evidence {
            ages: &self.ages,
            pinned: &self.pinned,
            late: &self.late,
            checks: &self.checks,
            path: barca_core::refresh_path::PathEvidence {
                paths: &self.paths,
                schedules: &self.path_schedules,
                paused: &self.paused,
                pinned: &self.pinned,
            },
            timing: self.timings.evidence(),
            now: self.now,
        }
    }
}

/// [`HealthFacts`] as of `now`. Lateness is each schedule's standing ([`live_schedules`]):
/// read on the server's clock and in its `--timezone`, never for a paused schedule, and the
/// value `GET /schedule` and the `schedule` block report.
pub(crate) async fn health_facts(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<HealthFacts, ApiError> {
    let ages = state.cached_ages(db_state, db_path).await?;
    let pinned = state.cached_pinned(db_state, db_path).await?;
    let checks = state.cached_checks(dag, db_state, db_path, &ages).await?;
    let paused = state.cached_paused_nodes(db_state, db_path).await?;
    let paths = state.cached_paths(dag).await?;
    let schedules = live_schedules(state, dag, db_state, db_path, now).await?;
    let late = late_of(&schedules);
    let path_schedules = path_schedules(&schedules);
    // The times read what `GET /state` reads: every node's typical duration, kept per
    // database state, and the runs in flight.
    let durations = state
        .cached_durations(dag, db_state, {
            let (dag, db_path) = (dag.clone(), db_path.to_string());
            move || async move {
                let ids: Vec<String> = dag.topo_order().into_iter().map(str::to_string).collect();
                Ok(durations_from(
                    db::recent_durations(&db_path, &ids, DURATION_WINDOW).await?,
                ))
            }
        })
        .await?;
    let runs = crate::running::in_flight(state, Some(db_path)).await;
    let timings = timings(state, dag, &durations, &runs, now.timestamp()).await;
    Ok(HealthFacts {
        ages,
        pinned,
        checks,
        paused,
        paths,
        late,
        path_schedules,
        timings,
        runs,
        now: now.timestamp(),
    })
}

/// The scheduled nodes whose schedule is late, of [`live_schedules`].
fn late_of(
    schedules: &std::collections::HashMap<String, LiveSchedule>,
) -> std::collections::HashSet<String> {
    schedules
        .iter()
        .filter(|(_, (standing, _, _))| standing.late)
        .map(|(id, _)| id.clone())
        .collect()
}

/// The body of `GET /schedule` as of `now`, over the recorded `ticks`
/// ([`published_ticks`]). Where each schedule stands (`next_fire`, `last_fired`,
/// `last_run_id`, `last_status`, `late`, `late_since`) is
/// [`barca_core::schedule_standing::standing`], in the zone the server evaluates cron in
/// (`--timezone`), so `next_fire` is the time the scheduler will fire the job.
pub(crate) fn schedule_at(
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
    ticks: &std::collections::HashMap<String, barca_core::schedule_standing::Ticks>,
) -> Value {
    use barca_core::CronExpr;

    let zone = crate::scheduler::zone_of(&state.config);
    let jobs = state.schedule.read().map(|g| g.clone()).unwrap_or_default();
    let items: Vec<Value> = jobs
        .into_iter()
        .map(|j| {
            let pause = crate::pause::active(state, &j.id, now.timestamp());
            let failed = (j.failures, j.max_failures);
            let standing = CronExpr::parse(&j.cron).ok().map(|cron| {
                barca_core::schedule_standing::standing(
                    &cron,
                    &zone,
                    now,
                    pause.is_some() || failed.0 >= failed.1,
                    ticks.get(&j.id),
                )
            });
            let standing = standing.as_ref();
            // The one word for where it stands, and the fact behind it: what the `schedule`
            // block of `GET /state` and a node's `refresh_path` carry too.
            let word = standing.map(|s| {
                barca_core::schedule_standing::ScheduleState::of(s, pause.is_some(), failed)
            });
            let detail = standing.zip(word).and_then(|(s, word)| {
                word.detail(
                    barca_core::targets::short_name(&j.id),
                    s,
                    failed,
                    j.kind == barca_core::NodeKind::Task,
                )
            });
            json!({
                "state": word,
                "state_detail": detail,
                "id": j.id,
                "cron": j.cron,
                "kind": j.kind,
                "next_fire": standing.and_then(|s| s.next_fire),
                "last_fired": standing.and_then(|s| s.last_fired),
                "last_run_id": standing.and_then(|s| s.last_run_id.clone()),
                // The same id under its older name.
                "last_run": standing.and_then(|s| s.last_run_id.clone()),
                "last_status": standing.and_then(|s| s.last_status.clone()),
                "late": standing.is_some_and(|s| s.late),
                "late_since": standing.and_then(|s| s.late_since),
                // How long its ticks' runs typically wait for a run slot and take, whole
                // seconds, and whether the wait exceeds its interval (#962).
                "typical_queue_wait_seconds": standing
                    .map_or(0, |s| s.typical_queue_wait_seconds.round() as u64),
                "typical_run_seconds": standing
                    .and_then(|s| s.typical_run_seconds)
                    .map(|s| s.round() as u64),
                "queue_bound": standing.is_some_and(|s| s.queue_bound),
                "last_skipped": j.last_skipped,
                "skipped_for": j.skipped_for,
                "skip_reason": j.skip_reason,
                "paused": pause.is_some(),
                "paused_until": pause.and_then(|p| p.until),
                "failures": j.failures,
                "max_failures": j.max_failures,
                "failing": j.failures >= j.max_failures,
            })
        })
        .collect();
    json!(items)
}

/// `GET /logs/{run_id}` — persisted stdout lines for a run (durable history).
pub async fn logs(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let snap = snapshot_db(&state).await?;
    let entries = db::get_logs(&snap.path, &run_id).await?;
    Ok(Json(json!({ "logs": entries })))
}

/// Any request no route matches: `404` with the error envelope every other error has,
/// naming what was asked for. A trigger with nothing after `/get/` or `/run/` is told what is
/// missing.
pub async fn no_route(method: axum::http::Method, uri: axum::http::Uri) -> ApiError {
    let path = uri.path();
    match (method.as_str(), path) {
        ("POST", "/get/" | "/get") => ApiError::NotFound(
            "POST /get/{target} needs a target: an asset or sensor name or its full id\nPOST /runs with {\"command\": \"get\", \"targets\": [...]} is the run route.".into(),
        ),
        ("POST", "/run/") => ApiError::NotFound(
            "POST /run/{target} needs a target: a task or sensor name or its full id \
             (POST /run, without the slash, gets every asset and sensor)\nPOST /runs with {\"command\": \"run\", \"targets\": [...]} is the run route."
                .into(),
        ),
        _ => ApiError::NotFound(format!(
            "no such endpoint: {method} {path}\nThe routes are listed at https://barca.sh/reference/server-api-endpoints/."
        )),
    }
}

/// The methods `path` takes, for a `405`.
fn allowed_methods(path: &str) -> &'static str {
    let runs_item = path
        .strip_prefix("/runs/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
    if path == "/runs" {
        "GET, POST"
    } else if runs_item {
        "GET, DELETE"
    } else if path == "/run"
        || path.starts_with("/get/")
        || (path.starts_with("/schedule/")
            && (path.ends_with("/pause") || path.ends_with("/resume")))
    {
        "POST"
    } else if path.starts_with("/run/") {
        "POST, DELETE"
    } else {
        "GET"
    }
}

/// A known path asked with a method it does not take: `405` with the error envelope and the
/// methods it does take, also in the `Allow` header.
pub async fn wrong_method(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    let path = uri.path();
    let allowed = allowed_methods(path);
    let error = ApiError::BadRequest(format!(
        "{method} is not allowed on {path}: use {}",
        allowed.replace(", ", " or ")
    ));
    let (_, envelope) = error.parts();
    (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, allowed)],
        Json(envelope.to_json()),
    )
        .into_response()
}

/// Organizational metadata; never part of the execution graph.
pub async fn groups(
    State(state): State<AppState>,
) -> Result<Json<Vec<barca_core::groups::NodeGroup>>, ApiError> {
    let dag = state.loaded_dag().await?;
    Ok(Json(dag.groups.clone()))
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    use crate::state::RunStatus;
    use std::time::Duration;

    #[test]
    fn a_queued_run_revalidates_source_before_any_worker_starts() {
        crate::test_support::bounded(|| async {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("p.py").display().to_string();
            std::fs::write(
                &file,
                "from barca import asset\n@asset()\ndef target(): return 1\n",
            )
            .unwrap();
            let mut cfg = barca_core::config::resolve_in(None, dir.path()).unwrap();
            cfg.db_path = dir.path().join("metadata.db").display().to_string();
            let mut state = AppState::new(crate::ServeConfig {
                files: vec![file.clone()],
                host: "127.0.0.1".parse().unwrap(),
                port: 0,
                watch: false,
                schedule: false,
                timezone: "utc".into(),
                // A stale graph would try spawning this nonexistent interpreter.
                python: "/nonexistent-barca-python".into(),
                resolved: cfg,
                read_only: false,
            });
            state.run_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
            assert!(
                crate::runs::check_target(&state, "target", "get")
                    .await
                    .is_ok()
            );
            // Every slot taken: the run waits in the queue.
            let held = state.run_slots.clone().try_acquire_owned().unwrap();
            let handle = crate::runs::start(
                state.clone(),
                crate::runs::RunSpec::api(
                    barca_core::outcome::RunCommand::Get,
                    vec!["target".into()],
                    vec![],
                ),
            )
            .await
            .unwrap();
            assert_eq!(state.runs.get(&handle).unwrap().status, RunStatus::Pending);
            std::fs::write(&file, "from barca import asset\ndef broken(: pass\n").unwrap();
            drop(held);
            crate::queue::dispatch(&state);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if state.runs.get(&handle).unwrap().status == RunStatus::Failed {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let run = state.runs.get(&handle).unwrap();
            assert!(
                run.error.as_ref().unwrap().contains("not found"),
                "{:?}",
                run.error
            );
            assert!(
                state.load_errors.read().unwrap()[0]
                    .error
                    .contains("syntax error")
            );
        });
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v = [1.0, 2.0, 3.0, 4.0, 100.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.95), 100.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }
}
