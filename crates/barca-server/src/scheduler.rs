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

use crate::handlers;
use crate::state::{AppState, JobStatus, RunStatus};
#[cfg(test)]
use barca_core::results::AssetSummary;
use barca_core::schedule::{ScheduledJob, collect_jobs};
#[cfg(test)]
use barca_core::schedule::{describe_schedule, jobs_from_summaries};
use barca_core::{NodeKind, db};
use chrono::{DateTime, FixedOffset, Local, TimeZone, Timelike, Utc};
use croner::Cron;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

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
                    barca_core::errln!("[barca] unknown timezone {s:?}, using local time");
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

/// Decide, for a tick at `now`, which due jobs to fire and which to skip. Pure:
/// takes the last handle issued per job and a predicate reporting whether a
/// handle is still running, so the overlap-skip logic is testable without a
/// live server or wall clock.
fn plan_tick<'a, Tz: TimeZone>(
    now: &DateTime<Tz>,
    jobs: &'a [ScheduledJob],
    last_handle: &HashMap<String, String>,
    in_flight: impl Fn(&str) -> bool,
) -> Vec<TickAction<'a>> {
    due_jobs(now, jobs)
        .into_iter()
        .map(|job| match last_handle.get(&job.id) {
            Some(h) if in_flight(h) => TickAction::Skip {
                job,
                handle: h.clone(),
            },
            _ => TickAction::Fire(job),
        })
        .collect()
}

/// Whether a previously-issued run handle is still pending or running.
fn is_in_flight(state: &AppState, handle: &str) -> bool {
    state
        .runs
        .get(handle)
        .is_some_and(|r| matches!(r.status, RunStatus::Pending | RunStatus::Running))
}

