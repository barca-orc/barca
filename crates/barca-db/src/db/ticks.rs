//! Reads and writes of `schedule_ticks`: what each due tick of a schedule decided, and how the
//! run a fired tick started ended (#960, `specs/behavior/freshness.md`, "The tick record").
//!
//! The scheduler of `barca serve` writes a row when it decides (fired, paused, skipped,
//! failing), and the run of a fired tick adds when it started and how it ended. Nothing here
//! classifies: the readers ([`ticks_for_schedule`], [`tick_outcomes_for_node`]) return what was
//! recorded. The table is evidence, not state; losing it loses nothing a run needs.
//!
//! Each write is one transaction with its trim. It is bounded by its writers: per schedule the newest [`TICKS_PER_SCHEDULE`] ticks and none
//! older than [`TICK_MAX_AGE_SECONDS`] ([`record_tick`]), and per-target outcomes for the
//! newest [`TICKS_WITH_TARGETS`] fired ticks of a schedule, counts only for older ones
//! ([`record_tick_finished`]).

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The table, created by `init_db` and by the additive migration (`schema::ddl::ADDED_TABLES`).
/// Times are unix seconds: `scheduled_at` whole (the slot), `started_at` and `finished_at`
/// fractional. `targets` is a JSON array of `[node id, outcome]`, NULL for a tick that did not
/// fire, one whose run has not ended, and one past the newest [`TICKS_WITH_TARGETS`].
pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS schedule_ticks (
    schedule_id TEXT NOT NULL,
    scheduled_at INTEGER NOT NULL,
    action TEXT NOT NULL,
    skipped_for TEXT,
    run_id TEXT,
    started_at REAL,
    finished_at REAL,
    run_status TEXT,
    queue_wait_seconds REAL,
    rebuilt INTEGER,
    up_to_date INTEGER,
    failed INTEGER,
    not_reached INTEGER,
    targets TEXT,
    PRIMARY KEY (schedule_id, scheduled_at)
) WITHOUT ROWID";

/// How many ticks of one schedule are kept.
pub const TICKS_PER_SCHEDULE: i64 = 1_000;
/// How old a kept tick may be: 14 days.
pub const TICK_MAX_AGE_SECONDS: i64 = 14 * 24 * 3600;
/// How many fired ticks of one schedule keep their per-target outcomes.
pub const TICKS_WITH_TARGETS: i64 = 20;

/// What the scheduler decided for a due tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickDecision {
    /// It started a run (`run_id`; NULL when the run could not join the queue).
    Fired,
    /// The schedule was paused.
    Paused,
    /// A run of the scheduled node was in flight (`skipped_for`).
    Skipped,
    /// The schedule's consecutive failed ticks were at its `max_failures`.
    Failing,
}

impl TickDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            TickDecision::Fired => "fired",
            TickDecision::Paused => "paused",
            TickDecision::Skipped => "skipped",
            TickDecision::Failing => "failing",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "fired" => Some(TickDecision::Fired),
            "paused" => Some(TickDecision::Paused),
            "skipped" => Some(TickDecision::Skipped),
            "failing" => Some(TickDecision::Failing),
            _ => None,
        }
    }
}

/// What the run of a fired tick did with one of its targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickTargetOutcome {
    /// Its step executed and succeeded.
    Rebuilt,
    /// Its step was a cache hit.
    UpToDate,
    /// Its step failed.
    Failed,
    /// The run ended before it decided the target: a failure upstream, a cancellation, a run
    /// that never started.
    NotReached,
}

impl TickTargetOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            TickTargetOutcome::Rebuilt => "rebuilt",
            TickTargetOutcome::UpToDate => "up_to_date",
            TickTargetOutcome::Failed => "failed",
            TickTargetOutcome::NotReached => "not_reached",
        }
    }
}

/// A tick as the scheduler records it when it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewTick<'a> {
    /// The scheduled node's id.
    pub schedule_id: &'a str,
    /// The slot, unix seconds.
    pub scheduled_at: i64,
    pub action: TickDecision,
    /// The run in flight, for [`TickDecision::Skipped`].
    pub skipped_for: Option<&'a str>,
    /// The run it started, for [`TickDecision::Fired`].
    pub run_id: Option<&'a str>,
}

/// How many targets of a finished fired tick ended each way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickCounts {
    pub rebuilt: u32,
    pub up_to_date: u32,
    pub failed: u32,
    pub not_reached: u32,
}

impl TickCounts {
    fn of(targets: &[(String, TickTargetOutcome)]) -> Self {
        let mut counts = Self::default();
        for (_, outcome) in targets {
            match outcome {
                TickTargetOutcome::Rebuilt => counts.rebuilt += 1,
                TickTargetOutcome::UpToDate => counts.up_to_date += 1,
                TickTargetOutcome::Failed => counts.failed += 1,
                TickTargetOutcome::NotReached => counts.not_reached += 1,
            }
        }
        counts
    }
}

