//! Cron scheduler — the background task that gives `Freshness::Schedule(...)` teeth.
//!
//! `barca serve` parses `@asset(freshness=Schedule("0 5 * * *"))` into
//! `Freshness::Schedule(CronExpr)`, but nothing in the executor ever acts on it.
//! This module closes that gap: at startup it enumerates every scheduled node, and
//! then on each live cron match it triggers a run through the same
//! [`crate::handlers`] run pool the HTTP `/run` endpoints use — so scheduled
//! runs are bounded the same way and land in the `runs` history table for free.
//! A scheduled task reuses cached upstream assets; `POST /run/{task}` does not.
//!
//! Semantics: cron is evaluated in the configured timezone (`--timezone`, local
//! by default). On startup a job fires once if a tick elapsed while the daemon
//! was down (catch-up), then fires on each live cron match. Last-fired times are
//! persisted so catch-up survives restarts; runs execute through the bounded run
//! pool and are visible via `GET /schedule`.
//!
//! Jobs that fire together (due at the same tick, or caught up together at startup) and have a
//! step in common share one run over the union of their cones, so that step is computed once
//! (`barca_core::share` decides who shares). Sharing a run does not tie the jobs to each
//! other: each job is followed through the run's events ([`follow_run`]), and "its previous
//! run is still going" means its own step has not ended, whatever the rest of the run is doing.

use crate::handlers;
use crate::state::{AppState, JobStatus, RunResult, RunState, RunStatus};
use barca_core::commands::TargetStatus;
use barca_core::commands::{self, AssetSummary};
use barca_core::{CronExpr, Freshness, NodeKind, RunEvent, db};
use chrono::{DateTime, FixedOffset, Local, TimeZone, Timelike, Utc};
use croner::Cron;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Static description of one scheduled job, for `barca schedule` (no server).
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleInfo {
    pub id: String,
    pub cron: String,
    pub kind: NodeKind,
    /// Next fire time as unix epoch seconds (for programmatic use).
    pub next_fire: Option<i64>,
    /// Next fire time formatted in local time (for display).
    pub next_fire_local: Option<String>,
}

/// Enumerate scheduled jobs from source and compute each one's next fire time.
/// Pure static analysis — used by the `barca schedule` CLI, no running server.
pub async fn describe_schedule(files: &[String], python: &std::path::Path) -> Vec<ScheduleInfo> {
    let now = Local::now();
    collect_jobs(files, python)
        .await
        .iter()
        .map(|j| {
            let next = j.cron.find_next_occurrence(&now, false).ok();
            ScheduleInfo {
                id: j.id.clone(),
                cron: j.cron_str.clone(),
                kind: j.kind,
                next_fire: next.map(|t| t.timestamp()),
                next_fire_local: next.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            }
        })
        .collect()
}

/// A parsed cron job discovered from the DAG at startup.
struct ScheduledJob {
    /// Full node id (e.g. `pipeline.py:daily_report`), used verbatim as the run target.
    id: String,
    /// Node kind decides the trigger path: asset/sensor → `get`, task → `run`.
    kind: NodeKind,
    /// The original cron string, kept for logging.
    cron_str: String,
    /// Parsed cron (5-field minute-granular, or 6-field seconds-granular),
    /// evaluated in the scheduler's configured timezone.
    cron: Cron,
}

/// Enumerate every node whose freshness is `Schedule(cron)` and parse each cron.
/// A DAG-analysis failure disables the scheduler (returns empty); individual
/// invalid/empty cron strings are logged and skipped rather than aborting.
async fn collect_jobs(files: &[String], python: &std::path::Path) -> Vec<ScheduledJob> {
    match commands::list_assets(files, python).await {
        Ok(summaries) => jobs_from_summaries(summaries),
        Err(e) => {
            eprintln!("[barca] scheduler disabled: failed to analyze DAG: {e}");
            Vec::new()
        }
    }
}

/// Pure summary → job mapping (split out from [`collect_jobs`] so it is testable
/// without a Python interpreter). Drops entries whose cron fails to parse.
fn jobs_from_summaries(summaries: Vec<AssetSummary>) -> Vec<ScheduledJob> {
    let mut jobs = Vec::new();
    for s in summaries {
        let Freshness::Schedule(expr) = &s.freshness else {
            continue;
        };
        match CronExpr::parse(&expr.0) {
            Ok(cron) => jobs.push(ScheduledJob {
                id: s.id,
                kind: s.kind,
                cron_str: expr.0.clone(),
                cron,
            }),
            Err(e) => eprintln!(
                "[barca] skipping '{}': invalid cron {:?}: {e}",
                s.id, expr.0
            ),
        }
    }
    jobs
}

/// Pure eligibility check: which jobs fire at `now`? Split out so it can be
/// unit-tested against fixed timestamps without a running server or wall clock.
///
/// Matching is at whole-second resolution: the tick loop wakes just after each
/// second boundary, so we drop only the sub-millisecond remainder and match the
/// wall-clock second directly. A 6-field cron fires at its declared seconds; a
/// 5-field cron has its seconds field pinned to `0` by the parser, so it still
/// matches only at second `0` of a matching minute.
fn due_jobs<'a, Tz: TimeZone>(
    now: &DateTime<Tz>,
    jobs: &'a [ScheduledJob],
) -> Vec<&'a ScheduledJob> {
    let tick = now.with_nanosecond(0).unwrap_or_else(|| now.clone());
    jobs.iter()
        .filter(|j| j.cron.is_time_matching(&tick).unwrap_or(false))
        .collect()
}

/// Milliseconds from `now` until just after the next second boundary. A small
/// cushion guarantees the tick wakes with the sub-second component at ~`0`. Pure
/// so the boundary arithmetic can be unit-tested without sleeping.
fn millis_to_next_second<Tz: TimeZone>(now: &DateTime<Tz>) -> u64 {
    let elapsed_ms = now.nanosecond() as u64 / 1_000_000;
    1_000u64.saturating_sub(elapsed_ms) + 5
}

/// Sleep until just after the next second boundary in the scheduler's timezone.
async fn sleep_to_next_second(zone: &Zone) {
    tokio::time::sleep(Duration::from_millis(millis_to_next_second(&zone.now()))).await;
}

/// Resolved timezone that cron expressions are evaluated in.
enum Zone {
    Local,
    Utc,
    Named(chrono_tz::Tz),
}

impl Zone {
    /// Parse a `--timezone` value: `local` (default), `utc`, or an IANA name
    /// like `America/New_York`. Unknown names fall back to local with a warning.
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "local" => Zone::Local,
            "utc" => Zone::Utc,
            _ => match s.trim().parse::<chrono_tz::Tz>() {
                Ok(tz) => Zone::Named(tz),
                Err(_) => {
                    eprintln!("[barca] unknown timezone {s:?}, using local time");
                    Zone::Local
                }
            },
        }
    }

    /// The current instant in this zone as a fixed-offset datetime.
    fn now(&self) -> DateTime<FixedOffset> {
        match self {
            Zone::Local => Local::now().fixed_offset(),
            Zone::Utc => Utc::now().fixed_offset(),
            Zone::Named(tz) => Utc::now().with_timezone(tz).fixed_offset(),
        }
    }

    /// An epoch-seconds timestamp interpreted in this zone.
    fn timestamp(&self, secs: i64) -> DateTime<FixedOffset> {
        let dt = match self {
            Zone::Local => Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.fixed_offset()),
            Zone::Utc => Utc
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.fixed_offset()),
            Zone::Named(tz) => tz.timestamp_opt(secs, 0).single().map(|t| t.fixed_offset()),
        };
        dt.unwrap_or_else(|| self.now())
    }
}

