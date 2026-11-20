//! Whether a scheduler is firing a schedule, for a process that runs none itself
//! (`specs/behavior/health.md`, "Scheduler liveness"; #1039): a `--read-only` or
//! `--no-schedule` server, and `barca status` on a machine whose database a server writes.
//!
//! The evidence is what the scheduling server already writes ([`recorded`]): the tick record
//! (a row for every due slot it decided, whatever it decided), the slot each schedule was
//! anchored at or last fired for (`schedule_state`), and two entries of `server_state` it
//! writes when it starts, when its job set changes and when it stops ([`SchedulerRecord`],
//! [`ServingRecord`]). [`observe`] is the rule, a pure function of that and the clock.
//!
//! A recent heartbeat establishes that the scheduler loop is running. Each claimed schedule
//! must also have decided its due slots within its grace. Without a writer record, only a
//! schedule's own decisions establish liveness.

use barca_decide::schedule::{ScheduledJob, Zone, next_fire, prev_fire};
use barca_model::BarcaError;
use barca_store::run_queue::{set_state_value, state_value};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The `server_state` key of [`SchedulerRecord`].
const SCHEDULER: &str = "scheduler";
/// The `server_state` key of [`ServingRecord`].
const SERVING: &str = "serving";
const HEARTBEAT: &str = "scheduler_heartbeat";
const LOST_AFTER_SECONDS: i64 = 45;
pub const HEARTBEAT_SECONDS: i64 = 20;

pub async fn heartbeat(db_path: &str, now: i64) -> Result<(), BarcaError> {
    set_state_value(db_path, HEARTBEAT, &now.to_string()).await
}

/// What a scheduler records of itself: written when it starts, when its job set changes
/// (`--watch`) and when its server begins to stop. Times are unix seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerRecord {
    pub started_at: i64,
    /// When its server began to stop; `None` while it runs, and after a crash.
    #[serde(default)]
    pub stopped_at: Option<i64>,
    /// The scheduled nodes it fires.
    #[serde(default)]
    pub schedules: Vec<String>,
    #[serde(default)]
    pub timezone: Option<String>,
}

/// What a server that runs work records of itself: written when it starts and when it begins
/// to stop. Times are unix seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingRecord {
    pub started_at: i64,
    #[serde(default)]
    pub stopped_at: Option<i64>,
    /// How many runs it executes at once (`--max-runs`).
    pub max_runs: usize,
    /// How many steps of one run execute at once (its worker pool); 0 when not recorded.
    #[serde(default)]
    pub workers: usize,
}

/// Record that a server that runs work started at `now` with `max_runs` run slots and a
/// pool of `workers`.
pub async fn record_serving(
    db_path: &str,
    now: i64,
    max_runs: usize,
    workers: usize,
) -> Result<(), BarcaError> {
    let record = ServingRecord {
        started_at: now,
        stopped_at: None,
        max_runs,
        workers,
    };
    set_state_value(db_path, SERVING, &text(&record)).await
}

/// Record the scheduler that started at `started_at` and fires `schedules`.
pub async fn record_scheduler(
    db_path: &str,
    started_at: i64,
    schedules: Vec<String>,
    timezone: &str,
) -> Result<(), BarcaError> {
    if !schedules.is_empty() {
        heartbeat(db_path, started_at).await?;
    }
    let record = SchedulerRecord {
        started_at,
        stopped_at: None,
        schedules,
        timezone: Some(timezone.to_string()),
    };
    set_state_value(db_path, SCHEDULER, &text(&record)).await
}

/// Record that the server (and its scheduler, when it ran one) began to stop at `now`. A
/// record that already says so, and a database with no record, are left as they are.
pub async fn record_stopped(db_path: &str, now: i64) -> Result<(), BarcaError> {
    if let Some(mut record) = read::<ServingRecord>(db_path, SERVING).await?
        && record.stopped_at.is_none()
    {
        record.stopped_at = Some(now);
        set_state_value(db_path, SERVING, &text(&record)).await?;
    }
    record_scheduler_stopped(db_path, now).await
}

