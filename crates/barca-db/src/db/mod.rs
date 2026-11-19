//! Database operations on `.barca/metadata.db`. This file holds what every family shares:
//! the process-wide guard and the cross-process lock ([`db_guard`], [`lock_db`]), opening a
//! connection ([`open_conn`]) and waiting on SQLite's own locks, the database files this
//! process keeps open ([`held_file`]), checkpoints, prepared statements and transactions. Each
//! table family is a submodule ([`schema`] opens and migrates; [`runs`], [`run_steps`],
//! [`run_liveness`], [`logs`], [`materializations`], [`cache_reader`], [`history`], [`stats`],
//! [`partition_keys`], [`schedule`], [`cost_estimates`], [`snapshots`], ...) whose public items
//! are re-exported here, so callers name them `db::X`.
//!
//! All functions are `async` and run on whatever runtime the caller provides —
//! this crate never constructs a runtime of its own.

use barca_model::BarcaError;
use libsql::Builder;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{Mutex, MutexGuard};

/// The index hint every lookup of a node's materializations by run hash carries: Turso
/// otherwise intersects the node and run indexes, scanning the run for each partition. A macro
/// so it concatenates into SQL literals (`concat!`); [`BY_NODE_RUN`] is the same text for
/// `format!`.
macro_rules! by_node_run {
    () => {
        "INDEXED BY idx_mat_node_run"
    };
}
pub const BY_NODE_RUN: &str = by_node_run!();

// The metadata write boundary: every INSERT, UPDATE, DELETE or REPLACE of the database is in
// this directory (one file per table family); `recording.rs` drives the recording ones. The
// boundary test is barca-core's `write_boundary_tests.rs` (it scans every crate).
mod asset_pauses;
mod cache_reader;
pub mod cost_estimates;
mod failure;
pub mod freshness_proofs;
mod history;
pub mod ledger_posts;
mod logs;
pub mod materializations;
pub mod notifications;
mod partition_keys;
pub(crate) mod pins;
mod run_filter;
mod run_liveness;
pub mod run_queue;
mod run_steps;
pub mod runs;
mod schedule;
pub mod schema;
pub(crate) mod sink_publications;
mod snapshots;
mod stats;
mod ticks;
pub(crate) mod uploads;

// Every family's items keep their `db::X` path. The private `use`s are `pub(super)` helpers one
// family calls in another; each family file starts with `use super::*`.
pub(crate) use asset_pauses::CREATE as CREATE_ASSET_PAUSES;
pub use asset_pauses::{AssetPause, get_asset_pauses, pause_assets, resume_assets};
pub use cache_reader::{CacheReader, READ_STAMP_REFRESH};
pub use cost_estimates::cost_estimates_for;
use cost_estimates::cost_estimates_on;
pub use failure::{FailureFacts, LastSuccess, SourceLine, failure_facts, raised_at};
#[doc(hidden)]
pub use history::MaterializationRecord;
pub use history::changed_bytes_in_run;
use history::query_i64;
pub use history::{
    BytesChange, NodeAge, NodeAgesIndex, RowChange, RowFacts, RunTotals, node_ages, rows_changed,
};
pub use history::{NodeHistory, node_histories};
pub use logs::insert_logs;
pub use logs::{LogEntry, get_logs};
pub use materializations::last_output_hashes;
pub use materializations::persist_outputs;
use materializations::record_definitions;
pub use materializations::{
    DEFINITION_KEY, StepHistory, SuccessRow, adopt_store_digests, any_key_succeeded,
    components_of_rows, confirmed_object, definition_components, evicted_among, evicted_among_on,
    key_range, last_success_components, latest_success, latest_success_id,
    partition_artifacts_for_config, step_histories, touch_read, touch_read_in, touch_read_on,
};
pub use materializations::{max_materialization_id, succeeded_since};
pub use partition_keys::FailedKeys;
pub use partition_keys::{
    KeyAttempt, LatestAttempt, PartitionErrorGroup, PartitionKeyRow, PartitionKeys,
    PartitionKeysFilter, error_shape, key_attempts, latest_attempts, partition_keys,
};
pub use partition_keys::{empty_partition_keys, failed_partition_keys};
pub use pins::{
    AssetPin, Upload, UploadedArtifact, Version, clear_pin, get_pins, get_versions, record_upload,
    set_pin,
};
pub use pins::{pins_on, success_at};
pub use run_liveness::{AliveRun, alive_running_runs, alive_running_runs_in};
use run_liveness::{interrupted_runs, mark_interrupted_runs};
pub use run_liveness::{live_runs, runs_alive_here};
pub use run_steps::get_run_steps;
#[doc(hidden)]
pub use run_steps::worker_exit;
pub use run_steps::{
    RUN_STEPS_LIMIT, RunNodeSummary, RunStep, StepRow, StepsPage, attach_trace_ids,
    get_run_steps_page, get_run_steps_window, get_run_traced, read_step_rows, run_node_summaries,
};
#[cfg(test)]
pub(crate) use runs::create_run_with;
pub use runs::{
    ActivityDuration, ActivityFailure, ActivityRow, NewRun, RunActivityFacts, RunFilter, RunRecord,
    count_runs, count_runs_filtered, create_run, encode_files, finish_run, generate_run_id,
    get_recent_runs, get_run, get_run_outcome, get_runs_filtered, reconcile_run,
    record_unstarted_run, replace_run_outcome, run_activity, run_artifact, set_cancellation,
    set_plan_reasons, set_run_outcome,
};
pub use runs::{RunFailure, StartedRun, split_exception, start_run};
pub use schedule::{
    SCHEDULED_RUNS_WINDOW, ScheduleFailures, SchedulePause, ScheduledRun, get_schedule_failures,
    get_schedule_pauses, get_schedule_state, scheduled_runs, set_schedule_failures,
    set_schedule_pause, upsert_schedule_state,
};
use schema::{has_column, has_index, initialized};
pub use schema::{init_db, init_db_as, probe_metadata};
pub use schema::{init_locked, init_schema};
pub use snapshots::copy_for_backup;
pub use snapshots::{DbSnapshot, ReadView};
pub use stats::{AssetRunEntry, AssetStats, get_asset_stats, recent_durations};
use stats::{IN_LIST_CHUNK, KEY_ROWS, avg_elapsed_on, partitioned_avg_elapsed_on};
pub(crate) use ticks::CREATE as CREATE_SCHEDULE_TICKS;
pub use ticks::{
    LatestTicks, NewTick, NodeTickOutcome, TICK_MAX_AGE_SECONDS, TICKS_PER_SCHEDULE,
    TICKS_WITH_TARGETS, TickCounts, TickDecision, TickRecord, TickTargetOutcome, TickTimings,
    latest_ticks, newest_decided, recent_ticks, record_tick, record_tick_finished,
    record_tick_started, tick_outcomes_for_node, tick_timings, ticks_for_schedule, trim_ticks,
};

/// Process-wide serialization of `metadata.db` operations. The server can run
/// multiple pipelines in parallel (they execute Python concurrently), but their
/// brief reads/writes to the shared SQLite file must not overlap. Every DB
/// helper — and the inline cache-check/persist ops in `execution::execute` — holds
/// this guard for the duration of its (short) database work, so runs never race
/// on the file without depending on WAL support. A one-shot CLI run leaves it
/// uncontended.
///
/// Each open also holds a short cross-process lock ([`acquire_file_lock`]). It
/// serializes the read/write operation with snapshots, restores and other Barca processes.
/// libSQL uses SQLite WAL; no database lock is held across Python execution.
///
/// Known limit: this serializes every DB op and each helper opens a fresh
/// connection, which becomes the contention point under many concurrent daemon
/// runs. The Engine extraction (#80) replaces this with a single owner holding
/// a persistent connection.
static DB_LOCK: Mutex<()> = Mutex::const_new(());

