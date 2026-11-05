//! One tick: which jobs are due, and whether each fires or is skipped because it is paused,
//! failing, or its node already has a run in flight (the server's or a CLI run on the same
//! project).

use crate::state::{AppState, RunStatus};
use barca_core::schedule::{ScheduledJob, Zone};
use barca_core::{NodeKind, db};
use chrono::{DateTime, TimeZone, Timelike};
use std::time::Duration;

/// Pure eligibility check: which jobs fire at `now`? Split out so it can be
/// unit-tested against fixed timestamps without a running server or wall clock.
///
/// Matching is at whole-second resolution: the tick loop wakes just after each
/// second boundary, so we drop only the sub-millisecond remainder and match the
/// wall-clock second directly. A 6-field cron fires at its declared seconds; a
/// 5-field cron has its seconds field pinned to `0` by the parser, so it still
/// matches only at second `0` of a matching minute.
pub(super) fn due_jobs<'a, Tz: TimeZone>(
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
pub(super) fn millis_to_next_second<Tz: TimeZone>(now: &DateTime<Tz>) -> u64 {
    let elapsed_ms = now.nanosecond() as u64 / 1_000_000;
    1_000u64.saturating_sub(elapsed_ms) + 5
}

/// Sleep until just after the next second boundary in the scheduler's timezone.
pub(super) async fn sleep_to_next_second(zone: &Zone) {
    tokio::time::sleep(Duration::from_millis(millis_to_next_second(&zone.now()))).await;
}

/// What the scheduler decided to do with a due job on a given tick.
pub(super) enum TickAction<'a> {
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
pub(super) fn plan_tick<'a, Tz: TimeZone>(
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
pub(super) fn run_covers(command: &str, nodes: &[String], job: &ScheduledJob) -> bool {
    if nodes.is_empty() {
        command == "get" && job.kind != NodeKind::Task
    } else {
        nodes.contains(&job.id)
    }
}

/// The handle of the oldest pending or running run that covers `job` ([`run_covers`]),
/// whoever started it: the scheduler or an HTTP request.
pub(super) fn run_in_flight_for(state: &AppState, job: &ScheduledJob) -> Option<String> {
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
pub(super) fn cli_run_in_flight_for(alive: &[db::AliveRun], job: &ScheduledJob) -> Option<String> {
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
pub(super) fn any_run_in_flight_for(
    state: &AppState,
    cli: &[db::AliveRun],
    job: &ScheduledJob,
) -> Option<String> {
    run_in_flight_for(state, job).or_else(|| cli_run_in_flight_for(cli, job))
}