/// One recorded tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TickRecord {
    pub schedule_id: String,
    /// The slot, unix seconds.
    pub scheduled_at: i64,
    pub action: TickDecision,
    pub skipped_for: Option<String>,
    pub run_id: Option<String>,
    /// When the run left the queue and started, unix seconds. `None` until it does.
    pub started_at: Option<f64>,
    /// When the run recorded how it ended, unix seconds. `None` until it does: a fired tick
    /// with no `finished_at` is queued, running, or was lost with its server.
    pub finished_at: Option<f64>,
    /// How the run ended, as `runs.status` spells it (`success`, `failed`, `error`,
    /// `cancelled`, `interrupted`).
    pub run_status: Option<String>,
    /// From the run joining the queue to its start. `None` until it starts.
    pub queue_wait_seconds: Option<f64>,
    /// Present on a finished fired tick.
    pub counts: Option<TickCounts>,
    /// Each target and its outcome, in the tick's target order. `None` unless the tick is a
    /// finished one among the newest [`TICKS_WITH_TARGETS`] fired ticks of its schedule.
    pub targets: Option<Vec<(String, TickTargetOutcome)>>,
}

/// What one fired tick did with one node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTickOutcome {
    pub schedule_id: String,
    pub scheduled_at: i64,
    pub run_id: Option<String>,
    pub finished_at: Option<f64>,
    pub outcome: TickTargetOutcome,
}

fn failed(what: &'static str) -> impl Fn(libsql::Error) -> BarcaError {
    move |e| BarcaError::Db(format!("schedule_ticks: {what}: {e}"))
}

async fn has_table(conn: &libsql::Connection) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schedule_ticks'",
            (),
        )
        .await
        .map_err(failed("reading the schema"))?;
    let found = rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some();
    while rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some()
    {}
    Ok(found)
}

fn text(value: Option<&str>) -> libsql::Value {
    value.map_or(libsql::Value::Null, |v| libsql::Value::Text(v.to_string()))
}

/// Record what the scheduler decided for one due tick, and trim the schedule's ticks to the
/// newest [`TICKS_PER_SCHEDULE`] and to [`TICK_MAX_AGE_SECONDS`] before this one. Two
/// statements on the primary key in one transaction; a tick recorded twice for the same slot
/// keeps the later decision.
pub async fn record_tick(db_path: &str, tick: NewTick<'_>) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    with_tx(&conn, "BEGIN IMMEDIATE", "tick record", async {
        conn.execute(
            "INSERT OR REPLACE INTO schedule_ticks (schedule_id, scheduled_at, action, skipped_for, run_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![
                tick.schedule_id,
                tick.scheduled_at,
                tick.action.as_str(),
                text(tick.skipped_for),
                text(tick.run_id),
            ],
        )
        .await
        .map_err(failed("recording a tick"))?;
        conn.execute(
            "DELETE FROM schedule_ticks WHERE schedule_id = ?1 AND (scheduled_at < ?2 OR scheduled_at <= (
                 SELECT scheduled_at FROM schedule_ticks WHERE schedule_id = ?1
                 ORDER BY scheduled_at DESC LIMIT 1 OFFSET ?3))",
            libsql::params![
                tick.schedule_id,
                tick.scheduled_at - TICK_MAX_AGE_SECONDS,
                TICKS_PER_SCHEDULE,
            ],
        )
        .await
        .map_err(failed("trimming"))?;
        Ok(())
    })
    .await
}

/// Remove every tick older than [`TICK_MAX_AGE_SECONDS`] before `now`, of any schedule: the
/// ticks of a schedule that no longer fires, which [`record_tick`] never reaches. One scan of
/// the table; the scheduler calls it at startup and about once an hour.
pub async fn trim_ticks(db_path: &str, now: i64) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "DELETE FROM schedule_ticks WHERE scheduled_at < ?1",
        [now - TICK_MAX_AGE_SECONDS],
    )
    .await
    .map_err(failed("trimming by age"))?;
    Ok(())
}

/// Record that the run a fired tick started left the queue: when, and how long it waited there.
/// Only the tick's own row changes (`run_id` must match); with none, nothing does. A run put
/// back in the queue and started again records its latest start.
pub async fn record_tick_started(
    db_path: &str,
    schedule_id: &str,
    scheduled_at: i64,
    run_id: &str,
    started_at: f64,
    queue_wait_seconds: f64,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "UPDATE schedule_ticks SET started_at = ?4, queue_wait_seconds = ?5
         WHERE schedule_id = ?1 AND scheduled_at = ?2 AND run_id = ?3",
        libsql::params![
            schedule_id,
            scheduled_at,
            run_id,
            started_at,
            queue_wait_seconds.max(0.0),
        ],
    )
    .await
    .map_err(failed("recording a run's start"))?;
    Ok(())
}

