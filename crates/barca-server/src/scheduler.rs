//! Cron scheduler — the background task that gives `Freshness::Schedule(...)` teeth.
//!
//! `barca serve` parses `@asset(freshness=Schedule("0 5 * * *"))` into
//! `Freshness::Schedule(CronExpr)`, but nothing in the executor ever acts on it.
//! This module closes that gap: at startup it enumerates every scheduled node, and
//! then on each live cron match it triggers a run through the same
//! [`crate::runs`] run pool the HTTP `/run` endpoints use — so scheduled
//! runs are bounded the same way and land in the `runs` history table for free.
//! A scheduled task reuses cached upstream assets; `POST /run/{task}` does not.
//!
//! A tick is one run (RFC-0008): the scheduled node and every `Always` node below it or below
//! an unscheduled sensor the run polls ([`barca_service::freshness::tick_targets`]), a `Manual`
//! node read as it is. A job whose ticks failed `max_failures` times in a row is skipped and
//! reported `failing` until a node it failed on (or an input of one, or the scheduled node)
//! succeeds in another run.
//!
//! Semantics: cron is evaluated in the configured timezone (`--timezone`, local
//! by default). On startup a job fires once if a tick elapsed while the daemon
//! was down (catch-up), then fires on each live cron match. Last-fired times are
//! persisted so catch-up survives restarts; runs execute through the bounded run
//! pool and are visible via `GET /schedule`.
//!
//! The loop, catch-up and the `/schedule` registry are here; which jobs a tick fires or skips
//! is [`tick`], and the failing rule with the per-job bookkeeping is [`failures`].

use crate::runs;
use crate::state::AppState;
use crate::views::JobStatus;
use barca_service::schedule::ScheduledJob;
pub(crate) use barca_service::schedule::Zone;
use barca_service::{NodeKind, db};
use chrono::{DateTime, FixedOffset};
use std::sync::atomic::Ordering;

mod failures;
#[cfg(test)]
mod tests;
mod tick;

use failures::*;
use tick::*;

/// Note a skipped tick of `job` at `epoch`: the line on stderr, the `/schedule` entry, and a
/// `barca.run` span with status `skipped` when telemetry is on. `handle` is the run still in
/// flight; `None` for a paused job.
fn record_skip(
    book: &mut Bookkeeping,
    job: &ScheduledJob,
    handle: Option<&str>,
    epoch: i64,
    what: &str,
) {
    let (reason, line) = match handle {
        Some(handle) => (
            "in_flight",
            format!(
                "[barca] {what} {} skipped — run {handle} of the same node still in flight",
                job.id
            ),
        ),
        None => (
            "paused",
            format!("[barca] {what} {} skipped — the schedule is paused", job.id),
        ),
    };
    barca_service::jsonlog::line(barca_service::jsonlog::Level::Warning, Some(&job.id), &line);
    book.last_skip
        .insert(job.id.clone(), (epoch, reason, handle.map(str::to_string)));
    let report = skip_report(job, reason, handle, epoch);
    tokio::spawn(async move {
        let integrations = barca_service::telemetry::configured();
        if !integrations.is_empty() {
            // A skipped tick is not an outcome of the job: no history to read.
            let history = barca_service::telemetry::JobHistory::none();
            barca_service::telemetry::export(&integrations, &report, &history).await;
        }
    });
}

/// The `barca.run` report for a skipped tick: no steps, no duration, status `skipped`, its
/// reason (`in_flight` or `paused`) and the handle of the run that was in flight. It gets an
/// id of its own, so it is a trace of its own.
fn skip_report(
    job: &ScheduledJob,
    reason: &str,
    handle: Option<&str>,
    epoch: i64,
) -> barca_service::telemetry::RunReport {
    barca_service::telemetry::RunReport {
        run_id: db::generate_run_id(),
        command: match job.kind {
            NodeKind::Task => "run",
            NodeKind::Asset | NodeKind::Sensor => "get",
        }
        .to_string(),
        target: Some(job.id.clone()),
        job: job.id.clone(),
        env: None,
        targets: vec![barca_service::telemetry::short_name(&job.id).to_string()],
        files: Vec::new(),
        status: "skipped".to_string(),
        reason: Some(reason.to_string()),
        in_flight_run: handle.map(str::to_string),
        start_unix_ns: u64::try_from(epoch)
            .unwrap_or(0)
            .saturating_mul(1_000_000_000),
        duration_ns: 0,
        steps_total: 0,
        steps_executed: 0,
        steps_cached: 0,
        steps: Vec::new(),
        trigger: barca_service::outcome::Trigger::schedule(&job.id, epoch),
    }
}

