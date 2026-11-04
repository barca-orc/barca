//! Database operations — schema init, output persistence, connection helpers.
//!
//! All functions are `async` and run on whatever runtime the caller provides —
//! this crate never constructs a runtime of its own.

use crate::BarcaError;
use crate::dispatch::OutputRef;
use libsql::Builder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, MutexGuard};

// The metadata write boundary: every INSERT, UPDATE, DELETE or REPLACE of the database is in
// this file, one of these owners (a table family each) or `persist/recording.rs`.
mod cache_reader;
pub(crate) mod cost_estimates;
pub(crate) mod freshness_proofs;
pub(crate) mod ledger_posts;
mod logs;
pub(crate) mod materializations;
pub(crate) mod notifications;
mod partition_keys;
mod run_liveness;
pub(crate) mod run_queue;
mod run_steps;
pub(crate) mod runs;
mod schedule;
mod schema;
pub(crate) mod sink_publications;
mod snapshots;
mod stats;
pub(crate) mod uploads;
#[cfg(test)]
mod write_boundary_tests;
pub use cache_reader::CacheReader;
pub(crate) use cost_estimates::cost_estimates_for;
use cost_estimates::cost_estimates_on;
pub use cost_estimates::{load_cost_estimates, upsert_cost_estimates};
pub use logs::{LogEntry, get_logs, insert_logs};
use materializations::record_definitions;
pub(crate) use materializations::{
    adopt_store_digests, confirmed_object, evicted_among, evicted_paths, key_range,
    last_success_components, latest_success, latest_success_id, partition_artifacts_for_config,
    touch_read, touch_read_in, touch_read_on,
};
pub use materializations::{
    last_output_hashes, max_materialization_id, persist_output_refs, persist_outputs,
    succeeded_since,
};
pub use partition_keys::{
    FailedKeys, PartitionErrorGroup, PartitionKeyRow, PartitionKeys, PartitionKeysFilter,
    failed_partition_keys, partition_keys,
};
pub use run_liveness::{AliveRun, alive_running_runs, alive_running_runs_in, local_host};
use run_liveness::{interrupted_runs, mark_interrupted_runs};
pub(crate) use run_liveness::{live_runs, pid_alive, runs_alive_here};
pub use run_steps::{
    OUTPUT_TAIL_LINES, RUN_STEPS_LIMIT, RunNodeSummary, RunStep, StepRow, StepsPage, get_run_steps,
    get_run_steps_page, planned_run_nodes, read_step_rows, run_node_summaries, worker_exit,
};
pub use runs::{
    NewRun, RunFailure, RunRecord, StartedRun, count_runs, create_run, create_run_with,
    decode_files, encode_files, finish_run, generate_run_id, get_recent_runs, get_run,
    get_run_outcome, reconcile_run, record_unstarted_run, replace_run_outcome, run_artifact,
    set_run_outcome, split_exception, start_run,
};
pub use schedule::{
    ScheduleFailures, SchedulePause, get_schedule_failures, get_schedule_pauses,
    get_schedule_state, set_schedule_failures, set_schedule_pause, upsert_schedule_state,
};
use schema::{has_column, has_index, initialized};
pub use schema::{init_db, init_db_as, probe_metadata};
pub(crate) use schema::{init_locked, init_schema};
pub(crate) use snapshots::copy_for_backup;
pub use snapshots::{DbSnapshot, ReadView};
pub use stats::{
    AssetRunEntry, AssetStats, get_asset_stats, get_avg_elapsed, get_avg_elapsed_for_partitioned,
    recent_durations,
};
use stats::{IN_LIST_CHUNK, KEY_ROWS, avg_elapsed_on, partitioned_avg_elapsed_on};

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
pub(crate) async fn prepare(
    conn: &libsql::Connection,
    sql: &str,
) -> Result<Prepared, libsql::Error> {
    #[cfg(test)]
    PREPARES.with(|n| n.set(n.get() + 1));
    conn.prepare(sql).await.map(Prepared)
}