/// Prepare `sql` once for a loop that runs it per row. The driver parses and plans the SQL of every
/// `Connection::execute` again, which dominated recording thousands of rows (#460); a prepared
/// statement is parsed once and rebound for each row. Every per-row write statement goes
/// through here, so tests can count preparations ([`prepares`]).
pub async fn prepare(conn: &libsql::Connection, sql: &str) -> Result<Prepared, libsql::Error> {
    PREPARES.with(|n| n.set(n.get() + 1));
    conn.prepare(sql).await.map(Prepared)
}

/// A reusable statement always starts from its initial state. Keep this driver detail
/// here: libSQL leaves a completed statement at SQLITE_DONE until it is reset.
pub struct Prepared(libsql::Statement);
impl Prepared {
    pub(crate) async fn execute(
        &mut self,
        params: impl libsql::params::IntoParams,
    ) -> Result<u64, libsql::Error> {
        self.0.reset();
        self.0.execute(params).await.map(|n| n as u64)
    }
    pub async fn query(
        &mut self,
        params: impl libsql::params::IntoParams,
    ) -> Result<libsql::Rows, libsql::Error> {
        self.0.reset();
        self.0.query(params).await
    }
}

/// Run `body` in a transaction on `conn`: `begin` (`BEGIN` or `BEGIN IMMEDIATE`), then `COMMIT`
/// when `body` succeeds, `ROLLBACK` when it or the commit fails. `body` is not polled before the
/// transaction has begun. Errors name `what`: `failed to begin {what}`, `failed to commit {what}`,
/// and a failed rollback is added to the error that caused it (`{error}; {what} rollback
/// failed`). A best-effort caller ignores the result.
pub async fn with_tx<T>(
    conn: &libsql::Connection,
    begin: &str,
    what: &str,
    body: impl std::future::Future<Output = Result<T, BarcaError>>,
) -> Result<T, BarcaError> {
    conn.execute(begin, ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin {what}: {e}")))?;
    let result = match body.await {
        Ok(value) => conn
            .execute("COMMIT", ())
            .await
            .map(|_| value)
            .map_err(|e| BarcaError::Db(format!("failed to commit {what}: {e}"))),
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        conn.execute("ROLLBACK", ())
            .await
            .map_err(|e| BarcaError::Db(format!("{error}; {what} rollback failed: {e}")))?;
        return Err(error);
    }
    result
}

thread_local! {
    static PREPARES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many statements [`prepare`] has prepared on this thread. Tests run on a current-thread
/// runtime, so this counts only their own writes. Always compiled (one thread-local add per
/// prepared statement): barca-core's tests count too, and a dependency's `cfg(test)` does not
/// reach them.
#[doc(hidden)]
pub fn prepares() -> u64 {
    PREPARES.with(std::cell::Cell::get)
}

/// Acquire the process-wide DB lock. Hold the returned guard only across a
/// single database operation; never across Python execution.
pub async fn db_guard() -> MutexGuard<'static, ()> {
    DB_LOCK.lock().await
}

/// [`db_guard`] for an operation on `db_path`; none for a [`ReadView`], which reads beside
/// the writers instead of taking turns with them.
pub async fn db_guard_for(db_path: &str) -> Option<MutexGuard<'static, ()>> {
    if is_read_view(db_path) {
        return None;
    }
    Some(DB_LOCK.lock().await)
}

/// The [`ReadView`]s open in this process: each one's path, and the database it reads.
static READ_VIEWS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(Default::default);

/// True when `db_path` names a [`ReadView`].
pub fn is_read_view(db_path: &str) -> bool {
    READ_VIEWS.lock().unwrap().contains_key(db_path)
}

/// The database `db_path` is: itself, or for a [`ReadView`] the database the view reads. What
/// lives beside a database (`live-logs/`, `run-owners/`) is beside this path, not beside a
/// view's link in a temp dir.
pub fn database_of(db_path: &str) -> std::borrow::Cow<'_, str> {
    match READ_VIEWS.lock().unwrap().get(db_path) {
        Some(database) => std::borrow::Cow::Owned(database.clone()),
        None => std::borrow::Cow::Borrowed(db_path),
    }
}

/// How long a connection waits on SQLite's own locks before `database is locked`
/// ([`wait_when_busy`]): in WAL mode only while the log's index is rebuilt or a checkpoint
/// truncates the log, which takes milliseconds.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// How long [`fold_log`] tries again a checkpoint that a [`ReadView`]'s query kept from
/// finishing: one query of a read route lasts milliseconds. As long as a connection waits on
/// SQLite's own locks ([`BUSY_WAIT`]), which is how long one try used to block.
const CHECKPOINT_BUSY_WAIT: Duration = BUSY_WAIT;

