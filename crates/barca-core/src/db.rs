//! Database operations — schema init, output persistence, connection helpers.
//!
//! All functions are `async` and run on whatever runtime the caller provides —
//! this crate never constructs a runtime of its own.

use crate::BarcaError;
use crate::dispatch::OutputRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, MutexGuard};
use turso::Builder;

/// Process-wide serialization of `metadata.db` operations. The server can run
/// multiple pipelines in parallel (they execute Python concurrently), but their
/// brief reads/writes to the shared SQLite file must not overlap. Every DB
/// helper — and the inline cache-check/persist ops in `execution::execute` — holds
/// this guard for the duration of its (short) database work, so runs never race
/// on the file without depending on WAL support. A one-shot CLI run leaves it
/// uncontended.
///
/// This guard only covers one process. Turso's default mode is single-process: it
/// takes a non-blocking exclusive lock on the DB file, so a second *process*
/// that overlaps used to fail with "File is locked by another process". Each
/// open therefore also holds a short cross-process lock (see
/// [`acquire_file_lock`]) so concurrent barca processes queue instead of fail.
/// (Turso's `experimental_multiprocess_wal` would allow true concurrent access,
/// but it is experimental and unsupported on some filesystems.)
///
/// Known limit: this serializes every DB op and each helper opens a fresh
/// connection, which becomes the contention point under many concurrent daemon
/// runs. The Engine extraction (#80) replaces this with a single owner holding
/// a persistent connection.
static DB_LOCK: Mutex<()> = Mutex::const_new(());

/// Prepare `sql` once for a loop that runs it per row. Turso parses and plans the SQL of every
/// `Connection::execute` again, which dominated recording thousands of rows (#460); a prepared
/// statement is parsed once and rebound for each row. Every per-row write statement goes
/// through here, so tests can count preparations ([`prepares`]).
pub(crate) async fn prepare(
    conn: &turso::Connection,
    sql: &str,
) -> Result<turso::Statement, turso::Error> {
    #[cfg(test)]
    PREPARES.with(|n| n.set(n.get() + 1));
    conn.prepare(sql).await
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

/// The `runs.files` column: a JSON array of paths. Rows written before 0.12 hold the paths
/// joined by spaces; those are split on whitespace.
pub fn encode_files(files: &[String]) -> String {
    serde_json::to_string(files).unwrap_or_default()
}

/// See [`encode_files`].
pub fn decode_files(raw: &str) -> Vec<String> {
    if raw.trim_start().starts_with('[')
        && let Ok(files) = serde_json::from_str::<Vec<String>>(raw)
    {
        return files;
    }
    raw.split_whitespace().map(str::to_string).collect()
}

/// Record of a single run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub command: String,
    /// The `.py` files the run was given.
    pub files: Vec<String>,
    pub target: Option<String>,
    pub status: String,
    pub steps_total: Option<i64>,
    pub steps_executed: i64,
    pub steps_cached: i64,
    /// Total recorded materialization attempts; cached steps contribute zero.
    #[serde(default)]
    pub attempts: Option<i64>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    /// What started the run: `manual` (a command typed on a machine), `api`, `schedule` or
    /// `client`. `null` for runs recorded before barca recorded it.
    #[serde(default)]
    pub triggered_by: Option<crate::outcome::TriggeredBy>,
    /// For a scheduled run: the tick it was started for (RFC 3339, UTC).
    #[serde(default)]
    pub scheduled_at: Option<String>,
    /// For a scheduled run: the schedule's id (the scheduled node).
    #[serde(default)]
    pub schedule_id: Option<String>,
    /// For a run a laptop posted to a server's ledger (`triggered_by: client`): the run id it
    /// had on that laptop. `null` for every other run.
    #[serde(default)]
    pub client_run_id: Option<String>,
}

/// Aggregated statistics for a single asset node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetStats {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_runs: i64,
    pub avg_elapsed_seconds: Option<f64>,
    pub median_elapsed_seconds: Option<f64>,
    pub max_elapsed_seconds: Option<f64>,
    pub p95_elapsed_seconds: Option<f64>,
    pub cache_hit_rate: f64,
    pub recent_runs: Vec<AssetRunEntry>,
}

/// One materialization entry for asset stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetRunEntry {
    pub elapsed_seconds: Option<f64>,
    pub status: String,
    pub created_at: String,
    /// Error message for `status='failed'` rows (None for successes).
    pub error_message: Option<String>,
    /// Number of attempts made.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub attempts: i64,
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

pub fn ensure_db_dir() -> Result<String, BarcaError> {
    Ok(ensure_env_dirs(crate::config::DEFAULT_ENV)?.db_path)
}

/// How long one barca process waits for another's DB operation before giving up.
/// Operations are brief (a few queries), so hitting this means a stuck process.
const DB_FILE_LOCK_WAIT: Duration = Duration::from_secs(60);