/// What the scheduler decided to do with a due job on a given tick.
enum TickAction<'a> {
    /// Trigger a run of this job.
    Fire(&'a ScheduledJob),
    /// Skip because this job's previous run (`handle`) is still in flight.
    Skip {
        job: &'a ScheduledJob,
        handle: String,
    },
}

/// Decide, for a tick at `now`, which due jobs to fire and which to skip. The jobs one tick
/// fires ([`fired`]) are due together whatever their cron expressions: `0 5 * * *` and
/// `*/5 * * * *` coincide at 05:00, and can share a run there.
///
/// Pure: `running` reports, for a job id, the handle of that job's previous run when the job
/// itself is still going in it, so the overlap-skip logic is testable without a live server or
/// wall clock. A job is judged on its own: a job that shared its previous run with one that is
/// still running fires again once its own step has ended.
fn plan_tick<'a, Tz: TimeZone>(
    now: &DateTime<Tz>,
    jobs: &'a [ScheduledJob],
    running: impl Fn(&str) -> Option<String>,
) -> Vec<TickAction<'a>> {
    due_jobs(now, jobs)
        .into_iter()
        .map(|job| match running(&job.id) {
            Some(handle) => TickAction::Skip { job, handle },
            None => TickAction::Fire(job),
        })
        .collect()
}

/// The jobs a tick's plan fires: the ones due together, which share runs where they can.
fn fired<'a>(plan: &[TickAction<'a>]) -> Vec<&'a ScheduledJob> {
    plan.iter()
        .filter_map(|action| match action {
            TickAction::Fire(job) => Some(*job),
            TickAction::Skip { .. } => None,
        })
        .collect()
}

/// Whether a run is still pending or running.
fn still_going(status: RunStatus) -> bool {
    matches!(status, RunStatus::Pending | RunStatus::Running)
}

/// Whether a previously-issued run handle is still pending or running.
fn is_in_flight(state: &AppState, handle: &str) -> bool {
    state
        .runs
        .get(handle)
        .is_some_and(|r| still_going(r.status))
}

/// Trigger one run for jobs that share it (or for a job on its own) and return its handle.
///
/// A job firing alone takes the path for its kind: assets and sensors go through `get`, tasks
/// through `run`. Several jobs share one run over the union of their cones, so an upstream they
/// share is computed once.
///
/// A tick brings a node up to date, it does not force it: sensors upstream
/// are polled, anything whose inputs changed is recomputed, and an asset whose
/// inputs did not change is served from cache. A task itself always runs.
fn trigger(state: &AppState, due: &[&ScheduledJob]) -> String {
    match due {
        [job] => match job.kind {
            NodeKind::Task => handlers::start_scheduled_task(state.clone(), job.id.clone()),
            NodeKind::Asset | NodeKind::Sensor => {
                handlers::start_run(state.clone(), Some(job.id.clone()))
            }
        },
        many => handlers::start_scheduled_batch(
            state.clone(),
            many.iter().map(|j| j.id.clone()).collect(),
        ),
    }
}

/// Split the jobs that are due together into runs, given the groups of job ids that can share
/// one (`None`: nothing is shared, every job gets its own run, as before runs were shared).
/// A job no group names gets its own run too.
fn runs_for<'a>(
    due: &[&'a ScheduledJob],
    groups: Option<&[Vec<String>]>,
) -> Vec<Vec<&'a ScheduledJob>> {
    let alone = |job: &&'a ScheduledJob| vec![*job];
    let Some(groups) = groups else {
        return due.iter().map(alone).collect();
    };
    let mut runs: Vec<Vec<&ScheduledJob>> = groups
        .iter()
        .map(|group| {
            let in_group = |job: &&&ScheduledJob| group.contains(&job.id);
            due.iter().filter(in_group).copied().collect()
        })
        .filter(|run: &Vec<&ScheduledJob>| !run.is_empty())
        .collect();
    let grouped = |job: &&&ScheduledJob| groups.iter().any(|g| g.contains(&job.id));
    runs.extend(due.iter().filter(|job| !grouped(job)).map(alone));
    runs
}

/// Whether a scheduled tick elapsed between `last_fired` and `now` — i.e. the
/// next cron occurrence strictly after `last_fired` is already in the past. This
/// drives the single catch-up run after the daemon was down. Pure and generic
/// over timezone so it is testable and reusable once a `--timezone` is honored.
fn needs_catchup<Tz: TimeZone>(cron: &Cron, last_fired: &DateTime<Tz>, now: &DateTime<Tz>) -> bool {
    match cron.find_next_occurrence(last_fired, false) {
        Ok(next) => next <= *now,
        Err(_) => false,
    }
}

/// What to do with each job at startup.
struct CatchUp<'a> {
    /// Jobs that missed a tick while the server was down. They fire once, together, exactly as
    /// jobs due at the same tick do.
    fire: Vec<&'a ScheduledJob>,
    /// Jobs never seen before: anchored to now, not fired (no first-launch stampede).
    anchor: Vec<&'a ScheduledJob>,
}

/// Decide the startup catch-up. Pure: `last_fired` gives a job's persisted last fire time.
fn plan_catchup<'a, Tz: TimeZone>(
    now: &DateTime<Tz>,
    jobs: &'a [ScheduledJob],
    last_fired: impl Fn(&str) -> Option<DateTime<Tz>>,
) -> CatchUp<'a> {
    let mut plan = CatchUp {
        fire: Vec::new(),
        anchor: Vec::new(),
    };
    for job in jobs {
        match last_fired(&job.id) {
            Some(last) if needs_catchup(&job.cron, &last, now) => plan.fire.push(job),
            Some(_) => {}
            None => plan.anchor.push(job),
        }
    }
    plan
}

/// Record that `node_id` fired at `epoch` seconds. Best-effort durability.
async fn persist_fired(db_path: &str, node_id: &str, epoch: i64) {
    if let Err(e) = db::upsert_schedule_state(db_path, node_id, epoch).await {
        eprintln!("[barca] schedule_state write failed for {node_id}: {e}");
    }
}

/// A job's most recent scheduled run, and how the job itself is doing in it.
#[derive(Clone, Debug, PartialEq)]
struct JobRun {
    /// The run's handle. Jobs fired together have the same one.
    handle: String,
    /// `Pending` or `Running` until the job's own step has ended, then how the job ended.
    status: RunStatus,
}

/// What the scheduler remembers about one job between ticks.
#[derive(Clone, Debug, Default)]
struct JobRecord {
    last_fired: Option<i64>,
    last_run: Option<JobRun>,
}

/// The scheduler's memory, by job id. Shared with the tasks that follow each run. Entries for
/// jobs removed on reload are harmless.
#[derive(Clone, Default)]
struct Ledger(Arc<Mutex<HashMap<String, JobRecord>>>);

impl Ledger {
    fn get(&self, job_id: &str) -> JobRecord {
        let records = self.0.lock().unwrap();
        records.get(job_id).cloned().unwrap_or_default()
    }

    fn set_fired(&self, job_id: &str, epoch: i64) {
        let mut records = self.0.lock().unwrap();
        records.entry(job_id.to_string()).or_default().last_fired = Some(epoch);
    }

    /// `job_id` was just fired into the run `handle`, which has not started yet.
    fn set_run(&self, job_id: &str, handle: &str) {
        let mut records = self.0.lock().unwrap();
        records.entry(job_id.to_string()).or_default().last_run = Some(JobRun {
            handle: handle.to_string(),
            status: RunStatus::Pending,
        });
    }