/// Record how the run a fired tick started ended: when, its status, and what it did with each
/// target (the counts, and the list for the newest [`TICKS_WITH_TARGETS`] fired ticks of the
/// schedule: older ticks' lists are dropped here). Only the tick's own row changes (`run_id`
/// must match); with none, nothing does.
pub async fn record_tick_finished(
    db_path: &str,
    schedule_id: &str,
    scheduled_at: i64,
    run_id: &str,
    finished_at: f64,
    run_status: &str,
    targets: &[(String, TickTargetOutcome)],
) -> Result<(), BarcaError> {
    let counts = TickCounts::of(targets);
    let list = serde_json::to_string(targets)
        .map_err(|e| BarcaError::Db(format!("schedule_ticks: encoding targets: {e}")))?;
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    with_tx(&conn, "BEGIN IMMEDIATE", "tick record", async {
        let changed = conn
            .execute(
                "UPDATE schedule_ticks SET finished_at = ?4, run_status = ?5, rebuilt = ?6,
                     up_to_date = ?7, failed = ?8, not_reached = ?9, targets = ?10
                 WHERE schedule_id = ?1 AND scheduled_at = ?2 AND run_id = ?3",
                libsql::params![
                    schedule_id,
                    scheduled_at,
                    run_id,
                    finished_at,
                    run_status,
                    counts.rebuilt,
                    counts.up_to_date,
                    counts.failed,
                    counts.not_reached,
                    list,
                ],
            )
            .await
            .map_err(failed("recording a run's end"))?;
        if changed == 0 {
            return Ok(());
        }
        conn.execute(
            "UPDATE schedule_ticks SET targets = NULL
             WHERE schedule_id = ?1 AND targets IS NOT NULL AND scheduled_at <= (
                 SELECT scheduled_at FROM schedule_ticks WHERE schedule_id = ?1 AND action = 'fired'
                 ORDER BY scheduled_at DESC LIMIT 1 OFFSET ?2)",
            libsql::params![schedule_id, TICKS_WITH_TARGETS],
        )
        .await
        .map_err(failed("trimming targets"))?;
        Ok(())
    })
    .await
}

const COLUMNS: &str = "schedule_id, scheduled_at, action, skipped_for, run_id, started_at, \
     finished_at, run_status, queue_wait_seconds, rebuilt, up_to_date, failed, not_reached, targets";

fn record_of(row: &libsql::Row) -> Option<TickRecord> {
    let count = |at: i32| row.get::<i64>(at).ok().and_then(|n| u32::try_from(n).ok());
    let counts = match (count(9), count(10), count(11), count(12)) {
        (Some(rebuilt), Some(up_to_date), Some(failed), Some(not_reached)) => Some(TickCounts {
            rebuilt,
            up_to_date,
            failed,
            not_reached,
        }),
        _ => None,
    };
    Some(TickRecord {
        schedule_id: row.get::<String>(0).ok()?,
        scheduled_at: row.get::<i64>(1).ok()?,
        action: TickDecision::parse(&row.get::<String>(2).ok()?)?,
        skipped_for: row.get::<String>(3).ok(),
        run_id: row.get::<String>(4).ok(),
        started_at: row.get::<f64>(5).ok(),
        finished_at: row.get::<f64>(6).ok(),
        run_status: row.get::<String>(7).ok(),
        queue_wait_seconds: row.get::<f64>(8).ok(),
        counts,
        targets: row
            .get::<String>(13)
            .ok()
            .and_then(|list| serde_json::from_str(&list).ok()),
    })
}