/// A reusable statement always starts from its initial state. Keep this driver detail
/// here: libSQL leaves a completed statement at SQLITE_DONE until it is reset.
pub(crate) struct Prepared(libsql::Statement);
impl Prepared {
    pub(crate) async fn execute(
        &mut self,
        params: impl libsql::params::IntoParams,
    ) -> Result<u64, libsql::Error> {
        self.0.reset();
        self.0.execute(params).await.map(|n| n as u64)
    }
    pub(crate) async fn query(
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
pub(crate) async fn with_tx<T>(
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

#[cfg(test)]
thread_local! {
    static PREPARES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many statements [`prepare`] has prepared on this thread. Tests run on a current-thread
/// runtime, so this counts only their own writes.
#[cfg(test)]
pub(crate) fn prepares() -> u64 {
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

/// The paths of the [`ReadView`]s open in this process.
static READ_VIEWS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// True when `db_path` names a [`ReadView`].
pub fn is_read_view(db_path: &str) -> bool {
    READ_VIEWS.lock().unwrap().contains(db_path)
}

/// How long a connection waits on SQLite's own locks before `database is locked`
/// ([`wait_when_busy`]): in WAL mode only while the log's index is rebuilt or a checkpoint
/// truncates the log, which takes milliseconds.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// How long [`fold_log`] tries again a checkpoint that a [`ReadView`]'s query kept from
/// finishing: one query of a read route lasts milliseconds.
const CHECKPOINT_BUSY_WAIT: Duration = Duration::from_secs(2);

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
    crate::db_schema::check(&conn)
        .await
        .map_err(|e| unusable_at(e, path))?;
    Ok((db, conn))
}

/// Local filesystem locations for one environment.
#[derive(Debug, Clone)]
pub struct LocalPaths {
    pub db_path: String,
    pub artifact_dir: String,
}

/// Path derivation for an environment, with no filesystem side effects.
/// The default env keeps the legacy layout so existing projects need no
/// migration; named envs live under `.barca/envs/<name>/`.
pub fn env_local_paths(env: &str) -> LocalPaths {
    let base = if env == crate::config::DEFAULT_ENV {
        PathBuf::from(".barca")
    } else {
        PathBuf::from(".barca").join("envs").join(env)
    };
    LocalPaths {
        db_path: base.join("metadata.db").to_string_lossy().to_string(),
        artifact_dir: base.join("artifacts").to_string_lossy().to_string(),
    }
}

/// Create the `.barca` tree for an environment and return its paths.
pub fn ensure_env_dirs(env: &str) -> Result<LocalPaths, BarcaError> {
    let paths = env_local_paths(env);
    let db_dir = Path::new(&paths.db_path)
        .parent()
        .expect("db path has a parent")
        .to_path_buf();
    fs::create_dir_all(&db_dir)
        .map_err(|e| BarcaError::Db(format!("failed to create {}: {e}", db_dir.display())))?;
    fs::create_dir_all(&paths.artifact_dir)
        .map_err(|e| BarcaError::Db(format!("failed to create artifacts dir: {e}")))?;
    let gitignore = PathBuf::from(".barca").join(".gitignore");
    if !gitignore.exists() {
        let _ = fs::write(&gitignore, "*\n");
    }
    Ok(paths)
}

/// How long one barca process waits for another's DB operation before giving up.
/// Operations are brief (a few queries), so hitting this means a stuck process.
const DB_FILE_LOCK_WAIT: Duration = Duration::from_secs(60);

/// An open database plus the cross-process lock that makes it safe to open. Field
/// order matters: the database is closed before the lock is released.
pub struct DbHandle {
    _db: libsql::Database,
    /// None for a [`ReadView`], which reads without the lock.
    _lock: Option<fs::File>,
}

/// Take the cross-process lock for `db_path` (a `<db>.lock` file next to it), waiting
/// up to `wait` for another barca process to finish its operation. The lock is
/// released when the returned file is dropped (or the process exits or crashes).
async fn acquire_file_lock(db_path: &str, wait: Duration) -> Result<fs::File, BarcaError> {
    let lock_path = format!("{db_path}.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| BarcaError::Db(format!("failed to open lock file {lock_path}: {e}")))?;
    let deadline = Instant::now() + wait;
    let mut delay = Duration::from_millis(2);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(e)) => {
                return Err(BarcaError::Db(format!("failed to lock {lock_path}: {e}")));
            }
        }
        if Instant::now() >= deadline {
            return Err(BarcaError::Db(format!(
                "timed out after {:.1}s waiting for {lock_path}: another barca process is \
                 holding the metadata DB (a very long operation, or a stuck process)",
                wait.as_secs_f32()
            )));
        }
        // Back off with a little jitter so waiters don't wake in lockstep.
        let jitter = Duration::from_micros(u64::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() % 2000)
                .unwrap_or(0),
        ));
        tokio::time::sleep(delay + jitter).await;
        delay = (delay * 2).min(Duration::from_millis(40));
    }
}

