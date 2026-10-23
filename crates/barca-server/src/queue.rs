//! The run queue. Every run the server accepts (`POST /runs`, the older trigger routes, the web
//! UI, `--server` clients, scheduled ticks) joins it, and is recorded in the metadata DB
//! (`run_queue`) before the server answers, under the run id it answers with. Runs start in the
//! order they were accepted, at most `--max-runs` at once (`[server] max_runs`, default one per
//! core), while the queue is neither paused nor draining.
//!
//! A run leaves the queue when it has recorded how it ended. A server that starts resumes the runs
//! it finds queued, in order ([`resume_at_startup`]). A run the stopping server stopped at its
//! drain deadline goes back to its place ([`crate::runs`], `requeue`) and starts again from the
//! beginning after the restart: steps that finished are cache hits then, so a run is executed
//! at least once, and a step that was interrupted runs again.
//!
//! `POST /queue/pause` holds every queued run back (runs in flight go on) until
//! `POST /queue/resume`; the pause is kept in `server_state`, so a restart keeps it.

use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::runs::RunSpec;
use crate::state::{AppState, RunChannel, RunState, RunStatus, now_ts};
use axum::Json;
use axum::extract::State;
use barca_core::cache::CachePolicy;
use barca_core::db;
use barca_core::outcome::{RunCommand, Trigger, TriggeredBy};
use barca_core::run_queue::{self, QueueState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

/// The queue of one server.
pub struct Queue {
    /// The runs waiting to start, in order.
    waiting: Mutex<VecDeque<Waiting>>,
    /// When an operator paused the queue (unix seconds), while it is paused.
    paused_at: Mutex<Option<i64>>,
    /// Held while a run is recorded and joins `waiting`, so both orders agree.
    admit: tokio::sync::Mutex<()>,
    /// How many runs execute at once.
    pub max_runs: usize,
    /// Set when the drain deadline passed: the runs it stops go back to the queue instead of
    /// ending `cancelled`.
    pub requeue: AtomicBool,
}

/// A run waiting to start.
pub struct Waiting {
    pub run_id: String,
    pub spec: RunSpec,
}

impl Queue {
    pub fn new(max_runs: usize) -> Self {
        Self {
            waiting: Mutex::new(VecDeque::new()),
            paused_at: Mutex::new(None),
            admit: tokio::sync::Mutex::new(()),
            max_runs: max_runs.max(1),
            requeue: AtomicBool::new(false),
        }
    }

    /// When the queue was paused, while it is.
    pub fn paused_at(&self) -> Option<i64> {
        *self.paused_at.lock().unwrap()
    }

    fn set_paused(&self, at: Option<i64>) {
        *self.paused_at.lock().unwrap() = at;
    }

    /// The ids of the runs waiting, in order.
    pub fn waiting_ids(&self) -> Vec<String> {
        self.waiting
            .lock()
            .unwrap()
            .iter()
            .map(|w| w.run_id.clone())
            .collect()
    }

    /// Take `run_id` off the queue, if it is still waiting.
    pub fn take(&self, run_id: &str) -> Option<Waiting> {
        let mut waiting = self.waiting.lock().unwrap();
        let at = waiting.iter().position(|w| w.run_id == run_id)?;
        waiting.remove(at)
    }

    /// The metadata DB was started over (`POST /state/reset`, refused while a run is queued):
    /// the pause it kept goes with it, as schedule pauses do.
    pub fn reset(&self) {
        self.set_paused(None);
    }
}

// ─── what is recorded ───────────────────────────────────────────────────────

/// A run as `run_queue.spec` keeps it: everything [`RunSpec`] holds.
#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    command: RunCommand,
    targets: Vec<String>,
    nodes: Vec<String>,
    files: Vec<String>,
    policy: StoredPolicy,
    trigger: Trigger,
    #[serde(default)]
    values: bool,
    #[serde(default)]
    partition: Option<BTreeMap<String, BTreeSet<String>>>,
    #[serde(default)]
    pin_manual: bool,
    #[serde(default)]
    tick: Option<String>,
    /// When the server accepted it (unix seconds).
    accepted_at: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredPolicy {
    CacheAware,
    RefreshAll,
    Refresh { names: Vec<String>, cascade: bool },
}

fn store(spec: &RunSpec, accepted_at: f64) -> String {
    let stored = Stored {
        command: spec.command,
        targets: spec.targets.clone(),
        nodes: spec.nodes.clone(),
        files: spec.files.clone(),
        policy: match &spec.policy {
            CachePolicy::CacheAware => StoredPolicy::CacheAware,
            CachePolicy::RefreshAll => StoredPolicy::RefreshAll,
            CachePolicy::RefreshSelective { names, cascade } => StoredPolicy::Refresh {
                names: names.clone(),
                cascade: *cascade,
            },
        },
        trigger: spec.trigger.clone(),
        values: spec.values,
        partition: spec.partition.as_ref().map(|p| p.0.clone()),
        pin_manual: spec.rules.pin_manual,
        tick: spec.rules.tick.clone(),
        accepted_at,
    };
    serde_json::to_string(&stored).expect("a run spec is JSON")
}

fn load(text: &str) -> Result<(RunSpec, f64), String> {
    let stored: Stored = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let spec = RunSpec {
        command: stored.command,
        targets: stored.targets,
        nodes: stored.nodes,
        files: stored.files,
        policy: match stored.policy {
            StoredPolicy::CacheAware => CachePolicy::CacheAware,
            StoredPolicy::RefreshAll => CachePolicy::RefreshAll,
            StoredPolicy::Refresh { names, cascade } => {
                CachePolicy::RefreshSelective { names, cascade }
            }
        },
        trigger: stored.trigger,
        values: stored.values,
        partition: stored
            .partition
            .map(barca_core::selection::PartitionSelection),
        rules: barca_core::freshness::RunRules {
            pin_manual: stored.pin_manual,
            tick: stored.tick,
        },
    };
    Ok((spec, stored.accepted_at))
}

// ─── admitting and starting runs ────────────────────────────────────────────

/// What [`admit`] answers: the run id, and whether the run started at once or waits (with its
/// place, 1 = next).
pub struct Admitted {
    pub run_id: String,
    pub status: RunStatus,
    pub position: Option<usize>,
}

/// The metadata DB exists and has barca's tables: the first run of a new project creates it.
async fn ensure_db(state: &AppState) -> Result<(), barca_core::BarcaError> {
    let cfg = &state.config.resolved;
    let path = std::path::Path::new(&cfg.db_path);
    if !path.exists() {
        if !path.parent().is_some_and(std::path::Path::exists) {
            db::ensure_env_dirs(&cfg.env)?;
        }
        db::init_db(&cfg.db_path).await?;
    }
    Ok(())
}

/// Accept a run: allocate its id, record it in the queue (the metadata DB) and in memory, and
/// start it when the queue lets it. A run that cannot be recorded is refused (`500`): it would
/// not survive a restart.
pub async fn admit(state: &AppState, spec: RunSpec) -> Result<Admitted, ApiError> {
    let run_id = db::generate_run_id();
    let accepted_at = now_ts();
    {
        let _admitting = state.queue.admit.lock().await;
        ensure_db(state).await?;
        run_queue::push(
            &state.config.resolved.db_path,
            &run_id,
            &store(&spec, accepted_at),
            &crate::runs::timestamp(accepted_at),
        )
        .await
        .map_err(|e| {
            ApiError::Barca(barca_core::BarcaError::Db(format!(
                "could not record the run in the run queue: {e}"
            )))
        })?;
        track(state, &run_id, &spec, accepted_at);
        state.queue.waiting.lock().unwrap().push_back(Waiting {
            run_id: run_id.clone(),
            spec,
        });
    }
    dispatch(state);
    let status = state
        .runs
        .get(&run_id)
        .map_or(RunStatus::Pending, |r| r.status);
    let position = state
        .queue
        .waiting_ids()
        .iter()
        .position(|id| *id == run_id)
        .map(|at| at + 1);
    Ok(Admitted {
        run_id,
        status,
        position,
    })
}

/// The live record and event channel of a run that joins the queue.
fn track(state: &AppState, run_id: &str, spec: &RunSpec, accepted_at: f64) {
    // Child of the server-wide shutdown token: DELETE /runs/{id} cancels just this run;
    // a forced stop cancels all of them.
    let cancel = state.shutdown.child_token();
    state.runs.insert(
        run_id.to_string(),
        RunState {
            handle: run_id.to_string(),
            db_run_id: None,
            command: spec.command.as_str().into(),
            target: (!spec.targets.is_empty()).then(|| spec.targets.join(",")),
            nodes: spec.nodes.clone(),
            trigger: spec.trigger.clone(),
            files: spec.files.clone(),
            status: RunStatus::Pending,
            result: None,
            error: None,
            started_at: accepted_at,
            finished_at: None,
            outcome: None,
            cancel,
            cancel_requested: false,
        },
    );
    state.events.insert(run_id.to_string(), RunChannel::new());
}

/// Start the runs at the head of the queue while there is room: not paused, not draining, fewer
/// than `max_runs` executing. Called when a run joins, when one ends and when the queue is
/// resumed.
pub fn dispatch(state: &AppState) {
    loop {
        if state.is_draining() || state.queue.paused_at().is_some() {
            return;
        }
        let Ok(permit) = state.run_slots.clone().try_acquire_owned() else {
            return;
        };
        let next = {
            let mut waiting = state.queue.waiting.lock().unwrap();
            let next = waiting.pop_front();
            if let Some(w) = &next
                && let Some(mut run) = state.runs.get_mut(&w.run_id)
            {
                run.status = RunStatus::Running;
            }
            next
        };
        let Some(next) = next else {
            return;
        };
        crate::runs::launch(state.clone(), next, permit);
    }
}

/// At startup, before anything is admitted: read the queue the last server left. Queued runs
/// join the queue in their order, under their run ids; a run that was started and never
/// recorded how it ended (the server was killed) is not started again: it is recorded as
/// interrupted. Then read whether the queue is paused. Prints what it resumes.
pub async fn resume_at_startup(state: &AppState) -> Result<(), barca_core::BarcaError> {
    let path = state.config.resolved.db_path.clone();
    if !std::path::Path::new(&path).exists() {
        return Ok(());
    }
    let rows = run_queue::list(&path).await?;
    let paused = run_queue::paused_at(&path).await?;
    state.queue.set_paused(paused);
    let mut resumed = 0;
    for row in rows {
        match (row.state, load(&row.spec)) {
            (QueueState::Queued, Ok((spec, accepted_at))) => {
                track(state, &row.run_id, &spec, accepted_at);
                state.queue.waiting.lock().unwrap().push_back(Waiting {
                    run_id: row.run_id,
                    spec,
                });
                resumed += 1;
            }
            (QueueState::Running, loaded) => {
                interrupted(state, &row.run_id, loaded.ok().map(|(s, _)| s)).await;
            }
            (QueueState::Queued, Err(why)) => {
                barca_core::errln!(
                    "[barca] warning: queued run {} cannot be read ({why}); it is not resumed",
                    row.run_id
                );
                run_queue::remove(&path, &row.run_id).await?;
            }
        }
    }
    if resumed > 0 {
        barca_core::errln!(
            "[barca] resuming {resumed} queued run{}",
            if resumed == 1 { "" } else { "s" }
        );
    }
    if let Some(at) = paused {
        barca_core::errln!(
            "[barca] the run queue is paused (since {}): queued runs wait for POST /queue/resume \
             (barca queue resume)",
            barca_core::outcome::rfc3339(at)
        );
    }
    Ok(())
}

/// A run the last server started and never finished recording: it was killed (SIGKILL, a
/// crash, a supervisor's grace period that ran out). It is not started again; its row, when it
/// wrote one, is recorded `interrupted` by the usual recovery, and one that never wrote a row
/// gets one with an `error` outcome, so its id is not lost.
async fn interrupted(state: &AppState, run_id: &str, spec: Option<RunSpec>) {
    let cfg = &state.config.resolved;
    barca_core::errln!(
        "[barca] warning: run {run_id} was interrupted when the server last stopped (it was \
         killed before it finished); it is not started again"
    );
    if let Ok(None) = db::get_run(&cfg.db_path, run_id).await
        && let Some(spec) = spec
    {
        let outcome = barca_core::outcome::Outcome::ended(
            barca_core::outcome::OutcomeStatus::Error,
            &barca_core::envelope::ErrorEnvelope::from_prose(
                barca_core::envelope::ErrorKind::Infra,
                "run interrupted: the server was killed before the run recorded itself\nRun it \
                 again; give the server a longer stop grace period than its drain timeout.",
            ),
        );
        let target = (!spec.targets.is_empty()).then(|| spec.targets.join(","));
        let files = if spec.files.is_empty() {
            &state.config.files
        } else {
            &spec.files
        };
        let _ = db::record_unstarted_run(
            &cfg.db_path,
            db::NewRun {
                run_id,
                command: spec.command.as_str(),
                files: &db::encode_files(files),
                target: target.as_deref(),
                steps_total: None,
                job: None,
                trigger: &spec.trigger,
            },
            "error",
            outcome.text(),
        )
        .await;
    }
    if let Err(e) = run_queue::remove(&cfg.db_path, run_id).await {
        barca_core::errln!("[barca] warning: {e}");
    }
}

// ─── the routes ─────────────────────────────────────────────────────────────

/// One run of `GET /queue`.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct QueueEntry {
    pub run_id: String,
    /// `pending` (waiting to start) or `running`.
    pub status: String,
    /// Its place among the waiting runs, 1 = next; `null` for a running run.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub position: Option<usize>,
    pub command: String,
    pub target: Option<String>,
    pub triggered_by: TriggeredBy,
    pub schedule_id: Option<String>,
    /// When the server accepted it.
    pub queued_at: String,
}

