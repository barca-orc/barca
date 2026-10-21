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
//! A tick is one run (RFC-0008): the scheduled node and every `Always` node below it
//! ([`barca_core::freshness::followers`]), a `Manual` node read as it is. A job whose ticks
//! failed `max_failures` times in a row is skipped and reported `failing` until a node it
//! failed on (or an input of one, or the scheduled node) succeeds in another run.
//!
//! Semantics: cron is evaluated in the configured timezone (`--timezone`, local
//! by default). On startup a job fires once if a tick elapsed while the daemon
//! was down (catch-up), then fires on each live cron match. Last-fired times are
//! persisted so catch-up survives restarts; runs execute through the bounded run
//! pool and are visible via `GET /schedule`.

use crate::runs;
use crate::state::{AppState, JobStatus, RunStatus};
#[cfg(test)]
use barca_core::results::AssetSummary;
use barca_core::schedule::ScheduledJob;
pub(crate) use barca_core::schedule::Zone;
#[cfg(test)]
use barca_core::schedule::next_fire;
#[cfg(test)]
use barca_core::schedule::{describe_schedule, jobs_from_summaries};
use barca_core::{NodeKind, db};
use chrono::{DateTime, FixedOffset, TimeZone, Timelike};
#[cfg(test)]
use croner::Cron;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(crate) fn zone_of(config: &crate::state::ServeConfig) -> Zone {
    Zone::parse(&config.timezone).unwrap_or(Zone::Local)
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

/// What the scheduler decided to do with a due job on a given tick.
enum TickAction<'a> {
    /// Trigger a run of this job.
    Fire(&'a ScheduledJob),
    /// Skip because the job is paused (`POST /schedule/{id}/pause`).
    Paused(&'a ScheduledJob),
    /// Skip because a run of this job's node (`handle`) is still in flight, whoever started
    /// it: an earlier tick, `POST /get`, `POST /run/{task}` or `POST /run`, or a `barca get`
    /// / `barca run` from a shell on the same project.
    Skip {
        job: &'a ScheduledJob,
        handle: String,
    },
    /// Skip because the job's last `failures` ticks failed, at least its `max_failures`.
    Failing {
        job: &'a ScheduledJob,
        failures: u32,
    },
}

/// Decide, for a tick at `now`, which due jobs to fire and which to skip. Pure:
/// takes a lookup returning the handle of a run of the job's node still in
/// flight, if any, and one returning a job's consecutive failed ticks, so the skip
/// logic is testable without a live server or wall clock.
fn plan_tick<'a, Tz: TimeZone>(
    now: &DateTime<Tz>,
    jobs: &'a [ScheduledJob],
    paused: impl Fn(&ScheduledJob) -> bool,
    in_flight: impl Fn(&ScheduledJob) -> Option<String>,
    failures: impl Fn(&ScheduledJob) -> u32,
) -> Vec<TickAction<'a>> {
    due_jobs(now, jobs)
        .into_iter()
        .map(|job| {
            if paused(job) {
                return TickAction::Paused(job);
            }
            let failures = failures(job);
            if failures >= job.max_failures {
                return TickAction::Failing { job, failures };
            }
            match in_flight(job) {
                Some(handle) => TickAction::Skip { job, handle },
                None => TickAction::Fire(job),
            }
        })
        .collect()
}

/// Whether a run with this `command` and resolved `node` brings `job`'s node up to date: a
/// run of that node, or a run of every asset and sensor (`POST /run`, which has no node)
/// for a scheduled asset or sensor. A run of another node does not count, even one whose
/// cone includes this node.
fn run_covers(command: &str, nodes: &[String], job: &ScheduledJob) -> bool {
    if nodes.is_empty() {
        command == "get" && job.kind != NodeKind::Task
    } else {
        nodes.contains(&job.id)
    }
}

/// The handle of the oldest pending or running run that covers `job` ([`run_covers`]),
/// whoever started it: the scheduler or an HTTP request.
fn run_in_flight_for(state: &AppState, job: &ScheduledJob) -> Option<String> {
    state
        .runs
        .iter()
        .filter(|r| matches!(r.status, RunStatus::Pending | RunStatus::Running))
        .filter(|r| run_covers(&r.command, &r.nodes, job))
        .min_by(|a, b| a.started_at.total_cmp(&b.started_at))
        .map(|r| r.handle.clone())
}

/// The run id of the oldest CLI run in `alive` ([`db::alive_running_runs`]: the `running`
/// rows of the project's metadata database whose owner is alive) that covers `job`
/// ([`run_covers`]). A `barca get` or `barca run` from a shell on the same project and
/// `.barca/` is not in the server's memory; its row says which job it runs (`runs.job`:
/// `all`, or the resolved node ids joined by `,`). A row with no job (an older barca) does
/// not count.
fn cli_run_in_flight_for(alive: &[db::AliveRun], job: &ScheduledJob) -> Option<String> {
    alive
        .iter()
        .find(|run| {
            let nodes: Vec<String> = match run.job.as_str() {
                "" => return false,
                "all" => Vec::new(),
                ids => ids.split(',').map(str::to_string).collect(),
            };
            run_covers(&run.command, &nodes, job)
        })
        .map(|run| run.run_id.clone())
}

/// The run of `job`'s node in flight, if any: one the server holds ([`run_in_flight_for`])
/// or else a CLI run on the same project ([`cli_run_in_flight_for`]).
fn any_run_in_flight_for(
    state: &AppState,
    cli: &[db::AliveRun],
    job: &ScheduledJob,
) -> Option<String> {
    run_in_flight_for(state, job).or_else(|| cli_run_in_flight_for(cli, job))
}