/// A read-only connection to a [`ReadView`]'s database.
async fn connect_view(path: &str) -> Result<(libsql::Database, libsql::Connection), BarcaError> {
    OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db = Builder::new_local(path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .map_err(db_open_error)?;
    let conn = db.connect().map_err(db_open_error)?;
    // Before the first query: the first reader after the last writer closed rebuilds the log's
    // index, and a reader that comes then waits for it instead of failing.
    wait_when_busy(&conn)?;
    let _ = conn.execute("PRAGMA cache_size = -32768", ()).await;
    crate::db::schema::ddl::check(&conn)
        .await
        .map_err(|e| unusable_at(e, path))?;
    Ok((db, conn))
}

// barca-server names these as `db::` paths; the `.barca/` layout is `barca_model::layout`'s.
pub use barca_model::layout::{ensure_env_dirs, env_local_paths, serve_socket_path};

/// How long one barca process waits for another's DB operation before giving up.
/// Operations are brief (a few queries), so hitting this means a stuck process.
const DB_FILE_LOCK_WAIT: Duration = Duration::from_secs(60);

/// A database open for one operation, plus the cross-process lock that makes it safe to use.
/// Field order matters: a database opened for this operation alone is closed before the lock
/// is released.
pub struct DbHandle {
    /// The owner's connection this operation was handed ([`OWNED`]): looked at once more when
    /// the handle is dropped, with the lock still held.
    owned: Option<OwnedUse>,
    /// The database of a connection opened for this operation alone: a [`ReadView`]'s, or one
    /// of a file the owner does not keep a connection to ([`owns`]).
    _db: Option<libsql::Database>,
    /// None for a [`ReadView`], which reads without the lock.
    _lock: Option<FileLock>,
}

impl DbHandle {
    /// The handle of a connection opened for one operation.
    fn of_one_operation(db: libsql::Database, lock: Option<FileLock>) -> Self {
        Self {
            owned: None,
            _db: Some(db),
            _lock: lock,
        }
    }
}

impl Drop for DbHandle {
    fn drop(&mut self) {
        // Before the fields drop, so before the lock is released.
        if let Some(used) = self.owned.take() {
            release_owned(used);
        }
    }
}

// ─── The owner: one write connection per metadata database, per process ──────────────────────

/// The write connections this process keeps open, by database path: one per metadata
/// database, handed to each operation in turn by [`open_conn`] in place of a connection opened
/// and closed for it (#1056, A8; `docs/engineering/metadata-owner.md`).
///
/// Opening a connection for every operation cost two opens (a read-only format check, then the
/// writer) and, in a process with no other connection, a checkpoint and the deletion of the
/// log at every close, with the log and its index created again by the next operation.
///
/// Nothing about the locks changes: an operation still takes the cross-process lock for itself
/// and holds it until its [`DbHandle`] is dropped, so two operations never use the connection
/// at once (the lock is a `flock` on a descriptor of each operation's own, which excludes a
/// second holder in this process as it does one in another), other processes get their turns
/// between operations, and a backup, a restore or a trim that holds the lock keeps every
/// writer out.
///
/// What a kept connection adds is the question whether it is still a connection to the
/// database at that path. Every handout answers it under the lock ([`reusable`]): the file at
/// the path and its `-wal` must be the files the connection has open (device and inode), and
/// no transaction may be open on it. A database a restore or a reset replaced, or one whose
/// log was deleted under this process, gets a new connection, opened as every connection used
/// to be.
static OWNED: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Owned>>> =
    std::sync::LazyLock::new(Default::default);

/// One kept connection. Field order: the connection closes before its database.
struct Owned {
    conn: libsql::Connection,
    _db: libsql::Database,
    /// The database file the connection was opened on.
    main: FileId,
    /// Its `-wal` when the connection was last released (or opened): the log the connection
    /// has open. `None` while the database has none.
    wal: Option<FileId>,
    /// Which connection this is, of all this process kept: tests tell a kept one from a new.
    serial: u64,
}

/// The serial of the connection kept for the database at `path`.
#[cfg(test)]
pub(crate) fn kept_serial(path: &str) -> Option<u64> {
    OWNED.lock().unwrap().get(path).map(|entry| entry.serial)
}

static KEPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The owner's connection in the hands of one operation.
struct OwnedUse {
    path: String,
    conn: libsql::Connection,
}

/// The copies [`DbSnapshot`]s made that still exist, by path: files named `metadata.db` in a
/// temp dir, which the owner must not keep open past their snapshot.
static SNAPSHOT_COPIES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// Whether the owner keeps a connection to the file at `path`: an environment's metadata
/// database (`metadata.db`, `specs/interfaces/dot-barca-layout.md`). A backup copy, a staged
/// restore and a snapshot's copy are temporary files read once: a kept connection would hold
/// their disk space after they are deleted.
fn owns(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .is_some_and(|name| name == "metadata.db")
        && !SNAPSHOT_COPIES.lock().unwrap().contains(path)
}

/// The device and inode of the file at `path`, if there is one.
fn file_id(path: &str) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

fn wal_id(path: &str) -> Option<FileId> {
    file_id(&format!("{path}-wal"))
}

/// The owner's connection to the database at `path`, if it has one that is still a connection
/// to the files at that path with no transaction open. The caller holds the database's lock,
/// so the files cannot be replaced by a barca process until it lets go. A connection that
/// fails the check is dropped here: it closes, and SQLite neither checkpoints nor deletes the
/// log of a database that is no longer at the path it was opened on.
fn reusable(path: &str) -> Option<libsql::Connection> {
    let mut stale = None;
    let conn = {
        let mut owned = OWNED.lock().unwrap();
        let entry = owned.get(path)?;
        if Some(entry.main) == file_id(path)
            && entry.wal == wal_id(path)
            && entry.conn.is_autocommit()
        {
            Some(entry.conn.clone())
        } else {
            stale = owned.remove(path);
            None
        }
    };
    // Closed outside the table's lock.
    drop(stale);
    conn
}

/// Keep `conn`, just opened on the database at `path` under its lock, for the operations to
/// come. Connections to databases that are no longer at their paths (a test's temp dir, a
/// database moved aside) are dropped first.
fn keep(path: &str, main: FileId, db: libsql::Database, conn: libsql::Connection) -> OwnedUse {
    let entry = Owned {
        conn: conn.clone(),
        _db: db,
        main,
        wal: wal_id(path),
        serial: KEPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    };
    let stale: Vec<Owned> = {
        let mut owned = OWNED.lock().unwrap();
        let gone: Vec<String> = owned
            .iter()
            .filter(|(at, entry)| file_id(at) != Some(entry.main))
            .map(|(at, _)| at.clone())
            .collect();
        let mut stale: Vec<Owned> = gone.iter().filter_map(|at| owned.remove(at)).collect();
        stale.extend(owned.insert(path.to_string(), entry));
        stale
    };
    drop(stale);
    OwnedUse {
        path: path.to_string(),
        conn,
    }
}

/// The handle of an operation on a connection just opened on the database at `path` under
/// `lock`: kept by the owner for the operations to come when the file is one it keeps a
/// connection to ([`owns`]), else the operation's own.
fn handle_of_new(
    path: &str,
    db: libsql::Database,
    conn: &libsql::Connection,
    lock: FileLock,
) -> DbHandle {
    match file_id(path).filter(|_| owns(path)) {
        Some(main) => DbHandle {
            owned: Some(keep(path, main, db, conn.clone())),
            _db: None,
            _lock: Some(lock),
        },
        None => DbHandle::of_one_operation(db, Some(lock)),
    }
}

/// An operation is done with the owner's connection; its lock is still held. A connection left
/// in a transaction (the operation's future was dropped between `BEGIN` and `COMMIT`) is not
/// kept: it closes when the operation's own clone drops, and SQLite rolls the transaction
/// back, as it did when every operation had a connection of its own.
fn release_owned(used: OwnedUse) {
    let mut dropped = None;
    {
        let mut owned = OWNED.lock().unwrap();
        if !used.conn.is_autocommit() {
            dropped = owned.remove(&used.path);
        } else if let Some(entry) = owned.get_mut(&used.path) {
            // The log the connection has open now: created by its first write in WAL mode,
            // and never replaced while this process holds the lock.
            entry.wal = wal_id(&used.path);
        }
    }
    drop(dropped);
    drop(used);
}

/// The connection for one operation on the database at `path`, whose lock the caller holds
/// (`lock`): the owner's, kept or opened now, or for a file the owner does not keep one to
/// ([`owns`]) a connection of the operation's own.
async fn connection_for(
    path: &str,
    lock: FileLock,
) -> Result<(DbHandle, libsql::Connection), BarcaError> {
    if !owns(path) {
        let (db, conn) = connect(path).await?;
        return Ok((DbHandle::of_one_operation(db, Some(lock)), conn));
    }
    // As every open checked: a file that is not a database is not handed to SQLite.
    if let Some(why) = unusable_file(path) {
        return Err(unusable_at(unusable(why), path));
    }
    if let Some(conn) = reusable(path) {
        // What every open read: a database a newer barca rewrote meanwhile is refused now.
        if let Err(e) = crate::db::schema::ddl::check(&conn).await {
            let dropped = OWNED.lock().unwrap().remove(path);
            drop(dropped);
            return Err(unusable_at(e, path));
        }
        return Ok((handle_of_kept(path, &conn, lock), conn));
    }
    let (db, conn) = connect(path).await?;
    Ok((handle_of_new(path, db, &conn, lock), conn))
}

/// The handle of an operation on the owner's connection `conn` to the database at `path`.
fn handle_of_kept(path: &str, conn: &libsql::Connection, lock: FileLock) -> DbHandle {
    DbHandle {
        owned: Some(OwnedUse {
            path: path.to_string(),
            conn: conn.clone(),
        }),
        _db: None,
        _lock: Some(lock),
    }
}

/// Close the connections this process keeps ([`OWNED`]), each under its database's lock when
/// that is free right now: the last connection to close folds the log into the main file and
/// deletes it, which is what every operation's close did. The CLI calls this as a command
/// ends. A connection whose lock is held by another process is left to the exit: that process
/// has the database open, and the log is its to fold.
pub fn close_connections() {
    close_kept(|_| true);
}

/// [`close_connections`] for the databases `which` picks.
fn close_kept(which: impl Fn(&str) -> bool) {
    let kept: Vec<(String, Owned)> = {
        let mut owned = OWNED.lock().unwrap();
        let paths: Vec<String> = owned.keys().filter(|path| which(path)).cloned().collect();
        paths
            .into_iter()
            .filter_map(|path| owned.remove(&path).map(|entry| (path, entry)))
            .collect()
    };
    for (path, entry) in kept {
        let lock = fs::OpenOptions::new()
            .write(true)
            .open(format!("{path}.lock"))
            .ok()
            .filter(|file| file.try_lock().is_ok());
        match lock {
            Some(_lock) => drop(entry),
            None => std::mem::forget(entry),
        }
    }
}

/// The cross-process lock of one database, held: released when dropped (or when the process
/// exits or crashes). How long it was held is counted ([`lock_holds`]).
#[derive(Debug)]
pub(crate) struct FileLock {
    _file: fs::File,
    lock_path: String,
    /// The lock file's stamp when this hold began ([`Turn`]).
    stamp: Option<SystemTime>,
    since: Instant,
}

impl FileLock {
    fn held(file: fs::File, lock_path: String) -> Self {
        Self {
            stamp: stamp_of(&file),
            _file: file,
            lock_path,
            since: Instant::now(),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        note_lock_hold(self.since.elapsed());
        let mut turns = TURNS.lock().unwrap();
        // One entry per database this process used; a test process uses thousands.
        if turns.len() >= 64 {
            turns.retain(|_, turn| turn.released.elapsed() < TURN_RECENT);
        }
        turns.insert(
            std::mem::take(&mut self.lock_path),
            Turn {
                released: Instant::now(),
                stamp: self.stamp,
            },
        );
    }
}

/// Taking turns. The lock is a `flock` that waiters poll, so a process that takes it again
/// right after releasing it (a run recording step after step, a trim working through its
/// batches) leaves a gap of microseconds that a waiter almost never finds: a `barca gc` beside
/// a server's two recording runs timed out after 60 s without one turn (#1054), though no
/// single hold was long.
///
/// So a waiter says it is waiting, in the one place every process already looks: it sets the
/// lock file's modification time each time it finds the lock held ([`stamp`]). A process
/// about to take the lock again within [`TURN_RECENT`] of releasing it compares the file's
/// time with the one it saw when its last hold began; a different one means somebody waited
/// during that hold or since, and it leaves the lock free for [`TURN`] first, which is longer
/// than a waiter's longest pause between tries ([`WAIT_POLL_MAX`]). The file stays empty and
/// nothing else is written. A process of an earlier release neither stamps nor gives way: it
/// gets and gives the turns it always did.
#[derive(Clone, Copy)]
struct Turn {
    released: Instant,
    stamp: Option<SystemTime>,
}

/// This process's last hold of each lock file, by path.
static TURNS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Turn>>> =
    std::sync::LazyLock::new(Default::default);

/// How long a process that sees a waiter leaves the lock free before taking it again.
const TURN: Duration = Duration::from_millis(8);
/// A release longer ago than this already left the lock free for any waiter.
const TURN_RECENT: Duration = Duration::from_millis(100);
/// The longest pause of a waiter between two tries: shorter than [`TURN`].
const WAIT_POLL_MAX: Duration = Duration::from_millis(5);

fn stamp_of(file: &fs::File) -> Option<SystemTime> {
    file.metadata().and_then(|m| m.modified()).ok()
}

/// Say that this process is waiting for the lock `file` is ([`Turn`]). Best effort.
fn stamp(file: &fs::File) {
    let _ = file.set_modified(SystemTime::now());
}

/// Whether another process waited for `lock_path` during this process's last hold of it or
/// since, when that hold ended a moment ago.
fn waiter_seen(lock_path: &str, file: &fs::File) -> bool {
    let Some(last) = TURNS.lock().unwrap().get(lock_path).copied() else {
        return false;
    };
    last.released.elapsed() < TURN_RECENT && stamp_of(file) != last.stamp
}

/// Take the cross-process lock for `db_path` (a `<db>.lock` file next to it), waiting
/// up to `wait` for another barca process to finish its operation. The lock is
/// released when the returned file is dropped (or the process exits or crashes).
async fn acquire_file_lock(db_path: &str, wait: Duration) -> Result<FileLock, BarcaError> {
    let lock_path = format!("{db_path}.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| BarcaError::Db(format!("failed to open lock file {lock_path}: {e}")))?;
    // Somebody waited while this process had the lock: their turn first.
    if waiter_seen(&lock_path, &file) {
        note_turn_given();
        tokio::time::sleep(TURN).await;
    }
    // Uncontended (every operation of a one-shot command): one `flock`, no clock read.
    match file.try_lock() {
        Ok(()) => return Ok(FileLock::held(file, lock_path)),
        Err(fs::TryLockError::WouldBlock) => {}
        Err(fs::TryLockError::Error(e)) => {
            return Err(BarcaError::Db(format!("failed to lock {lock_path}: {e}")));
        }
    }
    let started = Instant::now();
    let deadline = started + wait;
    let mut delay = Duration::from_millis(1);
    loop {
        if Instant::now() >= deadline {
            let holder = match lock_holder(&file) {
                Some(holder) => format!("held now by {holder}"),
                None => "its holder could not be named on this system".to_string(),
            };
            return Err(BarcaError::Db(format!(
                "timed out after {:.1}s waiting for {lock_path}: another barca process is \
                 holding the metadata DB ({holder}): a very long operation, a stuck process, or \
                 many short operations of a busy `barca serve` in this project\n\
                 With a server running here, leave out --local so the command goes to it \
                 (barca docs server-mode); otherwise wait for that process or stop it, then run \
                 the command again.",
                wait.as_secs_f32()
            )));
        }
        // Tell the holder somebody is waiting, then back off with a little jitter so waiters
        // don't wake in lockstep.
        stamp(&file);
        let jitter =
            Duration::from_micros((barca_model::time::unix_nanos(SystemTime::now()) % 1000) as u64);
        tokio::time::sleep(delay + jitter).await;
        delay = (delay * 2).min(WAIT_POLL_MAX - Duration::from_millis(1));
        match file.try_lock() {
            Ok(()) => {
                note_lock_wait(started.elapsed());
                return Ok(FileLock::held(file, lock_path));
            }
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(e)) => {
                return Err(BarcaError::Db(format!("failed to lock {lock_path}: {e}")));
            }
        }
    }
}

/// Times this process left the lock free for a waiter ([`Turn`]).
static TURNS_GIVEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn note_turn_given() {
    TURNS_GIVEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// How often this process gave way to a waiter before taking the lock again.
pub fn turns_given() -> u64 {
    TURNS_GIVEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Contended acquisitions of the cross-process lock in this process: how many, their total
/// wait and the longest, in microseconds. An uncontended acquisition is not counted.
static LOCK_WAITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_WAIT_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_WAIT_LONGEST_MICROS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn note_lock_wait(waited: Duration) {
    use std::sync::atomic::Ordering::Relaxed;
    let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
    LOCK_WAITS.fetch_add(1, Relaxed);
    LOCK_WAIT_MICROS.fetch_add(micros, Relaxed);
    LOCK_WAIT_LONGEST_MICROS.fetch_max(micros, Relaxed);
}

/// Holds of the cross-process lock by this process: how many, their total and the longest, in
/// microseconds. The longest is how long another barca process can have waited for this one.
static LOCK_HOLDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_HOLD_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_HOLD_LONGEST_MICROS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn note_lock_hold(held: Duration) {
    use std::sync::atomic::Ordering::Relaxed;
    let micros = u64::try_from(held.as_micros()).unwrap_or(u64::MAX);
    LOCK_HOLDS.fetch_add(1, Relaxed);
    LOCK_HOLD_MICROS.fetch_add(micros, Relaxed);
    LOCK_HOLD_LONGEST_MICROS.fetch_max(micros, Relaxed);
}

/// How long this process held a metadata database's lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockHolds {
    /// Times the lock was taken.
    pub holds: u64,
    /// Their total.
    pub total: Duration,
    /// The longest one.
    pub longest: Duration,
}

/// [`LockHolds`] so far (`BARCA_TRACE_TIMING` prints it at the end of a command).
pub fn lock_holds() -> LockHolds {
    use std::sync::atomic::Ordering::Relaxed;
    LockHolds {
        holds: LOCK_HOLDS.load(Relaxed),
        total: Duration::from_micros(LOCK_HOLD_MICROS.load(Relaxed)),
        longest: Duration::from_micros(LOCK_HOLD_LONGEST_MICROS.load(Relaxed)),
    }
}

/// What this process waited for another process's hold on a metadata database's lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockWaits {
    /// Acquisitions that found the lock held and waited.
    pub waits: u64,
    /// Their total wait.
    pub total: Duration,
    /// The longest one.
    pub longest: Duration,
}

/// [`LockWaits`] so far (`BARCA_TRACE_TIMING` prints it at the end of a run).
pub fn lock_waits() -> LockWaits {
    use std::sync::atomic::Ordering::Relaxed;
    LockWaits {
        waits: LOCK_WAITS.load(Relaxed),
        total: Duration::from_micros(LOCK_WAIT_MICROS.load(Relaxed)),
        longest: Duration::from_micros(LOCK_WAIT_LONGEST_MICROS.load(Relaxed)),
    }
}

/// The process holding the lock on `file` right now, as `pid N (command line)`, read from the
/// kernel's lock table (`/proc/locks`, Linux only). `None` where that table does not exist or
/// does not name the file. Nothing is written to disk to make this possible: the holder is
/// looked up only when a wait has timed out.
fn lock_holder(file: &fs::File) -> Option<String> {
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        file.metadata().ok()?.ino()
    };
    #[cfg(not(unix))]
    let inode: u64 = {
        let _ = file;
        return None;
    };
    let table = fs::read_to_string("/proc/locks").ok()?;
    let pid = holder_pid(&table, inode, std::process::id())?;
    let command = fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|raw| {
            raw.split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|command| !command.is_empty());
    Some(match command {
        Some(command) => {
            let short: String = command.chars().take(200).collect();
            format!("pid {pid} ({short})")
        }
        None => format!("pid {pid}"),
    })
}

/// The pid holding a `flock` on `inode` in a `/proc/locks` table: a line reads
/// `<n>: FLOCK ADVISORY WRITE <pid> <major>:<minor>:<inode> <start> <end>` (a waiter's has
/// `->` after the ordinal). Another process is preferred to this one, which may hold the same
/// lock on another descriptor.
fn holder_pid(table: &str, inode: u64, me: u32) -> Option<u32> {
    let mut mine = None;
    for line in table.lines() {
        let mut fields = line.split_whitespace().skip(1);
        if fields.next() != Some("FLOCK") {
            continue;
        }
        let (Some(_), Some(_), Some(pid), Some(file)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if file.rsplit(':').next().and_then(|i| i.parse::<u64>().ok()) != Some(inode) {
            continue;
        }
        match pid.parse::<u32>() {
            Ok(pid) if pid == me => mine = Some(pid),
            Ok(pid) => return Some(pid),
            Err(_) => {}
        }
    }
    mine
}

/// The in-process guard and the cross-process lock of the database at `db_path`, for an
/// operation that must see no other barca process touch the file. Held until dropped.
pub struct DbLock {
    _lock: FileLock,
    _guard: MutexGuard<'static, ()>,
}

pub async fn lock_db(db_path: &str) -> Result<DbLock, BarcaError> {
    lock_db_with_until(db_path, barca_model::process::Until::done()).await
}

/// Cancellation applies only to lock admission. The caller must finish any admitted
/// atomic database mutation before releasing this lock.
pub(crate) async fn lock_db_with_until(
    db_path: &str,
    until: barca_model::process::Until<'_>,
) -> Result<DbLock, BarcaError> {
    let guard = until.admit(db_guard()).await?;
    let lock = until
        .admit(acquire_file_lock(db_path, DB_FILE_LOCK_WAIT))
        .await??;
    until.check()?;
    Ok(DbLock {
        _lock: lock,
        _guard: guard,
    })
}

/// Database files this process opened itself, outside SQLite, by device and inode. None is
/// ever closed while a path still names it.
///
/// SQLite's locks on a database are POSIX record locks, and a process loses every lock it
/// holds on a file, from every connection, as soon as it closes any descriptor of that file.
/// In WAL mode each connection keeps a shared lock on the database file for as long as it is
/// open, and a connection that closes and finds no other lock takes the database for its own:
/// it checkpoints without asking the readers, then deletes the log and its index under them.
/// So a header check that opened and closed the file let a writer in another process do that
/// under a `--read-only` server's queries, which then read `database disk image is malformed`.
/// SQLite keeps its own descriptors open for the same reason; barca reads a database file only
/// through these.
static HELD_FILES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<FileId, std::sync::Arc<fs::File>>>,
> = std::sync::LazyLock::new(Default::default);

/// A file's device and inode.
type FileId = (u64, u64);

/// The database file at `path`, opened once and kept open ([`HELD_FILES`]). Read it with
/// positional reads (`FileExt::read_at`): the descriptor is shared.
pub(crate) fn held_file(path: &Path) -> std::io::Result<std::sync::Arc<fs::File>> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    let mut held = HELD_FILES.lock().unwrap();
    if let Some(file) = held.get(&(meta.dev(), meta.ino())) {
        return Ok(file.clone());
    }
    // A file no path names any more (a snapshot's copy, a database replaced by a restore)
    // can be let go: no connection opened from now on can reach it.
    held.retain(|_, file| file.metadata().is_ok_and(|m| m.nlink() > 0));
    let file = fs::File::open(path)?;
    let opened = file.metadata()?;
    let key = (opened.dev(), opened.ino());
    if let Some(existing) = held.get(&key) {
        // Replaced and put back between the two looks: keep the first, and never close this.
        std::mem::forget(file);
        return Ok(existing.clone());
    }
    let file = std::sync::Arc::new(file);
    held.insert(key, file.clone());
    Ok(file)
}

/// Checkpoint the database at `db_path` and verify that its main file is now the whole
/// database. The caller holds the database's lock.
pub async fn fold_log(db_path: &str) -> Result<(), BarcaError> {
    {
        let (_db, conn) = connect(db_path).await?;
        // The lock keeps every writer out, not a [`ReadView`]: a reader in the middle of a
        // query holds the log until the query ends, so a busy checkpoint (and only that) is
        // tried again for a moment. Each try answers at once: SQLite's own wait sleeps on
        // this thread, and a reader on the same thread (a current-thread runtime) could then
        // never finish. The log is rarely empty now that a process keeps its connection
        // ([`OWNED`]), so a checkpoint really does wait for the readers of what is in it.
        conn.busy_timeout(Duration::ZERO)
            .map_err(|e| BarcaError::Db(format!("failed to set busy timeout: {e}")))?;
        let deadline = Instant::now() + CHECKPOINT_BUSY_WAIT;
        while checkpoint_once(&conn).await? {
            if Instant::now() >= deadline {
                return Err(checkpoint_busy());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    if !wal_is_clean(db_path) {
        let wal = format!("{db_path}-wal");
        return Err(BarcaError::Db(format!(
            "WAL not empty after checkpoint ({} bytes remain in {wal}) — \
             refusing to copy a torn database",
            fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
        )));
    }
    Ok(())
}

/// Why the file at `path` is certainly not a usable SQLite database: it does not start with
/// the SQLite header, or its length is not a whole number of pages (it was cut short). None
/// for a file that looks like a database, and for an empty one (a database not yet
/// checkpointed has an empty main file). A file that cannot be read is not judged here.
pub(crate) fn not_a_database(path: &str) -> Option<String> {
    use std::os::unix::fs::FileExt;
    let len = fs::metadata(path).ok()?.len();
    if len == 0 {
        return None;
    }
    let mut header = [0u8; 100];
    // Through the held file: opening and closing the database here would drop every lock this
    // process's connections hold on it ([`HELD_FILES`]).
    let file = held_file(Path::new(path)).ok()?;
    if len < header.len() as u64 {
        return Some(format!("{len} bytes: too short to be a database"));
    }
    file.read_exact_at(&mut header, 0).ok()?;
    if &header[..16] != b"SQLite format 3\0" {
        return Some("it does not start with a SQLite header".to_string());
    }
    let page_size = match u16::from_be_bytes([header[16], header[17]]) {
        1 => 65536u64,
        n => u64::from(n),
    };
    if !page_size.is_power_of_two() || page_size < 512 {
        return Some(format!(
            "its header gives an impossible page size ({page_size})"
        ));
    }
    if len % page_size != 0 {
        return Some(format!(
            "{len} bytes is not a whole number of {page_size}-byte pages: the file was cut short"
        ));
    }
    None
}

/// Remove the `-wal` and `-shm` files of the database at `path`, if they are there.
pub fn remove_sidecars(path: &str) -> Result<(), BarcaError> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = format!("{path}{suffix}");
        match fs::remove_file(&sidecar) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(BarcaError::Db(format!("failed to remove {sidecar}: {e}")));
            }
        }
    }
    Ok(())
}