/// `GET /queue` (and the answer of pause and resume).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct QueueView {
    /// Paused by an operator: nothing new starts until it is resumed.
    pub paused: bool,
    /// When it was paused, RFC 3339; `null` when not paused.
    pub paused_at: Option<String>,
    /// The server is stopping: nothing new starts; the queue waits for the next start.
    pub draining: bool,
    /// How many runs execute at once.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub max_runs: usize,
    /// The runs executing now, oldest first.
    pub running: Vec<QueueEntry>,
    /// The runs waiting to start, next first.
    pub queued: Vec<QueueEntry>,
}

fn entry(run: &RunState, position: Option<usize>) -> QueueEntry {
    QueueEntry {
        run_id: run.handle.clone(),
        status: if run.status == RunStatus::Running {
            "running"
        } else {
            "pending"
        }
        .into(),
        position,
        command: run.command.clone(),
        target: run.target.clone(),
        triggered_by: run.trigger.triggered_by,
        schedule_id: run.trigger.schedule_id.clone(),
        queued_at: crate::runs::timestamp(run.started_at),
    }
}

/// The queue as this server holds it.
pub fn view(state: &AppState) -> QueueView {
    let queued = state
        .queue
        .waiting_ids()
        .iter()
        .enumerate()
        .filter_map(|(at, id)| state.runs.get(id).map(|r| entry(&r, Some(at + 1))))
        .collect();
    let mut running: Vec<QueueEntry> = state
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Running)
        .map(|r| entry(&r, None))
        .collect();
    running.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));
    let paused_at = state.queue.paused_at();
    QueueView {
        paused: paused_at.is_some(),
        paused_at: paused_at.map(barca_core::outcome::rfc3339),
        draining: state.is_draining(),
        max_runs: state.queue.max_runs,
        running,
        queued,
    }
}

