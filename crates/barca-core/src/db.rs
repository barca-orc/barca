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

/// The live metadata DB, opened for reading only: a path (a symbolic link to it in a temp
/// dir, removed on drop) that every reader in this module accepts in place of the database.
///
/// A [`DbSnapshot`] copies the database, which costs its size on every change (gigabytes for
/// a long history, written on Linux in full) and starts every read with a cold page cache. A
/// view instead reads the database in place: in WAL mode SQLite lets readers in any process
/// see the last commit while one writer appends, so a view takes neither barca's
/// cross-process lock nor the process-wide guard, and never waits for a run's writes or makes
/// one wait. Its connections are read-only (they never write, migrate or checkpoint) and run
/// each query in its own transaction, so a checkpoint ([`fold_log`]) waits at most for one
/// query. SQLite resolves the link, so the view shares the database's `-wal` and `-shm`.
///
/// Only a database this release uses as it is can be read in place: [`ReadView::open`]
/// answers `None` for one that is missing, not in WAL mode or would be migrated, and the
/// caller takes a [`DbSnapshot`] instead.
pub struct ReadView {
    path: String,
    _dir: tempfile::TempDir,
}

impl ReadView {
    pub async fn open(db_path: &str) -> Result<Option<Self>, BarcaError> {
        let Ok(target) = fs::canonicalize(db_path) else {
            return Ok(None);
        };
        if unusable_file(db_path).is_some() || empty_file(db_path) {
            return Ok(None);
        }
        let dir = tempfile::tempdir()
            .map_err(|e| BarcaError::Db(format!("failed to create read view dir: {e}")))?;
        let link = dir.path().join("metadata.db");
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|e| BarcaError::Db(format!("failed to create read view: {e}")))?;
        let path = link.display().to_string();
        let (db, conn) = connect_view(&path).await.map_err(|e| named(e, db_path))?;
        let usable = view_usable(&conn).await;
        drop(conn);
        drop(db);
        if !usable {
            return Ok(None);
        }
        READ_VIEWS.lock().unwrap().insert(path.clone());
        Ok(Some(Self { path, _dir: dir }))
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for ReadView {
    fn drop(&mut self) {
        READ_VIEWS.lock().unwrap().remove(&self.path);
    }
}

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

/// Whether the database behind `conn` is read as it is: this release's format with nothing
/// to add, in WAL mode.
async fn view_usable(conn: &libsql::Connection) -> bool {
    if !matches!(crate::db_schema::read_version(conn).await, Ok(v) if v == crate::db_schema::VERSION)
    {
        return false;
    }
    let Ok(schema) = crate::state_validate::Schema::read(conn).await else {
        return false;
    };
    if crate::db_schema::additions_missing_in(&schema) {
        return false;
    }
    let Ok(mut rows) = conn.query("PRAGMA journal_mode", ()).await else {
        return false;
    };
    let mode = match rows.next().await {
        Ok(Some(row)) => row.get::<String>(0).ok(),
        _ => None,
    };
    mode.is_some_and(|mode| mode.eq_ignore_ascii_case("wal"))
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
    /// The run's job: `all` for a run without targets, otherwise its resolved node ids, sorted
    /// and comma-separated (`barca.job` in telemetry). `null` for runs recorded before 0.29.0.
    #[serde(default)]
    pub job: Option<String>,
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
    /// For a run with a failed step: the first step that failed and the first line of its
    /// error. `null` for every other run (also for a run that failed before any step did).
    #[serde(default)]
    pub failure: Option<RunFailure>,
}

/// A recorded step error as (type, message, traceback). A worker reports a Python exception as
/// the generic `WorkerError` whose text is `Type: message` followed by the traceback frames;
/// this takes the type and the frames back out of it.
pub fn split_exception(
    error_type: &str,
    message: &str,
    traceback: &str,
) -> (String, String, Option<String>) {
    // The frames are the trailing run of `  File "..."` lines and their indented source lines.
    // Taking only that run keeps a message that itself quotes a traceback in one piece.
    let lines: Vec<&str> = message.split('\n').collect();
    let mut first_frame = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        if line.starts_with("  File \"") {
            first_frame = i;
        } else if !line.starts_with("    ") {
            break;
        }
    }
    let joined;
    let (text, frames) = if first_frame > 0 && first_frame < lines.len() {
        joined = lines[..first_frame].join("\n");
        (joined.as_str(), Some(lines[first_frame..].join("\n")))
    } else {
        (message, None)
    };
    let stack = Some(traceback.to_string())
        .filter(|t| !t.is_empty())
        .or(frames);
    if error_type == "WorkerError"
        && let Some((head, rest)) = text.split_once(": ")
        && !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return (head.to_string(), rest.to_string(), stack);
    }
    (error_type.to_string(), text.to_string(), stack)
}

/// Why a run failed, in one line: its first failed step and that step's error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunFailure {
    /// The node id of the step, with its partition key when it has one.
    pub node: String,
    /// `ErrorType: message`, the message's first line only.
    pub error: String,
}

impl RunFailure {
    /// From a failed row's `error_type` and `error_message` (see [`split_exception`]).
    pub fn new(node: String, error_type: &str, message: &str) -> Self {
        let (error_type, message, _) = split_exception(error_type, message, "");
        let first = message.lines().next().unwrap_or_default().trim_end();
        let error = match (error_type.is_empty(), first.is_empty()) {
            (true, _) => first.to_string(),
            (false, true) => error_type.to_string(),
            (false, false) => format!("{error_type}: {first}"),
        };
        Self { node, error }
    }
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
    /// The share of executions that repeated an earlier execution's run hash (not cache hits,
    /// which execute nothing and are not recorded per asset).
    pub cache_hit_rate: f64,
    /// The largest peak RSS any recorded execution of this node reached, in bytes (#627): the
    /// worker process's peak while the step ran. `null` when none was recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_rss_bytes: Option<i64>,
    /// The median of the recorded executions' peak RSS, in bytes. `null` when none was
    /// recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub median_max_rss_bytes: Option<i64>,
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
    /// The run that recorded this execution (`barca history RUN_ID`); `null` on rows from
    /// before barca recorded it.
    #[serde(default)]
    pub run_id: Option<String>,
    /// CPU seconds the step used (user and system, the worker process's own).
    #[serde(default)]
    pub cpu_seconds: Option<f64>,
    /// The worker process's peak RSS while the step ran, in bytes; `null` when not recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_rss_bytes: Option<i64>,
    /// A partitioned node's entry is one run: the keys it recorded, and how many failed.
    /// Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub keys: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub failed_keys: Option<i64>,
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
    let tracing = std::env::var_os("BARCA_TRACE_TIMING").is_some();
    let hashing = Instant::now();
    let digest = file_sha256(Path::new(db_path))
        .map_err(|e| BarcaError::Db(format!("failed to read {db_path} for a backup: {e}")))?;
    let bytes = tracing.then(|| fs::metadata(db_path).map(|m| m.len()).unwrap_or(0));
    if let Some(bytes) = bytes {
        crate::errln!(
            "[timing] metadata backup hash: {bytes} bytes in {:.3} ms",
            hashing.elapsed().as_secs_f64() * 1000.0
        );
    }
    if unless == Some(digest.as_str()) {
        return Ok(None);
    }
    let copying = Instant::now();
    let copied = copy_snapshot_bytes(db_path, copy, until).await;
    if let Some(bytes) = bytes {
        crate::errln!(
            "[timing] metadata backup copy: {bytes} bytes in {:.3} ms (complete={})",
            copying.elapsed().as_secs_f64() * 1000.0,
            copied.is_ok()
        );
    }
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

/// A short-lived handle for one phase's cache lookups. It holds the in-process guard
/// *and* the cross-process lock for as long as it lives, so open it for the lookups
/// and drop it before any step runs: another barca process can then use the DB while
/// this run executes Python. (Fields drop in order: connection, database + file lock,
/// then the in-process guard.)
pub struct CacheReader {
    /// The statement of a one-key cache lookup, compiled on first use and kept for the
    /// reader's life ([`crate::cache::lookup_cached`]): compiling it cost more than the lookup.
    lookup: tokio::sync::Mutex<Option<libsql::Statement>>,
    /// The statement of [`CacheReader::lookup_many`], kept the same way.
    lookup_many: tokio::sync::Mutex<Option<libsql::Statement>>,
    local_artifact_dir: Option<String>,
    /// A server's ledger, which answers cache lookups in place of this database.
    ledger: Option<crate::ledger::Ledger>,
    /// The successful rows of a partitioned node's keys, read in one pass the first time a
    /// node with many keys is looked up ([`CacheReader::key_rows`]); `None` for a node whose
    /// keys are looked up one by one.
    key_rows: std::sync::Mutex<HashMap<String, Option<std::sync::Arc<KeyRows>>>>,
    conn: libsql::Connection,
    _handle: DbHandle,
    /// None for a [`ReadView`] ([`db_guard_for`]).
    _guard: Option<MutexGuard<'static, ()>>,
}

impl CacheReader {
    pub async fn open(db_path: &str) -> Result<Self, BarcaError> {
        // Same order as every other helper: in-process guard first, then the file lock.
        let guard = db_guard_for(db_path).await;
        let (handle, conn) = open_conn(db_path).await?;
        Ok(Self {
            lookup: Default::default(),
            lookup_many: Default::default(),
            local_artifact_dir: None,
            ledger: None,
            key_rows: Default::default(),
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

    pub fn conn(&self) -> &libsql::Connection {
        &self.conn
    }

    /// The one-key lookup statement, compiled from the first of `sql` this database accepts
    /// on first use and reset for the next query.
    pub(crate) async fn lookup_statement(
        &self,
        sql: &[&str],
    ) -> tokio::sync::MappedMutexGuard<'_, libsql::Statement> {
        Self::kept_statement(&self.conn, &self.lookup, sql)
            .await
            .expect("the materializations table accepts a lookup")
    }

    /// The statement kept in `slot`, compiled from the first of `sql` that `conn` accepts on
    /// first use, and reset for the next query; `None` when it accepts none.
    async fn kept_statement<'a>(
        conn: &libsql::Connection,
        slot: &'a tokio::sync::Mutex<Option<libsql::Statement>>,
        sql: &[&str],
    ) -> Option<tokio::sync::MappedMutexGuard<'a, libsql::Statement>> {
        let mut slot = slot.lock().await;
        if slot.is_none() {
            for candidate in sql {
                #[cfg(test)]
                PREPARES.with(|n| n.set(n.get() + 1));
                if let Ok(statement) = conn.prepare(candidate).await {
                    *slot = Some(statement);
                    break;
                }
            }
        }
        let statement = tokio::sync::MutexGuard::try_map(slot, Option::as_mut).ok()?;
        statement.reset();
        Some(statement)
    }