    /// Change how `job_id` is doing in the run `handle`, through `change(current status)`.
    /// Does nothing when the job's most recent run is a newer one. Returns the new status when
    /// it changed.
    fn update(
        &self,
        job_id: &str,
        handle: &str,
        change: impl FnOnce(RunStatus) -> RunStatus,
    ) -> Option<RunStatus> {
        let mut records = self.0.lock().unwrap();
        let run = records.get_mut(job_id)?.last_run.as_mut()?;
        if run.handle != handle {
            return None;
        }
        let next = change(run.status);
        (next != run.status).then(|| {
            run.status = next;
            next
        })
    }
}

/// How a job ended in a run that has finished.
///
/// A run over several jobs that ran to its end says how each one ended. Otherwise the run has
/// one status: it is the job's too, when the job had the run to itself or its own step had not
/// ended (the run was cancelled, timed out or failed before it could). A job whose step had
/// ended keeps how that went: stopping the run afterwards does not undo it.
fn job_outcome(run: &RunState, job_id: &str, so_far: RunStatus, alone: bool) -> RunStatus {
    if let Some(RunResult::Multi(result)) = &run.result
        && let Some(target) = result.target(job_id)
    {
        return match target.status {
            TargetStatus::Success => RunStatus::Complete,
            TargetStatus::Failed => RunStatus::Failed,
        };
    }
    if alone || still_going(so_far) {
        run.status
    } else {
        so_far
    }
}

/// Apply one event of the run `handle` to the jobs fired into it. Returns true once the run
/// has finished. Applying an event twice changes nothing, so a replay is safe.
fn apply_event(
    state: &AppState,
    ledger: &Ledger,
    handle: &str,
    jobs: &[String],
    event: &RunEvent,
) -> bool {
    let set = |job_id: &str, change: &dyn Fn(RunStatus) -> RunStatus| {
        if let Some(status) = ledger.update(job_id, handle, change) {
            publish_status(state, job_id, handle, status);
        }
    };
    match event {
        RunEvent::RunStarted { .. } => {
            for job_id in jobs {
                set(job_id, &|so_far| match so_far {
                    RunStatus::Pending => RunStatus::Running,
                    other => other,
                });
            }
            false
        }
        // The job's own step has ended: from here on the job is not "still running", even
        // though the run goes on for the jobs it was fired with.
        RunEvent::TargetFinished { node_id, ok } if jobs.contains(node_id) => {
            let ended = if *ok {
                RunStatus::Complete
            } else {
                RunStatus::Failed
            };
            set(node_id, &|_| ended);
            false
        }
        RunEvent::RunFinished { .. } => {
            // The run's final state is in place before `RunFinished` is emitted.
            if let Some(run) = state.runs.get(handle) {
                for job_id in jobs {
                    set(job_id, &|so_far| {
                        job_outcome(&run, job_id, so_far, jobs.len() == 1)
                    });
                }
            }
            true
        }
        _ => false,
    }
}

/// Follow the run `handle` to its end, keeping the status of each job fired into it up to date
/// in the ledger and in the published `GET /schedule` view.
///
/// This is how the scheduler knows when a job's own step has ended inside a run it shares with
/// other jobs: the run reports each target as it finishes ([`RunEvent::TargetFinished`]). The
/// scheduler is an ordinary subscriber of the run's event channel, the one `GET /events` reads.
async fn follow_run(state: AppState, ledger: Ledger, handle: String, jobs: Vec<String>) {
    let Some(channel) = state.events.get(&handle).map(|c| c.clone()) else {
        return;
    };
    // Events emitted before this task started are in the backlog.
    let (mut pending, mut live) = channel.snapshot_and_subscribe();
    loop {
        for event in pending.drain(..) {
            if apply_event(&state, &ledger, &handle, &jobs, &event) {
                return;
            }
        }
        match live.recv().await {
            Ok(event) => pending.push(event),
            // A chatty run outpaced this subscriber: start over from the backlog, which holds
            // every event.
            Err(RecvError::Lagged(_)) => (pending, live) = channel.snapshot_and_subscribe(),
            Err(RecvError::Closed) => return,
        }
    }
}

/// Set one job's status in the published `GET /schedule` view, if that view still shows the
/// run `handle` for it.
fn publish_status(state: &AppState, job_id: &str, handle: &str, status: RunStatus) {
    if let Ok(mut published) = state.schedule.write()
        && let Some(job) = published
            .iter_mut()
            .find(|j| j.id == job_id && j.last_handle.as_deref() == Some(handle))
    {
        job.last_status = Some(status);
    }
}

/// The DAG of the server's files, for deciding which jobs share a run. `None` (with a note on
/// stderr) when it cannot be read; every job then runs on its own.
async fn read_dag(state: &AppState) -> Option<barca_core::Dag> {
    match commands::build_dag(&state.config.files, &state.config.python).await {
        Ok(dag) => Some(dag),
        Err(e) => {
            eprintln!("[barca] scheduler: due jobs will not share runs: {e}");
            None
        }
    }
}

/// Log the current schedule and each job's next fire time.
fn log_schedule(jobs: &[ScheduledJob], zone: &Zone) {
    if jobs.is_empty() {
        eprintln!("[barca] no scheduled assets yet (watching for changes)");
        return;
    }
    eprintln!(
        "[barca] scheduling {} asset{}:",
        jobs.len(),
        if jobs.len() == 1 { "" } else { "s" }
    );
    let now = zone.now();
    for job in jobs {
        let next = job
            .cron
            .find_next_occurrence(&now, false)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|_| "?".to_string());
        eprintln!("  {} — {} (next {})", job.id, job.cron_str, next);
    }
}

/// The scheduler: the job set, what it remembers about each job, and where it persists fire
/// times. `run_scheduler` drives it from the wall clock; tests call [`Scheduler::catch_up`] and
/// [`Scheduler::tick`] with a time of their choosing.
struct Scheduler {
    state: AppState,
    zone: Zone,
    jobs: Vec<ScheduledJob>,
    /// The DAG the job set was read from, for deciding which due jobs share a run. `None` when
    /// it could not be read: then nothing is shared.
    dag: Option<barca_core::Dag>,
    /// The metadata DB that holds each job's last fire time. `None`: durability is disabled,
    /// so there is no catch-up.
    db_path: Option<String>,
    ledger: Ledger,
    /// What [`Scheduler::explain_once`] has already said.
    explained: Mutex<std::collections::HashSet<(String, String)>>,
}

impl Scheduler {
    /// Read the job set from source and open the metadata DB (same `.barca` a CLI run uses).
    async fn load(state: AppState) -> Self {
        let zone = Zone::parse(&state.config.timezone);
        let jobs = collect_jobs(&state.config.files, &state.config.python).await;
        let dag = read_dag(&state).await;
        let db_path = match db::ensure_env_dirs(&state.config.resolved.env) {
            Ok(_) => {
                let path = state.config.resolved.db_path.clone();
                let _ = db::init_db(&path).await;
                Some(path)
            }
            Err(_) => {
                eprintln!("[barca] scheduler: durability disabled (no metadata db)");
                None
            }
        };
        Self {
            state,
            zone,
            jobs,
            dag,
            db_path,
            ledger: Ledger::default(),
            explained: Mutex::default(),
        }
    }

    /// `--watch`: re-read the job set after a source file changed. The parse itself runs on
    /// the blocking pool inside `commands::list_assets`.
    async fn reload(&mut self) {
        self.jobs = collect_jobs(&self.state.config.files, &self.state.config.python).await;
        self.dag = read_dag(&self.state).await;
        self.explained.lock().unwrap().clear();
        eprintln!("[barca] schedule reloaded: {} job(s)", self.jobs.len());
        log_schedule(&self.jobs, &self.zone);
        self.publish();
    }

