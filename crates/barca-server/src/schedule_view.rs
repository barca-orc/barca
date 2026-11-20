//! Where every schedule stands, as this server holds it: the recorded ticks kept per database
//! state, its pauses, and its scheduler's own word for the jobs it fires. `GET /schedule`,
//! the `schedule` block of `GET /state` and `GET /nodes`, and the queue's estimates all read
//! it here.

use crate::error::ApiError;
use crate::state::AppState;
use crate::views::JobStatus;
use barca_service::db::{self, SchedulePause};
use barca_service::served::ScheduleJob;
use std::collections::HashMap;
use std::sync::RwLock;

/// What every schedule is read from ([`barca_service::refresh_path::ScheduleInputs`]), as this
/// server holds it as of `now`: the recorded ticks kept per database state, its pauses, and
/// its scheduler's own word for the jobs it fires. Without a scheduler here (a read-only
/// server, `--no-schedule`) the records say which schedules a scheduler fires, in the zone it
/// recorded.
pub(crate) struct Schedules {
    jobs: Vec<barca_service::schedule::ScheduledJob>,
    zone: barca_service::schedule::Zone,
    now: chrono::DateTime<chrono::Utc>,
    ticks: std::sync::Arc<crate::read_cache::ScheduleFacts>,
    pauses: std::collections::HashMap<String, db::SchedulePause>,
    firing: std::collections::HashMap<String, barca_service::refresh_path::Firing>,
    observed: barca_service::liveness::Observed,
}

impl Schedules {
    pub(crate) fn inputs(&self) -> barca_service::refresh_path::ScheduleInputs<'_> {
        barca_service::refresh_path::ScheduleInputs {
            jobs: &self.jobs,
            zone: &self.zone,
            now: self.now,
            ticks: &self.ticks.ticks,
            failures: &self.ticks.failures,
            pauses: &self.pauses,
            firing: &self.firing,
            observed: &self.observed,
        }
    }
}

/// The paused schedules this server holds ([`crate::schedule_view::pauses`] reads them first).
fn held_pauses(state: &AppState) -> std::collections::HashMap<String, db::SchedulePause> {
    state
        .scheduling
        .pauses
        .get()
        .and_then(|pauses| pauses.read().ok().map(|held| held.clone()))
        .unwrap_or_default()
}

/// The scheduler's published jobs as the schedule walk reads them.
fn firing_of(
    published: &[JobStatus],
) -> std::collections::HashMap<String, barca_service::refresh_path::Firing> {
    published
        .iter()
        .map(|job| {
            (
                job.id.clone(),
                barca_service::refresh_path::Firing {
                    failures: job.failures,
                    max_failures: job.max_failures,
                    skip_reason: job.skip_reason.map(str::to_string),
                },
            )
        })
        .collect()
}