/// The in-process guard and the cross-process lock of the database at `db_path`, for an
/// operation that must see no other barca process touch the file. Held until dropped.
pub(crate) struct DbLock {
    _lock: fs::File,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) async fn lock_db(db_path: &str) -> Result<DbLock, BarcaError> {
    lock_db_with_until(db_path, crate::helper_proc::Until::done()).await
}

/// Cancellation applies only to lock admission. The caller must finish any admitted
/// atomic database mutation before releasing this lock.
pub(crate) async fn lock_db_with_until(
    db_path: &str,
    until: crate::helper_proc::Until<'_>,
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

/// The SHA-256 of the file at `path`, as lowercase hex.
pub(crate) fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Checkpoint the database at `db_path` and verify that its main file is now the whole
/// database. The caller holds the database's lock.
pub(crate) async fn fold_log(db_path: &str) -> Result<(), BarcaError> {
    {
        let (_db, conn) = connect(db_path).await?;
        // The lock keeps every writer out, not a [`ReadView`]: a reader in the middle of a
        // query holds the log until the query ends, so a busy checkpoint (and only that) is
        // tried again for a moment.
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
pub(crate) fn remove_sidecars(path: &str) -> Result<(), BarcaError> {
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
    crate::db_schema::check(&conn)
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
    crate::db_schema::check(&conn)
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
pub(crate) fn empty_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0) && wal_is_clean(path)
}

/// Open the database at `db_path` and connect. Callers must hold [`db_guard`]
/// for the duration of their work on the returned connection; the returned
/// handle holds the cross-process lock until it is dropped.
pub(crate) async fn open_conn(db_path: &str) -> Result<(DbHandle, libsql::Connection), BarcaError> {
    open_conn_with_until(db_path, crate::helper_proc::Until::done()).await
}

pub(crate) async fn open_conn_with_until(
    db_path: &str,
    until: crate::helper_proc::Until<'_>,
) -> Result<(DbHandle, libsql::Connection), BarcaError> {
    if is_read_view(db_path) {
        until.check()?;
        let (db, conn) = connect_view(db_path).await?;
        return Ok((
            DbHandle {
                _db: db,
                _lock: None,
            },
            conn,
        ));
    }
    let lock = until
        .admit(acquire_file_lock(db_path, DB_FILE_LOCK_WAIT))
        .await??;
    until.check()?;
    let (db, conn) = connect(db_path).await?;
    Ok((
        DbHandle {
            _db: db,
            _lock: Some(lock),
        },
        conn,
    ))
}

/// Fold the write-ahead log into the main database file (`PRAGMA wal_checkpoint(TRUNCATE)`).
/// No other connection may be open on the file.
pub(crate) async fn checkpoint(conn: &libsql::Connection) -> Result<(), BarcaError> {
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
pub(crate) async fn connect_locked(
    db_path: &str,
) -> Option<(libsql::Database, libsql::Connection)> {
    connect(db_path).await.ok()
}

/// One row of `materializations`, as `barca status` reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializationRecord {
    pub content_hash: Option<String>,
    /// Exact node id, including the partition suffix for a partitioned step.
    pub node_id: String,
    pub run_hash: Option<String>,
    pub artifact_path: Option<String>,
    pub artifact_format: Option<String>,
    pub artifact_size_bytes: Option<i64>,
    pub elapsed_seconds: Option<f64>,
    /// `success` or `failed`.
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: String,
}

/// The last observed change in bytes for unchanged code and inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BytesChange {
    pub node_id: String,
    pub run_hash: String,
    pub content_hash: String,
    pub created_at: String,
}

/// Nodes whose materialization in this run differed from the preceding digest.
pub(crate) async fn changed_bytes_in_run(
    db_path: &str,
    run_id: &str,
) -> Result<Vec<String>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn.query(
        "SELECT node_id FROM materializations INDEXED BY idx_mat_run WHERE run_id = ?1 AND bytes_changed = 1 ORDER BY node_id",
        [run_id],
    ).await.map_err(|e| BarcaError::Db(format!("failed to read byte changes: {e}")))?;
    let mut nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read byte changes: {e}")))?
    {
        nodes.push(
            row.get::<String>(0)
                .map_err(|e| BarcaError::Db(format!("failed to read node id: {e}")))?,
        );
    }
    Ok(nodes)
}

