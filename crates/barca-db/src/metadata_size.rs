//! How much of the size limit the metadata database may use, and keeping it there.
//!
//! The database is counted in `[cache] max_size` and eviction cannot delete it, so a database
//! that outgrows the limit makes every check evict artifacts for nothing (41 GB against a
//! 13 GB limit whose artifacts took 7 GB). [`trim`] keeps it within [`allowance`]: past it, this
//! removes what explains or reports history and leaves what a cache decision reads, in this
//! order, down to [`LOW_WATERMARK_PERCENT`] of it:
//!
//! 1. `components` of superseded rows, and those written before they were split from
//!    `definition_components`. Only a node's latest success is ever read, and the digests never
//!    decide anything: a node without them says a plain `changed`.
//! 2. `runs.outcome_json`, except the last [`KEPT_OUTCOMES`] runs'. `GET /runs/{id}` then
//!    answers from the run's own row.
//! 3. Rows older than their node's latest success whose artifact file is gone. A return to that
//!    exact state computes again. A row whose file is on disk stays: it is what names the file.
//! 4. Runs and their logs, oldest first, except the last [`KEPT_RUNS`], a run still running and
//!    a run in the queue.
//!
//! Each batch takes the database lock for itself, so a run's writes go on between batches, and
//! no hold of the lock lasts past [`HOLD_TARGET`] (batches shrink to keep to it; the longest
//! hold of a pass is reported). SQLite reuses freed pages but does not give them back, and a
//! page a row left half empty still counts as used, so only `VACUUM` says what the database
//! really takes. Steps 3 and 4 delete history, so each waits for one and runs only if that was
//! not enough: with a run alive, they wait for the next check.
//!
//! **Giving the space back** is bounded too. A database created with `auto_vacuum=INCREMENTAL`
//! gives freed pages back in chunks (`PRAGMA incremental_vacuum(N)`), each under its own hold
//! of the lock. Every other database (all that earlier releases created: barca does not change
//! the mode of an existing file) needs a full `VACUUM`, which rewrites the file with the lock
//! held and cannot be cut short. A pass with a time budget runs one only when its estimated time
//! ([`VACUUM_BYTES_PER_SEC`]) fits what is left of the budget, and with no run alive; otherwise
//! it says so ([`Limit::VacuumDeferred`], [`Limit::LiveRun`]) and `barca gc`, which has no
//! budget, does it. A `VACUUM` that a reader keeps from reaching the file ([`Limit::CheckpointBlocked`])
//! frees nothing on disk, so a pass says that too.
//!
//! **Why a pass stopped.** A pass over the share that did not get under it names why in
//! [`Trimmed::limit`]: the working set alone is larger than the share, a live run protects what
//! is left, a reader holds the log, VACUUM was deferred, or what is left is history the
//! retention constants ([`KEPT_OUTCOMES`], [`KEPT_RUNS`]) keep.

use crate::db;
use barca_model::BarcaError;
use libsql::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The largest share of the disk the database gets, however large the limit.
const LARGEST_ALLOWANCE: u64 = 1 << 30;
/// The database gets this share of the limit.
const ALLOWANCE_PERCENT: u64 = 10;
/// Trimming stops once the database holds at most this share of its allowance.
const LOW_WATERMARK_PERCENT: u64 = 80;
/// `components` larger than this were written before definitions were stored once. A row's own
/// part names its inputs, in a few hundred bytes.
const OVERSIZED_COMPONENTS: u64 = 16 * 1024;
const KEPT_OUTCOMES: i64 = 100;
const KEPT_RUNS: i64 = 1000;
/// Row ids one batch looks at (at most), and runs one batch changes.
const ID_SPAN: i64 = 2000;
const MIN_ID_SPAN: i64 = 50;
const RUN_BATCH: i64 = 200;
/// No hold of the database lock should last past this: a batch that took longer halves the next
/// one.
pub const HOLD_TARGET: Duration = Duration::from_millis(250);
/// How fast a full `VACUUM` writes, at the least: it rewrites the live pages (1.2 GB took 40 to
/// 45 s). A pass with a time budget runs one only if the estimate fits.
pub const VACUUM_BYTES_PER_SEC: u64 = 25 << 20;
/// Pages one `incremental_vacuum` chunk starts with; later chunks scale to [`HOLD_TARGET`].
const FIRST_CHUNK_PAGES: i64 = 256;
/// How long one batch of step 4 deletes for before it lets the lock go: a run's logs can be
/// thousands of lines, and 200 runs of 100 lines each held the lock for 0.4 to 1.1 s.
const RUN_BATCH_TIME: Duration = Duration::from_millis(100);

/// What the metadata database may use of `limit` (`None`: `[cache] max_size` is off).
pub fn allowance(limit: Option<u64>) -> u64 {
    limit.map_or(LARGEST_ALLOWANCE, |limit| {
        (limit / 100 * ALLOWANCE_PERCENT).min(LARGEST_ALLOWANCE)
    })
}

/// Why a pass that ended over its allowance did not get under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Limit {
    /// Everything trim may remove is gone, and what the database holds without the history the
    /// retention constants keep is larger than the allowance: the current working set (the
    /// definitions and rows that name results on disk) does not fit this share.
    WorkingSet { working_set_bytes: u64 },
    /// A run that is alive here protects the rest: `VACUUM` and deleting history wait for it.
    LiveRun { run_id: String },
    /// A reader kept the checkpoint from folding the log into the file, so space freed in the
    /// database is not yet space freed on disk.
    CheckpointBlocked { log_bytes: u64 },
    /// What is left is run history the retention constants keep ([`KEPT_OUTCOMES`] outcomes,
    /// [`KEPT_RUNS`] runs): nothing more is eligible under them.
    NoEligibleHistory { history_bytes: u64, runs: u64 },
    /// A full `VACUUM` would hold the lock for longer than the pass's budget, so it was left
    /// for `barca gc`.
    VacuumDeferred { estimated_ms: u64, budget_ms: u64 },
}