/// Record that the scheduler stopped at `now` (its server stops, or it gave up on an error).
pub async fn record_scheduler_stopped(db_path: &str, now: i64) -> Result<(), BarcaError> {
    if let Some(mut record) = read::<SchedulerRecord>(db_path, SCHEDULER).await?
        && record.stopped_at.is_none()
    {
        record.stopped_at = Some(now);
        set_state_value(db_path, SCHEDULER, &text(&record)).await?;
    }
    Ok(())
}

/// A record as its `server_state` value. These records hold numbers and node ids only.
fn text<T: Serialize>(record: &T) -> String {
    serde_json::to_string(record).unwrap_or_default()
}

async fn read<T: serde::de::DeserializeOwned>(
    db_path: &str,
    key: &str,
) -> Result<Option<T>, BarcaError> {
    Ok(state_value(db_path, key)
        .await?
        .and_then(|text| serde_json::from_str(&text).ok()))
}

/// Settings of the writer, if it has recorded them.
pub async fn serving(db_path: &str) -> Result<Option<ServingRecord>, BarcaError> {
    read(db_path, SERVING).await
}

/// What the metadata database says of the scheduler that writes it ([`recorded`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recorded {
    /// For each schedule, the newest slot a scheduler decided or anchored it at, unix
    /// seconds: the later of its newest tick of any action (`schedule_ticks`) and its
    /// `schedule_state` entry.
    pub decided: HashMap<String, i64>,
    pub heartbeat: Option<i64>,
    pub scheduler: Option<SchedulerRecord>,
    pub serving: Option<ServingRecord>,
}

impl Recorded {
    /// The writer stopped, or its scheduler loop disappeared without recording a stop.
    pub fn writer_stopped(&self, now: i64) -> bool {
        self.serving
            .as_ref()
            .is_some_and(|s| s.stopped_at.is_some())
            || self.scheduler.as_ref().is_some_and(|r| {
                !r.schedules.is_empty()
                    && self
                        .heartbeat
                        .is_some_and(|at| at >= r.started_at && now - at >= LOST_AFTER_SECONDS)
            })
    }

    /// The cron zone used by the writer, when recorded.
    pub fn zone(&self) -> Option<Zone> {
        Zone::parse(self.scheduler.as_ref()?.timezone.as_deref()?).ok()
    }
}

/// [`Recorded`] of the database at `db_path`: four bounded reads (one grouped read of the
/// tick record's primary key, `schedule_state`, two keys of `server_state`), none per
/// schedule. A database that does not exist yet records nothing.
pub async fn recorded(db_path: &str) -> Result<Recorded, BarcaError> {
    if std::fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Recorded::default());
    }
    let mut decided = barca_db::db::newest_decided(db_path).await?;
    // A database from before the table has no anchors: not an error on a read path.
    for (schedule, at) in barca_db::db::get_schedule_state(db_path)
        .await
        .unwrap_or_default()
    {
        let newest = decided.entry(schedule).or_insert(at);
        *newest = (*newest).max(at);
    }
    Ok(Recorded {
        decided,
        heartbeat: read(db_path, HEARTBEAT).await?,
        scheduler: read(db_path, SCHEDULER).await?,
        serving: serving(db_path).await?,
    })
}

/// One schedule as [`observe`] found it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// A scheduler is firing it.
    pub live: bool,
    /// The first slot after its newest decided one that has passed (unix seconds): a slot no
    /// scheduler has served yet. `None` while every slot was decided, and for a schedule no
    /// scheduler ever served.
    pub unserved_since: Option<i64>,
    /// For a schedule that is not live: the last sign of the scheduler that served it (when
    /// it stopped, else its newest recorded decision, unix seconds); `None` when none ever
    /// did.
    pub off_since: Option<i64>,
}