/// The recorded ticks of `schedule_id`, newest first, at most `limit`. A database without the
/// table (one older than it, or none at all) has none. A row this version cannot read (an
/// action a newer one wrote) is left out.
pub async fn ticks_for_schedule(
    db_path: &str,
    schedule_id: &str,
    limit: usize,
) -> Result<Vec<TickRecord>, BarcaError> {
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Vec::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            &format!(
                "SELECT {COLUMNS} FROM schedule_ticks WHERE schedule_id = ?1
                 ORDER BY scheduled_at DESC LIMIT ?2"
            ),
            libsql::params![schedule_id, i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
        .map_err(failed("reading a schedule's ticks"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(failed("reading a schedule's ticks"))?
    {
        out.extend(record_of(&row));
    }
    Ok(out)
}

/// Every recorded tick, newest slot first (then by schedule), at most `limit`, and how many
/// there are in all: the rows of `barca.ticks` (`barca sql`). The table is bounded by its
/// writers, so this is one bounded read. A database without the table has none.
pub async fn recent_ticks(
    db_path: &str,
    limit: usize,
) -> Result<(Vec<TickRecord>, usize), BarcaError> {
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok((Vec::new(), 0));
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok((Vec::new(), 0));
    }
    let mut rows = conn
        .query("SELECT COUNT(*) FROM schedule_ticks", ())
        .await
        .map_err(failed("counting the ticks"))?;
    let total = rows
        .next()
        .await
        .map_err(failed("counting the ticks"))?
        .and_then(|row| row.get::<i64>(0).ok())
        .map_or(0, |n| usize::try_from(n).unwrap_or(0));
    while rows
        .next()
        .await
        .map_err(failed("counting the ticks"))?
        .is_some()
    {}
    let mut rows = conn
        .query(
            &format!(
                "SELECT {COLUMNS} FROM schedule_ticks
                 ORDER BY scheduled_at DESC, schedule_id LIMIT ?1"
            ),
            libsql::params![i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
        .map_err(failed("reading the ticks"))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed("reading the ticks"))? {
        out.extend(record_of(&row));
    }
    Ok((out, total))
}

/// What the newest finished fired ticks that had `node_id` as a target did with it, newest
/// first, at most `limit`, across every schedule whose tick reaches the node. Only ticks that
/// still hold their target list answer: the newest [`TICKS_WITH_TARGETS`] fired ticks of each
/// schedule. A database without the table has none.
///
/// It reads the table once (no index: the table is bounded) and parses only the lists that
/// name the node.
pub async fn tick_outcomes_for_node(
    db_path: &str,
    node_id: &str,
    limit: usize,
) -> Result<Vec<NodeTickOutcome>, BarcaError> {
    if limit == 0 || fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Vec::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(Vec::new());
    }
    // The node as the list spells it: a JSON string, followed by its outcome.
    let needle = format!("[{},", serde_json::Value::from(node_id));
    let mut rows = conn
        .query(
            "SELECT schedule_id, scheduled_at, run_id, finished_at, targets FROM schedule_ticks
             WHERE targets IS NOT NULL AND instr(targets, ?1) > 0
             ORDER BY scheduled_at DESC, schedule_id",
            [needle],
        )
        .await
        .map_err(failed("reading a node's ticks"))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(failed("reading a node's ticks"))?
    {
        if out.len() == limit {
            // Drain, so the statement is finished before the connection closes.
            continue;
        }
        let targets: Vec<(String, TickTargetOutcome)> = row
            .get::<String>(4)
            .ok()
            .and_then(|list| serde_json::from_str(&list).ok())
            .unwrap_or_default();
        let Some((_, outcome)) = targets.into_iter().find(|(node, _)| node == node_id) else {
            continue;
        };
        out.push(NodeTickOutcome {
            schedule_id: row.get::<String>(0).unwrap_or_default(),
            scheduled_at: row.get::<i64>(1).unwrap_or_default(),
            run_id: row.get::<String>(2).ok(),
            finished_at: row.get::<f64>(3).ok(),
            outcome,
        });
    }
    Ok(out)
}

/// What the tick record says about every node at once ([`latest_ticks`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatestTicks {
    /// For every node some retained tick had as a target: what the newest finished one did
    /// with it, across schedules (newest by when its run ended, then by slot).
    pub outcomes: HashMap<String, NodeTickOutcome>,
    /// For every node whose newest finished tick did not leave it fresh (`failed` or
    /// `not_reached`): each such tick, newest first, as `(schedule, when its run ended)`,
    /// back to the newest tick that found the node up to date or rebuilt it. Bounded like
    /// `outcomes`, by the retained target lists.
    pub misses: HashMap<String, Vec<(String, f64)>>,
    /// The runs of fired ticks that have not recorded how they ended: queued or running (or
    /// lost with their server, until its next start ends them).
    pub in_flight: Vec<String>,
}

/// [`LatestTicks`]: one read of the table for every node, which is bounded by its writers
/// (the lists of the newest [`TICKS_WITH_TARGETS`] fired ticks of each schedule, and at most
/// [`TICKS_PER_SCHEDULE`] rows a schedule). A database without the table has nothing.
pub async fn latest_ticks(db_path: &str) -> Result<LatestTicks, BarcaError> {
    let mut out = LatestTicks::default();
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(out);
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(out);
    }
    let mut rows = conn
        .query(
            "SELECT schedule_id, scheduled_at, run_id, finished_at, targets FROM schedule_ticks
             WHERE (targets IS NOT NULL AND finished_at IS NOT NULL)
                OR (action = 'fired' AND run_id IS NOT NULL AND finished_at IS NULL)
             ORDER BY finished_at DESC, scheduled_at DESC, schedule_id",
            (),
        )
        .await
        .map_err(failed("reading the newest outcomes"))?;
    // Nodes a tick left fresh: an older miss of one is before that, and not counted.
    let mut settled: HashSet<String> = HashSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(failed("reading the newest outcomes"))?
    {
        let Ok(finished_at) = row.get::<f64>(3) else {
            out.in_flight.extend(row.get::<String>(2).ok());
            continue;
        };
        let targets: Vec<(String, TickTargetOutcome)> = row
            .get::<String>(4)
            .ok()
            .and_then(|list| serde_json::from_str(&list).ok())
            .unwrap_or_default();
        for (node, outcome) in targets {
            if !settled.contains(&node) {
                match outcome {
                    TickTargetOutcome::Failed | TickTargetOutcome::NotReached => out
                        .misses
                        .entry(node.clone())
                        .or_default()
                        .push((row.get::<String>(0).unwrap_or_default(), finished_at)),
                    TickTargetOutcome::Rebuilt | TickTargetOutcome::UpToDate => {
                        settled.insert(node.clone());
                    }
                }
            }
            // Rows come newest first: the first outcome of a node is its newest.
            if let std::collections::hash_map::Entry::Vacant(slot) = out.outcomes.entry(node) {
                slot.insert(NodeTickOutcome {
                    schedule_id: row.get::<String>(0).unwrap_or_default(),
                    scheduled_at: row.get::<i64>(1).unwrap_or_default(),
                    run_id: row.get::<String>(2).ok(),
                    finished_at: Some(finished_at),
                    outcome,
                });
            }
        }
    }
    Ok(out)
}

