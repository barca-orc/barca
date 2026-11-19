//! Where a schedule stands: its last tick, the run that tick started, its next slot and whether
//! it is late (`specs/behavior/health.md`, "What estimates change"; #958).
//!
//! [`standing`] is the one definition. `GET /schedule`, the schedule fields of `GET /state`, the
//! `schedule` block of `GET /nodes` and `barca status --json` all call it over the same
//! [`recorded`] ticks, so no two of them can disagree about a schedule; the web UI draws what
//! they say and derives nothing.
//!
//! What a tick is read from: the run queue (a tick whose run waits for a run slot has no
//! `runs` row yet), the `runs` rows a schedule's ticks started, and the tick record
//! (`schedule_ticks`, #960) for how long its runs waited and took ([`Ticks::queue_waits`],
//! [`Ticks::run_seconds`]; #962).

use crate::run_queue::{self, QueueState};
use crate::schedule::{Zone, next_fire, prev_fire};
use crate::time::rfc3339_secs;
pub use crate::views::ScheduleState;
use crate::views::ScheduleStatus;
use crate::{BarcaError, db};
use chrono::{DateTime, Utc};
use croner::Cron;
use std::collections::HashMap;

/// The run a schedule's tick started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickRun {
    pub run_id: String,
    /// The tick (unix seconds): the slot the scheduler fired it for.
    pub scheduled_at: i64,
    /// The run's status, in the words of `GET /runs`: `pending` while it waits in the run
    /// queue, then `running`, `success`, `failed`, `cancelled`, `error` or `interrupted`.
    pub status: String,
    /// When the run queue accepted it (unix seconds); `None` once its queue row is gone.
    pub queued_at: Option<i64>,
    /// When it started executing; `None` while it waits.
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl TickRun {
    /// How long the run waited for a run slot, seconds: up to `now` while it still waits.
    fn queue_wait(&self, now: i64) -> i64 {
        let from = self.queued_at.unwrap_or(self.scheduled_at);
        let until = match self.started_at {
            Some(started) => started,
            None if self.status == "pending" => now,
            None => from,
        };
        (until - from).max(0)
    }
}

/// What is recorded of one schedule's ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ticks {
    /// The newest tick the scheduler fired, with its run; `None` when none is recorded.
    pub newest: Option<TickRun>,
    /// Queue waits (seconds) of its newest fired ticks that started: the tick record's
    /// `queue_wait_seconds` (from joining the run queue to starting, at most
    /// [`db::TICKS_WITH_TARGETS`]); for a schedule with no tick record that says, the `runs`
    /// rows its ticks started (slot to start, at most [`db::SCHEDULED_RUNS_WINDOW`]).
    pub queue_waits: Vec<f64>,
    /// How long the runs of its newest fired ticks that succeeded took, start to end, seconds
    /// (the tick record; at most [`db::TICKS_WITH_TARGETS`]).
    pub run_seconds: Vec<f64>,
    /// What the scheduler decided for its newest due slots, as `(slot, decision)`, newest
    /// first (the tick record; at most [`db::TICKS_WITH_TARGETS`]): a slot it fired, and
    /// equally one it skipped because the schedule's run was still in flight, because the
    /// schedule was paused or because it is held after `max_failures` failed ticks.
    pub decisions: Vec<(i64, db::TickDecision)>,
}

/// How many of a schedule's newest decided slots ([`Ticks::decisions`]) were skipped because
/// its own run was still in flight before the schedule is [`ScheduleState::Overrunning`]:
/// one skip is a run that took long once (or a run someone asked for), three are a schedule
/// that cannot keep its cron.
pub const OVERRUN_SKIPS: usize = 3;

impl ScheduleState {
    /// The state of a schedule that stands as `standing`, paused or not, with `failures`
    /// failed ticks in a row of the `max_failures` it is held at.
    pub fn of(standing: &Standing, paused: bool, (failures, max_failures): (u32, u32)) -> Self {
        if paused {
            ScheduleState::Paused
        } else if failures >= max_failures {
            ScheduleState::Held
        } else if standing.late {
            ScheduleState::Late
        } else if standing.overrunning {
            ScheduleState::Overrunning
        } else {
            ScheduleState::OnTime
        }
    }