/// Trigger the run for the tick of a due job at `epoch`: its node and the `Always` nodes below
/// it and below the unscheduled sensors the run polls, in one run under `run_id`. Returns the
/// run id: the run joins the queue (`None` when it could not be recorded there; the error is
/// printed).
///
/// A tick brings the nodes up to date, it does not force them: sensors upstream are polled,
/// anything whose inputs changed is recomputed, and an asset whose inputs did not change is
/// served from cache. The scheduled task itself always runs; a task below it fires only when
/// it is out of date. The run's command follows the scheduled node's kind: `run` for a task,
/// `get` for an asset or sensor.
async fn trigger(
    state: &AppState,
    job: &ScheduledJob,
    epoch: i64,
    run_id: String,
) -> Option<String> {
    let command = match job.kind {
        NodeKind::Task => barca_service::outcome::RunCommand::Run,
        NodeKind::Asset | NodeKind::Sensor => barca_service::outcome::RunCommand::Get,
    };
    let targets = job.targets();
    runs::start_as(
        state.clone(),
        run_id,
        runs::RunSpec {
            command,
            targets: targets.clone(),
            nodes: targets,
            files: Vec::new(),
            policy: barca_service::cache::CachePolicy::CacheAware,
            trigger: barca_service::outcome::Trigger::schedule(&job.id, epoch),
            values: false,
            partition: None,
            batch: job.batch.iter().cloned().collect(),
            rules: barca_service::freshness::RunRules::tick(&job.id),
            timeout: job.timeout_seconds,
            kill_requeues: 0,
            queue: Default::default(),
        },
    )
    .await
    .map_err(|e| {
        let (_, envelope) = e.parts();
        barca_service::errln!(
            "[barca] scheduled run {} not started: {}",
            job.id,
            envelope.error
        );
    })
    .ok()
}

/// Fire `job`'s tick at `epoch`: record the decision, then [`trigger`] the run and name it on
/// the record. The row is written before the run joins the queue, so the run's start and end
/// always find it; a run that could not join leaves a fired tick with no run.
async fn fire(
    state: &AppState,
    job: &ScheduledJob,
    epoch: i64,
    db_path: Option<&str>,
) -> Option<String> {
    let handle = db::generate_run_id();
    if let Some(dbp) = db_path {
        record_tick(dbp, job, epoch, db::TickDecision::Fired, Some(&handle)).await;
    }
    let started = trigger(state, job, epoch, handle).await;
    if started.is_none()
        && let Some(dbp) = db_path
    {
        record_tick(dbp, job, epoch, db::TickDecision::Fired, None).await;
    }
    started
}