/// Trigger a run for a due job, routed by node kind: assets and sensors go
/// through the `get` path, tasks through the `run` path. Returns the handle.
///
/// A tick brings the node up to date, it does not force it: sensors upstream
/// are polled, anything whose inputs changed is recomputed, and an asset whose
/// inputs did not change is served from cache. A task itself always runs.
fn trigger(state: &AppState, job: &ScheduledJob) -> String {
    match job.kind {
        NodeKind::Task => handlers::start_scheduled_task(state.clone(), job.id.clone()),
        NodeKind::Asset | NodeKind::Sensor => {
            handlers::start_run(state.clone(), Some(job.id.clone()))
        }
    }
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

/// Catch-up at startup, as of `now`: fire once each job whose scheduled tick elapsed while
/// the server was down, however many ticks that was, and move its last-fired record to `now`.
/// A job with no record is anchored to `now` and does not run (no stampede on first launch).
/// Returns (handle of the run triggered per job, last-fired epoch per job).
///
/// `now` is a parameter so a restart across a tick can be tested with fixed times.
async fn catch_up(
    state: &AppState,
    jobs: &[ScheduledJob],
    zone: &Zone,
    now: &DateTime<FixedOffset>,
    db_path: &str,
) -> (HashMap<String, String>, HashMap<String, i64>) {
    let saved = db::get_schedule_state(db_path).await.unwrap_or_default();
    let mut last_handle: HashMap<String, String> = HashMap::new();
    let mut last_fired = saved.clone();
    for job in jobs {
        match saved.get(&job.id) {
            Some(&last_epoch) => {
                let last = zone.timestamp(last_epoch);
                if needs_catchup(&job.cron, &last, now) {
                    let handle = trigger(state, job);
                    barca_core::errln!("[barca] catch-up run {} → {handle}", job.id);
                    last_handle.insert(job.id.clone(), handle);
                    last_fired.insert(job.id.clone(), now.timestamp());
                    persist_fired(db_path, &job.id, now.timestamp()).await;
                }
            }
            None => {
                last_fired.insert(job.id.clone(), now.timestamp());
                persist_fired(db_path, &job.id, now.timestamp()).await;
            }
        }
    }
    (last_handle, last_fired)
}

/// Record that `node_id` fired at `epoch` seconds. Best-effort durability.
async fn persist_fired(db_path: &str, node_id: &str, epoch: i64) {
    if let Err(e) = db::upsert_schedule_state(db_path, node_id, epoch).await {
        barca_core::errln!("[barca] schedule_state write failed for {node_id}: {e}");
    }
}

/// Publish the current job set + last-fired/last-handle bookkeeping into shared
/// state for `GET /schedule` to read.
fn publish_registry(
    state: &AppState,
    jobs: &[ScheduledJob],
    last_handle: &HashMap<String, String>,
    last_fired: &HashMap<String, i64>,
) {
    let snapshot: Vec<JobStatus> = jobs
        .iter()
        .map(|j| JobStatus {
            id: j.id.clone(),
            cron: j.cron_str.clone(),
            kind: j.kind,
            last_fired: last_fired.get(&j.id).copied(),
            last_handle: last_handle.get(&j.id).cloned(),
        })
        .collect();
    if let Ok(mut w) = state.schedule.write() {
        *w = snapshot;
    }
}

/// Re-run static analysis to enumerate scheduled jobs. The parse itself runs
/// on the blocking pool inside `barca_core::queries::list_assets`.
async fn reload_jobs(state: &AppState) -> Vec<ScheduledJob> {
    collect_jobs(&state.config.files, &state.config.python).await
}

/// Log the current schedule and each job's next fire time.
fn log_schedule(jobs: &[ScheduledJob], zone: &Zone) {
    if jobs.is_empty() {
        barca_core::errln!("[barca] no scheduled assets yet (watching for changes)");
        return;
    }
    barca_core::errln!(
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
        barca_core::errln!("  {} — {} (next {})", job.id, job.cron_str, next);
    }
}

/// The scheduler background task. Spawned from `serve_async` when scheduling is
/// enabled; runs for the lifetime of the server.
pub async fn run_scheduler(state: AppState) {
    let zone = Zone::parse(&state.config.timezone);

    let mut jobs = reload_jobs(&state).await;

    if jobs.is_empty() && !state.config.watch {
        barca_core::errln!("[barca] no scheduled assets — scheduler idle");
        return;
    }
    log_schedule(&jobs, &zone);

    // Resolve the metadata DB path (same `.barca` a CLI run uses) and ensure the
    // schedule_state table exists. `None` → durability disabled, live-match only.
    let db_path = match db::ensure_env_dirs(&state.config.resolved.env) {
        Ok(_) => {
            let path = state.config.resolved.db_path.clone();
            let _ = db::init_db(&path).await;
            Some(path)
        }
        Err(_) => {
            barca_core::errln!("[barca] scheduler: durability disabled (no metadata db)");
            None
        }
    };

    // Handle issued and last-fired epoch per job. `last_handle` powers the
    // overlap skip ("passes do not overlap"); `last_fired` powers durability and
    // the `/schedule` view. Entries for jobs removed on reload are harmless.
    // Catch-up requires durability; it is skipped entirely if the DB is unavailable.
    let (mut last_handle, mut last_fired) = match &db_path {
        Some(dbp) => catch_up(&state, &jobs, &zone, &zone.now(), dbp).await,
        None => (HashMap::new(), HashMap::new()),
    };
    publish_registry(&state, &jobs, &last_handle, &last_fired);

    let mut seen_gen = state.dag_generation.load(Ordering::Relaxed);

    loop {
        sleep_to_next_second(&zone).await;

        // `--watch`: re-read the job set when a source file changed.
        let current_gen = state.dag_generation.load(Ordering::Relaxed);
        if current_gen != seen_gen {
            seen_gen = current_gen;
            let fresh = reload_jobs(&state).await;
            barca_core::errln!("[barca] schedule reloaded: {} job(s)", fresh.len());
            jobs = fresh;
            log_schedule(&jobs, &zone);
            publish_registry(&state, &jobs, &last_handle, &last_fired);
        }

        let now = zone.now();
        let mut fired_any = false;
        // NOTE: benchmarks/scheduler_overhead/barca/run.sh's CI smoke greps stderr for
        // "scheduled run.*:probe" to count ticks independent of worker execution — keep
        // that substring ("scheduled run", plus the job id) if this wording changes.
        for action in plan_tick(&now, &jobs, &last_handle, |h| is_in_flight(&state, h)) {
            match action {
                TickAction::Skip { job, handle } => barca_core::errln!(
                    "[barca] scheduled run {} skipped — previous run {handle} still in flight",
                    job.id
                ),
                TickAction::Fire(job) => {
                    let handle = trigger(&state, job);
                    barca_core::errln!("[barca] scheduled run {} → {handle}", job.id);
                    last_handle.insert(job.id.clone(), handle);
                    last_fired.insert(job.id.clone(), now.timestamp());
                    if let Some(dbp) = &db_path {
                        persist_fired(dbp, &job.id, now.timestamp()).await;
                    }
                    fired_any = true;
                }
            }
        }
        if fired_any {
            publish_registry(&state, &jobs, &last_handle, &last_fired);
        }
    }
}

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
    fn plan_tick_fires_when_no_prior_run() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(5, 0), &jobs, &HashMap::new(), |_| false);
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    }

    #[test]
    fn plan_tick_skips_when_prior_run_in_flight() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let last = HashMap::from([("f.py:daily".to_string(), "h1".to_string())]);
        let plan = plan_tick(&at(5, 0), &jobs, &last, |h| h == "h1");
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
        let last = HashMap::from([("f.py:daily".to_string(), "h1".to_string())]);
        // Same handle recorded, but it is no longer in flight (completed/evicted).
        let plan = plan_tick(&at(5, 0), &jobs, &last, |_| false);
        assert_eq!(fired_ids(&plan), vec!["f.py:daily"]);
    }

    #[test]
    fn plan_tick_ignores_jobs_not_due_this_minute() {
        let jobs = vec![job("f.py:daily", NodeKind::Asset, "0 5 * * *")];
        let plan = plan_tick(&at(6, 0), &jobs, &HashMap::new(), |_| false);
        assert!(plan.is_empty(), "06:00 is not the job's minute");
    }

    #[test]
    fn plan_tick_mixes_fire_and_skip_across_jobs() {
        let jobs = vec![
            job("f.py:a", NodeKind::Asset, "0 5 * * *"),
            job("f.py:b", NodeKind::Task, "0 5 * * *"),
        ];
        // `a` has a run still going; `b` has never run.
        let last = HashMap::from([("f.py:a".to_string(), "ha".to_string())]);
        let plan = plan_tick(&at(5, 0), &jobs, &last, |h| h == "ha");
        assert_eq!(skipped_ids(&plan), vec!["f.py:a"]);
        assert_eq!(fired_ids(&plan), vec!["f.py:b"]);
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

    #[tokio::test]
    async fn a_first_start_anchors_every_job_to_now_and_runs_nothing() {
        let (_dir, db_path) = schedule_db().await;
        let st = app_state();
        let jobs = [job("f.py:daily", NodeKind::Asset, "0 6 * * *")];
        let now = utc(2, 9, 0);

        let (handles, fired) = catch_up(&st, &jobs, &Zone::Utc, &now, &db_path).await;

        assert!(handles.is_empty());
        assert!(st.runs.is_empty(), "nothing may run on a first start");
        assert_eq!(
            fired,
            HashMap::from([("f.py:daily".to_string(), now.timestamp())])
        );
        assert_eq!(db::get_schedule_state(&db_path).await.unwrap(), fired);
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
        catch_up(&app_state(), &jobs, &Zone::Utc, &stopped, &db_path).await;

        // Back at 05:50: no tick of either job has passed.
        let st = app_state();
        let early = utc(2, 5, 50);
        let (handles, fired) = catch_up(&st, &jobs, &Zone::Utc, &early, &db_path).await;
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
        let (handles, fired) = catch_up(&st, &jobs, &Zone::Utc, &back, &db_path).await;
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
        let (handles, fired) = catch_up(&st, &jobs, &Zone::Utc, &again, &db_path).await;
        assert!(handles.is_empty() && st.runs.is_empty());
        assert_eq!(fired["f.py:hourly"], back.timestamp());
    }

    #[tokio::test]
    async fn a_job_added_while_the_server_was_down_is_anchored_not_run() {
        let (_dir, db_path) = schedule_db().await;
        let old = job("f.py:daily", NodeKind::Asset, "0 6 * * *");
        catch_up(
            &app_state(),
            std::slice::from_ref(&old),
            &Zone::Utc,
            &utc(2, 5, 0),
            &db_path,
        )
        .await;

        let st = app_state();
        let jobs = [old, job("f.py:new", NodeKind::Asset, "0 6 * * *")];
        let back = utc(4, 9, 0);
        let (handles, fired) = catch_up(&st, &jobs, &Zone::Utc, &back, &db_path).await;

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
        let ha = trigger(&st, &asset);
        let ht = trigger(&st, &task);
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