/// A `--read-only` server runs nothing: its view is what the metadata DB records, read from a
/// snapshot (the queue of the server that writes it, as of its last write).
async fn recorded_view(state: &AppState) -> Result<QueueView, ApiError> {
    let snapshot = snapshot_db(state).await?;
    let rows = run_queue::list(&snapshot.path).await?;
    let paused_at = run_queue::paused_at(&snapshot.path).await?;
    let (mut running, mut queued) = (Vec::new(), Vec::new());
    for row in rows {
        let Ok((spec, accepted_at)) = load(&row.spec) else {
            continue;
        };
        let started = row.state == QueueState::Running;
        let item = QueueEntry {
            run_id: row.run_id,
            status: if started { "running" } else { "pending" }.into(),
            position: (!started).then_some(queued.len() + 1),
            command: spec.command.as_str().into(),
            target: (!spec.targets.is_empty()).then(|| spec.targets.join(",")),
            triggered_by: spec.trigger.triggered_by,
            schedule_id: spec.trigger.schedule_id.clone(),
            queued_at: crate::runs::timestamp(accepted_at),
        };
        if started {
            running.push(item);
        } else {
            queued.push(item);
        }
    }
    Ok(QueueView {
        paused: paused_at.is_some(),
        paused_at: paused_at.map(barca_core::outcome::rfc3339),
        draining: state.is_draining(),
        max_runs: state.queue.max_runs,
        running,
        queued,
    })
}