impl Limit {
    /// The stable name of the reason (`working_set`, `live_run`, `checkpoint_blocked`,
    /// `no_eligible_history`, `vacuum_deferred`).
    pub fn code(&self) -> &'static str {
        match self {
            Limit::WorkingSet { .. } => "working_set",
            Limit::LiveRun { .. } => "live_run",
            Limit::CheckpointBlocked { .. } => "checkpoint_blocked",
            Limit::NoEligibleHistory { .. } => "no_eligible_history",
            Limit::VacuumDeferred { .. } => "vacuum_deferred",
        }
    }

    /// The reason in words, for the log and `barca status`.
    pub fn describe(&self) -> String {
        let human = barca_model::cache_size::human;
        match self {
            Limit::WorkingSet { working_set_bytes } => format!(
                "the current working set ({}) is larger than the share; no history is left to \
                 remove",
                human(*working_set_bytes)
            ),
            Limit::LiveRun { run_id } => format!(
                "run {run_id} is alive: VACUUM deferred: server busy, and history is protected \
                 until it ends"
            ),
            Limit::CheckpointBlocked { log_bytes } => format!(
                "a reader is blocking the checkpoint, so {} of log cannot be folded into the \
                 file yet",
                human(*log_bytes)
            ),
            Limit::NoEligibleHistory { history_bytes, .. } => format!(
                "no eligible history: the {KEPT_RUNS} newest runs and {KEPT_OUTCOMES} newest \
                 outcomes ({}) are kept by the retention constants",
                human(*history_bytes)
            ),
            Limit::VacuumDeferred {
                estimated_ms,
                budget_ms,
            } => format!(
                "VACUUM deferred: server busy; a full VACUUM would take about {estimated_ms} \
                 ms against a budget of {budget_ms} ms (`barca gc` runs it)"
            ),
        }
    }

    /// The run that protects what is left, for [`Limit::LiveRun`].
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Limit::LiveRun { run_id } => Some(run_id),
            _ => None,
        }
    }
}

/// What one [`trim`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trimmed {
    pub components: u64,
    pub outcomes: u64,
    pub rows: u64,
    pub runs: u64,
    pub before_bytes: u64,
    pub after_bytes: u64,
    /// The write-ahead log beside the file after the pass.
    #[serde(default)]
    pub log_bytes: u64,
    /// Every step was tried: what is left is not removable now.
    pub finished: bool,
    /// Stopped because time ran out, not to wait for a moment with no run alive.
    pub out_of_time: bool,
    /// Why the file is still over its allowance (`None` when it is not, or when the pass ran
    /// out of time and a later one goes on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    /// The longest time one hold of the database lock lasted in the pass, in milliseconds.
    #[serde(default)]
    pub longest_hold_ms: u64,
    /// How the space freed in the file went back: `none`, `incremental` or `full`.
    #[serde(default)]
    pub vacuum: String,
}

impl Trimmed {
    pub fn removed(&self) -> bool {
        self.components + self.outcomes + self.rows + self.runs > 0
    }

    /// The file shrank, or the pass is still removing things and a later one goes on.
    pub fn progressed(&self) -> bool {
        self.after_bytes < self.before_bytes || (self.out_of_time && self.removed())
    }
}

/// Where a pass that ran out of time stopped: (step, row id), by database. A later pass in this
/// process goes on from there instead of looking at the rows it kept again.
static RESUME: Mutex<Option<HashMap<String, (u8, i64)>>> = Mutex::new(None);

fn resume_at(db_path: &str) -> (u8, i64) {
    RESUME
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|at| at.get(db_path).copied())
        .unwrap_or((0, 0))
}

fn save_resume(db_path: &str, at: Option<(u8, i64)>) {
    let mut resume = RESUME.lock().unwrap();
    let map = resume.get_or_insert_with(HashMap::new);
    match at {
        Some(at) => map.insert(db_path.to_string(), at),
        None => map.remove(db_path),
    };
}

fn failed(what: &str) -> impl Fn(libsql::Error) -> BarcaError + '_ {
    move |e| BarcaError::Db(format!("failed to trim the metadata database: {what}: {e}"))
}

async fn scalar(conn: &Connection, sql: &str) -> Result<i64, BarcaError> {
    let mut rows = conn.query(sql, ()).await.map_err(failed(sql))?;
    match rows.next().await.map_err(failed(sql))? {
        Some(row) => Ok(row.get::<Option<i64>>(0).map_err(failed(sql))?.unwrap_or(0)),
        None => Ok(0),
    }
}

/// The bytes the database's pages hold, freed pages left out.
async fn used_bytes(conn: &Connection) -> Result<u64, BarcaError> {
    let page_size = scalar(conn, "PRAGMA page_size").await?;
    let pages = scalar(conn, "PRAGMA page_count").await?;
    let free = scalar(conn, "PRAGMA freelist_count").await?;
    Ok((pages.saturating_sub(free).saturating_mul(page_size)).max(0) as u64)
}

fn file_bytes(db_path: &str) -> u64 {
    std::fs::metadata(db_path).map_or(0, |m| m.len())
}

/// The id of a node's latest success with a result, for a row `m` of `materializations`.
const LATEST_SUCCESS: &str = "(SELECT s.id FROM materializations s INDEXED BY idx_mat_node_latest \
     WHERE s.node_id = m.node_id AND s.status = 'success' AND s.artifact_path IS NOT NULL \
     ORDER BY s.id DESC LIMIT 1)";