/// An open database plus the cross-process lock that makes it safe to open. Field
/// order matters: the database is closed before the lock is released.
pub struct DbHandle {
    _db: turso::Database,
    _lock: fs::File,
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

/// Fold the local database's write-ahead log into its main file and, unless that file's
/// SHA-256 is `unless`, copy it to `copy`; all under the database's lock. The copy is then one
/// consistent database, whatever other barca processes do to the database meanwhile: a running
/// write never produces a torn backup (RFC-0009). Returns the digest of what was copied, or
/// `None` when the database is still the one `unless` names and nothing was copied. A copy cut
/// short (cancelled, failed) is removed.
pub(crate) async fn copy_for_backup(
    db_path: &str,
    copy: &Path,
    unless: Option<&str>,
    until: crate::helper_proc::Until<'_>,
) -> Result<Option<String>, BarcaError> {
    let _lock = lock_db_with_until(db_path, until).await?;
    // Finish the admitted WAL checkpoint safely, then observe cancellation again.
    fold_log(db_path).await?;
    until.check()?;
    let digest = file_sha256(Path::new(db_path))
        .map_err(|e| BarcaError::Db(format!("failed to read {db_path} for a backup: {e}")))?;
    if unless == Some(digest.as_str()) {
        return Ok(None);
    }
    let copied = copy_snapshot_bytes(db_path, copy, until).await;
    if copied.is_err() {
        let _ = fs::remove_file(copy);
    }
    copied.map(|()| Some(digest))
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

async fn copy_snapshot_bytes(
    db_path: &str,
    target_path: &Path,
    until: crate::helper_proc::Until<'_>,
) -> Result<(), BarcaError> {
    use std::io::{Read, Write};
    let failed = |e| {
        BarcaError::Db(format!(
            "failed to copy {db_path} to {} for a backup: {e}",
            target_path.display(),
        ))
    };
    let mut source = fs::File::open(db_path).map_err(failed)?;
    let mut target = fs::File::create(target_path).map_err(failed)?;
    target
        .set_permissions(source.metadata().map_err(failed)?.permissions())
        .map_err(failed)?;
    let mut bytes = [0u8; 64 * 1024];
    loop {
        until.check()?;
        let count = source.read(&mut bytes).map_err(failed)?;
        if count == 0 {
            break;
        }
        target.write_all(&bytes[..count]).map_err(failed)?;
        // No background filesystem worker may outlive the lock or partial-copy cleanup.
        tokio::task::yield_now().await;
    }
    until.check()?;
    Ok(())
}

/// Checkpoint the database at `db_path` and verify that its main file is now the whole
/// database. The caller holds the database's lock.
pub(crate) async fn fold_log(db_path: &str) -> Result<(), BarcaError> {
    {
        let (_db, conn) = connect(db_path).await?;
        checkpoint(&conn).await?;
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
    use std::io::Read;
    let len = fs::metadata(path).ok()?.len();
    if len == 0 {
        return None;
    }
    let mut header = [0u8; 100];
    let mut file = fs::File::open(path).ok()?;
    if len < header.len() as u64 {
        return Some(format!("{len} bytes: too short to be a database"));
    }
    file.read_exact(&mut header).ok()?;
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

/// Wrap a Turso open failure, adding a hint when the DB is locked by something that
/// does not follow barca's own locking.
fn db_open_error(detail: impl std::fmt::Display) -> BarcaError {
    let detail = detail.to_string();
    let mut msg = format!("failed to open DB: {detail}");
    if detail.contains("locked by another process") {
        msg.push_str(
            "\n\nSomething outside barca's own locking has the metadata DB open (a DB \
             browser, a backup tool, or a barca process from an older version). Close it \
             and retry.",
        );
    }
    BarcaError::Db(msg)
}

/// A short-lived handle for one phase's cache lookups. It holds the in-process guard
/// *and* the cross-process lock for as long as it lives, so open it for the lookups
/// and drop it before any step runs: another barca process can then use the DB while
/// this run executes Python. (Fields drop in order: connection, database + file lock,
/// then the in-process guard.)
pub struct CacheReader {
    local_artifact_dir: Option<String>,
    /// A server's ledger, which answers cache lookups in place of this database.
    ledger: Option<crate::ledger::Ledger>,
    conn: turso::Connection,
    _handle: DbHandle,
    _guard: MutexGuard<'static, ()>,
}

impl CacheReader {
    pub async fn open(db_path: &str) -> Result<Self, BarcaError> {
        // Same order as every other helper: in-process guard first, then the file lock.
        let guard = db_guard().await;
        let (handle, conn) = open_conn(db_path).await?;
        Ok(Self {
            local_artifact_dir: None,
            ledger: None,
            conn,
            _handle: handle,
            _guard: guard,
        })
    }

    pub(crate) async fn for_config(
        cfg: &crate::config::ResolvedConfig,
    ) -> Result<Self, BarcaError> {
        let mut cache = Self::open(&cfg.db_path).await?;
        cache.local_artifact_dir = cfg.remote_off.then(|| cfg.local_artifact_dir.clone());
        cache.ledger = cfg.ledger.clone();
        Ok(cache)
    }

    /// The server's ledger, when this command runs against one.
    pub(crate) fn ledger(&self) -> Option<&crate::ledger::Ledger> {
        self.ledger.as_ref()
    }

    pub(crate) fn allows_artifact(&self, path: &str) -> bool {
        self.local_artifact_dir
            .as_deref()
            .is_none_or(|root| crate::config::local_artifact(path, root))
    }

    pub fn conn(&self) -> &turso::Connection {
        &self.conn
    }
}

/// How many times this process has opened a metadata DB ([`connect`]). Each open costs a
/// fraction of a millisecond plus the lock; `BARCA_TRACE_TIMING` prints the count at the end
/// of a run, and a test holds a warm run's count down (#560).
static OPENS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// [`OPENS`] so far.
pub fn opens() -> usize {
    OPENS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Open the database file at `path` and connect, taking no lock. For callers that already
/// hold the cross-process lock for the database this file is (or is about to become).
async fn connect(path: &str) -> Result<(turso::Database, turso::Connection), BarcaError> {
    OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let db = Builder::new_local(path)
        .build()
        .await
        .map_err(db_open_error)?;
    let conn = db
        .connect()
        .map_err(|e| BarcaError::Db(format!("failed to connect: {e}")))?;
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
pub(crate) async fn open_conn(db_path: &str) -> Result<(DbHandle, turso::Connection), BarcaError> {
    open_conn_with_until(db_path, crate::helper_proc::Until::done()).await
}

pub(crate) async fn open_conn_with_until(
    db_path: &str,
    until: crate::helper_proc::Until<'_>,
) -> Result<(DbHandle, turso::Connection), BarcaError> {
    let lock = until
        .admit(acquire_file_lock(db_path, DB_FILE_LOCK_WAIT))
        .await??;
    until.check()?;
    let (db, conn) = connect(db_path).await?;
    Ok((
        DbHandle {
            _db: db,
            _lock: lock,
        },
        conn,
    ))
}

/// Fold the write-ahead log into the main database file (`PRAGMA wal_checkpoint(TRUNCATE)`).
/// No other connection may be open on the file.
pub(crate) async fn checkpoint(conn: &turso::Connection) -> Result<(), BarcaError> {
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
    if busy {
        return Err(BarcaError::Db(
            "wal_checkpoint(TRUNCATE) could not finish: the database is in use by another \
             connection"
                .to_string(),
        ));
    }
    Ok(())
}

/// True when the write-ahead log beside `db_path` is absent or empty: the main file alone is
/// then the whole database.
pub fn wal_is_clean(db_path: &str) -> bool {
    match fs::metadata(format!("{db_path}-wal")) {
        Err(_) => true,
        Ok(m) => m.len() == 0,
    }
}

pub async fn init_db(db_path: &str) -> Result<(), BarcaError> {
    init_db_as(db_path, db_path).await.map(|_| ())
}

/// [`init_db`] on the file at `path`, naming `shown` when the database cannot be used (a
/// snapshot names the database it copies). Returns the format version (`PRAGMA user_version`,
/// [`crate::db_schema::VERSION`] once initialized).
///
/// An empty file (0 bytes, no write-ahead log) holds no history, so it is initialized as a new
/// database, with a note naming it: a `touch`, a backup tool or a reader that opened the path
/// before barca created the database leaves one (#537). A database this release cannot use is
/// [`BarcaError::MetadataUnusable`]: a file that is not SQLite, a newer format, or a malformed
/// schema. A lock or I/O failure is [`BarcaError::Db`].
pub async fn init_db_as(path: &str, shown: &str) -> Result<i64, BarcaError> {
    let _g = db_guard().await;
    initialized(path, shown)
        .await
        .map(|(handle, conn, version)| {
            // The connection keeps the file open (and Turso's lock on it) until it is dropped,
            // so it goes before the handle releases barca's lock. Dropped the other way round,
            // another process could take barca's lock while the file was still held, and fail
            // with "File is locked by another process" (#537).
            drop(conn);
            drop(handle);
            version
        })
        .map_err(|e| named(e, shown))
}

/// The database at `path` opened under its lock and initialized ([`init_db_as`]), with its
/// format version. The caller holds the in-process guard, and drops the connection before the
/// handle.
async fn initialized(
    path: &str,
    shown: &str,
) -> Result<(DbHandle, turso::Connection, i64), BarcaError> {
    // Under the lock: a database another process is creating is empty until it commits.
    let lock = acquire_file_lock(path, DB_FILE_LOCK_WAIT).await?;
    if empty_file(path) {
        // Replaced rather than opened in place: nothing in it, and a new file is what a
        // missing database gets.
        fs::remove_file(path)?;
        // A snapshot's copy is initialized silently: the note is for the file itself.
        if path == shown {
            crate::errln!(
                "[barca] note: the metadata DB {shown} was empty (0 bytes); initialized it \
                 as a new database"
            );
        }
    } else if let Some(why) = unusable_file(path) {
        return Err(unusable(why));
    }
    let (db, conn) = connect(path).await?;
    let handle = DbHandle {
        _db: db,
        _lock: lock,
    };
    let version = match init_schema(&conn).await {
        Ok(()) => crate::db_schema::read_version(&conn).await,
        Err(e) => Err(e),
    };
    match version {
        Ok(version) => Ok((handle, conn, version)),
        Err(e) => {
            drop(conn);
            drop(handle);
            Err(e)
        }
    }
}

/// What a run reads from the metadata DB as it starts ([`start_run`]).
pub struct StartedRun {
    /// Every persisted per-node cost estimate ([`load_cost_estimates`]).
    pub cost_estimates: Vec<(String, crate::cost::NodeEstimate)>,
    /// [`get_avg_elapsed`] of the run's unpartitioned nodes.
    pub avg_elapsed: HashMap<String, f64>,
    /// [`get_avg_elapsed_for_partitioned`] of its partitioned base ids.
    pub partitioned_avg_elapsed: HashMap<String, f64>,
}

/// Start a run in one visit to the metadata DB: initialize it ([`init_db`]), record the run
/// ([`create_run_with`]) and read what the run needs (cost estimates, average step times). These
/// were four opens of the database, each with its lock and schema check (#560).
pub async fn start_run(
    db_path: &str,
    run: NewRun<'_>,
    node_ids: &[String],
    base_ids: &[String],
) -> Result<StartedRun, BarcaError> {
    let _g = db_guard().await;
    let (handle, conn, _) = initialized(db_path, db_path)
        .await
        .map_err(|e| named(e, db_path))?;
    let started = async {
        insert_run(&conn, db_path, &run).await;
        Ok(StartedRun {
            cost_estimates: cost_estimates_on(&conn).await?,
            avg_elapsed: avg_elapsed_on(&conn, node_ids).await?,
            partitioned_avg_elapsed: partitioned_avg_elapsed_on(&conn, base_ids).await?,
        })
    }
    .await;
    drop(conn);
    drop(handle);
    started
}

/// `error` naming `shown`, if it says the database cannot be used. Turso's own words for a file
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

/// The format version of the metadata DB at `path`, read from a private snapshot without
/// writing the file: `None` when there is no database yet. A database this release cannot use
/// is [`BarcaError::MetadataUnusable`] naming `path`. For a read-only server, which never
/// migrates the file itself.
pub async fn probe_metadata(path: &str) -> Result<Option<i64>, BarcaError> {
    directory_check(path)?;
    let Some(snapshot) = DbSnapshot::take(path).await? else {
        return Ok(None);
    };
    // The copy is private, so it can be judged without a lock. An empty file holds no history:
    // the same as none, which a writer initializes ([`init_db_as`]).
    if empty_file(snapshot.path()) {
        crate::errln!("[barca] note: the metadata DB {path} is empty (0 bytes): no history yet");
        return Ok(None);
    }
    if let Some(why) = unusable_file(snapshot.path()) {
        return Err(BarcaError::MetadataUnusable {
            path: path.to_string(),
            why,
        });
    }
    // The format the file is in, before the copy is migrated.
    let version = {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(snapshot.path())
            .await
            .map_err(|e| named(e, path))?;
        crate::db_schema::read_version(&conn)
            .await
            .map_err(|e| named(e, path))?
    };
    init_db_as(snapshot.path(), path).await?;
    Ok(Some(version))
}

/// Migrate legacy metadata atomically, preserving authoritative history. Unknown versions
/// are refused before any write; complete current databases are left unchanged.
///
/// Format 1 is additive: a format-1 database written by an earlier release may lack a
/// nullable column, a table or an index added since (`materializations.bytes_changed` in
/// 0.24.0; see `db_schema::ADDED_TABLES` and `db_schema::INDEXES`). It gets the same additive
/// migration as a legacy database, in one transaction, and an older release still reads the
/// result.
pub(crate) async fn init_schema(conn: &turso::Connection) -> Result<(), BarcaError> {
    let version = crate::db_schema::check(conn).await?;
    if version == crate::db_schema::VERSION {
        // Read once and judged twice: this runs at the start of every run.
        let schema = crate::state_validate::Schema::read(conn).await?;
        if crate::db_schema::additions_missing_in(&schema) {
            // A table that is there but lacks a column the additions do not bring is malformed,
            // not behind: refuse it before any write.
            schema.columns_fit(true).map_err(unusable)?;
            return add_to_current_schema(conn).await;
        }
        return schema.columns_fit(false).map_err(unusable);
    }
    crate::state_validate::legacy_schema(conn)
        .await?
        .map_err(unusable)?;
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin schema migration: {e}")))?;
    let migrated = async {
        migrate_legacy_schema(conn).await?;
        crate::state_validate::usable_schema(conn)
            .await?
            .map_err(unusable)?;
        conn.execute(
            &format!("PRAGMA user_version = {}", crate::db_schema::VERSION),
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record schema version: {e}")))?;
        conn.execute("COMMIT", ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to commit schema migration: {e}")))?;
        Ok(())
    }
    .await;
    if let Err(error) = migrated {
        conn.execute("ROLLBACK", ()).await.map_err(|e| {
            BarcaError::Db(format!("{error}; schema migration rollback failed: {e}"))
        })?;
        return Err(error);
    }
    Ok(())
}

/// Add this release's additive columns and indexes to a version-1 database written by an
/// earlier release, in one transaction: rows are untouched, and an error leaves the schema as it
/// was. The format stays version 1 (see `specs/metadata-schema.md`).
async fn add_to_current_schema(conn: &turso::Connection) -> Result<(), BarcaError> {
    conn.execute("BEGIN IMMEDIATE", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin schema migration: {e}")))?;
    let added = async {
        crate::db_schema::add_columns(
            conn,
            "materializations",
            crate::db_schema::MATERIALIZATION_COLUMNS,
        )
        .await?;
        crate::db_schema::add_columns(conn, "runs", crate::db_schema::RUN_COLUMNS).await?;
        crate::db_schema::add_columns(
            conn,
            "schedule_state",
            crate::db_schema::SCHEDULE_STATE_COLUMNS,
        )
        .await?;
        crate::db_schema::add_tables(conn).await?;
        for index in crate::db_schema::INDEXES {
            conn.execute(index, ())
                .await
                .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;
        }
        crate::state_validate::usable_schema(conn)
            .await?
            .map_err(unusable)?;
        conn.execute("COMMIT", ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to commit schema migration: {e}")))?;
        Ok(())
    }
    .await;
    if let Err(error) = added {
        conn.execute("ROLLBACK", ()).await.map_err(|e| {
            BarcaError::Db(format!("{error}; schema migration rollback failed: {e}"))
        })?;
        return Err(error);
    }
    Ok(())
}

async fn migrate_legacy_schema(conn: &turso::Connection) -> Result<(), BarcaError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS materializations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id TEXT NOT NULL,
            run_hash TEXT,
            output_json TEXT,
            artifact_path TEXT,
            artifact_format TEXT,
            artifact_size_bytes INTEGER,
            elapsed_seconds REAL,
            status TEXT NOT NULL DEFAULT 'success',
            error_message TEXT,
            error_traceback TEXT,
            attempts INTEGER DEFAULT 1,
            sinks_json TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create materializations table: {e}")))?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_mat_node_run ON materializations(node_id, run_hash)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;

    // Migrate existing databases: add artifact columns if missing.
    crate::db_schema::add_columns(
        conn,
        "materializations",
        crate::db_schema::MATERIALIZATION_COLUMNS,
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_mat_run ON materializations(run_id)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;

    // Per-node cost estimates: the persisted EWMA that seeds the next run's
    // batch sizing, so the 30s cold-start probe is paid once ever per stable
    // node, not once per run. One row per exact node id (including partition
    // suffix) — current estimate only, no history vector.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cost_estimates (
            node_id TEXT PRIMARY KEY,
            base_id TEXT NOT NULL,
            estimate_seconds REAL NOT NULL,
            cpu_seconds REAL,
            max_rss_bytes INTEGER,
            samples INTEGER DEFAULT 0,
            updated_at TEXT DEFAULT (datetime('now'))
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create cost_estimates table: {e}")))?;

    // Run history table.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT UNIQUE NOT NULL,
            command TEXT NOT NULL,
            files TEXT NOT NULL,
            target TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            steps_total INTEGER,
            steps_executed INTEGER DEFAULT 0,
            steps_cached INTEGER DEFAULT 0,
            started_at TEXT DEFAULT (datetime('now')),
            finished_at TEXT,
            elapsed_seconds REAL
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create runs table: {e}")))?;
    // Who is executing a `running` run (#214): the coordinator's pid and host, so a run whose
    // process died without recording an outcome can be told from one still in progress.
    crate::db_schema::add_columns(conn, "runs", crate::db_schema::RUN_COLUMNS).await?;
    // Indexes and tables added within format 1 (`db_schema::INDEXES`, `ADDED_TABLES`).
    for index in crate::db_schema::INDEXES {
        conn.execute(index, ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;
    }
    crate::db_schema::add_tables(conn).await?;

    // Unfinished runs are looked up by status (`interrupted_runs`): through this index that
    // costs the number of such runs, not the length of the history.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;

    // Scheduler durability: the last time each scheduled node was fired, as
    // unix epoch seconds. Lets `barca serve` catch up a single missed tick
    // after downtime instead of silently skipping it.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schedule_state (
            node_id TEXT PRIMARY KEY,
            last_fired_at INTEGER NOT NULL
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create schedule_state table: {e}")))?;
    crate::db_schema::add_columns(
        conn,
        "schedule_state",
        crate::db_schema::SCHEDULE_STATE_COLUMNS,
    )
    .await?;

    // Captured user stdout, one row per line. Rust owns persistence; workers
    // stream lines over the socket and the coordinator writes them here.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            line TEXT NOT NULL,
            created_at TEXT DEFAULT (datetime('now'))
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create logs table: {e}")))?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_logs_run ON logs(run_id, seq)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create logs index: {e}")))?;
    Ok(())
}

/// A private, point-in-time copy of a metadata DB (main file + WAL) in a temp
/// dir, deleted on drop. Read-only consumers query the copy, so the original —
/// possibly being written by another barca process — is never opened, locked
/// for longer than the copy takes, created, migrated, or checkpointed.
pub struct DbSnapshot {
    _dir: tempfile::TempDir,
    path: String,
}

impl DbSnapshot {
    /// Copy `db_path` (and its `-wal`) into a fresh temp dir. `None` when there
    /// is no DB yet — the absence is preserved, not filled in.
    ///
    /// If barca's cross-process lock file exists, the copy is taken under that
    /// lock so it can't interleave with another process's write. The lock file
    /// is opened, never created: an older barca that doesn't lock leaves no
    /// file, and the copy proceeds unlocked (WAL recovery then stops at the
    /// last complete commit).
    pub async fn take(db_path: &str) -> Result<Option<Self>, BarcaError> {
        if !Path::new(db_path).exists() {
            return Ok(None);
        }
        let lock_path = format!("{db_path}.lock");
        let _lock = if Path::new(&lock_path).exists() {
            Some(acquire_file_lock(db_path, DB_FILE_LOCK_WAIT).await?)
        } else {
            None
        };

        let dir = tempfile::tempdir()
            .map_err(|e| BarcaError::Db(format!("failed to create snapshot dir: {e}")))?;
        let copy = dir.path().join("metadata.db");
        fs::copy(db_path, &copy)
            .map_err(|e| BarcaError::Db(format!("failed to snapshot DB: {e}")))?;
        let wal = format!("{db_path}-wal");
        if Path::new(&wal).exists() {
            fs::copy(&wal, dir.path().join("metadata.db-wal"))
                .map_err(|e| BarcaError::Db(format!("failed to snapshot DB WAL: {e}")))?;
        }
        Ok(Some(Self {
            path: copy.display().to_string(),
            _dir: dir,
        }))
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

/// One row of materialization history, as needed for asset state.
#[derive(Debug, Clone)]
pub struct MaterializationRow {
    pub node_id: String,
    pub status: String,
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub error_message: Option<String>,
}

/// Every materialization attempt, oldest first. An older DB without the table
/// yields no rows rather than an error.
pub async fn materialization_history(db_path: &str) -> Result<Vec<MaterializationRow>, BarcaError> {
    let _g = db_guard().await;
    let (_h, conn) = open_conn(db_path).await?;
    let Ok(mut rows) = conn
        .query(
            "SELECT node_id, status, created_at, elapsed_seconds, error_message \
             FROM materializations ORDER BY id ASC",
            (),
        )
        .await
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read materialization: {e}")))?
    {
        out.push(MaterializationRow {
            node_id: row.get::<String>(0).unwrap_or_default(),
            status: row.get::<String>(1).unwrap_or_default(),
            created_at: row.get::<String>(2).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(3).ok(),
            error_message: row.get::<String>(4).ok(),
        });
    }
    Ok(out)
}

/// One captured stdout line for a run.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LogEntry {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
    pub line: String,
}

/// Persist captured stdout lines for a run, in order. `lines` is (node_id, line). Idempotent:
/// a run that already has lines here is left alone.
pub async fn insert_logs(
    db_path: &str,
    run_id: &str,
    lines: &[(String, String)],
) -> Result<(), BarcaError> {
    if lines.is_empty() {
        return Ok(());
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    // Store the entire ordered stream atomically. A failed write must not leave
    // a partial stream that the idempotence check would mistake for a complete one.
    let mut rows = conn
        .query("SELECT 1 FROM logs WHERE run_id = ?1 LIMIT 1", [run_id])
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect recorded logs: {e}")))?;
    if rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read recorded logs: {e}")))?
        .is_some()
    {
        return Ok(());
    }
    drop(rows);
    conn.execute("BEGIN", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin log write: {e}")))?;
    let result = async {
        let mut insert = prepare(
            &conn,
            "INSERT INTO logs (run_id, node_id, seq, line) VALUES (?1, ?2, ?3, ?4)",
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record stdout: {e}")))?;
        for (seq, (node_id, line)) in lines.iter().enumerate() {
            insert
                .execute((
                    run_id.to_string(),
                    node_id.clone(),
                    seq as i64,
                    line.clone(),
                ))
                .await
                .map_err(|e| BarcaError::Db(format!("failed to record stdout: {e}")))?;
        }
        drop(insert);
        conn.execute("COMMIT", ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to commit recorded stdout: {e}")))?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        conn.execute("ROLLBACK", ())
            .await
            .map_err(|e| BarcaError::Db(format!("{error}; log rollback failed: {e}")))?;
        return Err(error);
    }
    Ok(())
}

/// Fetch all persisted log lines for a run, in order.
pub async fn get_logs(db_path: &str, run_id: &str) -> Result<Vec<LogEntry>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT node_id, seq, line FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
            [run_id.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query logs: {e}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read log row: {e}")))?
    {
        out.push(LogEntry {
            node_id: row.get::<String>(0).unwrap_or_default(),
            seq: row.get::<i64>(1).unwrap_or_default(),
            line: row.get::<String>(2).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Load the last-fired time (unix epoch seconds) for every scheduled node.
pub async fn get_schedule_state(db_path: &str) -> Result<HashMap<String, i64>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query("SELECT node_id, last_fired_at FROM schedule_state", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query schedule_state: {e}")))?;
    let mut out = HashMap::new();
    while let Ok(Some(row)) = rows.next().await {
        let node_id = row.get::<String>(0).unwrap_or_default();
        let last = row.get::<i64>(1).unwrap_or_default();
        if !node_id.is_empty() {
            out.insert(node_id, last);
        }
    }
    Ok(out)
}

/// Record that a scheduled node fired at `epoch_secs` (unix epoch seconds).
pub async fn upsert_schedule_state(
    db_path: &str,
    node_id: &str,
    epoch_secs: i64,
) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "INSERT INTO schedule_state (node_id, last_fired_at) VALUES (?1, ?2)
         ON CONFLICT(node_id) DO UPDATE SET last_fired_at = ?2",
        [node_id.to_string(), epoch_secs.to_string()],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to upsert schedule_state: {e}")))?;
    Ok(())
}

/// A schedule held back by `POST /schedule/{id}/pause`: since when, and until when (`None`:
/// until it is resumed). Unix epoch seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulePause {
    pub paused_at: i64,
    pub until: Option<i64>,
}

/// Every paused schedule, by node id. Expired pauses are included; the caller compares
/// `until` with its clock.
pub async fn get_schedule_pauses(
    db_path: &str,
) -> Result<HashMap<String, SchedulePause>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_column(&conn, "schedule_state", "paused_at").await? {
        return Ok(HashMap::new());
    }
    let mut rows = conn
        .query(
            "SELECT node_id, paused_at, paused_until FROM schedule_state WHERE paused_at IS NOT NULL",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query schedule pauses: {e}")))?;
    let mut out = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read schedule pause: {e}")))?
    {
        let node_id = row.get::<String>(0).unwrap_or_default();
        if let Ok(paused_at) = row.get::<i64>(1) {
            out.insert(
                node_id,
                SchedulePause {
                    paused_at,
                    until: row.get::<i64>(2).ok(),
                },
            );
        }
    }
    Ok(out)
}

/// Pause `node_id`'s schedule (`Some`) or resume it (`None`). A schedule with no row yet gets
/// one anchored at `now`, as the scheduler anchors a new job. Resuming moves the last-fired
/// record to `now`, so the ticks skipped while paused are not caught up at the next start.
pub async fn set_schedule_pause(
    db_path: &str,
    node_id: &str,
    pause: Option<SchedulePause>,
    now: i64,
) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let opt = |v: Option<i64>| v.map(|v| v.to_string()).unwrap_or_default();
    let sql = match pause {
        Some(_) => {
            "INSERT INTO schedule_state (node_id, last_fired_at, paused_at, paused_until) VALUES (?1, ?2, NULLIF(?3, ''), NULLIF(?4, ''))
             ON CONFLICT(node_id) DO UPDATE SET paused_at = NULLIF(?3, ''), paused_until = NULLIF(?4, '')"
        }
        None => {
            "INSERT INTO schedule_state (node_id, last_fired_at, paused_at, paused_until) VALUES (?1, ?2, NULLIF(?3, ''), NULLIF(?4, ''))
             ON CONFLICT(node_id) DO UPDATE SET paused_at = NULL, paused_until = NULL, last_fired_at = MAX(last_fired_at, ?2)"
        }
    };
    conn.execute(
        sql,
        [
            node_id.to_string(),
            now.to_string(),
            opt(pause.map(|p| p.paused_at)),
            opt(pause.and_then(|p| p.until)),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to record schedule pause: {e}")))?;
    Ok(())
}

/// Consecutive failed ticks of one scheduled node (`schedule_failures`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleFailures {
    /// Consecutive ticks whose run failed.
    pub failures: u32,
    /// The nodes that failed in the last counted tick.
    pub failed_nodes: Vec<String>,
    /// The highest materialization id when the last failure was counted.
    pub mark: i64,
}

/// Consecutive failed ticks for every scheduled node that has any.
pub async fn get_schedule_failures(
    db_path: &str,
) -> Result<HashMap<String, ScheduleFailures>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT node_id, failures, failed_nodes, mark FROM schedule_failures",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query schedule_failures: {e}")))?;
    let mut out = HashMap::new();
    while let Ok(Some(row)) = rows.next().await {
        let node_id = row.get::<String>(0).unwrap_or_default();
        if node_id.is_empty() {
            continue;
        }
        out.insert(
            node_id,
            ScheduleFailures {
                failures: u32::try_from(row.get::<i64>(1).unwrap_or_default()).unwrap_or(0),
                failed_nodes: row
                    .get::<String>(2)
                    .ok()
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
                mark: row.get::<i64>(3).unwrap_or_default(),
            },
        );
    }
    Ok(out)
}

/// Record `node_id`'s consecutive failed ticks; zero failures removes its row.
pub async fn set_schedule_failures(
    db_path: &str,
    node_id: &str,
    state: &ScheduleFailures,
) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let failed = |e| BarcaError::Db(format!("failed to write schedule_failures: {e}"));
    if state.failures == 0 {
        conn.execute(
            "DELETE FROM schedule_failures WHERE node_id = ?1",
            [node_id.to_string()],
        )
        .await
        .map_err(failed)?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO schedule_failures (node_id, failures, failed_nodes, mark)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(node_id) DO UPDATE SET failures = ?2, failed_nodes = ?3, mark = ?4",
        [
            node_id.to_string(),
            state.failures.to_string(),
            serde_json::to_string(&state.failed_nodes).unwrap_or_else(|_| "[]".into()),
            state.mark.to_string(),
        ],
    )
    .await
    .map_err(failed)?;
    Ok(())
}

/// The highest materialization id recorded so far (0 when there is none).
pub async fn max_materialization_id(db_path: &str) -> Result<i64, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query("SELECT COALESCE(MAX(id), 0) FROM materializations", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    Ok(match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0),
        _ => 0,
    })
}

/// Whether a successful materialization of one of `nodes` (or of one of their partitions) was
/// recorded after materialization id `mark`.
pub async fn succeeded_since(
    db_path: &str,
    mark: i64,
    nodes: &[String],
) -> Result<bool, BarcaError> {
    if nodes.is_empty() {
        return Ok(false);
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT DISTINCT node_id FROM materializations WHERE id > ?1 AND status = 'success'",
            [mark.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    while let Ok(Some(row)) = rows.next().await {
        let id = row.get::<String>(0).unwrap_or_default();
        let base = id.split('[').next().unwrap_or(&id);
        if nodes.iter().any(|n| n == base) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The latest successful materialization of one step (a display id) that has an artifact.
#[derive(Debug, Clone)]
pub(crate) struct LatestSuccess {
    /// Its row id: a later success of another step has a higher one.
    pub(crate) id: i64,
    pub(crate) run_hash: String,
    pub(crate) output: OutputRef,
}

/// The latest successful materialization of `node_id` (a display id) whose artifact this run may
/// read; `None` when it never succeeded.
pub(crate) async fn latest_success(cache: &CacheReader, node_id: &str) -> Option<LatestSuccess> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
             output_hash FROM materializations WHERE node_id = ?1 AND status = 'success' \
             AND artifact_path IS NOT NULL ORDER BY id DESC",
            [node_id.to_string()],
        )
        .await
        .ok()?;
    while let Ok(Some(row)) = rows.next().await {
        let Ok(path) = row.get::<String>(2) else {
            continue;
        };
        if !cache.allows_artifact(&path) {
            continue;
        }
        return Some(LatestSuccess {
            id: row.get::<i64>(0).ok()?,
            run_hash: row.get::<String>(1).ok().unwrap_or_default(),
            output: OutputRef {
                path,
                format: row.get::<String>(3).ok()?,
                size_bytes: row.get::<i64>(4).ok()? as u64,
                elapsed_seconds: None,
                content_hash: row.get::<String>(5).ok().filter(|h| !h.is_empty()),
            },
        });
    }
    None
}

/// The id of the latest successful materialization of `node_id` (a display id), artifact or
/// not; `None` when it never succeeded.
pub(crate) async fn latest_success_id(cache: &CacheReader, node_id: &str) -> Option<i64> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT MAX(id) FROM materializations WHERE node_id = ?1 AND status = 'success'",
            [node_id.to_string()],
        )
        .await
        .ok()?;
    rows.next().await.ok()??.get::<i64>(0).ok()
}

/// The output hash of the latest successful materialization of each step of `base_ids` (the
/// base id itself, or any of its partitions), keyed by display id. Steps never recorded with an
/// output hash (never ran, or ran before barca recorded sensor outputs) are absent; so is
/// everything when the database predates the `output_hash` column.
pub async fn last_output_hashes(
    cache: &CacheReader,
    base_ids: &[&str],
) -> Result<HashMap<String, String>, BarcaError> {
    let mut out = HashMap::new();
    for base in base_ids {
        let Ok(mut rows) = cache
            .conn()
            .query(
                "SELECT node_id, output_hash FROM materializations \
                 WHERE (node_id = ?1 OR node_id LIKE ?2) AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY id",
                [base.to_string(), format!("{base}[%")],
            )
            .await
        else {
            return Ok(HashMap::new());
        };
        // LIKE treats `_` in a node id as a wildcard, and assets record output hashes too.
        let partition_prefix = format!("{base}[");
        while let Ok(Some(row)) = rows.next().await {
            if let (Ok(id), Ok(h)) = (row.get::<String>(0), row.get::<String>(1))
                && (id == *base || id.starts_with(&partition_prefix))
            {
                out.insert(id, h);
            }
        }
    }
    Ok(out)
}

pub async fn persist_outputs(
    db_path: &str,
    outputs: &HashMap<String, OutputRef>,
    run_hashes: &HashMap<String, String>,
) -> Result<(), BarcaError> {
    if outputs.is_empty() {
        return Ok(());
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    for (node_id, oref) in outputs {
        let run_hash = run_hashes.get(node_id).cloned().unwrap_or_default();
        let elapsed_str = oref
            .elapsed_seconds
            .map(|e| e.to_string())
            .unwrap_or_default();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds) VALUES (?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''))",
            [
                node_id.clone(),
                run_hash,
                oref.path.clone(),
                oref.format.clone(),
                oref.size_bytes.to_string(),
                elapsed_str,
            ],
        )
        .await
        .ok();
    }
    Ok(())
}

/// Load every persisted per-node cost estimate (run start — seeds the
/// in-memory `CostModel` so batch sizing starts pre-warmed).
pub async fn load_cost_estimates(
    db_path: &str,
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    cost_estimates_on(&conn).await
}

async fn cost_estimates_on(
    conn: &turso::Connection,
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT node_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples FROM cost_estimates",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query cost_estimates: {e}")))?;
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let node_id = row.get::<String>(0).unwrap_or_default();
        if node_id.is_empty() {
            continue;
        }
        out.push((
            node_id,
            crate::cost::NodeEstimate {
                estimate_seconds: row.get::<f64>(1).unwrap_or(0.0),
                cpu_seconds: row.get::<f64>(2).unwrap_or(0.0),
                max_rss_bytes: row.get::<i64>(3).unwrap_or(0).max(0) as u64,
                samples: row.get::<i64>(4).unwrap_or(0).max(0) as u64,
            },
        ));
    }
    Ok(out)
}

/// Upsert per-node cost estimates (run end — persists the EWMA so the next
/// run skips the cold-start probe entirely).
pub async fn upsert_cost_estimates(
    db_path: &str,
    estimates: &[(String, crate::cost::NodeEstimate)],
) -> Result<(), BarcaError> {
    if estimates.is_empty() {
        return Ok(());
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    for (node_id, est) in estimates {
        let base = crate::StepId::parse(node_id).base_id().to_string();
        conn.execute(
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(node_id) DO UPDATE SET
                 estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
                 samples = ?6, updated_at = datetime('now')",
            [
                node_id.clone(),
                base,
                est.estimate_seconds.to_string(),
                est.cpu_seconds.to_string(),
                est.max_rss_bytes.to_string(),
                est.samples.to_string(),
            ],
        )
        .await
        .ok();
    }
    Ok(())
}

/// Alias for backward compat in tests — delegates to persist_outputs.
pub async fn persist_output_refs(db_path: &str, outputs: &HashMap<String, OutputRef>) {
    persist_outputs(db_path, outputs, &HashMap::new())
        .await
        .ok();
}

/// Generate a short run ID from timestamp + random bits (no uuid crate).
pub fn generate_run_id() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Mix in the address of a stack variable for pseudo-randomness.
    let stack_addr = &nanos as *const _ as u64;
    let mixed = nanos as u64 ^ stack_addr;
    format!("{:012x}", mixed & 0xFFFF_FFFF_FFFF)
}

/// Create a new run record at the start of execution, started by hand.
pub async fn create_run(
    db_path: &str,
    run_id: &str,
    command: &str,
    files: &str,
    target: Option<&str>,
    steps_total: Option<usize>,
) -> Result<(), BarcaError> {
    create_run_with(
        db_path,
        NewRun {
            run_id,
            command,
            files,
            target,
            steps_total,
            job: None,
            trigger: &crate::outcome::Trigger::default(),
        },
    )
    .await
}

/// A run about to start.
pub struct NewRun<'a> {
    pub run_id: &'a str,
    pub command: &'a str,
    /// [`encode_files`] of the files.
    pub files: &'a str,
    pub target: Option<&'a str>,
    pub steps_total: Option<usize>,
    /// The canonical job id (`persist::canonical_job`), when known at the start. Written with
    /// the row, so a `barca serve` on the same project sees which nodes a running CLI run
    /// covers and skips their ticks (#479). [`crate::persist`] writes it again at the end.
    pub job: Option<&'a str>,
    pub trigger: &'a crate::outcome::Trigger,
}

/// Create a new run record at the start of execution.
pub async fn create_run_with(db_path: &str, run: NewRun<'_>) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    insert_run(&conn, db_path, &run).await;
    Ok(())
}

/// The row of a run that is starting, and the runs it finds interrupted ([`create_run_with`]).
async fn insert_run(conn: &turso::Connection, db_path: &str, run: &NewRun<'_>) {
    // The marker exists from before the row does until the run has recorded its outcome
    // ([`finish_run`], or the end of this process).
    let owner = crate::run_owner::claim(db_path, run.run_id);
    conn.execute(
        "INSERT INTO runs (run_id, command, files, target, status, steps_total, pid, host, owner, triggered_by, scheduled_at, schedule_id, job) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, NULLIF(?8, ''), ?9, NULLIF(?10, ''), NULLIF(?11, ''), NULLIF(?12, ''))",
        [
            run.run_id.to_string(),
            run.command.to_string(),
            run.files.to_string(),
            run.target.unwrap_or("").to_string(),
            run.steps_total.map(|n| n.to_string()).unwrap_or_default(),
            std::process::id().to_string(),
            local_host(),
            owner,
            run.trigger.triggered_by.as_str().to_string(),
            run.trigger.scheduled_at.clone().unwrap_or_default(),
            run.trigger.schedule_id.clone().unwrap_or_default(),
            run.job.unwrap_or("").to_string(),
        ],
    )
    .await
    .ok();
    mark_interrupted_runs(conn, db_path).await;
}

/// Record a run that ended before it started executing (its target was refused, it was
/// cancelled while queued, its source no longer loads): a terminal row with its outcome, so
/// the id a server answered with is found in the history like any other run.
pub async fn record_unstarted_run(
    db_path: &str,
    run: NewRun<'_>,
    status: &str,
    outcome_json: &str,
) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "INSERT INTO runs (run_id, command, files, target, status, steps_total, finished_at, elapsed_seconds, triggered_by, scheduled_at, schedule_id, outcome_json) VALUES (?1, ?2, ?3, ?4, ?5, 0, datetime('now'), 0, ?6, NULLIF(?7, ''), NULLIF(?8, ''), ?9) ON CONFLICT(run_id) DO NOTHING",
        [
            run.run_id.to_string(),
            run.command.to_string(),
            run.files.to_string(),
            run.target.unwrap_or("").to_string(),
            status.to_string(),
            run.trigger.triggered_by.as_str().to_string(),
            run.trigger.scheduled_at.clone().unwrap_or_default(),
            run.trigger.schedule_id.clone().unwrap_or_default(),
            outcome_json.to_string(),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to record run: {e}")))?;
    Ok(())
}

/// Store a finished run's outcome (`barca serve`): `GET /runs/{run_id}` answers from it after
/// a restart. `status` corrects the row when the run ended before it recorded one.
pub async fn set_run_outcome(
    db_path: &str,
    run_id: &str,
    status: &str,
    outcome_json: &str,
) -> Result<bool, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let changed = conn
        .execute(
            "UPDATE runs SET outcome_json = ?2, status = CASE WHEN status = 'running' THEN ?3 ELSE status END, finished_at = COALESCE(finished_at, datetime('now')) WHERE run_id = ?1",
            [
                run_id.to_string(),
                outcome_json.to_string(),
                status.to_string(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record run outcome: {e}")))?;
    Ok(changed == 1)
}

/// The artifact (path, format) `run_id` wrote for `node_id`, if it wrote one.
pub async fn run_artifact(
    db_path: &str,
    run_id: &str,
    node_id: &str,
) -> Result<Option<(String, String)>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT artifact_path, COALESCE(artifact_format, '') FROM materializations WHERE run_id = ?1 AND node_id = ?2 AND status = 'success' AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1",
            [run_id, node_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run artifact: {e}")))?;
    let row = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run artifact: {e}")))?;
    Ok(row.and_then(|row| Some((row.get::<String>(0).ok()?, row.get::<String>(1).ok()?))))
}

/// A run's stored outcome ([`set_run_outcome`]), if it has one.
pub async fn get_run_outcome(db_path: &str, run_id: &str) -> Result<Option<String>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_column(&conn, "runs", "outcome_json").await? {
        return Ok(None);
    }
    let mut rows = conn
        .query("SELECT outcome_json FROM runs WHERE run_id = ?1", [run_id])
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run outcome: {e}")))?;
    let row = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run outcome: {e}")))?;
    Ok(row
        .and_then(|row| row.get::<String>(0).ok())
        .filter(|s| !s.is_empty()))
}

/// Whether `table` has `column` (an inspected snapshot may predate it).
async fn has_column(
    conn: &turso::Connection,
    table: &str,
    column: &str,
) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect {table} columns: {e}")))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read {table} columns: {e}")))?
    {
        if row.get::<String>(1).ok().as_deref() == Some(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// This machine's host name ("" when it cannot be read). Stored on a run row next to the pid:
/// a database restored from another machine's backup holds that machine's runs, whose pids
/// mean nothing here.
pub fn local_host() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into `buf`.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// True unless the process is known to be gone. (A pid reused by an unrelated process reads as
/// alive: the run then stays `running`, which is what it was before pids were recorded.)
pub(crate) fn pid_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    // SAFETY: signal 0 sends nothing; it only checks that the pid exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The `running` runs whose process is known to be gone (killed, out of memory, power loss):
/// they will never record an outcome themselves.
///
/// A run that recorded who started it is judged by [`crate::run_owner::judge`], which says
/// "gone" only when this process can establish it, also in a container, where the pid is 1
/// on every start and the host name changes. When it cannot (a run on another machine, in
/// another container whose marker cannot be consulted, on the host of this container) the
/// run is not listed: it may be going.
///
/// A run that recorded nothing of the kind (an older barca) is judged as before: gone when
/// it was started on this host and its pid does not exist. Best effort: empty when nothing
/// can be read.
async fn interrupted_runs(conn: &turso::Connection, db_path: &str) -> Vec<String> {
    let on = crate::run_owner::System::for_db(db_path);
    interrupted_among(&running_runs(conn).await, &on, pid_alive)
}

/// A `running` row, as much of it as says who is running it and what.
#[derive(Debug, Clone)]
struct RunningRun {
    run_id: String,
    pid: Option<i64>,
    host: String,
    /// The `owner` column: empty on rows from older versions.
    owner: String,
    command: String,
    /// The `job` column: empty on rows from older versions.
    job: String,
}

async fn running_runs(conn: &turso::Connection) -> Vec<RunningRun> {
    // A database this version has not migrated (history read from one an older barca wrote)
    // has no `owner` or `job` column yet. `idx_runs_status` serves the filter.
    let with_owner = "SELECT run_id, pid, COALESCE(host, ''), COALESCE(owner, ''), command, COALESCE(job, '') FROM runs WHERE status = 'running' ORDER BY id";
    let without = "SELECT run_id, pid, COALESCE(host, ''), '', command, '' FROM runs WHERE status = 'running' ORDER BY id";
    let mut rows = match conn.query(with_owner, ()).await {
        Ok(rows) => rows,
        Err(_) => match conn.query(without, ()).await {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        },
    };
    let mut running = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(run_id) = row.get::<String>(0) {
            running.push(RunningRun {
                run_id,
                pid: row.get::<i64>(1).ok(),
                host: row.get::<String>(2).unwrap_or_default(),
                owner: row.get::<String>(3).unwrap_or_default(),
                command: row.get::<String>(4).unwrap_or_default(),
                job: row.get::<String>(5).unwrap_or_default(),
            });
        }
    }
    running
}

/// [`interrupted_runs`] with what it asks of the machine passed in.
fn interrupted_among(
    running: &[RunningRun],
    on: &dyn crate::run_owner::Observer,
    pid_alive: impl Fn(i64) -> bool,
) -> Vec<String> {
    use crate::run_owner::{Owner, judge_json};
    let this_host = on.here().host;
    running
        .iter()
        .filter(|run| {
            if run.owner.is_empty() {
                let on_this_host = !this_host.is_empty() && run.host == this_host;
                on_this_host && run.pid.is_some_and(|pid| !pid_alive(pid))
            } else {
                judge_json(run.pid, &run.owner, on) == Owner::Gone
            }
        })
        .map(|run| run.run_id.clone())
        .collect()
}

/// Record [`interrupted_runs`] as `interrupted`. Their `finished_at` and `elapsed_seconds`
/// stay NULL, since nobody saw them end. Only a run does this (it writes anyway, and with
/// shared state pushes what it wrote); reading history reports the same status without
/// writing it (see [`get_recent_runs`]). Both go by the same rule: a run is recorded as
/// interrupted only by a process that has established that its owner is gone.
///
/// Afterwards the markers that no `running` row names any more are removed
/// ([`crate::run_owner::sweep`]).
async fn mark_interrupted_runs(conn: &turso::Connection, db_path: &str) {
    for run_id in interrupted_runs(conn, db_path).await {
        conn.execute(
            "UPDATE runs SET status = 'interrupted' WHERE run_id = ?1 AND status = 'running'",
            [run_id],
        )
        .await
        .ok();
    }
    let named = running_runs(conn)
        .await
        .iter()
        .filter_map(|run| crate::run_owner::token_of(&run.owner))
        .collect();
    crate::run_owner::sweep(db_path, &named, SystemTime::now());
}

/// The `running` runs whose owner is not known to be gone: in progress, here or elsewhere, or
/// unknowable (see [`interrupted_runs`]). Best effort: empty when nothing can be read.
pub(crate) async fn live_runs(conn: &turso::Connection, db_path: &str) -> Vec<String> {
    let running = running_runs(conn).await;
    let gone: std::collections::HashSet<String> = interrupted_among(
        &running,
        &crate::run_owner::System::for_db(db_path),
        pid_alive,
    )
    .into_iter()
    .collect();
    running
        .into_iter()
        .map(|run| run.run_id)
        .filter(|id| !gone.contains(id))
        .collect()
}

/// The `running` runs whose owner this process can see is alive: on this machine, a process
/// that exists. A run whose owner cannot be judged from here (another machine, an older row
/// from another host) is not listed: unlike [`live_runs`], which keeps those, this is for a
/// refusal that must not hold forever because of a row nobody will finish (`barca state
/// reset`, and the scheduler's skip of a node a CLI run covers). Best effort: empty when
/// nothing can be read.
pub(crate) async fn runs_alive_here(conn: &turso::Connection, db_path: &str) -> Vec<String> {
    alive_here(running_runs(conn).await, db_path)
        .into_iter()
        .map(|run| run.run_id)
        .collect()
}

/// The rows of `running` whose owner this process can see is alive (see [`runs_alive_here`]).
fn alive_here(running: Vec<RunningRun>, db_path: &str) -> Vec<RunningRun> {
    use crate::run_owner::{Owner, judge_json};
    let on = crate::run_owner::System::for_db(db_path);
    let this_host = crate::run_owner::Observer::here(&on).host;
    running
        .into_iter()
        .filter(|run| {
            if run.owner.is_empty() {
                !this_host.is_empty()
                    && run.host == this_host
                    && run.pid.is_some_and(|pid| pid > 0 && pid_alive(pid))
            } else {
                judge_json(run.pid, &run.owner, &on) == Owner::Alive
            }
        })
        .collect()
}

/// Open the database file at `db_path`, taking no lock, for a caller that holds it
/// ([`lock_db`]). `None` when it cannot be opened or is not one this version reads.
pub(crate) async fn connect_locked(db_path: &str) -> Option<(turso::Database, turso::Connection)> {
    connect(db_path).await.ok()
}

/// Create barca's tables in a new database at `db_path`, for a caller that holds its lock.
pub(crate) async fn init_locked(db_path: &str) -> Result<(), BarcaError> {
    let (_db, conn) = connect(db_path).await?;
    init_schema(&conn).await
}

/// A `running` run of another process whose owner is alive, and what it runs: see
/// [`alive_running_runs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliveRun {
    pub run_id: String,
    /// `get` or `run`.
    pub command: String,
    /// The canonical job id (`all`, or the resolved node ids joined by `,`); empty when the
    /// run was started by a barca that wrote it only at the end.
    pub job: String,
}

