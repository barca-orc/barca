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
//! Each batch takes the database lock for itself, so a run's writes go on between batches.
//! SQLite reuses freed pages but does not give them back, and a page a row left half empty still
//! counts as used, so only `VACUUM` says what the database really takes. Steps 3 and 4 delete
//! history, so under `barca gc` each waits for one and runs only if that was not enough: with a
//! run alive, they wait. A pass with a budget does not wait (below).
//!
//! **`VACUUM` is for `barca gc` only.** It rewrites the live data with the lock held and cannot
//! be cut short, so a pass with a time budget (the check after a command or a run, `serve`'s
//! minute checkpoint) never runs one, and with it no checkpoint under the lock. Such a pass
//! measures the database by its used size, the pages that hold data (`page_count` less
//! `freelist_count`, times `page_size`; the log is left out, only a checkpoint shrinks it), and
//! orders 3 and 4 run without waiting for a `VACUUM`: SQLite reuses the pages their deletes free,
//! so the used size comes under the share while the file keeps its length until `barca gc`
//! ([`Trimmed::vacuum_deferred`]). A pass reports the longest hold of the lock it took
//! ([`Trimmed::longest_hold_ms`]).

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
/// Row ids one batch looks at, and runs one batch changes.
const ID_SPAN: i64 = 2000;
const RUN_BATCH: i64 = 200;
/// How long one batch of step 4 deletes for before it lets the lock go: a run's logs can be
/// thousands of lines, and 200 runs of 100 lines each held the lock for 0.4 to 1.1 s.
const RUN_BATCH_TIME: Duration = Duration::from_millis(100);

/// What the metadata database may use of `limit` (`None`: `[cache] max_size` is off).
pub fn allowance(limit: Option<u64>) -> u64 {
    limit.map_or(LARGEST_ALLOWANCE, |limit| {
        (limit / 100 * ALLOWANCE_PERCENT).min(LARGEST_ALLOWANCE)
    })
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
    /// Every step was tried: what is left is not removable now.
    pub finished: bool,
    /// Stopped because time ran out, not to wait for a moment with no run alive.
    pub out_of_time: bool,
    /// The bytes the pages that hold data take (the file less its free pages), before and after
    /// the pass. A pass with a time budget works to this size, not the file's.
    #[serde(default)]
    pub used_before_bytes: u64,
    #[serde(default)]
    pub used_after_bytes: u64,
    /// A run was running or queued (this one, by id) so the tiers that delete history (old results,
    /// old runs and logs) did not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_run: Option<String>,
    /// The pass could not get a lock within [`LOCK_WAIT`] and stopped.
    #[serde(default)]
    pub busy: bool,
    /// The database lacks an index an online pass needs (an older format, which `barca gc` or
    /// the next full open migrates), or the pass failed.
    #[serde(default)]
    pub needs_gc: bool,
    /// A full `VACUUM` was wanted and not run: the pass had a time budget (only `barca gc` runs
    /// one) or a run was alive. The file keeps its size until `barca gc`.
    #[serde(default)]
    pub vacuum_deferred: bool,
    /// A full `VACUUM` ran.
    #[serde(default)]
    pub vacuumed: bool,
    /// The longest time one hold of the database lock lasted in the pass, in milliseconds.
    #[serde(default)]
    pub longest_hold_ms: u64,
}

impl Trimmed {
    pub fn removed(&self) -> bool {
        self.components + self.outcomes + self.rows + self.runs > 0
    }