/// [`Schedules`] of every scheduled node of `dag` as of `now`, over the ticks the database in
/// state `db_state` records. `GET /state`, `GET /state/rollup` and `GET /nodes`
/// (`barca status`) all read this, and `GET /schedule` the same walk ([`schedule_at`]), so
/// they say the same of a schedule.
pub(crate) async fn schedules(
    state: &AppState,
    dag: &barca_service::dag::Dag,
    db_state: &crate::read_cache::Stamp,
    db_path: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Schedules, ApiError> {
    let observed_at = chrono::Utc::now();
    let jobs = barca_service::schedule::jobs_from_dag(dag);
    let mut zone = crate::config::zone_of(&state.config);
    if jobs.is_empty() {
        return Ok(Schedules {
            jobs,
            zone,
            now,
            ticks: Default::default(),
            pauses: Default::default(),
            firing: Default::default(),
            observed: Default::default(),
        });
    }
    crate::schedule_view::pauses(state).await?;
    let ids = jobs.iter().map(|j| j.id.clone()).collect();
    let ticks = state.reads.cached_ticks(db_state, db_path, ids).await?;
    let published = state
        .scheduling
        .jobs
        .read()
        .map(|g| g.clone())
        .unwrap_or_default();
    let observed = if state.config.read_only || !state.config.schedule {
        let record = barca_service::liveness::recorded(db_path).await?;
        zone = record.zone().unwrap_or(zone);
        barca_service::liveness::observe(&jobs, &zone, observed_at, &record)
    } else {
        Default::default()
    };
    Ok(Schedules {
        jobs,
        zone,
        now,
        ticks,
        pauses: held_pauses(state),
        firing: firing_of(&published),
        observed,
    })
}

/// The recorded ticks of the schedules the scheduler has published, read once per database
/// state and shared with `GET /state` ([`crate::read_cache::Reads::cached_ticks`]).
pub(crate) async fn published_ticks(
    state: &AppState,
) -> Result<std::sync::Arc<crate::read_cache::ScheduleFacts>, ApiError> {
    let ids: Vec<String> = state
        .scheduling
        .jobs
        .read()
        .map(|jobs| jobs.iter().map(|j| j.id.clone()).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let db_state = state.reads.db_state();
    let snapshot = state.reads.snapshot_db().await?;
    Ok(state
        .reads
        .cached_ticks(&db_state, &snapshot.path, ids)
        .await?)
}

pub(crate) async fn recorded_queue(
    state: &AppState,
    dag: &barca_service::dag::Dag,
    path: &str,
    now: i64,
) -> Result<barca_service::liveness::RecordedTiming, ApiError> {
    let jobs = barca_service::schedule::jobs_from_dag(dag);
    let rec = barca_service::liveness::recorded(path).await?;
    let zone = rec
        .zone()
        .unwrap_or_else(|| crate::config::zone_of(&state.config));
    let observed = barca_service::liveness::observe(
        &jobs,
        &zone,
        chrono::DateTime::from_timestamp(now, 0).unwrap(),
        &rec,
    );
    let facts = state
        .reads
        .cached_ticks(
            &state.reads.db_state(),
            path,
            jobs.iter().map(|j| j.id.clone()).collect(),
        )
        .await?;
    Ok(
        barca_service::health::recorded_queue(path, dag, &facts.ticks, &rec, &observed, now)
            .await?,
    )
}

/// Each scheduled node's next fire time in this machine's local time, as `barca list` shows.
pub(crate) fn next_fires(
    dag: &barca_service::dag::Dag,
) -> std::collections::HashMap<String, String> {
    barca_service::schedule::describe_schedule_from_dag(
        Some(dag),
        &barca_service::schedule::Zone::Local,
    )
    .into_iter()
    .filter_map(|j| j.next_fire_local.map(|t| (j.id, t)))
    .collect()
}

/// The body of `GET /schedule` as of `now`, over the recorded `ticks`
/// ([`published_ticks`]). Where each schedule stands (`next_fire`, `last_fired`,
/// `last_run_id`, `last_status`, `late`, `late_since`) is
/// [`barca_service::schedule_standing::standing`], in the zone the server evaluates cron in
/// (`--timezone`), so `next_fire` is the time the scheduler will fire the job.
pub(crate) fn schedule_at(
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
    ticks: &crate::read_cache::ScheduleFacts,
) -> Vec<ScheduleJob> {
    let published = state
        .scheduling
        .jobs
        .read()
        .map(|g| g.clone())
        .unwrap_or_default();
    // The jobs as the scheduler published them: it parsed each cron before publishing.
    let jobs = published
        .iter()
        .filter_map(|job| {
            Some(barca_service::schedule::ScheduledJob {
                id: job.id.clone(),
                kind: job.kind,
                cron_str: job.cron.clone(),
                cron: barca_service::CronExpr::parse(&job.cron).ok()?,
                followers: Vec::new(),
                max_failures: job.max_failures,
                timeout_seconds: None,
                batch: None,
            })
        })
        .collect();
    schedule_items(
        state,
        now,
        ticks,
        &published,
        jobs,
        crate::config::zone_of(&state.config),
    )
}

/// One `GET /schedule` entry per job of `jobs`, in their order
/// ([`barca_service::served::schedule_jobs`]), from the walk every surface reads: the `schedule`
/// block of `GET /state` and a node's `refresh_path` carry the same state. `published`: the
/// scheduler's own jobs (failed ticks, the last skip); none where no scheduler runs here.
pub(crate) fn schedule_items(
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
    ticks: &crate::read_cache::ScheduleFacts,
    published: &[JobStatus],
    jobs: Vec<barca_service::schedule::ScheduledJob>,
    zone: barca_service::schedule::Zone,
) -> Vec<ScheduleJob> {
    let pauses = held_pauses(state);
    let skips = published
        .iter()
        .map(|job| {
            (
                job.id.clone(),
                barca_service::served::JobSkips {
                    last_skipped: job.last_skipped,
                    skipped_for: job.skipped_for.clone(),
                    skip_reason: job
                        .skip_reason
                        .and_then(barca_service::served::SkipReason::parse),
                },
            )
        })
        .collect();
    barca_service::served::schedule_jobs(
        barca_service::refresh_path::ScheduleInputs {
            jobs: &jobs,
            zone: &zone,
            now,
            ticks: &ticks.ticks,
            failures: &ticks.failures,
            pauses: &pauses,
            firing: &firing_of(published),
            // `live` is not served here.
            observed: &Default::default(),
        },
        &skips,
    )
}

/// The paused schedules, read from `schedule_state` on first use.
pub(crate) async fn pauses(
    state: &AppState,
) -> Result<&RwLock<HashMap<String, SchedulePause>>, ApiError> {
    if state.config.read_only
        && let Some(pauses) = state.scheduling.pauses.get()
    {
        let snapshot = state.reads.snapshot_db().await?;
        let saved = db::get_schedule_pauses(&snapshot.path).await?;
        *pauses.write().unwrap() = saved;
        return Ok(pauses);
    }
    state
        .scheduling
        .pauses
        .get_or_try_init(|| async {
            let snapshot = state.reads.snapshot_db().await?;
            let saved = db::get_schedule_pauses(&snapshot.path).await?;
            Ok::<_, ApiError>(RwLock::new(saved))
        })
        .await
}

/// `node_id`'s pause, if it is paused at `now` (unix seconds). A pause whose `until` has
/// passed is over.
pub(crate) fn active(state: &AppState, node_id: &str, now: i64) -> Option<SchedulePause> {
    let pauses = state.scheduling.pauses.get()?.read().ok()?;
    pauses
        .get(node_id)
        .copied()
        .filter(|p| p.until.is_none_or(|until| until > now))
}
