//! A run's life once the queue accepted it: starting it ([`launch`], [`execute`]), putting it
//! back in the queue when a stopping server interrupted it ([`requeue`]), recording how it
//! ended ([`finish`], [`persist_outcome`]) and dropping finished runs from memory.

use super::{RunSpec, context, run_files, run_timeout};
use crate::error::ApiError;
use crate::state::{AppState, RunChannel, RunState, RunStatus, now_ts};
use barca_core::commands::{RunResult, ServedRun, serve_run};
use barca_core::envelope::{Context, ErrorEnvelope, ErrorKind};
use barca_core::outcome::{Outcome, OutcomeStatus, RunCommand, RunOptions};
use barca_core::{BarcaError, RunEvent, db, results::GetResult};
use std::time::Duration;

/// Accept a run ([`crate::queue::admit`]): it is recorded in the run queue and starts when the
/// queue lets it. Returns its run id.
pub(crate) async fn start(state: AppState, spec: RunSpec) -> Result<String, ApiError> {
    crate::queue::admit(&state, spec).await.map(|a| a.run_id)
}

/// Execute a run the queue let start, in the background, holding one of the run slots.
pub(crate) fn launch(
    state: AppState,
    waiting: crate::queue::Waiting,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let run_id = waiting.run_id;
    let cancel = state.runs.get(&run_id).map(|r| r.cancel.clone());
    let channel = state.events.get(&run_id).map(|c| c.clone());
    let (Some(cancel), Some(channel)) = (cancel, channel) else {
        // Forgotten meanwhile (it cannot be: a run is tracked until it finishes).
        return;
    };
    // Tracked, so a drain waits for it; inside the server's cancel-reason scope, so a run the
    // server cancels records why (`server shutdown`). Boxed: the run's future is large, and
    // the scope must not hold it on the stack.
    let reason = state.cancel_reason.clone();
    let tasks = state.run_tasks.clone();
    tasks.spawn(reason.scope(Box::pin(execute(
        state,
        run_id,
        waiting.spec,
        cancel,
        channel,
        permit,
    ))));
}