/// Catch-up at startup, as of `now`: fire once each job whose scheduled tick elapsed while
/// the server was down, however many ticks that was, and move its last-fired record to `now`.
/// A job with no record is anchored to `now` and does not run (no stampede on first launch).
/// A job whose node already has a run in flight (one started over HTTP while the server was
/// loading, or a CLI run on the same project) is skipped like a tick, and its last-fired record still moves to `now`.
///
/// `now` is a parameter so a restart across a tick can be tested with fixed times, and `cli`
/// ([`db::alive_running_runs`]) so a CLI run can be given without another process.
async fn catch_up(
    state: &AppState,
    cli: &[db::AliveRun],
    jobs: &[ScheduledJob],
    zone: &Zone,
    now: &DateTime<FixedOffset>,
    db_path: &str,
) -> Result<Bookkeeping, barca_service::BarcaError> {
    let saved = db::get_schedule_state(db_path).await?;
    let pauses = db::get_schedule_pauses(db_path).await?;
    let mut book = Bookkeeping {
        last_fired: saved.clone(),
        failures: db::get_schedule_failures(db_path).await?,
        ..Bookkeeping::default()
    };
    for job in jobs {
        match saved.get(&job.id) {
            Some(&last_epoch) => {
                // Ticks skipped while a pause was on are not caught up: a pause that has
                // ended counts from when it ended.
                let pause = pauses.get(&job.id);
                let paused_now = pause.is_some_and(|p| p.until.is_none_or(|u| u > now.timestamp()));
                let since = match pause.and_then(|p| p.until) {
                    Some(until) if !paused_now => last_epoch.max(until),
                    _ => last_epoch,
                };
                let last = zone.timestamp(since);
                if barca_service::schedule::next_fire(
                    &job.cron,
                    zone,
                    last.with_timezone(&chrono::Utc),
                )
                .is_some_and(|next| next <= *now)
                {
                    if !paused_now && book.failures(job) >= job.max_failures {
                        let failures = book.failures.get(&job.id).cloned().unwrap_or_default();
                        if failing_reset(state, job, &failures, db_path).await {
                            book.failures.remove(&job.id);
                            persist_failures(db_path, job, &db::ScheduleFailures::default()).await;
                        }
                    }
                    if paused_now {
                        record_skip(&mut book, job, None, now.timestamp(), "catch-up run");
                        record_tick(
                            db_path,
                            job,
                            now.timestamp(),
                            db::TickDecision::Paused,
                            None,
                        )
                        .await;
                    } else if book.failures(job) >= job.max_failures {
                        record_tick(
                            db_path,
                            job,
                            now.timestamp(),
                            db::TickDecision::Failing,
                            None,
                        )
                        .await;
                        barca_service::jsonlog::line(
                            barca_service::jsonlog::Level::Warning,
                            Some(&job.id),
                            &format!(
                                "[barca] catch-up run {} skipped — failing ({} consecutive failed ticks)",
                                job.id,
                                book.failures(job)
                            ),
                        );
                    } else if let Some(handle) = any_run_in_flight_for(state, cli, job) {
                        record_skip(
                            &mut book,
                            job,
                            Some(&handle),
                            now.timestamp(),
                            "catch-up run",
                        );
                        record_tick(
                            db_path,
                            job,
                            now.timestamp(),
                            db::TickDecision::Skipped,
                            Some(&handle),
                        )
                        .await;
                    } else if let Some(handle) =
                        fire(state, job, now.timestamp(), Some(db_path)).await
                    {
                        barca_service::jsonlog::line(
                            barca_service::jsonlog::Level::Info,
                            Some(&job.id),
                            &format!("[barca] catch-up run {} → {handle}", job.id),
                        );
                        book.last_handle.insert(job.id.clone(), handle);
                    }
                    book.last_fired.insert(job.id.clone(), now.timestamp());
                    persist_fired(db_path, &job.id, now.timestamp()).await;
                }
            }
            None => {
                book.last_fired.insert(job.id.clone(), now.timestamp());
                persist_fired(db_path, &job.id, now.timestamp()).await;
            }
        }
    }
    Ok(book)
}

/// How often the scheduler trims the tick record by age across every schedule
/// ([`db::trim_ticks`]): the ticks of a schedule that no longer fires.
const TICK_TRIM_EVERY_SECONDS: i64 = 3600;

/// Record what the scheduler decided for `job`'s tick at `epoch` (#960): values it holds, one
/// write, no read. Best-effort: a failed write is logged and does not fail the tick.
async fn record_tick(
    db_path: &str,
    job: &ScheduledJob,
    epoch: i64,
    action: db::TickDecision,
    other: Option<&str>,
) {
    let (skipped_for, run_id) = match action {
        db::TickDecision::Skipped => (other, None),
        db::TickDecision::Fired => (None, other),
        db::TickDecision::Paused | db::TickDecision::Failing => (None, None),
    };
    let tick = db::NewTick {
        schedule_id: &job.id,
        scheduled_at: epoch,
        action,
        skipped_for,
        run_id,
    };
    if let Err(e) = db::record_tick(db_path, tick).await {
        barca_service::errln!("[barca] tick record write failed for {}: {e}", job.id);
    }
}