/// A row `m` that a pin or an upload names: never changed.
const NOT_HELD: &str = "NOT EXISTS (SELECT 1 FROM asset_pins p WHERE p.node_id = m.node_id \
     AND p.run_hash = m.run_hash) AND NOT EXISTS (SELECT 1 FROM asset_uploads u \
     WHERE u.node_id = m.node_id AND u.run_hash = m.run_hash)";

async fn ids_in(
    conn: &Connection,
    filter: &str,
    from: i64,
    span: i64,
) -> Result<Vec<i64>, BarcaError> {
    let sql = format!(
        "SELECT m.id FROM materializations m WHERE m.id > {from} AND m.id <= {} AND {filter}",
        from.saturating_add(span)
    );
    let mut rows = conn.query(&sql, ()).await.map_err(failed("find rows"))?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed("find rows"))? {
        ids.push(row.get::<i64>(0).map_err(failed("find rows"))?);
    }
    Ok(ids)
}

/// One batch of step 1 or 3, over the ids after `from`. The number of rows changed.
async fn row_batch(conn: &Connection, step: u8, from: i64, span: i64) -> Result<u64, BarcaError> {
    if step == 0 {
        let filter = format!(
            "m.components IS NOT NULL AND (octet_length(m.components) > {OVERSIZED_COMPONENTS} \
             OR m.id < {LATEST_SUCCESS}) AND {NOT_HELD}"
        );
        let ids = ids_in(conn, &filter, from, span).await?;
        if ids.is_empty() {
            return Ok(0);
        }
        return db::materializations::clear_components(conn, &ids)
            .await
            .map_err(failed("clear components"));
    }
    // The newest byte change of a node stays: `barca status` shows it.
    let filter = format!(
        "m.id < {LATEST_SUCCESS} AND {NOT_HELD} \
         AND NOT (m.status = 'success' AND m.artifact_path IS NOT NULL AND m.evicted_at IS NULL) \
         AND NOT (m.bytes_changed = 1 AND m.id = (SELECT max(c.id) FROM materializations c \
             INDEXED BY idx_mat_changed WHERE c.node_id = m.node_id AND c.bytes_changed = 1))"
    );
    let ids = ids_in(conn, &filter, from, span).await?;
    if ids.is_empty() {
        return Ok(0);
    }
    db::materializations::delete_rows(conn, &ids)
        .await
        .map_err(failed("delete rows"))
}

/// One batch of step 4: the oldest runs that may go, logs first.
async fn run_batch(conn: &Connection) -> Result<u64, BarcaError> {
    let sql = format!(
        "SELECT run_id FROM runs WHERE id < (SELECT id FROM runs ORDER BY id DESC LIMIT 1 \
         OFFSET {}) AND status != 'running' AND run_id NOT IN (SELECT run_id FROM run_queue) \
         ORDER BY id LIMIT {RUN_BATCH}",
        KEPT_RUNS - 1
    );
    let mut rows = conn.query(&sql, ()).await.map_err(failed("find runs"))?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed("find runs"))? {
        runs.push(row.get::<String>(0).map_err(failed("find runs"))?);
    }
    drop(rows);
    let started = Instant::now();
    let mut deleted = 0;
    for run in &runs {
        db::runs::delete_logs(conn, run)
            .await
            .map_err(failed("delete logs"))?;
        db::runs::delete_run(conn, run)
            .await
            .map_err(failed("delete runs"))?;
        deleted += 1;
        // The rest are found again by the next batch, under its own hold of the lock.
        if started.elapsed() >= RUN_BATCH_TIME {
            break;
        }
    }
    Ok(deleted)
}

/// Definitions no remaining row names. Only with no run alive: a starting run writes its
/// definitions before its rows.
async fn drop_unnamed_definitions(conn: &Connection) -> Result<(), BarcaError> {
    db::materializations::delete_unnamed_definitions(conn)
        .await
        .map_err(failed("delete definitions"))?;
    Ok(())
}

/// Tuning of a pass. [`trim`] uses the defaults; a test shrinks them to reach a case with a small
/// store.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// How fast a full `VACUUM` is taken to write ([`VACUUM_BYTES_PER_SEC`]).
    pub vacuum_bytes_per_sec: u64,
    /// The longest a hold of the lock should last ([`HOLD_TARGET`]).
    pub hold_target: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            vacuum_bytes_per_sec: VACUUM_BYTES_PER_SEC,
            hold_target: HOLD_TARGET,
        }
    }
}

/// A hold of the database lock: the longest of a pass is what it reports.
struct Hold<'a> {
    longest: &'a AtomicU64,
    since: Instant,
}

impl<'a> Hold<'a> {
    fn start(longest: &'a AtomicU64) -> Self {
        Self {
            longest,
            since: Instant::now(),
        }
    }

    fn elapsed(&self) -> Duration {
        self.since.elapsed()
    }
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let micros = self.since.elapsed().as_micros() as u64;
        self.longest.fetch_max(micros, Ordering::Relaxed);
    }
}

/// How a pass gave freed space back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vacuumed {
    No,
    Incremental,
    Full,
}

impl Vacuumed {
    fn name(self) -> &'static str {
        match self {
            Vacuumed::No => "none",
            Vacuumed::Incremental => "incremental",
            Vacuumed::Full => "full",
        }
    }
}

/// What [`reclaim`] did.
struct Reclaim {
    how: Vacuumed,
    /// The file really holds what is left, so deleting history is judged on its real size.
    settled: bool,
    /// Why not (a run alive, a full `VACUUM` over budget) or what held it back (a reader).
    limit: Option<Limit>,
}

/// The time left until `deadline` (`None`: no budget).
fn left(deadline: Option<Instant>) -> Option<Duration> {
    deadline.map(|d| d.saturating_duration_since(Instant::now()))
}