    /// [`crate::cache::lookup_cached`] of each `(node id, run hash)`, in order, a few thousand
    /// at a time: one query (compiled once, the keys passed as one JSON array) instead of one
    /// query and one transaction per key. `None` when this database cannot answer them so (a
    /// server's ledger answers lookups, or the table predates output hashes); the caller then
    /// looks them up one by one.
    pub(crate) async fn lookup_many(
        &self,
        keys: &[(&str, &str)],
    ) -> Option<Vec<Option<OutputRef>>> {
        if self.ledger.is_some() {
            return None;
        }
        const CHUNK: usize = 4096;
        const SQL: &str = "SELECT k.key, m.artifact_path, m.artifact_format, \
             m.artifact_size_bytes, m.output_hash FROM json_each(?1) AS k \
             JOIN materializations m INDEXED BY idx_mat_node_run \
             ON m.node_id = json_extract(k.value, '$[0]') \
             AND m.run_hash = json_extract(k.value, '$[1]') \
             WHERE m.status = 'success' ORDER BY k.key, m.id DESC";
        let mut out: Vec<Option<OutputRef>> = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(CHUNK) {
            let pairs = serde_json::to_string(chunk).ok()?;
            let statement = Self::kept_statement(&self.conn, &self.lookup_many, &[SQL]).await?;
            let mut rows = statement.query([pairs]).await.ok()?;
            // Each key's rows newest first: its first row with an artifact this run may read
            // decides, as in a lookup.
            let mut found: Vec<Option<Option<OutputRef>>> = vec![None; chunk.len()];
            while let Some(row) = rows.next().await.ok()? {
                let Ok(i) = row.get::<i64>(0) else { continue };
                let Some(slot) = found.get_mut(i as usize) else {
                    continue;
                };
                if slot.is_some() {
                    continue;
                }
                let Ok(path) = row.get::<String>(1) else {
                    continue;
                };
                if !self.allows_artifact(&path) {
                    continue;
                }
                *slot = Some((|| {
                    Some(OutputRef {
                        path,
                        format: row.get::<String>(2).ok()?,
                        size_bytes: row.get::<i64>(3).ok()? as u64,
                        elapsed_seconds: None,
                        content_hash: row.get::<String>(4).ok().filter(|h| !h.is_empty()),
                    })
                })());
            }
            drop(rows);
            drop(statement);
            out.extend(found.into_iter().map(Option::flatten));
            tokio::task::yield_now().await;
        }
        Some(out)
    }

    /// Every successful row of `base`'s partition keys, by key id, when reading them in one
    /// pass is cheaper than looking each key up: a dry run of a 200k-key node asked 200k
    /// times, at about 5µs a lookup. The pass reads the whole table (an index-ordered read of
    /// the keys' rows costs a random page read per row), so it is taken only when the table has
    /// at most [`ROWS_PER_KEY`] rows for each of the node's `keys`, the keys the plan has;
    /// otherwise `None`, and the keys are looked up one by one. Asked once per node; the reader
    /// is short-lived, so the rows are as current as one lookup's.
    pub(crate) async fn key_rows(
        &self,
        base: &str,
        keys: usize,
    ) -> Option<std::sync::Arc<KeyRows>> {
        if self.ledger.is_some() {
            return None;
        }
        if let Some(known) = self.key_rows.lock().unwrap().get(base) {
            return known.clone();
        }
        let rows = self
            .read_key_rows(base, keys)
            .await
            .ok()
            .flatten()
            .map(std::sync::Arc::new);
        self.key_rows
            .lock()
            .unwrap()
            .insert(base.to_string(), rows.clone());
        rows
    }

    async fn read_key_rows(&self, base: &str, keys: usize) -> Result<Option<KeyRows>, BarcaError> {
        let (lo, hi) = key_range(base);
        let table = query_i64(
            &self.conn,
            "SELECT max(id) FROM materializations",
            Vec::new(),
        )
        .await?
        .unwrap_or(0);
        if table > (keys as i64).saturating_mul(ROWS_PER_KEY) {
            return Ok(None);
        }
        let mut rows = self
            .conn
            .query(
                &format!(
                    "SELECT id, node_id, run_hash, artifact_path, artifact_format, \
                     artifact_size_bytes, output_hash, evicted_at IS NULL AND \
                     last_read_at >= datetime('now', '{}') FROM materializations NOT INDEXED \
                     WHERE node_id >= ?1 AND node_id < ?2 AND status = 'success' \
                     AND artifact_path IS NOT NULL",
                    crate::cache::READ_STAMP_REFRESH
                ),
                [lo, hi],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        let mut out = KeyRows::default();
        let mut read = 0usize;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            // A long read that never waits: let the thread's other tasks run now and then.
            read += 1;
            if read.is_multiple_of(4096) {
                tokio::task::yield_now().await;
            }
            let (Ok(id), Ok(node), Ok(run_hash), Ok(path)) = (
                row.get::<i64>(0),
                row.get::<String>(1),
                row.get::<String>(2),
                row.get::<String>(3),
            ) else {
                continue;
            };
            if !self.allows_artifact(&path) {
                continue;
            }
            let format = row.get::<String>(4).ok();
            let size = row.get::<i64>(5).ok();
            let content = row.get::<String>(6).ok().filter(|h| !h.is_empty());
            // Read within the hour (`cache::READ_STAMP_REFRESH`): a read now need not be stamped.
            let recent = row.get::<i64>(7).is_ok_and(|r| r == 1);
            out.add(
                id,
                &node,
                &run_hash,
                &path,
                format.as_deref(),
                size,
                content.as_deref(),
                recent,
            );
        }
        Ok(Some(out))
    }
}

/// How many rows of the table per key [`CacheReader::key_rows`] reads at most in its pass: a
/// lookup costs about as much as reading this many rows in order.
const ROWS_PER_KEY: i64 = 8;

/// A partitioned node's successful rows: for each key id and run hash, the latest row's id and
/// output. Every string is kept in one buffer, so that building and dropping the rows of 400k
/// keys costs a few allocations, not millions.
#[derive(Default)]
pub(crate) struct KeyRows {
    text: String,
    rows: Vec<KeyRow>,
    /// The first row of each (key id, run hash) digest; further rows chain through `next`.
    first: HashMap<u64, u32>,
}

/// A span of [`KeyRows::text`].
#[derive(Clone, Copy)]
struct Span(u32, u32);

struct KeyRow {
    id: i64,
    node: Span,
    run_hash: Span,
    path: Span,
    /// `None` when the latest row has no format or size: as for a lookup, no hit.
    format_size: Option<(Span, u64)>,
    content: Option<Span>,
    /// Its `last_read_at` is less than an hour old.
    recently_read: bool,
    next: Option<u32>,
}

impl KeyRows {
    fn key(node: &str, run_hash: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (node, run_hash).hash(&mut h);
        h.finish()
    }

    fn span(&mut self, s: &str) -> Span {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        Span(start, self.text.len() as u32)
    }

    fn text(&self, span: Span) -> &str {
        &self.text[span.0 as usize..span.1 as usize]
    }

