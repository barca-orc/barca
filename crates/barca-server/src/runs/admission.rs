//! Admitting and starting runs: a run is recorded in the run queue ([`crate::queue`]) and in
//! memory before the server answers, and starts when the queue lets it. At startup the queue
//! the last server left is resumed.

use super::RunSpec;
use crate::error::ApiError;
use crate::queue::{Waiting, goes_ahead, load, store, with_scheduled};
use crate::replay::RunChannel;
use crate::run_model::{RunState, RunStatus};
use crate::state::AppState;
use barca_service::cache::CachePolicy;
use barca_service::db;
use barca_service::serve::run_queue::{self, QueueState};
use barca_service::time::{now_secs_f64 as now_ts, rfc3339_millis as timestamp};

/// What [`admit`] answers: the run id, and whether the run started at once or waits (with its
/// place, 1 = next).
pub struct Admitted {
    pub run_id: String,
    pub status: RunStatus,
    pub position: Option<usize>,
    pub waiting_for: Option<barca_service::run_explain::WaitingFor>,
}

/// The metadata DB exists and has barca's tables: the first run of a new project creates it.
pub(crate) async fn ensure_db(state: &AppState) -> Result<(), barca_service::BarcaError> {
    let cfg = &state.config.resolved;
    let path = std::path::Path::new(&cfg.db_path);
    if !path.exists() {
        if !path.parent().is_some_and(std::path::Path::exists) {
            db::ensure_env_dirs(&cfg.env)?;
        }
        db::init_db(&cfg.db_path).await?;
    }
    state.record_writer().await
}

/// Accept a run: allocate its id, record it in the queue (the metadata DB) and in memory, and
/// start it when the queue lets it. A run that cannot be recorded is refused (`500`): it would
/// not survive a restart.
pub async fn admit(state: &AppState, spec: RunSpec) -> Result<Admitted, ApiError> {
    admit_as(state, spec, db::generate_run_id()).await
}

/// [`admit`] under a run id the caller allocated ([`db::generate_run_id`]): the scheduler
/// names the run on its tick record before the run can start.
pub async fn admit_as(
    state: &AppState,
    spec: RunSpec,
    run_id: String,
) -> Result<Admitted, ApiError> {
    let channel = state.runs.replay.channel()?;
    let accepted_at = now_ts();
    {
        let _admitting = state.runs.queue.admit.lock().await;
        ensure_db(state).await?;
        run_queue::push_with_slot(
            &state.config.resolved.db_path,
            &run_id,
            &store(&spec, accepted_at),
            &timestamp(accepted_at),
            super::lifecycle::tick_of(&spec),
        )
        .await
        .map_err(|e| {
            ApiError::Barca(barca_service::BarcaError::Db(format!(
                "could not record the run in the run queue: {e}"
            )))
        })?;
        track(state, &run_id, &spec, accepted_at, channel);
        state.runs.queue.waiting.lock().unwrap().push_back(Waiting {
            run_id: run_id.clone(),
            spec,
        });
    }
    dispatch(state);
    let status = state
        .runs
        .registry
        .get(&run_id)
        .map_or(RunStatus::Pending, |r| r.status);
    let position = state
        .runs
        .queue
        .waiting_ids()
        .iter()
        .position(|id| *id == run_id)
        .map(|at| at + 1);
    Ok(Admitted {
        run_id,
        status,
        position,
        waiting_for: state.lifecycle.waiting_for(status),
    })
}

/// Whether a scheduled run, or one asked to queue last, is executing now.
fn scheduled_running(state: &AppState) -> bool {
    state
        .runs
        .registry
        .iter()
        .any(|r| r.status == RunStatus::Running && with_scheduled(&r.trigger, r.queue))
}

/// The live record and event channel of a run that joins the queue.
fn track(state: &AppState, run_id: &str, spec: &RunSpec, accepted_at: f64, channel: RunChannel) {
    // Child of the server-wide shutdown token: DELETE /runs/{id} cancels just this run;
    // a forced stop cancels all of them.
    let cancel = state.lifecycle.shutdown.child_token();
    state.runs.registry.insert(
        run_id.to_string(),
        RunState {
            handle: run_id.to_string(),
            db_run_id: None,
            command: spec.command.as_str().into(),
            target: (!spec.targets.is_empty()).then(|| spec.targets.join(",")),
            nodes: spec.nodes.clone(),
            trigger: spec.trigger.clone(),
            queue: spec.queue,
            refreshes: !matches!(spec.policy, CachePolicy::CacheAware),
            files: spec.files.clone(),
            status: RunStatus::Pending,
            result: None,
            error: None,
            started_at: accepted_at,
            finished_at: None,
            outcome: None,
            cancel,
            cancel_requested: false,
            cancellation: None,
        },
    );
    state.runs.events.insert(run_id.to_string(), channel);
}