/// How late a node is, from its materialization rows (keys of a partitioned node count toward
/// their base id). Timestamps are as recorded: UTC `YYYY-MM-DD HH:MM:SS`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeAge {
    /// When its newest successful row was written.
    pub last_success_at: Option<String>,
    /// The first failed attempt after its last clean run: `None` unless its latest attempts failed.
    pub failing_since: Option<String>,
    /// How many runs in a row failed it, up to now. A run counts once however many keys failed.
    pub consecutive_failures: u32,
    /// Its newest run as a whole; `None` until a run recorded a row for it.
    pub last_run: Option<RunTotals>,
}

/// One run's rows for one node, keys folded into their base id: what the run cost on the node
/// as a whole, where `elapsed_seconds` of a key's row is that key's share alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunTotals {
    pub run_id: String,
    /// Wall time summed over the rows that recorded one; `None` when none did (every key
    /// failed before finishing).
    pub elapsed_seconds: Option<f64>,
    /// Rows the run recorded for the node: its keys, or 1.
    pub keys: u32,
    /// Batched calls (RFC-0013) those keys ran in; 0 when each ran on its own.
    pub calls: u32,
    pub failed: u32,
}

/// The base id of a materialization row's node id (`<base>[k=v]` for a key).
fn base_of(node_id: &str) -> &str {
    node_id.split_once('[').map_or(node_id, |(base, _)| base)
}