/// What [`observe`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// Every schedule of the graph, by scheduled node id.
    pub schedules: HashMap<String, Seen>,
    /// At least one claimed schedule is being served.
    pub running: bool,
    /// The last sign of a scheduler that is not running ([`Seen::off_since`]).
    pub off_since: Option<i64>,
    /// The scheduler stopped without recording it: its server was killed, or is unreachable.
    /// The runs it left `running` in the run queue are not in flight.
    pub lost: bool,
}

impl Observed {
    /// Whether the server that writes the database is known to have stopped or been lost:
    /// what it recorded as queued or running is then waiting for the next start.
    pub fn server_gone(&self, recorded: &Recorded) -> bool {
        self.lost
            || recorded
                .serving
                .as_ref()
                .is_some_and(|s| s.stopped_at.is_some())
    }
}

/// The grace of a schedule as of `now`: how long after a slot its decision may be recorded
/// before the schedule counts as unserved ([`crate::schedule_standing`]'s grace: 60 seconds,
/// or half the interval under two minutes).
fn grace(job: &ScheduledJob, zone: &Zone, now: DateTime<Utc>) -> i64 {
    crate::schedule_standing::grace_seconds(
        prev_fire(&job.cron, zone, now).map(|t| t.timestamp()),
        next_fire(&job.cron, zone, now).map(|t| t.timestamp()),
    )
}

/// Whether a scheduler is firing each of `jobs` as of `now`, from what the database records.
///
/// - A scheduler **claims** the schedules its record lists; with no record (a database last
///   written by a release that kept none), every schedule with a recorded decision.
/// - A claimed schedule is **unserved** when a slot after its newest decided one passed
///   longer ago than its grace.
/// - A stopped or lost scheduler serves none of its jobs. Otherwise, each schedule's own
///   decisions determine whether it is live.
pub fn observe(
    jobs: &[ScheduledJob],
    zone: &Zone,
    now: DateTime<Utc>,
    recorded: &Recorded,
) -> Observed {
    let at = now.timestamp();
    let record = recorded.scheduler.as_ref();
    let decided = |job: &ScheduledJob| match record {
        // From its start a scheduler owes every slot: catch-up decided what was missed
        // before it, or found nothing owed (a pause that ended while the server was down).
        Some(record) if record.schedules.contains(&job.id) => Some(
            recorded
                .decided
                .get(&job.id)
                .map_or(record.started_at, |d| (*d).max(record.started_at)),
        ),
        Some(_) => None,
        None => recorded.decided.get(&job.id).copied(),
    };
    let mut claimed: Vec<(&ScheduledJob, i64, Option<i64>)> = Vec::new();
    for job in jobs {
        let Some(decided) = decided(job) else {
            continue;
        };
        let unserved = DateTime::from_timestamp(decided, 0)
            .and_then(|d| next_fire(&job.cron, zone, d))
            .map(|t| t.timestamp())
            .filter(|first| *first <= at);
        claimed.push((job, decided, unserved));
    }
    let stopped = record.and_then(|r| r.stopped_at);
    let heartbeat = recorded
        .heartbeat
        .filter(|at| record.is_some_and(|r| *at >= r.started_at));
    let last_seen = claimed
        .iter()
        .map(|(_, decided, _)| *decided)
        .chain(heartbeat)
        .max();
    let late =
        |job: &ScheduledJob, slot: Option<i64>| slot.filter(|u| at - u > grace(job, zone, now));
    // A later decision proves another job is still being served. With no later
    // evidence, a missed decision is the last sign from the loop that owns these jobs.
    let lost = heartbeat.is_some_and(|beat| at - beat >= LOST_AFTER_SECONDS)
        || claimed.iter().any(|(job, _, slot)| {
            late(job, *slot).is_some_and(|missed| {
                heartbeat.is_none()
                    && record.is_some()
                    && last_seen.is_some_and(|last| last < missed)
            })
        });
    let running =
        stopped.is_none() && !lost && claimed.iter().any(|(j, _, s)| late(j, *s).is_none());
    let off_since = (!running)
        .then(|| stopped.or(last_seen))
        .flatten()
        .map(|since| since.min(at));
    let mut schedules: HashMap<String, Seen> = jobs
        .iter()
        .map(|job| (job.id.clone(), Seen::default()))
        .collect();
    for (job, decided, unserved) in claimed {
        let live = running && late(job, unserved).is_none();
        schedules.insert(
            job.id.clone(),
            Seen {
                live,
                unserved_since: unserved,
                off_since: (!live).then_some(off_since.unwrap_or(decided).min(at)),
            },
        );
    }
    Observed {
        schedules,
        running,
        off_since,
        lost: stopped.is_none() && lost,
    }
}