/// Bring `db_path` within `allowance`, for at most `time` (`None`: until it is done), `VACUUM`
/// counted: a pass with a budget runs a full one only when its estimated time fits what is left.
/// `None` when its file is within the allowance already.
pub async fn trim(
    db_path: &str,
    allowance: u64,
    time: Option<Duration>,
) -> Result<Option<Trimmed>, BarcaError> {
    trim_with(db_path, allowance, time, Policy::default()).await
}

/// [`trim`] with its tuning given.
pub async fn trim_with(
    db_path: &str,
    allowance: u64,
    time: Option<Duration>,
    policy: Policy,
) -> Result<Option<Trimmed>, BarcaError> {
    let before_bytes = file_bytes(db_path);
    if before_bytes <= allowance {
        return Ok(None);
    }
    db::init_db(db_path).await?;
    let target = allowance / 100 * LOW_WATERMARK_PERCENT;
    let deadline = time.map(|time| Instant::now() + time);
    let out_of_time = || deadline.is_some_and(|deadline| Instant::now() >= deadline);
    let longest = AtomicU64::new(0);
    let mut done = Trimmed {
        before_bytes,
        finished: true,
        ..Trimmed::default()
    };
    let mut how = Vacuumed::No;
    let mut blocked: Option<Limit> = None;
    let (mut step, mut from) = resume_at(db_path);
    let mut span = ID_SPAN;
    // Whether the file may hold space only a vacuum gives back, and the step that last asked.
    // A pass that has not removed anything starts with it only if pages are already free or a
    // large log is waiting: a store that cannot shrink is not rewritten again for nothing.
    let mut dirty = {
        let _guard = db::db_guard_for(db_path).await;
        let _hold = Hold::start(&longest);
        let (_handle, conn) = db::open_conn(db_path).await?;
        scalar(&conn, "PRAGMA freelist_count").await? > 0
            || file_bytes(&format!("{db_path}-wal")) > LARGE_LOG
    };
    let mut asked = None;
    'steps: while step < 4 {
        if step >= 2 && dirty && asked != Some(step) {
            asked = Some(step);
            let reclaimed = reclaim(db_path, left(deadline), &policy, &longest, true).await?;
            if reclaimed.how != Vacuumed::No {
                how = reclaimed.how;
            }
            if !reclaimed.settled {
                done.finished = false;
                blocked = reclaimed.limit;
                break;
            }
            blocked = reclaimed.limit;
            dirty = false;
        }
        let _guard = db::db_guard_for(db_path).await;
        let hold = Hold::start(&longest);
        let (_handle, conn) = db::open_conn(db_path).await?;
        if used_bytes(&conn).await? <= target {
            break;
        }
        match step {
            0 | 2 => {
                let last = scalar(&conn, "SELECT max(id) FROM materializations").await?;
                if from >= last {
                    (step, from) = (step + 1, 0);
                    continue 'steps;
                }
                let changed = row_batch(&conn, step, from, span).await?;
                dirty |= changed > 0;
                if step == 0 {
                    done.components += changed;
                } else {
                    done.rows += changed;
                }
                from = from.saturating_add(span);
                // A batch that held the lock too long is followed by a smaller one.
                if hold.elapsed() > policy.hold_target {
                    span = (span / 2).max(MIN_ID_SPAN);
                } else if hold.elapsed() < policy.hold_target / 4 {
                    span = (span * 2).min(ID_SPAN);
                }
            }
            1 => {
                let changed = db::runs::clear_old_outcomes(&conn, KEPT_OUTCOMES, RUN_BATCH)
                    .await
                    .map_err(failed("clear outcomes"))?;
                done.outcomes += changed;
                dirty |= changed > 0;
                if changed == 0 {
                    (step, from) = (step + 1, 0);
                    continue 'steps;
                }
            }
            _ => {
                let changed = run_batch(&conn).await?;
                done.runs += changed;
                dirty |= changed > 0;
                if changed == 0 {
                    step += 1;
                    continue 'steps;
                }
            }
        }
        drop(hold);
        if out_of_time() {
            done.finished = false;
            done.out_of_time = true;
            break;
        }
    }
    save_resume(db_path, (!done.finished).then_some((step, from)));
    if dirty && done.finished {
        let reclaimed = reclaim(db_path, left(deadline), &policy, &longest, true).await?;
        if reclaimed.how != Vacuumed::No {
            how = reclaimed.how;
        }
        blocked = reclaimed.limit;
    } else if done.finished && file_bytes(&format!("{db_path}-wal")) > LARGE_LOG {
        // Nothing to rewrite, but a log a reader held back last time may be foldable now.
        let reclaimed = reclaim(db_path, left(deadline), &policy, &longest, false).await?;
        blocked = reclaimed.limit;
    }
    done.after_bytes = file_bytes(db_path);
    done.log_bytes = file_bytes(&format!("{db_path}-wal"));
    done.vacuum = how.name().to_string();
    if done.after_bytes > allowance && !done.out_of_time {
        done.limit = Some(match blocked {
            Some(limit) => limit,
            None => classify(db_path, allowance, &longest).await?,
        });
    }
    done.longest_hold_ms = longest.load(Ordering::Relaxed).div_ceil(1000);
    Ok(Some(done))
}

/// Whether something that held the last pass back has changed, so another is worth its time
/// before its backoff ends: a run that protected what was left has ended, or enough runs have
/// finished since to make older ones eligible past the retention constants. One short hold of
/// the lock, a few cheap queries.
pub async fn eligibility_changed(db_path: &str, last: &Limit) -> bool {
    let _guard = db::db_guard_for(db_path).await;
    let Ok((_handle, conn)) = db::open_conn(db_path).await else {
        return false;
    };
    match last {
        Limit::LiveRun { .. } => db::runs_alive_here(&conn, db_path).await.is_empty(),
        Limit::NoEligibleHistory { runs, .. } => scalar(&conn, "SELECT count(*) FROM runs")
            .await
            .is_ok_and(|now| {
                now > KEPT_RUNS && now as u64 >= runs.saturating_add(RUN_BATCH as u64)
            }),
        Limit::WorkingSet { .. }
        | Limit::CheckpointBlocked { .. }
        | Limit::VacuumDeferred { .. } => false,
    }
}

