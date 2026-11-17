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

use crate::schedule::{Zone, next_fire, prev_fire};
use crate::serve::run_queue::{self, QueueState};
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
    /// The first slot after `last_fired` that has passed: a slot no tick fired for. `None`
    /// while on time, while paused, and when no tick is recorded (a schedule that never fired
    /// is not late: the scheduler anchors a new job at its first start).
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

/// The one definition of a schedule's last tick, last run, next slot and lateness.
///
/// A schedule is late when it is not paused, a slot after its newest fired tick has passed
/// (`late_since`), and that was longer ago than its grace plus its queue wait. The queue wait
/// is the typical one (the median of `ticks.queue_waits`) or, when longer, the newest tick's
/// own: a tick whose run waits for a run slot is not late for waiting; the run queue is what
/// is behind.
pub fn standing(
    cron: &Cron,
    zone: &Zone,
    now: DateTime<Utc>,
    paused: bool,
    ticks: Option<&Ticks>,
) -> Standing {
    let at = now.timestamp();
    let prev_fire = prev_fire(cron, zone, now).map(|t| t.timestamp());
    let next = next_fire(cron, zone, now).map(|t| t.timestamp());
    let newest = ticks.and_then(|t| t.newest.as_ref());
    let late_since = newest
        .filter(|_| !paused)
        .and_then(|run| DateTime::from_timestamp(run.scheduled_at, 0))
        .and_then(|tick| next_fire(cron, zone, tick))
        .map(|t| t.timestamp())
        .filter(|first| *first <= at);
    let grace_seconds = grace_seconds(prev_fire, next);
    let typical_queue_wait_seconds = median(ticks.map_or(&[], |t| &t.queue_waits));
    let own_wait = newest.map_or(0, |run| run.queue_wait(at)) as f64;
    let allowance = grace_seconds as f64 + typical_queue_wait_seconds.max(own_wait);
    let run_seconds = ticks.map_or(&[][..], |t| &t.run_seconds);
    let interval_seconds = match (prev_fire, next) {
        (Some(prev), Some(next)) if next > prev => Some(next - prev),
        _ => None,
    };
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
    }
}

/// The schedules of `jobs` that are late as of `now` ([`standing`]): what row 6 of the health
/// table reads (`crate::health`).
pub fn late_schedules(
    jobs: &[crate::schedule::ScheduledJob],
    ticks: &HashMap<String, Ticks>,
    zone: &Zone,
    now: DateTime<Utc>,
    paused: &dyn Fn(&str) -> bool,
) -> std::collections::HashSet<String> {
    jobs.iter()
        .filter(|job| standing(&job.cron, zone, now, paused(&job.id), ticks.get(&job.id)).late)
        .map(|job| job.id.clone())
        .collect()
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