    /// The fact behind the word in a line, with what ends it; `None` for a schedule on time
    /// and a paused one (its pause says until when). `name` is the scheduled node's.
    pub fn detail(
        self,
        name: &str,
        standing: &Standing,
        (failures, _): (u32, u32),
        task: bool,
    ) -> Option<String> {
        match self {
            ScheduleState::Held => Some(format!(
                "held after {failures} failed {}: its ticks are skipped until a run of it \
                 succeeds (barca {} {name}) or an input of a node it failed on changes",
                if failures == 1 { "tick" } else { "ticks" },
                if task { "run" } else { "get" },
            )),
            ScheduleState::Late => standing.late_since.map(|since| {
                format!(
                    "no tick has been decided since the {} slot: no scheduler is firing it",
                    crate::time::rfc3339_secs(since).unwrap_or_default()
                )
            }),
            ScheduleState::Overrunning => Some(format!(
                "its runs outlast its interval: {} of its newest {} slots were skipped while \
                 its run was still in flight{}",
                standing.skipped_in_flight,
                standing.decided,
                match (standing.typical_run_seconds, standing.interval_seconds) {
                    (Some(run), Some(interval)) => format!(
                        " (a run takes about {}, a slot comes every {})",
                        crate::text::span(run.round() as i64),
                        crate::text::span(interval)
                    ),
                    _ => String::new(),
                }
            )),
            ScheduleState::Paused | ScheduleState::OnTime => None,
        }
    }
}

/// The ticks recorded in the metadata database at `db_path` for each of `schedules` (scheduled
/// node ids). A schedule with none recorded is absent.
pub async fn recorded(
    db_path: &str,
    schedules: &[String],
) -> Result<HashMap<String, Ticks>, BarcaError> {
    let mut out: HashMap<String, Ticks> = HashMap::new();
    for (schedule, runs) in db::scheduled_runs(db_path, schedules).await? {
        let ticks = out.entry(schedule).or_default();
        ticks.queue_waits = runs
            .iter()
            .filter_map(|r| Some((r.started_at? - r.scheduled_at).max(0) as f64))
            .collect();
        ticks.newest = runs.into_iter().next().map(|r| TickRun {
            run_id: r.run_id,
            scheduled_at: r.scheduled_at,
            status: r.status,
            queued_at: None,
            started_at: r.started_at,
            finished_at: r.finished_at,
        });
    }
    // The tick record says how long each tick's run waited in the queue and how long it took:
    // it outlives the `runs` rows, which are trimmed.
    for (schedule, timings) in db::tick_timings(db_path).await? {
        if !schedules.contains(&schedule) {
            continue;
        }
        let ticks = out.entry(schedule).or_default();
        if !timings.queue_waits.is_empty() {
            ticks.queue_waits = timings.queue_waits;
        }
        ticks.run_seconds = timings.run_seconds;
        ticks.decisions = timings.decisions;
    }
    // The run queue: a tick that waits for a run slot is only here, and a run that has started
    // is here too until it records how it ended.
    for row in run_queue::list(db_path).await? {
        let Ok(spec) = serde_json::from_str::<serde_json::Value>(&row.spec) else {
            continue;
        };
        let trigger = &spec["trigger"];
        let (Some(schedule), Some(tick)) = (
            trigger["schedule_id"].as_str(),
            trigger["scheduled_at"]
                .as_str()
                .and_then(crate::time::parse_rfc3339),
        ) else {
            continue;
        };
        if !schedules.iter().any(|s| s == schedule) {
            continue;
        }
        let queued_at = crate::time::parse_rfc3339(&row.queued_at);
        let ticks = out.entry(schedule.to_string()).or_default();
        match &mut ticks.newest {
            Some(run) if run.run_id == row.run_id => run.queued_at = queued_at,
            Some(run) if run.scheduled_at >= tick => {}
            newest => {
                *newest = Some(TickRun {
                    run_id: row.run_id,
                    scheduled_at: tick,
                    status: match row.state {
                        QueueState::Queued => "pending",
                        QueueState::Running | QueueState::Draining => "running",
                    }
                    .into(),
                    queued_at,
                    started_at: None,
                    finished_at: None,
                });
            }
        }
    }
    Ok(out)
}