/// A run the serving server's queue records (`run_queue`): accepted and not finished.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedRun {
    pub run_id: String,
    /// `get` or `run`.
    pub command: String,
    pub refreshes: bool,
    /// The scheduled node whose tick started it; `None` for a run someone asked for.
    pub schedule: Option<String>,
    pub triggered_by: Option<barca_model::trigger::TriggeredBy>,
    /// The request asked to queue last (`queue` of the server's description): it starts with
    /// the scheduled runs, in the order they were accepted, not ahead of them.
    pub queue_last: bool,
    /// Its resolved target node ids; empty for a run of everything.
    pub targets: Vec<String>,
    /// It started executing (`run_queue` state `running` or `draining`).
    pub started: bool,
    /// When the server accepted it, unix seconds.
    pub queued_at: Option<i64>,
    /// When it started executing, unix seconds: what its `runs` row records. `None` while it
    /// waits, and for a started run with no row yet.
    pub started_at: Option<i64>,
    pub last_step: Option<i64>,
}

/// The runs the queue of the database at `db_path` records, in the order the server accepted
/// them: one read of `run_queue` and a query for the start and latest materialization of each started run
/// (at most the serving server's run slots).
pub async fn recorded_runs(db_path: &str) -> Result<Vec<RecordedRun>, BarcaError> {
    if std::fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let rows = barca_store::run_queue::list(db_path).await?;
    let progress = if rows
        .iter()
        .any(|r| r.state != barca_store::run_queue::QueueState::Queued)
    {
        barca_store::run_queue::progress(db_path).await?
    } else {
        HashMap::new()
    };
    for row in rows {
        let Ok(spec) = serde_json::from_str::<serde_json::Value>(&row.spec) else {
            continue;
        };
        let started = row.state != barca_store::run_queue::QueueState::Queued;
        let progress = progress.get(&row.run_id);
        out.push(RecordedRun {
            refreshes: spec["policy"]["kind"]
                .as_str()
                .is_some_and(|kind| kind != "cache_aware"),
            command: spec["command"].as_str().unwrap_or("get").to_string(),
            schedule: spec["trigger"]["schedule_id"].as_str().map(str::to_string),
            triggered_by: serde_json::from_value(spec["trigger"]["triggered_by"].clone()).ok(),
            queue_last: spec["queue"].as_str() == Some("last"),
            targets: spec["nodes"]
                .as_array()
                .map(|nodes| {
                    nodes
                        .iter()
                        .filter_map(|n| n.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            started,
            queued_at: barca_model::time::parse_rfc3339(&row.queued_at),
            started_at: progress.map(|p| p.0),
            last_step: progress.and_then(|p| p.1),
            run_id: row.run_id,
        });
    }
    Ok(out)
}

/// A run in flight, as an estimate reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlannedRun {
    pub run_id: String,
    /// The scheduled node whose tick started it; `None` for a run someone asked for.
    pub schedule: Option<String>,
    /// The nodes it computes when they are out of date: its targets and everything above
    /// them. `None` for a run of every node. Not read for a tick's run, whose nodes are its
    /// schedule's targets ([`crate::refresh_path::Paths`]).
    pub nodes: Option<HashSet<String>>,
    /// It is executing; false while it waits for a run slot.
    pub running: bool,
    /// When what it has left starts, unix seconds: for a running run its latest progress
    /// (its start, or the last step it finished); for a waiting run its estimated start
    /// ([`queue_starts`]). `None` when that is not known (a paused or draining queue).
    pub start: Option<i64>,
}

/// A run of the run queue, as [`queue_starts`] reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueRun {
    pub run_id: String,
    /// When it started executing, unix seconds; `None` while it waits.
    pub started_at: Option<i64>,
    /// How long such a run typically takes, seconds.
    pub typical_seconds: f64,
}

/// When each waiting run of the run queue is expected to start, unix seconds, and when a run
/// that joined now would. `runs`: the running runs, then the waiting ones in the order they
/// start. Each of `max_runs` slots is free when its run ends (its start plus its typical
/// time, or `now` when that has passed); a waiting run takes the slot that frees first. The
/// extra slot a run someone asked for may take past scheduled work is not modelled.
pub fn queue_starts(runs: &[QueueRun], max_runs: usize, now: i64) -> (HashMap<String, i64>, i64) {
    let mut slots: Vec<i64> = runs
        .iter()
        .filter_map(|run| {
            run.started_at
                .map(|at| (at + run.typical_seconds.ceil() as i64).max(now))
        })
        .collect();
    while slots.len() < max_runs.max(1) {
        slots.push(now);
    }
    let mut starts = HashMap::new();
    for run in runs.iter().filter(|run| run.started_at.is_none()) {
        let (slot, free) = slots
            .iter()
            .copied()
            .enumerate()
            .min_by_key(|(_, free)| *free)
            .expect("at least one slot");
        starts.insert(run.run_id.clone(), free);
        slots[slot] = free + run.typical_seconds.ceil() as i64;
    }
    let clear = slots.into_iter().min().unwrap_or(now);
    (starts, clear)
}

/// How long a run that `schedule`'s tick started typically takes, seconds: the median of its
/// newest ticks' successful runs ([`crate::schedule_standing::Ticks::run_seconds`]). For a
/// run no schedule started, and a schedule with none recorded: the median of the schedules
/// that have one; 0 when none has.
pub fn typical_run_seconds(
    ticks: &HashMap<String, crate::schedule_standing::Ticks>,
    schedule: Option<&str>,
) -> f64 {
    use crate::schedule_standing::median;
    let of = |ticks: &crate::schedule_standing::Ticks| {
        (!ticks.run_seconds.is_empty()).then(|| median(&ticks.run_seconds))
    };
    schedule
        .and_then(|id| ticks.get(id))
        .and_then(of)
        .unwrap_or_else(|| {
            let known: Vec<f64> = ticks.values().filter_map(of).collect();
            median(&known)
        })
}

/// The recorded queue as the estimates read it ([`recorded_timing`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordedTiming {
    /// The recorded runs in flight ([`crate::estimates::Timing::runs`]); none when their
    /// server is gone.
    pub runs: Vec<PlannedRun>,
    /// Each started run: when it started executing.
    pub started: HashMap<String, i64>,
    /// Each waiting run: when it is expected to start. Empty while the queue starts nothing
    /// and when the serving server's run slots are not recorded.
    pub starts: HashMap<String, i64>,
    pub queue_clear_at: Option<i64>,
    pub queue_open: bool,
    /// The serving server's run slots, when it recorded them.
    pub max_runs: Option<usize>,
    /// Its worker pool, when it recorded it.
    pub workers: Option<usize>,
}

