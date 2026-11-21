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
) -> Result<String, barca_service::BarcaError> {
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
    .map_err(|e| match e {
        crate::error::ApiError::Barca(error) => error,
        other => barca_service::BarcaError::Other(other.parts().1.error),
    })
}

/// Admission commits the fired tick, queue row and restart cursor together.
async fn fire(state: &AppState, job: &ScheduledJob, epoch: i64) -> Option<String> {
    let progress = &state.scheduling.progress;
    let handle = db::generate_run_id();
    let started = progress
        .bounded("trigger", trigger(state, job, epoch, handle))
        .await
        .and_then(|r| r)
        .map_err(|e| barca_service::errln!("[barca] scheduled run {} not started: {e}", job.id))
        .ok();
    if started.is_some() {
        progress.enqueued();
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
    let progress = &state.scheduling.progress;
    let saved = progress
        .startup("schedule_state", || db::get_schedule_state(db_path))
        .await?;
    let pauses = progress
        .startup("schedule_pauses", || db::get_schedule_pauses(db_path))
        .await?;
    let mut book = Bookkeeping {
        last_fired: saved.clone(),
        failures: progress
            .startup("schedule_failures", || db::get_schedule_failures(db_path))
            .await?,
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
                if let Some(slot) = barca_service::schedule::prev_fire(
                    &job.cron,
                    zone,
                    now.with_timezone(&chrono::Utc),
                ) && slot.timestamp() > since
                {
                    state.scheduling.progress.due(slot.timestamp());
                    if !paused_now && book.failures(job) >= job.max_failures {
                        let failures = book.failures.get(&job.id).cloned().unwrap_or_default();
                        if failing_reset(state, job, &failures, db_path).await {
                            book.failures.remove(&job.id);
                            persist_failures(db_path, &job.id, &db::ScheduleFailures::default())
                                .await;
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
                    } else {
                        let handle = progress
                            .startup("trigger", || {
                                trigger(state, job, now.timestamp(), db::generate_run_id())
                            })
                            .await?;
                        progress.enqueued();
                        barca_service::jsonlog::line(
                            barca_service::jsonlog::Level::Info,
                            Some(&job.id),
                            &format!("[barca] catch-up run {} → {handle}", job.id),
                        );
                        book.accepted(job, handle);
                    }
                    book.last_fired.insert(job.id.clone(), now.timestamp());
                    progress
                        .startup("persist_fired", || {
                            db::upsert_schedule_state(db_path, &job.id, now.timestamp())
                        })
                        .await?;
                }
            }
            None => {
                book.last_fired.insert(job.id.clone(), now.timestamp());
                progress
                    .startup("persist_fired", || {
                        db::upsert_schedule_state(db_path, &job.id, now.timestamp())
                    })
                    .await?;
            }
        }
    }
    Ok(book)
}

/// How often the scheduler trims the tick record by age across every schedule
/// ([`db::trim_ticks`]): the ticks of a schedule that no longer fires.
const TICK_TRIM_EVERY_SECONDS: i64 = 3600;
pub(crate) const METADATA_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

async fn metadata<T>(
    name: &str,
    work: impl Future<Output = Result<T, barca_service::BarcaError>>,
) -> Result<T, barca_service::BarcaError> {
    tokio::time::timeout(METADATA_WAIT, work)
        .await
        .map_err(|_| barca_service::BarcaError::Db(format!("timed out waiting for {name}")))?
}

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
    if let Err(e) = metadata("record_tick", db::record_tick(db_path, tick)).await {
        barca_service::errln!("[barca] tick record write failed for {}: {e}", job.id);
    }
}