/// Where a schedule stands as of some moment ([`standing`]). Times are unix seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Standing {
    /// The most recent slot at or before now.
    pub prev_fire: Option<i64>,
    /// The next slot.
    pub next_fire: Option<i64>,
    /// The newest tick the scheduler fired (the slot); `None` when none is recorded.
    pub last_fired: Option<i64>,
    /// The run that tick started.
    pub last_run_id: Option<String>,
    /// That run's status ([`TickRun::status`]).
    pub last_status: Option<String>,
    /// The first slot that has passed after the newest slot the scheduler decided (it fired
    /// it, or skipped it because the schedule's run was still in flight): a slot nothing was
    /// decided for. `None` while on time, while paused or held as failing, and when no tick
    /// is recorded (a schedule that never fired is not late: the scheduler anchors a new job
    /// at its first start).
    pub late_since: Option<i64>,
    /// `late_since` is further back than the grace plus the queue wait.
    pub late: bool,
    /// How long after a slot a tick may fire before the schedule is late.
    pub grace_seconds: i64,
    /// The median of [`Ticks::queue_waits`], 0 with none.
    pub typical_queue_wait_seconds: f64,
    /// The median of [`Ticks::run_seconds`]; `None` with none.
    pub typical_run_seconds: Option<f64>,
    /// From the last slot to the next, seconds; `None` when the cron has no such pair.
    pub interval_seconds: Option<i64>,
    /// Queue-bound: its typical queue wait exceeds its interval, so its ticks wait for a run
    /// slot longer than they are apart.
    pub queue_bound: bool,
    /// How many of its newest decided slots ([`Ticks::decisions`]) were skipped because its
    /// own run was still in flight, and how many slots that is of.
    pub skipped_in_flight: usize,
    pub decided: usize,
    /// At least [`OVERRUN_SKIPS`] of them were: its runs outlast its interval.
    pub overrunning: bool,
}

/// The grace: 60 seconds, or half the interval of a schedule more frequent than every two
/// minutes (at least one second).
pub(crate) fn grace_seconds(prev: Option<i64>, next: Option<i64>) -> i64 {
    match (prev, next) {
        (Some(prev), Some(next)) if next > prev => ((next - prev) / 2).clamp(1, 60),
        _ => 60,
    }
}

pub(crate) fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

fn interval_between(prev: Option<i64>, next: Option<i64>) -> Option<i64> {
    match (prev, next) {
        (Some(prev), Some(next)) if next > prev => Some(next - prev),
        _ => None,
    }
}

/// How far apart a schedule's slots are as of `now`, in seconds: from the slot before `now`
/// to the one after it, what [`Standing::interval_seconds`] holds. `None` for a cron with no
/// slot on one side.
pub fn interval_seconds(cron: &Cron, zone: &Zone, now: DateTime<Utc>) -> Option<i64> {
    interval_between(
        prev_fire(cron, zone, now).map(|t| t.timestamp()),
        next_fire(cron, zone, now).map(|t| t.timestamp()),
    )
}

/// The one definition of a schedule's last tick, last run, next slot and lateness.
///
/// A schedule is late when it is not `stopped`, a slot after the newest slot the scheduler
/// decided has passed (`late_since`), and that was longer ago than its grace plus its queue
/// wait. The queue wait is the typical one (the median of `ticks.queue_waits`) or, when
/// longer, the newest tick's own: a tick whose run waits for a run slot is not late for
/// waiting; the run queue is what is behind.
///
/// `stopped`: no tick of it is expected, because it is paused or held after `max_failures`
/// failed ticks. Such a schedule is never late.
///
/// A decided slot is one the scheduler fired or skipped because the schedule's own run was
/// still in flight ([`Ticks::decisions`]): a schedule whose run outlasts its interval skips
/// slots, and each skip is the scheduler at work, not a missed tick. A slot skipped for a
/// pause or a hold that has since ended counts too: the schedule owes nothing for it.
pub fn standing(
    cron: &Cron,
    zone: &Zone,
    now: DateTime<Utc>,
    stopped: bool,
    ticks: Option<&Ticks>,
) -> Standing {
    let at = now.timestamp();
    let prev_fire = prev_fire(cron, zone, now).map(|t| t.timestamp());
    let next = next_fire(cron, zone, now).map(|t| t.timestamp());
    let newest = ticks.and_then(|t| t.newest.as_ref());
    let decisions = ticks.map_or(&[][..], |t| &t.decisions);
    let decided_slot = decisions.iter().map(|(slot, _)| *slot).max();
    let skipped_in_flight = decisions
        .iter()
        .filter(|(_, decision)| *decision == db::TickDecision::Skipped)
        .count();
    let late_since = newest
        .map(|run| run.scheduled_at)
        .into_iter()
        .chain(decided_slot)
        .max()
        .filter(|_| !stopped)
        .and_then(|slot| DateTime::from_timestamp(slot, 0))
        .and_then(|tick| next_fire(cron, zone, tick))
        .map(|t| t.timestamp())
        .filter(|first| *first <= at);
    let grace_seconds = grace_seconds(prev_fire, next);
    let typical_queue_wait_seconds = median(ticks.map_or(&[], |t| &t.queue_waits));
    let own_wait = newest.map_or(0, |run| run.queue_wait(at)) as f64;
    let allowance = grace_seconds as f64 + typical_queue_wait_seconds.max(own_wait);
    let run_seconds = ticks.map_or(&[][..], |t| &t.run_seconds);
    let interval_seconds = interval_between(prev_fire, next);
    Standing {
        prev_fire,
        next_fire: next,
        last_fired: newest.map(|run| run.scheduled_at),
        last_run_id: newest.map(|run| run.run_id.clone()),
        last_status: newest.map(|run| run.status.clone()),
        late_since,
        late: late_since.is_some_and(|since| (at - since) as f64 > allowance),
        grace_seconds,
        typical_queue_wait_seconds,
        typical_run_seconds: (!run_seconds.is_empty()).then(|| median(run_seconds)),
        interval_seconds,
        queue_bound: interval_seconds
            .is_some_and(|interval| typical_queue_wait_seconds > interval as f64),
        skipped_in_flight,
        decided: decisions.len(),
        overrunning: skipped_in_flight >= OVERRUN_SKIPS,
    }
}

