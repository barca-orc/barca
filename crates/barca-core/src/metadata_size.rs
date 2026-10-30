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
//! history, so each waits for one and runs only if that was not enough: with a run alive, they
//! wait for the next check.

use crate::{BarcaError, db};
use libsql::Connection;
use std::collections::HashMap;
use std::sync::Mutex;
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

/// What the metadata database may use of `limit` (`None`: `[cache] max_size` is off).
pub(crate) fn allowance(limit: Option<u64>) -> u64 {
    limit.map_or(LARGEST_ALLOWANCE, |limit| {
        (limit / 100 * ALLOWANCE_PERCENT).min(LARGEST_ALLOWANCE)
    })
}

/// What one [`trim`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Trimmed {
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
}

impl Trimmed {
    pub(crate) fn removed(&self) -> bool {
        self.components + self.outcomes + self.rows + self.runs > 0
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

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
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
        return conn
            .execute(
                &format!(
                    "UPDATE materializations SET components = NULL WHERE id IN ({})",
                    id_list(&ids)
                ),
                (),
            )
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
    conn.execute(
        &format!(
            "DELETE FROM materializations WHERE id IN ({})",
            id_list(&ids)
        ),
        (),
    )
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
    for run in &runs {
        conn.execute("DELETE FROM logs WHERE run_id = ?1", [run.as_str()])
            .await
            .map_err(failed("delete logs"))?;
        conn.execute("DELETE FROM runs WHERE run_id = ?1", [run.as_str()])
            .await
            .map_err(failed("delete runs"))?;
    }
    Ok(runs.len() as u64)
}

/// Definitions no remaining row names. Only with no run alive: a starting run writes its
/// definitions before its rows.
async fn drop_unnamed_definitions(conn: &Connection) -> Result<(), BarcaError> {
    conn.execute(
        "DELETE FROM definition_components WHERE definition_hash NOT IN (SELECT \
         json_extract(components, '$.\"@definition\"') FROM materializations \
         WHERE components IS NOT NULL AND json_valid(components) \
         AND json_extract(components, '$.\"@definition\"') IS NOT NULL)",
        (),
    )
    .await
    .map_err(failed("delete definitions"))?;
    Ok(())
}

/// Bring `db_path` within `allowance`, for at most `time` (`None`: until it is done); `VACUUM` is
/// not counted. `None` when its file is within the allowance already.
pub(crate) async fn trim(
    db_path: &str,
    allowance: u64,
    time: Option<Duration>,
) -> Result<Option<Trimmed>, BarcaError> {
    let before_bytes = file_bytes(db_path);
    if before_bytes <= allowance {
        return Ok(None);
    }
    db::init_db(db_path).await?;
    let target = allowance / 100 * LOW_WATERMARK_PERCENT;
    let deadline = time.map(|time| Instant::now() + time);
    let out_of_time = || deadline.is_some_and(|deadline| Instant::now() >= deadline);
    let mut done = Trimmed {
        before_bytes,
        finished: true,
        ..Trimmed::default()
    };
    let (mut step, mut from) = resume_at(db_path);
    // Whether the file may hold space only `VACUUM` gives back, and the step that last asked.
    let (mut dirty, mut asked) = (true, None);
    'steps: while step < 4 {
        if step >= 2 && dirty && asked != Some(step) {
            asked = Some(step);
            if !vacuum(db_path).await? {
                done.finished = false;
                break;
            }
            dirty = false;
        }
        let _guard = db::db_guard_for(db_path).await;
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
                let changed = conn
                    .execute(
                        &format!(
                            "UPDATE runs SET outcome_json = NULL WHERE id IN (SELECT id FROM runs \
                             WHERE outcome_json IS NOT NULL AND id < (SELECT id FROM runs ORDER BY \
                             id DESC LIMIT 1 OFFSET {}) ORDER BY id LIMIT {RUN_BATCH})",
                            KEPT_OUTCOMES - 1
                        ),
                        (),
                    )
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
    save_resume(db_path, (!done.finished).then_some((step, from)));
    if dirty && done.finished {
        vacuum(db_path).await?;
    }
    done.after_bytes = file_bytes(db_path);
    Ok(Some(done))
}

/// `VACUUM`, unless a run is alive: it writes the live data again with the lock held, so a run
/// would wait for it. False when a run is alive.
async fn vacuum(db_path: &str) -> Result<bool, BarcaError> {
    let _guard = db::db_guard_for(db_path).await;
    let (_handle, conn) = db::open_conn(db_path).await?;
    if !db::runs_alive_here(&conn, db_path).await.is_empty() {
        return Ok(false);
    }
    drop_unnamed_definitions(&conn).await?;
    conn.execute("VACUUM", ()).await.map_err(failed("VACUUM"))?;
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
}