/// Trim the tick record by age when the last trim is [`TICK_TRIM_EVERY_SECONDS`] old.
async fn trim_ticks(db_path: &str, book: &mut Bookkeeping, epoch: i64) {
    if epoch - book.ticks_trimmed_at < TICK_TRIM_EVERY_SECONDS {
        return;
    }
    book.ticks_trimmed_at = epoch;
    if let Err(e) = metadata("trim_ticks", db::trim_ticks(db_path, epoch)).await {
        barca_service::errln!("[barca] tick record trim failed: {e}");
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
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    state.scheduling.progress.start();
    tokio::spawn(supervise(state, run_scheduler))
}

async fn supervise<F>(state: AppState, run: impl Fn(AppState) -> F)
where
    F: Future<Output = Result<&'static str, String>> + Send + 'static,
{
    let progress = &state.scheduling.progress;
    let mut delay = std::time::Duration::from_secs(1);
    loop {
        if state.lifecycle.is_draining() {
            progress.exited("draining".into(), false);
            return;
        }
        progress.start();
        let started = tokio::time::Instant::now();
        // Dropping the supervisor (including the test server handle) aborts its child.
        let mut child = tokio::task::JoinSet::new();
        child.spawn(run(state.clone()));
        let why = match child.join_next().await.expect("one scheduler task") {
            Ok(Ok(expected)) => {
                progress.exited(expected.into(), false);
                return;
            }
            Ok(Err(reason)) => reason,
            Err(join) => join.to_string(),
        };
        if started.elapsed() >= std::time::Duration::from_secs(600) {
            delay = std::time::Duration::from_secs(1);
        }
        progress.exited(why.clone(), true);
        barca_service::jsonlog::line(
            barca_service::jsonlog::Level::Error,
            None,
            &format!(
                "[barca] scheduler exited: {why}; retrying in {}s",
                delay.as_secs()
            ),
        );
        tokio::select! {
            _ = state.lifecycle.draining.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
        if !state.lifecycle.is_draining() {
            progress.restarted();
        }
        delay = (delay * 2).min(std::time::Duration::from_secs(60));
    }
}

async fn run_scheduler(state: AppState) -> Result<&'static str, String> {
    tokio::select! {
        biased;
        _ = state.lifecycle.draining.cancelled() => Ok("draining"),
        result = scheduler_loop(&state) => result,
    }
}

async fn scheduler_loop(state: &AppState) -> Result<&'static str, String> {
    let progress = &state.scheduling.progress;
    // `serve` checks the timezone before it starts anything, so this only fails for a caller
    // that spawned the scheduler on its own.
    let zone = match Zone::parse(&state.config.timezone) {
        Ok(zone) => zone,
        Err(e) => {
            barca_service::errln!("[barca] scheduler disabled: {e}");
            return Err(e.to_string());
        }
    };

    // Associate the initial jobs with the generation before loading them. Edits during
    // loading, DB initialization or catch-up must remain pending for the first tick.
    let startup = zone.now();
    let mut seen_gen = state.reads.sources.generation.load(Ordering::Relaxed);
    let mut jobs = progress
        .startup("reload_jobs", || async { Ok(reload_jobs(state).await) })
        .await
        .map_err(|e| e.to_string())?;

    if jobs.is_empty() && !state.config.watch {
        barca_service::errln!("[barca] no scheduled nodes — scheduler idle");
        return Ok("idle");
    }
    log_schedule(&jobs, &zone);

    // Resolve the metadata DB path (same `.barca` a CLI run uses) and ensure the
    // schedule_state table exists. `None` → durability disabled, live-match only.
    let db_path = match db::ensure_env_dirs(&state.config.resolved.env) {
        Ok(_) => {
            let path = state.config.resolved.db_path.clone();
            // `serve` initialized it before binding and exits 3 when it cannot; this is for a
            // caller that spawned the scheduler on its own, or a database broken since.
            if let Err(error) = progress.startup("init_db", || db::init_db(&path)).await {
                barca_service::errln!(
                    "[barca] scheduler disabled: metadata initialization failed: {error}"
                );
                state.scheduler_failed(error.to_string());
                return Err(error.to_string());
            }
            if let Err(error) = progress
                .startup("record_writer", || state.record_writer())
                .await
            {
                state.scheduler_failed(error.to_string());
                return Err(error.to_string());
            }
            Some(path)
        }
        Err(_) => {
            barca_service::errln!("[barca] scheduler: durability disabled (no metadata db)");
            None
        }
    };

    // Paused schedules, for the tick to skip. They are kept in step by the pause routes.
    if let Err(e) = progress
        .startup("pauses", || async {
            crate::schedule_view::pauses(state)
                .await
                .map_err(|e| match e {
                    crate::error::ApiError::Barca(err) => err,
                    other => barca_service::BarcaError::Other(other.parts().1.error),
                })
        })
        .await
    {
        barca_service::errln!("[barca] scheduler: cannot read paused schedules: {}", e);
        state.scheduler_failed(e.to_string());
        return Err(e.to_string());
    }

    // Handle issued, last-fired epoch and last skip per job, for the `/schedule` view;
    // `last_fired` also powers durability. The overlap skip reads `state.runs.registry` and the
    // `running` rows of the metadata database, not these. Entries for jobs removed on reload are harmless.
    // Catch-up requires durability; it is skipped entirely if the DB is unavailable.
    let mut evaluated = startup.timestamp();
    let mut book = match &db_path {
        Some(dbp) => match progress
            .waiting(
                "catch_up",
                catch_up(
                    state,
                    &progress
                        .startup("alive_running_runs", || async {
                            Ok(db::alive_running_runs(dbp).await)
                        })
                        .await
                        .map_err(|e| e.to_string())?,
                    &jobs,
                    &zone,
                    &startup,
                    dbp,
                ),
            )
            .await
        {
            Ok(caught_up) => caught_up,
            Err(error) => {
                barca_service::errln!(
                    "[barca] scheduler disabled: cannot read schedule history: {error}"
                );
                state.scheduler_failed(format!("cannot read schedule history: {error}"));
                return Err(format!("cannot read schedule history: {error}"));
            }
        },
        None => Bookkeeping::default(),
    };
    if let Some(dbp) = &db_path {
        let _ = progress
            .bounded(
                "trim_ticks",
                trim_ticks(dbp, &mut book, zone.now().timestamp()),
            )
            .await;
    }
    if let Some(path) = &db_path
        && let Err(e) = progress
            .bounded(
                "record_scheduler",
                barca_service::liveness::record_scheduler(
                    path,
                    zone.now().timestamp(),
                    jobs.iter().map(|j| j.id.clone()).collect(),
                    &state.config.timezone,
                ),
            )
            .await
            .and_then(|v| v)
    {
        barca_service::errln!("[barca] could not record scheduler: {e}");
    }
    if state.scheduling.stopped.swap(false, Ordering::Relaxed) {
        *state.reads.metadata_error.write().unwrap() = None;
    }
    publish_registry(state, &jobs, &book);

    let mut last_heartbeat = zone.now().timestamp();
    loop {
        progress.beat();
        // A draining server starts no run: the scheduler stops, and a tick it misses is
        // caught up by the next server's startup catch-up.
        tokio::select! {
            _ = sleep_to_next_second(&zone) => {}
            _ = state.lifecycle.draining.cancelled() => {}
        }
        if state.lifecycle.is_draining() {
            barca_service::errln!("[barca] scheduler stopped: the server is draining");
            return Ok("draining");
        }

        // `--watch`: re-read the job set when a source file changed.
        let current_gen = state.reads.sources.generation.load(Ordering::Relaxed);
        if current_gen != seen_gen {
            let Ok(fresh) = progress.bounded("reload_jobs", reload_jobs(state)).await else {
                continue;
            };
            seen_gen = current_gen;
            barca_service::errln!("[barca] schedule reloaded: {} job(s)", fresh.len());
            let discovered = zone.now().timestamp() - 1;
            for job in &fresh {
                if !jobs.iter().any(|old| old.id == job.id) {
                    book.last_fired.insert(job.id.clone(), discovered);
                }
            }
            jobs = fresh;
            if let Some(path) = &db_path
                && let Err(e) = progress
                    .bounded(
                        "record_scheduler",
                        barca_service::liveness::record_scheduler(
                            path,
                            zone.now().timestamp(),
                            jobs.iter().map(|j| j.id.clone()).collect(),
                            &state.config.timezone,
                        ),
                    )
                    .await
                    .and_then(|v| v)
            {
                barca_service::errln!("[barca] could not record scheduler: {e}");
            }
            log_schedule(&jobs, &zone);
            publish_registry(state, &jobs, &book);
        }

        evaluated = evaluate_through(
            state,
            &jobs,
            &mut book,
            &zone,
            evaluated,
            zone.now().timestamp(),
            db_path.as_deref(),
        )
        .await;
        let wall = zone.now();
        if let Some(path) = &db_path
            && wall.timestamp() - last_heartbeat >= barca_service::liveness::HEARTBEAT_SECONDS
        {
            if let Err(e) = progress
                .bounded(
                    "heartbeat",
                    barca_service::liveness::heartbeat(path, wall.timestamp()),
                )
                .await
                .and_then(|v| v)
            {
                barca_service::errln!("[barca] could not record scheduler heartbeat: {e}");
            }
            last_heartbeat = wall.timestamp();
        }
    }
}