/// The `running` runs of other processes whose owner this process can see is alive
/// ([`runs_alive_here`]), oldest first: a `barca get` or `barca run` in progress on this
/// machine. `barca serve` skips a tick of a node one of them covers (#479).
///
/// This process's own rows are left out: the server knows its runs from memory, and a row of
/// its own left `running` (its outcome could not be written) must not hold a schedule back.
/// A row whose owner cannot be judged from here (another machine's, restored from a backup)
/// is left out too, so it never holds a schedule back for ever. One query of the `running`
/// rows (`idx_runs_status`); best effort: empty when nothing can be read.
pub async fn alive_running_runs(db_path: &str) -> Vec<AliveRun> {
    let running = {
        let _g = db_guard().await;
        let Ok((_db, conn)) = open_conn(db_path).await else {
            return Vec::new();
        };
        running_runs(&conn).await
    };
    let others = running
        .into_iter()
        .filter(|run| !crate::run_owner::is_this_process(run.pid, &run.host, &run.owner))
        .collect();
    alive_here(others, db_path)
        .into_iter()
        .map(|run| AliveRun {
            run_id: run.run_id,
            command: run.command,
            job: run.job,
        })
        .collect()
}

// ─── Cache size limit (RFC-0012) ─────────────────────────────────────────────