async fn execute(
    state: AppState,
    run_id: String,
    spec: RunSpec,
    cancel: barca_core::CancellationToken,
    channel: RunChannel,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    // The run holds one run slot (`--max-runs`) until it ends. Runs execute in parallel; the
    // shared metadata.db is kept safe by barca-core's process-wide DB lock.
    let ctx = context(spec.command, &spec.files);

    if cancel.is_cancelled() {
        // Cancelled as it was leaving the queue: never started, nothing to clean up.
        let outcome = cancelled(&state, &ctx);
        finish(
            &state, &run_id, &spec, &channel, None, outcome, false, false,
        )
        .await;
        drop(permit);
        crate::queue::dispatch(&state);
        return;
    }
    if let Err(e) =
        barca_core::serve::run_queue::mark_running(&state.config.resolved.db_path, &run_id).await
    {
        barca_core::errln!("[barca] warning: run {run_id}: {e}");
    }
    channel.emit(RunEvent::RunStarted {
        run_id: run_id.clone(),
    });

    // Core streams events over an unbounded channel (sync send from inside the worker-pool
    // loop). A drain task forwards them onto the run's channel. The engine's own
    // `run_started` says its database row exists; it is not forwarded (the run already
    // announced itself).
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunEvent>();
    let drain = {
        let (channel, state, run_id) = (channel.clone(), state.clone(), run_id.clone());
        tokio::spawn(async move {
            let mut recorded = false;
            while let Some(event) = event_rx.recv().await {
                if let RunEvent::RunStarted { run_id: id } = event {
                    recorded = true;
                    if let Some(mut run) = state.runs.get_mut(&run_id) {
                        run.db_run_id = Some(id);
                    }
                } else {
                    channel.emit(event);
                }
            }
            recorded
        })
    };

    let mut timed_out = false;
    let timeout = run_timeout(&state, &spec);
    // The run's own cancel reason, so a timeout names itself in the run's logs
    // (`[barca] run cancelled: run timed out after ...`); a server that stops copies its own in.
    let run_reason = barca_core::interrupt::CancelReason::new();
    let result: Result<RunResult, BarcaError> = {
        let fut = run_reason.clone().scope(async {
            // A request that names files reads only those, as `barca get x a.py` does.
            let files = run_files(&state, &spec.files);
            let dag = if spec.files.is_empty() {
                state.current_dag().await?
            } else {
                std::sync::Arc::new(barca_core::load::build_dag(files, &state.config.python).await?)
            };
            serve_run(ServedRun {
                dag,
                cfg: &state.config.resolved,
                command: spec.command,
                targets: &spec.targets,
                files,
                python: &state.config.python,
                policy: spec.policy.clone(),
                cancel: cancel.clone(),
                event_tx: Some(event_tx),
                options: RunOptions {
                    run_id: Some(run_id.clone()),
                    trigger: spec.trigger.clone(),
                    host: std::sync::Arc::new(barca_core::serve::ServeHost::new(
                        state.capacity.clone(),
                        state.shared_steps.clone(),
                    )),
                    batch: barca_core::batch::BatchOverride::parse(&spec.batch).unwrap_or_default(),
                },
                partition: spec.partition.clone(),
                rules: spec.rules.clone(),
            })
            .await
        });
        tokio::pin!(fut);
        // On timeout, cancel the token and keep awaiting: the run observes the cancellation,
        // terminates its workers, persists partial results, and returns — nothing is left
        // running in the background.
        tokio::select! {
            // Before the run is polled again: it reads the reason when it sees the cancel.
            biased;
            _ = cancel.cancelled() => {
                if let Some(why) = state.cancel_reason.get() {
                    run_reason.set(why);
                }
                fut.await
            }
            _ = async {
                match &timeout {
                    Some((limit, _)) => tokio::time::sleep(*limit).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                // An operator cancel that is still unwinding when the deadline hits stays
                // classified as cancelled, not as a timeout.
                timed_out = !cancel.is_cancelled();
                if let Some((limit, source)) = &timeout {
                    run_reason.set(format!(
                        "run timed out after {}s ({source})",
                        limit.as_secs()
                    ));
                }
                cancel.cancel();
                fut.await
            }
            res = &mut fut => res,
        }
        // `fut` (and with it the event sender) is dropped here.
    };
    // The run has returned and its event sender is gone: wait for the drain so trailing
    // lines and logs land before `run_finished`.
    let recorded = drain.await.unwrap_or(false);

    // Stopped by a server whose drain deadline passed: back to the queue, to start again after
    // the restart, unless someone cancelled it.
    if matches!(result, Err(BarcaError::Cancelled))
        && !timed_out
        && state
            .queue
            .requeue
            .load(std::sync::atomic::Ordering::SeqCst)
        && state.runs.get(&run_id).is_some_and(|r| !r.cancel_requested)
    {
        requeue(&state, &run_id).await;
        return;
    }

    let single = match &result {
        Ok(RunResult::Single(r)) => Some(r.clone()),
        _ => None,
    };
    let values = spec.values && spec.command == RunCommand::Get;
    let returned_values = |single, keys, summary| {
        barca_core::values::returned_values(
            &state.config.python,
            &state.config.resolved,
            single,
            keys,
            summary,
        )
    };
    let outcome = match result {
        Ok(RunResult::Single(r)) if values || spec.command == RunCommand::Run => {
            let output = if spec.command == RunCommand::Run {
                barca_core::values::task_output(
                    &state.config.python,
                    &state.config.resolved,
                    r.final_output.as_ref(),
                    r.partitions.as_deref(),
                    r.key_summary.as_ref(),
                )
                .await
            } else {
                returned_values(
                    r.final_output.as_ref(),
                    r.partitions.as_deref(),
                    r.key_summary.as_ref(),
                )
                .await
            };
            Outcome::success_with(&r, output)
        }
        Ok(RunResult::Single(r)) => Outcome::success(&r, spec.command),
        Ok(RunResult::Multi(m)) if values || spec.command == RunCommand::Run => {
            let mut outputs = Vec::with_capacity(m.targets.len());
            for (_, t) in &m.targets {
                let output = if spec.command == RunCommand::Run {
                    barca_core::values::task_output(
                        &state.config.python,
                        &state.config.resolved,
                        t.final_output.as_ref(),
                        t.partitions.as_deref(),
                        t.key_summary.as_ref(),
                    )
                    .await
                } else {
                    returned_values(
                        t.final_output.as_ref(),
                        t.partitions.as_deref(),
                        t.key_summary.as_ref(),
                    )
                    .await
                };
                outputs.push(output);
            }
            Outcome::multi_with(&m, &outputs, &ctx)
        }
        Ok(RunResult::Multi(m)) => Outcome::multi(&m, spec.command, &ctx),
        Err(BarcaError::Cancelled) if timed_out => Outcome::ended(
            OutcomeStatus::Error,
            &ErrorEnvelope::from_prose(ErrorKind::Infra, {
                let (limit, source) = timeout.clone().unwrap_or_default();
                format!(
                    "run timed out after {}s ({source})\nThe server stops a run that takes \
                         longer than its run timeout; steps that finished are cached. Raise it \
                         with --run-timeout, [server] run_timeout_seconds or \
                         Schedule(..., timeout_seconds=N); 0 means no limit.",
                    limit.as_secs(),
                )
            }),
        ),
        Err(BarcaError::Cancelled) => cancelled(&state, &ctx),
        // A partitioned `get` that stopped at a failed key still returns the keys that
        // succeeded, as `barca get` does.
        Err(BarcaError::WorkerFailed(f))
            if spec.command == RunCommand::Get
                && f.run.as_ref().is_some_and(|r| r.partitions.is_some()) =>
        {
            let keys = f.run.as_ref().and_then(|r| r.partitions.clone());
            let summary = f.run.as_ref().and_then(|r| r.key_summary.clone());
            let output = if values {
                returned_values(None, keys.as_deref(), summary.as_ref()).await
            } else {
                barca_core::report::pointer_output(None, keys.as_deref(), summary.as_ref())
            };
            Outcome::of_failed(BarcaError::WorkerFailed(f), &ctx, output)
        }
        Err(e) => Outcome::of_error(e, &ctx),
    };
    finish(
        &state, &run_id, &spec, &channel, single, outcome, recorded, timed_out,
    )
    .await;
    // The run slot is released here, freeing capacity for a queued run.
    drop(permit);
    crate::queue::dispatch(&state);
}

/// Put a run the stopping server stopped back in the queue ([`barca_core::serve::run_queue::requeue`]):
/// the next server starts it again, under the same id. Its event stream ends without
/// `run_finished` when the server stops; a client following it reconnects to the next one.
async fn requeue(state: &AppState, run_id: &str) {
    match barca_core::serve::run_queue::requeue(&state.config.resolved.db_path, run_id, None).await
    {
        Ok(()) => {
            barca_core::errln!(
                "[barca] run {run_id} stopped and put back in the queue: it starts again when \
                 the server restarts"
            );
            if let Some(mut r) = state.runs.get_mut(run_id) {
                r.status = RunStatus::Pending;
            }
        }
        Err(e) => {
            barca_core::errln!("[barca] could not put run {run_id} back in the queue: {e}");
        }
    }
}

/// The outcome of a cancelled run. When the server cancelled it (a forced stop), its message
/// names why: `run cancelled: server shutdown`.
pub(super) fn cancelled(state: &AppState, ctx: &Context) -> Outcome {
    let mut envelope = barca_core::outcome::envelope_of(BarcaError::Cancelled, ctx);
    if state.cancel_reason.get().is_some() {
        envelope.error = state.cancelled_message();
    }
    Outcome::ended(OutcomeStatus::Cancelled, &envelope)
}

/// Record how a run ended: its outcome in the metadata DB (a row of its own when the run
/// never wrote one), the live state, and the terminal `run_finished` event.
#[allow(clippy::too_many_arguments)]
pub(super) async fn finish(
    state: &AppState,
    run_id: &str,
    spec: &RunSpec,
    channel: &RunChannel,
    result: Option<GetResult>,
    outcome: Outcome,
    recorded: bool,
    timed_out: bool,
) {
    let status = outcome.status();
    if let Err(e) = persist_outcome(state, run_id, spec, &outcome, recorded, timed_out).await {
        barca_core::errln!("[barca] could not record the outcome of run {run_id}: {e}");
    }
    // Recorded: it leaves the queue.
    if let Err(e) =
        barca_core::serve::run_queue::remove(&state.config.resolved.db_path, run_id).await
    {
        barca_core::errln!("[barca] warning: run {run_id}: {e}");
    }
    let error = outcome.envelope().map(|e| e.error);
    if let Some(mut r) = state.runs.get_mut(run_id) {
        r.finished_at = Some(now_ts());
        r.status = match status {
            OutcomeStatus::Success => RunStatus::Complete,
            OutcomeStatus::Cancelled => RunStatus::Cancelled,
            OutcomeStatus::Failed | OutcomeStatus::Error => RunStatus::Failed,
        };
        // The outcome holds what the run returned; the result is kept for its pointer and
        // counts, not for every key of a partitioned target (#623).
        r.result = result.map(|mut r| {
            r.partitions = None;
            r.key_summary = None;
            r
        });
        r.error = error;
        r.outcome = Some(outcome.clone());
    }
    channel.emit(RunEvent::RunFinished {
        run_id: run_id.to_string(),
        ok: status == OutcomeStatus::Success,
        outcome: Some(outcome.into_raw()),
    });
    evict_over_budget(state, FINISHED_RUNS_BUDGET);
    // What the run freed (at 200k keys, about a gigabyte) goes back to the system, so the
    // server's resident memory does not stay at the largest run each thread has served.
    tokio::task::spawn_blocking(barca_core::release_free_memory);
}

/// How many bytes of finished runs (their outcomes and event backlogs) the server keeps in
/// memory. Past it the oldest finished runs are dropped first, as runs older than an hour are;
/// their history stays in the DB, where `GET /runs/{run_id}` reads it (#623).
pub(crate) const FINISHED_RUNS_BUDGET: usize = 64 << 20;

/// About how many bytes a finished run holds in memory: its outcome and its event backlog.
fn retained_bytes(state: &AppState, handle: &str, run: &RunState) -> usize {
    run.outcome.as_ref().map_or(0, |o| o.text().len())
        + state.events.get(handle).map_or(0, |c| c.bytes())
}

/// Drop the oldest finished runs from memory until those left hold at most `budget` bytes.
/// The most recently finished run is always kept, so a client that has just seen it start can
/// still read its events.
pub(crate) fn evict_over_budget(state: &AppState, budget: usize) {
    let mut finished: Vec<(f64, String, usize)> = state
        .runs
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                RunStatus::Complete | RunStatus::Failed | RunStatus::Cancelled
            )
        })
        .map(|r| {
            let bytes = retained_bytes(state, r.key(), r.value());
            (r.finished_at.unwrap_or(0.0), r.key().clone(), bytes)
        })
        .collect();
    let mut total: usize = finished.iter().map(|(_, _, b)| b).sum();
    if total <= budget {
        return;
    }
    // Oldest first; the newest is never dropped.
    finished.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    finished.pop();
    for (_, handle, bytes) in finished {
        if total <= budget {
            break;
        }
        state.runs.remove(&handle);
        state.events.remove(&handle);
        total -= bytes;
    }
}