    fn find(&self, node: &str, run_hash: &str) -> Option<usize> {
        let mut at = self.first.get(&Self::key(node, run_hash)).copied();
        while let Some(i) = at {
            let row = &self.rows[i as usize];
            if self.text(row.node) == node && self.text(row.run_hash) == run_hash {
                return Some(i as usize);
            }
            at = row.next;
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        id: i64,
        node: &str,
        run_hash: &str,
        path: &str,
        format: Option<&str>,
        size: Option<i64>,
        content: Option<&str>,
        recently_read: bool,
    ) {
        if let Some(i) = self.find(node, run_hash)
            && self.rows[i].id > id
        {
            return;
        }
        let path_span = self.span(path);
        let format_size = match (format, size) {
            (Some(format), Some(size)) => Some((self.span(format), size as u64)),
            _ => None,
        };
        let content = content.map(|c| self.span(c));
        if let Some(i) = self.find(node, run_hash) {
            let row = &mut self.rows[i];
            row.id = id;
            row.path = path_span;
            row.format_size = format_size;
            row.content = content;
            row.recently_read = recently_read;
            return;
        }
        let node_span = self.span(node);
        let hash_span = self.span(run_hash);
        let key = Self::key(node, run_hash);
        let next = self.first.get(&key).copied();
        self.first.insert(key, self.rows.len() as u32);
        self.rows.push(KeyRow {
            id,
            node: node_span,
            run_hash: hash_span,
            path: path_span,
            format_size,
            content,
            recently_read,
            next,
        });
    }

    /// Whether the row a lookup of `node_id` at `run_hash` finds was read within the hour.
    pub(crate) fn recently_read(&self, node_id: &str, run_hash: &str) -> bool {
        self.find(node_id, run_hash)
            .is_some_and(|i| self.rows[i].recently_read)
    }

    /// What a lookup of `node_id` at `run_hash` finds.
    pub(crate) fn get(&self, node_id: &str, run_hash: &str) -> Option<OutputRef> {
        let row = &self.rows[self.find(node_id, run_hash)?];
        let (format, size_bytes) = row.format_size?;
        Some(OutputRef {
            path: self.text(row.path).to_string(),
            format: self.text(format).to_string(),
            size_bytes,
            elapsed_seconds: None,
            content_hash: row.content.map(|c| self.text(c).to_string()),
        })
    }
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
            // The connection keeps the file open (and its SQLite locks) until it is dropped,
            // so it goes before the handle releases barca's lock (#537).
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
) -> Result<(DbHandle, libsql::Connection, i64), BarcaError> {
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
    if Path::new(path).exists() {
        // A read-write SQLite connection may checkpoint on close, even after a failed
        // schema check. Judge compatibility read-only before opening the writer.
        let (_db, read) = connect_read_only(path).await?;
        let version = crate::db_schema::check(&read).await?;
        if version == crate::db_schema::VERSION {
            crate::state_validate::Schema::read(&read)
                .await?
                .columns_fit(true)
                .map_err(unusable)?;
        } else {
            crate::state_validate::legacy_schema(&read)
                .await?
                .map_err(unusable)?;
        }
    }
    let (db, conn) = connect_write(path).await?;
    let handle = DbHandle {
        _db: db,
        _lock: Some(lock),
    };
    let version = match init_schema(&conn).await {
        Ok(()) => crate::db_schema::read_version(&conn).await,
        Err(e) => Err(e),
    };
    match version {
        Ok(version) => {
            // Preserve the existing WAL metadata format for newly created databases too.
            let wal = async {
                let mut rows = conn.query("PRAGMA journal_mode=WAL", ()).await?;
                while rows.next().await?.is_some() {}
                Ok::<_, libsql::Error>(())
            }
            .await;
            if let Err(error) = wal {
                drop(conn);
                drop(handle);
                return Err(BarcaError::Db(format!(
                    "failed to enable metadata WAL: {error}"
                )));
            }
            Ok((handle, conn, version))
        }
        Err(e) => {
            drop(conn);
            drop(handle);
            Err(e)
        }
    }
}

/// What a run reads from the metadata DB as it starts ([`start_run`]).
pub struct StartedRun {
    /// Logical-node scheduling priors for the requested plan.
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
    definitions: &[(String, String)],
) -> Result<StartedRun, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (handle, conn, _) = initialized(db_path, db_path)
        .await
        .map_err(|e| named(e, db_path))?;
    let started = async {
        insert_run(&conn, db_path, &run).await;
        record_definitions(&conn, definitions).await;
        Ok(StartedRun {
            cost_estimates: {
                let ids: std::collections::BTreeSet<_> =
                    node_ids.iter().chain(base_ids).cloned().collect();
                cost_estimates_on(&conn, Some(&ids.into_iter().collect::<Vec<_>>())).await?
            },
            avg_elapsed: avg_elapsed_on(&conn, node_ids).await?,
            partitioned_avg_elapsed: partitioned_avg_elapsed_on(&conn, base_ids).await?,
        })
    }
    .await;
    drop(conn);
    drop(handle);
    started
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

/// The format version of the metadata DB at `path`, read in place when compatible, otherwise
/// from a private snapshot without writing the file: `None` when there is no database yet. A database this release cannot use
/// is [`BarcaError::MetadataUnusable`] naming `path`. For a read-only server, which never
/// migrates the file itself.
pub async fn probe_metadata(path: &str) -> Result<Option<i64>, BarcaError> {
    directory_check(path)?;
    if ReadView::open(path).await?.is_some() {
        return Ok(Some(crate::db_schema::VERSION));
    }
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
pub(crate) async fn init_schema(conn: &libsql::Connection) -> Result<(), BarcaError> {
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
    with_tx(conn, "BEGIN IMMEDIATE", "schema migration", async {
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
        Ok(())
    })
    .await
}

/// Add this release's additive columns and indexes to a version-1 database written by an
/// earlier release, in one transaction: rows are untouched, and an error leaves the schema as it
/// was. The format stays version 1 (see `specs/interfaces/metadata-schema.md`).
async fn add_to_current_schema(conn: &libsql::Connection) -> Result<(), BarcaError> {
    with_tx(conn, "BEGIN IMMEDIATE", "schema migration", async {
        for (table, definitions) in crate::db_schema::ADDED_COLUMNS {
            crate::db_schema::add_columns(conn, table, definitions).await?;
        }
        crate::db_schema::add_tables(conn).await?;
        for index in crate::db_schema::INDEXES {
            conn.execute(index, ())
                .await
                .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;
        }
        crate::state_validate::usable_schema(conn)
            .await?
            .map_err(unusable)?;
        Ok(())
    })
    .await
}

async fn migrate_legacy_schema(conn: &libsql::Connection) -> Result<(), BarcaError> {
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
    crate::db_schema::add_columns(conn, "logs", crate::db_schema::LOG_COLUMNS).await?;
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

        let db_path = db_path.to_owned();
        tokio::task::spawn_blocking(move || {
            // Keep the writer lock until both files have been copied, even if the caller
            // is cancelled while the blocking operation is running.
            let _lock = _lock;
            let dir = tempfile::tempdir()
                .map_err(|e| BarcaError::Db(format!("failed to create snapshot dir: {e}")))?;
            let copy = dir.path().join("metadata.db");
            fs::copy(&db_path, &copy)
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
        })
        .await
        .map_err(|e| BarcaError::Db(format!("snapshot task failed: {e}")))?
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

/// The elapsed seconds of each node's last `window` successful materializations, its partition
/// keys' rows folded into the node (newest first). What `GET /state` reports as typical
/// durations, without reading every row of the table: a node's own rows come from the node id
/// index; its keys' rows from the rows just below the latest key row, a window that grows until
/// it holds `window` successes or reaches the first row. The keys of one run are written
/// together, so for a node with many keys the first window is enough.
pub async fn recent_durations(
    db_path: &str,
    base_ids: &[String],
    window: usize,
) -> Result<HashMap<String, Vec<f64>>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_h, conn) = open_conn(db_path).await?;
    let mut out = HashMap::new();
    for base in base_ids {
        // (row id, elapsed)
        let mut found: Vec<(i64, f64)> = Vec::new();
        let mut rows = conn
            .query(
                "SELECT id, elapsed_seconds FROM materializations INDEXED BY idx_mat_node_run \
                 WHERE node_id = ?1 AND status = 'success' AND elapsed_seconds IS NOT NULL \
                 ORDER BY id DESC LIMIT ?2",
                vec![
                    libsql::Value::Text(base.clone()),
                    libsql::Value::Integer(window as i64),
                ],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query durations: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read durations: {e}")))?
        {
            if let (Ok(id), Ok(e)) = (row.get::<i64>(0), row.get::<f64>(1)) {
                found.push((id, e));
            }
        }
        drop(rows);
        let (lo, hi) = key_range(base);
        let latest_key = query_i64(
            &conn,
            "SELECT max(id) FROM materializations INDEXED BY idx_mat_node_run \
             WHERE node_id >= ?1 AND node_id < ?2",
            vec![
                libsql::Value::Text(lo.clone()),
                libsql::Value::Text(hi.clone()),
            ],
        )
        .await?;
        if let Some(top) = latest_key {
            let mut keys: Vec<(i64, f64)> = Vec::new();
            let mut span: i64 = 1024;
            let mut below = top + 1;
            while keys.len() < window && below > 0 {
                let from = (top + 1 - span).max(0);
                let mut rows = conn
                    .query(
                        "SELECT id, node_id, elapsed_seconds FROM materializations \
                         WHERE id >= ?1 AND id < ?2 AND status = 'success' \
                         AND elapsed_seconds IS NOT NULL ORDER BY id DESC",
                        [from, below],
                    )
                    .await
                    .map_err(|e| BarcaError::Db(format!("failed to query durations: {e}")))?;
                while let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| BarcaError::Db(format!("failed to read durations: {e}")))?
                {
                    let node = row.get::<String>(1).unwrap_or_default();
                    if node.as_str() >= lo.as_str()
                        && node.as_str() < hi.as_str()
                        && let (Ok(id), Ok(e)) = (row.get::<i64>(0), row.get::<f64>(2))
                    {
                        keys.push((id, e));
                    }
                }
                below = from;
                span *= 4;
            }
            found.extend(keys);
        }
        if found.is_empty() {
            continue;
        }
        found.sort_unstable_by_key(|(id, _)| std::cmp::Reverse(*id));
        found.truncate(window);
        out.insert(base.clone(), found.into_iter().map(|(_, e)| e).collect());
    }
    Ok(out)
}

/// One recorded output line of a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LogEntry {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
    pub line: String,
    /// `stdout` or `stderr`; lines recorded before barca captured stderr are `stdout`.
    #[serde(default)]
    pub stream: crate::LogStream,
    /// When the line arrived (RFC 3339, UTC, milliseconds); null on lines recorded before
    /// barca stamped them.
    #[serde(default)]
    pub ts: Option<String>,
}

impl LogEntry {
    /// The entry at `seq` of a run's recorded lines.
    pub fn at(seq: usize, line: crate::run_logs::RecordedLine) -> Self {
        Self {
            node_id: line.node_id,
            seq: seq as i64,
            line: line.line,
            stream: line.stream,
            ts: line.ts,
        }
    }
}

/// Persist a run's recorded lines, in order. Idempotent: a run that already has lines here is
/// left alone.
pub async fn insert_logs(
    db_path: &str,
    run_id: &str,
    lines: &[crate::run_logs::RecordedLine],
) -> Result<(), BarcaError> {
    if lines.is_empty() {
        return Ok(());
    }
    let _g = db_guard_for(db_path).await;
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
    with_tx(&conn, "BEGIN", "log write", async {
        // Many rows per statement: one statement per row cost ~9µs a line, which a step that
        // prints 100k lines felt.
        for (chunk_no, chunk) in lines.chunks(LOG_INSERT_ROWS).enumerate() {
            let marks = (0..chunk.len())
                .map(|i| {
                    let p = i * 6;
                    format!(
                        "(?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                        p + 1,
                        p + 2,
                        p + 3,
                        p + 4,
                        p + 5,
                        p + 6
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() * 6);
            for (i, line) in chunk.iter().enumerate() {
                let seq = (chunk_no * LOG_INSERT_ROWS + i) as i64;
                params.push(libsql::Value::Text(run_id.to_string()));
                params.push(libsql::Value::Text(line.node_id.clone()));
                params.push(libsql::Value::Integer(seq));
                params.push(libsql::Value::Text(line.line.clone()));
                params.push(libsql::Value::Text(line.stream.as_str().to_string()));
                params.push(match &line.ts {
                    Some(ts) => libsql::Value::Text(ts.clone()),
                    None => libsql::Value::Null,
                });
            }
            conn.execute(
                &format!(
                    "INSERT INTO logs (run_id, node_id, seq, line, stream, ts) VALUES {marks}"
                ),
                libsql::params_from_iter(params),
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to record output: {e}")))?;
        }
        Ok(())
    })
    .await
}

/// Rows per `INSERT` of a run's lines (6 parameters each, well under SQLite's limit).
const LOG_INSERT_ROWS: usize = 500;

/// Fetch all persisted log lines for a run, in order. An older database without the
/// `stream` and `ts` columns reads them as stdout lines with no time.
pub async fn get_logs(db_path: &str, run_id: &str) -> Result<Vec<LogEntry>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let full = conn
        .query(
            "SELECT node_id, seq, line, stream, ts FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
            [run_id.to_string()],
        )
        .await;
    let (mut rows, columns) = match full {
        Ok(rows) => (rows, 5),
        Err(_) => (
            conn.query(
                "SELECT node_id, seq, line FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
                [run_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query logs: {e}")))?,
            3,
        ),
    };
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read log row: {e}")))?
    {
        let stream = if columns == 5 {
            row.get::<String>(3)
                .ok()
                .and_then(|s| crate::LogStream::parse(&s))
                .unwrap_or_default()
        } else {
            crate::LogStream::Stdout
        };
        out.push(LogEntry {
            node_id: row.get::<String>(0).unwrap_or_default(),
            seq: row.get::<i64>(1).unwrap_or_default(),
            line: row.get::<String>(2).unwrap_or_default(),
            stream,
            ts: if columns == 5 {
                row.get::<String>(4).ok()
            } else {
                None
            },
        });
    }
    Ok(out)
}

/// Load the last-fired time (unix epoch seconds) for every scheduled node.
pub async fn get_schedule_state(db_path: &str) -> Result<HashMap<String, i64>, BarcaError> {
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT COALESCE((SELECT id FROM materializations ORDER BY id DESC LIMIT 1),0)",
            (),
        )
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
    let _g = db_guard_for(db_path).await;
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

/// The run-hash components the latest successful materialization of `node_id` (a display id)
/// recorded ([`crate::changes`]); `None` when it recorded none (before 0.29) or never succeeded.
pub(crate) async fn last_success_components(cache: &CacheReader, node_id: &str) -> Option<String> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT components FROM materializations WHERE node_id = ?1 AND status = 'success' \
             ORDER BY id DESC LIMIT 1",
            [node_id.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let recorded = row.get::<String>(0).ok().filter(|c| !c.is_empty())?;
    drop(rows);
    let mut own: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&recorded).ok()?;
    let Some(definition) = own.remove(crate::changes::DEFINITION_KEY) else {
        return Some(recorded);
    };
    let mut rows = cache
        .conn()
        .query(
            "SELECT components FROM definition_components WHERE definition_hash = ?1",
            [definition.as_str()?.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let mut all: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&row.get::<String>(0).ok()?).ok()?;
    all.append(&mut own);
    serde_json::to_string(&all).ok()
}

/// Record the definitions a starting run may name ([`crate::changes::definition_rows`]). Most runs
/// find them all there and write nothing. Without its row a step says a plain `changed`, so a
/// failure here is no reason to fail the run.
async fn record_definitions(conn: &libsql::Connection, definitions: &[(String, String)]) {
    let Ok(mut rows) = conn
        .query("SELECT definition_hash FROM definition_components", ())
        .await
    else {
        return;
    };
    let mut known = std::collections::HashSet::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(hash) = row.get::<String>(0) {
            known.insert(hash);
        }
    }
    drop(rows);
    for (hash, components) in definitions.iter().filter(|(h, _)| !known.contains(h)) {
        let _ = conn
            .execute(
                "INSERT OR IGNORE INTO definition_components (definition_hash, components) \
                 VALUES (?1, ?2)",
                [hash.as_str(), components.as_str()],
            )
            .await;
    }
}

/// The object this machine knows to be in the artifact store for `node_id` (a display id) at
/// `run_hash`: the store location and digest of its latest successful row, unless an upload to
/// that location is pending (#390) or the row has no digest. A row with a store location is
/// recorded only once its upload was confirmed (`specs/interfaces/artifact-upload-receipts.md`), so a
/// step that produces the same digest again need not upload it again (#554).
pub(crate) async fn confirmed_object(
    cache: &CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<(String, String)> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT artifact_path, output_hash FROM materializations INDEXED BY idx_mat_node_run \
             WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' \
             AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1",
            [node_id.to_string(), run_hash.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let path = row.get::<String>(0).ok()?;
    let digest = row.get::<String>(1).ok().filter(|d| !d.is_empty())?;
    drop(rows);
    // Any pending upload to that location (this host's or another's), or a database that
    // cannot say: the object is not known to be there.
    let mut pending = cache
        .conn()
        .query(
            "SELECT 1 FROM pending_uploads WHERE store_path = ?1",
            [path.clone()],
        )
        .await
        .ok()?;
    match pending.next().await {
        Ok(None) => Some((path, digest)),
        _ => None,
    }
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
        // One seek for the base and two per recorded partition: skip directly to the next
        // node id instead of scanning every observation in each key's history.
        let (lo, hi) = key_range(base);
        for (sql, params) in [
            (
                "SELECT node_id, COALESCE(observation_hash, output_hash) FROM materializations \
                 INDEXED BY idx_mat_output_latest WHERE node_id = ?1 AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1",
                vec![base.to_string()],
            ),
            (
                "WITH RECURSIVE nodes(node_id) AS ( \
                 SELECT (SELECT node_id FROM materializations INDEXED BY idx_mat_output_latest \
                 WHERE node_id >= ?1 AND node_id < ?2 AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY node_id LIMIT 1) UNION ALL \
                 SELECT (SELECT node_id FROM materializations INDEXED BY idx_mat_output_latest \
                 WHERE node_id > nodes.node_id AND node_id < ?2 AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY node_id LIMIT 1) \
                 FROM nodes WHERE node_id IS NOT NULL) \
                 SELECT node_id, (SELECT COALESCE(observation_hash, output_hash) \
                 FROM materializations INDEXED BY idx_mat_output_latest \
                 WHERE node_id = nodes.node_id AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1) \
                 FROM nodes WHERE node_id IS NOT NULL",
                vec![lo, hi],
            ),
        ] {
            let Ok(mut rows) = cache.conn().query(sql, params).await else {
                return Ok(HashMap::new());
            };
            while let Ok(Some(row)) = rows.next().await {
                if let (Ok(node), Ok(hash)) = (row.get::<String>(0), row.get::<String>(1)) {
                    out.insert(node, hash);
                }
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
    crate::persist::recording::record_legacy_outputs(db_path, outputs, run_hashes).await
}

/// Load every persisted per-node cost estimate (run start — seeds the
/// in-memory `CostModel` so batch sizing starts pre-warmed).
pub async fn load_cost_estimates(
    db_path: &str,
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    cost_estimates_on(&conn, None).await
}

pub(crate) async fn cost_estimates_for(
    conn: &libsql::Connection,
    ids: &[String],
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    cost_estimates_on(conn, Some(ids)).await
}

async fn cost_estimates_on(
    conn: &libsql::Connection,
    ids: Option<&[String]>,
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    let columns =
        "SELECT node_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples FROM cost_estimates";
    let queries = match ids {
        None => vec![(columns.to_owned(), Vec::new())],
        Some(ids) => ids
            .chunks(IN_LIST_CHUNK)
            .map(|chunk| {
                let marks = (1..=chunk.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                (
                    format!("{columns} WHERE node_id IN ({marks})"),
                    chunk.to_vec(),
                )
            })
            .collect(),
    };
    let mut out = Vec::new();
    for (sql, params) in queries {
        let failed = |e| BarcaError::Db(format!("failed to query cost_estimates: {e}"));
        let mut rows = conn.query(&sql, params).await.map_err(failed)?;
        while let Some(row) = rows.next().await.map_err(failed)? {
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    insert_run(&conn, db_path, &run).await;
    Ok(())
}

/// The row of a run that is starting, and the runs it finds interrupted ([`create_run_with`]).
async fn insert_run(conn: &libsql::Connection, db_path: &str, run: &NewRun<'_>) {
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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

/// [`set_run_outcome`] over whatever status the run recorded: a run `barca serve` stopped at its
/// run timeout recorded itself `cancelled` (it was stopped as a cancel is) and is an `error`
/// (#628).
pub async fn replace_run_outcome(
    db_path: &str,
    run_id: &str,
    status: &str,
    outcome_json: &str,
) -> Result<bool, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let changed = conn
        .execute(
            "UPDATE runs SET outcome_json = ?2, status = ?3, finished_at = COALESCE(finished_at, datetime('now')) WHERE run_id = ?1",
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
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
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
/// Whether the database has the index `name` (a snapshot of an older database may not).
async fn has_index(conn: &libsql::Connection, name: &str) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1",
            [name],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect indexes: {e}")))?;
    Ok(rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect indexes: {e}")))?
        .is_some())
}

async fn has_column(
    conn: &libsql::Connection,
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
async fn interrupted_runs(conn: &libsql::Connection, db_path: &str) -> Vec<String> {
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

async fn running_runs(conn: &libsql::Connection) -> Vec<RunningRun> {
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
async fn mark_interrupted_runs(conn: &libsql::Connection, db_path: &str) {
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
pub(crate) async fn live_runs(conn: &libsql::Connection, db_path: &str) -> Vec<String> {
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
pub(crate) async fn runs_alive_here(conn: &libsql::Connection, db_path: &str) -> Vec<String> {
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
pub(crate) async fn connect_locked(
    db_path: &str,
) -> Option<(libsql::Database, libsql::Connection)> {
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
    alive_running_runs_in(db_path, db_path).await
}

/// [`alive_running_runs`], with the `running` rows read from `read_path`: a snapshot of the
/// metadata database at `db_path` ([`DbSnapshot`]), so a polled reader (`GET /state`) never
/// opens the real one. The owners are judged against `db_path`'s marker directory.
pub async fn alive_running_runs_in(read_path: &str, db_path: &str) -> Vec<AliveRun> {
    let running = {
        let _g = db_guard_for(db_path).await;
        let Ok((_db, conn)) = open_conn(read_path).await else {
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
    let _g = db_guard_for(db_path).await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return;
    };
    touch_read_on(&conn, paths).await;
}

/// Record the digest of the store's copy as the digest of the result stored at each location
/// (store location, SHA-256), on its latest successful row. A fetch found that copy without the
/// recorded digest and the run used it (`barca docs remote`, "Checking a local copy against the
/// store"): an artifact's path names the computation, so the store's copy is the result, and
/// the recorded digest names bytes that may exist nowhere (a `--refresh` killed between its
/// upload and its row, #576). Once recorded, this machine's copy, the store's and the row agree
/// and the next `get` or `status` finds nothing to report. Best effort.
pub(crate) async fn adopt_store_digests(db_path: &str, adopted: &[(String, String)]) {
    if adopted.is_empty() || !Path::new(db_path).exists() {
        return;
    }
    let _g = db_guard_for(db_path).await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return;
    };
    for (store, sha256) in adopted {
        let _ = conn
            .execute(
                "UPDATE materializations SET output_hash = ?2 WHERE id = (SELECT id FROM \
                 materializations WHERE artifact_path = ?1 AND status = 'success' \
                 ORDER BY id DESC LIMIT 1)",
                [store.clone(), sha256.clone()],
            )
            .await;
    }
}

/// [`touch_read`] on an open connection, in a transaction of its own. Best effort.
pub(crate) async fn touch_read_on(conn: &libsql::Connection, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let _ = with_tx(conn, "BEGIN IMMEDIATE", "read times", async {
        if touch_read_in(conn, paths).await {
            Ok(())
        } else {
            Err(BarcaError::Db("a read time was not recorded".into()))
        }
    })
    .await;
}

/// Set `last_read_at` of `paths` inside the caller's transaction. Returns whether every update
/// went through; it stops at the first that does not (the earlier ones stay in the transaction).
pub(crate) async fn touch_read_in(conn: &libsql::Connection, paths: &[String]) -> bool {
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
    let _g = db_guard_for(db_path).await;
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
            "WITH evicted AS (SELECT artifact_path, MAX(id) AS id \
             FROM materializations INDEXED BY idx_mat_evicted WHERE evicted_at IS NOT NULL \
             AND status = 'success' GROUP BY artifact_path) \
             SELECT e.artifact_path FROM evicted e WHERE NOT EXISTS ( \
             SELECT 1 FROM materializations n INDEXED BY idx_mat_path \
             WHERE n.artifact_path = e.artifact_path AND n.status = 'success' AND n.id > e.id)",
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
    let _g = db_guard_for(db_path).await;
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
        let _g = db_guard_for(db_path).await;
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

/// A run's first failed step and its error, read from `idx_mat_failed` ([`read_runs`]).
const FAILURE_INDEXED: &str = "(SELECT node_id || char(31) || COALESCE(error_type, '') || char(31) || COALESCE(error_message, '') FROM materializations INDEXED BY idx_mat_failed WHERE run_id = runs.run_id AND status = 'failed' ORDER BY id LIMIT 1)";

async fn read_runs(
    db_path: &str,
    run_id: Option<&str>,
    limit: usize,
) -> Result<Vec<RunRecord>, BarcaError> {
    let _g = db_guard_for(db_path).await;
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
    // The sum of a run's attempts, read from indexes only: its row count (`idx_mat_run`),
    // corrected by the few rows whose attempts are not 1 (`idx_mat_attempts`). Summing the
    // column itself reads every row of the run, 200k for a run of a 200k-key node.
    let attempts = if !(names.contains("run_id") && names.contains("attempts")) {
        "NULL"
    } else if has_index(&conn, "idx_mat_attempts").await? {
        "((SELECT COUNT(*) FROM materializations INDEXED BY idx_mat_run WHERE run_id = runs.run_id) \
          - (SELECT COUNT(*) - COALESCE(SUM(attempts), 0) FROM materializations \
             INDEXED BY idx_mat_attempts WHERE run_id = runs.run_id \
             AND (attempts IS NULL OR attempts != 1)))"
    } else {
        "(SELECT COALESCE(SUM(attempts), 0) FROM materializations WHERE run_id = runs.run_id)"
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
    let job = if has_column(&conn, "runs", "job").await? {
        "job"
    } else {
        "NULL"
    };
    // The first failed step and its error, joined by the unit separator (a byte no node id,
    // error type or message line holds).
    let failure = if !(names.contains("run_id") && names.contains("error_type")) {
        "NULL"
    } else if has_index(&conn, "idx_mat_failed").await? {
        // Named: without statistics SQLite's planner picks `idx_mat_run` and reads every row
        // of the run to check its status.
        FAILURE_INDEXED
    } else {
        "(SELECT node_id || char(31) || COALESCE(error_type, '') || char(31) || COALESCE(error_message, '') FROM materializations WHERE run_id = runs.run_id AND status = 'failed' ORDER BY id LIMIT 1)"
    };
    let mut rows = conn
        .query(
            &format!("SELECT run_id, command, files, target, status, steps_total, steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, {attempts}, {trigger}, {client}, {job}, {failure} FROM runs WHERE (?1 = '' OR run_id = ?1) ORDER BY id DESC LIMIT ?2"),
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
            job: row.get::<String>(16).ok().filter(|t| !t.is_empty()),
            failure: row.get::<String>(17).ok().and_then(|raw| {
                let mut parts = raw.splitn(3, '\u{1f}');
                let node = parts.next()?.to_string();
                let error_type = parts.next().unwrap_or_default();
                let message = parts.next().unwrap_or_default();
                Some(RunFailure::new(node, error_type, message))
            }),
        });
    }
    Ok(records)
}

/// A step recorded by this run: one it executed (`success`) or one that failed. Cached
/// steps have no new materialization row, so they are not listed (a live run on a server also
/// lists the `cached` steps it has reported); nor are steps a failure skipped.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunStep {
    pub node_id: String,
    pub status: String,
    pub elapsed_seconds: Option<f64>,
    /// A failed step's error as recorded: for a Python exception, `Type: message` and the
    /// traceback's frames.
    pub error: Option<String>,
    /// The partition key (`k=v`, pairs joined by `,`) of one key of a partitioned step.
    #[serde(default)]
    pub partition_key: Option<String>,
    /// Attempts made, retries included.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub attempts: Option<i64>,
    #[serde(default)]
    pub run_hash: Option<String>,
    /// Where the result is recorded: this machine's artifact path, or its artifact-store
    /// location when it was uploaded.
    #[serde(default)]
    pub artifact_path: Option<String>,
    #[serde(default)]
    pub artifact_format: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub artifact_size_bytes: Option<i64>,
    /// A failed step's exception type (`ZeroDivisionError`; `WorkerError` when its worker
    /// died, `UploadError` when its result could not be stored).
    #[serde(default)]
    pub error_type: Option<String>,
    /// A failed step's error message alone (without its type and traceback).
    #[serde(default)]
    pub error_message: Option<String>,
    /// A failed step's Python traceback: barca's own frames left out of a failure in the
    /// step's code, every frame kept for a failure in barca's own code (`error_phase`).
    #[serde(default)]
    pub error_traceback: Option<String>,
    /// Where a failed step failed (#664): `user_code` (the step's own code: importing its
    /// module, calling it, checking what it returned), or barca's own work around it:
    /// `load_inputs`, `split_output` (a batched call's result), `write_output` or `sink`.
    /// `null` for a step that did not fail, a worker that died, or a row from an older barca.
    #[serde(default)]
    pub error_phase: Option<String>,
    /// How the worker process ended, for a step whose worker died: `SIGKILL (possibly the OOM
    /// killer)`, another signal, `code N`, or `disconnected`. `null` for a step that raised.
    #[serde(default)]
    pub worker_exit: Option<String>,
    /// CPU seconds the step used (the worker process's user and system time).
    #[serde(default)]
    pub cpu_seconds: Option<f64>,
    /// The worker process's peak RSS while the step ran, in bytes (#627). `null` when not
    /// recorded (a failed step, a row from an older barca, or a platform that cannot tell).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_rss_bytes: Option<i64>,
    /// A failed step's last lines of output, stdout and stderr in the order they were written,
    /// without the traceback barca already shows (at most [`OUTPUT_TAIL_LINES`]; `barca logs
    /// RUN_ID --step STEP` has every line). `null` for a step that did not fail, `[]` when it
    /// printed nothing.
    #[serde(default)]
    pub output_tail: Option<Vec<String>>,
    /// The batched call (RFC-0013) that computed this key: the keys of one call share it, and
    /// its output is recorded once under `<node>[batch=<id>]`. `null` for a step that ran on
    /// its own. Experimental.
    #[serde(default)]
    pub batch: Option<String>,
}

/// How many lines of a failed step's output `barca history RUN_ID` shows ([`RunStep::output_tail`]).
pub const OUTPUT_TAIL_LINES: usize = 20;

/// How the worker ended, read from the error barca recorded when a worker died
/// (`worker for <node> exited with <how>`, or `worker disconnected`).
pub fn worker_exit(error_type: Option<&str>, message: Option<&str>) -> Option<String> {
    if error_type != Some("WorkerError") {
        return None;
    }
    let message = message?.lines().next()?;
    if message.starts_with("worker disconnected") {
        return Some("disconnected".to_string());
    }
    let (_, how) = message.split_once(" exited with ")?;
    let how = how
        .split_once(" while it waited")
        .map_or(how, |(how, _)| how);
    Some(how.to_string())
}

pub async fn get_run_steps(db_path: &str, run_id: &str) -> Result<Vec<RunStep>, BarcaError> {
    Ok(get_run_steps_page(db_path, run_id, None).await?.0)
}

/// A page of a run's recorded steps: what `barca history RUN_ID` and `GET /runs/{run_id}`
/// list (#623). A run of many partition keys records one step per key it computed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StepsPage {
    /// How many steps the run recorded.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    /// True when `steps` lists fewer than `total`.
    pub truncated: bool,
    /// Per node over every recorded step, listed or not, when the run has a partitioned node.
    /// Failed nodes first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<RunNodeSummary>>", optional))]
    pub nodes: Vec<RunNodeSummary>,
}

/// A node's steps in one run: its keys, however many `steps` lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunNodeSummary {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub keys: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed: usize,
    /// Summed over its keys, which may have overlapped.
    pub elapsed_seconds: f64,
    /// Batched calls that computed them; 0 when each key ran on its own.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub calls: usize,
}

/// How many recorded steps `barca history RUN_ID` and `GET /runs/{run_id}` list by default.
pub const RUN_STEPS_LIMIT: usize = 1000;

/// At most `limit` of `run_id`'s recorded steps (every one with `None`), in the order they were
/// recorded, failed steps first among those kept; and how many there are.
pub async fn get_run_steps_page(
    db_path: &str,
    run_id: &str,
    limit: Option<usize>,
) -> Result<(Vec<RunStep>, StepsPage), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    // Read-only snapshots of older databases may lack later columns: those read as null.
    let mut columns = Vec::new();
    for name in [
        "attempts",
        "error_type",
        "cpu_seconds",
        "max_rss_bytes",
        "batch_id",
        "error_phase",
    ] {
        columns.push(if has_column(&conn, "materializations", name).await? {
            name
        } else {
            "NULL"
        });
    }
    let (attempts, error_type, cpu, rss, batch, phase) = (
        columns[0], columns[1], columns[2], columns[3], columns[4], columns[5],
    );
    let total = match limit {
        None => None,
        Some(_) => {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM materializations WHERE run_id = ?1",
                    [run_id],
                )
                .await
                .map_err(|e| BarcaError::Db(format!("failed to count run steps: {e}")))?;
            let row = rows
                .next()
                .await
                .map_err(|e| BarcaError::Db(format!("failed to count run steps: {e}")))?;
            Some(row.and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0) as usize)
        }
    };
    // Past the limit, the failed steps are kept before the others.
    let page = match (limit, total) {
        (Some(limit), Some(total)) if total > limit => format!(
            " AND id IN (SELECT id FROM materializations WHERE run_id = ?1 ORDER BY (status = 'failed') DESC, id LIMIT {limit})"
        ),
        _ => String::new(),
    };
    let mut rows = conn.query(
        &format!("SELECT node_id, status, elapsed_seconds, error_message, {attempts}, run_hash, artifact_path, artifact_format, artifact_size_bytes, {error_type}, error_traceback, {cpu}, {rss}, {batch}, {phase} FROM materializations WHERE run_id = ?1{page} ORDER BY id"),
        [run_id],
    ).await.map_err(|e| BarcaError::Db(format!("failed to query run steps: {e}")))?;
    let text = |row: &libsql::Row, i: i32| row.get::<String>(i).ok().filter(|t| !t.is_empty());
    let mut steps = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run step: {e}")))?
    {
        let node_id = row.get::<String>(0).unwrap_or_default();
        let partition = crate::StepId::parse(&node_id).partition.suffix();
        let raw_error = row.get::<String>(3).ok();
        let raw_type = text(&row, 9);
        let worker_exit = worker_exit(raw_type.as_deref(), raw_error.as_deref());
        let (error_type, error_message, error_traceback) = match &raw_error {
            Some(message) => {
                let (kind, message, stack) = split_exception(
                    raw_type.as_deref().unwrap_or_default(),
                    message,
                    &text(&row, 10).unwrap_or_default(),
                );
                (Some(kind).filter(|k| !k.is_empty()), Some(message), stack)
            }
            None => (raw_type, None, text(&row, 10)),
        };
        steps.push(RunStep {
            partition_key: (!partition.is_empty()).then_some(partition),
            status: row.get::<String>(1).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(2).ok(),
            attempts: row.get::<i64>(4).ok(),
            run_hash: text(&row, 5),
            artifact_path: text(&row, 6),
            artifact_format: text(&row, 7),
            artifact_size_bytes: row.get::<i64>(8).ok(),
            worker_exit,
            error_type,
            error_message,
            error_traceback,
            error: raw_error,
            cpu_seconds: row.get::<f64>(11).ok(),
            max_rss_bytes: row.get::<i64>(12).ok().filter(|rss| *rss > 0),
            output_tail: None,
            batch: text(&row, 13),
            error_phase: text(&row, 14),
            node_id,
        });
    }
    drop(rows);
    // A failed step's last lines (#627). Failed steps are kept first in a page (#623), so every
    // failed step listed has its tail; only the listed failed steps' lines are read.
    // A key of a batched call: its call's output, recorded once for the call.
    let log_id = |s: &RunStep| match &s.batch {
        Some(b) => format!("{}[batch={b}]", crate::StepId::parse(&s.node_id).base_id()),
        None => s.node_id.clone(),
    };
    let failed: std::collections::HashSet<String> = steps
        .iter()
        .filter(|s| s.status == "failed")
        .map(log_id)
        .collect();
    if !failed.is_empty() {
        let lines = output_lines(&conn, run_id, &failed).await?;
        for step in steps.iter_mut().filter(|s| s.status == "failed") {
            step.output_tail = Some(output_tail(&lines, &log_id(step)));
        }
    }
    let total = total.unwrap_or(steps.len());
    let mut summary = conn
        .query(
            &format!(
                "SELECT substr(node_id, 1, instr(node_id || '[', '[') - 1) AS base, COUNT(*), SUM(status = 'failed'), SUM(elapsed_seconds), COUNT(DISTINCT {batch}) \
                 FROM materializations WHERE run_id = ?1 GROUP BY base ORDER BY SUM(status = 'failed') > 0 DESC, MIN(id)"
            ),
            [run_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to summarize run steps: {e}")))?;
    let mut nodes = Vec::new();
    while let Some(row) = summary
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run summary: {e}")))?
    {
        nodes.push(RunNodeSummary {
            node_id: row.get::<String>(0).unwrap_or_default(),
            keys: row.get::<i64>(1).unwrap_or(0) as usize,
            failed: row.get::<i64>(2).unwrap_or(0) as usize,
            elapsed_seconds: row.get::<f64>(3).unwrap_or(0.0),
            calls: row.get::<i64>(4).unwrap_or(0) as usize,
        });
    }
    if nodes.iter().all(|n| n.keys <= 1) {
        nodes.clear();
    }
    let page = StepsPage {
        total,
        truncated: steps.len() < total,
        nodes,
    };
    Ok((steps, page))
}

/// The recorded lines of `run_id` written by `nodes`, (node id, line), in the order they were
/// written. Empty on a database without a `logs` table.
async fn output_lines(
    conn: &libsql::Connection,
    run_id: &str,
    nodes: &std::collections::HashSet<String>,
) -> Result<Vec<(String, String)>, BarcaError> {
    let Ok(mut rows) = conn
        .query(
            "SELECT node_id, line FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
            [run_id],
        )
        .await
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read log row: {e}")))?
    {
        let node = row.get::<String>(0).unwrap_or_default();
        if nodes.contains(&node) {
            out.push((node, row.get::<String>(1).unwrap_or_default()));
        }
    }
    Ok(out)
}

/// The last [`OUTPUT_TAIL_LINES`] lines `node_id` wrote, up to the traceback of its last
/// failed attempt (the run detail prints the traceback on its own).
fn output_tail(lines: &[(String, String)], node_id: &str) -> Vec<String> {
    let mut own: Vec<&str> = lines
        .iter()
        .filter(|(node, _)| node == node_id)
        .map(|(_, line)| line.as_str())
        .collect();
    if let Some(at) = own
        .iter()
        .rposition(|line| line.trim_start() == "Traceback (most recent call last):")
    {
        own.truncate(at);
    }
    let from = own.len().saturating_sub(OUTPUT_TAIL_LINES);
    own[from..].iter().map(|line| line.to_string()).collect()
}

/// One step result as `barca sql` shows it in `barca.materializations` and `GET
/// /materializations` returns it: a row of `materializations` without the columns that describe
/// one machine's disk (`evicted_at`, `last_read_at`) or that are large (`error_traceback`,
/// `output_json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepRow {
    /// Node id, with the partition suffix for a partitioned step (`p.py:daily[day=d1]`).
    pub node_id: String,
    /// The partition key (`day=d1`), or `null`.
    pub partition: Option<String>,
    pub run_id: Option<String>,
    pub run_hash: Option<String>,
    /// `success` or `failed`.
    pub status: String,
    /// When it was recorded (UTC, `YYYY-MM-DD HH:MM:SS`).
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub cpu_seconds: Option<f64>,
    pub max_rss_bytes: Option<i64>,
    pub attempts: Option<i64>,
    /// `json`, `parquet`, `csv` or `pickle`.
    pub format: Option<String>,
    pub size_bytes: Option<i64>,
    /// Where the result is stored (a store path with a remote store).
    pub artifact: Option<String>,
    /// SHA-256 of the artifact's bytes.
    pub content_hash: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
}

/// Step results, newest first: at most `limit` rows from `offset`, and how many there are in
/// all. Read-only; columns an older database lacks are `null`.
pub async fn read_step_rows(
    db_path: &str,
    limit: usize,
    offset: usize,
) -> Result<(Vec<StepRow>, usize), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut select = Vec::new();
    for column in [
        "node_id",
        "run_id",
        "run_hash",
        "status",
        "created_at",
        "elapsed_seconds",
        "cpu_seconds",
        "max_rss_bytes",
        "attempts",
        "artifact_format",
        "artifact_size_bytes",
        "artifact_path",
        "output_hash",
        "error_type",
        "error_message",
    ] {
        if has_column(&conn, "materializations", column).await? {
            select.push(column.to_string());
        } else {
            select.push(format!("NULL AS {column}"));
        }
    }
    let failed =
        |e: libsql::Error| BarcaError::Db(format!("failed to query materializations: {e}"));
    let mut rows = conn
        .query("SELECT COUNT(*) FROM materializations", ())
        .await
        .map_err(failed)?;
    let total = rows
        .next()
        .await
        .map_err(failed)?
        .map_or(0, |r| r.get::<i64>(0).unwrap_or(0).max(0) as usize);
    drop(rows);
    let mut rows = conn
        .query(
            &format!(
                "SELECT {} FROM materializations ORDER BY id DESC LIMIT ?1 OFFSET ?2",
                select.join(", ")
            ),
            [
                limit.min(i64::MAX as usize) as i64,
                offset.min(i64::MAX as usize) as i64,
            ],
        )
        .await
        .map_err(failed)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed)? {
        let text = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
        let node_id = row.get::<String>(0).unwrap_or_default();
        let partition = node_id
            .split_once('[')
            .and_then(|(_, key)| key.strip_suffix(']'))
            .map(str::to_string);
        out.push(StepRow {
            partition,
            node_id,
            run_id: text(1),
            run_hash: text(2),
            status: text(3).unwrap_or_else(|| "success".to_string()),
            created_at: row.get::<String>(4).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(5).ok(),
            cpu_seconds: row.get::<f64>(6).ok(),
            max_rss_bytes: row.get::<i64>(7).ok(),
            attempts: row.get::<i64>(8).ok(),
            format: text(9),
            size_bytes: row.get::<i64>(10).ok(),
            artifact: text(11),
            content_hash: text(12),
            error_type: text(13),
            error_message: text(14),
        });
    }
    Ok((out, total))
}

/// Total number of recorded runs (for `barca history` truncation reporting).
pub async fn count_runs(db_path: &str) -> Result<usize, BarcaError> {
    let _g = db_guard_for(db_path).await;
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
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    // A partitioned node's rows are its keys (`node[key]`): an execution of it is a run.
    if !node_id.contains('[') && has_key_rows(&conn, node_id).await? {
        return partitioned_stats(&conn, node_id).await;
    }

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

    // `cache_hit_rate` (the JSON key keeps its name): the share of this node's executions that
    // repeated an earlier execution's run hash, i.e. ran again with unchanged code and inputs
    // (`--refresh`, a retry after a failure, an evicted result). A cache hit executes nothing
    // and writes no row, so it is not counted here; `barca history` counts them per run (#496).
    let cache_hit_rate = if total_runs > 1 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) - COUNT(DISTINCT run_hash) FROM materializations WHERE node_id = ?1 AND run_hash IS NOT NULL AND run_hash != ''",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query repeated executions: {e}")))?;
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

    // Peak RSS per execution, for the largest and the median (#627).
    let mut all_rss: Vec<i64> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT max_rss_bytes FROM materializations WHERE node_id = ?1 AND max_rss_bytes > 0 ORDER BY max_rss_bytes",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query peak memory: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            if let Ok(rss) = row.get::<i64>(0) {
                all_rss.push(rss);
            }
        }
    }
    let max_rss_bytes = all_rss.last().copied();
    let median_max_rss_bytes = (!all_rss.is_empty()).then(|| all_rss[(all_rss.len() - 1) / 2]);

    // Recent runs (last 10).
    let mut rows = conn
        .query(
            "SELECT elapsed_seconds, status, created_at, error_message, attempts, run_id, cpu_seconds, max_rss_bytes FROM materializations WHERE node_id = ?1 ORDER BY id DESC LIMIT 10",
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
            run_id: row.get::<String>(5).ok().filter(|id| !id.is_empty()),
            cpu_seconds: row.get::<f64>(6).ok(),
            max_rss_bytes: row.get::<i64>(7).ok().filter(|rss| *rss > 0),
            keys: None,
            failed_keys: None,
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
        max_rss_bytes,
        median_max_rss_bytes,
        recent_runs,
    })
}

/// `WHERE` over the rows of a node's keys: `node[`..`node\\`, `\\` being the byte after `[`.
const KEY_ROWS: &str = "node_id >= ?1 || '[' AND node_id < ?1 || '\\'";

async fn has_key_rows(conn: &libsql::Connection, node_id: &str) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            &format!("SELECT 1 FROM materializations WHERE {KEY_ROWS} LIMIT 1"),
            [node_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query partition keys: {e}")))?;
    Ok(rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read partition keys: {e}")))?
        .is_some())
}

/// [`AssetStats`] of a partitioned node: one execution per run, its time and CPU summed over
/// the run's keys and its memory the largest of them.
async fn partitioned_stats(
    conn: &libsql::Connection,
    node_id: &str,
) -> Result<AssetStats, BarcaError> {
    let failure = |e: libsql::Error| BarcaError::Db(format!("failed to query key history: {e}"));
    let mut rows = conn
        .query(
            &format!(
                "SELECT SUM(elapsed_seconds), MAX(created_at), MIN(error_message), MAX(attempts), MAX(NULLIF(run_id, '')), SUM(cpu_seconds), MAX(max_rss_bytes), COUNT(*), SUM(status = 'failed') \
                 FROM materializations WHERE {KEY_ROWS} \
                 GROUP BY COALESCE(NULLIF(run_id, ''), 'row:' || id) ORDER BY MAX(id) DESC"
            ),
            [node_id],
        )
        .await
        .map_err(failure)?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        let failed_keys = row.get::<i64>(8).unwrap_or(0);
        runs.push(AssetRunEntry {
            elapsed_seconds: row.get::<f64>(0).ok(),
            status: if failed_keys > 0 { "failed" } else { "success" }.to_string(),
            created_at: row.get::<String>(1).unwrap_or_default(),
            error_message: row.get::<String>(2).ok(),
            attempts: row.get::<i64>(3).unwrap_or(1),
            run_id: row.get::<String>(4).ok(),
            cpu_seconds: row.get::<f64>(5).ok(),
            max_rss_bytes: row.get::<i64>(6).ok().filter(|rss| *rss > 0),
            keys: row.get::<i64>(7).ok(),
            failed_keys: Some(failed_keys),
        });
    }
    drop(rows);
    let mut repeated = conn
        .query(
            &format!(
                "SELECT COUNT(*) - COUNT(DISTINCT node_id || ':' || run_hash), COUNT(*) FROM materializations WHERE {KEY_ROWS} AND run_hash IS NOT NULL AND run_hash != ''"
            ),
            [node_id],
        )
        .await
        .map_err(failure)?;
    let (repeats, executions) = match repeated.next().await.map_err(failure)? {
        Some(row) => (
            row.get::<i64>(0).unwrap_or(0),
            row.get::<i64>(1).unwrap_or(0),
        ),
        None => (0, 0),
    };
    let mut elapsed: Vec<f64> = runs
        .iter()
        .filter_map(|r| r.elapsed_seconds)
        .filter(|e| *e > 0.0)
        .collect();
    elapsed.sort_by(f64::total_cmp);
    let mut rss: Vec<i64> = runs.iter().filter_map(|r| r.max_rss_bytes).collect();
    rss.sort_unstable();
    let total_runs = runs.len() as i64;
    runs.truncate(10);
    Ok(AssetStats {
        node_id: node_id.to_string(),
        total_runs,
        avg_elapsed_seconds: (!elapsed.is_empty())
            .then(|| elapsed.iter().sum::<f64>() / elapsed.len() as f64),
        median_elapsed_seconds: percentile(&elapsed, 50.0),
        max_elapsed_seconds: elapsed.last().copied(),
        p95_elapsed_seconds: percentile(&elapsed, 95.0),
        cache_hit_rate: if executions > 0 {
            repeats as f64 / executions as f64
        } else {
            0.0
        },
        max_rss_bytes: rss.last().copied(),
        median_max_rss_bytes: (!rss.is_empty()).then(|| rss[(rss.len() - 1) / 2]),
        recent_runs: runs,
    })
}

/// A partitioned node's keys whose latest recorded attempt failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailedKeys {
    pub count: usize,
    /// The first of them in key order, at most 20.
    pub keys: Vec<String>,
}