/// Record that the results at `paths` (recorded artifact paths) were read now: by a step that
/// took them as inputs, by `barca get` returning them, or by `barca sql`. A result that is read
/// is on disk again, so an eviction recorded for it no longer holds. Best effort: a database
/// that cannot be written is left as it is.
pub(crate) async fn touch_read(db_path: &str, paths: &[String]) {
    if paths.is_empty() || !Path::new(db_path).exists() {
        return;
    }
    let _g = db_guard().await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return;
    };
    touch_read_on(&conn, paths).await;
}

/// [`touch_read`] on an open connection, in a transaction of its own. Best effort.
pub(crate) async fn touch_read_on(conn: &turso::Connection, paths: &[String]) {
    if paths.is_empty() || conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
        return;
    }
    let ok = touch_read_in(conn, paths).await;
    let end = if ok { "COMMIT" } else { "ROLLBACK" };
    let _ = conn.execute(end, ()).await;
}

/// Set `last_read_at` of `paths` inside the caller's transaction. Returns whether every update
/// went through; it stops at the first that does not (the earlier ones stay in the transaction).
pub(crate) async fn touch_read_in(conn: &turso::Connection, paths: &[String]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut ok = true;
    match prepare(
        conn,
        "UPDATE materializations SET last_read_at = datetime('now'), evicted_at = NULL \
         WHERE artifact_path = ?1 AND status = 'success'",
    )
    .await
    {
        Ok(mut touch) => {
            for path in paths {
                ok &= touch.execute([path.as_str()]).await.is_ok();
                if !ok {
                    break;
                }
            }
        }
        Err(_) => ok = false,
    }
    ok
}