/// What the estimates read of a queue this process does not hold, as of `now` (unix
/// seconds): the same model the serving server applies to its own queue
/// ([`crate::estimates::queue_starts`] over its recorded run slots, each run taking its
/// schedule's typical time), from `runs` ([`recorded_runs`]), the schedules' recorded `ticks`
/// and the queue's pause. `gone`: the serving server stopped or was lost
/// ([`Observed::server_gone`]), so nothing recorded is in flight and nothing will start.
pub fn recorded_timing(
    runs: &[RecordedRun],
    ticks: &HashMap<String, crate::schedule_standing::Ticks>,
    recorded: &Recorded,
    paused: bool,
    gone: bool,
    dag: &barca_model::dag::Dag,
    now: i64,
) -> RecordedTiming {
    let serving = recorded.serving.as_ref();
    let max_runs = serving.map(|s| s.max_runs).filter(|n| *n > 0);
    let workers = serving.map(|s| s.workers).filter(|n| *n > 0);
    let open = !paused && !gone;
    let mut out = RecordedTiming {
        queue_open: open,
        max_runs,
        workers,
        ..RecordedTiming::default()
    };
    if gone {
        return out;
    }
    // The order they start in: runs someone asked for first, then scheduled runs and the
    // runs asked to queue last.
    let behind = |r: &&RecordedRun| r.schedule.is_some() || r.queue_last;
    let waiting = runs
        .iter()
        .filter(|r| !r.started && !behind(r))
        .chain(runs.iter().filter(|r| !r.started && behind(r)));
    let queue: Vec<QueueRun> = runs
        .iter()
        .filter(|r| r.started)
        .chain(waiting)
        .map(|r| QueueRun {
            run_id: r.run_id.clone(),
            started_at: r
                .started
                .then(|| r.started_at.or(r.queued_at).unwrap_or(now)),
            typical_seconds: typical_run_seconds(ticks, r.schedule.as_deref()),
        })
        .collect();
    for run in &queue {
        if let Some(at) = run.started_at {
            out.started.insert(run.run_id.clone(), at);
        }
    }
    if let Some(max_runs) = max_runs {
        let running = out.started.len();
        let (starts, clear) = queue_starts(&queue, max_runs, now);
        if open {
            out.starts = starts;
        }
        out.queue_clear_at = (open && (running >= max_runs || queue.len() > running))
            .then_some(clear)
            .filter(|at| *at > now);
    }
    out.runs = runs
        .iter()
        .map(|r| PlannedRun {
            run_id: r.run_id.clone(),
            schedule: r.schedule.clone(),
            nodes: (r.schedule.is_none() && !r.targets.is_empty()).then(|| {
                let targets: Vec<&str> = r.targets.iter().map(String::as_str).collect();
                dag.subgraph_many(&targets)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            }),
            running: r.started,
            start: if r.started {
                out.started.get(&r.run_id).copied().max(r.last_step)
            } else {
                out.starts.get(&r.run_id).copied()
            },
        })
        .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs() -> Vec<ScheduledJob> {
        let source = r#"
from barca import sensor, Schedule

@sensor(freshness=Schedule("* * * * *"))
def minutely(): return 1

@sensor(freshness=Schedule("0 6 * * *"))
def daily(): return 1

@sensor(freshness=Schedule("*/5 * * * *"))
def unclaimed(): return 1
"#;
        let dag =
            barca_python::link::build(&barca_python::parse::extract_nodes(source, "p.py").unwrap())
                .expect("valid dag");
        barca_decide::schedule::jobs_from_dag(&dag)
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn secs(text: &str) -> i64 {
        at(text).timestamp()
    }

    /// A scheduler that started at 09:00 and claims `minutely` and `daily`, with the newest
    /// decided slot of each.
    fn recorded(minutely: &str, daily: &str) -> Recorded {
        Recorded {
            decided: [
                ("p.py:minutely".to_string(), secs(minutely)),
                ("p.py:daily".to_string(), secs(daily)),
            ]
            .into(),
            heartbeat: None,
            scheduler: Some(SchedulerRecord {
                started_at: secs("2026-11-17T09:00:00Z"),
                stopped_at: None,
                timezone: None,
                schedules: vec!["p.py:minutely".into(), "p.py:daily".into()],
            }),
            serving: None,
        }
    }

    fn observed(now: &str, recorded: &Recorded) -> Observed {
        observe(&jobs(), &Zone::Utc, at(now), recorded)
    }

    #[test]
    fn a_scheduler_that_decided_every_slot_is_live_for_what_it_claims_and_nothing_else() {
        let recorded = recorded("2026-11-17T14:57:00Z", "2026-11-17T06:00:00Z");
        // 20 seconds into the minute, and 29 seconds after the next slot (grace: 30s).
        for now in ["2026-11-17T14:57:20Z", "2026-11-17T14:58:29Z"] {
            let seen = observed(now, &recorded);
            assert!(seen.running, "{now}");
            assert_eq!(seen.off_since, None);
            assert!(seen.schedules["p.py:minutely"].live);
            // The daily schedule's last tick was nine hours ago: no slot of it has passed,
            // and the scheduler that serves it is running.
            assert!(seen.schedules["p.py:daily"].live);
            assert_eq!(seen.schedules["p.py:daily"].unserved_since, None);
            // No scheduler claims it: never live, with nothing to date it from.
            assert_eq!(seen.schedules["p.py:unclaimed"], Seen::default());
        }
    }

    #[test]
    fn one_unserved_slot_past_its_grace_means_the_scheduler_is_not_running() {
        let recorded = recorded("2026-11-17T14:57:00Z", "2026-11-17T06:00:00Z");
        // 14:58:00 passed 31 seconds ago with no decision recorded (grace: 30s).
        let seen = observed("2026-11-17T14:58:31Z", &recorded);
        assert!(!seen.running);
        assert!(seen.lost, "it did not record that it stopped");
        // Its last sign of life: the newest decision it recorded.
        assert_eq!(seen.off_since, Some(secs("2026-11-17T14:57:00Z")));
        let minutely = seen.schedules["p.py:minutely"];
        assert!(!minutely.live);
        assert_eq!(minutely.unserved_since, Some(secs("2026-11-17T14:58:00Z")));
        // The daily schedule is not live either, a day before its own slot passes: the one
        // scheduler that fires it is gone. No slot of it has passed yet.
        let daily = seen.schedules["p.py:daily"];
        assert!(!daily.live);
        assert_eq!(daily.unserved_since, None);
        assert_eq!(daily.off_since, seen.off_since);
    }

    #[test]
    fn a_stale_schedule_does_not_hide_another_schedules_later_decision() {
        let mut record = recorded("2026-11-17T14:57:00Z", "2026-11-16T06:00:00Z");
        // The scheduler started after today's daily slot, so move its start before that slot.
        record.scheduler.as_mut().unwrap().started_at = secs("2026-11-16T09:00:00Z");
        let seen = observed("2026-11-17T14:57:20Z", &record);
        assert!(seen.schedules["p.py:minutely"].live);
        assert!(!seen.schedules["p.py:daily"].live);
        assert!(!seen.lost);
    }

    #[test]
    fn a_lost_writer_is_seen_before_the_next_daily_slot() {
        let mut record = recorded("2026-11-17T14:57:00Z", "2026-11-17T06:00:00Z");
        record.scheduler.as_mut().unwrap().schedules = vec!["p.py:daily".into()];
        record.heartbeat = Some(secs("2026-11-17T14:57:00Z"));
        assert!(observed("2026-11-17T14:57:44Z", &record).running);
        let seen = observed("2026-11-17T14:57:45Z", &record);
        assert!(seen.lost && !seen.running);
    }

    #[test]
    fn a_recorded_stop_is_known_at_once_and_a_restart_clears_it() {
        let mut recorded = recorded("2026-11-17T14:57:00Z", "2026-11-17T06:00:00Z");
        recorded.scheduler.as_mut().unwrap().stopped_at = Some(secs("2026-11-17T14:57:10Z"));
        // Five seconds after it stopped, before any slot passed.
        let seen = observed("2026-11-17T14:57:15Z", &recorded);
        assert!(!seen.running);
        assert!(!seen.lost);
        assert_eq!(seen.off_since, Some(secs("2026-11-17T14:57:10Z")));
        assert!(seen.schedules.values().all(|s| !s.live));
        assert_eq!(seen.schedules["p.py:minutely"].unserved_since, None);

        // It starts again at 15:30: catch-up records a decision for the schedule that missed
        // slots, and the record no longer says stopped.
        recorded.scheduler.as_mut().unwrap().stopped_at = None;
        recorded.scheduler.as_mut().unwrap().started_at = secs("2026-11-17T15:30:05Z");
        recorded
            .decided
            .insert("p.py:minutely".into(), secs("2026-11-17T15:30:05Z"));
        let seen = observed("2026-11-17T15:30:20Z", &recorded);
        assert!(seen.running);
        assert!(seen.schedules["p.py:daily"].live);
    }

    #[test]
    fn without_a_record_the_schedules_with_decisions_are_the_claimed_ones() {
        let mut recorded = recorded("2026-11-17T14:57:00Z", "2026-11-17T06:00:00Z");
        recorded.scheduler = None;
        let seen = observed("2026-11-17T14:57:20Z", &recorded);
        assert!(seen.running);
        assert!(seen.schedules["p.py:minutely"].live);
        assert!(!seen.schedules["p.py:unclaimed"].live);
        let seen = observed("2026-11-17T15:10:00Z", &recorded);
        assert!(seen.running && !seen.schedules["p.py:minutely"].live);
        // Nothing recorded at all: no scheduler ever ran, so nothing dates it.
        let seen = observed("2026-11-17T15:10:00Z", &Recorded::default());
        assert!(!seen.running && !seen.lost);
        assert_eq!(seen.off_since, None);
        assert!(seen.schedules.values().all(|s| *s == Seen::default()));
    }

    #[test]
    fn a_schedule_the_scheduler_claims_and_has_not_decided_yet_counts_from_its_start() {
        // A schedule added by a deploy: claimed at the start, no tick or anchor read yet.
        let mut recorded = recorded("2026-11-17T14:57:00Z", "2026-11-17T06:00:00Z");
        recorded.decided.remove("p.py:daily");
        let seen = observed("2026-11-17T14:57:20Z", &recorded);
        assert!(seen.running);
        assert!(seen.schedules["p.py:daily"].live);
    }

    #[tokio::test]
    async fn the_records_round_trip_and_a_stop_is_recorded_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        assert_eq!(super::recorded(&path).await.unwrap(), Recorded::default());
        barca_db::db::init_db(&path).await.unwrap();
        // Nothing recorded yet: a stop writes nothing.
        record_stopped(&path, 5).await.unwrap();
        assert_eq!(super::recorded(&path).await.unwrap(), Recorded::default());

        record_serving(&path, 10, 3, 4).await.unwrap();
        record_scheduler(&path, 11, vec!["p.py:a".into()], "utc")
            .await
            .unwrap();
        barca_db::db::upsert_schedule_state(&path, "p.py:a", 60)
            .await
            .unwrap();
        barca_db::db::record_tick(
            &path,
            barca_db::db::NewTick {
                schedule_id: "p.py:a",
                scheduled_at: 120,
                action: barca_db::db::TickDecision::Skipped,
                skipped_for: Some("r1"),
                run_id: None,
            },
        )
        .await
        .unwrap();
        record_stopped(&path, 130).await.unwrap();
        record_stopped(&path, 999).await.unwrap();
        let recorded = super::recorded(&path).await.unwrap();
        // A skipped tick is a decision: the newest of the tick record and the anchor.
        assert_eq!(recorded.decided["p.py:a"], 120);
        let serving = recorded.serving.unwrap();
        assert_eq!((serving.max_runs, serving.stopped_at), (3, Some(130)));
        let scheduler = recorded.scheduler.unwrap();
        assert_eq!(scheduler.stopped_at, Some(130));
        assert_eq!(scheduler.schedules, vec!["p.py:a".to_string()]);
    }
}