/// Wrap a database open failure, adding a hint when the DB is locked by something that
/// does not follow barca's own locking.
fn db_open_error(detail: impl std::fmt::Display) -> BarcaError {
    let detail = detail.to_string();
    let mut msg = format!("failed to open DB: {detail}");
    if detail.contains("locked by another process") || detail.contains("database is locked") {
        msg.push_str(
            "\n\nSomething outside barca's own locking has the metadata DB open (a DB \
             browser, a backup tool, or a barca process from an older version). Close it \
             and retry.",
        );
    }
    BarcaError::Db(msg)
}

/// Lock-coordinated metadata connections, including read-only compatibility preflights. Each open costs a
/// fraction of a millisecond plus the lock; `BARCA_TRACE_TIMING` prints the count at the end
/// of a run, and a test holds a warm run's count down (#560).
static OPENS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// [`OPENS`] so far.
pub fn opens() -> usize {
    OPENS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Open the database file at `path` and connect, taking no lock. For callers that already
/// hold the cross-process lock for the database this file is (or is about to become).
async fn connect(path: &str) -> Result<(libsql::Database, libsql::Connection), BarcaError> {
    // Do not let SQLite recover or discard sidecars of a file that is not a database.
    if let Some(why) = unusable_file(path) {
        return Err(unusable_at(unusable(why), path));
    }
    if Path::new(path).exists() {
        let (db, read) = connect_read_only(path).await?;
        drop(read);
        drop(db);
    }
    connect_write(path).await
}

/// Format checks happen read-only: opening an unsupported database must not checkpoint it.
async fn connect_read_only(
    path: &str,
) -> Result<(libsql::Database, libsql::Connection), BarcaError> {
    OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db = Builder::new_local(path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .map_err(db_open_error)?;
    let conn = db.connect().map_err(db_open_error)?;
    wait_when_busy(&conn)?;
    crate::db::schema::ddl::check(&conn)
        .await
        .map_err(|e| unusable_at(e, path))?;
    Ok((db, conn))
}

/// Before a connection's first query: wait for SQLite's own locks instead of failing with
/// `database is locked`. barca's lock keeps writers apart, but a [`ReadView`] reads beside
/// them, and the first connection after the last one closed rebuilds the log's index under a
/// lock for a moment, whichever connection it is.
fn wait_when_busy(conn: &libsql::Connection) -> Result<(), BarcaError> {
    conn.busy_timeout(BUSY_WAIT)
        .map_err(|e| BarcaError::Db(format!("failed to set busy timeout: {e}")))
}

/// The caller holds the file lock and has checked an existing database read-only.
async fn connect_write(path: &str) -> Result<(libsql::Database, libsql::Connection), BarcaError> {
    OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db = Builder::new_local(path)
        .build()
        .await
        .map_err(db_open_error)?;
    let conn = db
        .connect()
        .map_err(|e| BarcaError::Db(format!("failed to connect: {e}")))?;
    wait_when_busy(&conn)?;
    // A page cache of up to 32 MiB (allocated as pages are read). The default is small enough
    // that one pass over the results of a node with 200k partition keys reads its pages from
    // disk several times over: 2.8 s against 0.25 s (benchmarks/keyed_partitions).
    let _ = conn.execute("PRAGMA cache_size = -32768", ()).await;
    // A checkpoint hands back the log file past this size. It otherwise stays at the size the
    // largest transaction left it.
    let _ = conn.query("PRAGMA journal_size_limit = 67108864", ()).await;
    crate::db::schema::ddl::check(&conn)
        .await
        .map_err(|e| unusable_at(e, path))?;
    Ok((db, conn))
}

/// A [`BarcaError::MetadataUnusable`] that does not name the database yet; [`unusable_at`]
/// names it.
pub(crate) fn unusable(why: String) -> BarcaError {
    BarcaError::MetadataUnusable {
        path: String::new(),
        why,
    }
}

/// `error`, naming `path` if it is a [`BarcaError::MetadataUnusable`] that names no database.
pub(crate) fn unusable_at(error: BarcaError, path: &str) -> BarcaError {
    match error {
        BarcaError::MetadataUnusable { path: p, why } if p.is_empty() => {
            BarcaError::MetadataUnusable {
                path: path.to_string(),
                why,
            }
        }
        e => e,
    }
}

/// Why the file at `path` cannot be a metadata DB before it is opened: it is empty with no
/// write-ahead log to hold its pages, or it is not a SQLite file. None for a missing file (a new
/// database is created there) and for one that looks like a database.
pub fn unusable_file(path: &str) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_dir() {
        return Some("it is a directory".to_string());
    }
    if empty_file(path) {
        return Some("it is empty (0 bytes)".to_string());
    }
    not_a_database(path)
}

/// [`BarcaError::MetadataUnusable`] when `path` is a directory, which no snapshot can copy. Other
/// checks of the file wait for its lock ([`init_db_as`]).
pub fn directory_check(path: &str) -> Result<(), BarcaError> {
    if Path::new(path).is_dir() {
        return Err(BarcaError::MetadataUnusable {
            path: path.to_string(),
            why: "it is a directory".to_string(),
        });
    }
    Ok(())
}

/// Whether `path` is a 0-byte file with no write-ahead log beside it: no database, though
/// SQLite would start one there. barca never leaves a database it creates empty.
pub fn empty_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0) && wal_is_clean(path)
}