/// [`FailedKeys`] of every partitioned node with some. Reads `idx_mat_failed`, then one
/// `idx_mat_node_latest` probe per failed key, so a node's healthy keys cost nothing.
pub async fn failed_partition_keys(
    db_path: &str,
) -> Result<HashMap<String, FailedKeys>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failure = |e: libsql::Error| BarcaError::Db(format!("failed to query failed keys: {e}"));
    let mut rows = conn
        .query(
            "SELECT DISTINCT f.node_id FROM materializations f \
             WHERE f.status = 'failed' AND instr(f.node_id, '[') > 0 AND NOT EXISTS ( \
               SELECT 1 FROM materializations s WHERE s.node_id = f.node_id AND s.status = 'success' \
               AND s.artifact_path IS NOT NULL AND s.id > f.id) \
             ORDER BY f.node_id",
            (),
        )
        .await
        .map_err(failure)?;
    let mut out: HashMap<String, FailedKeys> = HashMap::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        let node = row.get::<String>(0).unwrap_or_default();
        let id = crate::StepId::parse(&node);
        let entry = out.entry(id.base_id().to_string()).or_default();
        entry.count += 1;
        if entry.keys.len() < 20 {
            entry.keys.push(id.partition.suffix());
        }
    }
    Ok(out)
}