    /// The handle of `job_id`'s previous run, when the job itself is still going in it: its own
    /// step has not ended and the run is still pending or running.
    fn running(&self, job_id: &str) -> Option<String> {
        self.ledger
            .get(job_id)
            .last_run
            .filter(|run| still_going(run.status) && is_in_flight(&self.state, &run.handle))
            .map(|run| run.handle)
    }

    /// Startup: fire once, together, the jobs whose scheduled tick elapsed while the server
    /// was down, and anchor jobs with no prior record to `now`. Requires durability; does
    /// nothing if the DB is unavailable.
    async fn catch_up(&self, now: DateTime<FixedOffset>) {
        let Some(db_path) = &self.db_path else {
            return;
        };
        let saved = db::get_schedule_state(db_path).await.unwrap_or_default();
        for (job_id, epoch) in &saved {
            self.ledger.set_fired(job_id, *epoch);
        }
        let plan = plan_catchup(&now, &self.jobs, |job_id| {
            saved.get(job_id).map(|epoch| self.zone.timestamp(*epoch))
        });
        for job in &plan.anchor {
            self.ledger.set_fired(&job.id, now.timestamp());
            persist_fired(db_path, &job.id, now.timestamp()).await;
        }
        self.fire(&plan.fire, now.timestamp(), "catch-up run").await;
    }

    /// One tick at `now`: fire, together, every due job that is not still going in its
    /// previous run.
    async fn tick(&self, now: DateTime<FixedOffset>) {
        let plan = plan_tick(&now, &self.jobs, |job_id| self.running(job_id));
        for action in &plan {
            if let TickAction::Skip { job, handle } = action {
                eprintln!(
                    "[barca] scheduled run {} skipped — previous run {handle} still in flight",
                    job.id
                );
            }
        }
        self.fire(&fired(&plan), now.timestamp(), "scheduled run")
            .await;
    }

    /// Fire the jobs that are due together, sharing runs where [`Scheduler::groups`] says to.
    async fn fire(&self, due: &[&ScheduledJob], fired_at: i64, what: &str) {
        let groups = self.groups(due);
        for jobs in runs_for(due, groups.as_deref()) {
            self.start_run(&jobs, fired_at, what).await;
        }
    }

    /// Which of the jobs due together share a run: the ones with a step in common that
    /// sharing holds none of back (`barca_core::share`). Jobs with nothing in common gain
    /// nothing from one run, and would only be tied to each other's timing, failures and
    /// cancellation, so they stay apart. `None`: every job gets its own run.
    ///
    /// Decided from the DAG read with the job set, so a tick reads no source file.
    ///
    /// Nothing is shared when artifacts go to a remote store. There a step is recorded only
    /// when its run ends (once its upload is confirmed), so a job could not be told apart from
    /// the run it is in: it would wait for the slowest job it fired with before its next tick
    /// could fire. Keeping every job's ticks independent comes first, so such a server keeps
    /// one run per job (and a step upstream of two jobs due together may be computed by both).
    fn groups(&self, due: &[&ScheduledJob]) -> Option<Vec<Vec<String>>> {
        if due.len() < 2 || self.state.config.resolved.remote_artifacts() {
            return None;
        }
        let dag = self.dag.as_ref()?;
        let ids: Vec<String> = due.iter().map(|job| job.id.clone()).collect();
        let sharing = barca_core::share::shared_run_groups(dag, &ids);
        for out in &sharing.left_out {
            self.explain_once(out);
        }
        Some(sharing.groups)
    }

    /// Say on stderr, once per job and reason, why a job that has a step in common with
    /// others still runs on its own. Not part of any contract: it is there so the answer to
    /// "why was this computed twice" can be read off the server's log.
    fn explain_once(&self, out: &barca_core::share::LeftOut) {
        let reason = (out.target.clone(), out.waits_for.clone());
        if self.explained.lock().unwrap().insert(reason) {
            eprintln!(
                "[barca] {} runs on its own, not in one run with {}: there it would wait for \
                 {}, which it does not depend on (steps they have in common may run once per run)",
                out.target,
                out.with.join(", "),
                out.waits_for
            );
        }
    }

    /// Start one run for `jobs`, record it for each of them, and follow it.
    ///
    /// NOTE: benchmarks/scheduler_overhead/barca/run.sh's CI smoke greps stderr for
    /// "scheduled run.*:probe" to count ticks independent of worker execution — keep
    /// that substring ("scheduled run", plus the job id) if this wording changes.
    async fn start_run(&self, jobs: &[&ScheduledJob], fired_at: i64, what: &str) {
        let handle = trigger(&self.state, jobs);
        for job in jobs {
            eprintln!("[barca] {what} {} → {handle}", job.id);
            self.ledger.set_fired(&job.id, fired_at);
            self.ledger.set_run(&job.id, &handle);
            if let Some(db_path) = &self.db_path {
                persist_fired(db_path, &job.id, fired_at).await;
            }
        }
        self.publish();
        tokio::spawn(follow_run(
            self.state.clone(),
            self.ledger.clone(),
            handle,
            jobs.iter().map(|job| job.id.clone()).collect(),
        ));
    }

    /// Publish the current job set and what is remembered about each job into shared state,
    /// for `GET /schedule` to read.
    fn publish(&self) {
        // The view is locked before the ledger is read, so a status a follower sets in between
        // is not overwritten by an older one.
        let Ok(mut published) = self.state.schedule.write() else {
            return;
        };
        *published = self
            .jobs
            .iter()
            .map(|j| {
                let record = self.ledger.get(&j.id);
                JobStatus {
                    id: j.id.clone(),
                    cron: j.cron_str.clone(),
                    kind: j.kind,
                    last_fired: record.last_fired,
                    last_handle: record.last_run.as_ref().map(|run| run.handle.clone()),
                    last_status: record.last_run.as_ref().map(|run| run.status),
                }
            })
            .collect();
    }
}

/// The scheduler background task. Spawned from `serve_async` when scheduling is
/// enabled; runs for the lifetime of the server.
pub async fn run_scheduler(state: AppState) {
    let mut scheduler = Scheduler::load(state.clone()).await;

    if scheduler.jobs.is_empty() && !state.config.watch {
        eprintln!("[barca] no scheduled assets — scheduler idle");
        return;
    }
    log_schedule(&scheduler.jobs, &scheduler.zone);

    scheduler.catch_up(scheduler.zone.now()).await;
    scheduler.publish();

    let mut seen_gen = state.dag_generation.load(Ordering::Relaxed);

    loop {
        sleep_to_next_second(&scheduler.zone).await;

        // `--watch`: re-read the job set when a source file changed.
        let current_gen = state.dag_generation.load(Ordering::Relaxed);
        if current_gen != seen_gen {
            seen_gen = current_gen;
            scheduler.reload().await;
        }

        scheduler.tick(scheduler.zone.now()).await;
    }
}