/// Of `paths` (recorded artifact paths), those whose latest successful row was evicted to stay
/// under the cache limit: their missing file is an eviction, not a deletion behind barca's back.
pub(crate) async fn evicted_among(
    db_path: &str,
    paths: &[&str],
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if paths.is_empty() || !Path::new(db_path).exists() {
        return out;
    }
    let _g = db_guard().await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return out;
    };
    for path in paths {
        let Ok(mut rows) = conn
            .query(
                "SELECT evicted_at IS NOT NULL FROM materializations WHERE artifact_path = ?1 \
                 AND status = 'success' ORDER BY id DESC LIMIT 1",
                [*path],
            )
            .await
        else {
            return out;
        };
        if let Ok(Some(row)) = rows.next().await
            && row.get::<i64>(0).unwrap_or(0) != 0
        {
            out.insert(path.to_string());
        }
    }
    out
}

/// Every recorded artifact path whose latest successful row is evicted (`barca status`,
/// `--dry-run`). Empty when the database predates the column.
pub(crate) async fn evicted_paths(cache: &CacheReader) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut rows) = cache
        .conn()
        .query(
            "SELECT m.artifact_path FROM materializations m WHERE m.evicted_at IS NOT NULL \
             AND m.status = 'success' AND m.id = (SELECT MAX(id) FROM materializations n \
             WHERE n.artifact_path = m.artifact_path AND n.status = 'success')",
            (),
        )
        .await
    else {
        return out;
    };
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            out.insert(path);
        }
    }
    out
}

/// Finalize a run record with status and stats.
pub async fn finish_run(
    db_path: &str,
    run_id: &str,
    status: &str,
    steps_executed: usize,
    steps_cached: usize,
    elapsed_seconds: f64,
) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let updated = conn.execute(
        "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, elapsed_seconds = ?4, finished_at = datetime('now') WHERE run_id = ?5",
        [
            status.to_string(),
            steps_executed.to_string(),
            steps_cached.to_string(),
            elapsed_seconds.to_string(),
            run_id.to_string(),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to finish run: {e}")))?;
    if updated != 1 {
        return Err(BarcaError::Db("cannot finish a missing run".to_string()));
    }
    crate::run_owner::release(db_path, run_id);
    Ok(())
}

/// Retrieve recent run records, newest first.
pub async fn get_recent_runs(db_path: &str, limit: usize) -> Result<Vec<RunRecord>, BarcaError> {
    read_runs(db_path, None, limit).await
}

/// Explicit operator reconciliation when a run is known to have stopped but its
/// owner cannot be proven dead (for example, an old row or a VM-shared mount).
/// Positive live-owner evidence always refuses the operation. Preserve successful
/// materializations and all counters; nobody observed an end time or duration.
pub async fn reconcile_run(db_path: &str, run_id: &str) -> Result<RunRecord, BarcaError> {
    {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(db_path).await?;
        let mut rows = conn
            .query(
                "SELECT status, pid, COALESCE(owner, '') FROM runs WHERE run_id = ?1",
                [run_id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read run: {e}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read run: {e}")))?
        else {
            return Err(BarcaError::Usage(format!(
                "no run '{run_id}' in this environment\nUse barca history --all to find the run id."
            )));
        };
        let status = row
            .get::<String>(0)
            .map_err(|e| BarcaError::Db(e.to_string()))?;
        if status != "running" {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' is already {status}; only running rows can be reconciled"
            )));
        }
        let owner = row
            .get::<String>(2)
            .map_err(|e| BarcaError::Db(e.to_string()))?;
        let on = crate::run_owner::System::for_db(db_path);
        if crate::run_owner::judge_json(row.get::<i64>(1).ok(), &owner, &on)
            == crate::run_owner::Owner::Alive
        {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' has a live owner\nWait for or stop that run before reconciling its history."
            )));
        }
        drop(rows);
        let changed = conn
            .execute(
                "UPDATE runs SET status = 'interrupted' WHERE run_id = ?1 AND status = 'running'",
                [run_id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to reconcile run: {e}")))?;
        if changed == 0 {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' ended while it was being reconciled; read barca history again"
            )));
        }
    }
    get_run(db_path, run_id)
        .await?
        .ok_or_else(|| BarcaError::Db(format!("reconciled run '{run_id}' could not be read")))
}

/// Read a durable run by id, including the same interrupted-owner diagnosis as history.
pub async fn get_run(db_path: &str, run_id: &str) -> Result<Option<RunRecord>, BarcaError> {
    Ok(read_runs(db_path, Some(run_id), 1)
        .await?
        .into_iter()
        .next())
}

async fn read_runs(
    db_path: &str,
    run_id: Option<&str>,
    limit: usize,
) -> Result<Vec<RunRecord>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    // A run whose process died is reported as `interrupted`, not as still `running`. Reported,
    // not written: reading history must not change the database (the next run records it).
    let interrupted = interrupted_runs(&conn, db_path).await;
    // Inspection also reads legacy snapshots without migrating them. Unknown attempt
    // metadata is null, rather than fabricating a zero or requiring newer columns.
    let mut columns = conn
        .query("PRAGMA table_info(materializations)", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect materialization columns: {e}")))?;
    let mut names = std::collections::HashSet::new();
    while let Some(row) = columns
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read materialization columns: {e}")))?
    {
        names.insert(
            row.get::<String>(1)
                .map_err(|e| BarcaError::Db(e.to_string()))?,
        );
    }
    drop(columns);
    let attempts = if names.contains("run_id") && names.contains("attempts") {
        "(SELECT COALESCE(SUM(attempts), 0) FROM materializations WHERE run_id = runs.run_id)"
    } else {
        "NULL"
    };
    let trigger = if has_column(&conn, "runs", "triggered_by").await? {
        "triggered_by, scheduled_at, schedule_id"
    } else {
        "NULL, NULL, NULL"
    };
    let client = if has_column(&conn, "runs", "client_run_id").await? {
        "client_run_id"
    } else {
        "NULL"
    };
    let mut rows = conn
        .query(
            &format!("SELECT run_id, command, files, target, status, steps_total, steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, {attempts}, {trigger}, {client} FROM runs WHERE (?1 = '' OR run_id = ?1) ORDER BY id DESC LIMIT ?2"),
            [run_id.unwrap_or_default().to_string(), limit.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query runs: {e}")))?;
    let mut records = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        let run_id = row.get::<String>(0).unwrap_or_default();
        let status = if interrupted.contains(&run_id) {
            "interrupted".to_string()
        } else {
            row.get::<String>(4).unwrap_or_default()
        };
        records.push(RunRecord {
            run_id,
            command: row.get::<String>(1).unwrap_or_default(),
            files: decode_files(&row.get::<String>(2).unwrap_or_default()),
            target: {
                let t = row.get::<String>(3).unwrap_or_default();
                if t.is_empty() { None } else { Some(t) }
            },
            status,
            steps_total: row.get::<i64>(5).ok(),
            steps_executed: row.get::<i64>(6).unwrap_or(0),
            steps_cached: row.get::<i64>(7).unwrap_or(0),
            attempts: row.get::<i64>(11).ok(),
            started_at: row.get::<String>(8).unwrap_or_default(),
            finished_at: {
                let t = row.get::<String>(9).unwrap_or_default();
                if t.is_empty() { None } else { Some(t) }
            },
            elapsed_seconds: row.get::<f64>(10).ok(),
            triggered_by: row
                .get::<String>(12)
                .ok()
                .and_then(|t| crate::outcome::TriggeredBy::parse(&t)),
            scheduled_at: row.get::<String>(13).ok().filter(|t| !t.is_empty()),
            schedule_id: row.get::<String>(14).ok().filter(|t| !t.is_empty()),
            client_run_id: row.get::<String>(15).ok().filter(|t| !t.is_empty()),
        });
    }
    Ok(records)
}

/// A step recorded by this run. Cached inputs have no new materialization row.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunStep {
    pub node_id: String,
    pub status: String,
    pub elapsed_seconds: Option<f64>,
    pub error: Option<String>,
}

pub async fn get_run_steps(db_path: &str, run_id: &str) -> Result<Vec<RunStep>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn.query(
        "SELECT node_id, status, elapsed_seconds, error_message FROM materializations WHERE run_id = ?1 ORDER BY id",
        [run_id],
    ).await.map_err(|e| BarcaError::Db(format!("failed to query run steps: {e}")))?;
    let mut steps = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run step: {e}")))?
    {
        steps.push(RunStep {
            node_id: row.get::<String>(0).unwrap_or_default(),
            status: row.get::<String>(1).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(2).ok(),
            error: row.get::<String>(3).ok(),
        });
    }
    Ok(steps)
}

/// Total number of recorded runs (for `barca history` truncation reporting).
pub async fn count_runs(db_path: &str) -> Result<usize, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query("SELECT COUNT(*) FROM runs", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to count runs: {e}")))?;
    let n = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        .map(|r| r.get::<i64>(0).unwrap_or(0))
        .unwrap_or(0);
    Ok(n.max(0) as usize)
}

/// Get aggregated stats for a specific asset/node.
pub async fn get_asset_stats(db_path: &str, node_id: &str) -> Result<AssetStats, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;

    // Fetch all elapsed times for percentile computation.
    let mut all_elapsed: Vec<f64> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT elapsed_seconds FROM materializations WHERE node_id = ?1 AND elapsed_seconds IS NOT NULL AND elapsed_seconds > 0 ORDER BY elapsed_seconds",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query elapsed: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            if let Ok(e) = row.get::<f64>(0) {
                all_elapsed.push(e);
            }
        }
    }

    let total_runs: i64;
    {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM materializations WHERE node_id = ?1",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query count: {e}")))?;
        total_runs = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);
    }

    let avg_elapsed = if all_elapsed.is_empty() {
        None
    } else {
        Some(all_elapsed.iter().sum::<f64>() / all_elapsed.len() as f64)
    };
    let median_elapsed = percentile(&all_elapsed, 50.0);
    let max_elapsed = all_elapsed.last().copied();
    let p95_elapsed = percentile(&all_elapsed, 95.0);

    // Cache hit rate: rows with non-null, non-empty run_hash that appear more than once.
    let cache_hit_rate = if total_runs > 1 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM materializations WHERE node_id = ?1 AND run_hash != '' AND run_hash IN (SELECT run_hash FROM materializations WHERE node_id = ?1 GROUP BY run_hash HAVING COUNT(*) > 1)",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query cache hits: {e}")))?;
        let cached_count: i64 = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);
        cached_count as f64 / total_runs as f64
    } else {
        0.0
    };

    // Recent runs (last 10).
    let mut rows = conn
        .query(
            "SELECT elapsed_seconds, status, created_at, error_message, attempts FROM materializations WHERE node_id = ?1 ORDER BY id DESC LIMIT 10",
            [node_id.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query recent runs: {e}")))?;
    let mut recent_runs = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        recent_runs.push(AssetRunEntry {
            elapsed_seconds: row.get::<f64>(0).ok(),
            status: row
                .get::<String>(1)
                .unwrap_or_else(|_| "success".to_string()),
            created_at: row.get::<String>(2).unwrap_or_default(),
            error_message: row.get::<String>(3).ok(),
            attempts: row.get::<i64>(4).unwrap_or(1),
        });
    }

    Ok(AssetStats {
        node_id: node_id.to_string(),
        total_runs,
        avg_elapsed_seconds: avg_elapsed,
        median_elapsed_seconds: median_elapsed,
        max_elapsed_seconds: max_elapsed,
        p95_elapsed_seconds: p95_elapsed,
        cache_hit_rate,
        recent_runs,
    })
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
    let _g = db_guard().await;
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
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut out = HashMap::new();
    for base in base_ids {
        let prefix = format!("{base}[");
        let filter = "(node_id = ?1 OR substr(node_id, 1, length(?2)) = ?2)";
        let mut h = NodeHistory::default();
        // The last materialization is the latest run's (by row id). A partitioned node writes
        // one row per key in completion order, so within that run the first key in sort order
        // is reported, not whichever finished last (#475). Rows without a run id (an older
        // barca's) keep the latest row.
        let mut rows = conn
            .query(
                &format!(
                    "SELECT run_id FROM materializations WHERE {filter} ORDER BY id DESC LIMIT 1"
                ),
                [base.clone(), prefix.clone()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        let latest_run = match rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            Some(row) => row.get::<String>(0).ok().filter(|s| !s.is_empty()),
            None => None,
        };
        drop(rows);
        let columns = "SELECT node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
                       elapsed_seconds, status, error_message, created_at, output_hash FROM materializations";
        let (sql, params) = match latest_run {
            Some(run) => (
                format!(
                    "{columns} WHERE {filter} AND run_id = ?3 ORDER BY node_id ASC, id DESC LIMIT 1"
                ),
                vec![base.clone(), prefix.clone(), run],
            ),
            None => (
                format!("{columns} WHERE {filter} ORDER BY id DESC LIMIT 1"),
                vec![base.clone(), prefix.clone()],
            ),
        };
        let mut rows = conn
            .query(&sql, params)
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            let opt = |i: usize| row.get::<String>(i).ok().filter(|s| !s.is_empty());
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
        drop(rows);
        let mut rows = conn
            .query(
                &format!(
                    "SELECT 1 FROM materializations WHERE {filter} AND status = 'success' LIMIT 1"
                ),
                [base.clone(), prefix],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        h.ever_succeeded = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            .is_some();
        drop(rows);
        let mut rows = conn.query(
            &format!("SELECT node_id, run_hash, output_hash, created_at FROM materializations WHERE {filter} AND bytes_changed = 1 ORDER BY id DESC LIMIT 1"),
            [base.clone(), format!("{base}[")],
        ).await.map_err(|e| BarcaError::Db(format!("failed to query byte change: {e}")))?;
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
        out.insert(base.clone(), h);
    }
    Ok(out)
}

/// The latest successful artifact of every partition key of `base_id` (node ids
/// `<base_id>[<key>]`), as (partition node id, artifact path, artifact format), sorted by node id.
pub async fn latest_partition_artifacts(
    db_path: &str,
    base_id: &str,
) -> Result<Vec<(String, String, String)>, BarcaError> {
    latest_partition_artifacts_scoped(db_path, base_id, None).await
}

pub(crate) async fn partition_artifacts_for_config(
    cfg: &crate::config::ResolvedConfig,
    base_id: &str,
) -> Result<Vec<(String, String, String)>, BarcaError> {
    latest_partition_artifacts_scoped(
        &cfg.db_path,
        base_id,
        cfg.remote_off.then_some(cfg.local_artifact_dir.as_str()),
    )
    .await
}

async fn latest_partition_artifacts_scoped(
    db_path: &str,
    base_id: &str,
    local_dir: Option<&str>,
) -> Result<Vec<(String, String, String)>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    let prefix = format!("{base_id}[");
    let mut rows = conn
        .query(
            "SELECT node_id, artifact_path, artifact_format FROM materializations \
             WHERE substr(node_id, 1, length(?1)) = ?1 AND status = 'success' \
             AND artifact_path IS NOT NULL AND artifact_path != '' ORDER BY id DESC",
            [prefix],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    let mut seen = std::collections::BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        if local_dir.is_some_and(|root| {
            !crate::config::local_artifact(&row.get::<String>(1).unwrap_or_default(), root)
        }) {
            continue;
        }
        let node_id = row.get::<String>(0).unwrap_or_default();
        seen.entry(node_id).or_insert_with(|| {
            (
                row.get::<String>(1).unwrap_or_default(),
                row.get::<String>(2).unwrap_or_default(),
            )
        });
    }
    Ok(seen.into_iter().map(|(k, (p, f))| (k, p, f)).collect())
}

/// Compute the p-th percentile from a sorted slice of values.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        Some(sorted[lower])
    } else {
        let frac = rank - lower as f64;
        Some(sorted[lower] * (1.0 - frac) + sorted[upper] * frac)
    }
}

