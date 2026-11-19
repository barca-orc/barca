//! Inspection endpoint handlers. CPU-heavy status and history reads and snapshot migrations
//! use the blocking pool so cheap routes remain responsive. Runs are in [`crate::runs`].

use crate::error::ApiError;
use crate::state::{AppState, Durations, NodeState};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use barca_core::health::DURATION_WINDOW;
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
        "graph_stale": state.graph_stale(),
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
    let health = facts.inputs().assemble();
    // "N running" counts the nodes a run is executing, not every node of every run in flight.
    let running =
        crate::running::executing(&crate::running::by_node(&facts.runs, &result.nodes, &dag));
    health.attach(&mut result, &running);
    let mut rollup = health.rollup(&result, &dag.groups, &running, as_of, true);
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
    let (status, durations) = if targets.is_empty() {
        // Together: after a change, each waits a moment for its new value (`read_cache`).
        let (status, durations) = tokio::join!(
            whole_status(state, &dag, &db_state, &snapshot),
            state.cached_durations(&dag, &db_state, {
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
        let status = status_for_targets(state, &dag, &db_state, &snapshot, targets).await?;
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

/// What every schedule is read from ([`barca_core::refresh_path::ScheduleInputs`]), as this
/// server holds it as of `now`: the recorded ticks kept per database state, its pauses, and
/// its scheduler's own word for the jobs it fires. Without a scheduler here (a read-only
/// server, `--no-schedule`) the records say which schedules a scheduler fires, in the zone it
/// recorded.
pub(crate) struct Schedules {
    jobs: Vec<barca_core::schedule::ScheduledJob>,
    zone: barca_core::schedule::Zone,
    now: chrono::DateTime<chrono::Utc>,
    ticks: std::sync::Arc<crate::read_cache::ScheduleFacts>,
    pauses: std::collections::HashMap<String, db::SchedulePause>,
    firing: std::collections::HashMap<String, barca_core::refresh_path::Firing>,
    observed: barca_core::liveness::Observed,
}

impl Schedules {
    pub(crate) fn inputs(&self) -> barca_core::refresh_path::ScheduleInputs<'_> {
        barca_core::refresh_path::ScheduleInputs {
            jobs: &self.jobs,
            zone: &self.zone,
            now: self.now,
            ticks: &self.ticks.ticks,
            failures: &self.ticks.failures,
            pauses: &self.pauses,
            firing: &self.firing,
            observed: &self.observed,
        }
    }
}

/// The paused schedules this server holds ([`crate::pause::pauses`] reads them first).
fn held_pauses(state: &AppState) -> std::collections::HashMap<String, db::SchedulePause> {
    state
        .pauses
        .get()
        .and_then(|pauses| pauses.read().ok().map(|held| held.clone()))
        .unwrap_or_default()
}

/// The scheduler's published jobs as the schedule walk reads them.
fn firing_of(
    published: &[crate::state::JobStatus],
) -> std::collections::HashMap<String, barca_core::refresh_path::Firing> {
    published
        .iter()
        .map(|job| {
            (
                job.id.clone(),
                barca_core::refresh_path::Firing {
                    failures: job.failures,
                    max_failures: job.max_failures,
                    skip_reason: job.skip_reason.map(str::to_string),
                },
            )
        })
        .collect()
}

/// [`Schedules`] of every scheduled node of `dag` as of `now`, over the ticks the database in
/// state `db_state` records. `GET /state`, `GET /state/rollup` and `GET /nodes`
/// (`barca status`) all read this, and `GET /schedule` the same walk ([`schedule_at`]), so
/// they say the same of a schedule.
pub(crate) async fn schedules(
    state: &AppState,
    dag: &barca_core::dag::Dag,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Schedules, ApiError> {
    let observed_at = chrono::Utc::now();
    let jobs = barca_core::schedule::jobs_from_dag(dag);
    let mut zone = crate::scheduler::zone_of(&state.config);
    if jobs.is_empty() {
        return Ok(Schedules {
            jobs,
            zone,
            now,
            ticks: Default::default(),
            pauses: Default::default(),
            firing: Default::default(),
            observed: Default::default(),
        });
    }
    crate::pause::pauses(state).await?;
    let ids = jobs.iter().map(|j| j.id.clone()).collect();
    let ticks = state.cached_ticks(db_state, db_path, ids).await?;
    let published = state.schedule.read().map(|g| g.clone()).unwrap_or_default();
    let observed = if state.config.read_only || !state.config.schedule {
        let record = barca_core::liveness::recorded(db_path).await?;
        zone = record.zone().unwrap_or(zone);
        barca_core::liveness::observe(&jobs, &zone, observed_at, &record)
    } else {
        Default::default()
    };
    Ok(Schedules {
        jobs,
        zone,
        now,
        ticks,
        pauses: held_pauses(state),
        firing: firing_of(&published),
        observed,
    })
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

/// The run queue as the estimates read it ([`barca_core::health::Queue`]) as of `now` (unix
/// seconds): the `runs` in flight with where each is (its latest progress, or its estimated
/// start in the run queue: [`crate::queue::estimate`]) and the worker pool. A read-only
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
    Ok(barca_core::health::Queue {
        runs,
        clear_at: queue.clear_at,
        open: queue.open,
        workers,
        max_runs: Some(state.queue.max_runs),
    })
}

pub(crate) async fn recorded_queue(
    state: &AppState,
    dag: &barca_core::dag::Dag,
    path: &str,
    now: i64,
) -> Result<barca_core::liveness::RecordedTiming, ApiError> {
    let jobs = barca_core::schedule::jobs_from_dag(dag);
    let rec = barca_core::liveness::recorded(path).await?;
    let zone = rec
        .zone()
        .unwrap_or_else(|| crate::scheduler::zone_of(&state.config));
    let observed = barca_core::liveness::observe(
        &jobs,
        &zone,
        chrono::DateTime::from_timestamp(now, 0).unwrap(),
        &rec,
    );
    let facts = state
        .cached_ticks(
            &state.db_state(),
            path,
            jobs.iter().map(|j| j.id.clone()).collect(),
        )
        .await?;
    Ok(barca_core::health::recorded_queue(path, dag, &facts.ticks, &rec, &observed, now).await?)
}

/// The `GET /state` rows: each node's status with its durations, its schedule, run and data
/// signals, classified with `facts`.
fn rows(
    nodes: &[barca_core::status::NodeStatus],
    facts: &HealthFacts,
    mut running: std::collections::HashMap<String, crate::running::NodeRunning>,
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
                last_run: age.last_run.map(Into::into),
            }
        })
        .collect()
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
    if state.config.read_only || !state.config.schedule {
        let dag = state.loaded_dag().await?;
        let jobs = barca_core::schedule::jobs_from_dag(&dag);
        let snap = snapshot_db(&state).await?;
        let facts = state
            .cached_ticks(
                &state.db_state(),
                &snap.path,
                jobs.iter().map(|j| j.id.clone()).collect(),
            )
            .await?;
        // No scheduler here: failed ticks are the recorded ones, in the recorded zone.
        return Ok(Json(schedule_items(
            &state,
            chrono::Utc::now(),
            &facts,
            &[],
            jobs,
            barca_core::liveness::recorded(&snap.path)
                .await?
                .zone()
                .unwrap_or_else(|| crate::scheduler::zone_of(&state.config)),
        )));
    }
    let facts = published_ticks(&state).await?;
    Ok(Json(schedule_at(&state, chrono::Utc::now(), &facts)))
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
        .cached_durations(dag, db_state, {
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
    let ages = state.cached_ages(db_state, db_path).await?;
    let pinned = state.cached_pinned(db_state, db_path).await?;
    let checks = state.cached_checks(dag, db_state, db_path, &ages).await?;
    let paused = state.cached_paused_nodes(db_state, db_path).await?;
    let paths = state.cached_paths(dag).await?;
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
        source_times: state.cached_source_times(dag),
        runs,
    })
}

/// The body of `GET /schedule` as of `now`, over the recorded `ticks`
/// ([`published_ticks`]). Where each schedule stands (`next_fire`, `last_fired`,
/// `last_run_id`, `last_status`, `late`, `late_since`) is
/// [`barca_core::schedule_standing::standing`], in the zone the server evaluates cron in
/// (`--timezone`), so `next_fire` is the time the scheduler will fire the job.
pub(crate) fn schedule_at(
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
    ticks: &crate::read_cache::ScheduleFacts,
) -> Value {
    let published = state.schedule.read().map(|g| g.clone()).unwrap_or_default();
    // The jobs as the scheduler published them: it parsed each cron before publishing.
    let jobs = published
        .iter()
        .filter_map(|job| {
            Some(barca_core::schedule::ScheduledJob {
                id: job.id.clone(),
                kind: job.kind,
                cron_str: job.cron.clone(),
                cron: barca_core::CronExpr::parse(&job.cron).ok()?,
                followers: Vec::new(),
                max_failures: job.max_failures,
                timeout_seconds: None,
                batch: None,
            })
        })
        .collect();
    schedule_items(
        state,
        now,
        ticks,
        &published,
        jobs,
        crate::scheduler::zone_of(&state.config),
    )
}

/// One `GET /schedule` entry per job of `jobs`, in their order, from the walk every surface
/// reads ([`barca_core::refresh_path::schedule_views`]): the `schedule` block of `GET /state`
/// and a node's `refresh_path` carry the same state. `published`: the scheduler's own jobs
/// (failed ticks, the last skip); none where no scheduler runs here.
fn schedule_items(
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
    ticks: &crate::read_cache::ScheduleFacts,
    published: &[crate::state::JobStatus],
    jobs: Vec<barca_core::schedule::ScheduledJob>,
    zone: barca_core::schedule::Zone,
) -> Value {
    let pauses = held_pauses(state);
    let views =
        barca_core::refresh_path::schedule_views(barca_core::refresh_path::ScheduleInputs {
            jobs: &jobs,
            zone: &zone,
            now,
            ticks: &ticks.ticks,
            failures: &ticks.failures,
            pauses: &pauses,
            firing: &firing_of(published),
            // `live` is not served here.
            observed: &Default::default(),
        });
    let items: Vec<Value> = jobs
        .iter()
        .filter_map(|job| {
            let view = views.get(&job.id)?;
            let (standing, block) = (&view.standing, &view.block);
            let published = published.iter().find(|p| p.id == job.id);
            Some(json!({
                // The one word for where it stands, and the fact behind it.
                "state": block.state,
                "state_detail": block.state_detail,
                "id": job.id,
                "cron": job.cron_str,
                "kind": job.kind,
                "next_fire": standing.next_fire,
                "last_fired": standing.last_fired,
                "last_run_id": standing.last_run_id,
                // The same id under its older name.
                "last_run": standing.last_run_id,
                "last_status": standing.last_status,
                "late": standing.late,
                "late_since": standing.late_since,
                // How long its ticks' runs typically wait for a run slot and take, whole
                // seconds, and whether the wait exceeds its interval (#962).
                "typical_queue_wait_seconds": block.typical_queue_wait_seconds,
                "typical_run_seconds": block.typical_run_seconds,
                "queue_bound": standing.queue_bound,
                "last_skipped": published.and_then(|p| p.last_skipped),
                "skipped_for": published.and_then(|p| p.skipped_for.clone()),
                "skip_reason": published.and_then(|p| p.skip_reason),
                "paused": block.paused,
                "paused_until": pauses
                    .get(&job.id)
                    .filter(|_| block.paused)
                    .and_then(|p| p.until),
                "failures": block.failures,
                "max_failures": block.max_failures,
                "failing": block.failing,
            }))
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
}