/// [`NodeAge`] of every node with history. One read of `idx_mat_failed`, then one pass over the
/// successful rows: an attempt is a run, and a run with a failed key is a failed attempt even
/// when other keys succeeded. Rows without a run id (an older barca's) are each their own attempt.
/// Read-only; the caller ensures the DB exists.
pub async fn node_ages(db_path: &str) -> Result<HashMap<String, NodeAge>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_h, conn) = open_conn(db_path).await?;
    let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
    let attempt = |run: Option<String>, id: i64| run.unwrap_or_else(|| format!("#{id}"));
    // base id -> run -> (its newest failed row, its earliest failure's time).
    let mut failed: HashMap<String, HashMap<String, (i64, String)>> = HashMap::new();
    // base id -> (its newest row, that row's run).
    let mut newest: HashMap<String, (i64, Option<String>)> = HashMap::new();
    let mut note_newest = |base: &str, id: i64, run: Option<String>| {
        let entry = newest.entry(base.to_string()).or_insert((id, run.clone()));
        if id >= entry.0 {
            *entry = (id, run);
        }
    };
    let mut rows = conn
        .query(
            "SELECT id, node_id, run_id, created_at FROM materializations \
             WHERE status = 'failed' ORDER BY run_id, id",
            (),
        )
        .await
        .map_err(failure)?;
    while let Some(row) = rows.next().await.map_err(failure)? {
        let id = row.get::<i64>(0).unwrap_or_default();
        let node = row.get::<String>(1).unwrap_or_default();
        let recorded = row.get::<String>(2).ok().filter(|r| !r.is_empty());
        note_newest(base_of(&node), id, recorded.clone());
        let run = attempt(recorded, id);
        let at = row.get::<String>(3).unwrap_or_default();
        failed
            .entry(base_of(&node).to_string())
            .or_default()
            .entry(run)
            .and_modify(|seen| seen.0 = seen.0.max(id))
            .or_insert((id, at));
    }
    drop(rows);
    // base id -> (newest successful row and its time, newest row of a run that did not fail it).
    let mut success: HashMap<String, (i64, String, i64)> = HashMap::new();
    let mut rows = conn
        .query(
            "SELECT id, node_id, run_id, created_at FROM materializations \
             WHERE status = 'success'",
            (),
        )
        .await
        .map_err(failure)?;
    while let Some(row) = rows.next().await.map_err(failure)? {
        let id = row.get::<i64>(0).unwrap_or_default();
        let node = row.get::<String>(1).unwrap_or_default();
        let base = base_of(&node);
        let run = row.get::<String>(2).ok().filter(|r| !r.is_empty());
        note_newest(base, id, run.clone());
        let clean = failed
            .get(base)
            .is_none_or(|runs| run.is_none_or(|r| !runs.contains_key(&r)));
        let entry = match success.get_mut(base) {
            Some(entry) => entry,
            None => success.entry(base.to_string()).or_default(),
        };
        if id > entry.0 {
            entry.0 = id;
            entry.1 = row.get::<String>(3).unwrap_or_default();
        }
        if clean {
            entry.2 = entry.2.max(id);
        }
    }
    let mut out: HashMap<String, NodeAge> = HashMap::with_capacity(success.len());
    for (base, runs) in failed {
        let clean = success.get(&base).map_or(0, |s| s.2);
        let trailing = runs.values().filter(|(newest, _)| *newest > clean);
        out.insert(
            base,
            NodeAge {
                last_success_at: None,
                failing_since: trailing.clone().map(|(_, at)| at.clone()).min(),
                consecutive_failures: trailing.count() as u32,
                last_run: None,
            },
        );
    }
    for (base, (_, at, _)) in success {
        out.entry(base).or_default().last_success_at = Some(at);
    }
    // The newest run's rows, one index seek per node (`idx_mat_run`). A run requeued onto new
    // code can hold two rows for a key, one per run hash (#794), so keys are counted distinct.
    // Rows without a run id (an older barca's) leave it unknown.
    for (base, (_, run)) in newest {
        let Some(run) = run else { continue };
        let (lo, hi) = key_range(&base);
        let mut rows = conn
            .query(
                "SELECT sum(elapsed_seconds), count(DISTINCT node_id), count(DISTINCT batch_id), \
                 sum(status = 'failed') FROM materializations INDEXED BY idx_mat_run \
                 WHERE run_id = ?1 AND (node_id = ?2 OR (node_id >= ?3 AND node_id < ?4))",
                libsql::params![run.clone(), base.clone(), lo, hi],
            )
            .await
            .map_err(failure)?;
        if let Some(row) = rows.next().await.map_err(failure)? {
            out.entry(base).or_default().last_run = Some(RunTotals {
                run_id: run,
                elapsed_seconds: row.get::<f64>(0).ok(),
                keys: row.get::<i64>(1).unwrap_or_default() as u32,
                calls: row.get::<i64>(2).unwrap_or_default() as u32,
                failed: row.get::<i64>(3).unwrap_or_default() as u32,
            });
        }
    }
    Ok(out)
}

/// What the metadata DB knows about one node (all partition keys of a partitioned node).
#[derive(Debug, Clone, Default)]
pub struct NodeHistory {
    pub last_bytes_change: Option<BytesChange>,
    /// The most recent materialization attempt, successful or not.
    pub latest: Option<MaterializationRecord>,
    /// Whether any attempt ever succeeded.
    pub ever_succeeded: bool,
}

/// Latest materialization and success flag for each base node id. Rows of partitioned steps
/// (`<base>[k=v]`) count toward their base id. Read-only; the caller ensures the DB exists.
pub async fn node_histories(
    db_path: &str,
    base_ids: &[String],
) -> Result<HashMap<String, NodeHistory>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut out = HashMap::new();
    for base in base_ids {
        out.insert(base.clone(), node_history(&conn, base).await?);
    }
    Ok(out)
}