impl ScheduleStatus {
    /// `job`'s schedule as it stands (`standing`), with its failed ticks `(failures,
    /// max_failures)` and its pause when one is on. Not `live`: the caller says whether a
    /// scheduler fires it (its own, or [`crate::liveness::observe`]).
    pub fn of(
        job: &crate::schedule::ScheduledJob,
        standing: crate::schedule_standing::Standing,
        (failures, max_failures): (u32, u32),
        pause: Option<&db::SchedulePause>,
    ) -> Self {
        let state = crate::schedule_standing::ScheduleState::of(
            &standing,
            pause.is_some(),
            (failures, max_failures),
        );
        Self {
            state,
            state_detail: state.detail(
                crate::targets::short_name(&job.id),
                &standing,
                (failures, max_failures),
                job.kind == crate::NodeKind::Task,
            ),
            cron: job.cron_str.clone(),
            next_fire: standing.next_fire.and_then(rfc3339_secs),
            last_fired: standing.last_fired.and_then(rfc3339_secs),
            last_status: standing.last_status,
            last_run_id: standing.last_run_id,
            late: standing.late,
            late_since: standing.late_since.and_then(rfc3339_secs),
            failures,
            max_failures,
            failing: failures >= max_failures,
            paused: pause.is_some(),
            paused_until: pause.and_then(|p| p.until).and_then(rfc3339_secs),
            skip_reason: None,
            live: false,
            typical_queue_wait_seconds: standing.typical_queue_wait_seconds.round() as u64,
            typical_run_seconds: standing.typical_run_seconds.map(|s| s.round() as u64),
            queue_bound: standing.queue_bound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CronExpr;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn secs(s: &str) -> i64 {
        at(s).timestamp()
    }

    fn run(tick: &str, status: &str, queued: Option<&str>, started: Option<&str>) -> Ticks {
        Ticks {
            newest: Some(TickRun {
                run_id: "r1".into(),
                scheduled_at: secs(tick),
                status: status.into(),
                queued_at: queued.map(secs),
                started_at: started.map(secs),
                finished_at: None,
            }),
            queue_waits: Vec::new(),
            run_seconds: Vec::new(),
            decisions: Vec::new(),
        }
    }

    #[test]
    fn a_schedule_is_late_from_the_first_slot_after_its_last_fired_tick() {
        let cron = CronExpr::parse("*/5 * * * *").unwrap();
        let now = at("2026-11-16T12:21:30Z");
        let done = |tick: &str| run(tick, "success", None, Some(tick));
        // The 12:20 tick fired: on time, its last slot 12:20 and its next 12:25.
        let on_time = standing(
            &cron,
            &Zone::Utc,
            now,
            false,
            Some(&done("2026-11-16T12:20:00Z")),
        );
        assert_eq!(on_time.prev_fire, Some(secs("2026-11-16T12:20:00Z")));
        assert_eq!(on_time.next_fire, Some(secs("2026-11-16T12:25:00Z")));
        assert_eq!(on_time.last_fired, Some(secs("2026-11-16T12:20:00Z")));
        assert_eq!((on_time.late_since, on_time.late), (None, false));
        // The newest tick that fired was 12:00: every slot from 12:05 was missed.
        let behind = standing(
            &cron,
            &Zone::Utc,
            now,
            false,
            Some(&done("2026-11-16T12:00:00Z")),
        );
        assert_eq!(behind.late_since, Some(secs("2026-11-16T12:05:00Z")));
        assert!(behind.late);
        assert_eq!(behind.grace_seconds, 60);
        // Paused: not late, and no missed slot is named.
        let paused = standing(
            &cron,
            &Zone::Utc,
            now,
            true,
            Some(&done("2026-11-16T12:00:00Z")),
        );
        assert_eq!((paused.late_since, paused.late), (None, false));
        // Never fired: not late.
        let never = standing(&cron, &Zone::Utc, at("2026-11-16T12:25:00Z"), false, None);
        assert_eq!(never.prev_fire, Some(secs("2026-11-16T12:25:00Z")));
        assert_eq!(
            (never.late_since, never.late, never.last_fired),
            (None, false, None)
        );
    }

    #[test]
    fn the_grace_is_a_minute_or_half_a_short_interval() {
        let every_minute = CronExpr::parse("* * * * *").unwrap();
        let done = run(
            "2026-11-16T12:00:00Z",
            "success",
            None,
            Some("2026-11-16T12:00:00Z"),
        );
        let stand = |now: &str| standing(&every_minute, &Zone::Utc, at(now), false, Some(&done));
        assert_eq!(stand("2026-11-16T12:01:30Z").grace_seconds, 30);
        // The 12:01 slot is missed from 12:01; late once that is more than 30 seconds ago.
        assert!(!stand("2026-11-16T12:01:30Z").late);
        assert_eq!(
            stand("2026-11-16T12:01:30Z").late_since,
            Some(secs("2026-11-16T12:01:00Z"))
        );
        assert!(stand("2026-11-16T12:01:31Z").late);
    }

    #[test]
    fn a_tick_waiting_in_the_run_queue_is_not_late_for_waiting() {
        let cron = CronExpr::parse("* * * * *").unwrap();
        let now = at("2026-11-16T12:10:00Z");
        // The 12:00 tick still waits for a run slot: ten slots have passed, the queue is behind.
        let queued = run(
            "2026-11-16T12:00:00Z",
            "pending",
            Some("2026-11-16T12:00:00Z"),
            None,
        );
        let s = standing(&cron, &Zone::Utc, now, false, Some(&queued));
        assert_eq!(s.last_status.as_deref(), Some("pending"));
        assert_eq!(s.late_since, Some(secs("2026-11-16T12:01:00Z")));
        assert!(!s.late);
        // It started at 12:08 after waiting eight minutes: not late for that wait either.
        let running = run(
            "2026-11-16T12:00:00Z",
            "running",
            Some("2026-11-16T12:00:00Z"),
            Some("2026-11-16T12:08:00Z"),
        );
        let soon_after = at("2026-11-16T12:09:00Z");
        assert!(!standing(&cron, &Zone::Utc, soon_after, false, Some(&running)).late);
        // A run that started at once and no tick since: late.
        let stuck = run(
            "2026-11-16T12:00:00Z",
            "success",
            None,
            Some("2026-11-16T12:00:00Z"),
        );
        assert!(standing(&cron, &Zone::Utc, now, false, Some(&stuck)).late);
    }

    #[test]
    fn the_typical_queue_wait_is_the_median_and_extends_the_grace() {
        let cron = CronExpr::parse("*/5 * * * *").unwrap();
        let mut ticks = run(
            "2026-11-16T12:00:00Z",
            "success",
            None,
            Some("2026-11-16T12:00:00Z"),
        );
        // 12:05 was missed; at 12:07 that is 120s ago, past the 60s grace.
        let now = at("2026-11-16T12:07:00Z");
        assert!(standing(&cron, &Zone::Utc, now, false, Some(&ticks)).late);
        // Its ticks typically wait 90s for a run slot: 120s is within 60s + 90s.
        ticks.queue_waits = vec![30.0, 90.0, 200.0];
        let s = standing(&cron, &Zone::Utc, now, false, Some(&ticks));
        assert_eq!(s.typical_queue_wait_seconds, 90.0);
        assert!(!s.late);
        assert_eq!(median(&[10.0, 20.0]), 15.0);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn alternate_in_flight_skips_keep_the_scheduler_live() {
        let cron = CronExpr::parse("*/10 * * * * *").unwrap();
        let now = at("2026-11-16T12:01:17Z");
        let mut ticks = run("2026-11-16T12:01:00Z", "success", None, None);
        let start = secs("2026-11-16T12:00:00Z");
        ticks.decisions = (0..8)
            .map(|i| {
                (
                    start + i * 10,
                    if i % 2 == 0 {
                        db::TickDecision::Fired
                    } else {
                        db::TickDecision::Skipped
                    },
                )
            })
            .collect();
        let s = standing(&cron, &Zone::Utc, now, false, Some(&ticks));
        assert_eq!(s.late_since, None);
        assert_eq!(
            ScheduleState::of(&s, false, (0, 3)),
            ScheduleState::Overrunning
        );
    }

    /// A queued tick is in the run queue only; a started one has a `runs` row, and its queue
    /// row until it ends. `barca status` without a server reads both, as a server does.
    #[tokio::test]
    async fn recorded_ticks_are_the_run_queue_and_the_runs_a_schedule_started() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        db::init_db(&path).await.unwrap();
        let schedules = ["p.py:minutely".to_string(), "p.py:hourly".to_string()];
        assert!(recorded(&path, &schedules).await.unwrap().is_empty());

        let spec = |schedule: &str, tick: &str| {
            serde_json::json!({"trigger": {"triggered_by": "schedule", "schedule_id": schedule, "scheduled_at": tick}})
                .to_string()
        };
        // Two earlier ticks of `minutely` started 30s and 90s after their slots; its newest
        // tick waits in the queue. `other` is not asked for.
        for (id, tick, started) in [
            ("r1", "2026-11-16T12:00:00Z", "2026-11-16 12:00:30"),
            ("r2", "2026-11-16T12:01:00Z", "2026-11-16 12:02:30"),
        ] {
            db::create_run(&path, id, "get", "[]", None, Some(1))
                .await
                .unwrap();
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&path).await.unwrap();
            conn.execute(
                "UPDATE runs SET schedule_id = 'p.py:minutely', scheduled_at = ?2, started_at = ?3, \
                 status = 'success' WHERE run_id = ?1",
                [id, tick, started],
            )
            .await
            .unwrap();
        }
        run_queue::push(
            &path,
            "r3",
            &spec("p.py:minutely", "2026-11-16T12:03:00Z"),
            "2026-11-16T12:03:00Z",
        )
        .await
        .unwrap();
        run_queue::push(&path, "x", &spec("p.py:other", "2026-11-16T12:03:00Z"), "t")
            .await
            .unwrap();
        run_queue::push(
            &path,
            "manual",
            "{\"trigger\":{\"triggered_by\":\"api\"}}",
            "t",
        )
        .await
        .unwrap();

        let ticks = recorded(&path, &schedules).await.unwrap();
        assert_eq!(ticks.len(), 1, "{ticks:?}");
        let minutely = &ticks["p.py:minutely"];
        assert_eq!(minutely.queue_waits, [90.0, 30.0]);
        let newest = minutely.newest.clone().unwrap();
        assert_eq!(
            (newest.run_id.as_str(), newest.status.as_str()),
            ("r3", "pending")
        );
        assert_eq!(newest.scheduled_at, secs("2026-11-16T12:03:00Z"));
        assert_eq!(newest.queued_at, Some(secs("2026-11-16T12:03:00Z")));

        // It starts: the `runs` row says so, and the queue row still says when it was queued.
        run_queue::mark_running(&path, "r3").await.unwrap();
        db::create_run(&path, "r3", "get", "[]", None, Some(1))
            .await
            .unwrap();
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&path).await.unwrap();
            conn.execute(
                "UPDATE runs SET schedule_id = 'p.py:minutely', scheduled_at = '2026-11-16T12:03:00Z', \
                 started_at = '2026-11-16 12:05:00' WHERE run_id = 'r3'",
                (),
            )
            .await
            .unwrap();
        }
        let ticks = recorded(&path, &schedules).await.unwrap();
        let newest = ticks["p.py:minutely"].newest.clone().unwrap();
        assert_eq!(
            (newest.run_id.as_str(), newest.status.as_str()),
            ("r3", "running")
        );
        assert_eq!(newest.started_at, Some(secs("2026-11-16T12:05:00Z")));
        assert_eq!(newest.queued_at, Some(secs("2026-11-16T12:03:00Z")));
        assert_eq!(ticks["p.py:minutely"].queue_waits, [120.0, 90.0, 30.0]);
    }
}