/// Trim the tick record by age when the last trim is [`TICK_TRIM_EVERY_SECONDS`] old.
async fn trim_ticks(db_path: &str, book: &mut Bookkeeping, epoch: i64) {
    if epoch - book.ticks_trimmed_at < TICK_TRIM_EVERY_SECONDS {
        return;
    }
    book.ticks_trimmed_at = epoch;
    if let Err(e) = db::trim_ticks(db_path, epoch).await {
        barca_service::errln!("[barca] tick record trim failed: {e}");
    }
}

/// Record that `node_id` fired at `epoch` seconds. Best-effort durability.
async fn persist_fired(db_path: &str, node_id: &str, epoch: i64) {
    if let Err(e) = db::upsert_schedule_state(db_path, node_id, epoch).await {
        barca_service::errln!("[barca] schedule_state write failed for {node_id}: {e}");
    }
}

/// Publish the current job set with its last-skip and failure bookkeeping into shared state
/// for `GET /schedule` to read.
fn publish_registry(state: &AppState, jobs: &[ScheduledJob], book: &Bookkeeping) {
    let snapshot: Vec<JobStatus> = jobs
        .iter()
        .map(|j| {
            let skip = book.last_skip.get(&j.id);
            JobStatus {
                id: j.id.clone(),
                cron: j.cron_str.clone(),
                kind: j.kind,
                last_skipped: skip.map(|(at, _, _)| *at),
                skipped_for: skip.and_then(|(_, _, handle)| handle.clone()),
                skip_reason: skip.map(|(_, reason, _)| *reason),
                failures: book.failures(j),
                max_failures: j.max_failures,
            }
        })
        .collect();
    if let Ok(mut w) = state.scheduling.jobs.write() {
        *w = snapshot;
    }
}

/// Re-run static analysis to enumerate scheduled jobs. The parse itself runs
/// on the blocking pool inside `barca_service::queries::list_assets`.
async fn reload_jobs(state: &AppState) -> Vec<ScheduledJob> {
    match state.reads.loaded_dag().await {
        Ok(dag) => barca_service::schedule::jobs_from_dag(&dag),
        Err(error) => {
            barca_service::errln!("[barca] scheduler load failed: {error}");
            Vec::new()
        }
    }
}