/// The newest slot recorded for each schedule, whatever the scheduler decided for it (fired,
/// paused, skipped, failing), unix seconds: the last time a scheduler is known to have served
/// the schedule (`barca_core::liveness`). One grouped read of the primary key; a database
/// without the table has none.
pub async fn newest_decided(db_path: &str) -> Result<HashMap<String, i64>, BarcaError> {
    let mut out = HashMap::new();
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(out);
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(out);
    }
    let mut rows = conn
        .query(
            "SELECT schedule_id, MAX(scheduled_at) FROM schedule_ticks GROUP BY schedule_id",
            (),
        )
        .await
        .map_err(failed("reading the newest ticks"))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(failed("reading the newest ticks"))?
    {
        if let (Ok(schedule), Ok(at)) = (row.get::<String>(0), row.get::<i64>(1)) {
            out.insert(schedule, at);
        }
    }
    Ok(out)
}

/// How long the newest fired ticks of one schedule waited and ran ([`tick_timings`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickTimings {
    /// `queue_wait_seconds` of its newest fired ticks whose run started, newest first, at most
    /// [`TICKS_WITH_TARGETS`].
    pub queue_waits: Vec<f64>,
    /// How long the runs of its newest fired ticks that ended `success` took (start to end),
    /// newest first, at most [`TICKS_WITH_TARGETS`].
    pub run_seconds: Vec<f64>,
}

