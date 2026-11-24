//! `GET /schedule`: one entry per scheduled node ([`ScheduleJob`]).
//!
//! The same walk as every other surface ([`crate::refresh_path::schedule_views`]), so `state`,
//! `late` and the rest agree with the `schedule` block of `GET /state`
//! ([`crate::status::ScheduleStatus`]). All public schedule timestamps use RFC 3339 UTC.
//! This route also carries the scheduler's last skipped tick and the legacy `last_run` alias.

use crate::NodeKind;
use crate::refresh_path::ScheduleInputs;
use crate::schedule_standing::ScheduleState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Why a scheduler skipped a schedule's last due tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// A run of its node was still in flight.
    InFlight,
    /// The schedule, or its node, was paused.
    Paused,
}

impl SkipReason {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "in_flight" => Some(SkipReason::InFlight),
            "paused" => Some(SkipReason::Paused),
            _ => None,
        }
    }
}

/// What a scheduler in this process says of the last due tick it skipped for one schedule.
/// Kept in its memory only: a restart clears it, and a host without a scheduler has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobSkips {
    /// When (unix seconds).
    pub last_skipped: Option<i64>,
    /// The run in flight that caused it; `None` for a paused schedule.
    pub skipped_for: Option<String>,
    pub skip_reason: Option<SkipReason>,
}

/// One entry of `GET /schedule`. Times are RFC 3339 UTC.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ScheduleJob {
    pub cron: String,
    /// `failures >= max_failures`: ticks are skipped until an explicit request or a change in
    /// the node's inputs.
    #[serde(default)]
    pub failing: bool,
    /// Consecutive failed ticks (RFC-0008); kept across restarts.
    #[serde(default)]
    pub failures: u32,
    /// The scheduled node's full id.
    pub id: String,
    pub kind: NodeKind,
    /// The newest tick the scheduler fired.
    pub last_fired: Option<String>,
    /// `last_run_id` under its older name.
    pub last_run: Option<String>,
    /// The run that tick started.
    pub last_run_id: Option<String>,
    /// When a due tick was last skipped; `null` after a restart.
    pub last_skipped: Option<String>,
    /// That run's status, in the run list's words (`pending` while it waits in the run queue).
    pub last_status: Option<String>,
    /// Explicit task reason from the newest fired run; separate from scheduler decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_task_skip_reason: Option<String>,
    /// The server's one answer to "is it late" (`barca docs scheduling`).
    #[serde(default)]
    pub late: bool,
    /// The first slot after `last_fired` that has passed; `null` while on time or paused.
    pub late_since: Option<String>,
    /// `Schedule(cron, max_failures=N)`, default 5.
    #[serde(default)]
    pub max_failures: u32,
    /// The next slot, in the zone the scheduler evaluates cron in.
    pub next_fire: Option<String>,
    #[serde(default)]
    pub paused: bool,
    /// When a timed pause ends; `null` for an open-ended pause.
    pub paused_until: Option<String>,
    /// Its typical queue wait exceeds its interval (#962).
    #[serde(default)]
    pub queue_bound: bool,
    /// Why the last due tick was skipped.
    pub skip_reason: Option<SkipReason>,
    /// The run in flight the last due tick was skipped for.
    pub skipped_for: Option<String>,
    /// The one word for where it stands, and the fact behind it.
    #[serde(default)]
    pub state: ScheduleState,
    pub state_detail: Option<String>,
    /// How long its ticks' runs typically wait for a run slot, whole seconds.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub typical_queue_wait_seconds: u64,
    /// How long its ticks' runs typically take, whole seconds; `null` with none recorded.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub typical_run_seconds: Option<u64>,
}

/// The answer of `POST /schedule/{id}/pause` and `/resume`: the schedule's entry, or, while no
/// scheduler has published the job (it is off, or still starting), what the pause is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(untagged)]
pub enum SchedulePauseAnswer {
    Job(Box<ScheduleJob>),
    Pause {
        id: String,
        paused: bool,
        /// RFC 3339 UTC; `null` for an open-ended pause.
        paused_until: Option<String>,
    },
}

/// One [`ScheduleJob`] per job of `inputs`, in their order. `skips`: what a scheduler here
/// says of each job's last skipped tick; empty where none runs.
pub fn schedule_jobs(
    inputs: ScheduleInputs<'_>,
    skips: &HashMap<String, JobSkips>,
) -> Vec<ScheduleJob> {
    let views = crate::refresh_path::schedule_views(inputs);
    inputs
        .jobs
        .iter()
        .filter_map(|job| {
            let view = views.get(&job.id)?;
            let (standing, block) = (&view.standing, &view.block);
            let skip = skips.get(&job.id);
            Some(ScheduleJob {
                state: block.state,
                state_detail: block.state_detail.clone(),
                id: job.id.clone(),
                cron: job.cron_str.clone(),
                kind: job.kind,
                next_fire: standing.next_fire.and_then(crate::time::rfc3339_secs),
                last_fired: standing.last_fired.and_then(crate::time::rfc3339_secs),
                last_run_id: standing.last_run_id.clone(),
                last_run: standing.last_run_id.clone(),
                last_status: standing.last_status.clone(),
                last_task_skip_reason: inputs
                    .ticks
                    .get(&job.id)
                    .and_then(|t| {
                        let run = t.newest.as_ref()?;
                        let later_decision = t.decisions.first().is_some_and(|(at, decision)| {
                            *at >= run.scheduled_at && *decision != crate::db::TickDecision::Fired
                        });
                        let later_skip = skip
                            .and_then(|s| s.last_skipped)
                            .is_some_and(|at| at >= run.scheduled_at);
                        (!later_decision && !later_skip && run.status == "skipped").then_some(run)
                    })
                    .and_then(|r| r.task_skip_reason.clone()),
                late: standing.late,
                late_since: standing.late_since.and_then(crate::time::rfc3339_secs),
                typical_queue_wait_seconds: block.typical_queue_wait_seconds,
                typical_run_seconds: block.typical_run_seconds,
                queue_bound: standing.queue_bound,
                last_skipped: skip
                    .and_then(|s| s.last_skipped)
                    .and_then(crate::time::rfc3339_secs),
                skipped_for: skip.and_then(|s| s.skipped_for.clone()),
                skip_reason: skip.and_then(|s| s.skip_reason),
                paused: block.paused,
                paused_until: inputs
                    .pauses
                    .get(&job.id)
                    .filter(|_| block.paused)
                    .and_then(|p| p.until)
                    .and_then(crate::time::rfc3339_secs),
                failures: block.failures,
                max_failures: block.max_failures,
                failing: block.failing,
            })
        })
        .collect()
}
