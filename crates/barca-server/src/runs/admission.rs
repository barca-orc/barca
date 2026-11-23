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
    mut spec: RunSpec,
    run_id: String,
) -> Result<Admitted, ApiError> {
    let channel = state.runs.replay.channel()?;
    let accepted_at = now_ts();
    if let Some((result, outcome, lines)) = fresh_read(state, &spec, &run_id).await {
        ensure_db(state).await?;
        // A cache read has no durable queue entry to recover. Refuse it if its terminal
        // record cannot be written, rather than acknowledge an identity that can be lost.
        let body: serde_json::Value = serde_json::from_str(outcome.text()).expect("typed outcome");
        let cached = body["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| s["partitions"]["cached"].as_u64().unwrap_or(1) as usize)
            .sum();
        let target = (!spec.targets.is_empty()).then(|| spec.targets.join(","));
        let mut nodes = spec.nodes.clone();
        nodes.sort();
        let job = if nodes.is_empty() {
            "all".into()
        } else {
            nodes.join(",")
        };
        barca_service::serve::record_cached_run(
            &state.config.resolved.db_path,
            barca_service::serve::UnstartedRun {
                scope: Some(&spec.scope(&state.config.files)),
                run_id: &run_id,
                command: spec.command.as_str(),
                files: super::run_files(state, &spec.files),
                target: target.as_deref(),
                trigger: &spec.trigger,
            },
            &job,
            cached,
            body["elapsed_seconds"].as_f64().unwrap_or_default(),
            outcome.text(),
        )
        .await?;
        track(state, &run_id, &spec, accepted_at, channel.clone());
        state.runs.registry.get_mut(&run_id).unwrap().db_run_id = Some(run_id.clone());
        channel.emit(barca_service::RunEvent::RunStarted {
            run_id: run_id.clone(),
        });
        for (when, line) in lines {
            channel.emit(barca_service::RunEvent::Line { line, when });
        }
        for step in body["steps"].as_array().into_iter().flatten() {
            channel.emit(barca_service::RunEvent::StepFinished {
                node_id: step["id"].as_str().unwrap_or_default().into(),
                ok: true,
                cached: true,
                elapsed_seconds: None,
                error: None,
                import_seconds: None,
            });
        }
        super::lifecycle::finish(
            state, &run_id, &spec, &channel, result, outcome, true, false,
        )
        .await;
        return Ok(Admitted {
            run_id,
            status: RunStatus::Complete,
            position: None,
            waiting_for: None,
        });
    }
    spec.policy.accepted_at(accepted_at);
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
                             it is put back in the queue and starts again (matching completed assets are reused; tasks may repeat)",
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
/// wrote one, is recorded `interrupted` by queue ownership, and one that never wrote a row
/// gets one with a `failed` outcome, so its id is not lost.
async fn interrupted(state: &AppState, run_id: &str, spec: Option<RunSpec>) {
    let cfg = &state.config.resolved;
    barca_service::errln!(
        "[barca] warning: run {run_id} was interrupted when the server last stopped (it was \
         killed before it finished); it is not started again"
    );
    let unrecorded = matches!(db::get_run(&cfg.db_path, run_id).await, Ok(None));
    if let Some(spec) = &spec {
        // The tick that started it ends as the run's row does: `interrupted`, or `failed` for
        // a run that never wrote one.
        let status = if unrecorded { "failed" } else { "interrupted" };
        crate::runs::record_tick_finished(state, run_id, spec, status, None).await;
    }
    if unrecorded && let Some(spec) = spec {
        let outcome = barca_service::outcome::Outcome::ended(
            barca_service::outcome::OutcomeStatus::Failed,
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
                scope: Some(&spec.scope(&state.config.files)),
                run_id,
                command: spec.command.as_str(),
                files,
                target: target.as_deref(),
                trigger: &spec.trigger,
            },
            "error",
            outcome.text(),
        )
        .await;
    }
    if let Err(e) = run_queue::interrupt_predecessor(&cfg.db_path, run_id).await {
        barca_service::errln!("[barca] warning: {e}");
    }
}

/// A proved cache hit needs neither a run slot nor a pipeline worker. Keep the normal
/// outcome/history/event contract; every inconclusive probe goes through admission.
async fn fresh_read(
    state: &AppState,
    spec: &RunSpec,
    run_id: &str,
) -> Option<(
    Option<barca_service::results::GetResult>,
    barca_service::outcome::Outcome,
    Vec<(barca_service::lines::LineWhen, String)>,
)> {
    use barca_service::{
        cached,
        outcome::{Outcome, RunCommand},
        results::GetResult,
    };
    if spec.command != RunCommand::Get
        || !matches!(spec.policy, CachePolicy::CacheAware)
        || spec
            .nodes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != spec.targets.len()
        || spec.rules.tick.is_some()
    {
        return None;
    }
    let _permit = state.reads.artifact_reads.acquire().await.ok()?;
    let started = std::time::Instant::now();
    let request = cached::Request {
        targets: spec.targets.clone(),
        files: super::run_files(state, &spec.files).to_vec(),
        tier: cached::Tier::Store,
        partition: spec.partition.as_ref().map(|s| s.to_json().to_string()),
        values: spec.values,
        url_expires: None,
    };
    let (answer, mut prediction) = cached::fresh(
        &state.config.resolved,
        &state.config.python,
        &request,
        &spec.batch,
        &spec.rules,
    )
    .await
    .ok()??;
    let outputs: Vec<barca_service::cached::OutputRef> = answer
        .outcomes
        .iter()
        .map(|o| serde_json::from_value(o.clone()))
        .collect::<Result<_, _>>()
        .ok()?;
    for step in &mut prediction.steps {
        step.status = step.action.take();
    }
    let partitions = answer
        .outcomes
        .first()
        .is_some_and(|o| spec.targets.len() <= 1 && o["partition"].is_string())
        .then(|| {
            answer
                .outcomes
                .iter()
                .zip(&outputs)
                .map(|(o, output)| (o["partition"].as_str().unwrap().to_owned(), output.clone()))
                .collect()
        });
    let result = GetResult {
        run_id: run_id.into(),
        elapsed_seconds: started.elapsed().as_secs_f64(),
        steps_executed: 0,
        phases: answer.phases,
        final_output: outputs.first().cloned(),
        steps: prediction.steps,
        warnings: prediction.warnings,
        partitions,
        key_summary: answer.summaries.values().next().map(|keys| {
            barca_service::results::KeySummary {
                keys: *keys,
                computed: 0,
                listed: vec![],
            }
        }),
        cache: answer.cache.clone(),
        uploads: prediction.uploads,
        metadata_seconds: None,
        partition_selection: prediction.partition_selection,
    };
    if spec.targets.len() <= 1 {
        let outcome = Outcome::success_with(&result, answer.final_output);
        return Some((Some(result), outcome, answer.lines));
    }
    let mut targets = Vec::new();
    let mut values = Vec::new();
    for (name, node) in spec.targets.iter().zip(&spec.nodes) {
        let selected: Vec<_> = answer
            .outcomes
            .iter()
            .zip(&outputs)
            .filter(|(o, _)| o["node"] == *node)
            .collect();
        let partitions = selected
            .first()
            .is_some_and(|(o, _)| o["partition"].is_string())
            .then(|| {
                selected
                    .iter()
                    .map(|(o, output)| {
                        (
                            o["partition"].as_str().unwrap().to_owned(),
                            (*output).clone(),
                        )
                    })
                    .collect()
            });
        targets.push((
            name.clone(),
            barca_service::results::TargetOutcome {
                status: "success".into(),
                final_output: selected.first().map(|(_, output)| (*output).clone()),
                error: None,
                failed_node: None,
                failed_phase: None,
                partitions,
                key_summary: answer.summaries.get(name).map(|keys| {
                    barca_service::results::KeySummary {
                        keys: *keys,
                        computed: 0,
                        listed: vec![],
                    }
                }),
            },
        ));
        values.push(
            answer
                .final_output
                .as_ref()
                .and_then(|v| v.get(name))
                .cloned(),
        );
    }
    let multi = barca_service::results::MultiResult {
        run_id: result.run_id,
        elapsed_seconds: result.elapsed_seconds,
        steps_executed: 0,
        phases: answer.phases,
        steps: result.steps,
        warnings: result.warnings,
        targets,
        cache: answer.cache.clone(),
        uploads: result.uploads,
        metadata_seconds: None,
        partition_selection: result.partition_selection,
    };
    Some((
        None,
        Outcome::multi_with(&multi, &values, &super::context(spec.command, &spec.files)),
        answer.lines,
    ))
}