/// A key's latest recorded attempt (`GET /assets/{name}/partitions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionKeyRow {
    /// `retailer=acct_0001`.
    pub key: String,
    pub status: String,
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub error: Option<String>,
    pub run_id: Option<String>,
    pub artifact_path: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
}

/// How many keys last failed with one error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionErrorGroup {
    /// The error's first line, with the key's own values replaced by `<key>`.
    pub error: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub keys: usize,
}

/// One page of a node's keys that have a recorded attempt, in key order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionKeys {
    pub keys: Vec<PartitionKeyRow>,
    /// Keys matching the filters; `keys` is at most a page of them.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    /// Over every recorded key, whatever the filters.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub recorded: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed: usize,
    /// The errors of the failed keys, most common first, at most 20.
    pub errors: Vec<PartitionErrorGroup>,
}

/// Which keys [`partition_keys`] lists: `status` is a key's latest attempt, `query` a substring
/// of the key.
#[derive(Debug, Clone, Default)]
pub struct PartitionKeysFilter {
    pub status: Option<String>,
    pub query: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// The latest attempt of each of `node_id`'s keys. Reads the node's rows through
/// `idx_mat_node_run`, so it grows with the node's history, not the database's.
pub async fn partition_keys(
    db_path: &str,
    node_id: &str,
    filter: &PartitionKeysFilter,
) -> Result<PartitionKeys, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failure = |e: libsql::Error| BarcaError::Db(format!("failed to query partition keys: {e}"));
    // SQLite takes a bare column from the row that holds the `MAX`: each key's latest attempt.
    let mut rows = conn
        .query(
            &format!(
                "SELECT node_id, MAX(id), status, created_at, elapsed_seconds, error_message, run_id, artifact_path, artifact_size_bytes \
                 FROM materializations WHERE {KEY_ROWS} GROUP BY node_id ORDER BY node_id"
            ),
            [node_id],
        )
        .await
        .map_err(failure)?;
    let mut out = PartitionKeys::default();
    let mut errors: HashMap<String, usize> = HashMap::new();
    let needle = filter.query.as_deref().map(str::to_lowercase);
    while let Some(row) = rows.next().await.map_err(failure)? {
        out.recorded += 1;
        let status = row.get::<String>(2).unwrap_or_default();
        let error = row.get::<String>(5).ok();
        let id = crate::StepId::parse(&row.get::<String>(0).unwrap_or_default());
        if status == "failed" {
            out.failed += 1;
            let first = error.as_deref().unwrap_or("").lines().next().unwrap_or("");
            // "no plan for acct_0010" and "no plan for acct_0020" are one problem.
            let shape = id
                .partition
                .0
                .values()
                .fold(first.to_string(), |text, value| {
                    text.replace(value, "<key>")
                });
            *errors.entry(shape).or_default() += 1;
        }
        let key = id.partition.suffix();
        if filter.status.as_deref().is_some_and(|s| s != status)
            || needle
                .as_deref()
                .is_some_and(|n| !key.to_lowercase().contains(n))
        {
            continue;
        }
        out.total += 1;
        if out.total > filter.offset && out.keys.len() < filter.limit {
            out.keys.push(PartitionKeyRow {
                key,
                status,
                created_at: row.get::<String>(3).unwrap_or_default(),
                elapsed_seconds: row.get::<f64>(4).ok(),
                error,
                run_id: row.get::<String>(6).ok().filter(|r| !r.is_empty()),
                artifact_path: row.get::<String>(7).ok().filter(|p| !p.is_empty()),
                size_bytes: row.get::<i64>(8).ok(),
            });
        }
    }
    let mut groups: Vec<PartitionErrorGroup> = errors
        .into_iter()
        .map(|(error, keys)| PartitionErrorGroup { error, keys })
        .collect();
    groups.sort_by(|a, b| b.keys.cmp(&a.keys).then_with(|| a.error.cmp(&b.error)));
    groups.truncate(20);
    out.errors = groups;
    Ok(out)
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
    // The newest run's rows, one index seek per node (`idx_mat_run`: a run has at most one row
    // per key). Rows without a run id (an older barca's) leave it unknown.
    for (base, (_, run)) in newest {
        let Some(run) = run else { continue };
        let (lo, hi) = key_range(&base);
        let mut rows = conn
            .query(
                "SELECT sum(elapsed_seconds), count(*), count(DISTINCT batch_id), \
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

/// The node ids of `base`'s partition keys, `base[...]`, as a half-open range of the
/// `node_id` index: from `base[` up to `base\` (the character after `[`).
pub(crate) fn key_range(base: &str) -> (String, String) {
    (format!("{base}["), format!("{base}\\"))
}

/// The latest successful result of one key of a partitioned node.
#[derive(Debug, Clone)]
pub(crate) struct PartitionArtifact {
    /// `<base>[key]`.
    pub node_id: String,
    pub path: String,
    pub format: String,
    pub content_hash: Option<String>,
}

pub(crate) async fn partition_artifacts_for_config(
    cfg: &crate::config::ResolvedConfig,
    base_id: &str,
) -> Result<Vec<PartitionArtifact>, BarcaError> {
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
) -> Result<Vec<PartitionArtifact>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let (lo, hi) = key_range(base_id);
    let mut rows = conn
        .query(
            "SELECT node_id, artifact_path, artifact_format, output_hash FROM materializations \
             INDEXED BY idx_mat_node_run WHERE node_id >= ?1 AND node_id < ?2 \
             AND status = 'success' AND artifact_path IS NOT NULL AND artifact_path != '' \
             ORDER BY id DESC",
            [lo, hi],
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
        seen.entry(node_id).or_insert_with(|| PartitionArtifact {
            node_id: String::new(),
            path: row.get::<String>(1).unwrap_or_default(),
            format: row.get::<String>(2).unwrap_or_default(),
            content_hash: row.get::<String>(3).ok().filter(|h| !h.is_empty()),
        });
    }
    Ok(seen
        .into_iter()
        .map(|(node_id, a)| PartitionArtifact { node_id, ..a })
        .collect())
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
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    avg_elapsed_on(&conn, node_ids).await
}

/// How many values one `IN (...)` list binds: well under SQLite's parameter limit.
const IN_LIST_CHUNK: usize = 500;

/// [`get_avg_elapsed`] on an open connection: one statement per 500 nodes, not one per node.
async fn avg_elapsed_on(
    conn: &libsql::Connection,
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
                libsql::params_from_iter(chunk.iter().cloned()),
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

/// Look up the average elapsed_seconds of each partitioned node over all its keys.
/// For base_node_ids like "file.py:fetch", matches all "file.py:fetch[%]" in the DB.
/// Used for ETA estimation when steps carry partition_keys (late expansion).
pub async fn get_avg_elapsed_for_partitioned(
    db_path: &str,
    base_node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    if base_node_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    partitioned_avg_elapsed_on(&conn, base_node_ids).await
}

async fn partitioned_avg_elapsed_on(
    conn: &libsql::Connection,
    base_node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    let mut result = HashMap::new();
    // A node split into one step per worker is named once per step: ask once per node.
    let mut asked = std::collections::HashSet::new();
    for nid in base_node_ids {
        if !asked.insert(nid.as_str()) {
            continue;
        }
        // Every key of the node, `nid[...]`, as a range of the node id index (a `LIKE` scanned
        // the whole table).
        let (lo, hi) = key_range(nid);
        let mut rows = conn
            .query(
                "SELECT AVG(elapsed_seconds) FROM materializations INDEXED BY idx_mat_node_run \
                 WHERE node_id >= ?1 AND node_id < ?2 AND elapsed_seconds IS NOT NULL",
                [lo, hi],
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
    async fn status_history_queries_are_bounded_and_keep_each_partition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        let cache = super::CacheReader::open(&path).await.unwrap();
        cache.conn().execute(
            "WITH RECURSIVE history(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM history WHERE n < 500000) \
             INSERT INTO materializations(node_id, run_hash, output_hash, artifact_path, evicted_at) \
             SELECT 'sensor', 'run', 'old', 'restored', 'yesterday' FROM history", (),
        ).await.unwrap();
        cache.conn().execute(
            "INSERT INTO materializations(node_id, output_hash, observation_hash, artifact_path, evicted_at) VALUES \
             ('sensor', 'bytes', 'revision', 'restored', NULL), \
             ('sensor[key=a]', 'a-old', NULL, 'a', NULL), \
             ('sensor[key=b]', 'b', NULL, 'b', 'yesterday'), \
             ('sensor[key=a]', 'a', NULL, 'a', NULL), \
             ('other', 'other', NULL, 'other', NULL)", (),
        ).await.unwrap();
        let started = std::time::Instant::now();
        let hashes = super::last_output_hashes(&cache, &["sensor"])
            .await
            .unwrap();
        assert_eq!(
            hashes,
            std::collections::HashMap::from([
                ("sensor".into(), "revision".into()),
                ("sensor[key=a]".into(), "a".into()),
                ("sensor[key=b]".into(), "b".into()),
            ])
        );
        assert_eq!(
            super::evicted_paths(&cache).await,
            std::collections::HashSet::from(["b".into()])
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "status queries took {:?} for 500k historical rows",
            started.elapsed()
        );
    }

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
    /// #576: the store copy's digest is recorded on the latest successful row at its location,
    /// and on no other row.
    #[tokio::test]
    async fn an_adopted_store_digest_is_recorded_on_the_latest_row_at_that_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        let rows = [
            ("s3://b/a/n/h.json", "success", "first"),
            ("s3://b/a/n/h.json", "success", "second"),
            ("s3://b/a/n/h.json", "failed", ""),
            ("s3://b/a/m/h.json", "success", "other"),
        ];
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for (at, status, hash) in rows {
                conn.execute(
                    "INSERT INTO materializations (node_id, run_hash, artifact_path, status, \
                     output_hash) VALUES ('p.py:n', 'h', ?1, ?2, NULLIF(?3, ''))",
                    [at, status, hash],
                )
                .await
                .unwrap();
            }
        }
        let adopted = [("s3://b/a/n/h.json".to_string(), "new".to_string())];
        super::adopt_store_digests(&path, &adopted).await;
        let _guard = super::db_guard().await;
        let (_db, conn) = super::open_conn(&path).await.unwrap();
        let mut found = conn
            .query(
                "SELECT COALESCE(output_hash, '') FROM materializations ORDER BY id",
                (),
            )
            .await
            .unwrap();
        let mut hashes = Vec::new();
        while let Some(row) = found.next().await.unwrap() {
            hashes.push(row.get::<String>(0).unwrap());
        }
        assert_eq!(hashes, ["first", "new", "", "other"]);
    }

    /// #475: the keys of one run finish in any order, so the row id alone picks an arbitrary key
    /// as a partitioned node's last materialization. The rule: the latest run (by row id), and
    /// within it the first key in sort order; rows without a run id fall back to the row id.
    #[tokio::test]
    async fn a_page_of_run_steps_keeps_failed_steps_and_counts_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for i in 0..5 {
                let node = format!("p.py:k[n={i}]");
                let status = if i == 3 { "failed" } else { "success" };
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, run_hash, status) VALUES ('many', ?1, 'h', ?2)",
                    [node.as_str(), status],
                )
                .await
                .unwrap();
            }
        }
        let (steps, page) = super::get_run_steps_page(&path, "many", Some(2))
            .await
            .unwrap();
        let ids: Vec<&str> = steps.iter().map(|s| s.node_id.as_str()).collect();
        // In recorded order, the failed step kept although it is not among the first two.
        assert_eq!(ids, ["p.py:k[n=0]", "p.py:k[n=3]"]);
        assert_eq!(
            page,
            super::StepsPage {
                total: 5,
                truncated: true,
                nodes: page.nodes.clone(),
            }
        );
        let (steps, page) = super::get_run_steps_page(&path, "many", None)
            .await
            .unwrap();
        assert_eq!(steps.len(), 5);
        assert_eq!(
            page,
            super::StepsPage {
                total: 5,
                truncated: false,
                nodes: page.nodes.clone(),
            }
        );
        let (steps, page) = super::get_run_steps_page(&path, "many", Some(10))
            .await
            .unwrap();
        assert_eq!((steps.len(), page.truncated), (5, false));
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
            test_line("a.py:load", "loading…"),
            test_line("a.py:load", "read 30/150"),
            crate::run_logs::RecordedLine {
                stream: crate::LogStream::Stderr,
                ts: Some("2026-10-22T10:00:00.123Z".into()),
                ..test_line("a.py:load", "done")
            },
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
        assert_eq!(got[0].stream, crate::LogStream::Stdout);
        assert_eq!(got[0].ts, None);
        assert_eq!(got[2].stream, crate::LogStream::Stderr);
        assert_eq!(got[2].ts.as_deref(), Some("2026-10-22T10:00:00.123Z"));
    }

    fn test_line(node: &str, line: &str) -> crate::run_logs::RecordedLine {
        crate::run_logs::RecordedLine {
            node_id: node.into(),
            line: line.into(),
            stream: crate::LogStream::Stdout,
            ts: None,
        }
    }

    #[tokio::test]
    async fn logs_are_scoped_by_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        insert_logs(&db_path, "runA", &[test_line("n", "a-line")])
            .await
            .unwrap();
        insert_logs(&db_path, "runB", &[test_line("n", "b-line")])
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
        let lines = vec![test_line("p.py:a", "same"), test_line("p.py:a", "same")];
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
mod duration_tests {
    /// `GET /state`'s typical durations: the last successes of a node, its keys folded in,
    /// found below the latest key row however many other rows were written since.
    #[tokio::test]
    async fn recent_durations_take_the_last_successes_and_fold_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            let insert = |node: &str, status: &str, elapsed: f64| {
                let conn = &conn;
                let (node, status, elapsed) = (node.to_string(), status.to_string(), elapsed);
                async move {
                    conn.execute(
                        "INSERT INTO materializations (node_id, status, elapsed_seconds) \
                         VALUES (?1, ?2, ?3)",
                        libsql::params![node, status, elapsed],
                    )
                    .await
                    .unwrap();
                }
            };
            for _ in 0..5 {
                insert("p.py:a", "success", 1.0).await;
            }
            for k in 0..30 {
                insert(&format!("p.py:f[k={k}]"), "success", k as f64).await;
            }
            for _ in 0..20 {
                insert("p.py:a", "success", 10.0).await;
            }
            insert("p.py:a", "failed", 999.0).await;
            // A key row far below the latest one, and many rows of other nodes since.
            insert("p.py:g[k=old]", "success", 7.0).await;
            for _ in 0..3000 {
                insert("p.py:other", "success", 0.5).await;
            }
            insert("p.py:g[k=new]", "failed", 99.0).await;
            // `p.py:a_b` is not a key of `p.py:a`.
            insert("p.py:a_b[k=1]", "success", 5.0).await;
        }
        let ids: Vec<String> = ["p.py:a", "p.py:f", "p.py:g", "p.py:missing"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let d = super::recent_durations(&path, &ids, 20).await.unwrap();
        assert_eq!(
            d["p.py:a"],
            vec![10.0; 20],
            "the last 20 successes, failures left out"
        );
        let f: Vec<f64> = (10..30).rev().map(|k| k as f64).collect();
        assert_eq!(d["p.py:f"], f, "the latest 20 key rows, newest first");
        assert_eq!(d["p.py:g"], vec![7.0], "found below 3000 other rows");
        assert!(!d.contains_key("p.py:missing"));
    }
}