/// Start the runs at the head of the queue while there is room: not paused, not draining, fewer
/// than `max_runs` executing. Called when a run joins, when one ends and when the queue is
/// resumed.
pub fn dispatch(state: &AppState) {
    loop {
        if state.lifecycle.is_draining() || state.runs.queue.paused_at().is_some() {
            return;
        }
        let mut waiting = state.runs.queue.waiting.lock().unwrap();
        // The first run someone asked for goes ahead of scheduled runs and of the runs asked
        // to queue last.
        let Some(at) = waiting
            .iter()
            .position(|w| goes_ahead(&w.spec))
            .or((!waiting.is_empty()).then_some(0))
        else {
            return;
        };
        let interactive = goes_ahead(&waiting[at].spec);
        let permit = match state.runs.slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            // The extra slot is for running past scheduled work, not for running two
            // requested runs past `max_runs`.
            Err(_) if interactive && scheduled_running(state) => {
                match state.runs.interactive_slot.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => return,
                }
            }
            Err(_) => return,
        };
        let next = waiting.remove(at).expect("index in range");
        drop(waiting);
        if let Some(mut run) = state.runs.registry.get_mut(&next.run_id) {
            run.status = RunStatus::Running;
        }
        crate::runs::launch(state.clone(), next, permit);
    }
}

/// At startup, before anything is admitted: read the queue the last server left. Queued runs
/// join the queue in their order, under their run ids; a run that was started and never
/// recorded how it ended (the server was killed) is not started again: it is recorded as
/// interrupted. Then read whether the queue is paused. Prints what it resumes.
pub async fn resume_at_startup(state: &AppState) -> Result<(), barca_service::BarcaError> {
    let path = state.config.resolved.db_path.clone();
    if !std::path::Path::new(&path).exists() {
        return Ok(());
    }
    let rows = run_queue::list(&path).await?;
    let paused = run_queue::paused_at(&path).await?;
    state.runs.queue.set_paused(paused);
    let mut resumed = 0;
    for row in rows {
        match (row.state, load(&row.spec)) {
            (QueueState::Queued, Ok((spec, accepted_at))) => {
                track(
                    state,
                    &row.run_id,
                    &spec,
                    accepted_at,
                    state.runs.replay.channel()?,
                );
                state.runs.queue.waiting.lock().unwrap().push_back(Waiting {
                    run_id: row.run_id,
                    spec,
                });
                resumed += 1;
            }
            // In flight when the last server was told to stop, and it never finished the drain:
            // what sent the signal killed it (a supervisor's grace period ran out), not a crash
            // of its own. Back in its place, once: a run killed in two drains in a row is
            // interrupted like a crash, so a run that keeps its server from stopping cannot
            // loop (#628).
            (QueueState::Draining, Ok((mut spec, accepted_at))) if spec.kill_requeues == 0 => {
                spec.kill_requeues += 1;
                match run_queue::requeue(&path, &row.run_id, Some(&store(&spec, accepted_at))).await
                {
                    Ok(()) => {
                        barca_service::errln!(
                            "[barca] run {} was in flight when the last server was killed during \
                             its drain (SIGKILL after SIGTERM: the stop grace period ran out); \
                             it is put back in the queue and starts again (steps that finished \
                             are cached)",
                            row.run_id
                        );
                        track(
                            state,
                            &row.run_id,
                            &spec,
                            accepted_at,
                            state.runs.replay.channel()?,
                        );
                        state.runs.queue.waiting.lock().unwrap().push_back(Waiting {
                            run_id: row.run_id,
                            spec,
                        });
                        resumed += 1;
                    }
                    Err(e) => {
                        barca_service::errln!(
                            "[barca] warning: could not put run {} back in the queue: {e}",
                            row.run_id
                        );
                        interrupted(state, &row.run_id, Some(spec)).await;
                    }
                }
            }
            (QueueState::Running | QueueState::Draining, loaded) => {
                interrupted(state, &row.run_id, loaded.ok().map(|(s, _)| s)).await;
            }
            (QueueState::Queued, Err(why)) => {
                barca_service::errln!(
                    "[barca] warning: queued run {} cannot be read ({why}); it is not resumed",
                    row.run_id
                );
                run_queue::remove(&path, &row.run_id).await?;
            }
        }
    }
    if resumed > 0 {
        barca_service::errln!(
            "[barca] resuming {resumed} queued run{}",
            if resumed == 1 { "" } else { "s" }
        );
    }
    if let Some(at) = paused {
        barca_service::errln!(
            "[barca] the run queue is paused (since {}): queued runs wait for POST /queue/resume \
             (barca queue resume)",
            barca_service::outcome::rfc3339(at)
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
    barca_service::errln!(
        "[barca] warning: run {run_id} was interrupted when the server last stopped (it was \
         killed before it finished); it is not started again"
    );
    let unrecorded = matches!(db::get_run(&cfg.db_path, run_id).await, Ok(None));
    if let Some(spec) = &spec {
        // The tick that started it ends as the run's row does: `interrupted`, or `error` for
        // a run that never wrote one.
        let status = "interrupted";
        crate::runs::record_tick_finished(state, run_id, spec, status, None).await;
    }
    if unrecorded && let Some(spec) = spec {
        let outcome = barca_service::outcome::Outcome::ended(
            barca_service::outcome::OutcomeStatus::Interrupted,
            &barca_service::envelope::ErrorEnvelope::from_prose(
                barca_service::envelope::ErrorKind::Infra,
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
        let _ = barca_service::serve::record_unstarted_run(
            &cfg.db_path,
            barca_service::serve::UnstartedRun {
                run_id,
                command: spec.command.as_str(),
                files,
                target: target.as_deref(),
                trigger: &spec.trigger,
            },
            "interrupted",
            outcome.text(),
        )
        .await;
    }
    if let Err(e) = run_queue::remove(&cfg.db_path, run_id).await {
        barca_service::errln!("[barca] warning: {e}");
    }
}