/// A log past this is worth a checkpoint of its own.
const LARGE_LOG: u64 = 16 << 20;

/// Whether the file is over its allowance because of what no trim removes (the working set), or
/// because of run history the retention constants keep. The size of that history is estimated
/// from the row counts and a sample of rows: a count of the runs and log lines and the average
/// size of a few hundred of each.
async fn classify(db_path: &str, allowance: u64, longest: &AtomicU64) -> Result<Limit, BarcaError> {
    let _guard = db::db_guard_for(db_path).await;
    let _hold = Hold::start(longest);
    let (_handle, conn) = db::open_conn(db_path).await?;
    let used = used_bytes(&conn).await?;
    let runs = scalar(&conn, "SELECT count(*) FROM runs").await?.max(0) as u64;
    let run_row = scalar(
        &conn,
        "SELECT avg(coalesce(length(outcome_json), 0) + length(command) + length(files) + 64) \
         FROM (SELECT outcome_json, command, files FROM runs ORDER BY id DESC LIMIT 200)",
    )
    .await?
    .max(0) as u64;
    let lines = scalar(&conn, "SELECT count(*) FROM logs").await?.max(0) as u64;
    let line_row = scalar(
        &conn,
        "SELECT avg(length(line) + length(run_id) + length(node_id) + 16) \
         FROM (SELECT line, run_id, node_id FROM logs ORDER BY rowid DESC LIMIT 200)",
    )
    .await?
    .max(0) as u64;
    let history = runs
        .saturating_mul(run_row)
        .saturating_add(lines.saturating_mul(line_row));
    let working_set = used.saturating_sub(history);
    Ok(if working_set > allowance {
        Limit::WorkingSet {
            working_set_bytes: working_set,
        }
    } else {
        Limit::NoEligibleHistory {
            history_bytes: history,
            runs,
        }
    })
}