#[cfg(all(test, unix))]
mod batch_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RunState, ServeConfig};
    use barca_core::{CronExpr, Freshness, NodeKind};
    use std::net::{IpAddr, Ipv4Addr};

    fn summary(id: &str, kind: NodeKind, cron: &str) -> AssetSummary {
        AssetSummary {
            id: id.to_string(),
            kind,
            freshness: Freshness::Schedule(CronExpr(cron.to_string())),
            inputs: vec![],
            env: vec![],
        }
    }

    /// Build a single `ScheduledJob` through the real parse path.
    fn job(id: &str, kind: NodeKind, cron: &str) -> ScheduledJob {
        jobs_from_summaries(vec![summary(id, kind, cron)])
            .pop()
            .expect("valid cron")
    }

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 7, 2, hour, minute, 0)
            .single()
            .unwrap()
    }

    fn at_s(hour: u32, minute: u32, sec: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 7, 2, hour, minute, sec)
            .single()
            .unwrap()
    }

    /// A minimal `AppState`. `files` points at a nonexistent path so any run this
    /// triggers fails fast in the background (DAG read error) with no side effects.
    fn app_state() -> AppState {
        AppState::new(ServeConfig {
            files: vec!["/nonexistent-barca-test-file.py".to_string()],
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            watch: false,
            schedule: true,
            timezone: "local".to_string(),
            python: std::path::PathBuf::from("python3"),
            resolved: barca_core::config::resolve_in(None, std::path::Path::new("/nonexistent"))
                .unwrap(),
            read_only: false,
        })
    }

    fn insert_run(state: &AppState, handle: &str, status: RunStatus) {
        state.runs.insert(
            handle.to_string(),
            RunState {
                handle: handle.to_string(),
                status,
                result: None,
                error: None,
                started_at: 0.0,
                finished_at: None,
                cancel: barca_core::CancellationToken::new(),
            },
        );
    }

    #[test]
    fn invalid_and_empty_crons_are_dropped() {
        let summaries = vec![
            summary("f.py:good", NodeKind::Asset, "0 5 * * *"),
            summary("f.py:empty", NodeKind::Asset, ""),
            summary("f.py:garbage", NodeKind::Asset, "not a cron"),
        ];
        let jobs = jobs_from_summaries(summaries);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "f.py:good");
        assert_eq!(jobs[0].kind, NodeKind::Asset);
    }

    #[test]
    fn non_scheduled_freshness_is_ignored() {
        let summaries = vec![AssetSummary {
            id: "f.py:always".into(),
            kind: NodeKind::Asset,
            freshness: Freshness::Always,
            inputs: vec![],
            env: vec![],
        }];
        assert!(jobs_from_summaries(summaries).is_empty());
    }

    #[test]
    fn daily_cron_fires_only_at_its_minute() {
        let jobs = jobs_from_summaries(vec![summary("f.py:daily", NodeKind::Asset, "0 5 * * *")]);
        assert_eq!(due_jobs(&at(5, 0), &jobs).len(), 1, "05:00 should fire");
        assert!(
            due_jobs(&at(5, 1), &jobs).is_empty(),
            "05:01 should not fire"
        );
        assert!(
            due_jobs(&at(6, 0), &jobs).is_empty(),
            "06:00 should not fire"
        );
    }

    #[test]
    fn step_cron_fires_on_multiples() {
        let jobs = jobs_from_summaries(vec![summary("f.py:poll", NodeKind::Sensor, "*/5 * * * *")]);
        assert_eq!(due_jobs(&at(5, 0), &jobs).len(), 1);
        assert_eq!(due_jobs(&at(5, 5), &jobs).len(), 1);
        assert!(due_jobs(&at(5, 3), &jobs).is_empty());
    }

    #[test]
    fn five_field_cron_pinned_to_second_zero() {
        // A 5-field cron has its seconds field pinned to `0` by the parser, so it
        // matches only at second 0 of its minute — NOT at other seconds. This is
        // the regression guard that the per-second tick loop never fires a
        // 5-field job 60× within its matching minute.
        let jobs = jobs_from_summaries(vec![summary("f.py:daily", NodeKind::Asset, "0 5 * * *")]);
        assert_eq!(due_jobs(&at_s(5, 0, 0), &jobs).len(), 1, "05:00:00 fires");
        assert!(
            due_jobs(&at_s(5, 0, 30), &jobs).is_empty(),
            "05:00:30 does not fire"
        );
        assert!(
            due_jobs(&at_s(5, 0, 42), &jobs).is_empty(),
            "05:00:42 does not fire"
        );
    }

    #[test]
    fn six_field_seconds_cron_parses() {
        // The shared parse helper must accept a 6-field cron in the *execution*
        // path (jobs_from_summaries), not just in CronExpr::validate — this was
        // rejected at parse time before sub-minute support (issue #109).
        let jobs = jobs_from_summaries(vec![summary("f.py:tick", NodeKind::Task, "*/5 * * * * *")]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, NodeKind::Task);
    }

    #[test]
    fn six_field_cron_fires_on_second_multiples() {
        // A 6-field cron fires at its declared seconds cadence, not once a minute.
        let jobs = jobs_from_summaries(vec![summary("f.py:tick", NodeKind::Task, "*/5 * * * * *")]);
        assert_eq!(due_jobs(&at_s(5, 0, 0), &jobs).len(), 1, ":00 fires");
        assert_eq!(due_jobs(&at_s(5, 0, 5), &jobs).len(), 1, ":05 fires");
        assert!(
            due_jobs(&at_s(5, 0, 3), &jobs).is_empty(),
            ":03 does not fire"
        );
    }

    // ─── catch-up detection ────────────────────────────────────────────────

    #[test]
    fn needs_catchup_true_when_a_tick_was_missed() {
        let j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        // Last fired yesterday 05:00; now is today 06:00 → today's 05:00 was missed.
        let last = at(5, 0) - chrono::Duration::days(1);
        assert!(needs_catchup(&j.cron, &last, &at(6, 0)));
    }

    #[test]
    fn needs_catchup_false_when_no_tick_elapsed() {
        let j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        // Fired at today 05:00; now 05:30 — next occurrence is tomorrow, not past.
        let last = at(5, 0);
        let now = Local
            .with_ymd_and_hms(2026, 7, 2, 5, 30, 0)
            .single()
            .unwrap();
        assert!(!needs_catchup(&j.cron, &last, &now));
    }

    #[test]
    fn needs_catchup_true_after_long_downtime() {
        // A */5 job down for an hour: a tick is certainly in the past → catch up once.
        let j = job("f.py:poll", NodeKind::Sensor, "*/5 * * * *");
        let last = at(5, 0);
        assert!(needs_catchup(&j.cron, &last, &at(6, 0)));
    }

    // ─── timezone handling ─────────────────────────────────────────────────

    #[test]
    fn zone_parse_handles_local_utc_named_and_unknown() {
        assert!(matches!(Zone::parse("local"), Zone::Local));
        assert!(matches!(Zone::parse(""), Zone::Local));
        assert!(matches!(Zone::parse("UTC"), Zone::Utc));
        assert!(matches!(Zone::parse("America/New_York"), Zone::Named(_)));
        // Unknown names fall back to local rather than erroring.
        assert!(matches!(Zone::parse("Not/AZone"), Zone::Local));
    }

    #[test]
    fn zone_timestamp_round_trips_epoch() {
        assert_eq!(Zone::Utc.timestamp(0).timestamp(), 0);
        assert_eq!(Zone::Local.timestamp(1_000).timestamp(), 1_000);
    }

    #[test]
    fn due_jobs_works_in_a_fixed_offset_zone() {
        use chrono::FixedOffset;
        // Matching must work for the DateTime<FixedOffset> the scheduler actually
        // uses in production (not just the Local type the other tests exercise).
        let jobs = jobs_from_summaries(vec![summary("f.py:daily", NodeKind::Asset, "0 5 * * *")]);
        let tz = FixedOffset::east_opt(5 * 3600).unwrap();
        let at_five = tz.with_ymd_and_hms(2026, 7, 2, 5, 0, 0).single().unwrap();
        assert_eq!(due_jobs(&at_five, &jobs).len(), 1);
        let at_six = tz.with_ymd_and_hms(2026, 7, 2, 6, 0, 0).single().unwrap();
        assert!(due_jobs(&at_six, &jobs).is_empty());
    }

    #[test]
    fn scheduled_task_keeps_task_kind() {
        // Discovery must preserve kind so `trigger` can route tasks to the run path.
        let jobs = jobs_from_summaries(vec![summary(
            "f.py:cleanup",
            NodeKind::Task,
            "*/10 * * * *",
        )]);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, NodeKind::Task);
    }

    // ─── second-boundary arithmetic ────────────────────────────────────────

    #[test]
    fn millis_to_next_second_from_boundary_and_mid() {
        let with_nanos = |nanos: u32| {
            Local
                .with_ymd_and_hms(2026, 7, 2, 5, 0, 0)
                .single()
                .unwrap()
                .with_nanosecond(nanos)
                .unwrap()
        };
        assert_eq!(millis_to_next_second(&with_nanos(0)), 1_005);
        assert_eq!(millis_to_next_second(&with_nanos(500_000_000)), 505);
        assert_eq!(millis_to_next_second(&with_nanos(999_000_000)), 6);
    }

    // ─── in-flight detection ───────────────────────────────────────────────

    #[test]
    fn is_in_flight_only_for_pending_or_running() {
        let st = app_state();
        insert_run(&st, "pending", RunStatus::Pending);
        insert_run(&st, "running", RunStatus::Running);
        insert_run(&st, "complete", RunStatus::Complete);
        insert_run(&st, "failed", RunStatus::Failed);
        assert!(is_in_flight(&st, "pending"));
        assert!(is_in_flight(&st, "running"));
        assert!(!is_in_flight(&st, "complete"));
        assert!(!is_in_flight(&st, "failed"));
        assert!(
            !is_in_flight(&st, "missing"),
            "unknown handle is not in flight"
        );
    }

    // ─── per-tick planning (the overlap-skip guard) ────────────────────────

    fn fired_ids(plan: &[TickAction]) -> Vec<String> {
        fired(plan).iter().map(|j| j.id.clone()).collect()
    }

    /// A `running` predicate for [`plan_tick`]: these jobs are still going, each in its run.
    fn running(jobs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let jobs: HashMap<String, String> = jobs
            .iter()
            .map(|(id, handle)| (id.to_string(), handle.to_string()))
            .collect();
        move |job_id| jobs.get(job_id).cloned()
    }

    fn skipped_ids(plan: &[TickAction]) -> Vec<String> {
        plan.iter()
            .filter_map(|a| match a {
                TickAction::Skip { job, .. } => Some(job.id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plan_tick_fires_when_no_prior_run() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(5, 0), &jobs, |_| None);
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    }

    #[test]
    fn plan_tick_skips_when_prior_run_in_flight() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(5, 0), &jobs, running(&[("f.py:daily", "h1")]));
        assert!(fired_ids(&plan).is_empty());
        assert_eq!(skipped_ids(&plan), vec!["f.py:daily"]);
        // The skip carries the offending handle for the log line.
        match &plan[0] {
            TickAction::Skip { handle, .. } => assert_eq!(handle, "h1"),
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn plan_tick_fires_again_once_prior_run_finished() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        // A run was recorded for the job, but it is no longer in flight (completed/evicted).
        let plan = plan_tick(&at(5, 0), &jobs, |_| None);
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    }

    #[test]
    fn plan_tick_ignores_jobs_not_due_this_minute() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(6, 0), &jobs, |_| None);
        assert!(plan.is_empty(), "06:00 is not the job's minute");
    }

    #[test]
    fn plan_tick_mixes_fire_and_skip_across_jobs() {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:b", NodeKind::Task, "0 5 * * *"),
        ];
        // `a` has a run still going; `b` has never run.
        let plan = plan_tick(&at(5, 0), &jobs, running(&[("f.py:a", "ha")]));
        assert_eq!(skipped_ids(&plan), vec!["f.py:a"]);
        assert_eq!(fired_ids(&plan), vec!["f.py:b"]);
    }

    // ─── jobs that fire together ───────────────────────────────────────────

    #[test]
    fn jobs_due_at_the_same_tick_fire_together() {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:t", NodeKind::Task, "0 5 * * *"),
            job("f.py:s", NodeKind::Sensor, "0 5 * * *"),
        ];
        let plan = plan_tick(&at(5, 0), &jobs, |_| None);
        // Due together, whatever their kind.
        assert_eq!(fired_ids(&plan), vec!["f.py:a", "f.py:t", "f.py:s"]);
        assert!(skipped_ids(&plan).is_empty());
    }

    #[test]
    fn differing_crons_fire_together_only_when_they_coincide() {
        let jobs = vec![
            job("f.py:daily", NodeKind::Asset, "0 5 * * *"),
            job("f.py:poll", NodeKind::Task, "*/5 * * * *"),
            job("f.py:seconds", NodeKind::Task, "*/30 * * * * *"),
        ];
        // 05:00:00 matches all three expressions.
        let together = plan_tick(&at(5, 0), &jobs, |_| None);
        assert_eq!(
            fired_ids(&together),
            vec!["f.py:daily", "f.py:poll", "f.py:seconds"]
        );
        // 05:05:00: the daily job is not due.
        let later = plan_tick(&at(5, 5), &jobs, |_| None);
        assert_eq!(fired_ids(&later), vec!["f.py:poll", "f.py:seconds"]);
        // 05:05:30: only the seconds-granular job.
        let half = plan_tick(&at_s(5, 5, 30), &jobs, |_| None);
        assert_eq!(fired_ids(&half), vec!["f.py:seconds"]);
    }

    #[test]
    fn a_job_still_running_is_left_out_and_the_rest_fire_together() {
        let jobs = vec![
            job("f.py:fast", NodeKind::Task, "* * * * * *"),
            job("f.py:slow", NodeKind::Task, "* * * * * *"),
            job("f.py:other", NodeKind::Asset, "* * * * * *"),
        ];
        // All three shared run `h1`; only `slow` is still going in it.
        let plan = plan_tick(&at_s(5, 0, 1), &jobs, running(&[("f.py:slow", "h1")]));
        assert_eq!(fired_ids(&plan), vec!["f.py:fast", "f.py:other"]);
        assert_eq!(skipped_ids(&plan), vec!["f.py:slow"]);
    }

    #[test]
    fn due_jobs_are_split_into_runs_by_the_groups_that_can_share_one() {
        let jobs = [
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:t", NodeKind::Task, "0 5 * * *"),
            job("f.py:other", NodeKind::Task, "0 5 * * *"),
        ];
        let due: Vec<&ScheduledJob> = jobs.iter().collect();
        let ids = |runs: Vec<Vec<&ScheduledJob>>| -> Vec<Vec<String>> {
            runs.iter()
                .map(|run| run.iter().map(|j| j.id.clone()).collect())
                .collect()
        };
        let group = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();

        // `a` and `t` have a step in common; `other` has nothing in common with them.
        let groups = [group(&["f.py:a", "f.py:t"]), group(&["f.py:other"])];
        assert_eq!(
            ids(runs_for(&due, Some(&groups))),
            vec![vec!["f.py:a", "f.py:t"], vec!["f.py:other"]]
        );
        // Nothing can be shared: one run per job, as before runs were shared.
        assert_eq!(
            ids(runs_for(&due, None)),
            vec![vec!["f.py:a"], vec!["f.py:t"], vec!["f.py:other"]]
        );
        // A job the grouping does not mention still runs, on its own.
        let partial = [group(&["f.py:a", "f.py:t"])];
        assert_eq!(
            ids(runs_for(&due, Some(&partial))),
            vec![vec!["f.py:a", "f.py:t"], vec!["f.py:other"]]
        );
        assert!(runs_for(&[], Some(&groups)).is_empty(), "nothing due");
    }

    /// A DAG in which `a` and `t` read `base`.
    fn shared_dag() -> barca_core::Dag {
        let source = concat!(
            "from barca import asset, task\n\n",
            "@asset()\ndef base() -> dict:\n    return {}\n\n",
            "@asset(inputs={\"base\": base})\ndef a(base: dict) -> dict:\n    return {}\n\n",
            "@task(inputs={\"base\": base})\ndef t(base: dict) -> None:\n    pass\n",
        );
        let nodes = barca_core::parse::extract_nodes(source, "f.py").unwrap();
        barca_core::Dag::build(&nodes).unwrap()
    }

    fn two_jobs_on_one_upstream(state: &AppState) -> Scheduler {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "* * * * * *"),
            job("f.py:t", NodeKind::Task, "* * * * * *"),
        ];
        let mut sched = scheduler(state, jobs);
        sched.dag = Some(shared_dag());
        sched
    }

    #[test]
    fn jobs_with_a_step_in_common_are_grouped_from_the_dag_read_with_the_job_set() {
        let st = app_state();
        let sched = two_jobs_on_one_upstream(&st);
        let due: Vec<&ScheduledJob> = sched.jobs.iter().collect();
        // No source file is read at a tick (the state's file does not even exist).
        assert_eq!(
            sched.groups(&due),
            Some(vec![vec!["f.py:a".to_string(), "f.py:t".to_string()]])
        );
        assert_eq!(
            sched.groups(&due[..1]),
            None,
            "one due job needs no grouping"
        );
    }

    #[test]
    fn nothing_is_shared_with_a_remote_artifact_store() {
        let mut st = app_state();
        let mut config = (*st.config).clone();
        // A store that is not the local artifact dir.
        config.resolved.artifact_root = "/nonexistent-barca-test-store".to_string();
        assert!(config.resolved.remote_artifacts());
        st.config = std::sync::Arc::new(config);
        let sched = two_jobs_on_one_upstream(&st);
        let due: Vec<&ScheduledJob> = sched.jobs.iter().collect();
        assert_eq!(sched.groups(&due), None, "one run per job");
    }

    #[test]
    fn nothing_is_shared_when_the_dag_could_not_be_read() {
        let st = app_state();
        let mut sched = two_jobs_on_one_upstream(&st);
        sched.dag = None;
        let due: Vec<&ScheduledJob> = sched.jobs.iter().collect();
        assert_eq!(sched.groups(&due), None);
    }

    // ─── startup catch-up ──────────────────────────────────────────────────

    #[test]
    fn catch_up_fires_together_the_jobs_that_missed_a_tick() {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:t", NodeKind::Task, "0 5 * * *"),
            job("f.py:current", NodeKind::Task, "0 5 * * *"),
            job("f.py:new", NodeKind::Asset, "0 5 * * *"),
        ];
        let yesterday = at(5, 0) - chrono::Duration::days(1);
        let plan = plan_catchup(&at(6, 0), &jobs, |job_id| match job_id {
            // Fired yesterday at 05:00: today's 05:00 was missed.
            "f.py:a" | "f.py:t" => Some(yesterday),
            // Fired today at 05:00: nothing missed.
            "f.py:current" => Some(at(5, 0)),
            // Never seen before.
            _ => None,
        });
        let ids = |jobs: &[&ScheduledJob]| jobs.iter().map(|j| j.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(&plan.fire),
            vec!["f.py:a", "f.py:t"],
            "the two that missed a tick are fired together"
        );
        assert_eq!(ids(&plan.anchor), vec!["f.py:new"], "anchored, not fired");
    }

    #[test]
    fn catch_up_fires_nothing_when_no_tick_was_missed() {
        let jobs = vec![job("f.py:a", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_catchup(&at(5, 30), &jobs, |_| Some(at(5, 0)));
        assert!(plan.fire.is_empty());
        assert!(plan.anchor.is_empty());
    }

    // ─── each job's own status in a shared run ─────────────────────────────

    fn run_state(status: RunStatus, result: Option<RunResult>) -> RunState {
        RunState {
            handle: "h1".to_string(),
            status,
            result,
            error: None,
            started_at: 0.0,
            finished_at: Some(1.0),
            cancel: barca_core::CancellationToken::new(),
        }
    }

    fn multi_result(targets: &[(&str, TargetStatus)]) -> RunResult {
        RunResult::Multi(barca_core::commands::MultiResult {
            run_id: "r1".to_string(),
            elapsed_seconds: 0.0,
            steps_executed: 0,
            phases: 1,
            steps: Vec::new(),
            targets: targets
                .iter()
                .map(|(name, status)| {
                    (
                        name.to_string(),
                        barca_core::commands::TargetOutcome {
                            status: *status,
                            final_output: None,
                            error: None,
                            failed_node: None,
                        },
                    )
                })
                .collect(),
        })
    }

    #[test]
    fn a_finished_shared_run_gives_each_job_its_own_outcome() {
        let result = multi_result(&[
            ("f.py:ok", TargetStatus::Success),
            ("f.py:broken", TargetStatus::Failed),
        ]);
        // The run is `failed` because one of its jobs failed; the other job is not.
        let run = run_state(RunStatus::Failed, Some(result));
        assert_eq!(
            job_outcome(&run, "f.py:ok", RunStatus::Complete, false),
            RunStatus::Complete
        );
        assert_eq!(
            job_outcome(&run, "f.py:broken", RunStatus::Running, false),
            RunStatus::Failed
        );
    }

    #[test]
    fn a_stopped_shared_run_only_changes_jobs_that_had_not_ended() {
        for stopped in [RunStatus::Cancelled, RunStatus::Failed] {
            let run = run_state(stopped, None);
            assert_eq!(
                job_outcome(&run, "f.py:done", RunStatus::Complete, false),
                RunStatus::Complete,
                "its step had ended: stopping the run does not undo it"
            );
            assert_eq!(
                job_outcome(&run, "f.py:slow", RunStatus::Running, false),
                stopped
            );
        }
    }

    #[test]
    fn a_job_alone_in_its_run_ends_as_the_run_does() {
        let run = run_state(RunStatus::Failed, None);
        assert_eq!(
            job_outcome(&run, "f.py:a", RunStatus::Complete, true),
            RunStatus::Failed
        );
        let run = run_state(RunStatus::Complete, None);
        assert_eq!(
            job_outcome(&run, "f.py:a", RunStatus::Running, true),
            RunStatus::Complete
        );
    }

    /// A scheduler over `jobs` with no source files behind it and no DB.
    fn scheduler(state: &AppState, jobs: Vec<ScheduledJob>) -> Scheduler {
        Scheduler {
            state: state.clone(),
            zone: Zone::Local,
            jobs,
            dag: None,
            db_path: None,
            ledger: Ledger::default(),
            explained: Mutex::default(),
        }
    }

    fn target_finished(node_id: &str, ok: bool) -> RunEvent {
        RunEvent::TargetFinished {
            node_id: node_id.to_string(),
            ok,
        }
    }

    #[test]
    fn a_job_stops_running_when_its_own_step_ends_not_when_the_shared_run_does() {
        let st = app_state();
        let sched = scheduler(
            &st,
            vec![
                job("f.py:fast", NodeKind::Task, "* * * * * *"),
                job("f.py:broken", NodeKind::Asset, "* * * * * *"),
                job("f.py:slow", NodeKind::Task, "* * * * * *"),
            ],
        );
        let ids: Vec<String> = sched.jobs.iter().map(|j| j.id.clone()).collect();
        insert_run(&st, "h1", RunStatus::Running);
        for id in &ids {
            sched.ledger.set_run(id, "h1");
        }
        sched.publish();
        let apply = |event: RunEvent| apply_event(&st, &sched.ledger, "h1", &ids, &event);

        assert!(!apply(RunEvent::RunStarted {
            run_id: "h1".into()
        }));
        for id in &ids {
            assert_eq!(sched.running(id).as_deref(), Some("h1"), "{id}");
        }

        // `fast` finishes and `broken` fails while `slow` is still running.
        assert!(!apply(target_finished("f.py:fast", true)));
        assert!(!apply(target_finished("f.py:broken", false)));
        assert_eq!(sched.running("f.py:fast"), None);
        assert_eq!(sched.running("f.py:broken"), None);
        assert_eq!(sched.running("f.py:slow").as_deref(), Some("h1"));

        // So the next tick fires the two that ended and skips only `slow`.
        let plan = plan_tick(&at_s(5, 0, 1), &sched.jobs, |id| sched.running(id));
        assert_eq!(fired_ids(&plan), vec!["f.py:fast", "f.py:broken"]);
        assert_eq!(skipped_ids(&plan), vec!["f.py:slow"]);

        // `GET /schedule` reads the same per-job status.
        let published = st.schedule.read().unwrap().clone();
        let status = |id: &str| {
            let job = published.iter().find(|j| j.id == id).unwrap();
            (job.last_handle.clone(), job.last_status)
        };
        let h1 = Some("h1".to_string());
        assert_eq!(status("f.py:fast"), (h1.clone(), Some(RunStatus::Complete)));
        assert_eq!(status("f.py:broken"), (h1.clone(), Some(RunStatus::Failed)));
        assert_eq!(status("f.py:slow"), (h1, Some(RunStatus::Running)));
    }

    #[test]
    fn events_of_an_older_run_do_not_touch_a_job_that_has_fired_again() {
        let st = app_state();
        let sched = scheduler(&st, vec![job("f.py:fast", NodeKind::Task, "* * * * * *")]);
        let ids = vec!["f.py:fast".to_string()];
        insert_run(&st, "h1", RunStatus::Failed);
        insert_run(&st, "h2", RunStatus::Running);
        // The job fired into `h1`, ended there, and has since fired again into `h2`.
        sched.ledger.set_run("f.py:fast", "h2");
        let finished = RunEvent::RunFinished {
            run_id: "h1".into(),
            ok: false,
        };
        assert!(apply_event(&st, &sched.ledger, "h1", &ids, &finished));
        assert_eq!(
            sched.ledger.get("f.py:fast").last_run,
            Some(JobRun {
                handle: "h2".to_string(),
                status: RunStatus::Pending
            }),
            "the newer run's status is untouched"
        );
    }

    #[test]
    fn replaying_a_runs_events_changes_nothing() {
        let st = app_state();
        let sched = scheduler(&st, vec![job("f.py:fast", NodeKind::Task, "* * * * * *")]);
        let ids = vec!["f.py:fast".to_string()];
        insert_run(&st, "h1", RunStatus::Running);
        sched.ledger.set_run("f.py:fast", "h1");
        let events = [
            RunEvent::RunStarted {
                run_id: "h1".into(),
            },
            target_finished("f.py:fast", true),
        ];
        // A subscriber that fell behind starts over from the first event.
        for event in events.iter().chain(events.iter()) {
            apply_event(&st, &sched.ledger, "h1", &ids, event);
        }
        assert_eq!(
            sched.ledger.get("f.py:fast").last_run.unwrap().status,
            RunStatus::Complete,
            "a replayed `run_started` does not put an ended job back to running"
        );
    }

    #[test]
    fn a_job_is_not_running_once_its_run_is_over_even_if_no_event_said_so() {
        let st = app_state();
        let sched = scheduler(&st, vec![job("f.py:a", NodeKind::Asset, "* * * * * *")]);
        insert_run(&st, "h1", RunStatus::Complete);
        sched.ledger.set_run("f.py:a", "h1");
        // The ledger still says `Pending` (the follower has not caught up), but the run is over.
        assert_eq!(sched.running("f.py:a"), None);
        // A run evicted from memory is over too.
        sched.ledger.set_run("f.py:a", "evicted");
        assert_eq!(sched.running("f.py:a"), None);
    }

    // ─── kind-based dispatch ───────────────────────────────────────────────

    #[tokio::test]
    async fn trigger_registers_a_tracked_run_for_asset_and_task() {
        let st = app_state();
        let asset = job("f.py:a", NodeKind::Asset, "* * * * *");
        let task = job("f.py:t", NodeKind::Task, "* * * * *");
        // Both kinds must produce a handle that is registered in the runs map,
        // proving each routes into a real run-trigger path (get vs run).
        let ha = trigger(&st, &[&asset]);
        let ht = trigger(&st, &[&task]);
        assert!(st.runs.contains_key(&ha));
        assert!(st.runs.contains_key(&ht));
        assert_ne!(ha, ht);
    }

    // ─── real-parser enumeration seam ──────────────────────────────────────

    #[tokio::test]
    async fn collect_jobs_discovers_scheduled_nodes_from_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.py");
        std::fs::write(
            &path,
            concat!(
                "from barca import asset, sensor, Schedule, Always\n\n",
                "@asset(freshness=Schedule(\"0 6 * * *\"))\n",
                "def daily_report() -> dict:\n    return {}\n\n",
                "@sensor(freshness=Schedule(\"*/5 * * * *\"))\n",
                "def inbox() -> tuple:\n    return (True, [])\n\n",
                "@asset(freshness=Always)\n",
                "def raw() -> dict:\n    return {}\n",
            ),
        )
        .unwrap();

        let jobs = collect_jobs(
            &[path.display().to_string()],
            &barca_core::commands::find_python(),
        )
        .await;

        assert_eq!(
            jobs.len(),
            2,
            "only the two Schedule nodes, not the Always asset"
        );
        assert!(
            jobs.iter().any(|j| j.id.ends_with(":daily_report")
                && j.kind == NodeKind::Asset
                && j.cron_str == "0 6 * * *"),
            "scheduled asset discovered with round-tripped cron",
        );
        assert!(
            jobs.iter().any(|j| j.id.ends_with(":inbox")
                && j.kind == NodeKind::Sensor
                && j.cron_str == "*/5 * * * *"),
            "scheduled sensor discovered with round-tripped cron",
        );
    }

    #[tokio::test]
    async fn describe_schedule_reports_next_fire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.py");
        std::fs::write(
            &path,
            concat!(
                "from barca import asset, Schedule\n\n",
                "@asset(freshness=Schedule(\"*/5 * * * *\"))\n",
                "def poll() -> dict:\n    return {}\n",
            ),
        )
        .unwrap();

        let infos = describe_schedule(
            &[path.display().to_string()],
            &barca_core::commands::find_python(),
        )
        .await;
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].cron, "*/5 * * * *");
        assert_eq!(infos[0].kind, NodeKind::Asset);
        // A `*/5` cron always has an upcoming occurrence.
        assert!(infos[0].next_fire.is_some());
        assert!(infos[0].next_fire_local.is_some());
    }
}