async fn persist_outcome(
    state: &AppState,
    run_id: &str,
    spec: &RunSpec,
    outcome: &Outcome,
    recorded: bool,
    timed_out: bool,
) -> Result<(), BarcaError> {
    let cfg = &state.config.resolved;
    let status = outcome.status().as_str();
    // A run stopped at its timeout recorded itself `cancelled`: it is an `error`, with the
    // reason in its outcome.
    if recorded && timed_out {
        if db::replace_run_outcome(&cfg.db_path, run_id, status, outcome.text()).await? {
            return Ok(());
        }
    } else if recorded && db::set_run_outcome(&cfg.db_path, run_id, status, outcome.text()).await? {
        return Ok(());
    }
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    let target = (!spec.targets.is_empty()).then(|| spec.targets.join(","));
    db::record_unstarted_run(
        &cfg.db_path,
        db::NewRun {
            run_id,
            command: spec.command.as_str(),
            files: &db::encode_files(run_files(state, &spec.files)),
            target: target.as_deref(),
            steps_total: None,
            job: None,
            trigger: &spec.trigger,
        },
        status,
        outcome.text(),
    )
    .await
}

/// Evict finished runs older than `max_age` from the in-memory runs map, checking every
/// `interval`. Their history stays in the DB.
pub async fn evict_finished_runs(state: AppState, interval: Duration, max_age: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        let cutoff = now_ts() - max_age.as_secs_f64();
        state.runs.retain(|handle, run| {
            let keep = match run.status {
                RunStatus::Complete | RunStatus::Failed | RunStatus::Cancelled => {
                    run.finished_at.is_none_or(|t| t > cutoff)
                }
                _ => true,
            };
            if !keep {
                // Drop the live event channel too; subscribers' streams end.
                state.events.remove(handle);
            }
            keep
        });
    }
}