/// `GET /queue` — the runs executing and the runs waiting, in order, and whether the queue is
/// paused.
pub async fn get(State(state): State<AppState>) -> Result<Json<QueueView>, ApiError> {
    if state.config.read_only {
        return recorded_view(&state).await.map(Json);
    }
    Ok(Json(view(&state)))
}

/// `POST /queue/pause` — hold every queued run back until `POST /queue/resume`; runs in flight
/// go on, and new runs (and scheduled ticks) still join the queue. Kept across restarts.
/// Pausing a paused queue changes nothing. `403` on a `--read-only` server.
pub async fn pause(State(state): State<AppState>) -> Result<Json<QueueView>, ApiError> {
    crate::runs::refuse_if_read_only(&state)?;
    if state.queue.paused_at().is_none() {
        let now = chrono::Utc::now().timestamp();
        ensure_db(&state).await?;
        run_queue::set_paused(&state.config.resolved.db_path, Some(now)).await?;
        state.queue.set_paused(Some(now));
        barca_core::errln!(
            "[barca] run queue paused: queued runs wait until it is resumed ({} queued, {} \
             running)",
            state.queue.waiting_ids().len(),
            state
                .runs
                .iter()
                .filter(|r| r.status == RunStatus::Running)
                .count()
        );
    }
    Ok(Json(view(&state)))
}