/// Evaluate (last, wall] in bounded batches. A failed admission retains the suffix.
async fn evaluate_through(
    state: &AppState,
    jobs: &[ScheduledJob],
    book: &mut Bookkeeping,
    zone: &Zone,
    mut last: i64,
    wall: i64,
    db_path: Option<&str>,
) -> i64 {
    for epoch in last + 1..=wall.min(last.saturating_add(60)) {
        state.scheduling.progress.beat();
        if !evaluate_slot(state, jobs, book, zone, epoch, wall, db_path).await {
            break;
        }
        last = epoch;
        state.scheduling.progress.recovered();
    }
    last
}

/// False keeps this slot at the front of the next pass. Individual decisions already
/// completed in the slot are remembered if another job still needs admission.
async fn evaluate_slot(
    state: &AppState,
    jobs: &[ScheduledJob],
    book: &mut Bookkeeping,
    zone: &Zone,
    epoch: i64,
    wall: i64,
    db_path: Option<&str>,
) -> bool {
    let progress = &state.scheduling.progress;
    let now = zone.timestamp(epoch);
    let mut changed = progress
        .waiting("count_outcomes", count_outcomes(state, jobs, book, db_path))
        .await;
    if let Some(dbp) = db_path {
        progress
            .waiting("reset_failing", reset_failing(state, &now, jobs, book, dbp))
            .await;
    }
    // NOTE: benchmarks/scheduler_overhead/barca/run.sh's CI smoke greps stderr for
    // "scheduled run.*:probe" to count ticks independent of worker execution — keep
    // that substring ("scheduled run", plus the job id) if this wording changes.
    let mut retry = false;
    // CLI runs on the same project, read only on a second some job is due: one indexed
    // query of the `running` rows.
    let cli = match db_path {
        Some(dbp) if !due_jobs(&now, jobs).is_empty() => {
            let Ok(rows) = progress
                .bounded("alive_running_runs", db::alive_running_runs(dbp))
                .await
            else {
                return false;
            };
            rows
        }
        _ => Vec::new(),
    };
    let actions = plan_tick(
        &now,
        jobs,
        |job| {
            crate::schedule_view::active(state, &job.id, epoch)
                .is_some_and(|p| p.paused_at <= epoch)
        },
        |job| {
            run_in_flight_at(state, job, epoch, wall).or_else(|| cli_run_in_flight_for(&cli, job))
        },
        |job| book.failures(job),
    );
    if !actions.is_empty() {
        progress.due(epoch);
    }
    for action in actions {
        if state.lifecycle.is_draining() {
            return false;
        }
        progress.beat();
        let job = match &action {
            TickAction::Paused(j) | TickAction::Fire(j) => j,
            TickAction::Skip { job, .. } | TickAction::Failing { job, .. } => job,
        };
        let id = job.id.clone();
        if book
            .last_fired
            .get(&job.id)
            .is_some_and(|last| *last >= epoch)
        {
            continue;
        }
        match action {
            TickAction::Paused(job) => {
                record_skip(book, job, None, epoch, "scheduled run");
                if let Some(dbp) = db_path {
                    progress
                        .waiting(
                            "record_tick",
                            record_tick(dbp, job, epoch, db::TickDecision::Paused, None),
                        )
                        .await;
                }
            }
            TickAction::Skip { job, handle } => {
                record_skip(book, job, Some(&handle), epoch, "scheduled run");
                if let Some(dbp) = db_path {
                    progress
                        .waiting(
                            "record_tick",
                            record_tick(dbp, job, epoch, db::TickDecision::Skipped, Some(&handle)),
                        )
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
                if let Some(dbp) = db_path {
                    progress
                        .waiting(
                            "record_tick",
                            record_tick(dbp, job, epoch, db::TickDecision::Failing, None),
                        )
                        .await;
                }
            }
            TickAction::Fire(job) => {
                let Some(handle) = fire(state, job, epoch).await else {
                    retry = true;
                    continue;
                };
                barca_service::jsonlog::line(
                    barca_service::jsonlog::Level::Info,
                    Some(&job.id),
                    &format!("[barca] scheduled run {} → {handle}", job.id),
                );
                book.accepted(job, handle);
            }
        }
        book.last_fired.insert(id, epoch);
        changed = true;
    }
    if let Some(path) = db_path {
        for job in std::mem::take(&mut book.dirty_failures) {
            let streak = book.failures.get(&job).cloned().unwrap_or_default();
            persist_failures(path, &job, &streak).await;
        }
    }
    if changed {
        publish_registry(state, jobs, book);
        if let Some(dbp) = db_path {
            progress
                .waiting("trim_ticks", trim_ticks(dbp, book, epoch))
                .await;
        }
    }
    !retry
}