#[cfg(test)]
mod output_tail_tests {
    use super::{OUTPUT_TAIL_LINES, output_tail};

    #[test]
    fn the_tail_is_the_steps_own_last_lines_before_its_traceback() {
        let mut lines: Vec<(String, String)> = (0..30)
            .map(|i| ("p.py:a".to_string(), format!("line {i}")))
            .collect();
        lines.insert(3, ("p.py:b".to_string(), "other step".to_string()));
        lines.push(("p.py:a".into(), "Traceback (most recent call last):".into()));
        lines.push(("p.py:a".into(), "  File \"p.py\", line 3, in a".into()));
        lines.push(("p.py:a".into(), "ValueError: no".into()));
        let tail = output_tail(&lines, "p.py:a");
        assert_eq!(tail.len(), OUTPUT_TAIL_LINES);
        assert_eq!(tail.first().unwrap(), "line 10");
        assert_eq!(tail.last().unwrap(), "line 29");
        assert!(output_tail(&lines, "p.py:c").is_empty());
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    /// A reader compiles its lookup statements once, however many keys it looks up: 0.34.0
    /// compiled the one-key lookup again for every key, a third of `barca status` over a
    /// million keys.
    #[tokio::test(flavor = "current_thread")]
    async fn a_reader_compiles_each_lookup_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        let cache = CacheReader::open(&path).await.unwrap();
        let before = prepares();
        for k in 0..100 {
            crate::cache::lookup_cached(&cache, &format!("p.py:a[k={k}]"), "h").await;
        }
        assert_eq!(prepares() - before, 1, "one-key lookups");
        let keys: Vec<(String, String)> = (0..5000)
            .map(|k| (format!("p.py:a[k={k}]"), "h".to_string()))
            .collect();
        let pairs: Vec<(&str, &str)> = keys.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
        let found = cache.lookup_many(&pairs).await.unwrap();
        assert_eq!(found.len(), 5000);
        assert_eq!(
            prepares() - before,
            2,
            "and the lookup of many keys, in two chunks"
        );
    }