/// `POST /queue/resume` — let queued runs start again, in order. Resuming a queue that is not
/// paused changes nothing. `403` on a `--read-only` server.
pub async fn resume(State(state): State<AppState>) -> Result<Json<QueueView>, ApiError> {
    crate::runs::refuse_if_read_only(&state)?;
    if state.queue.paused_at().is_some() {
        ensure_db(&state).await?;
        run_queue::set_paused(&state.config.resolved.db_path, None).await?;
        state.queue.set_paused(None);
        barca_core::errln!(
            "[barca] run queue resumed ({} queued)",
            state.queue.waiting_ids().len()
        );
        dispatch(&state);
    }
    Ok(Json(view(&state)))
}

/// `queue` of `GET /health`: whether the queue is paused and how many runs wait and execute.
pub fn health(state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "paused": state.queue.paused_at().is_some(),
        "queued": state.queue.waiting_ids().len(),
        "running": state.runs.iter().filter(|r| r.status == RunStatus::Running).count(),
        "max_runs": state.queue.max_runs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &std::path::Path) -> AppState {
        let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
        resolved.db_path = dir.join("metadata.db").display().to_string();
        resolved.server_max_runs = Some(1);
        AppState::new(crate::ServeConfig {
            files: vec![dir.join("p.py").display().to_string()],
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: "/nonexistent-barca-python".into(),
            resolved,
            read_only: false,
        })
    }

    #[tokio::test]
    async fn a_restarted_server_resumes_the_queue_in_order_under_the_same_ids_and_keeps_the_pause()
    {
        let dir = tempfile::tempdir().unwrap();
        let first = state(dir.path());
        // Paused: every run waits, recorded in the order it was accepted.
        let _ = pause(State(first.clone())).await.unwrap();
        let mut ids = Vec::new();
        for target in ["a", "b", "c"] {
            let spec = RunSpec::api(RunCommand::Get, vec![target.into()], vec![]);
            let admitted = admit(&first, spec).await.unwrap();
            assert_eq!(admitted.status, RunStatus::Pending);
            assert_eq!(admitted.position, Some(ids.len() + 1));
            ids.push(admitted.run_id);
        }
        // One leaves the queue before the restart.
        let gone = first.queue.take(&ids[1]).unwrap();
        assert_eq!(gone.spec.targets, ["b"]);
        run_queue::remove(&first.config.resolved.db_path, &ids[1])
            .await
            .unwrap();
        drop(first);

        let next = state(dir.path());
        resume_at_startup(&next).await.unwrap();
        assert_eq!(next.queue.waiting_ids(), [ids[0].clone(), ids[2].clone()]);
        assert!(next.queue.paused_at().is_some(), "the pause is kept");
        let run = next.runs.get(&ids[2]).unwrap();
        assert_eq!(run.status, RunStatus::Pending);
        assert_eq!(run.target.as_deref(), Some("c"));
        assert!(next.events.contains_key(&ids[2]), "a client can follow it");
        let view = view(&next);
        assert_eq!(view.queued.len(), 2);
        assert_eq!(view.queued[1].position, Some(2));
        assert_eq!(view.max_runs, 1);
        // Paused, nothing starts; resumed, the head starts (and fails: no Python here).
        dispatch(&next);
        assert_eq!(next.queue.waiting_ids().len(), 2);
        let _ = resume(State(next.clone())).await.unwrap();
        assert_eq!(
            next.queue.waiting_ids(),
            [ids[2].clone()],
            "one run at a time"
        );
    }

    #[tokio::test]
    async fn a_run_the_last_server_had_started_is_recorded_interrupted_not_started_again() {
        let dir = tempfile::tempdir().unwrap();
        let first = state(dir.path());
        let _ = pause(State(first.clone())).await.unwrap();
        let spec = RunSpec::api(RunCommand::Get, vec!["a".into()], vec![]);
        let id = admit(&first, spec).await.unwrap().run_id;
        let db = first.config.resolved.db_path.clone();
        // The server started it and was killed before it recorded anything.
        run_queue::mark_running(&db, &id).await.unwrap();
        drop(first);
        let next = state(dir.path());
        resume_at_startup(&next).await.unwrap();
        assert!(next.queue.waiting_ids().is_empty());
        assert!(run_queue::list(&db).await.unwrap().is_empty());
        let record = db::get_run(&db, &id).await.unwrap().unwrap();
        assert_eq!(record.status, "error");
    }

    #[test]
    fn a_stored_run_reads_back_as_it_was_accepted() {
        let mut selection = BTreeMap::new();
        selection.insert(
            "region".to_string(),
            BTreeSet::from(["emea".to_string(), "amer".to_string()]),
        );
        let spec = RunSpec {
            command: RunCommand::Run,
            targets: vec!["report".into()],
            nodes: vec!["p.py:report".into()],
            files: vec!["p.py".into()],
            policy: CachePolicy::RefreshSelective {
                names: vec!["raw".into()],
                cascade: false,
            },
            trigger: Trigger::schedule("p.py:report", 1_760_000_000),
            values: true,
            partition: Some(barca_core::selection::PartitionSelection(selection)),
            rules: barca_core::freshness::RunRules::tick("p.py:report"),
        };
        let (back, at) = load(&store(&spec, 12.5)).unwrap();
        assert_eq!(at, 12.5);
        assert_eq!(back.command, spec.command);
        assert_eq!(back.targets, spec.targets);
        assert_eq!(back.nodes, spec.nodes);
        assert_eq!(back.files, spec.files);
        assert!(matches!(
            back.policy,
            CachePolicy::RefreshSelective { ref names, cascade: false } if names == &["raw"]
        ));
        assert_eq!(back.trigger, spec.trigger);
        assert!(back.values);
        assert_eq!(back.partition, spec.partition);
        assert_eq!(back.rules, spec.rules);
        for policy in [CachePolicy::CacheAware, CachePolicy::RefreshAll] {
            let mut spec = RunSpec::api(RunCommand::Get, vec![], vec![]);
            spec.policy = policy.clone();
            let (back, _) = load(&store(&spec, 0.0)).unwrap();
            assert_eq!(format!("{:?}", back.policy), format!("{policy:?}"));
        }
        assert!(load("{\"command\": \"get\"}").is_err());
    }
}