/// [`node_histories`] of one node. Each question is asked of the node's own rows and of its keys'
/// rows (`base[...]`, a range of `idx_mat_node_run`), so every query is an index seek or an
/// index-only scan. A `substr(node_id, ...)` filter cannot use the index: it scanned the whole
/// table four times per node, minutes for a project with a 200k-key node.
async fn node_history(conn: &libsql::Connection, base: &str) -> Result<NodeHistory, BarcaError> {
    let (lo, hi) = key_range(base);
    let exact = ("node_id = ?1", vec![libsql::Value::Text(base.to_string())]);
    let keys = (
        "node_id >= ?1 AND node_id < ?2",
        vec![libsql::Value::Text(lo), libsql::Value::Text(hi)],
    );
    let mut h = NodeHistory::default();
    let mut latest: Option<i64> = None;
    let mut bytes: Option<i64> = None;
    for (filter, params) in [&exact, &keys] {
        let id = query_i64(
            conn,
            &format!(
                "SELECT max(id) FROM materializations INDEXED BY idx_mat_node_run WHERE {filter}"
            ),
            params.clone(),
        )
        .await?;
        latest = latest.max(id);
        if !h.ever_succeeded {
            h.ever_succeeded = query_i64(
                conn,
                &format!(
                    "SELECT 1 FROM materializations INDEXED BY idx_mat_node_run \
                     WHERE {filter} AND status = 'success' LIMIT 1"
                ),
                params.clone(),
            )
            .await?
            .is_some();
        }
        // `idx_mat_changed` holds only the rows whose bytes changed.
        let changed = query_i64(
            conn,
            &format!("SELECT max(id) FROM materializations WHERE bytes_changed = 1 AND {filter}"),
            params.clone(),
        )
        .await?;
        bytes = bytes.max(changed);
    }
    // The last materialization is the latest run's (by row id). A partitioned node writes one
    // row per key in completion order, so within that run the first key in sort order is
    // reported, not whichever finished last (#475). Rows without a run id (an older barca's)
    // keep the latest row.
    let latest = match latest {
        Some(id) => Some(first_key_of_latest_run(conn, id, &exact, &keys).await?),
        None => None,
    };
    if let Some(id) = latest {
        let mut rows = conn
            .query(
                "SELECT node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
                 elapsed_seconds, status, error_message, created_at, output_hash \
                 FROM materializations WHERE id = ?1",
                [id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            let opt = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
            h.latest = Some(MaterializationRecord {
                content_hash: opt(9),
                node_id: row.get::<String>(0).unwrap_or_default(),
                run_hash: opt(1),
                artifact_path: opt(2),
                artifact_format: opt(3),
                artifact_size_bytes: row.get::<i64>(4).ok(),
                elapsed_seconds: row.get::<f64>(5).ok(),
                status: opt(6).unwrap_or_else(|| "success".to_string()),
                error_message: opt(7),
                created_at: row.get::<String>(8).unwrap_or_default(),
            });
        }
    }
    if let Some(id) = bytes {
        let mut rows = conn
            .query(
                "SELECT node_id, run_hash, output_hash, created_at FROM materializations \
                 WHERE id = ?1",
                [id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query byte change: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read byte change: {e}")))?
        {
            h.last_bytes_change = Some(BytesChange {
                node_id: row.get::<String>(0).unwrap_or_default(),
                run_hash: row.get::<String>(1).unwrap_or_default(),
                content_hash: row.get::<String>(2).unwrap_or_default(),
                created_at: row.get::<String>(3).unwrap_or_default(),
            });
        }
    }
    Ok(h)
}

/// The row [`node_history`] reports for a node whose latest row is `latest`: the row of the
/// first node id in sort order (the base id, then its keys) in that row's run, that node's
/// latest in the run. `latest` itself when it has no run id.
async fn first_key_of_latest_run(
    conn: &libsql::Connection,
    latest: i64,
    exact: &(&str, Vec<libsql::Value>),
    keys: &(&str, Vec<libsql::Value>),
) -> Result<i64, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT run_id FROM materializations WHERE id = ?1",
            [latest],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    let run = match rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        Some(row) => row.get::<String>(0).ok().filter(|s| !s.is_empty()),
        None => None,
    };
    drop(rows);
    let Some(run) = run else {
        return Ok(latest);
    };
    let with_run = |(filter, params): &(&str, Vec<libsql::Value>)| {
        let mut params = params.clone();
        params.push(libsql::Value::Text(run.clone()));
        let n = params.len();
        (format!("{filter} AND run_id = ?{n}"), params)
    };
    // The base id sorts before every key of it.
    let (filter, params) = with_run(exact);
    if let Some(id) = query_i64(
        conn,
        &format!("SELECT max(id) FROM materializations INDEXED BY idx_mat_node_run WHERE {filter}"),
        params,
    )
    .await?
    {
        return Ok(id);
    }
    // The first key in the run, walking the index in key order.
    let (filter, params) = with_run(keys);
    let mut rows = conn
        .query(
            &format!(
                "SELECT node_id FROM materializations INDEXED BY idx_mat_node_run WHERE {filter} \
                 ORDER BY node_id LIMIT 1"
            ),
            params,
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    let first = match rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        Some(row) => row.get::<String>(0).ok(),
        None => None,
    };
    drop(rows);
    let Some(first) = first else {
        return Ok(latest);
    };
    Ok(query_i64(
        conn,
        "SELECT max(id) FROM materializations INDEXED BY idx_mat_node_run \
         WHERE node_id = ?1 AND run_id = ?2",
        vec![libsql::Value::Text(first), libsql::Value::Text(run)],
    )
    .await?
    .unwrap_or(latest))
}

/// The first column of the first row as an integer; `None` for no row or a NULL.
async fn query_i64(
    conn: &libsql::Connection,
    sql: &str,
    params: Vec<libsql::Value>,
) -> Result<Option<i64>, BarcaError> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    Ok(rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        .and_then(|row| row.get::<i64>(0).ok()))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn with_tx_commits_a_body_that_succeeds_and_rolls_back_one_that_fails() {
        use crate::BarcaError;
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

    #[tokio::test]
    async fn last_materialization_of_a_partitioned_node_does_not_depend_on_completion_order() {
        let mut picked = Vec::new();
        for order in [["b", "a"], ["a", "b"]] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("metadata.db").display().to_string();
            super::init_db(&path).await.unwrap();
            {
                let _guard = super::db_guard().await;
                let (_db, conn) = super::open_conn(&path).await.unwrap();
                let mut rows = vec![("r0", "p.py:t[region=0]".to_string())];
                rows.extend(order.iter().map(|k| ("r1", format!("p.py:t[region={k}]"))));
                for (run, node) in rows {
                    conn.execute(
                        "INSERT INTO materializations (run_id, node_id, run_hash, status) VALUES (?1, ?2, 'h', 'success')",
                        [run, node.as_str()],
                    )
                    .await
                    .unwrap();
                }
            }
            let ids = vec!["p.py:t".to_string()];
            let first = super::node_histories(&path, &ids).await.unwrap();
            let again = super::node_histories(&path, &ids).await.unwrap();
            let node = |h: &HashMap<String, super::NodeHistory>| {
                h["p.py:t"].latest.as_ref().unwrap().node_id.clone()
            };
            assert_eq!(node(&first), node(&again));
            picked.push(node(&first));
        }
        assert_eq!(picked, ["p.py:t[region=a]", "p.py:t[region=a]"]);

        // Rows an older barca wrote without a run id: the latest row, as before.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for node in ["p.py:t[region=a]", "p.py:t[region=b]"] {
                conn.execute(
                    "INSERT INTO materializations (node_id, run_hash) VALUES (?1, 'h')",
                    [node],
                )
                .await
                .unwrap();
            }
        }
        let h = super::node_histories(&path, &["p.py:t".to_string()])
            .await
            .unwrap();
        assert_eq!(
            h["p.py:t"].latest.as_ref().unwrap().node_id,
            "p.py:t[region=b]"
        );
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

        drop(first);
        acquire_file_lock(&db_path, std::time::Duration::from_secs(1))
            .await
            .expect("lock is free again once the holder drops it");
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
mod fold_log_tests {
    use libsql::Builder;

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

#[cfg(test)]
mod failure_index_tests {
    use super::*;