/// The connection for one operation on the database at `db_path`. Callers must hold
/// [`db_guard`] for the duration of their work on the returned connection; the returned
/// handle holds the cross-process lock until it is dropped. For a metadata database the
/// connection is the one this process keeps ([`OWNED`]), not one opened for the operation:
/// use it only while the handle lives.
pub async fn open_conn(db_path: &str) -> Result<(DbHandle, libsql::Connection), BarcaError> {
    open_conn_with_until(db_path, barca_model::process::Until::done()).await
}

pub async fn open_conn_with_until(
    db_path: &str,
    until: barca_model::process::Until<'_>,
) -> Result<(DbHandle, libsql::Connection), BarcaError> {
    if is_read_view(db_path) {
        until.check()?;
        let (db, conn) = connect_view(db_path).await?;
        return Ok((DbHandle::of_one_operation(db, None), conn));
    }
    let lock = until
        .admit(acquire_file_lock(db_path, DB_FILE_LOCK_WAIT))
        .await??;
    until.check()?;
    connection_for(db_path, lock).await
}

/// Fold the write-ahead log into the main database file (`PRAGMA wal_checkpoint(TRUNCATE)`).
/// No other connection may be open on the file.
pub async fn checkpoint(conn: &libsql::Connection) -> Result<(), BarcaError> {
    if checkpoint_once(conn).await? {
        return Err(checkpoint_busy());
    }
    Ok(())
}