/// Look up the average elapsed_seconds for a list of node_ids.
/// Used for progress bar ETA estimation.
pub async fn get_avg_elapsed(
    db_path: &str,
    node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    if node_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    avg_elapsed_on(&conn, node_ids).await
}

/// How many values one `IN (...)` list binds: well under SQLite's parameter limit.
const IN_LIST_CHUNK: usize = 500;

/// [`get_avg_elapsed`] on an open connection: one statement per 500 nodes, not one per node.
async fn avg_elapsed_on(
    conn: &turso::Connection,
    node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    let mut result = HashMap::new();
    for chunk in node_ids.chunks(IN_LIST_CHUNK) {
        let marks = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut rows = conn
            .query(
                &format!(
                    "SELECT node_id, AVG(elapsed_seconds) FROM materializations \
                     WHERE node_id IN ({marks}) AND elapsed_seconds IS NOT NULL GROUP BY node_id"
                ),
                turso::params_from_iter(chunk.iter().cloned()),
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query avg elapsed: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            if let (Ok(node_id), Ok(avg)) = (row.get::<String>(0), row.get::<f64>(1)) {
                result.insert(node_id, avg);
            }
        }
    }
    Ok(result)
}

/// Look up the average elapsed_seconds for partitioned nodes using LIKE pattern matching.
/// For base_node_ids like "file.py:fetch", matches all "file.py:fetch[%]" in the DB.
/// Used for ETA estimation when steps carry partition_keys (late expansion).
pub async fn get_avg_elapsed_for_partitioned(
    db_path: &str,
    base_node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    if base_node_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    partitioned_avg_elapsed_on(&conn, base_node_ids).await
}