/// What the scheduler records about each job between ticks, for `GET /schedule`.
#[derive(Debug, Default)]
struct Bookkeeping {
    /// Handle of the last run the scheduler triggered, per job.
    last_handle: HashMap<String, String>,
    /// Last-fired epoch per job; persisted for catch-up.
    last_fired: HashMap<String, i64>,
    /// Last skipped tick per job: its epoch, why (`in_flight`, `paused`) and the run in
    /// flight that caused it.
    last_skip: HashMap<String, (i64, &'static str, Option<String>)>,
    /// Consecutive failed ticks per job (RFC-0008 rule 9); persisted for restarts.
    failures: HashMap<String, db::ScheduleFailures>,
    /// Handles of tick runs whose outcome has been counted.
    counted: HashSet<String>,
}

impl Bookkeeping {
    fn failures(&self, job: &ScheduledJob) -> u32 {
        self.failures.get(&job.id).map_or(0, |f| f.failures)
    }
}

/// How a finished tick run ended: `None` while it is pending or running, or when it was
/// cancelled (which is not a failure of the job); otherwise the nodes that failed, empty on
/// success. A failed run with no failed step (it could not start) failed on the scheduled node.
fn tick_outcome(run: &crate::state::RunState, job: &ScheduledJob) -> Option<Vec<String>> {
    match run.status {
        RunStatus::Pending | RunStatus::Running | RunStatus::Cancelled => None,
        RunStatus::Complete => Some(Vec::new()),
        RunStatus::Failed => {
            let mut nodes = failed_steps(run);
            nodes.dedup();
            if nodes.is_empty() {
                nodes.push(job.id.clone());
            }
            Some(nodes)
        }
    }
}

/// The ids of the steps that failed in a finished run: from its outcome (`steps`, and each
/// failed target's `failed_node` of a multi-target run), or from its single-target result.
fn failed_steps(run: &crate::state::RunState) -> Vec<String> {
    let mut nodes: Vec<String> = Vec::new();
    let outcome: Option<serde_json::Value> = run
        .outcome
        .as_ref()
        .and_then(|o| serde_json::from_str(o.text()).ok());
    let steps = outcome
        .as_ref()
        .and_then(|o| o["steps"].as_array().cloned())
        .unwrap_or_default();
    for step in &steps {
        if step["status"] == "failed"
            && let Some(id) = step["id"].as_str()
        {
            nodes.push(id.to_string());
        }
    }
    if let Some(targets) = outcome.as_ref().and_then(|o| o["targets"].as_object()) {
        for target in targets.values() {
            if target["status"] == "failed"
                && let Some(node) = target["failed_node"].as_str()
                && !nodes.iter().any(|n| n == node)
            {
                nodes.push(node.to_string());
            }
        }
    }
    if nodes.is_empty() {
        nodes = run
            .result
            .iter()
            .flat_map(|r| &r.steps)
            .filter(|s| s.status.as_deref() == Some("failed"))
            .map(|s| s.id.clone())
            .collect();
    }
    nodes
}

/// Count the outcome of each job's last tick run that has finished since the last call: a
/// failure adds one to the job's consecutive failures, a success clears them.
async fn count_outcomes(
    state: &AppState,
    jobs: &[ScheduledJob],
    book: &mut Bookkeeping,
    db_path: Option<&str>,
) -> bool {
    let mut changed = false;
    for job in jobs {
        let Some(handle) = book.last_handle.get(&job.id).cloned() else {
            continue;
        };
        if book.counted.contains(&handle) {
            continue;
        }
        let Some(failed) = state.runs.get(&handle).and_then(|r| tick_outcome(&r, job)) else {
            continue;
        };
        book.counted.insert(handle);
        let before = book.failures(job);
        let next = if failed.is_empty() {
            db::ScheduleFailures::default()
        } else {
            let mark = match db_path {
                Some(dbp) => db::max_materialization_id(dbp).await.unwrap_or(0),
                None => 0,
            };
            db::ScheduleFailures {
                failures: before + 1,
                failed_nodes: failed,
                mark,
            }
        };
        if next.failures == before && before == 0 {
            continue;
        }
        if next.failures >= job.max_failures && before < job.max_failures {
            barca_core::jsonlog::line(
                barca_core::jsonlog::Level::Error,
                Some(&job.id),
                &format!(
                    "[barca] {} failing: {} consecutive failed ticks (max_failures={}); skipped \
                     until an explicit request or a change in its inputs",
                    job.id, next.failures, job.max_failures
                ),
            );
        }
        if let Some(dbp) = db_path {
            persist_failures(dbp, job, &next).await;
        }
        book.failures.insert(job.id.clone(), next);
        changed = true;
    }
    changed
}

/// Whether a failing job may fire again: a success recorded after its last counted failure for
/// the scheduled node (an explicit request), a node it failed on, or a direct input of one (an
/// input change).
async fn failing_reset(
    state: &AppState,
    job: &ScheduledJob,
    failures: &db::ScheduleFailures,
    db_path: &str,
) -> bool {
    let mut watch: Vec<String> = vec![job.id.clone()];
    watch.extend(failures.failed_nodes.iter().cloned());
    if let Ok(dag) = state.loaded_dag().await {
        for node in &failures.failed_nodes {
            watch.extend(dag.upstream(node).into_iter().map(str::to_string));
        }
    }
    db::succeeded_since(db_path, failures.mark, &watch)
        .await
        .unwrap_or(false)
}

/// Clear the failures of each due failing job that may fire again ([`failing_reset`]).
async fn reset_failing<Tz: TimeZone>(
    state: &AppState,
    now: &DateTime<Tz>,
    jobs: &[ScheduledJob],
    book: &mut Bookkeeping,
    db_path: &str,
) {
    for job in due_jobs(now, jobs) {
        let Some(failures) = book.failures.get(&job.id) else {
            continue;
        };
        if failures.failures < job.max_failures
            || !failing_reset(state, job, failures, db_path).await
        {
            continue;
        }
        barca_core::jsonlog::line(
            barca_core::jsonlog::Level::Info,
            Some(&job.id),
            &format!(
                "[barca] {} no longer failing: something it failed on succeeded since",
                job.id
            ),
        );
        book.failures.remove(&job.id);
        persist_failures(db_path, job, &db::ScheduleFailures::default()).await;
    }
}

/// Record a job's consecutive failed ticks. Best-effort durability, like [`persist_fired`].
async fn persist_failures(db_path: &str, job: &ScheduledJob, failures: &db::ScheduleFailures) {
    if let Err(e) = db::set_schedule_failures(db_path, &job.id, failures).await {
        barca_core::errln!("[barca] schedule_failures write failed for {}: {e}", job.id);
    }
}

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
    barca_core::jsonlog::line(barca_core::jsonlog::Level::Warning, Some(&job.id), &line);
    book.last_skip
        .insert(job.id.clone(), (epoch, reason, handle.map(str::to_string)));
    let report = skip_report(job, reason, handle, epoch);
    tokio::spawn(async move {
        let integrations = barca_core::telemetry::configured();
        if !integrations.is_empty() {
            // A skipped tick is not an outcome of the job: no history to read.
            let history = barca_core::telemetry::JobHistory::none();
            barca_core::telemetry::export(&integrations, &report, &history).await;
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
) -> barca_core::telemetry::RunReport {
    barca_core::telemetry::RunReport {
        run_id: db::generate_run_id(),
        command: match job.kind {
            NodeKind::Task => "run",
            NodeKind::Asset | NodeKind::Sensor => "get",
        }
        .to_string(),
        target: Some(job.id.clone()),
        job: job.id.clone(),
        env: None,
        targets: vec![barca_core::telemetry::short_name(&job.id).to_string()],
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
        trigger: barca_core::outcome::Trigger::schedule(&job.id, epoch),
    }
}

/// Trigger the run for the tick of a due job at `epoch`: its node and the `Always` nodes below
/// it, in one run. Returns the run id.
///
/// A tick brings the nodes up to date, it does not force them: sensors upstream are polled,
/// anything whose inputs changed is recomputed, and an asset whose inputs did not change is
/// served from cache. The scheduled task itself always runs; a task below it fires only when
/// it is out of date. The run's command follows the scheduled node's kind: `run` for a task,
/// `get` for an asset or sensor.
fn trigger(state: &AppState, job: &ScheduledJob, epoch: i64) -> String {
    let command = match job.kind {
        NodeKind::Task => barca_core::outcome::RunCommand::Run,
        NodeKind::Asset | NodeKind::Sensor => barca_core::outcome::RunCommand::Get,
    };
    let targets: Vec<String> = std::iter::once(job.id.clone())
        .chain(job.followers.iter().cloned())
        .collect();
    runs::start(
        state.clone(),
        runs::RunSpec {
            command,
            targets: targets.clone(),
            nodes: targets,
            files: Vec::new(),
            policy: barca_core::cache::CachePolicy::CacheAware,
            trigger: barca_core::outcome::Trigger::schedule(&job.id, epoch),
            values: false,
            partition: None,
            rules: barca_core::freshness::RunRules::tick(&job.id),
        },
    )
}

/// Whether a scheduled tick elapsed between `last_fired` and `now` — i.e. the
/// next cron occurrence strictly after `last_fired` is already in the past. This
/// drives the single catch-up run after the daemon was down. Pure and generic
/// over timezone so it is testable and reusable once a `--timezone` is honored.
#[cfg(test)]
fn needs_catchup<Tz: TimeZone>(cron: &Cron, last_fired: &DateTime<Tz>, now: &DateTime<Tz>) -> bool {
    match cron.find_next_occurrence(last_fired, false) {
        Ok(next) => next <= *now,
        Err(_) => false,
    }
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
) -> Result<Bookkeeping, barca_core::BarcaError> {
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
                if barca_core::schedule::next_fire(
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
                    } else if book.failures(job) >= job.max_failures {
                        barca_core::jsonlog::line(
                            barca_core::jsonlog::Level::Warning,
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
                    } else {
                        let handle = trigger(state, job, now.timestamp());
                        barca_core::jsonlog::line(
                            barca_core::jsonlog::Level::Info,
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

/// Record that `node_id` fired at `epoch` seconds. Best-effort durability.
async fn persist_fired(db_path: &str, node_id: &str, epoch: i64) {
    if let Err(e) = db::upsert_schedule_state(db_path, node_id, epoch).await {
        barca_core::errln!("[barca] schedule_state write failed for {node_id}: {e}");
    }
}

/// Publish the current job set + last-fired/last-handle/last-skip bookkeeping into
/// shared state for `GET /schedule` to read.
fn publish_registry(state: &AppState, jobs: &[ScheduledJob], book: &Bookkeeping) {
    let snapshot: Vec<JobStatus> = jobs
        .iter()
        .map(|j| {
            let skip = book.last_skip.get(&j.id);
            JobStatus {
                id: j.id.clone(),
                cron: j.cron_str.clone(),
                kind: j.kind,
                last_fired: book.last_fired.get(&j.id).copied(),
                last_handle: book.last_handle.get(&j.id).cloned(),
                last_skipped: skip.map(|(at, _, _)| *at),
                skipped_for: skip.and_then(|(_, _, handle)| handle.clone()),
                skip_reason: skip.map(|(_, reason, _)| *reason),
                failures: book.failures(j),
                max_failures: j.max_failures,
            }
        })
        .collect();
    if let Ok(mut w) = state.schedule.write() {
        *w = snapshot;
    }
}

/// Re-run static analysis to enumerate scheduled jobs. The parse itself runs
/// on the blocking pool inside `barca_core::queries::list_assets`.
async fn reload_jobs(state: &AppState) -> Vec<ScheduledJob> {
    match state.loaded_dag().await {
        Ok(dag) => barca_core::schedule::jobs_from_dag(&dag),
        Err(error) => {
            barca_core::errln!("[barca] scheduler load failed: {error}");
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
        barca_core::errln!("[barca] no scheduled nodes yet (watching for changes)");
        return;
    }
    barca_core::errln!("[barca] scheduling {}:", count_by_kind(jobs));
    let now = chrono::Utc::now();
    for job in jobs {
        let next = barca_core::schedule::next_fire(&job.cron, zone, now)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "?".to_string());
        barca_core::errln!("  {} — {} (next {})", job.id, job.cron_str, next);
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
            barca_core::errln!("[barca] scheduler disabled: {e}");
            return;
        }
    };

    // Associate the initial jobs with the generation before loading them. Edits during
    // loading, DB initialization or catch-up must remain pending for the first tick.
    let mut seen_gen = state.dag_generation.load(Ordering::Relaxed);
    let mut jobs = reload_jobs(&state).await;

    if jobs.is_empty() && !state.config.watch {
        barca_core::errln!("[barca] no scheduled nodes — scheduler idle");
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
                barca_core::errln!(
                    "[barca] scheduler disabled: metadata initialization failed: {error}"
                );
                state.scheduler_failed(error.to_string());
                return;
            }
            Some(path)
        }
        Err(_) => {
            barca_core::errln!("[barca] scheduler: durability disabled (no metadata db)");
            None
        }
    };

    // Paused schedules, for the tick to skip. They are kept in step by the pause routes.
    if let Err(e) = crate::pause::pauses(&state).await {
        let (_, envelope) = e.parts();
        barca_core::errln!(
            "[barca] scheduler: cannot read paused schedules: {}",
            envelope.error
        );
    }

    // Handle issued, last-fired epoch and last skip per job, for the `/schedule` view;
    // `last_fired` also powers durability. The overlap skip reads `state.runs` and the
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
                barca_core::errln!(
                    "[barca] scheduler disabled: cannot read schedule history: {error}"
                );
                state.scheduler_failed(format!("cannot read schedule history: {error}"));
                return;
            }
        },
        None => Bookkeeping::default(),
    };
    publish_registry(&state, &jobs, &book);

    loop {
        // A draining server starts no run: the scheduler stops, and a tick it misses is
        // caught up by the next server's startup catch-up.
        tokio::select! {
            _ = sleep_to_next_second(&zone) => {}
            _ = state.draining.cancelled() => {}
        }
        if state.is_draining() {
            barca_core::errln!("[barca] scheduler stopped: the server is draining");
            return;
        }

        // `--watch`: re-read the job set when a source file changed.
        let current_gen = state.dag_generation.load(Ordering::Relaxed);
        if current_gen != seen_gen {
            seen_gen = current_gen;
            let fresh = reload_jobs(&state).await;
            barca_core::errln!("[barca] schedule reloaded: {} job(s)", fresh.len());
            jobs = fresh;
            log_schedule(&jobs, &zone);
            publish_registry(&state, &jobs, &book);
        }

        let now = zone.now();
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
            |job| crate::pause::active(&state, &job.id, epoch).is_some(),
            |job| any_run_in_flight_for(&state, &cli, job),
            |job| book.failures(job),
        );
        for action in actions {
            match action {
                TickAction::Paused(job) => {
                    record_skip(&mut book, job, None, epoch, "scheduled run");
                }
                TickAction::Skip { job, handle } => {
                    record_skip(&mut book, job, Some(&handle), epoch, "scheduled run");
                }
                TickAction::Failing { job, failures } => {
                    barca_core::jsonlog::line(
                        barca_core::jsonlog::Level::Warning,
                        Some(&job.id),
                        &format!(
                            "[barca] scheduled run {} skipped — failing ({failures} consecutive \
                             failed ticks, max_failures={})",
                            job.id, job.max_failures
                        ),
                    );
                }
                TickAction::Fire(job) => {
                    let handle = trigger(&state, job, epoch);
                    barca_core::jsonlog::line(
                        barca_core::jsonlog::Level::Info,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RunState, ServeConfig};
    use barca_core::{CronExpr, Freshness, NodeKind};
    use chrono::{Local, Utc};
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

    /// Record a run as the server would: `command` is `get` or `run`, `target` the name as
    /// given and `node` the full id it resolved to (`None` for `POST /run`, every asset).
    fn insert_run(
        state: &AppState,
        handle: &str,
        status: RunStatus,
        command: &str,
        target: Option<&str>,
        node: Option<&str>,
        started_at: f64,
    ) {
        state.runs.insert(
            handle.to_string(),
            RunState {
                handle: handle.to_string(),
                db_run_id: None,
                command: command.into(),
                target: target.map(str::to_string),
                nodes: node.map(str::to_string).into_iter().collect(),
                trigger: barca_core::outcome::Trigger::api(),
                files: Vec::new(),
                status,
                result: None,
                error: None,
                started_at,
                finished_at: None,
                outcome: None,
                cancel: barca_core::CancellationToken::new(),
            },
        );
    }

    #[tokio::test]
    async fn startup_db_admission_cannot_acknowledge_a_source_generation_it_did_not_load() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("pipeline.py");
        let initial = "from barca import task, Schedule\n@task(freshness=Schedule('0 5 1 1 *'))\ndef healthy(): pass\n";
        std::fs::write(&source, initial).unwrap();
        let mut config = app_state().config.as_ref().clone();
        config.files = vec![source.to_string_lossy().into()];
        config.watch = true;
        config.resolved.db_path = dir.path().join("metadata.db").to_string_lossy().into();
        let state = AppState::new(config);
        let held = db::db_guard().await;
        let task = tokio::spawn(run_scheduler(state.clone()));
        // The initial selected graph is published before scheduler DB admission.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state.loaded.lock().await.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial graph not loaded");
        std::fs::write(
            &source,
            format!("{initial}\n@task(freshness=Schedule('0 5 1 1 *'))\ndef repaired(): pass\n"),
        )
        .unwrap();
        // This is the existing watch event signal, while startup still waits for the DB.
        state.dag_generation.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            barca_core::queries::list_assets_from_dag(&state.loaded_dag().await.unwrap()).len(),
            2
        );
        drop(held);
        let repaired = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if state
                    .schedule
                    .read()
                    .unwrap()
                    .iter()
                    .any(|job| job.id.ends_with(":repaired"))
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        task.abort();
        let _ = task.await;
        repaired.expect("startup swallowed the repaired source generation");
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

    // ─── startup log wording ───────────────────────────────────────────────

    #[test]
    fn the_schedule_line_names_each_kind_with_its_count() {
        let asset = || job("f.py:a", NodeKind::Asset, "0 5 * * *");
        let sensor = || job("f.py:s", NodeKind::Sensor, "0 5 * * *");
        let task = || job("f.py:t", NodeKind::Task, "0 5 * * *");
        assert_eq!(count_by_kind(&[task()]), "1 task");
        assert_eq!(count_by_kind(&[asset()]), "1 asset");
        assert_eq!(count_by_kind(&[asset(), asset()]), "2 assets");
        assert_eq!(count_by_kind(&[task(), asset()]), "1 asset and 1 task");
        assert_eq!(
            count_by_kind(&[task(), sensor(), asset(), sensor(), task()]),
            "1 asset, 2 sensors and 2 tasks"
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
    fn zone_parse_accepts_local_utc_and_iana_names() {
        assert!(matches!(Zone::parse("local"), Ok(Zone::Local)));
        assert!(matches!(Zone::parse("Local"), Ok(Zone::Local)));
        assert!(matches!(Zone::parse("utc"), Ok(Zone::Utc)));
        assert!(matches!(Zone::parse("UTC"), Ok(Zone::Utc)));
        assert!(matches!(Zone::parse(" utc "), Ok(Zone::Utc)));
        assert!(matches!(
            Zone::parse("America/New_York"),
            Ok(Zone::Named(chrono_tz::America::New_York))
        ));
        assert!(matches!(Zone::parse("Etc/UTC"), Ok(Zone::Named(_))));
    }

    #[test]
    fn zone_parse_rejects_anything_else_and_names_it() {
        // An unknown zone used to fall back to local time with one line on stderr (#289).
        for bad in ["Not/AZone", "", "america/new_york", "EST5", "+02:00"] {
            let err = Zone::parse(bad).unwrap_err();
            assert!(
                err.contains(&format!("unknown timezone '{bad}'")),
                "{bad:?}: {err}"
            );
            assert!(err.contains("America/New_York"), "{err}");
        }
    }

    /// 2026-10-08 14:33:00 UTC.
    fn an_instant() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 8, 14, 33, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn the_next_fire_time_is_computed_in_the_given_zone() {
        let j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        let epoch = |zone: &Zone| next_fire(&j.cron, zone, an_instant()).unwrap().timestamp();
        let utc_5am = Utc.with_ymd_and_hms(2026, 10, 9, 5, 0, 0).single().unwrap();
        assert_eq!(epoch(&Zone::Utc), utc_5am.timestamp());
        // 05:00 in Kolkata (UTC+5:30) is 23:30 UTC the day before; in Honolulu (UTC-10), 15:00.
        let kolkata = Zone::parse("Asia/Kolkata").unwrap();
        assert_eq!(epoch(&kolkata), utc_5am.timestamp() - 5 * 3600 - 1800);
        let honolulu = Zone::parse("Pacific/Honolulu").unwrap();
        assert_eq!(
            epoch(&honolulu),
            Utc.with_ymd_and_hms(2026, 10, 8, 15, 0, 0)
                .single()
                .unwrap()
                .timestamp()
        );
    }

    #[test]
    fn get_schedule_reports_next_fire_in_the_servers_zone() {
        // It used to compute it in local time whatever `--timezone` said (#289). Two zones, so
        // the test cannot pass by running on a machine whose local zone is one of them.
        for (zone, want) in [
            ("Asia/Kolkata", "2026-10-08T23:30:00Z"),
            ("Pacific/Honolulu", "2026-10-08T15:00:00Z"),
            ("utc", "2026-10-09T05:00:00Z"),
        ] {
            let mut config = (*app_state().config).clone();
            config.timezone = zone.to_string();
            let st = AppState::new(config);
            publish_registry(
                &st,
                &[job("f.py:daily", NodeKind::Task, "0 5 * * *")],
                &Bookkeeping::default(),
            );
            let body = crate::handlers::schedule_at(&st, an_instant());
            let want = DateTime::parse_from_rfc3339(want).unwrap().timestamp();
            assert_eq!(body[0]["next_fire"], want, "{zone}: {body}");
            assert_eq!(body[0]["id"], "f.py:daily");
        }
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
    fn a_run_of_the_node_is_in_flight_only_while_pending_or_running() {
        let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        for (status, in_flight) in [
            (RunStatus::Pending, true),
            (RunStatus::Running, true),
            (RunStatus::Complete, false),
            (RunStatus::Failed, false),
            (RunStatus::Cancelled, false),
        ] {
            let st = app_state();
            insert_run(
                &st,
                "h",
                status,
                "get",
                Some("f.py:daily"),
                Some("f.py:daily"),
                1.0,
            );
            assert_eq!(
                run_in_flight_for(&st, &daily).is_some(),
                in_flight,
                "{status:?}"
            );
        }
        assert_eq!(run_in_flight_for(&app_state(), &daily), None, "no runs");
    }

    #[test]
    fn a_run_started_over_http_by_short_name_counts_for_the_scheduled_node() {
        // `POST /get/daily` records the name as given and the node it resolved to. The
        // scheduler only ever issued its own handles before, so this run was invisible (#432).
        let st = app_state();
        let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        insert_run(
            &st,
            "api",
            RunStatus::Running,
            "get",
            Some("daily"),
            Some("f.py:daily"),
            1.0,
        );
        assert_eq!(run_in_flight_for(&st, &daily).as_deref(), Some("api"));

        // `POST /run/{task}` for a scheduled task.
        let weekly = job("f.py:weekly", NodeKind::Task, "0 5 * * *");
        insert_run(
            &st,
            "task",
            RunStatus::Pending,
            "run",
            Some("weekly"),
            Some("f.py:weekly"),
            1.0,
        );
        assert_eq!(run_in_flight_for(&st, &weekly).as_deref(), Some("task"));
    }

    #[test]
    fn a_run_of_another_node_does_not_count() {
        let st = app_state();
        insert_run(
            &st,
            "other",
            RunStatus::Running,
            "get",
            Some("f.py:other"),
            Some("f.py:other"),
            1.0,
        );
        let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        assert_eq!(run_in_flight_for(&st, &daily), None);
    }

    #[test]
    fn a_run_of_every_asset_counts_for_scheduled_assets_and_sensors_not_tasks() {
        // `POST /run` gets every asset and sensor and skips tasks.
        let st = app_state();
        insert_run(&st, "all", RunStatus::Running, "get", None, None, 1.0);
        for kind in [NodeKind::Asset, NodeKind::Sensor] {
            let j = job("f.py:n", kind, "0 5 * * *");
            assert_eq!(
                run_in_flight_for(&st, &j).as_deref(),
                Some("all"),
                "{kind:?}"
            );
        }
        let task = job("f.py:t", NodeKind::Task, "0 5 * * *");
        assert_eq!(run_in_flight_for(&st, &task), None);
    }

    #[test]
    fn the_oldest_run_in_flight_is_the_one_named() {
        let st = app_state();
        insert_run(
            &st,
            "newer",
            RunStatus::Pending,
            "get",
            Some("daily"),
            Some("f.py:daily"),
            20.0,
        );
        insert_run(
            &st,
            "older",
            RunStatus::Running,
            "get",
            Some("f.py:daily"),
            Some("f.py:daily"),
            10.0,
        );
        let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        assert_eq!(run_in_flight_for(&st, &daily).as_deref(), Some("older"));
    }

    #[test]
    fn a_skipped_tick_is_reported_with_status_skipped_and_the_run_in_flight() {
        let task = job("f.py:weekly", NodeKind::Task, "0 5 * * *");
        let report = skip_report(&task, "in_flight", Some("h1"), 1_000);
        assert_eq!(report.status, "skipped");
        assert_eq!(report.reason.as_deref(), Some("in_flight"));
        assert_eq!(report.in_flight_run.as_deref(), Some("h1"));
        assert_eq!(report.command, "run");
        assert_eq!(report.job, "f.py:weekly");
        assert_eq!(report.start_unix_ns, 1_000_000_000_000);
        assert!(report.steps.is_empty());
        assert_ne!(report.run_id, "h1", "a skip is its own trace");
        assert_eq!(
            report.trigger.triggered_by,
            barca_core::outcome::TriggeredBy::Schedule
        );
        assert_eq!(report.trigger.schedule_id.as_deref(), Some("f.py:weekly"));
        let paused = skip_report(&task, "paused", None, 1_000);
        assert_eq!(paused.reason.as_deref(), Some("paused"));
        assert!(paused.in_flight_run.is_none());
    }

    #[tokio::test]
    async fn a_skip_is_published_to_get_schedule() {
        let st = app_state();
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let mut book = Bookkeeping::default();
        publish_registry(&st, &jobs, &book);
        let body = crate::handlers::schedule_at(&st, an_instant());
        assert_eq!(body[0]["last_skipped"], serde_json::Value::Null);
        assert_eq!(body[0]["skipped_for"], serde_json::Value::Null);

        record_skip(&mut book, &jobs[0], Some("api"), 1_234, "scheduled run");
        publish_registry(&st, &jobs, &book);
        let body = crate::handlers::schedule_at(&st, an_instant());
        assert_eq!(body[0]["last_skipped"], 1_234);
        assert_eq!(body[0]["skipped_for"], "api");
        assert_eq!(body[0]["skip_reason"], "in_flight");
        assert_eq!(body[0]["paused"], false);
    }

    // ─── per-tick planning (the overlap-skip guard) ────────────────────────

    fn fired_ids(plan: &[TickAction]) -> Vec<String> {
        plan.iter()
            .filter_map(|a| match a {
                TickAction::Fire(j) => Some(j.id.clone()),
                _ => None,
            })
            .collect()
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
    fn plan_tick_skips_a_job_at_its_max_failures_and_fires_it_below() {
        let mut j = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        assert_eq!(j.max_failures, 5, "the default");
        j.max_failures = 3;
        let jobs = vec![j];
        let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| None, |_| 2);
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
        // Failing wins over an in-flight run: nothing would fire either way.
        let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| Some("h".into()), |_| 3);
        match plan.as_slice() {
            [TickAction::Failing { job, failures: 3 }] => assert_eq!(job.id, "f.py:daily"),
            _ => panic!("expected failing"),
        }
    }

    fn finished(state: &AppState, handle: &str, status: RunStatus, failed: &[&str]) {
        insert_run(state, handle, status, "get", None, Some("f.py:s"), 1.0);
        let steps = failed
            .iter()
            .map(|id| barca_core::results::StepReport {
                id: id.to_string(),
                status: Some("failed".into()),
                ..Default::default()
            })
            .collect();
        state.runs.get_mut(handle).unwrap().result = Some(barca_core::results::GetResult {
            run_id: "r".into(),
            elapsed_seconds: 0.0,
            steps_executed: 0,
            phases: 0,
            final_output: None,
            steps,
            warnings: Vec::new(),
            partitions: None,
            cache: None,
            uploads: None,
        });
    }

    #[test]
    fn a_tick_run_counts_once_it_finished_and_not_when_cancelled() {
        let st = app_state();
        let j = job("f.py:s", NodeKind::Sensor, "* * * * *");
        insert_run(
            &st,
            "p",
            RunStatus::Running,
            "get",
            None,
            Some("f.py:s"),
            1.0,
        );
        assert_eq!(tick_outcome(&st.runs.get("p").unwrap(), &j), None);
        finished(&st, "c", RunStatus::Cancelled, &[]);
        assert_eq!(tick_outcome(&st.runs.get("c").unwrap(), &j), None);
        finished(&st, "ok", RunStatus::Complete, &[]);
        assert_eq!(tick_outcome(&st.runs.get("ok").unwrap(), &j), Some(vec![]));
        finished(&st, "f", RunStatus::Failed, &["f.py:report"]);
        assert_eq!(
            tick_outcome(&st.runs.get("f").unwrap(), &j),
            Some(vec!["f.py:report".to_string()])
        );
        // A multi-target tick's failed steps come from its outcome.
        insert_run(
            &st,
            "m",
            RunStatus::Failed,
            "get",
            None,
            Some("f.py:s"),
            1.0,
        );
        st.runs.get_mut("m").unwrap().outcome = Some(
            barca_core::outcome::Outcome::from_text(
                r#"{"status":"failed","steps":[{"id":"f.py:s","status":"success"},{"id":"f.py:publish","status":"failed"}],"targets":{"f.py:s":{"status":"success"},"f.py:publish":{"status":"failed","failed_node":"f.py:publish"},"f.py:late":{"status":"blocked","failed_node":"f.py:baseline"}}}"#,
            )
            .unwrap(),
        );
        assert_eq!(
            tick_outcome(&st.runs.get("m").unwrap(), &j),
            Some(vec!["f.py:publish".to_string()])
        );
        // A failed run with no failed step (it could not start) failed on the scheduled node.
        finished(&st, "x", RunStatus::Failed, &[]);
        assert_eq!(
            tick_outcome(&st.runs.get("x").unwrap(), &j),
            Some(vec!["f.py:s".to_string()])
        );
    }

    #[tokio::test]
    async fn consecutive_failed_ticks_add_up_and_a_success_clears_them() {
        let st = app_state();
        let jobs = vec![job("f.py:s", NodeKind::Sensor, "* * * * *")];
        let mut book = Bookkeeping::default();
        for (n, handle) in ["a", "b"].into_iter().enumerate() {
            finished(&st, handle, RunStatus::Failed, &["f.py:report"]);
            book.last_handle.insert("f.py:s".into(), handle.into());
            assert!(count_outcomes(&st, &jobs, &mut book, None).await);
            assert_eq!(book.failures(&jobs[0]), n as u32 + 1);
            // Counted once, however many ticks look at it.
            assert!(!count_outcomes(&st, &jobs, &mut book, None).await);
        }
        assert_eq!(
            book.failures["f.py:s"].failed_nodes,
            vec!["f.py:report".to_string()]
        );
        finished(&st, "c", RunStatus::Complete, &[]);
        book.last_handle.insert("f.py:s".into(), "c".into());
        assert!(count_outcomes(&st, &jobs, &mut book, None).await);
        assert_eq!(book.failures(&jobs[0]), 0);
    }

    #[test]
    fn plan_tick_fires_when_nothing_is_in_flight() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(5, 0), &jobs, |_| false, |_| None, |_| 0);
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    }

    #[test]
    fn plan_tick_skips_when_a_run_of_the_node_is_in_flight() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| (j.id == "f.py:daily").then(|| "h1".to_string()),
            |_| 0,
        );
        assert!(fired_ids(&plan).is_empty());
        assert_eq!(skipped_ids(&plan), vec!["f.py:daily"]);
        // The skip carries the offending handle for the log line.
        match &plan[0] {
            TickAction::Skip { handle, .. } => assert_eq!(handle, "h1"),
            _ => panic!("expected skip"),
        }
    }

    #[test]
    fn plan_tick_skips_for_a_run_started_over_http() {
        // End to end through the real lookup: the scheduler never issued this handle.
        let st = app_state();
        insert_run(
            &st,
            "api",
            RunStatus::Running,
            "get",
            Some("daily"),
            Some("f.py:daily"),
            1.0,
        );
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| run_in_flight_for(&st, j),
            |_| 0,
        );
        assert_eq!(skipped_ids(&plan), vec!["f.py:daily"]);

        // Once it finishes, the next due tick fires.
        st.runs.get_mut("api").unwrap().status = RunStatus::Complete;
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| run_in_flight_for(&st, j),
            |_| 0,
        );
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    }

    #[test]
    fn plan_tick_ignores_jobs_not_due_this_minute() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(6, 0), &jobs, |_| false, |_| Some("h".into()), |_| 0);
        assert!(plan.is_empty(), "06:00 is not the job's minute");
    }

    #[test]
    fn plan_tick_mixes_fire_and_skip_across_jobs() {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:b", NodeKind::Task, "0 5 * * *"),
        ];
        // `a` has a run still going; `b` has none.
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| (j.id == "f.py:a").then(|| "ha".into()),
            |_| 0,
        );
        assert_eq!(skipped_ids(&plan), vec!["f.py:a"]);
        assert_eq!(fired_ids(&plan), vec!["f.py:b"]);
    }

    #[tokio::test]
    async fn a_missed_tick_is_skipped_at_startup_while_its_node_is_running() {
        let (_dir, db_path) = schedule_db().await;
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
        catch_up(
            &app_state(),
            &[],
            &jobs,
            &Zone::Utc,
            &utc(2, 5, 0),
            &db_path,
        )
        .await
        .unwrap();

        let st = app_state();
        insert_run(
            &st,
            "api",
            RunStatus::Running,
            "get",
            Some("daily"),
            Some("f.py:daily"),
            1.0,
        );
        let back = utc(3, 9, 0);
        let book = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
            .await
            .unwrap();
        assert!(book.last_handle.is_empty());
        assert_eq!(st.runs.len(), 1, "no second run of the node");
        assert_eq!(
            book.last_skip["f.py:daily"],
            (back.timestamp(), "in_flight", Some("api".to_string()))
        );
        assert_eq!(
            book.last_fired["f.py:daily"],
            back.timestamp(),
            "not caught up again"
        );
    }

    fn alive(run_id: &str, command: &str, job: &str) -> db::AliveRun {
        db::AliveRun {
            run_id: run_id.into(),
            command: command.into(),
            job: job.into(),
        }
    }

    #[test]
    fn a_cli_run_counts_when_its_job_covers_the_node() {
        let daily = job("f.py:daily", NodeKind::Asset, "0 5 * * *");
        let task = job("f.py:t", NodeKind::Task, "0 5 * * *");
        let one = [alive("cli", "get", "f.py:daily")];
        assert_eq!(cli_run_in_flight_for(&one, &daily).as_deref(), Some("cli"));
        let several = [alive("cli", "get", "f.py:a,f.py:daily")];
        assert_eq!(
            cli_run_in_flight_for(&several, &daily).as_deref(),
            Some("cli")
        );
        // `barca get` with no target gets every asset and sensor, not tasks.
        let all = [alive("cli", "get", "all")];
        assert_eq!(cli_run_in_flight_for(&all, &daily).as_deref(), Some("cli"));
        assert_eq!(cli_run_in_flight_for(&all, &task), None);
        let run = [alive("cli", "run", "f.py:t")];
        assert_eq!(cli_run_in_flight_for(&run, &task).as_deref(), Some("cli"));
        // Another node, a prefix of the id, or a row with no job: no.
        for rows in [
            [alive("cli", "get", "f.py:other")],
            [alive("cli", "get", "f.py:dail")],
            [alive("cli", "get", "")],
        ] {
            assert_eq!(cli_run_in_flight_for(&rows, &daily), None);
        }
        // The oldest is named.
        let two = [
            alive("first", "get", "f.py:daily"),
            alive("second", "get", "all"),
        ];
        assert_eq!(
            cli_run_in_flight_for(&two, &daily).as_deref(),
            Some("first")
        );
    }

    #[tokio::test]
    async fn plan_tick_skips_for_a_cli_run_on_the_same_project() {
        let st = app_state();
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let max = jobs[0].max_failures;
        let cli = [alive("cli", "get", "f.py:daily")];
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| any_run_in_flight_for(&st, &cli, j),
            |_| max - 1,
        );
        let [TickAction::Skip { job, handle }] = &plan[..] else {
            panic!("expected a skip");
        };
        assert_eq!(handle, "cli");
        // The skip is not a failed tick: nothing is counted toward `max_failures`.
        let mut book = Bookkeeping::default();
        book.failures.insert(
            job.id.clone(),
            db::ScheduleFailures {
                failures: max - 1,
                ..db::ScheduleFailures::default()
            },
        );
        record_skip(&mut book, job, Some(handle), 1, "scheduled run");
        assert!(!count_outcomes(&st, &jobs, &mut book, None).await);
        assert_eq!(book.failures(job), max - 1);
        // Pause, then failing, come before the in-flight skip.
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| any_run_in_flight_for(&st, &cli, j),
            |_| max,
        );
        assert!(matches!(plan[..], [TickAction::Failing { .. }]));
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| true,
            |j| any_run_in_flight_for(&st, &cli, j),
            |_| max,
        );
        assert!(matches!(plan[..], [TickAction::Paused(_)]));
        // A run the server holds is named first.
        insert_run(
            &st,
            "api",
            RunStatus::Running,
            "get",
            Some("daily"),
            Some("f.py:daily"),
            1.0,
        );
        let plan = plan_tick(
            &at(5, 0),
            &jobs,
            |_| false,
            |j| any_run_in_flight_for(&st, &cli, j),
            |_| 0,
        );
        match &plan[..] {
            [TickAction::Skip { handle, .. }] => assert_eq!(handle, "api"),
            _ => panic!("expected a skip"),
        }
    }

    #[tokio::test]
    async fn a_missed_tick_is_skipped_at_startup_while_a_cli_run_of_its_node_is_going() {
        let (_dir, db_path) = schedule_db().await;
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
        catch_up(
            &app_state(),
            &[],
            &jobs,
            &Zone::Utc,
            &utc(2, 5, 0),
            &db_path,
        )
        .await
        .unwrap();
        // A `barca get daily` from a shell, still going.
        let cli = [alive("cli", "get", "f.py:daily")];
        let st = app_state();
        let back = utc(3, 9, 0);
        let book = catch_up(&st, &cli, &jobs, &Zone::Utc, &back, &db_path)
            .await
            .unwrap();
        assert!(st.runs.is_empty(), "no second run of the node");
        assert_eq!(
            book.last_skip["f.py:daily"],
            (back.timestamp(), "in_flight", Some("cli".to_string()))
        );
    }

    // ─── catch-up across a restart (fixed times, a real schedule_state table) ──

    /// A metadata DB in a temp dir, and the UTC instant `hour:minute` on 2026-07-02.
    async fn schedule_db() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        barca_core::db::init_db(&path).await.unwrap();
        (dir, path)
    }

    fn utc(day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
        Utc.with_ymd_and_hms(2026, 7, day, hour, minute, 0)
            .single()
            .unwrap()
            .fixed_offset()
    }

    #[test]
    fn plan_tick_skips_a_paused_job_before_anything_else() {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:b", NodeKind::Task, "0 5 * * *"),
        ];
        let plan = plan_tick(&at(5, 0), &jobs, |j| j.id == "f.py:a", |_| None, |_| 0);
        assert!(matches!(plan[0], TickAction::Paused(j) if j.id == "f.py:a"));
        assert_eq!(fired_ids(&plan), vec!["f.py:b"]);
    }

    /// A paused job's ticks are skipped as paused, also when it is failing: a paused tick is
    /// not a failed one, and nothing about it is counted.
    #[test]
    fn plan_tick_reports_a_paused_failing_job_as_paused() {
        let jobs = vec![job("f.py:a", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(5, 0), &jobs, |_| true, |_| Some("h".into()), |_| 9);
        assert!(matches!(plan.as_slice(), [TickAction::Paused(j)] if j.id == "f.py:a"));
    }

    #[tokio::test]
    async fn a_paused_job_is_not_caught_up_and_its_skip_says_paused() {
        let (_dir, db_path) = schedule_db().await;
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
        catch_up(
            &app_state(),
            &[],
            &jobs,
            &Zone::Utc,
            &utc(2, 5, 0),
            &db_path,
        )
        .await
        .unwrap();
        // Paused until resumed, across the restart: the missed 06:00 tick is skipped.
        db::set_schedule_pause(
            &db_path,
            "f.py:daily",
            Some(db::SchedulePause {
                paused_at: utc(2, 5, 30).timestamp(),
                until: None,
            }),
            utc(2, 5, 30).timestamp(),
        )
        .await
        .unwrap();
        let st = app_state();
        let back = utc(3, 9, 0);
        let book = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
            .await
            .unwrap();
        assert!(st.runs.is_empty(), "a paused job does not run");
        assert_eq!(
            book.last_skip["f.py:daily"],
            (back.timestamp(), "paused", None)
        );

        // A pause that ended counts from when it ended: the ticks inside it are not caught up.
        let (_dir, db_path) = schedule_db().await;
        catch_up(
            &app_state(),
            &[],
            &jobs,
            &Zone::Utc,
            &utc(2, 5, 0),
            &db_path,
        )
        .await
        .unwrap();
        db::set_schedule_pause(
            &db_path,
            "f.py:daily",
            Some(db::SchedulePause {
                paused_at: utc(2, 5, 30).timestamp(),
                until: Some(utc(4, 7, 0).timestamp()),
            }),
            utc(2, 5, 30).timestamp(),
        )
        .await
        .unwrap();
        let st = app_state();
        let book = catch_up(&st, &[], &jobs, &Zone::Utc, &utc(4, 8, 0), &db_path)
            .await
            .unwrap();
        assert!(
            st.runs.is_empty(),
            "no tick between the end of the pause and now"
        );
        assert!(book.last_skip.is_empty());
        let st = app_state();
        let book = catch_up(&st, &[], &jobs, &Zone::Utc, &utc(5, 8, 0), &db_path)
            .await
            .unwrap();
        assert_eq!(
            book.last_handle.len(),
            1,
            "the 06:00 tick after the pause is caught up"
        );
    }

    #[tokio::test]
    async fn unreadable_schedule_history_never_becomes_an_empty_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let st = app_state();
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
        // A directory cannot be opened as metadata. A failed read must not anchor
        // or trigger jobs as if the durable schedule history were simply absent.
        let error = catch_up(
            &st,
            &[],
            &jobs,
            &Zone::Utc,
            &utc(2, 9, 0),
            &dir.path().display().to_string(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, barca_core::BarcaError::Db(_)));
        assert!(st.runs.is_empty());
        assert!(st.schedule.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_first_start_anchors_every_job_to_now_and_runs_nothing() {
        let (_dir, db_path) = schedule_db().await;
        let st = app_state();
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
        let now = utc(2, 9, 0);

        let Bookkeeping {
            last_handle: handles,
            last_fired: fired,
            ..
        } = catch_up(&st, &[], &jobs, &Zone::Utc, &now, &db_path)
            .await
            .unwrap();

        assert!(handles.is_empty());
        assert!(st.runs.is_empty(), "nothing may run on a first start");
        assert_eq!(
            fired,
            HashMap::from([("f.py:daily".to_string(), now.timestamp())])
        );
        assert_eq!(db::get_schedule_state(&db_path).await.unwrap(), fired);
    }

    #[tokio::test]
    async fn catch_up_does_not_fire_early_across_the_autumn_offset_change() {
        let (_dir, db_path) = schedule_db().await;
        let zone = Zone::parse("America/New_York").unwrap();
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let before = Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap();
        db::upsert_schedule_state(&db_path, "f.py:daily", before.timestamp())
            .await
            .unwrap();
        // 09:30 UTC is 04:30 after the offset changed; the old fixed-offset
        // search incorrectly treated the next 05:00 as 09:00 UTC.
        let early = zone.at(Utc.with_ymd_and_hms(2026, 11, 1, 9, 30, 0).unwrap());
        let st = app_state();
        let Bookkeeping {
            last_handle: handles,
            last_fired: fired,
            ..
        } = catch_up(&st, &[], &jobs, &zone, &early, &db_path)
            .await
            .unwrap();
        assert!(handles.is_empty() && st.runs.is_empty());
        assert_eq!(fired["f.py:daily"], before.timestamp());
        let due = zone.at(Utc.with_ymd_and_hms(2026, 11, 1, 10, 0, 0).unwrap());
        let Bookkeeping {
            last_handle: handles,
            ..
        } = catch_up(&st, &[], &jobs, &zone, &due, &db_path)
            .await
            .unwrap();
        assert_eq!(handles.len(), 1);
    }

    #[tokio::test]
    async fn a_tick_missed_while_the_server_was_down_runs_once_when_it_comes_back() {
        let (_dir, db_path) = schedule_db().await;
        let jobs = [
            job("f.py:daily", NodeKind::Asset, "0 6 * * *"),
            job("f.py:hourly", NodeKind::Task, "0 * * * *"),
        ];
        // Running at 05:10: both jobs are anchored. The server then stops.
        let stopped = utc(2, 5, 10);
        catch_up(&app_state(), &[], &jobs, &Zone::Utc, &stopped, &db_path)
            .await
            .unwrap();

        // Back at 05:50: no tick of either job has passed.
        let st = app_state();
        let early = utc(2, 5, 50);
        let Bookkeeping {
            last_handle: handles,
            last_fired: fired,
            ..
        } = catch_up(&st, &[], &jobs, &Zone::Utc, &early, &db_path)
            .await
            .unwrap();
        assert!(handles.is_empty() && st.runs.is_empty());
        assert_eq!(
            fired["f.py:daily"],
            stopped.timestamp(),
            "the record is not moved"
        );

        // Back three days later: `daily` missed three ticks and `hourly` dozens. Each runs
        // exactly once, and its record moves to the restart time.
        let st = app_state();
        let back = utc(5, 9, 30);
        let Bookkeeping {
            last_handle: handles,
            last_fired: fired,
            ..
        } = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
            .await
            .unwrap();
        assert_eq!(handles.len(), 2);
        assert_eq!(
            st.runs.len(),
            2,
            "one run per job, however many ticks were missed"
        );
        for id in ["f.py:daily", "f.py:hourly"] {
            assert!(st.runs.contains_key(&handles[id]), "{id}");
            assert_eq!(fired[id], back.timestamp(), "{id}");
        }
        assert_eq!(db::get_schedule_state(&db_path).await.unwrap(), fired);

        // A restart right after has nothing left to catch up: the catch-up does not repeat.
        let st = app_state();
        let again = utc(5, 9, 31);
        let Bookkeeping {
            last_handle: handles,
            last_fired: fired,
            ..
        } = catch_up(&st, &[], &jobs, &Zone::Utc, &again, &db_path)
            .await
            .unwrap();
        assert!(handles.is_empty() && st.runs.is_empty());
        assert_eq!(fired["f.py:hourly"], back.timestamp());
    }

    #[tokio::test]
    async fn a_job_added_while_the_server_was_down_is_anchored_not_run() {
        let (_dir, db_path) = schedule_db().await;
        let old = job("f.py:daily", NodeKind::Asset, "0 6 * * *");
        catch_up(
            &app_state(),
            &[],
            std::slice::from_ref(&old),
            &Zone::Utc,
            &utc(2, 5, 0),
            &db_path,
        )
        .await
        .unwrap();

        let st = app_state();
        let jobs = [old, job("f.py:new", NodeKind::Asset, "0 6 * * *")];
        let back = utc(4, 9, 0);
        let Bookkeeping {
            last_handle: handles,
            last_fired: fired,
            ..
        } = catch_up(&st, &[], &jobs, &Zone::Utc, &back, &db_path)
            .await
            .unwrap();

        assert_eq!(handles.keys().collect::<Vec<_>>(), ["f.py:daily"]);
        assert_eq!(st.runs.len(), 1);
        assert_eq!(fired["f.py:new"], back.timestamp());
    }

    // ─── kind-based dispatch ───────────────────────────────────────────────

    #[tokio::test]
    async fn trigger_registers_a_tracked_run_for_asset_and_task() {
        let st = app_state();
        let asset = job("f.py:a", NodeKind::Asset, "* * * * *");
        let task = job("f.py:t", NodeKind::Task, "* * * * *");
        // Both kinds must produce a handle that is registered in the runs map,
        // proving each routes into a real run-trigger path (get vs run).
        let ha = trigger(&st, &asset, 0);
        let ht = trigger(&st, &task, 0);
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

        let jobs = barca_core::schedule::collect_jobs(
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