/// One `PRAGMA wal_checkpoint(TRUNCATE)`: true when another connection kept it from finishing.
pub(crate) async fn checkpoint_once(conn: &libsql::Connection) -> Result<bool, BarcaError> {
    let failed = |e| BarcaError::Db(format!("wal_checkpoint(TRUNCATE) failed: {e}"));
    // The pragma returns one (busy, log_pages, checkpointed_pages) row; busy means another
    // connection kept it from finishing.
    let mut rows = conn
        .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .map_err(failed)?;
    let mut busy = false;
    while let Some(row) = rows.next().await.map_err(failed)? {
        busy |= row.get::<i64>(0).unwrap_or(0) != 0;
    }
    Ok(busy)
}

fn checkpoint_busy() -> BarcaError {
    BarcaError::Db(
        "wal_checkpoint(TRUNCATE) could not finish: the database is in use by another \
         connection"
            .to_string(),
    )
}

/// True when the write-ahead log beside `db_path` is absent or empty: the main file alone is
/// then the whole database.
pub fn wal_is_clean(db_path: &str) -> bool {
    match fs::metadata(format!("{db_path}-wal")) {
        Err(_) => true,
        Ok(m) => m.len() == 0,
    }
}

/// `error` naming `shown`, if it says the database cannot be used. The driver's own words for a file
/// it cannot read as a database count.
fn named(error: BarcaError, shown: &str) -> BarcaError {
    match error {
        BarcaError::MetadataUnusable { why, .. } => BarcaError::MetadataUnusable {
            path: shown.to_string(),
            why,
        },
        BarcaError::Db(msg) if msg.contains("not a database") || msg.contains("malformed") => {
            BarcaError::MetadataUnusable {
                path: shown.to_string(),
                why: msg,
            }
        }
        e => e,
    }
}