/// Give freed space back to the file, within the bounds the pass keeps to. `rewrite`: pages may
/// be free or half empty, not only a log waiting for a checkpoint.
async fn reclaim(
    db_path: &str,
    budget: Option<Duration>,
    policy: &Policy,
    longest: &AtomicU64,
    rewrite: bool,
) -> Result<Reclaim, BarcaError> {
    let (mode, alive, used) = {
        let _guard = db::db_guard_for(db_path).await;
        let _hold = Hold::start(longest);
        let (_handle, conn) = db::open_conn(db_path).await?;
        (
            scalar(&conn, "PRAGMA auto_vacuum").await?,
            db::runs_alive_here(&conn, db_path).await,
            used_bytes(&conn).await?,
        )
    };
    let alive = alive.into_iter().next();
    if !rewrite {
        let busy = checkpoint(db_path, longest).await?;
        return Ok(Reclaim {
            how: Vacuumed::No,
            settled: true,
            limit: busy.then(|| Limit::CheckpointBlocked {
                log_bytes: file_bytes(&format!("{db_path}-wal")),
            }),
        });
    }
    if mode == 2 {
        // Freed pages go back a chunk at a time, each under its own hold of the lock; a run
        // alive does not matter to that, only to deleting history and to the definitions.
        let complete = incremental(db_path, budget, policy, longest, alive.is_none()).await?;
        let busy = complete && checkpoint(db_path, longest).await?;
        let limit = match (&alive, busy) {
            (Some(run), _) => Some(Limit::LiveRun {
                run_id: run.clone(),
            }),
            (None, true) => Some(Limit::CheckpointBlocked {
                log_bytes: file_bytes(&format!("{db_path}-wal")),
            }),
            _ => None,
        };
        return Ok(Reclaim {
            how: Vacuumed::Incremental,
            settled: complete && alive.is_none(),
            limit,
        });
    }
    if let Some(run) = alive {
        return Ok(Reclaim {
            how: Vacuumed::No,
            settled: false,
            limit: Some(Limit::LiveRun { run_id: run }),
        });
    }
    let estimated = Duration::from_millis(
        used.saturating_mul(1000)
            .div_ceil(policy.vacuum_bytes_per_sec.max(1)),
    );
    if let Some(budget) = budget
        && estimated > budget
    {
        return Ok(Reclaim {
            how: Vacuumed::No,
            settled: false,
            limit: Some(Limit::VacuumDeferred {
                estimated_ms: estimated.as_millis() as u64,
                budget_ms: budget.as_millis() as u64,
            }),
        });
    }
    // A `VACUUM` appends a copy of the whole store to the log, and only a checkpoint that
    // completes folds it back: with a reader holding the log it never does, and every pass
    // would add another copy (a 1.3 GB store reached a 5.9 GB log, each pass holding the lock
    // for 13 to 64 s). So it starts only when a checkpoint can finish now, and not onto a log
    // that is already large.
    if checkpoint(db_path, longest).await? {
        return Ok(Reclaim {
            how: Vacuumed::No,
            settled: false,
            limit: Some(Limit::CheckpointBlocked {
                log_bytes: file_bytes(&format!("{db_path}-wal")),
            }),
        });
    }
    let log_bytes = file_bytes(&format!("{db_path}-wal"));
    if log_bytes > LARGE_LOG {
        return Ok(Reclaim {
            how: Vacuumed::No,
            settled: false,
            limit: Some(Limit::CheckpointBlocked { log_bytes }),
        });
    }
    // The one piece of a trim that cannot be cut into shorter holds of the lock, so it is only
    // run when it fits.
    let _guard = db::db_guard_for(db_path).await;
    let hold = Hold::start(longest);
    let (_handle, conn) = db::open_conn(db_path).await?;
    // Re-checked under this hold: a run that began since would wait for the rewrite.
    if let Some(run) = db::runs_alive_here(&conn, db_path).await.into_iter().next() {
        return Ok(Reclaim {
            how: Vacuumed::No,
            settled: false,
            limit: Some(Limit::LiveRun { run_id: run }),
        });
    }
    drop_unnamed_definitions(&conn).await?;
    conn.execute("VACUUM", ()).await.map_err(failed("VACUUM"))?;
    // A reader in the way leaves the log to the next checkpoint.
    let busy = db::checkpoint_nowait(&conn).await.unwrap_or(true);
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        barca_model::errln!(
            "[timing] metadata vacuum: {} bytes in {:.3} ms",
            file_bytes(db_path),
            hold.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(Reclaim {
        how: Vacuumed::Full,
        settled: true,
        limit: busy.then(|| Limit::CheckpointBlocked {
            log_bytes: file_bytes(&format!("{db_path}-wal")),
        }),
    })
}

/// A checkpoint under its own hold of the lock: true when a reader kept it from finishing.
async fn checkpoint(db_path: &str, longest: &AtomicU64) -> Result<bool, BarcaError> {
    let _guard = db::db_guard_for(db_path).await;
    let _hold = Hold::start(longest);
    let (_handle, conn) = db::open_conn(db_path).await?;
    Ok(db::checkpoint_nowait(&conn).await.unwrap_or(true))
}

/// `PRAGMA incremental_vacuum` in chunks sized to keep each hold of the lock within
/// [`Policy::hold_target`], the lock let go between them. False when `budget` ran out first.
async fn incremental(
    db_path: &str,
    budget: Option<Duration>,
    policy: &Policy,
    longest: &AtomicU64,
    idle: bool,
) -> Result<bool, BarcaError> {
    let deadline = budget.map(|b| Instant::now() + b);
    let mut pages = FIRST_CHUNK_PAGES;
    if idle {
        let _guard = db::db_guard_for(db_path).await;
        let _hold = Hold::start(longest);
        let (_handle, conn) = db::open_conn(db_path).await?;
        drop_unnamed_definitions(&conn).await?;
    }
    loop {
        let _guard = db::db_guard_for(db_path).await;
        let hold = Hold::start(longest);
        let (_handle, conn) = db::open_conn(db_path).await?;
        if scalar(&conn, "PRAGMA freelist_count").await? == 0 {
            return Ok(true);
        }
        let mut rows = conn
            .query(&format!("PRAGMA incremental_vacuum({pages})"), ())
            .await
            .map_err(failed("incremental_vacuum"))?;
        while rows
            .next()
            .await
            .map_err(failed("incremental_vacuum"))?
            .is_some()
        {}
        drop(rows);
        let took = hold.elapsed();
        drop(hold);
        if took > policy.hold_target {
            pages = (pages / 2).max(16);
        } else if took < policy.hold_target / 4 {
            pages = pages.saturating_mul(2).min(1 << 16);
        }
        drop(_guard);
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(false);
        }
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exec(path: &str, sql: &str) {
        let (_handle, conn) = db::open_conn(path).await.unwrap();
        conn.execute(sql, ()).await.unwrap();
    }

    async fn count(path: &str, sql: &str) -> i64 {
        let (_handle, conn) = db::open_conn(path).await.unwrap();
        scalar(&conn, sql).await.unwrap()
    }

    fn rows_sql(from: u32, to: u32, node: &str, extra: &str) -> String {
        format!(
            "WITH RECURSIVE n(i) AS (SELECT {from} UNION ALL SELECT i + 1 FROM n WHERE i < {to}) \
             INSERT INTO materializations (node_id, run_hash, artifact_path, status, run_id, \
             components, evicted_at, bytes_changed) SELECT '{node}', 'h' || i, 'a/' || i, \
             'success', 'r', printf('%.*c', 30000, 'x'), {extra} FROM n"
        )
    }

    /// A database well over its allowance, with one run still `running` when `alive`.
    async fn bloated(path: &str, runs: u32) {
        db::init_db(path).await.unwrap();
        exec(path, &rows_sql(1, 2000, "p.py:old", "datetime('now'), 0")).await;
        exec(path, &rows_sql(2001, 2100, "p.py:live", "NULL, 0")).await;
        exec(
            path,
            &rows_sql(2101, 2101, "p.py:old", "datetime('now'), 1"),
        )
        .await;
        exec(path, &rows_sql(2102, 2102, "p.py:old", "NULL, 0")).await;
        exec(
            path,
            &format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {runs}) \
                 INSERT INTO runs (run_id, command, files, status, outcome_json) SELECT 'r' || i, \
                 'get', 'p.py', 'success', printf('%.*c', 2000, 'y') FROM n"
            ),
        )
        .await;
        exec(
            path,
            "INSERT INTO logs (run_id, node_id, seq, line) VALUES ('r1','n',0,'old')",
        )
        .await;
    }

    #[test]
    fn the_allowance_is_a_tenth_of_the_limit_up_to_a_gigabyte() {
        assert_eq!(allowance(Some(1_000_000_000)), 100_000_000);
        assert_eq!(allowance(Some(100_000_000_000)), LARGEST_ALLOWANCE);
        assert_eq!(allowance(None), LARGEST_ALLOWANCE);
    }

    #[tokio::test]
    async fn a_database_within_its_allowance_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        db::init_db(&path).await.unwrap();
        assert_eq!(trim(&path, u64::MAX, None).await.unwrap(), None);
    }

    /// Clearing what only explains history is enough here, so no history is deleted: page counts
    /// alone would have said otherwise, since a row that lost its text leaves its page half empty.
    #[tokio::test]
    async fn history_stays_when_clearing_components_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        let before = file_bytes(&path);
        let done = trim(&path, 8_000_000, None).await.unwrap().unwrap();
        assert!(done.finished);
        assert_eq!((done.rows, done.runs), (0, 0), "{done:?}");
        assert!(done.components >= 2000, "{done:?}");
        assert!(file_bytes(&path) < before / 4, "{done:?}");
        // Under its share: no limiting reason to give.
        assert!(
            done.after_bytes <= 8_000_000 && done.limit.is_none(),
            "{done:?}"
        );
        assert_eq!(
            count(&path, "SELECT count(*) FROM materializations").await,
            2102
        );
    }

    #[tokio::test]
    async fn past_that_it_removes_what_no_decision_reads_and_keeps_what_one_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        let done = trim(&path, 8192, None).await.unwrap().unwrap();
        assert!(done.finished, "{done:?}");
        assert!(done.after_bytes < done.before_bytes);
        // An evicted row older than its node's latest success goes; a file's row, the latest
        // success and the newest byte change stay.
        for (sql, want) in [
            (
                "SELECT count(*) FROM materializations WHERE node_id = 'p.py:old' AND id < 2101",
                0,
            ),
            (
                "SELECT count(*) FROM materializations WHERE node_id = 'p.py:live'",
                100,
            ),
            (
                "SELECT count(*) FROM materializations WHERE bytes_changed = 1",
                1,
            ),
            (
                "SELECT count(*) FROM materializations WHERE run_hash = 'h2102'",
                1,
            ),
            ("SELECT count(*) FROM runs", KEPT_RUNS),
            (
                "SELECT count(*) FROM runs WHERE outcome_json IS NOT NULL",
                KEPT_OUTCOMES,
            ),
            ("SELECT count(*) FROM runs WHERE run_id = 'r1500'", 1),
            ("SELECT count(*) FROM logs", 0),
        ] {
            assert_eq!(count(&path, sql).await, want, "{sql}");
        }
    }

    /// `VACUUM` cannot run beside a live run, and deleting history waits for it.
    #[tokio::test]
    async fn with_a_run_alive_no_history_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        db::create_run(&path, "alive", "get", "p.py", None, Some(1))
            .await
            .unwrap();
        let done = trim(&path, 8192, None).await.unwrap().unwrap();
        assert!(!done.finished, "{done:?}");
        assert_eq!((done.rows, done.runs), (0, 0), "{done:?}");
        assert!(done.components > 0);
        assert_eq!(done.limit.as_ref().and_then(Limit::run_id), Some("alive"));
        exec(
            &path,
            "UPDATE runs SET status = 'success' WHERE run_id = 'alive'",
        )
        .await;
        let done = trim(&path, 8192, None).await.unwrap().unwrap();
        assert!(done.finished, "{done:?}");
        assert!(done.rows > 0, "{done:?}");
    }

    /// A small database over a small share, which no trim can shrink: `nodes` nodes with one
    /// row each (the latest success of its node, its file on disk).
    async fn working_set(path: &str, nodes: u32) {
        db::init_db(path).await.unwrap();
        exec(
            path,
            &format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {nodes}) \
                 INSERT INTO materializations (node_id, run_hash, artifact_path, status, run_id, \
                 components, bytes_changed) SELECT 'p.py:n' || i, 'h' || i, 'a/' || i, \
                 'success', 'r', printf('%.*c', 2000, 'x'), 0 FROM n"
            ),
        )
        .await;
    }

    async fn history(path: &str, runs: u32, lines_per_run: u32) {
        exec(
            path,
            &format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {runs}) \
                 INSERT INTO runs (run_id, command, files, status, outcome_json) SELECT 'r' || i, \
                 'get', 'p.py', 'success', NULL FROM n"
            ),
        )
        .await;
        exec(
            path,
            &format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n \
                 WHERE i < {}) INSERT INTO logs (run_id, node_id, seq, line) SELECT 'r' || (i % \
                 {runs} + 1), 'p.py:n1', i, printf('%.*c', 200, 'z') FROM n",
                runs * lines_per_run
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn a_working_set_over_the_share_is_named_as_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        working_set(&path, 3000).await;
        let before = file_bytes(&path);
        let done = trim(&path, 1_000_000, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        // Nothing here is history, so nothing goes, and the pass says why.
        assert_eq!(
            (done.components, done.outcomes, done.rows, done.runs),
            (0, 0, 0, 0)
        );
        assert!(done.after_bytes > 1_000_000 && done.after_bytes <= before);
        let Some(Limit::WorkingSet { working_set_bytes }) = done.limit else {
            panic!("{done:?}");
        };
        assert!(working_set_bytes > 1_000_000, "{done:?}");
        assert!(!done.progressed());
        assert_eq!(
            count(&path, "SELECT count(*) FROM materializations").await,
            3000
        );
    }

    #[tokio::test]
    async fn history_the_retention_constants_keep_is_named_as_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        working_set(&path, 50).await;
        // 900 runs: fewer than KEPT_RUNS, so none is eligible, and their logs are most of the file.
        history(&path, 900, 40).await;
        let done = trim(&path, 1_000_000, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.runs, 0, "{done:?}");
        let Some(Limit::NoEligibleHistory { history_bytes, .. }) = done.limit else {
            panic!("{done:?}");
        };
        assert!(history_bytes > 1_000_000, "{done:?}");
        assert_eq!(count(&path, "SELECT count(*) FROM runs").await, 900);
    }

    #[tokio::test]
    async fn a_live_run_is_named_as_the_reason_with_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        db::create_run(&path, "busy-run", "get", "p.py", None, Some(1))
            .await
            .unwrap();
        let done = trim(&path, 8_000_000, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        let limit = done.limit.clone().expect("a reason");
        assert_eq!(limit.run_id(), Some("busy-run"), "{done:?}");
        assert!(limit.describe().contains("busy-run"));
        assert!(limit.describe().contains("VACUUM deferred: server busy"));
    }

    /// A full `VACUUM` that cannot finish within the budget does not start.
    #[tokio::test]
    async fn a_vacuum_over_the_budget_is_deferred_and_holds_no_lock_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        let slow = Policy {
            vacuum_bytes_per_sec: 1000,
            ..Policy::default()
        };
        let done = trim_with(&path, 8_000_000, Some(Duration::from_secs(5)), slow)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.vacuum, "none", "{done:?}");
        let Some(Limit::VacuumDeferred {
            estimated_ms,
            budget_ms,
        }) = done.limit
        else {
            panic!("{done:?}");
        };
        assert!(estimated_ms > budget_ms, "{done:?}");
        // The whole pass held the lock for much less than the VACUUM would have.
        assert!(done.longest_hold_ms < 1000, "{done:?}");
        // With no budget (`barca gc`) it runs, and the file shrinks.
        let done = trim_with(&path, 8_000_000, None, slow)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.vacuum, "full", "{done:?}");
        assert!(
            done.limit.is_none() && done.after_bytes <= 8_000_000,
            "{done:?}"
        );
    }

    /// A database created with `auto_vacuum=INCREMENTAL` gives pages back in bounded chunks.
    #[tokio::test]
    async fn an_incremental_database_gives_pages_back_in_bounded_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        exec(&path, "PRAGMA auto_vacuum = INCREMENTAL").await;
        exec(&path, "VACUUM").await;
        assert_eq!(count(&path, "PRAGMA auto_vacuum").await, 2);
        let before = file_bytes(&path);
        let tiny = Policy {
            hold_target: Duration::from_millis(20),
            ..Policy::default()
        };
        let done = trim_with(&path, 8_000_000, Some(Duration::from_secs(5)), tiny)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.vacuum, "incremental", "{done:?}");
        assert!(done.after_bytes < before / 4, "{done:?}");
        assert!(done.limit.is_none(), "{done:?}");
        eprintln!("incremental: longest lock hold {} ms", done.longest_hold_ms);
        assert!(done.longest_hold_ms < 1000, "{done:?}");
    }

    /// Another task asking for the lock during a trim waits for one hold at a time, never for
    /// the pass: the longest wait seen from outside is within the budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_hold_of_the_lock_outlasts_the_budget_seen_from_outside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let prober = {
            let stop = stop.clone();
            tokio::spawn(async move {
                let mut worst = Duration::ZERO;
                while !stop.load(Ordering::Relaxed) {
                    let asked = Instant::now();
                    drop(db::db_guard().await);
                    worst = worst.max(asked.elapsed());
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                worst
            })
        };
        let budget = Duration::from_secs(5);
        let started = Instant::now();
        let done = trim(&path, 8192, Some(budget)).await.unwrap().unwrap();
        stop.store(true, Ordering::Relaxed);
        let worst = prober.await.unwrap();
        eprintln!(
            "pass {:?}, longest hold {} ms, longest wait seen by another task {:?}",
            started.elapsed(),
            done.longest_hold_ms,
            worst
        );
        assert!(done.longest_hold_ms < budget.as_millis() as u64, "{done:?}");
        assert!(worst < budget, "waited {worst:?}");
        assert!(
            worst < started.elapsed(),
            "the lock was held for the whole pass"
        );
    }

    /// A reader that holds the log keeps `VACUUM` from reaching the file: said so, no wait.
    #[tokio::test]
    async fn a_reader_holding_the_log_is_named_and_not_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        let reader = libsql::Builder::new_local(&path).build().await.unwrap();
        let reader_conn = reader.connect().unwrap();
        reader_conn.execute("BEGIN", ()).await.unwrap();
        let mut rows = reader_conn
            .query("SELECT count(*) FROM materializations", ())
            .await
            .unwrap();
        rows.next().await.unwrap();
        let started = Instant::now();
        let done = trim(&path, 8_000_000, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        eprintln!(
            "reader-blocked pass: {:?}, longest hold {} ms, {done:?}",
            started.elapsed(),
            done.longest_hold_ms
        );
        assert!(
            matches!(done.limit, Some(Limit::CheckpointBlocked { .. })),
            "{done:?}"
        );
        assert!(done.longest_hold_ms < 2000, "{done:?}");
        drop(rows);
        reader_conn.execute("ROLLBACK", ()).await.unwrap();
    }

    /// What a cache decision reads, and what a pin or an upload names, is never removed.
    #[tokio::test]
    async fn current_results_pins_and_uploads_are_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        // An old, evicted row of `p.py:old` that a pin names, and one an upload names: both
        // would go as superseded history without them.
        exec(
            &path,
            "UPDATE materializations SET evicted_at = datetime('now') WHERE run_hash = 'h10'",
        )
        .await;
        exec(
            &path,
            "INSERT INTO asset_pins (node_id, run_hash, pinned_at) VALUES ('p.py:old', 'h10', 'x')",
        )
        .await;
        exec(
            &path,
            "INSERT INTO asset_uploads (node_id, run_hash, uploaded_at) \
             VALUES ('p.py:old', 'h11', 'x')",
        )
        .await;
        let done = trim(&path, 8192, None).await.unwrap().unwrap();
        assert!(done.finished && done.rows > 0, "{done:?}");
        for (sql, want) in [
            (
                "SELECT count(*) FROM materializations WHERE run_hash IN ('h10', 'h11') \
                 AND components IS NOT NULL",
                2,
            ),
            // Every result with its file on disk, and the latest success of each node.
            (
                "SELECT count(*) FROM materializations WHERE node_id = 'p.py:live'",
                100,
            ),
            (
                "SELECT count(*) FROM materializations WHERE run_hash = 'h2102'",
                1,
            ),
        ] {
            assert_eq!(count(&path, sql).await, want, "{sql}");
        }
    }
}