    /// Age is counted in runs: a run with a failed key is one failed attempt, a clean run ends
    /// the streak, and a key's row counts toward its base node.
    #[tokio::test]
    async fn a_nodes_age_counts_failed_runs_since_its_last_clean_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            for (run, node, status, at) in [
                ("r1", "p.py:recovered", "failed", "2026-01-01 00:00:00"),
                ("r2", "p.py:recovered", "success", "2026-01-02 00:00:00"),
                ("r1", "p.py:failing", "success", "2026-01-01 00:00:00"),
                ("r2", "p.py:failing", "failed", "2026-01-02 00:00:00"),
                ("r3", "p.py:failing", "failed", "2026-01-03 00:00:00"),
                ("r1", "p.py:keys[d=1]", "success", "2026-01-01 00:00:00"),
                ("r1", "p.py:keys[d=2]", "success", "2026-01-01 00:00:00"),
                ("r2", "p.py:keys[d=1]", "failed", "2026-01-02 00:00:00"),
                ("r2", "p.py:keys[d=2]", "success", "2026-01-02 12:00:00"),
                ("r2", "p.py:keys[d=3]", "failed", "2026-01-02 13:00:00"),
                ("r1", "p.py:never", "failed", "2026-01-01 00:00:00"),
            ] {
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, status, created_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    [run, node, status, at],
                )
                .await
                .unwrap();
            }
        }
        let ages = node_ages(&path).await.unwrap();
        let age = |id: &str, ok: Option<&str>, since: Option<&str>, n: u32| {
            assert_eq!(
                NodeAge {
                    last_run: None,
                    ..ages[id].clone()
                },
                NodeAge {
                    last_success_at: ok.map(str::to_string),
                    failing_since: since.map(str::to_string),
                    consecutive_failures: n,
                    last_run: None,
                },
                "{id}"
            );
        };
        age("p.py:recovered", Some("2026-01-02 00:00:00"), None, 0);
        age(
            "p.py:failing",
            Some("2026-01-01 00:00:00"),
            Some("2026-01-02 00:00:00"),
            2,
        );
        age(
            "p.py:keys",
            Some("2026-01-02 12:00:00"),
            Some("2026-01-02 00:00:00"),
            1,
        );
        age("p.py:never", None, Some("2026-01-01 00:00:00"), 1);
        // The newest run as a whole: its keys, how many failed, and (none recorded a time) no
        // duration rather than zero.
        assert_eq!(
            ages["p.py:keys"].last_run,
            Some(RunTotals {
                run_id: "r2".into(),
                elapsed_seconds: None,
                keys: 3,
                calls: 0,
                failed: 2,
            })
        );
        assert_eq!(
            ages["p.py:recovered"].last_run.as_ref().unwrap().run_id,
            "r2"
        );
    }

    /// A batched node's last run costs what its keys' rows add up to, over the calls they ran in.
    #[tokio::test]
    async fn a_nodes_last_run_sums_its_keys_over_their_batched_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            for (run, node, elapsed, batch) in [
                ("r1", "p.py:keys[d=1]", 9.0, "old"),
                ("r2", "p.py:keys[d=1]", 0.5, "b1"),
                ("r2", "p.py:keys[d=2]", 0.25, "b1"),
                ("r2", "p.py:keys[d=3]", 1.0, "b2"),
                ("r2", "p.py:plain", 2.0, ""),
            ] {
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, status, elapsed_seconds, \
                     batch_id) VALUES (?1, ?2, 'success', ?3, NULLIF(?4, ''))",
                    libsql::params![run, node, elapsed, batch],
                )
                .await
                .unwrap();
            }
        }
        let ages = node_ages(&path).await.unwrap();
        assert_eq!(
            ages["p.py:keys"].last_run,
            Some(RunTotals {
                run_id: "r2".into(),
                elapsed_seconds: Some(1.75),
                keys: 3,
                calls: 2,
                failed: 0,
            })
        );
        assert_eq!(
            ages["p.py:plain"].last_run,
            Some(RunTotals {
                run_id: "r2".into(),
                elapsed_seconds: Some(2.0),
                keys: 1,
                calls: 0,
                failed: 0,
            })
        );
    }
}