    fn db_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("metadata.db").display().to_string()
    }

    #[tokio::test]
    async fn lookups_together_answer_what_lookups_one_by_one_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        for (node, hash, path_, format, status) in [
            // The newest successful row decides, even without a format: no hit.
            (
                "p.py:a[k=1]",
                "h1",
                Some("/x/a1.json"),
                Some("json"),
                "success",
            ),
            ("p.py:a[k=1]", "h1", Some("/x/a1b.json"), None, "success"),
            // An older row with an artifact under a newer one without.
            (
                "p.py:a[k=2]",
                "h2",
                Some("/x/a2.json"),
                Some("json"),
                "success",
            ),
            ("p.py:a[k=2]", "h2", None, None, "success"),
            // Failed only, and another run hash.
            (
                "p.py:a[k=3]",
                "h3",
                Some("/x/a3.json"),
                Some("json"),
                "failed",
            ),
            (
                "p.py:a[k=4]",
                "other",
                Some("/x/a4.json"),
                Some("json"),
                "success",
            ),
            (
                "p.py:a[k=5]",
                "h5",
                Some("/x/a5.pkl"),
                Some("pickle"),
                "success",
            ),
        ] {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, \
                 artifact_format, artifact_size_bytes, status, output_hash) \
                 VALUES (?1, ?2, ?3, ?4, 7, ?5, 'c')",
                libsql::params![node, hash, path_, format, status],
            )
            .await
            .unwrap();
        }
        let keys: Vec<(String, String)> = (1..=6)
            .map(|k| (format!("p.py:a[k={k}]"), format!("h{k}")))
            .collect();
        let pairs: Vec<(&str, &str)> = keys.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
        let cache = CacheReader::open(&path).await.unwrap();
        let together = cache.lookup_many(&pairs).await.expect("answered together");
        let mut one_by_one = Vec::new();
        for (node, hash) in &pairs {
            one_by_one.push(crate::cache::lookup_cached(&cache, node, hash).await);
        }
        let paths = |found: &[Option<OutputRef>]| -> Vec<Option<String>> {
            found
                .iter()
                .map(|o| o.as_ref().map(|o| o.path.clone()))
                .collect()
        };
        assert_eq!(paths(&together), paths(&one_by_one));
        assert_eq!(
            paths(&together),
            [
                None,
                Some("/x/a2.json".into()),
                None,
                None,
                Some("/x/a5.pkl".into()),
                None
            ]
        );
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

    /// The runs list reads each run's first failure from `idx_mat_failed`: the plan names it,
    /// and the answer is the one a scan of the run's rows gives.
    #[tokio::test]
    async fn a_runs_first_failure_is_read_from_its_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        create_run(&path, "r", "get", "[]", None, Some(3))
            .await
            .unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            for (node, status, error) in [
                ("p.py:ok", "success", ""),
                ("p.py:first", "failed", "first bad"),
                ("p.py:second", "failed", "second bad"),
            ] {
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, status, error_type, \
                     error_message) VALUES ('r', ?1, ?2, 'ValueError', NULLIF(?3, ''))",
                    [node, status, error],
                )
                .await
                .unwrap();
            }
            let mut plan = conn
                .query(
                    &format!("EXPLAIN QUERY PLAN SELECT {FAILURE_INDEXED} FROM runs"),
                    (),
                )
                .await
                .unwrap();
            let mut details = Vec::new();
            while let Some(row) = plan.next().await.unwrap() {
                details.push(row.get::<String>(3).unwrap());
            }
            assert!(
                details.iter().any(|d| d.contains("idx_mat_failed")),
                "{details:?}"
            );
        }
        let run = get_run(&path, "r").await.unwrap().unwrap();
        let failure = run.failure.expect("the run has a failed step");
        assert_eq!(failure.node, "p.py:first");
        assert_eq!(failure.error, "ValueError: first bad");
    }
}