/// How many nodes of each kind are scheduled, in words: `1 task`, `2 assets and 1 task`,
/// `1 asset, 2 sensors and 1 task`. Kinds are listed in the order asset, sensor, task.
fn count_by_kind(jobs: &[ScheduledJob]) -> String {
    let parts: Vec<String> = [
        (NodeKind::Asset, "asset"),
        (NodeKind::Sensor, "sensor"),
        (NodeKind::Task, "task"),
    ]
    .into_iter()
    .filter_map(|(kind, word)| {
        let n = jobs.iter().filter(|j| j.kind == kind).count();
        (n > 0).then(|| format!("{n} {word}{}", if n == 1 { "" } else { "s" }))
    })
    .collect();
    match parts.as_slice() {
        [] => "0 nodes".to_string(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// Log the current schedule and each job's next fire time.
fn log_schedule(jobs: &[ScheduledJob], zone: &Zone) {
    if jobs.is_empty() {
        barca_service::errln!("[barca] no scheduled nodes yet (watching for changes)");
        return;
    }
    barca_service::errln!("[barca] scheduling {}:", count_by_kind(jobs));
    let now = chrono::Utc::now();
    for job in jobs {
        let next = barca_service::schedule::next_fire(&job.cron, zone, now)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "?".to_string());
        barca_service::errln!("  {} — {} (next {})", job.id, job.cron_str, next);
    }
}

/// The scheduler background task. Spawned from `serve_async` when scheduling is
/// enabled; runs for the lifetime of the server.
pub async fn run_scheduler(state: AppState) {
    // `serve` checks the timezone before it starts anything, so this only fails for a caller
    // that spawned the scheduler on its own.
    let zone = match Zone::parse(&state.config.timezone) {
        Ok(zone) => zone,
        Err(e) => {
            barca_service::errln!("[barca] scheduler disabled: {e}");
            return;
        }
    };

    // Associate the initial jobs with the generation before loading them. Edits during
    // loading, DB initialization or catch-up must remain pending for the first tick.
    let mut seen_gen = state.reads.sources.generation.load(Ordering::Relaxed);
    let mut jobs = reload_jobs(&state).await;

    if jobs.is_empty() && !state.config.watch {
        barca_service::errln!("[barca] no scheduled nodes — scheduler idle");
        return;
    }
    log_schedule(&jobs, &zone);

    // Resolve the metadata DB path (same `.barca` a CLI run uses) and ensure the
    // schedule_state table exists. `None` → durability disabled, live-match only.
    let db_path = match db::ensure_env_dirs(&state.config.resolved.env) {
        Ok(_) => {
            let path = state.config.resolved.db_path.clone();
            // `serve` initialized it before binding and exits 3 when it cannot; this is for a
            // caller that spawned the scheduler on its own, or a database broken since.
            if let Err(error) = db::init_db(&path).await {
                barca_service::errln!(
                    "[barca] scheduler disabled: metadata initialization failed: {error}"
                );
                state.scheduler_failed(error.to_string());
                return;
            }
            if let Err(error) = state.record_writer().await {
                state.scheduler_failed(error.to_string());
                return;
            }
            Some(path)
        }
        Err(_) => {
            barca_service::errln!("[barca] scheduler: durability disabled (no metadata db)");
            None
        }
    };

    // Paused schedules, for the tick to skip. They are kept in step by the pause routes.
    if let Err(e) = crate::schedule_view::pauses(&state).await {
        let (_, envelope) = e.parts();
        barca_service::errln!(
            "[barca] scheduler: cannot read paused schedules: {}",
            envelope.error
        );
    }

    // Handle issued, last-fired epoch and last skip per job, for the `/schedule` view;
    // `last_fired` also powers durability. The overlap skip reads `state.runs.registry` and the
    // `running` rows of the metadata database, not these. Entries for jobs removed on reload are harmless.
    // Catch-up requires durability; it is skipped entirely if the DB is unavailable.
    let mut book = match &db_path {
        Some(dbp) => match catch_up(
            &state,
            &db::alive_running_runs(dbp).await,
            &jobs,
            &zone,
            &zone.now(),
            dbp,
        )
        .await
        {
            Ok(caught_up) => caught_up,
            Err(error) => {
                barca_service::errln!(
                    "[barca] scheduler disabled: cannot read schedule history: {error}"
                );
                state.scheduler_failed(format!("cannot read schedule history: {error}"));
                return;
            }
        },
        None => Bookkeeping::default(),
    };
    if let Some(dbp) = &db_path {
        trim_ticks(dbp, &mut book, zone.now().timestamp()).await;
    }
    if let Some(path) = &db_path
        && let Err(e) = barca_service::liveness::record_scheduler(
            path,
            zone.now().timestamp(),
            jobs.iter().map(|j| j.id.clone()).collect(),
            &state.config.timezone,
        )
        .await
    {
        barca_service::errln!("[barca] could not record scheduler: {e}");
    }
    publish_registry(&state, &jobs, &book);

    let mut last_heartbeat = zone.now().timestamp();
    loop {
        // A draining server starts no run: the scheduler stops, and a tick it misses is
        // caught up by the next server's startup catch-up.
        tokio::select! {
            _ = sleep_to_next_second(&zone) => {}
            _ = state.lifecycle.draining.cancelled() => {}
        }
        if state.lifecycle.is_draining() {
            barca_service::errln!("[barca] scheduler stopped: the server is draining");
            return;
        }

        // `--watch`: re-read the job set when a source file changed.
        let current_gen = state.reads.sources.generation.load(Ordering::Relaxed);
        if current_gen != seen_gen {
            seen_gen = current_gen;
            let fresh = reload_jobs(&state).await;
            barca_service::errln!("[barca] schedule reloaded: {} job(s)", fresh.len());
            jobs = fresh;
            if let Some(path) = &db_path
                && let Err(e) = barca_service::liveness::record_scheduler(
                    path,
                    zone.now().timestamp(),
                    jobs.iter().map(|j| j.id.clone()).collect(),
                    &state.config.timezone,
                )
                .await
            {
                barca_service::errln!("[barca] could not record scheduler: {e}");
            }
            log_schedule(&jobs, &zone);
            publish_registry(&state, &jobs, &book);
        }

        let now = zone.now();
        if let Some(path) = &db_path
            && now.timestamp() - last_heartbeat >= barca_service::liveness::HEARTBEAT_SECONDS
        {
            if let Err(e) = barca_service::liveness::heartbeat(path, now.timestamp()).await {
                barca_service::errln!("[barca] could not record scheduler heartbeat: {e}");
            }
            last_heartbeat = now.timestamp();
        }
        let mut changed = count_outcomes(&state, &jobs, &mut book, db_path.as_deref()).await;
        if let Some(dbp) = &db_path {
            reset_failing(&state, &now, &jobs, &mut book, dbp).await;
        }
        // NOTE: benchmarks/scheduler_overhead/barca/run.sh's CI smoke greps stderr for
        // "scheduled run.*:probe" to count ticks independent of worker execution — keep
        // that substring ("scheduled run", plus the job id) if this wording changes.
        let epoch = now.timestamp();
        // CLI runs on the same project, read only on a second some job is due: one indexed
        // query of the `running` rows.
        let cli = match &db_path {
            Some(dbp) if !due_jobs(&now, &jobs).is_empty() => db::alive_running_runs(dbp).await,
            _ => Vec::new(),
        };
        let actions = plan_tick(
            &now,
            &jobs,
            |job| crate::schedule_view::active(&state, &job.id, epoch).is_some(),
            |job| any_run_in_flight_for(&state, &cli, job),
            |job| book.failures(job),
        );
        for action in actions {
            match action {
                TickAction::Paused(job) => {
                    record_skip(&mut book, job, None, epoch, "scheduled run");
                    if let Some(dbp) = &db_path {
                        record_tick(dbp, job, epoch, db::TickDecision::Paused, None).await;
                    }
                }
                TickAction::Skip { job, handle } => {
                    record_skip(&mut book, job, Some(&handle), epoch, "scheduled run");
                    if let Some(dbp) = &db_path {
                        record_tick(dbp, job, epoch, db::TickDecision::Skipped, Some(&handle))
                            .await;
                    }
                }
                TickAction::Failing { job, failures } => {
                    barca_service::jsonlog::line(
                        barca_service::jsonlog::Level::Warning,
                        Some(&job.id),
                        &format!(
                            "[barca] scheduled run {} skipped — failing ({failures} consecutive \
                             failed ticks, max_failures={})",
                            job.id, job.max_failures
                        ),
                    );
                    if let Some(dbp) = &db_path {
                        record_tick(dbp, job, epoch, db::TickDecision::Failing, None).await;
                    }
                }
                TickAction::Fire(job) => {
                    let Some(handle) = fire(&state, job, epoch, db_path.as_deref()).await else {
                        continue;
                    };
                    barca_service::jsonlog::line(
                        barca_service::jsonlog::Level::Info,
                        Some(&job.id),
                        &format!("[barca] scheduled run {} → {handle}", job.id),
                    );
                    book.last_handle.insert(job.id.clone(), handle);
                    book.last_fired.insert(job.id.clone(), now.timestamp());
                    if let Some(dbp) = &db_path {
                        persist_fired(dbp, &job.id, now.timestamp()).await;
                    }
                }
            }
            changed = true;
        }
        if changed {
            publish_registry(&state, &jobs, &book);
            if let Some(dbp) = &db_path {
                trim_ticks(dbp, &mut book, epoch).await;
            }
        }
    }
}
