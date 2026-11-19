//! Where a schedule stands as a node's row carries it: the one word, the `schedule` block of
//! a scheduled node and the `refresh_path` of a node a schedule reaches.
//! [`crate::schedule_standing`] decides the word and fills the block, [`crate::refresh_path`]
//! the path.

use serde::{Deserialize, Serialize};

/// Where a schedule stands, in one word (`specs/behavior/health.md`, "Schedule state"):
/// what `GET /schedule`, the `schedule` block and a node's `refresh_path` all carry, so no
/// surface words a schedule on its own. The first that applies, in this order.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    /// Paused (`barca pause --schedule`): its ticks are skipped until it is resumed.
    Paused,
    /// Held after `max_failures` failed ticks in a row: its ticks are skipped until a run of
    /// a node it failed on succeeds or an input of one changes. It is not late and has no
    /// next run.
    Held,
    /// A slot passed with no decision recorded for it, longer ago than its grace and queue
    /// wait ([`crate::schedule_standing::Standing::late`]): no scheduler is firing it.
    Late,
    /// Its runs outlast its interval: at least [`crate::schedule_standing::OVERRUN_SKIPS`] of its newest decided slots
    /// were skipped because its own run was still in flight. The scheduler is running; the
    /// schedule cannot keep its cron.
    Overrunning,
    /// None of the above.
    #[default]
    OnTime,
}

/// The state of one scheduled node's schedule. With a server it is what the server's scheduler
/// reports (`GET /schedule`, the numbers the web UI shows); on this machine there is no
/// scheduler, so it is read from the metadata DB, which holds what a `barca serve` of this
/// `.barca/` recorded (failures, pauses, the last tick), and `live` is false.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ScheduleStatus {
    /// The cron expression.
    pub cron: String,
    /// When the cron expression next matches (RFC 3339, UTC): in the server's `--timezone` from
    /// a server, in this machine's local time otherwise, as `barca list` computes it.
    pub next_fire: Option<String>,
    /// The newest tick the scheduler fired (RFC 3339, UTC): the slot of the newest run the
    /// schedule started or queued. `null` when none is recorded.
    pub last_fired: Option<String>,
    /// Status of the run that tick started, in the words of `barca history`: `pending` while
    /// it waits in the server's run queue, then `running`, `success`, `failed`, `cancelled`,
    /// `error` or `interrupted`.
    pub last_status: Option<String>,
    /// The run that tick started (`barca history --run <id>`).
    pub last_run_id: Option<String>,
    /// Where it stands, in one word: `paused`, `held` (after `max_failures` failed ticks),
    /// `late`, `overrunning` (its runs outlast its interval) or `on_time`
    /// ([`crate::schedule_standing::ScheduleState`]). The word every surface shows.
    #[serde(default)]
    pub state: super::ScheduleState,
    /// The fact behind `state` in a line, with what ends it; `null` for `on_time` and
    /// `paused`.
    #[serde(default)]
    pub state_detail: Option<String>,
    /// A slot passed with nothing decided for it, longer ago than its grace plus its queue
    /// wait ([`crate::schedule_standing::standing`]). Never while paused or held.
    pub late: bool,
    /// The first slot that has passed after the newest slot the scheduler decided (fired, or
    /// skipped because its run was still in flight) (RFC 3339, UTC); `null` while on time,
    /// while paused or held and when no tick is recorded.
    pub late_since: Option<String>,
    /// Consecutive failed ticks.
    pub failures: u32,
    /// `Schedule(..., max_failures=N)`, default 5.
    pub max_failures: u32,
    /// `failures >= max_failures`: ticks are skipped until an explicit request or a change in
    /// the node's inputs.
    pub failing: bool,
    /// Held back by `POST /schedule/{id}/pause`.
    pub paused: bool,
    /// When a timed pause ends (RFC 3339, UTC); `null` for a pause until resumed.
    pub paused_until: Option<String>,
    /// Why the last due tick was skipped: `in_flight` or `paused` (server only).
    pub skip_reason: Option<String>,
    /// A scheduler is firing the schedule: the answering server's own, or, where none runs
    /// (`barca status` without `--server`, a `--read-only` or `--no-schedule` server), one
    /// the metadata DB shows deciding its slots ([`crate::liveness::observe`]). False when
    /// none does: it stopped, it left a slot undecided past the grace, or none ever ran.
    pub live: bool,
    /// How long its ticks' runs typically wait for a run slot, whole seconds: the median
    /// queue wait of its newest 20 fired ticks; 0 with none recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub typical_queue_wait_seconds: u64,
    /// How long its ticks' runs typically take, start to end, whole seconds: the median of
    /// its newest 20 fired ticks whose run succeeded; `null` with none recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub typical_run_seconds: Option<u64>,
    /// Its typical queue wait exceeds its interval: its ticks wait for a run slot longer than
    /// they are apart (`--max-runs`).
    #[serde(default)]
    pub queue_bound: bool,
}