    /// The pass freed something: it cleared records or the file shrank.
    pub fn progressed(&self) -> bool {
        self.removed() || self.after_bytes < self.before_bytes
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
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let micros = self.since.elapsed().as_micros() as u64;
        self.longest.fetch_max(micros, Ordering::Relaxed);
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

async fn ids_in(conn: &Connection, filter: &str, from: i64) -> Result<Vec<i64>, BarcaError> {
    let sql = format!(
        "SELECT m.id FROM materializations m WHERE m.id > {from} AND m.id <= {} AND {filter}",
        from.saturating_add(ID_SPAN)
    );
    let mut rows = conn.query(&sql, ()).await.map_err(failed("find rows"))?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed("find rows"))? {
        ids.push(row.get::<i64>(0).map_err(failed("find rows"))?);
    }
    Ok(ids)
}

/// One batch of step 1 or 3, over the ids after `from`. The number of rows changed.
async fn row_batch(conn: &Connection, step: u8, from: i64) -> Result<u64, BarcaError> {
    if step == 0 {
        let filter = format!(
            "m.components IS NOT NULL AND (octet_length(m.components) > {OVERSIZED_COMPONENTS} \
             OR m.id < {LATEST_SUCCESS})"
        );
        let ids = ids_in(conn, &filter, from).await?;
        if ids.is_empty() {
            return Ok(0);
        }
        return db::materializations::clear_components(conn, &ids)
            .await
            .map_err(failed("clear components"));
    }
    // The newest byte change of a node stays: `barca status` shows it.
    let filter = format!(
        "m.id < {LATEST_SUCCESS} \
         AND NOT (m.status = 'success' AND m.artifact_path IS NOT NULL AND m.evicted_at IS NULL) \
         AND NOT (m.bytes_changed = 1 AND m.id = (SELECT max(c.id) FROM materializations c \
             INDEXED BY idx_mat_changed WHERE c.node_id = m.node_id AND c.bytes_changed = 1))"
    );
    let ids = ids_in(conn, &filter, from).await?;
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

/// Bring `db_path` within `allowance`, for at most `time` (`None`: until it is done, as `barca
/// gc` does, and the only case that runs `VACUUM`). `None` when its file is within the allowance
/// already.
pub async fn trim(
    db_path: &str,
    allowance: u64,
    time: Option<Duration>,
) -> Result<Option<Trimmed>, BarcaError> {
    trim_as(db_path, allowance, time, time.is_some()).await
}

/// [`trim`], with `online` saying whether it runs inside `barca serve`: then it never runs
/// `VACUUM`, deletes no history while a run is running or queued, and waits at most
/// [`LOCK_WAIT`] for a lock. Otherwise (`barca gc`, a CLI command) it is as it was: it waits for
/// the locks and runs a `VACUUM` when no run is alive, whatever its budget.
pub async fn trim_as(
    db_path: &str,
    allowance: u64,
    time: Option<Duration>,
    online: bool,
) -> Result<Option<Trimmed>, BarcaError> {
    // The budget starts here: before the schema check and every wait for a lock.
    let deadline = time.map(|time| Instant::now() + time);
    let before_bytes = file_bytes(db_path);
    if before_bytes <= allowance {
        return Ok(None);
    }
    let longest = AtomicU64::new(0);
    let vacuum_allowed = !online;
    let busy = |used: u64| Trimmed {
        before_bytes,
        after_bytes: before_bytes,
        used_before_bytes: used,
        used_after_bytes: used,
        finished: false,
        busy: true,
        longest_hold_ms: longest.load(Ordering::Relaxed).div_ceil(1000),
        ..Trimmed::default()
    };
    if vacuum_allowed {
        db::init_db(db_path).await?;
    }
    let Some((used_before, ready)) = used_now(db_path, vacuum_allowed, &longest).await? else {
        return Ok(Some(busy(0)));
    };
    if !ready {
        // Not an error of every command: the pass is skipped, remembered, and left to `barca gc`.
        return Ok(Some(Trimmed {
            needs_gc: true,
            busy: false,
            ..busy(used_before)
        }));
    }
    if !vacuum_allowed && used_before <= allowance {
        // The data fits its share; only the file is long, and only `barca gc` shortens it.
        return Ok(Some(Trimmed {
            before_bytes,
            after_bytes: before_bytes,
            used_before_bytes: used_before,
            used_after_bytes: used_before,
            finished: true,
            vacuum_deferred: true,
            longest_hold_ms: longest.load(Ordering::Relaxed).div_ceil(1000),
            ..Trimmed::default()
        }));
    }
    let target = allowance / 100 * LOW_WATERMARK_PERCENT;
    let out_of_time = || deadline.is_some_and(|deadline| Instant::now() >= deadline);
    let mut done = Trimmed {
        before_bytes,
        used_before_bytes: used_before,
        finished: true,
        ..Trimmed::default()
    };
    let (mut step, mut from) = resume_at(db_path);
    // Whether the file may hold space only `VACUUM` gives back, and the step that last asked.
    let (mut dirty, mut asked) = (true, None);
    'steps: while step < 4 {
        if vacuum_allowed && step >= 2 && dirty && asked != Some(step) {
            asked = Some(step);
            if !vacuum(db_path, vacuum_allowed, &longest).await? {
                done.finished = false;
                done.vacuum_deferred = true;
                break;
            }
            done.vacuumed = true;
            dirty = false;
        }
        let Some(_guard) = guard(db_path, vacuum_allowed).await else {
            done.busy = true;
            done.finished = false;
            break;
        };
        let _hold = Hold::start(&longest);
        let Some((_handle, conn)) = connect(db_path, vacuum_allowed).await? else {
            done.busy = true;
            done.finished = false;
            break;
        };
        if used_bytes(&conn).await? <= target {
            break;
        }
        // The tiers that delete history (old results, old runs and their logs) wait for a
        // moment with no run running or queued, of any owner: a pass with a budget cannot tell a
        // run that is gone from one that is only not ours, and a row a live run or a pin still
        // reads is not told from one nothing reads. `barca gc` makes its own check.
        if !vacuum_allowed
            && step >= 2
            && let Some(run) = live_run(&conn).await
        {
            done.live_run = Some(run);
            done.finished = false;
            break;
        }
        match step {
            0 | 2 => {
                let last = scalar(&conn, "SELECT max(id) FROM materializations").await?;
                if from >= last {
                    (step, from) = (step + 1, 0);
                    continue 'steps;
                }
                let changed = row_batch(&conn, step, from).await?;
                dirty |= changed > 0;
                if step == 0 {
                    done.components += changed;
                } else {
                    done.rows += changed;
                }
                from = from.saturating_add(ID_SPAN);
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
        if out_of_time() {
            done.finished = false;
            done.out_of_time = true;
            break;
        }
    }
    // A pass the live-run guard stopped starts over at step 0: a server that is never idle
    // would otherwise never clear component records or run reports again.
    save_resume(
        db_path,
        (!done.finished && done.live_run.is_none()).then_some((step, from)),
    );
    if dirty && done.finished {
        if vacuum(db_path, vacuum_allowed, &longest).await? {
            done.vacuumed = true;
        } else {
            done.vacuum_deferred = true;
        }
    }
    done.after_bytes = file_bytes(db_path);
    done.used_after_bytes = used_now(db_path, vacuum_allowed, &longest)
        .await?
        .map_or_else(
            || {
                done.busy = true;
                used_before
            },
            |(used, _)| used,
        );
    done.longest_hold_ms = longest.load(Ordering::Relaxed).div_ceil(1000);
    Ok(Some(done))
}

/// How long a pass with a time budget waits for the lock of this process or of another one: a
/// wait that is longer is a pass skipped (`busy`), because the wait is spent with the others
/// waiting too.
const LOCK_WAIT: Duration = Duration::from_millis(250);

/// The lock of this process: waited for until it is free for `barca gc`, for [`LOCK_WAIT`] for a
/// pass with a budget (`None`: it was not).
async fn guard(
    db_path: &str,
    wait_for_ever: bool,
) -> Option<Option<tokio::sync::MutexGuard<'static, ()>>> {
    if wait_for_ever {
        return Some(db::db_guard_for(db_path).await);
    }
    tokio::time::timeout(LOCK_WAIT, db::db_guard_for(db_path))
        .await
        .ok()
}

/// A connection under the cross-process lock, waited for as [`guard`] says.
async fn connect(
    db_path: &str,
    wait_for_ever: bool,
) -> Result<Option<(db::DbHandle, Connection)>, BarcaError> {
    if wait_for_ever {
        return db::open_conn(db_path).await.map(Some);
    }
    db::open_conn_within(db_path, LOCK_WAIT).await
}

/// The used size of the database ([`used_bytes`]), and whether it has everything this release adds
/// (the indexes a pass reads through with `INDEXED BY` among them), in one hold of the lock; `None` when a pass with a budget could not
/// get the lock in time.
async fn used_now(
    db_path: &str,
    wait_for_ever: bool,
    longest: &AtomicU64,
) -> Result<Option<(u64, bool)>, BarcaError> {
    let Some(_guard) = guard(db_path, wait_for_ever).await else {
        return Ok(None);
    };
    let _hold = Hold::start(longest);
    let Some((_handle, conn)) = connect(db_path, wait_for_ever).await? else {
        return Ok(None);
    };
    // Every column, table and index this release adds to a version-1 database (which
    // `init_db`, left to `barca gc` here, would add).
    let ready = !crate::db::schema::ddl::additions_missing(&conn).await?;
    Ok(Some((used_bytes(&conn).await?, ready)))
}

/// A run that is running or in the queue, of any owner: the `running` rows are not judged by
/// whether their process exists. A query that fails, or a row that cannot be read, counts as a
/// run (`unknown`): history is deleted only when it is known that none is.
async fn live_run(conn: &Connection) -> Option<String> {
    for sql in [
        "SELECT run_id FROM runs WHERE status = 'running' LIMIT 1",
        "SELECT run_id FROM run_queue LIMIT 1",
    ] {
        let Ok(mut rows) = conn.query(sql, ()).await else {
            return Some("unknown".to_string());
        };
        match rows.next().await {
            Ok(Some(row)) => {
                return Some(
                    row.get::<String>(0)
                        .unwrap_or_else(|_| "unknown".to_string()),
                );
            }
            Ok(None) => {}
            Err(_) => return Some("unknown".to_string()),
        }
    }
    None
}

/// `VACUUM`, unless `allowed` is false (a pass with a time budget) or a run is alive: it writes
/// the live data again with the lock held, so a run would wait for it. False when it did not run.
async fn vacuum(db_path: &str, allowed: bool, longest: &AtomicU64) -> Result<bool, BarcaError> {
    if !allowed {
        return Ok(false);
    }
    let _guard = db::db_guard_for(db_path).await;
    let _hold = Hold::start(longest);
    let (_handle, conn) = db::open_conn(db_path).await?;
    if !db::runs_alive_here(&conn, db_path).await.is_empty() {
        return Ok(false);
    }
    drop_unnamed_definitions(&conn).await?;
    let started = Instant::now();
    conn.execute("VACUUM", ()).await.map_err(failed("VACUUM"))?;
    // The one piece of a trim that cannot be cut into shorter holds of the lock.
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        barca_model::errln!(
            "[timing] metadata vacuum: {} bytes in {:.3} ms",
            file_bytes(db_path),
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    // A reader in the way leaves the log to the next checkpoint.
    let _ = db::checkpoint_once(&conn).await;
    Ok(true)
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
        exec(
            &path,
            "UPDATE runs SET status = 'success' WHERE run_id = 'alive'",
        )
        .await;
        let done = trim(&path, 8192, None).await.unwrap().unwrap();
        assert!(done.finished, "{done:?}");
        assert!(done.rows > 0, "{done:?}");
    }

    /// A pass with a time budget never runs `VACUUM`: it clears records and says it deferred
    /// one. `barca gc` (no budget) runs it, and the file shrinks.
    #[tokio::test]
    async fn a_budgeted_pass_never_runs_vacuum_and_gc_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        let done = trim(&path, 8_000_000, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        assert!(done.vacuum_deferred && !done.vacuumed, "{done:?}");
        assert!(done.components >= 2000, "{done:?}");
        assert!(
            done.after_bytes >= done.before_bytes,
            "no VACUUM, no shrink: {done:?}"
        );
        assert!(done.progressed());
        let done = trim(&path, 8_000_000, None).await.unwrap().unwrap();
        assert!(done.vacuumed && !done.vacuum_deferred, "{done:?}");
        assert!(done.after_bytes <= 8_000_000, "{done:?}");
        eprintln!("gc pass: longest lock hold {} ms", done.longest_hold_ms);
        assert!(done.longest_hold_ms > 0);
    }

    /// A pass with a time budget works to the used size and deletes eligible history without a
    /// `VACUUM`: the pages its deletes free are reused, and the file keeps its length.
    #[tokio::test]
    async fn a_budgeted_pass_deletes_eligible_history_without_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        let done = trim(&path, 8192, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        assert!(
            done.finished && !done.vacuumed && done.vacuum_deferred,
            "{done:?}"
        );
        assert!(
            done.rows > 0 && done.runs > 0 && done.progressed(),
            "{done:?}"
        );
        assert!(done.used_after_bytes < done.used_before_bytes, "{done:?}");
        assert!(
            done.after_bytes >= done.before_bytes,
            "the file keeps its length: {done:?}"
        );
        assert_eq!(count(&path, "SELECT count(*) FROM runs").await, KEPT_RUNS);
        assert_eq!(
            count(
                &path,
                "SELECT count(*) FROM materializations WHERE node_id = 'p.py:live'"
            )
            .await,
            100
        );
    }

    /// Once the data fits, the file is long only until `barca gc`: the next budgeted pass changes
    /// nothing and says so.
    #[tokio::test]
    async fn a_file_longer_than_its_data_waits_for_gc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        trim(&path, 8_000_000, Some(Duration::from_secs(60)))
            .await
            .unwrap();
        let again = trim(&path, 8_000_000, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        assert!(
            again.used_after_bytes <= 8_000_000 && again.after_bytes > 8_000_000,
            "{again:?}"
        );
        assert!(
            again.vacuum_deferred && !again.vacuumed && !again.progressed(),
            "{again:?}"
        );
    }

    /// A `running` row of any owner (here one nothing can judge) keeps a pass with a budget from
    /// deleting history; the tiers that main ran beside a live run still run.
    #[tokio::test]
    async fn with_a_run_running_a_budgeted_pass_deletes_no_history_and_says_live_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        exec(
            &path,
            "INSERT INTO runs (run_id, command, files, status) \
             VALUES ('foreign', 'get', 'p.py', 'running')",
        )
        .await;
        let done = trim(&path, 8192, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.live_run.as_deref(), Some("foreign"), "{done:?}");
        assert_eq!((done.rows, done.runs), (0, 0), "{done:?}");
        assert!(done.components > 0, "components are not history: {done:?}");
        assert!(!done.vacuumed && !done.finished, "{done:?}");
        // The guard did not leave step 2 as the resume point: the next pass starts over.
        assert_eq!(resume_at(&path), (0, 0));
        for (sql, want) in [
            ("SELECT count(*) FROM runs", 1501),
            (
                "SELECT count(*) FROM materializations WHERE node_id = 'p.py:old'",
                2002,
            ),
        ] {
            assert_eq!(count(&path, sql).await, want, "{sql}");
        }
        // The run ends: the next pass deletes the history.
        exec(
            &path,
            "UPDATE runs SET status = 'success' WHERE run_id = 'foreign'",
        )
        .await;
        let done = trim(&path, 8192, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        assert!(
            done.live_run.is_none() && done.rows > 0 && done.runs > 0,
            "{done:?}"
        );
    }

    /// The cross-process lock held by another connection: a pass with a budget gives up within
    /// a fraction of a second, with the process lock held no longer than that.
    #[tokio::test]
    async fn a_budgeted_pass_that_cannot_get_the_lock_is_busy_within_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        let other = std::fs::OpenOptions::new()
            .write(true)
            .open(format!("{path}.lock"))
            .unwrap();
        other.lock().unwrap();
        let started = Instant::now();
        let done = trim(&path, 8192, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        let took = started.elapsed();
        eprintln!(
            "busy pass: {took:?}, longest hold {} ms",
            done.longest_hold_ms
        );
        assert!(done.busy && !done.progressed(), "{done:?}");
        assert!(took < Duration::from_secs(1), "{took:?}");
        // Another test may hold this process's lock for the whole wait (a hold of 0 here), so
        // only the bound is asserted.
        assert!(done.longest_hold_ms < 1000, "{done:?}");
        // Released: the same pass works.
        other.unlock().unwrap();
        let done = trim(&path, 8192, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        assert!(!done.busy && done.components > 0, "{done:?}");
    }

    /// A query that fails is not "no run alive": history stays.
    #[tokio::test]
    async fn a_failed_check_for_live_runs_counts_as_a_live_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 1500).await;
        exec(&path, "DROP TABLE run_queue").await;
        let done = trim(&path, 8192, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        // `run_queue` is also a table this release adds, so the pass may stop even earlier, as
        // `needs_gc`; either way no history is deleted.
        assert!(
            done.live_run.as_deref() == Some("unknown") || done.needs_gc,
            "{done:?}"
        );
        assert_eq!((done.rows, done.runs), (0, 0), "{done:?}");
    }

    /// A database without the indexes an online pass reads through is skipped and remembered, not
    /// an error of every command; `barca gc` (which opens it fully) migrates it.
    #[tokio::test]
    async fn an_older_database_without_its_indexes_needs_gc_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        exec(&path, "DROP INDEX idx_mat_node_latest").await;
        let done = trim(&path, 8192, Some(Duration::from_secs(60)))
            .await
            .unwrap()
            .unwrap();
        assert!(
            done.needs_gc && !done.busy && !done.progressed(),
            "{done:?}"
        );
        let done = trim(&path, 8192, None).await.unwrap().unwrap();
        assert!(!done.needs_gc && done.vacuumed, "{done:?}");
    }

    /// Outside `barca serve` a pass with a budget is as it was on main: it compacts when no run
    /// is alive, and then history the compaction makes unnecessary to delete stays.
    #[tokio::test]
    async fn outside_a_server_a_budgeted_pass_may_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        bloated(&path, 10).await;
        let done = trim_as(&path, 8_000_000, Some(Duration::from_secs(60)), false)
            .await
            .unwrap()
            .unwrap();
        assert!(done.vacuumed && !done.vacuum_deferred, "{done:?}");
        assert!(
            done.after_bytes <= 8_000_000 && (done.rows, done.runs) == (0, 0),
            "{done:?}"
        );
    }
}