/// [`TickTimings`] of every schedule: one read of the table, which is bounded by its writers
/// (at most [`TICKS_PER_SCHEDULE`] rows a schedule). What a schedule's typical queue wait and
/// typical run time are the medians of. A database without the table has none.
pub async fn tick_timings(db_path: &str) -> Result<HashMap<String, TickTimings>, BarcaError> {
    let mut out: HashMap<String, TickTimings> = HashMap::new();
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(out);
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(out);
    }
    let mut rows = conn
        .query(
            "SELECT schedule_id, queue_wait_seconds, started_at, finished_at, run_status
             FROM schedule_ticks WHERE action = 'fired' AND started_at IS NOT NULL
             ORDER BY schedule_id, scheduled_at DESC",
            (),
        )
        .await
        .map_err(failed("reading tick timings"))?;
    let window = TICKS_WITH_TARGETS as usize;
    while let Some(row) = rows.next().await.map_err(failed("reading tick timings"))? {
        let timings = out
            .entry(row.get::<String>(0).unwrap_or_default())
            .or_default();
        if timings.queue_waits.len() < window
            && let Ok(wait) = row.get::<f64>(1)
        {
            timings.queue_waits.push(wait.max(0.0));
        }
        if timings.run_seconds.len() < window
            && row.get::<String>(4).is_ok_and(|status| status == "success")
            && let (Ok(started), Ok(finished)) = (row.get::<f64>(2), row.get::<f64>(3))
        {
            timings.run_seconds.push((finished - started).max(0.0));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        (dir, db_path)
    }

    fn tick<'a>(
        schedule_id: &'a str,
        scheduled_at: i64,
        action: TickDecision,
        skipped_for: Option<&'a str>,
        run_id: Option<&'a str>,
    ) -> NewTick<'a> {
        NewTick {
            schedule_id,
            scheduled_at,
            action,
            skipped_for,
            run_id,
        }
    }

    const JOB: &str = "pipeline.py:inbox";

    #[tokio::test]
    async fn every_schedules_ticks_are_read_newest_first_with_their_count() {
        let (_dir, db) = fresh().await;
        use TickDecision::*;
        assert_eq!(recent_ticks(&db, 10).await.unwrap(), (Vec::new(), 0));
        record_tick(&db, tick(JOB, 100, Fired, None, Some("run-1")))
            .await
            .unwrap();
        record_tick(&db, tick("pipeline.py:pulse", 130, Paused, None, None))
            .await
            .unwrap();
        record_tick(&db, tick(JOB, 160, Skipped, Some("run-1"), None))
            .await
            .unwrap();
        let (ticks, total) = recent_ticks(&db, 2).await.unwrap();
        assert_eq!(total, 3);
        let slots: Vec<(&str, i64)> = ticks
            .iter()
            .map(|t| (t.schedule_id.as_str(), t.scheduled_at))
            .collect();
        assert_eq!(slots, [(JOB, 160), ("pipeline.py:pulse", 130)]);
        let missing = _dir.path().join("none.db").to_string_lossy().to_string();
        assert_eq!(recent_ticks(&missing, 10).await.unwrap(), (Vec::new(), 0));
    }

    #[tokio::test]
    async fn each_decision_is_one_row_read_newest_first() {
        let (_dir, db) = fresh().await;
        use TickDecision::*;
        record_tick(&db, tick(JOB, 100, Fired, None, Some("run-1")))
            .await
            .unwrap();
        record_tick(&db, tick(JOB, 160, Skipped, Some("run-1"), None))
            .await
            .unwrap();
        record_tick(&db, tick(JOB, 220, Paused, None, None))
            .await
            .unwrap();
        record_tick(&db, tick(JOB, 280, Failing, None, None))
            .await
            .unwrap();
        record_tick(&db, tick("pipeline.py:other", 280, Fired, None, None))
            .await
            .unwrap();

        let ticks = ticks_for_schedule(&db, JOB, 10).await.unwrap();
        let seen: Vec<_> = ticks
            .iter()
            .map(|t| {
                (
                    t.scheduled_at,
                    t.action,
                    t.skipped_for.as_deref(),
                    t.run_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                (280, Failing, None, None),
                (220, Paused, None, None),
                (160, Skipped, Some("run-1"), None),
                (100, Fired, None, Some("run-1")),
            ]
        );
        // Nothing about a run until it starts and ends.
        assert!(ticks.iter().all(|t| t.started_at.is_none()
            && t.finished_at.is_none()
            && t.run_status.is_none()
            && t.queue_wait_seconds.is_none()
            && t.counts.is_none()
            && t.targets.is_none()));
        assert_eq!(ticks_for_schedule(&db, JOB, 2).await.unwrap().len(), 2);
        // A fired tick whose run could not join the queue has no run.
        let other = ticks_for_schedule(&db, "pipeline.py:other", 10)
            .await
            .unwrap();
        assert_eq!((other.len(), other[0].run_id.clone()), (1, None));
    }

    #[tokio::test]
    async fn a_run_adds_its_start_its_end_and_each_targets_outcome() {
        let (_dir, db) = fresh().await;
        record_tick(
            &db,
            tick(JOB, 100, TickDecision::Fired, None, Some("run-1")),
        )
        .await
        .unwrap();
        record_tick_started(&db, JOB, 100, "run-1", 104.5, 4.5)
            .await
            .unwrap();
        let started = &ticks_for_schedule(&db, JOB, 1).await.unwrap()[0];
        assert_eq!(started.started_at, Some(104.5));
        assert_eq!(started.queue_wait_seconds, Some(4.5));
        assert_eq!(started.finished_at, None, "started, not finished");

        let targets = vec![
            (JOB.to_string(), TickTargetOutcome::Rebuilt),
            (
                "pipeline.py:orders".to_string(),
                TickTargetOutcome::UpToDate,
            ),
            ("pipeline.py:report".to_string(), TickTargetOutcome::Failed),
            (
                "pipeline.py:export".to_string(),
                TickTargetOutcome::NotReached,
            ),
        ];
        record_tick_finished(&db, JOB, 100, "run-1", 110.0, "failed", &targets)
            .await
            .unwrap();
        let done = &ticks_for_schedule(&db, JOB, 1).await.unwrap()[0];
        assert_eq!(done.finished_at, Some(110.0));
        assert_eq!(done.run_status.as_deref(), Some("failed"));
        assert_eq!(
            done.counts,
            Some(TickCounts {
                rebuilt: 1,
                up_to_date: 1,
                failed: 1,
                not_reached: 1
            })
        );
        assert_eq!(done.targets.as_ref(), Some(&targets));

        // Another run's end does not touch the tick, nor does a tick that was never recorded.
        record_tick_finished(&db, JOB, 100, "run-other", 999.0, "success", &[])
            .await
            .unwrap();
        record_tick_finished(&db, JOB, 5_000, "run-1", 999.0, "success", &[])
            .await
            .unwrap();
        record_tick_started(&db, JOB, 100, "run-other", 999.0, 1.0)
            .await
            .unwrap();
        assert_eq!(
            &ticks_for_schedule(&db, JOB, 10).await.unwrap(),
            std::slice::from_ref(done)
        );
    }

    #[tokio::test]
    async fn a_nodes_outcomes_come_from_every_schedule_that_reaches_it() {
        let (_dir, db) = fresh().await;
        let orders = "pipeline.py:orders";
        // A node whose id is a prefix of another's is not confused with it.
        let longer = "pipeline.py:orders_daily";
        for (schedule, at, run, outcome) in [
            (JOB, 100, "run-1", TickTargetOutcome::Rebuilt),
            (
                "pipeline.py:nightly",
                150,
                "run-2",
                TickTargetOutcome::UpToDate,
            ),
            (JOB, 200, "run-3", TickTargetOutcome::Failed),
        ] {
            record_tick(
                &db,
                tick(schedule, at, TickDecision::Fired, None, Some(run)),
            )
            .await
            .unwrap();
            record_tick_finished(
                &db,
                schedule,
                at,
                run,
                at as f64 + 5.0,
                "success",
                &[
                    (schedule.to_string(), TickTargetOutcome::Rebuilt),
                    (orders.to_string(), outcome),
                ],
            )
            .await
            .unwrap();
        }
        // A fired tick still running, a skipped one and one that never had the node: no answer.
        record_tick(
            &db,
            tick(JOB, 300, TickDecision::Fired, None, Some("run-4")),
        )
        .await
        .unwrap();
        record_tick(
            &db,
            tick(JOB, 360, TickDecision::Skipped, Some("run-4"), None),
        )
        .await
        .unwrap();
        record_tick(
            &db,
            tick(
                "pipeline.py:hourly",
                400,
                TickDecision::Fired,
                None,
                Some("run-5"),
            ),
        )
        .await
        .unwrap();
        record_tick_finished(
            &db,
            "pipeline.py:hourly",
            400,
            "run-5",
            401.0,
            "success",
            &[(longer.to_string(), TickTargetOutcome::Rebuilt)],
        )
        .await
        .unwrap();

        let got = tick_outcomes_for_node(&db, orders, 10).await.unwrap();
        let seen: Vec<_> = got
            .iter()
            .map(|o| (o.schedule_id.as_str(), o.scheduled_at, o.outcome))
            .collect();
        assert_eq!(
            seen,
            vec![
                (JOB, 200, TickTargetOutcome::Failed),
                ("pipeline.py:nightly", 150, TickTargetOutcome::UpToDate),
                (JOB, 100, TickTargetOutcome::Rebuilt),
            ]
        );
        assert_eq!(got[0].run_id.as_deref(), Some("run-3"));
        assert_eq!(got[0].finished_at, Some(205.0));
        assert_eq!(
            tick_outcomes_for_node(&db, orders, 2).await.unwrap().len(),
            2
        );
        assert_eq!(
            tick_outcomes_for_node(&db, longer, 10).await.unwrap().len(),
            1
        );
        assert!(
            tick_outcomes_for_node(&db, "pipeline.py:absent", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    async fn count(db: &str, schedule: &str) -> usize {
        ticks_for_schedule(db, schedule, usize::MAX)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn a_schedule_keeps_its_newest_thousand_ticks_and_none_older_than_fourteen_days() {
        let (_dir, db) = fresh().await;
        // An every-minute schedule: 1,005 ticks.
        for i in 0..1_005 {
            record_tick(&db, tick(JOB, i * 60, TickDecision::Paused, None, None))
                .await
                .unwrap();
        }
        let ticks = ticks_for_schedule(&db, JOB, usize::MAX).await.unwrap();
        assert_eq!(ticks.len(), 1_000);
        assert_eq!(ticks[0].scheduled_at, 1_004 * 60);
        assert_eq!(ticks[999].scheduled_at, 5 * 60, "the oldest five are gone");

        // A daily schedule: the fifteenth tick drops the first, exactly 14 days is kept.
        let daily = "pipeline.py:daily";
        for day in 0..=14 {
            record_tick(
                &db,
                tick(daily, day * 86_400, TickDecision::Paused, None, None),
            )
            .await
            .unwrap();
        }
        assert_eq!(count(&db, daily).await, 15);
        record_tick(
            &db,
            tick(daily, 14 * 86_400 + 1, TickDecision::Paused, None, None),
        )
        .await
        .unwrap();
        assert_eq!(count(&db, daily).await, 15, "day 0 went, the new one came");
        // Trimming one schedule leaves the other alone.
        assert_eq!(count(&db, JOB).await, 1_000);

        // A schedule that no longer ticks is trimmed by age across the table.
        trim_ticks(&db, 1_004 * 60 + TICK_MAX_AGE_SECONDS - 60)
            .await
            .unwrap();
        assert_eq!(
            count(&db, JOB).await,
            2,
            "the two newest are within 14 days"
        );
        trim_ticks(&db, i64::MAX / 2).await.unwrap();
        assert_eq!(count(&db, JOB).await + count(&db, daily).await, 0);
    }

    #[tokio::test]
    async fn target_lists_are_kept_for_the_newest_twenty_fired_ticks() {
        let (_dir, db) = fresh().await;
        let targets = vec![(JOB.to_string(), TickTargetOutcome::Rebuilt)];
        for i in 0..25_i64 {
            let run = format!("run-{i}");
            record_tick(
                &db,
                tick(JOB, i * 120, TickDecision::Fired, None, Some(&run)),
            )
            .await
            .unwrap();
            // A skipped tick between fired ones does not count toward the twenty.
            record_tick(
                &db,
                tick(JOB, i * 120 + 60, TickDecision::Skipped, Some(&run), None),
            )
            .await
            .unwrap();
            record_tick_finished(
                &db,
                JOB,
                i * 120,
                &run,
                i as f64 * 120.0 + 90.0,
                "success",
                &targets,
            )
            .await
            .unwrap();
        }
        let fired: Vec<TickRecord> = ticks_for_schedule(&db, JOB, usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.action == TickDecision::Fired)
            .collect();
        assert_eq!(fired.len(), 25);
        assert!(fired[..20].iter().all(|t| t.targets.is_some()));
        assert!(fired[20..].iter().all(|t| t.targets.is_none()));
        // The counts stay.
        assert!(
            fired
                .iter()
                .all(|t| t.counts.is_some_and(|c| c.rebuilt == 1))
        );
        assert_eq!(
            tick_outcomes_for_node(&db, JOB, 100).await.unwrap().len(),
            20
        );
    }

    #[tokio::test]
    async fn a_database_without_the_table_has_no_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.db").to_string_lossy().to_string();
        assert!(
            ticks_for_schedule(&missing, JOB, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            tick_outcomes_for_node(&missing, JOB, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let (_dir, db) = fresh().await;
        {
            let _g = db_guard_for(&db).await;
            let (_db, conn) = open_conn(&db).await.unwrap();
            conn.execute("DROP TABLE schedule_ticks", ()).await.unwrap();
        }
        assert!(ticks_for_schedule(&db, JOB, 10).await.unwrap().is_empty());
        assert!(
            tick_outcomes_for_node(&db, JOB, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// One read answers for every node: its newest finished outcome across schedules, and
    /// the runs of ticks still in flight.
    #[tokio::test]
    async fn the_timings_of_every_schedule_are_read_at_once() {
        let (_dir, db) = fresh().await;
        assert!(tick_timings(&db).await.unwrap().is_empty());
        // Three fired ticks: one finished, one failed, one still running; and one that waits.
        for (slot, run, wait, end) in [
            (100, "run-1", 4.5, Some(("success", 110.0))),
            (160, "run-2", 0.0, Some(("failed", 163.0))),
            (220, "run-3", 2.0, None),
        ] {
            record_tick(&db, tick(JOB, slot, TickDecision::Fired, None, Some(run)))
                .await
                .unwrap();
            record_tick_started(&db, JOB, slot, run, slot as f64 + wait, wait)
                .await
                .unwrap();
            if let Some((status, at)) = end {
                record_tick_finished(&db, JOB, slot, run, at, status, &[])
                    .await
                    .unwrap();
            }
        }
        record_tick(
            &db,
            tick(JOB, 280, TickDecision::Fired, None, Some("run-4")),
        )
        .await
        .unwrap();
        record_tick(&db, tick(JOB, 340, TickDecision::Paused, None, None))
            .await
            .unwrap();
        let timings = tick_timings(&db).await.unwrap();
        assert_eq!(timings.len(), 1);
        // Newest first; a tick whose run has not started has no wait yet.
        assert_eq!(timings[JOB].queue_waits, [2.0, 0.0, 4.5]);
        // Only a run that succeeded says how long a run takes.
        assert_eq!(timings[JOB].run_seconds, [5.5]);
    }

    #[tokio::test]
    async fn the_newest_outcome_of_every_node_is_read_at_once() {
        let (_dir, db) = fresh().await;
        let (orders, report) = ("pipeline.py:orders", "pipeline.py:report");
        use TickTargetOutcome::*;
        for (job, slot, run, finished, targets) in [
            (
                JOB,
                100,
                "run-1",
                105.0,
                vec![(orders, Rebuilt), (report, Rebuilt)],
            ),
            (
                "pipeline.py:nightly",
                150,
                "run-2",
                190.0,
                vec![(orders, UpToDate)],
            ),
            (
                JOB,
                160,
                "run-3",
                170.0,
                vec![(orders, Failed), (report, NotReached)],
            ),
        ] {
            record_tick(&db, tick(job, slot, TickDecision::Fired, None, Some(run)))
                .await
                .unwrap();
            let targets: Vec<(String, TickTargetOutcome)> = targets
                .into_iter()
                .map(|(n, o)| (n.to_string(), o))
                .collect();
            record_tick_finished(&db, job, slot, run, finished, "success", &targets)
                .await
                .unwrap();
        }
        // A tick still running, one that was skipped, and one that never joined the queue.
        record_tick(
            &db,
            tick(JOB, 220, TickDecision::Fired, None, Some("run-4")),
        )
        .await
        .unwrap();
        record_tick(
            &db,
            tick(JOB, 280, TickDecision::Skipped, Some("run-4"), None),
        )
        .await
        .unwrap();
        record_tick(
            &db,
            tick("pipeline.py:nightly", 300, TickDecision::Fired, None, None),
        )
        .await
        .unwrap();

        let got = latest_ticks(&db).await.unwrap();
        // Newest by when the run ended, not by slot: the nightly tick ended last.
        let seen = |node: &str| {
            let o = &got.outcomes[node];
            (
                o.schedule_id.clone(),
                o.scheduled_at,
                o.finished_at,
                o.outcome,
            )
        };
        assert_eq!(
            seen(orders),
            (
                "pipeline.py:nightly".to_string(),
                150,
                Some(190.0),
                UpToDate
            )
        );
        assert_eq!(
            seen(report),
            (JOB.to_string(), 160, Some(170.0), NotReached)
        );
        assert_eq!(got.outcomes.len(), 2);
        assert_eq!(got.in_flight, vec!["run-4".to_string()]);
        // The ticks that did not leave a node fresh, back to the last one that did: `orders`
        // failed at 170 and was found up to date at 190; `report` was not reached at 170, after
        // the tick that rebuilt it.
        assert_eq!(
            got.misses,
            HashMap::from([(report.to_string(), vec![(JOB.to_string(), 170.0)])])
        );

        let (_dir, empty) = fresh().await;
        assert_eq!(latest_ticks(&empty).await.unwrap(), LatestTicks::default());
        assert_eq!(
            latest_ticks("/nonexistent/metadata.db").await.unwrap(),
            LatestTicks::default()
        );
    }
}