/// Open the database file at `db_path`, taking no lock, for a caller that holds it
/// ([`lock_db`]). `None` when it cannot be opened or is not one this version reads.
pub async fn connect_locked(db_path: &str) -> Option<(libsql::Database, libsql::Connection)> {
    connect(db_path).await.ok()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn with_tx_commits_a_body_that_succeeds_and_rolls_back_one_that_fails() {
        use barca_model::BarcaError;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        let _guard = super::db_guard().await;
        let (_db, conn) = super::open_conn(&path).await.unwrap();
        let insert = |key: &'static str| {
            let conn = &conn;
            async move {
                conn.execute(
                    "INSERT INTO logs (run_id, node_id, seq, line) VALUES (?1, 'n', 0, 'l')",
                    [key],
                )
                .await
                .map_err(|e| BarcaError::Db(e.to_string()))
            }
        };
        let kept = super::with_tx(&conn, "BEGIN", "test", async {
            insert("kept").await?;
            Ok(7)
        })
        .await;
        assert!(matches!(kept, Ok(7)));
        let failed = super::with_tx(&conn, "BEGIN IMMEDIATE", "test", async {
            insert("dropped").await?;
            Err::<(), _>(BarcaError::Other("body failed".into()))
        })
        .await;
        assert!(matches!(failed, Err(BarcaError::Other(ref m)) if m == "body failed"));
        let mut rows = conn
            .query("SELECT run_id FROM logs ORDER BY run_id", ())
            .await
            .unwrap();
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            keys.push(row.get::<String>(0).unwrap());
        }
        assert_eq!(keys, ["kept"]);
    }

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_writes_are_serialized_safely() {
        // Eight tasks write runs at once. The process-wide DB lock must keep
        // them from racing on the SQLite file — all rows land, none error.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let mut handles = vec![];
        for i in 0..8 {
            let p = db_path.clone();
            handles.push(tokio::spawn(async move {
                let rid = format!("run{i:02}");
                create_run(&p, &rid, "get", "f.py", None, Some(1))
                    .await
                    .unwrap();
                finish_run(&p, &rid, "success", 1, 0, 0.1).await.unwrap();
                upsert_schedule_state(&p, &format!("f.py:node{i}"), i)
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(get_recent_runs(&db_path, 100).await.unwrap().len(), 8);
        assert_eq!(count_runs(&db_path).await.unwrap(), 8);
        assert_eq!(get_schedule_state(&db_path).await.unwrap().len(), 8);
    }

    // ─── Cross-process lock ─────────────────────────────────────────────────

    #[tokio::test]
    async fn file_lock_excludes_a_second_holder_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();

        let first = acquire_file_lock(&db_path, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        // A second handle (another process behaves the same) must wait, then give up loudly.
        let err = acquire_file_lock(&db_path, std::time::Duration::from_millis(150))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("metadata.db.lock"), "{err}");
        assert!(err.contains("another barca process"), "{err}");
        // The kernel's lock table names the holder (this process, on another descriptor).
        if cfg!(target_os = "linux") && std::path::Path::new("/proc/locks").exists() {
            assert!(
                err.contains(&format!("held now by pid {}", std::process::id())),
                "{err}"
            );
        }
        assert!(err.contains("barca docs server-mode"), "{err}");

        drop(first);
        acquire_file_lock(&db_path, std::time::Duration::from_secs(1))
            .await
            .expect("lock is free again once the holder drops it");
    }

    #[test]
    fn the_lock_table_names_another_holder_before_this_process() {
        let table = "1: POSIX  ADVISORY  WRITE 900 00:2a:77 0 EOF\n\
                     2: FLOCK  ADVISORY  WRITE 41 00:2a:77 0 EOF\n\
                     3: -> FLOCK  ADVISORY  WRITE 43 00:2a:77 0 EOF\n\
                     4: FLOCK  ADVISORY  WRITE 42 00:2a:77 0 EOF\n\
                     5: FLOCK  ADVISORY  WRITE 50 00:2a:78 0 EOF\n";
        assert_eq!(holder_pid(table, 77, 41), Some(42));
        assert_eq!(holder_pid(table, 77, 7), Some(41));
        assert_eq!(holder_pid(table, 78, 50), Some(50));
        assert_eq!(holder_pid(table, 79, 7), None);
    }

    #[tokio::test]
    async fn a_contended_acquisition_is_counted_with_its_wait() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        let before = lock_waits();
        let held = acquire_file_lock(&db_path, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        // Uncontended: not counted (other tests of this process may add their own waits).
        let waiter_path = db_path.clone();
        let waiter = tokio::spawn(async move {
            acquire_file_lock(&waiter_path, std::time::Duration::from_secs(5)).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        drop(held);
        waiter.await.unwrap().unwrap();
        let after = lock_waits();
        assert!(after.waits > before.waits, "{before:?} {after:?}");
        assert!(
            after.total >= before.total + std::time::Duration::from_millis(50),
            "{before:?} {after:?}"
        );
        assert!(after.longest >= std::time::Duration::from_millis(50));
    }

    /// A holder that takes the lock again the moment it releases it (a trim's batches, a run
    /// recording step after step) left a waiter gaps of microseconds between holds of
    /// milliseconds, and the waiter's tries almost never landed in one: `barca gc` beside a
    /// busy server timed out after 60 s (#1054). The waiter now stamps the lock file and the
    /// holder, seeing the stamp, leaves the lock free for a moment before its next hold.
    #[tokio::test]
    async fn a_holder_that_takes_the_lock_again_at_once_gives_a_waiter_its_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Stops the holder however this test ends.
        struct Stop(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let _stop = Stop(stop.clone());
        // On a thread and a runtime of its own, as another process is.
        let holder = std::thread::spawn({
            let (db_path, stop) = (db_path.clone(), stop.clone());
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    let mut holds = 0u32;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let held = acquire_file_lock(&db_path, Duration::from_secs(5))
                            .await
                            .unwrap();
                        // The work of one batch: no pause the waiter could use.
                        std::thread::sleep(Duration::from_millis(20));
                        drop(held);
                        holds += 1;
                    }
                    holds
                })
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let given = turns_given();
        let mut waits = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let lock = acquire_file_lock(&db_path, Duration::from_secs(10))
                .await
                .expect("the waiter gets a turn");
            waits.push(started.elapsed());
            drop(lock);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let holds = holder.join().unwrap();
        // One hold of the holder, the turn it then leaves, and the scheduler's slack: far from
        // the seconds a waiter spent finding a gap by chance.
        let longest = waits.iter().max().unwrap();
        assert!(*longest < Duration::from_millis(1500), "{waits:?}");
        assert!(turns_given() > given, "the holder gave way");
        assert!(holds >= 5, "the holder went on between the turns: {holds}");
    }

    /// Nobody waiting: taking the lock again costs no pause.
    #[tokio::test]
    async fn a_holder_alone_never_pauses_between_holds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        let started = Instant::now();
        for _ in 0..200 {
            let held = acquire_file_lock(&db_path, Duration::from_secs(1))
                .await
                .unwrap();
            assert!(!waiter_seen(&held.lock_path, &held._file));
            drop(held);
        }
        // 200 turns given would be 1.6 s.
        assert!(started.elapsed() < Duration::from_millis(800));
        let lock_path = format!("{db_path}.lock");
        assert_eq!(std::fs::metadata(&lock_path).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn file_lock_waiter_proceeds_as_soon_as_the_holder_releases() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();

        let held = acquire_file_lock(&db_path, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        let waiter_path = db_path.clone();
        let waiter = tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            acquire_file_lock(&waiter_path, std::time::Duration::from_secs(5))
                .await
                .map(|_| t0.elapsed())
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        drop(held);
        let waited = waiter.await.unwrap().expect("waiter acquires the lock");
        assert!(
            waited >= std::time::Duration::from_millis(150),
            "waited {waited:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(2),
            "waited {waited:?}"
        );
    }

    #[test]
    fn database_lock_errors_get_an_actionable_hint() {
        let hinted = db_open_error(
            "Locking error: Failed locking file '.barca/metadata.db'. File is locked by another process",
        )
        .to_string();
        assert!(hinted.contains("File is locked by another process"));
        assert!(hinted.contains("outside barca"), "{hinted}");
        let other = db_open_error("disk I/O error").to_string();
        assert!(!other.contains("outside barca"), "{other}");
    }
}

#[cfg(test)]
mod owner_tests;

#[cfg(test)]
mod fold_log_tests {
    use super::{
        acquire_file_lock, connect, copy_for_backup, create_run, fold_log, init_db, lock_db,
        wal_is_clean,
    };
    use crate::db::snapshots::pin_main_file;
    use libsql::Builder;

    /// The guarantee a backup copies under: while the pin is held, commits from other
    /// connections land in the log and no checkpoint (automatic or asked for) writes them to the
    /// main file, so the copy reads stable bytes without barca's lock.
    #[tokio::test]
    async fn a_pinned_main_file_takes_writes_without_changing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        create_run(&path, "before", "get", "p.py", None, None)
            .await
            .unwrap();
        let pin = {
            let _lock = lock_db(&path).await.unwrap();
            fold_log(&path).await.unwrap();
            pin_main_file(&path).await.unwrap()
        };
        let before = std::fs::read(&path).unwrap();
        // Enough commits to pass the automatic checkpoint's 1000 pages.
        for i in 0..300 {
            create_run(
                &path,
                &format!("during-{i}"),
                "get",
                &"p".repeat(4000),
                None,
                None,
            )
            .await
            .unwrap();
        }
        {
            let (_db, conn) = connect(&path).await.unwrap();
            let mut rows = conn
                .query("PRAGMA wal_checkpoint(PASSIVE)", ())
                .await
                .unwrap();
            while rows.next().await.unwrap().is_some() {}
        }
        assert!(!wal_is_clean(&path), "the writes are in the log");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the main file did not change"
        );
        drop(pin);
        let _lock = lock_db(&path).await.unwrap();
        fold_log(&path).await.unwrap();
        assert_ne!(std::fs::read(&path).unwrap(), before);
        assert_eq!(open_and_count(&path, "runs").await, 301);
    }

    /// A backup leaves barca's lock while it hashes and copies: a run's writes go on beside it,
    /// and the copy is still one whole database whose digest is the one returned.
    #[tokio::test]
    async fn a_backup_copies_beside_writes_without_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        {
            // Some megabytes of history, so the copy lasts while the run below writes.
            let _lock = lock_db(&path).await.unwrap();
            let (_db, conn) = connect(&path).await.unwrap();
            conn.execute(
                "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i < 2000) \
                 INSERT INTO runs (run_id, command, files, status) \
                 SELECT 'old-' || i, 'get', printf('%.2000c', 'p'), 'complete' FROM n",
                (),
            )
            .await
            .unwrap();
        }
        let writer = {
            let path = path.clone();
            tokio::spawn(async move {
                for i in 0..200 {
                    create_run(&path, &format!("new-{i}"), "get", "p.py", None, None)
                        .await
                        .unwrap();
                }
            })
        };
        let copy = dir.path().join("backup.db");
        let digest = copy_for_backup(&path, &copy, None, barca_model::process::Until::done())
            .await
            .unwrap()
            .unwrap();
        writer.await.unwrap();
        assert_eq!(barca_model::hash::file_sha256(&copy).unwrap(), digest);
        let copied = open_and_count(&copy.display().to_string(), "runs").await;
        assert!((2000..=2200).contains(&copied), "{copied}");
        assert_eq!(open_and_count(&path, "runs").await, 2200);
        // Nothing is held after the backup: the lock is free at once.
        acquire_file_lock(&path, std::time::Duration::from_millis(100))
            .await
            .unwrap();
    }

    async fn open_and_count(db_path: &str, table: &str) -> u64 {
        let db = Builder::new_local(db_path).build().await.unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get_value(0).unwrap().as_integer().copied().unwrap() as u64
    }

    /// The load-bearing spike for metadata backups: after
    /// wal_checkpoint(TRUNCATE), the -wal sidecar must be empty/absent and
    /// all rows must be readable from the main file alone.
    #[tokio::test]
    async fn folding_the_log_leaves_every_row_in_the_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();

        // Keep an idle connection open so SQLite's last-close checkpoint cannot hide
        // whether fold_log actually folds a nonempty WAL.
        let held_db = Builder::new_local(&db_path).build().await.unwrap();
        let held = held_db.connect().unwrap();
        // Create a table and write enough rows that data definitely lives in the WAL.
        {
            let db = Builder::new_local(&db_path).build().await.unwrap();
            let conn = db.connect().unwrap();
            let mut mode = conn.query("PRAGMA journal_mode=WAL", ()).await.unwrap();
            while mode.next().await.unwrap().is_some() {}
            drop(mode);
            conn.execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)",
                (),
            )
            .await
            .unwrap();
            for i in 0..200 {
                conn.execute("INSERT INTO t (v) VALUES (?1)", [format!("value-{i}")])
                    .await
                    .unwrap();
            }
            let mut read = held.query("SELECT COUNT(*) FROM t", ()).await.unwrap();
            assert_eq!(
                read.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                200
            );
            while read.next().await.unwrap().is_some() {}
        }

        // Sanity: without a checkpoint the WAL holds the data.
        let wal = format!("{db_path}-wal");
        let wal_size_before = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert!(
            wal_size_before > 0,
            "expected data in the WAL before checkpoint (got {wal_size_before} bytes) — \
             the fixture must leave committed frames pending before fold_log"
        );

        super::fold_log(&db_path).await.unwrap();
        assert!(
            super::wal_is_clean(&db_path),
            "WAL must be empty after checkpoint"
        );

        // The main file alone (as a backup copies it: without the WAL)
        // must contain every row.
        let copy = dir.path().join("uploaded.db").to_string_lossy().to_string();
        std::fs::copy(&db_path, &copy).unwrap();
        assert_eq!(open_and_count(&copy, "t").await, 200);
    }
}