#[cfg(test)]
mod read_view_tests {
    use super::*;

    fn db_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("metadata.db").display().to_string()
    }

    async fn execute(path: &str, sql: &str) {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(path).await.unwrap();
        // A pragma that sets a mode answers a row.
        let mut rows = conn.query(sql, ()).await.unwrap();
        while rows.next().await.unwrap().is_some() {}
    }

    #[tokio::test]
    async fn a_read_view_reads_the_live_database_while_a_writer_holds_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        create_run(&path, "before", "get", "[]", None, Some(1))
            .await
            .unwrap();
        let view = ReadView::open(&path)
            .await
            .unwrap()
            .expect("a current WAL database");
        assert!(is_read_view(view.path()));
        // A commit after the view opened is seen: it reads the database, not a copy.
        create_run(&path, "after", "get", "[]", None, Some(1))
            .await
            .unwrap();
        // Neither the process-wide guard nor the cross-process lock is taken by a view.
        let held = lock_db(&path).await.unwrap();
        let read = tokio::time::timeout(Duration::from_secs(2), async {
            (
                get_run(view.path(), "before").await.unwrap(),
                get_run(view.path(), "after").await.unwrap(),
            )
        })
        .await
        .expect("a view does not wait for the writer's lock");
        drop(held);
        assert!(read.0.is_some() && read.1.is_some());
        let link = view.path().to_string();
        drop(view);
        assert!(!is_read_view(&link));
    }

    /// Readers in place and a writer meet at SQLite's own locks: the first connection after the
    /// last one closed rebuilds the log's index under a lock for a moment. Every connection
    /// waits for it; without that, a run's open failed with `database is locked` while the
    /// server read the database.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn readers_in_place_and_a_writer_wait_for_each_other_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let reads = tokio::spawn({
            let view_path = view.path().to_string();
            async move {
                for _ in 0..200 {
                    get_recent_runs(&view_path, 5)
                        .await
                        .expect("a read in place");
                }
            }
        });
        for i in 0..100 {
            create_run(&path, &format!("r{i}"), "get", "[]", None, Some(1))
                .await
                .expect("a write beside readers in place");
        }
        reads.await.unwrap();
        assert_eq!(get_recent_runs(view.path(), 1000).await.unwrap().len(), 100);
    }

    #[tokio::test]
    async fn a_read_view_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let (_db, conn) = open_conn(view.path()).await.unwrap();
        let wrote = conn
            .execute(
                "INSERT INTO logs (run_id, node_id, seq, line) VALUES ('r', 'n', 0, 'l')",
                (),
            )
            .await;
        assert!(wrote.is_err(), "a view's connection is read-only");
    }

    #[tokio::test]
    async fn no_read_view_of_a_database_that_is_missing_old_or_not_in_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        assert!(ReadView::open(&path).await.unwrap().is_none(), "missing");
        init_db(&path).await.unwrap();
        // An index this release adds is missing until a writer opens the database.
        execute(&path, "DROP INDEX idx_mat_failed").await;
        assert!(
            ReadView::open(&path).await.unwrap().is_none(),
            "would be migrated"
        );
        init_db(&path).await.unwrap();
        assert!(ReadView::open(&path).await.unwrap().is_some());
        execute(&path, "PRAGMA journal_mode=DELETE").await;
        assert!(
            ReadView::open(&path).await.unwrap().is_none(),
            "not in WAL mode"
        );
    }

    #[tokio::test]
    async fn a_checkpoint_waits_for_a_read_view_query_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        create_run(&path, "r", "get", "[]", None, Some(1))
            .await
            .unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let (_db, reader) = open_conn(view.path()).await.unwrap();
        // A read in progress: its transaction holds the log.
        reader.execute("BEGIN", ()).await.unwrap();
        let mut rows = reader.query("SELECT count(*) FROM runs", ()).await.unwrap();
        rows.next().await.unwrap();
        let folding = tokio::spawn({
            let path = path.clone();
            async move {
                let _lock = lock_db(&path).await.unwrap();
                fold_log(&path).await
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(rows);
        reader.execute("COMMIT", ()).await.unwrap();
        folding
            .await
            .unwrap()
            .expect("the checkpoint finishes once the read ends");
        assert!(wal_is_clean(&path));
    }
}