async fn partitioned_avg_elapsed_on(
    conn: &turso::Connection,
    base_node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    let mut result = HashMap::new();
    for nid in base_node_ids {
        let pattern = format!("{nid}[%]");
        let mut rows = conn
            .query(
                "SELECT AVG(elapsed_seconds) FROM materializations WHERE node_id LIKE ?1 AND elapsed_seconds IS NOT NULL",
                [pattern],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query avg elapsed: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            && let Ok(avg) = row.get::<f64>(0)
        {
            result.insert(nid.clone(), avg);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn cancelled_snapshot_copy_stops_between_chunks_and_cleans_partial_stage() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        std::fs::File::create(&source)
            .unwrap()
            .set_len(4 * 1024 * 1024)
            .unwrap();
        let stage_path = dir.path().join("stage.db");
        let cancel = tokio_util::sync::CancellationToken::new();
        let source_path = source.to_string_lossy();
        let target_path = stage_path.clone();
        let work = super::copy_snapshot_bytes(
            &source_path,
            &target_path,
            crate::helper_proc::Until::cancelled(&cancel),
        );
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = &mut work => panic!("copy finished without yielding between chunks"),
            _ = async {
                loop {
                    if std::fs::metadata(&stage_path).is_ok_and(|m| m.len() > 0) {break;}
                    tokio::task::yield_now().await;
                }
            } => {},
        }
        let copied = std::fs::metadata(&stage_path).unwrap().len();
        assert!(copied > 0 && copied < std::fs::metadata(&source).unwrap().len());
        cancel.cancel();
        assert!(matches!(work.await, Err(crate::BarcaError::Cancelled)));
        assert_eq!(std::fs::metadata(&source).unwrap().len(), 4 * 1024 * 1024);
    }
    /// #475: the keys of one run finish in any order, so the row id alone picks an arbitrary key
    /// as a partitioned node's last materialization. The rule: the latest run (by row id), and
    /// within it the first key in sort order; rows without a run id fall back to the row id.
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

    #[tokio::test]
    async fn run_inspection_selects_its_recorded_steps_and_failure_details() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for (id, node, status, error) in [
                ("first", "p.py:ok", "success", ""),
                ("first", "p.py:bad", "failed", "bad input"),
                ("other", "p.py:other", "success", ""),
            ] {
                conn.execute("INSERT INTO materializations (run_id,node_id,status,error_message,elapsed_seconds) VALUES (?1,?2,?3,NULLIF(?4,''),1.25)", [id,node,status,error]).await.unwrap();
            }
        }
        let steps = super::get_run_steps(&path, "first").await.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].node_id, "p.py:ok");
        assert_eq!(steps[1].status, "failed");
        assert_eq!(steps[1].error.as_deref(), Some("bad input"));
        assert_eq!(steps[1].elapsed_seconds, Some(1.25));
        assert!(
            super::get_run_steps(&path, "unknown")
                .await
                .unwrap()
                .is_empty()
        );
    }

    use super::*;

    #[tokio::test]
    async fn logs_round_trip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let lines = vec![
            ("a.py:load".to_string(), "loading…".to_string()),
            ("a.py:load".to_string(), "read 30/150".to_string()),
            ("a.py:load".to_string(), "done".to_string()),
        ];
        insert_logs(&db_path, "run123", &lines).await.unwrap();

        let got = get_logs(&db_path, "run123").await.unwrap();
        assert_eq!(got.len(), 3);
        // Order preserved via the seq column.
        assert_eq!(got[0].seq, 0);
        assert_eq!(got[0].line, "loading…");
        assert_eq!(got[1].line, "read 30/150");
        assert_eq!(got[2].line, "done");
        assert_eq!(got[0].node_id, "a.py:load");
    }

    #[tokio::test]
    async fn logs_are_scoped_by_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        insert_logs(&db_path, "runA", &[("n".to_string(), "a-line".to_string())])
            .await
            .unwrap();
        insert_logs(&db_path, "runB", &[("n".to_string(), "b-line".to_string())])
            .await
            .unwrap();

        let a = get_logs(&db_path, "runA").await.unwrap();
        let b = get_logs(&db_path, "runB").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].line, "a-line");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].line, "b-line");
        // Unknown run yields no rows, not an error.
        assert!(get_logs(&db_path, "nope").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn insert_empty_logs_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        insert_logs(&db_path, "run", &[]).await.unwrap();
        assert!(get_logs(&db_path, "run").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn round_trip_persist_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();

        init_db(&db_path).await.unwrap();

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "test.py:foo".to_string(),
            OutputRef {
                path: ".barca/artifacts/test.py--foo.json".to_string(),
                format: "json".to_string(),
                size_bytes: 15,
                elapsed_seconds: None,
                content_hash: None,
            },
        );
        persist_outputs(&db_path, &outputs, &HashMap::new())
            .await
            .unwrap();

        // Read it back.
        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1",
                ["test.py:foo".to_string()],
            )
            .await
            .unwrap();
        let result = rows.next().await.unwrap().map(|row| {
            (
                row.get::<String>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
            )
        });

        let (path, format, size) = result.unwrap();
        assert_eq!(path, ".barca/artifacts/test.py--foo.json");
        assert_eq!(format, "json");
        assert_eq!(size, 15);
    }

    #[tokio::test]
    async fn schedule_state_round_trips_and_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // Empty to start.
        assert!(get_schedule_state(&db_path).await.unwrap().is_empty());

        upsert_schedule_state(&db_path, "test.py:daily", 1_000)
            .await
            .unwrap();
        upsert_schedule_state(&db_path, "test.py:poll", 2_000)
            .await
            .unwrap();
        let state = get_schedule_state(&db_path).await.unwrap();
        assert_eq!(state.get("test.py:daily"), Some(&1_000));
        assert_eq!(state.get("test.py:poll"), Some(&2_000));

        // Upsert overwrites the same node rather than inserting a duplicate.
        upsert_schedule_state(&db_path, "test.py:daily", 3_000)
            .await
            .unwrap();
        let state = get_schedule_state(&db_path).await.unwrap();
        assert_eq!(state.len(), 2);
        assert_eq!(state.get("test.py:daily"), Some(&3_000));
    }

    #[tokio::test]
    async fn a_pause_is_kept_and_a_resume_moves_the_last_fired_record() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        upsert_schedule_state(&db_path, "test.py:daily", 1_000)
            .await
            .unwrap();
        let pause = SchedulePause {
            paused_at: 1_500,
            until: Some(9_000),
        };
        set_schedule_pause(&db_path, "test.py:daily", Some(pause), 1_500)
            .await
            .unwrap();
        // A job that never fired gets a row anchored at the pause.
        let forever = SchedulePause {
            paused_at: 1_600,
            until: None,
        };
        set_schedule_pause(&db_path, "test.py:new", Some(forever), 1_600)
            .await
            .unwrap();
        let pauses = get_schedule_pauses(&db_path).await.unwrap();
        assert_eq!(pauses["test.py:daily"], pause);
        assert_eq!(pauses["test.py:new"], forever);
        let state = get_schedule_state(&db_path).await.unwrap();
        assert_eq!(state["test.py:daily"], 1_000, "pausing does not fire");
        assert_eq!(state["test.py:new"], 1_600);
        set_schedule_pause(&db_path, "test.py:daily", None, 2_000)
            .await
            .unwrap();
        assert!(
            !get_schedule_pauses(&db_path)
                .await
                .unwrap()
                .contains_key("test.py:daily")
        );
        assert_eq!(
            get_schedule_state(&db_path).await.unwrap()["test.py:daily"],
            2_000,
            "the skipped ticks are not caught up"
        );
    }

    #[tokio::test]
    async fn a_run_records_its_trigger_and_a_served_run_its_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        let trigger = crate::outcome::Trigger::schedule("p.py:a", 0);
        create_run_with(
            &db_path,
            NewRun {
                run_id: "tick",
                command: "get",
                files: "[]",
                target: Some("p.py:a"),
                steps_total: Some(1),
                job: Some("p.py:a"),
                trigger: &trigger,
            },
        )
        .await
        .unwrap();
        let run = get_run(&db_path, "tick").await.unwrap().unwrap();
        assert_eq!(
            run.triggered_by,
            Some(crate::outcome::TriggeredBy::Schedule)
        );
        assert_eq!(run.scheduled_at.as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(run.schedule_id.as_deref(), Some("p.py:a"));
        assert!(get_run_outcome(&db_path, "tick").await.unwrap().is_none());
        assert!(
            set_run_outcome(&db_path, "tick", "success", r#"{"status":"success"}"#)
                .await
                .unwrap()
        );
        assert_eq!(
            get_run(&db_path, "tick").await.unwrap().unwrap().status,
            "success"
        );
        assert_eq!(
            get_run_outcome(&db_path, "tick").await.unwrap().as_deref(),
            Some(r#"{"status":"success"}"#)
        );
        // A run that never started still has a row, with its outcome.
        record_unstarted_run(
            &db_path,
            NewRun {
                run_id: "queued",
                command: "run",
                files: "[]",
                target: Some("t"),
                steps_total: None,
                job: None,
                trigger: &crate::outcome::Trigger::api(),
            },
            "cancelled",
            r#"{"status":"cancelled"}"#,
        )
        .await
        .unwrap();
        let run = get_run(&db_path, "queued").await.unwrap().unwrap();
        assert_eq!(run.status, "cancelled");
        assert_eq!(run.triggered_by, Some(crate::outcome::TriggeredBy::Api));
        assert!(run.finished_at.is_some());
        // An ordinary run is recorded as started by hand.
        create_run(&db_path, "cli", "get", "[]", None, Some(1))
            .await
            .unwrap();
        assert_eq!(
            get_run(&db_path, "cli")
                .await
                .unwrap()
                .unwrap()
                .triggered_by,
            Some(crate::outcome::TriggeredBy::Manual)
        );
    }

    #[tokio::test]
    async fn cost_estimates_round_trip_and_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        assert!(load_cost_estimates(&db_path).await.unwrap().is_empty());

        let est = |s: f64, n: u64| crate::cost::NodeEstimate {
            estimate_seconds: s,
            cpu_seconds: s * 0.9,
            max_rss_bytes: 1024,
            samples: n,
        };
        upsert_cost_estimates(
            &db_path,
            &[
                ("f.py:fetch[t=A]".to_string(), est(0.25, 3)),
                ("f.py:report".to_string(), est(2.0, 1)),
            ],
        )
        .await
        .unwrap();

        let loaded = load_cost_estimates(&db_path).await.unwrap();
        assert_eq!(loaded.len(), 2);
        let fetch = loaded.iter().find(|(n, _)| n == "f.py:fetch[t=A]").unwrap();
        assert!((fetch.1.estimate_seconds - 0.25).abs() < 1e-9);
        assert_eq!(fetch.1.samples, 3);
        assert_eq!(fetch.1.max_rss_bytes, 1024);

        // Upsert overwrites the same node rather than inserting a duplicate.
        upsert_cost_estimates(&db_path, &[("f.py:report".to_string(), est(1.5, 2))])
            .await
            .unwrap();
        let loaded = load_cost_estimates(&db_path).await.unwrap();
        assert_eq!(loaded.len(), 2);
        let report = loaded.iter().find(|(n, _)| n == "f.py:report").unwrap();
        assert!((report.1.estimate_seconds - 1.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn schema_has_timing_columns() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query("PRAGMA table_info(materializations)", ())
            .await
            .unwrap();
        let mut columns = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            columns.push(row.get::<String>(1).unwrap());
        }
        assert!(columns.contains(&"cpu_seconds".to_string()));
        assert!(columns.contains(&"max_rss_bytes".to_string()));
    }

    #[tokio::test]
    async fn a_running_run_whose_process_is_gone_is_reported_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // A pid that existed and is certainly gone: a child that has been reaped.
        let mut child =
            crate::helper_proc::spawn_std(&mut std::process::Command::new("true")).unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();

        // Ours, still going (create_run records this process and host).
        create_run(&db_path, "alive", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            let host = local_host();
            for (run_id, pid, host, status) in [
                ("dead", Some(dead_pid), host.as_str(), "running"),
                ("elsewhere", Some(dead_pid), "another-machine", "running"),
                ("done", Some(dead_pid), host.as_str(), "success"),
                ("old-version", None, "", "running"),
            ] {
                conn.execute(
                    "INSERT INTO runs (run_id, command, files, status, pid, host) VALUES (?1, 'get', 'f.py', ?2, NULLIF(?3, ''), NULLIF(?4, ''))",
                    [
                        run_id.to_string(),
                        status.to_string(),
                        pid.map(|p| p.to_string()).unwrap_or_default(),
                        host.to_string(),
                    ],
                )
                .await
                .unwrap();
            }
        }

        let status_of: HashMap<String, RunRecord> = get_recent_runs(&db_path, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.run_id.clone(), r))
            .collect();
        assert_eq!(status_of["dead"].status, "interrupted");
        // Nobody saw it end, so it has no finish time.
        assert_eq!(status_of["dead"].finished_at, None);
        assert_eq!(status_of["dead"].elapsed_seconds, None);
        assert_eq!(status_of["alive"].status, "running");
        // Another machine's pid means nothing here; neither does a row with no pid.
        assert_eq!(status_of["elsewhere"].status, "running");
        assert_eq!(status_of["old-version"].status, "running");
        assert_eq!(status_of["done"].status, "success");

        // Reading history reported it without writing; the next run to start records it.
        let stored = |run_id: &'static str| {
            let db_path = db_path.clone();
            async move {
                let _g = db_guard().await;
                let (_db, conn) = open_conn(&db_path).await.unwrap();
                let mut rows = conn
                    .query("SELECT status FROM runs WHERE run_id = ?1", [run_id])
                    .await
                    .unwrap();
                rows.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<String>(0)
                    .unwrap()
            }
        };
        assert_eq!(stored("dead").await, "running");
        create_run(&db_path, "next", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored("dead").await, "interrupted");
        assert_eq!(stored("elsewhere").await, "running");
        assert_eq!(stored("alive").await, "running");
    }

    // ─── who is running a `running` run (#290) ───────────────────────────────

    use crate::run_owner::{self, Here, Identity, Marker, Observer, Owner, Process};

    /// Insert a `running` run as some process recorded it.
    async fn add_running(db_path: &str, run_id: &str, pid: u32, host: &str, owner: &str) {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(db_path).await.unwrap();
        conn.execute(
            "INSERT INTO runs (run_id, command, files, status, pid, host, owner) VALUES (?1, 'get', 'f.py', 'running', ?2, ?3, NULLIF(?4, ''))",
            [
                run_id.to_string(),
                pid.to_string(),
                host.to_string(),
                owner.to_string(),
            ],
        )
        .await
        .unwrap();
    }

    async fn stored_status(db_path: &str, run_id: &str) -> String {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(db_path).await.unwrap();
        let mut rows = conn
            .query("SELECT status FROM runs WHERE run_id = ?1", [run_id])
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get::<String>(0).unwrap()
    }

    async fn reported(db_path: &str) -> HashMap<String, String> {
        get_recent_runs(db_path, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.run_id, r.status))
            .collect()
    }

    /// A pid that existed and is certainly gone: a child that has been reaped.
    fn a_dead_pid() -> u32 {
        let mut child =
            crate::helper_proc::spawn_std(&mut std::process::Command::new("true")).unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    async fn metadata_db(dir: &tempfile::TempDir) -> String {
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        db_path
    }

    fn marker_files(dir: &tempfile::TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path().join("run-owners"))
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// What the real machine says of itself, as an identity a run here would record.
    fn this_process(fifo: Option<run_owner::FifoId>) -> Identity {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("metadata.db").display().to_string();
        let owner = run_owner::claim(&db, "probe");
        run_owner::release(&db, "probe");
        let mut id: Identity = serde_json::from_str(&owner).expect("this kernel has a boot id");
        id.fifo = fifo;
        id
    }

    fn json(id: &Identity) -> String {
        serde_json::to_string(id).unwrap()
    }

    #[tokio::test]
    async fn a_run_killed_in_a_container_is_interrupted_after_the_restart() {
        // #290. The coordinator was pid 1 of a container; the container that replaces it has
        // another host name and a pid 1 of its own. By pid and host the killed run looked
        // alive and foreign, and stayed `running` for ever. Here: the same kernel, the pid
        // namespace number handed out again, and a pid 1 that started later.
        let run = Identity {
            boot: "the-kernel".into(),
            pidns: "pid:[4026532980]".into(),
            timens: "time:[4026531834]".into(),
            start: Some(1_000),
            ..Identity::default()
        };
        let rows = [RunningRun {
            run_id: "killed".into(),
            pid: Some(1),
            host: "container-1".into(),
            owner: json(&run),
            command: "get".into(),
            job: String::new(),
        }];
        let restarted = run_owner::tests::Fake {
            here: Here {
                boot: "the-kernel".into(),
                machine: String::new(),
                host: "container-2".into(),
                pidns: "pid:[4026532980]".into(),
                timens: "time:[4026531834]".into(),
                handles: true,
            },
            process: Process::Present(Some(9_000)),
            marker: Marker::Missing,
        };
        // `pid_alive` is what the old rule asked; it says the pid exists.
        assert_eq!(interrupted_among(&rows, &restarted, |_| true), ["killed"]);

        // The same container still going, seen from inside (`docker exec`): the same pid 1.
        let mut inside = restarted.clone();
        inside.process = Process::Present(Some(1_000));
        assert!(interrupted_among(&rows, &inside, |_| false).is_empty());

        // A new container that got another namespace number cannot look the pid up. It
        // takes the marker, where the filesystem lets it be verified...
        let mut with_marker = run.clone();
        with_marker.fifo = Some(run_owner::FifoId {
            token: run_owner::tests::T.into(),
            dev: 5,
            ino: 6,
            handle: Some("1:00ff".into()),
            fs: "volume".into(),
            local: true,
        });
        let rows = [RunningRun {
            owner: json(&with_marker),
            ..rows[0].clone()
        }];
        let mut elsewhere = restarted.clone();
        elsewhere.here.pidns = "pid:[4026533111]".into();
        let seen = |handle: Option<&str>, reader| Marker::Present {
            dev: 5,
            ino: 6,
            handle: handle.map(String::from),
            reader: Some(reader),
            fs: "volume".into(),
            local: true,
        };
        elsewhere.marker = seen(Some("1:00ff"), false);
        assert_eq!(interrupted_among(&rows, &elsewhere, |_| true), ["killed"]);
        // ...not while the first container is still running next to it...
        elsewhere.marker = seen(Some("1:00ff"), true);
        assert!(interrupted_among(&rows, &elsewhere, |_| false).is_empty());
        // ...and not where the file cannot be shown to be the one the owner held.
        elsewhere.marker = seen(None, false);
        assert!(interrupted_among(&rows, &elsewhere, |_| false).is_empty());
    }

    #[tokio::test]
    async fn a_live_run_that_cannot_be_observed_stays_running_and_keeps_its_marker() {
        // The review's failure, with the real files. A run is going on the host; its marker
        // is in the project directory, which a container mounts. The container is another
        // kernel: to it the marker FIFO has no reader and the pid does not exist. It must
        // report the run as `running`, record nothing, and leave the marker alone, also
        // when it starts a run of its own, and also long after.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        // The host's process: alive for the whole test.
        let (fifo, _host_process) = run_owner::tests::other_process(&db_path);
        let marker = format!("{}.fifo", fifo.token);
        let mut host_run = this_process(Some(fifo));
        // This test process stands for the container: the run is from another kernel. As
        // in the review's attack, the reader has the owner's machine id and host name
        // (`this_process` recorded this machine's), and the pid is dead here.
        host_run.boot = "the-host-kernel".into();
        add_running(
            &db_path,
            "host-run",
            a_dead_pid(),
            &local_host(),
            &json(&host_run),
        )
        .await;

        assert_eq!(reported(&db_path).await["host-run"], "running");
        create_run(&db_path, "container-run", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored_status(&db_path, "host-run").await, "running");
        assert!(marker_files(&dir).contains(&marker));

        // The same with every answer the container's kernel could give about a process and
        // a file it sees through a mount that is not a local disk: no such pid, nobody at
        // the other end of the marker, whatever inode numbers and filesystem id it shows.
        let rows = {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            running_runs(&conn).await
        };
        let host_row: Vec<RunningRun> = rows
            .into_iter()
            .filter(|r| r.run_id == "host-run")
            .collect();
        let real = run_owner::System::for_db(&db_path);
        let Marker::Present {
            dev,
            ino,
            handle,
            fs,
            ..
        } = real.marker(&host_run.fifo.as_ref().unwrap().token)
        else {
            panic!("the marker is there");
        };
        for process in [
            Process::Absent,
            Process::Present(None),
            Process::Present(Some(1)),
        ] {
            for reader in [Some(false), None] {
                for (handle, fs) in [(handle.clone(), fs.clone()), (None, String::new())] {
                    let blind = run_owner::tests::Fake {
                        here: real.here(),
                        process,
                        marker: Marker::Present {
                            dev,
                            ino,
                            handle,
                            reader,
                            fs,
                            local: false,
                        },
                    };
                    assert!(interrupted_among(&host_row, &blind, |_| false).is_empty());
                }
            }
        }

        // Hours later the container starts another run: the marker is still named by a
        // `running` row, so the sweep leaves it.
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            let named = running_runs(&conn)
                .await
                .iter()
                .filter_map(|run| run_owner::token_of(&run.owner))
                .collect();
            let later = SystemTime::now() + Duration::from_secs(48 * 3600);
            run_owner::sweep(&db_path, &named, later);
        }
        assert!(marker_files(&dir).contains(&marker));
        finish_run(&db_path, "container-run", "success", 1, 0, 0.1)
            .await
            .unwrap();
        assert_eq!(
            marker_files(&dir),
            [marker],
            "the container removed its own, only"
        );
        assert_eq!(reported(&db_path).await["host-run"], "running");
    }

    #[tokio::test]
    async fn a_run_whose_owner_died_next_door_is_interrupted_once_its_marker_has_no_reader() {
        // Another pid namespace on this kernel (a second container on the same volume): the
        // pid cannot be looked up, the marker can. Real files, real kernel.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        let (fifo, process) = run_owner::tests::other_process(&db_path);
        // Where this filesystem gives no file handle (Linux), the marker proves nothing
        // and the run must simply stay `running`.
        let provable = if cfg!(target_os = "macos") {
            fifo.local
        } else {
            fifo.handle.is_some()
        };
        let marker = format!("{}.fifo", fifo.token);
        let mut next_door = this_process(Some(fifo));
        next_door.pidns = "pid:[another-container]".into();
        add_running(
            &db_path,
            "theirs",
            std::process::id(),
            "container-1",
            &json(&next_door),
        )
        .await;

        assert_eq!(reported(&db_path).await["theirs"], "running");
        create_run(&db_path, "ours", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored_status(&db_path, "theirs").await, "running");

        // It is killed.
        drop(process);
        if !provable {
            assert_eq!(reported(&db_path).await["theirs"], "running");
            return;
        }
        let system = run_owner::System::for_db(&db_path);
        run_owner::tests::eventually("the owner is seen to be gone", || {
            run_owner::judge(None, &next_door, &system) == Owner::Gone
        });
        assert_eq!(reported(&db_path).await["theirs"], "interrupted");
        // Reading reported it and wrote nothing; the next run to start records it.
        assert_eq!(stored_status(&db_path, "theirs").await, "running");
        create_run(&db_path, "next", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored_status(&db_path, "theirs").await, "interrupted");
        assert_eq!(stored_status(&db_path, "ours").await, "running");
        // Its marker is not named by a `running` row any more: it goes once it is old.
        assert!(marker_files(&dir).contains(&marker));
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            let named = running_runs(&conn)
                .await
                .iter()
                .filter_map(|run| run_owner::token_of(&run.owner))
                .collect();
            run_owner::sweep(
                &db_path,
                &named,
                SystemTime::now() + Duration::from_secs(7200),
            );
        }
        assert_eq!(marker_files(&dir).len(), 1, "this process's own marker");
        assert!(!marker_files(&dir).contains(&marker));
    }

    #[tokio::test]
    async fn a_run_from_before_a_restart_of_this_machine_is_interrupted() {
        // What main reports by host name and pid, established by observation instead: the
        // marker is on this machine's local disk, the same filesystem and inode the owner
        // recorded under another boot id, and nobody on this kernel holds it.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        let (fifo, process) = run_owner::tests::other_process(&db_path);
        // A temp dir that is not on a local disk (a container's overlay) proves nothing.
        let provable = fifo.local && !fifo.fs.is_empty();
        let mut before = this_process(Some(fifo));
        before.boot = "the-boot-before".into();
        add_running(
            &db_path,
            "then",
            std::process::id(),
            "any-host",
            &json(&before),
        )
        .await;

        // While somebody holds the marker, the kernel it was created on is still running
        // (this one, in the test): no restart can have happened.
        assert_eq!(reported(&db_path).await["then"], "running");
        drop(process);
        let system = run_owner::System::for_db(&db_path);
        if provable {
            run_owner::tests::eventually("the marker has no reader", || {
                run_owner::judge(None, &before, &system) == Owner::Gone
            });
            assert_eq!(reported(&db_path).await["then"], "interrupted");
        } else {
            assert_eq!(reported(&db_path).await["then"], "running");
        }
    }

    #[tokio::test]
    async fn only_runs_whose_owner_is_alive_hold_a_schedule_back() {
        // #479: `barca serve` skips a tick while a CLI run of the node is going. A run
        // records its job when it starts, not only at the end.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        create_run_with(
            &db_path,
            NewRun {
                run_id: "mine",
                command: "get",
                files: "[]",
                target: Some("a"),
                steps_total: Some(1),
                job: Some("p.py:a"),
                trigger: &crate::outcome::Trigger::default(),
            },
        )
        .await
        .unwrap();
        // Another process on this host, still going (a row as an older barca writes it).
        let mut other =
            crate::helper_proc::spawn_std(std::process::Command::new("sleep").arg("30")).unwrap();
        add_running(&db_path, "cli", other.id(), &local_host(), "").await;
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            conn.execute("UPDATE runs SET job = 'p.py:a' WHERE run_id = 'cli'", ())
                .await
                .unwrap();
        }
        // Killed without recording an outcome: its owner is gone.
        add_running(
            &db_path,
            "stale",
            a_dead_pid(),
            &local_host(),
            &json(&this_process(None)),
        )
        .await;
        // Another machine's row, restored from a backup: nobody here can tell.
        let mut theirs = this_process(None);
        theirs.boot = "a-kernel-somewhere".into();
        add_running(&db_path, "foreign", 1, "elsewhere", &json(&theirs)).await;
        // An older barca's row, its pid gone.
        add_running(&db_path, "old", a_dead_pid(), &local_host(), "").await;
        // This process's own row ("mine") is left out: a server knows its runs from memory.
        assert_eq!(
            alive_running_runs(&db_path).await,
            [AliveRun {
                run_id: "cli".into(),
                command: "get".into(),
                job: "p.py:a".into(),
            }]
        );
        other.kill().ok();
        other.wait().ok();
        assert!(alive_running_runs(&db_path).await.is_empty());
        finish_run(&db_path, "mine", "success", 1, 0, 0.1)
            .await
            .ok();
    }

    #[tokio::test]
    async fn a_run_from_another_machine_is_never_marked_interrupted() {
        // A restored backup can bring another machine's `running` rows into this database,
        // and such a run may be going right now. Even with this machine's host name and a pid
        // that is dead here (which by pid and host alone read as interrupted).
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        let mut theirs = this_process(None);
        theirs.boot = "a-kernel-somewhere".into();
        for (run_id, machine) in [("twin", theirs.machine.clone()), ("other", "m-2".into())] {
            theirs.machine = machine;
            add_running(
                &db_path,
                run_id,
                a_dead_pid(),
                &local_host(),
                &json(&theirs),
            )
            .await;
        }
        create_run(&db_path, "ours", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        for run_id in ["twin", "other"] {
            assert_eq!(reported(&db_path).await[run_id], "running");
            assert_eq!(stored_status(&db_path, run_id).await, "running");
        }
    }

    #[tokio::test]
    async fn a_run_records_who_started_it_and_gives_up_its_marker_when_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        create_run(&db_path, "r1", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        let rows = {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            running_runs(&conn).await
        };
        assert_eq!(rows.len(), 1);
        let id: Identity = serde_json::from_str(&rows[0].owner).unwrap();
        assert!(!id.boot.is_empty());
        let token = id.fifo.as_ref().expect("a marker").token.clone();
        assert_eq!(marker_files(&dir), [format!("{token}.fifo")]);
        let system = run_owner::System::for_db(&db_path);
        assert_eq!(run_owner::judge(rows[0].pid, &id, &system), Owner::Alive);
        assert_eq!(reported(&db_path).await["r1"], "running");

        finish_run(&db_path, "r1", "success", 1, 0, 0.1)
            .await
            .unwrap();
        assert!(marker_files(&dir).is_empty());
    }

    #[tokio::test]
    async fn history_of_a_database_an_older_barca_wrote_is_read_without_the_owner_column() {
        // `barca history` does not migrate what it reads. A 0.18 database has pid and host
        // and no owner: its killed run is still reported, by pid and host.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            conn.execute(
                "CREATE TABLE runs (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT UNIQUE NOT NULL, \
                 command TEXT NOT NULL, files TEXT NOT NULL, target TEXT, \
                 status TEXT NOT NULL DEFAULT 'running', steps_total INTEGER, \
                 steps_executed INTEGER DEFAULT 0, steps_cached INTEGER DEFAULT 0, \
                 started_at TEXT DEFAULT (datetime('now')), finished_at TEXT, elapsed_seconds REAL, \
                 pid INTEGER, host TEXT)",
                (),
            )
            .await
            .unwrap();
            for (run_id, pid) in [("killed", a_dead_pid()), ("going", std::process::id())] {
                conn.execute(
                    "INSERT INTO runs (run_id, command, files, pid, host) VALUES (?1, 'get', 'f.py', ?2, ?3)",
                    [run_id.to_string(), pid.to_string(), local_host()],
                )
                .await
                .unwrap();
            }
        }
        let status = reported(&db_path).await;
        assert_eq!(status["killed"], "interrupted");
        assert_eq!(status["going"], "running");
        let history = get_recent_runs(&db_path, 10).await.unwrap();
        assert!(history.iter().all(|run| run.attempts.is_none()));
    }

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

    // ─── OutputRef artifact persistence tests ────────────────────────────────

    #[tokio::test]
    async fn round_trip_persist_output_ref() {
        use crate::dispatch::OutputRef;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "test.py:foo".to_string(),
            OutputRef {
                path: ".barca/artifacts/test.py--foo.json".to_string(),
                format: "json".to_string(),
                size_bytes: 42,
                elapsed_seconds: None,
                content_hash: None,
            },
        );

        persist_output_refs(&db_path, &outputs).await;

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1",
                ["test.py:foo".to_string()],
            )
            .await
            .unwrap();
        let result = rows.next().await.unwrap().map(|row| {
            (
                row.get::<String>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
            )
        });

        let (path, format, size) = result.unwrap();
        assert_eq!(path, ".barca/artifacts/test.py--foo.json");
        assert_eq!(format, "json");
        assert_eq!(size, 42);
    }

    #[tokio::test]
    async fn persist_multiple_output_refs() {
        use crate::dispatch::OutputRef;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "a.py:json_asset".to_string(),
            OutputRef {
                path: ".barca/artifacts/a--json_asset.json".to_string(),
                format: "json".to_string(),
                size_bytes: 100,
                elapsed_seconds: None,
                content_hash: None,
            },
        );
        outputs.insert(
            "a.py:df_asset".to_string(),
            OutputRef {
                path: ".barca/artifacts/a--df_asset.parquet".to_string(),
                format: "parquet".to_string(),
                size_bytes: 8192,
                elapsed_seconds: None,
                content_hash: None,
            },
        );
        outputs.insert(
            "a.py:obj_asset".to_string(),
            OutputRef {
                path: ".barca/artifacts/a--obj_asset.pkl".to_string(),
                format: "pickle".to_string(),
                size_bytes: 512,
                elapsed_seconds: None,
                content_hash: None,
            },
        );

        persist_output_refs(&db_path, &outputs).await;

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query("SELECT COUNT(*) FROM materializations", ())
            .await
            .unwrap();
        let count = rows
            .next()
            .await
            .unwrap()
            .map(|row| row.get::<i64>(0).unwrap())
            .unwrap();

        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn schema_has_artifact_columns() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query("PRAGMA table_info(materializations)", ())
            .await
            .unwrap();
        let mut columns = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            columns.push(row.get::<String>(1).unwrap());
        }

        assert!(columns.contains(&"artifact_path".to_string()));
        assert!(columns.contains(&"artifact_format".to_string()));
        assert!(columns.contains(&"artifact_size_bytes".to_string()));
        // Error/retry tracking columns.
        assert!(columns.contains(&"error_message".to_string()));
        assert!(columns.contains(&"error_traceback".to_string()));
        assert!(columns.contains(&"attempts".to_string()));
        // Old column still exists for backward compat
        assert!(columns.contains(&"output_json".to_string()));
    }

    #[tokio::test]
    async fn failed_row_round_trips_and_is_excluded_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        // Insert a failed materialization.
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, status, error_message, error_traceback, attempts) VALUES (?1, ?2, 'failed', ?3, ?4, ?5)",
            ["f:boom".to_string(), "rh1".to_string(), "kaboom".to_string(), "Traceback…".to_string(), "3".to_string()],
        )
        .await
        .unwrap();

        // Read it back.
        let mut rows = conn
            .query(
                "SELECT status, error_message, attempts FROM materializations WHERE node_id = ?1",
                ["f:boom".to_string()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let status = row.get::<String>(0).unwrap();
        let msg = row.get::<String>(1).unwrap();
        let attempts = row.get::<i64>(2).unwrap();

        // The cache lookup filters on status='success' — a failed row is never served.
        let mut hit_rows = conn
            .query(
                "SELECT artifact_path FROM materializations WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
                ["f:boom".to_string(), "rh1".to_string()],
            )
            .await
            .unwrap();
        let success_hits = hit_rows.next().await.unwrap().is_some();

        assert_eq!(status, "failed");
        assert_eq!(msg, "kaboom");
        assert_eq!(attempts, 3);
        assert!(
            !success_hits,
            "failed rows must not satisfy the cache lookup"
        );
    }

    #[tokio::test]
    async fn cache_lookup_returns_output_ref() {
        use crate::dispatch::OutputRef;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // Persist with a run_hash.
        {
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
                [
                    "test.py:foo".to_string(),
                    "abc123".to_string(),
                    ".barca/artifacts/test.py--foo.parquet".to_string(),
                    "parquet".to_string(),
                    "4096".to_string(),
                ],
            )
            .await
            .unwrap();
        }

        // Look it up by node_id + run_hash — should return OutputRef.
        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1 AND run_hash = ?2 ORDER BY id DESC LIMIT 1",
                ["test.py:foo".to_string(), "abc123".to_string()],
            )
            .await
            .unwrap();
        let result = rows.next().await.unwrap().map(|row| OutputRef {
            path: row.get::<String>(0).unwrap(),
            format: row.get::<String>(1).unwrap(),
            size_bytes: row.get::<i64>(2).unwrap() as u64,
            elapsed_seconds: None,
            content_hash: None,
        });

        let output_ref = result.unwrap();
        assert_eq!(output_ref.path, ".barca/artifacts/test.py--foo.parquet");
        assert_eq!(output_ref.format, "parquet");
        assert_eq!(output_ref.size_bytes, 4096);
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
    fn turso_lock_errors_get_an_actionable_hint() {
        let hinted = db_open_error(
            "Locking error: Failed locking file '.barca/metadata.db'. File is locked by another process",
        )
        .to_string();
        assert!(hinted.contains("File is locked by another process"));
        assert!(hinted.contains("outside barca"), "{hinted}");
        let other = db_open_error("disk I/O error").to_string();
        assert!(!other.contains("outside barca"), "{other}");
    }
    #[tokio::test]
    async fn a_log_write_failure_rolls_back_the_stream_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.db").to_string_lossy().to_string();
        init_db(&path).await.unwrap();
        {
            let _guard = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute("CREATE UNIQUE INDEX injected_log_failure ON logs(line)", ())
                .await
                .unwrap();
        }
        let lines = vec![
            ("p.py:a".into(), "same".into()),
            ("p.py:a".into(), "same".into()),
        ];
        assert!(insert_logs(&path, "r", &lines).await.is_err());
        assert!(get_logs(&path, "r").await.unwrap().is_empty());
        {
            let _guard = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute("DROP INDEX injected_log_failure", ())
                .await
                .unwrap();
        }
        insert_logs(&path, "r", &lines).await.unwrap();
        insert_logs(&path, "r", &lines).await.unwrap();
        assert_eq!(get_logs(&path, "r").await.unwrap().len(), 2);
    }
}

#[cfg(test)]
mod fold_log_tests {
    use turso::Builder;

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

        // Create a table and write enough rows that data definitely lives in the WAL.
        {
            let db = Builder::new_local(&db_path).build().await.unwrap();
            let conn = db.connect().unwrap();
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
        }

        // Sanity: without a checkpoint the WAL holds the data.
        let wal = format!("{db_path}-wal");
        let wal_size_before = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert!(
            wal_size_before > 0,
            "expected data in the WAL before checkpoint (got {wal_size_before} bytes) — \
             if this fails, turso started auto-checkpointing and the sync design should be revisited"
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