impl ScheduleStatus {
    /// A schedule that needs attention: paused, or skipped after `max_failures` failed ticks.
    pub(crate) fn needs_attention(&self) -> bool {
        self.paused || self.failing
    }

    /// The table's SCHEDULE cell: `*/5 * * * *; held, failing 3/3`, `0 6 * * *; paused`,
    /// `*/10 * * * * *; overrunning, next ...`,
    /// `0 6 * * *; next 2026-10-23 06:00:00 UTC`,
    /// `0 6 * * *; late since 2026-10-23 06:00:00 UTC, next 2026-10-24 06:00:00 UTC`.
    pub fn cell(&self) -> String {
        let state = if self.paused {
            match &self.paused_until {
                Some(until) => format!("paused until {}", crate::text::utc_cell(until)),
                None => "paused".to_string(),
            }
        } else if self.failing {
            format!("held, failing {}/{}", self.failures, self.max_failures)
        } else {
            let next = self
                .next_fire
                .as_deref()
                .map(|t| format!("next {}", crate::text::utc_cell(t)))
                .unwrap_or_else(|| "no next fire".to_string());
            let next = match self.late_since.as_deref().filter(|_| self.late) {
                Some(since) => format!("late since {}, {next}", crate::text::utc_cell(since)),
                None if self.state == super::ScheduleState::Overrunning => {
                    format!("overrunning, {next}")
                }
                None => next,
            };
            match self.failures {
                0 => next,
                1 => format!("1 failure, {next}"),
                n => format!("{n} failures, {next}"),
            }
        };
        format!("{}; {state}", self.cron)
    }
}

/// `refresh_path`: the schedule whose tick reaches the node as a target. With several, the
/// one that fires earliest next (a schedule with no next tick last, ties by schedule id).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RefreshPath {
    /// The scheduled node's id.
    pub schedule: String,
    /// The schedule's next slot (RFC 3339, UTC); the first slot after a timed pause ends;
    /// `null` for a pause until resumed and when `live` is false.
    pub next_tick: Option<String>,
    /// The schedule is paused (`POST /schedule/{id}/pause`).
    pub paused: bool,
    /// A running scheduler fires the schedule: the answering server's own, or one the
    /// metadata DB shows deciding its slots. False when none does and for a schedule held
    /// after `max_failures` failed ticks.
    pub live: bool,
    /// Where the schedule stands, in one word (`schedule.state`): `paused`, `held`, `late`,
    /// `overrunning` or `on_time`.
    #[serde(default)]
    pub state: super::ScheduleState,
    /// How many finished ticks of the node's path had it as a target and did not leave it
    /// fresh (`failed` or `not_reached`), counted back from the newest to the last one that
    /// found it up to date or rebuilt it, and only those that ended after its newest
    /// successful result. 0 for a fresh node. Bounded by the tick record: the newest 20
    /// fired ticks of each schedule keep their targets.
    pub missed_ticks: u32,
}

impl RefreshPath {
    /// What will refresh the node, for a table cell: `next tick 15:05 UTC by inbox`,
    /// `inbox is paused`, `inbox is not firing`; with `, missed 2 ticks` when it has. `now`
    /// (unix seconds) decides whether the day is named.
    pub fn line(&self, now: i64) -> String {
        let by = crate::targets::short_name(&self.schedule);
        let next = self
            .next_tick
            .as_deref()
            .and_then(crate::time::parse_rfc3339)
            .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
            .map(|at| {
                let today = chrono::DateTime::from_timestamp(now, 0)
                    .is_some_and(|now| now.date_naive() == at.date_naive());
                at.format(if today {
                    "%H:%M UTC"
                } else {
                    "%Y-%m-%d %H:%M UTC"
                })
                .to_string()
            });
        use super::ScheduleState;
        let path = match (self.paused, self.live, next) {
            (true, _, Some(next)) => format!("{by} is paused, next tick {next}"),
            (true, _, None) => format!("{by} is paused"),
            (false, true, Some(next)) if self.state == ScheduleState::Overrunning => {
                format!("next tick {next} by {by}, which is overrunning")
            }
            (false, true, Some(next)) => format!("next tick {next} by {by}"),
            (false, _, _) if self.state == ScheduleState::Held => {
                format!("{by} is held after failed ticks")
            }
            (false, _, _) => format!("{by} is not firing"),
        };
        match self.missed_ticks {
            0 => path,
            1 => format!("{path}, missed 1 tick"),
            n => format!("{path}, missed {n} ticks"),
        }
    }
}
