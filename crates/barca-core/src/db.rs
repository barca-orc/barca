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
/// helper — and the inline cache-check/persist ops in `commands::execute` — holds
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
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
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

/// Where [`replace_db`] can be cut short. Each is a point at which the process may die; the
/// tests stop there and check that the next pull still ends with nothing lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplaceStage {
    /// Local rows are copied onto the staged file and both logs are folded in.
    Carried,
    /// The base record says a swap is in progress and the old database's (empty) sidecar
    /// files are removed; the staged file is not moved yet.
    SidecarsRemoved,
    /// The staged file is in place; the base record does not name it yet.
    Renamed,
}

/// A downloaded copy of the shared state, about to replace the local database.
pub(crate) struct Incoming<'a> {
    /// The downloaded file, in the same directory as the database.
    pub staged: &'a Path,
    /// The token of the blob it is a copy of.
    pub token: &'a str,
    /// The base record ([`crate::state_base::read_raw`]) as it was before the download began.
    pub base_at_start: Option<&'a [u8]>,
}

/// What [`replace_db`] did.
#[derive(Debug)]
pub(crate) enum Replaced {
    /// The download is now the local database, with what was carried over from the old one.
    Swapped(crate::state_carry::Carried),
    /// The local database was replaced or pushed while this download was on its way, so the
    /// download may be older than it: nothing was touched. Carries the current base record.
    Superseded(Option<crate::state_base::Base>),
}

/// Replace the database at `db_path` with a download of the shared state. This is the only
/// place a local database is replaced.
///
/// A database is its main file *plus* its write-ahead log: frames left in `<db>-wal` are
/// applied on top of whatever main file is there, so replacing only the main file lays the old
/// database's log over the new one (#221). The old database can hold rows that were never
/// pushed, which a pull must not lose. And a download takes time, during which another process
/// can have pulled a newer blob. So, under the in-process guard and the cross-process lock (no
/// barca process has the database open meanwhile):
///
/// 0. **Still current?** If the base record changed since the download began, the download
///    is discarded ([`Replaced::Superseded`]): the local database is never replaced by a blob
///    older than the one it is based on.
/// 1. **Carry.** Rows the old database has and the download lacks are copied onto the
///    download ([`crate::state_carry`]). The old database is only read. Skipped when the base
///    record shows nothing was written locally since the last pull or push.
/// 2. **Fold.** Both logs are checkpointed into their main files and checked to be empty. Each
///    database is now one self-contained file, with the same rows as before.
/// 3. **Swap.** The base record is marked "swap in progress", the old sidecars (empty by now)
///    are removed, the download is renamed over `db_path` (after an fsync when it carries rows
///    that exist nowhere else), and the base record is written with the new token.
///
/// A process that dies before the rename leaves the old database complete, unpushed rows
/// included: nothing before that point changes what it holds, and the download is simply
/// abandoned (the next pull starts over, and carrying is idempotent). One that dies after it
/// leaves the new database complete. There is no state in between.
///
/// Only a local file that is positively not a database (wrong header, cut short, or reported
/// corrupt by the engine) is replaced without being read, and the returned
/// [`Carried::unreadable`](crate::state_carry::Carried) says why. A local database that cannot
/// be opened for any other reason (held open by another program, permissions, I/O) is an
/// error and stays as it was; so does everything when the download is not a database.
pub(crate) async fn replace_db(
    db_path: &str,
    incoming: Incoming<'_>,
) -> Result<Replaced, BarcaError> {
    replace_db_until(db_path, incoming, None).await
}

/// [`replace_db`] for a caller that took the database's lock before it began the download,
/// so that nothing can overtake it.
pub(crate) async fn replace_db_holding(
    _lock: &DbLock,
    db_path: &str,
    incoming: Incoming<'_>,
) -> Result<Replaced, BarcaError> {
    replace_locked(db_path, incoming, None).await
}

async fn replace_db_until(
    db_path: &str,
    incoming: Incoming<'_>,
    stop_after: Option<ReplaceStage>,
) -> Result<Replaced, BarcaError> {
    let _lock = lock_db(db_path).await?;
    replace_locked(db_path, incoming, stop_after).await
}

/// The in-process guard and the cross-process lock of the database at `db_path`, for an
/// operation that must see no other barca process touch the file. Held until dropped.
pub(crate) struct DbLock {
    _lock: fs::File,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) async fn lock_db(db_path: &str) -> Result<DbLock, BarcaError> {
    let guard = db_guard().await;
    let lock = acquire_file_lock(db_path, DB_FILE_LOCK_WAIT).await?;
    Ok(DbLock {
        _lock: lock,
        _guard: guard,
    })
}

/// The body of [`replace_db`]; the caller holds [`lock_db`].
async fn replace_locked(
    db_path: &str,
    incoming: Incoming<'_>,
    stop_after: Option<ReplaceStage>,
) -> Result<Replaced, BarcaError> {
    use crate::{state_base, state_carry::Carried};
    let staged = incoming.staged;
    let staged_path = staged.to_string_lossy().to_string();

    let base_raw = state_base::read_raw(db_path);
    if base_raw.as_deref() != incoming.base_at_start {
        return Ok(Replaced::Superseded(state_base::parse(base_raw.as_deref())));
    }
    let base = state_base::parse(base_raw.as_deref());
    if !staged.exists() {
        return Err(BarcaError::Db(format!(
            "the downloaded shared state {staged_path} is not there"
        )));
    }
    if let Some(why) = not_a_database(&staged_path) {
        return Err(pulled_is_not_a_database(why));
    }

    let local_len = fs::metadata(db_path).map(|m| m.len());
    let mut carried = if !Path::new(db_path).exists() && wal_is_clean(db_path) {
        // Nothing local.
        Carried::default()
    } else if !Path::new(db_path).exists() {
        // A log without its main file is not a database: there is nothing to apply it to.
        Carried {
            unreadable: Some(format!(
                "{db_path} is missing and only its write-ahead log was left"
            )),
            ..Default::default()
        }
    } else if base.as_ref().is_some_and(|b| b.trusted(db_path)) {
        // Provably the file a pull or push left, not written to since: exactly a blob that
        // was in the shared state, so nothing in it is unpushed.
        Carried::default()
    } else if matches!(local_len, Ok(0)) && wal_is_clean(db_path) {
        Carried {
            unreadable: Some("it is empty".to_string()),
            ..Default::default()
        }
    } else if matches!(local_len, Ok(0)) {
        // An empty main file beside a log with something in it: the rows, if any, are in
        // the log, and nothing here shows they can be read back. Not ours to throw away.
        return Err(BarcaError::Db(format!(
            "{db_path} is empty but {db_path}-wal is not: the local history cannot be read, \
             and it may hold rows that were never uploaded.\nThe local database was left as \
             it was, and nothing was pulled. To go on with the shared history alone, move \
             {db_path} and {db_path}-wal out of the way."
        )));
    } else if let Some(why) = not_a_database(db_path) {
        Carried {
            unreadable: Some(why),
            ..Default::default()
        }
    } else {
        carry_and_fold(db_path, &staged_path).await?
    };
    // The same rows, kept again before anything pushed them, are not news.
    carried.announced = base.as_ref().is_some_and(|b| b.unpushed)
        && base.as_ref().map(|b| b.kept.as_str()) == Some(carried.digest().as_str());
    if stop_after == Some(ReplaceStage::Carried) {
        return Ok(Replaced::Swapped(carried));
    }

    // Seam for #243: nothing above this line has changed what the old database holds, and
    // everything below is the replacement itself. Here `staged` is the complete next database
    // and `db_path` the complete previous one, each a single file with its log folded in
    // (the previous one may be absent, or not a database). Checks that must pass before a
    // pulled file may replace a local one, and keeping the previous file (`<db>.prev`), go
    // here, on these two paths.

    // From here the local database is about to change: any download begun before this point
    // must not be swapped in after it, even if this process dies before the last line.
    let io = |what: &str, e: std::io::Error| BarcaError::Db(format!("failed to {what}: {e}"));
    state_base::write(db_path, base_raw.as_deref(), "", false, "")
        .map_err(|e| io("write the base record", e))?;
    remove_sidecars(db_path)?;
    if stop_after == Some(ReplaceStage::SidecarsRemoved) {
        return Ok(Replaced::Swapped(carried));
    }
    // Rows that exist only here must be on disk before the file that held them is unlinked.
    let durable = carried.wrote();
    if durable {
        fs::File::open(staged)
            .and_then(|f| f.sync_all())
            .map_err(|e| io(&format!("sync {staged_path}"), e))?;
    }
    fs::rename(staged, db_path).map_err(|e| io(&format!("move {staged_path} to {db_path}"), e))?;
    if durable && let Some(dir) = Path::new(db_path).parent() {
        // Best effort: the rename is already visible; this makes it survive a power cut.
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        fs::File::open(dir).and_then(|d| d.sync_all()).ok();
    }
    if stop_after == Some(ReplaceStage::Renamed) {
        return Ok(Replaced::Swapped(carried));
    }
    // Best effort: without it the next pull compares instead of trusting the record.
    state_base::write(
        db_path,
        state_base::read_raw(db_path).as_deref(),
        incoming.token,
        carried.wrote(),
        &carried.digest(),
    )
    .ok();
    Ok(Replaced::Swapped(carried))
}

/// A copy of the local database, taken for an upload, and what the database was then.
pub(crate) struct PushCopy {
    /// The file to upload: the whole database in one file. The caller removes it.
    pub path: PathBuf,
    base_raw: Option<Vec<u8>>,
    id: Option<crate::state_base::FileId>,
}

/// Fold the local database's write-ahead log into its main file and copy the result next to
/// it, under the lock: the copy is one consistent database (turso keeps most data in the log;
/// the main file alone, without a checkpoint, would be an old or empty database), and it
/// stays that whatever other barca processes do to the database during the upload, which
/// therefore needs no lock.
pub(crate) async fn copy_for_push(db_path: &str, copy: PathBuf) -> Result<PushCopy, BarcaError> {
    let _lock = lock_db(db_path).await?;
    fold_log(db_path).await?;
    fs::copy(db_path, &copy).map_err(|e| {
        BarcaError::Db(format!(
            "failed to copy {db_path} to {} for upload: {e}",
            copy.display()
        ))
    })?;
    Ok(PushCopy {
        path: copy,
        base_raw: crate::state_base::read_raw(db_path),
        id: crate::state_base::FileId::of(db_path),
    })
}

/// After the upload of `copy` succeeded as the blob `token`: record that the local database
/// is that blob, if it still is. If anything touched it since the copy was taken (another
/// run recorded a step, a pull replaced it), nothing is claimed: the next pull compares.
pub(crate) async fn record_pushed(db_path: &str, copy: &PushCopy, token: &str) {
    use crate::state_base;
    let Ok(_lock) = lock_db(db_path).await else {
        return;
    };
    let base_raw = state_base::read_raw(db_path);
    let unchanged = base_raw == copy.base_raw
        && copy.id.is_some()
        && wal_is_clean(db_path)
        && state_base::FileId::of(db_path) == copy.id
        && wal_is_clean(db_path);
    if unchanged {
        state_base::write(db_path, base_raw.as_deref(), token, false, "").ok();
    }
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
             refusing to upload a torn database",
            fs::metadata(&wal).map(|m| m.len()).unwrap_or(0)
        )));
    }
    Ok(())
}

/// [`fold_log`] under the database's lock.
pub(crate) async fn fold_log_locked(db_path: &str) -> Result<(), BarcaError> {
    let _lock = lock_db(db_path).await?;
    fold_log(db_path).await
}

/// A whole pull of an already downloaded file, for tests: the download began just now.
#[cfg(test)]
pub(crate) async fn pull_for_tests(
    db_path: &str,
    staged: &Path,
    token: &str,
) -> crate::state_carry::Carried {
    let base = crate::state_base::read_raw(db_path);
    let incoming = Incoming {
        staged,
        token,
        base_at_start: base.as_deref(),
    };
    match replace_db(db_path, incoming).await.unwrap() {
        Replaced::Swapped(carried) => carried,
        other => panic!("{other:?}"),
    }
}

fn pulled_is_not_a_database(why: impl std::fmt::Display) -> BarcaError {
    BarcaError::Db(format!(
        "the shared state that was pulled cannot be opened as a database ({why}). The local \
         database was left as it was."
    ))
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

/// How long a pull waits for another program to let go of the local database before failing.
const LOCAL_BUSY_WAIT: Duration = Duration::from_secs(5);

/// The local database, opened for the carry, or the reason it is positively not a database.
enum LocalDb {
    Open(turso::Database, turso::Connection),
    Damaged(String),
}

/// Open the local database to read what must be carried. Decided by the kind of error, and
/// anything not recognised is an error, never a reason to replace the file:
///
/// - the engine says the file is not a database or is corrupt: [`LocalDb::Damaged`];
/// - the file is held by another program: wait up to [`LOCAL_BUSY_WAIT`], then an error naming
///   the likely holders;
/// - anything else (permissions, I/O): an error.
async fn open_local(db_path: &str) -> Result<LocalDb, BarcaError> {
    let deadline = Instant::now() + LOCAL_BUSY_WAIT;
    loop {
        let attempt: Result<_, turso::Error> = async {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            // Reading the schema is what first touches the file's pages.
            let mut rows = conn.query("SELECT COUNT(*) FROM sqlite_schema", ()).await?;
            while rows.next().await?.is_some() {}
            drop(rows);
            Ok((db, conn))
        }
        .await;
        let error = match attempt {
            Ok((db, conn)) => return Ok(LocalDb::Open(db, conn)),
            Err(e) => e,
        };
        match &error {
            turso::Error::NotAdb(why) | turso::Error::Corrupt(why) => {
                return Ok(LocalDb::Damaged(why.clone()));
            }
            // The engine reports a file lock held by another process as a plain error.
            turso::Error::Busy(_) | turso::Error::BusySnapshot(_) => {}
            other if other.to_string().contains("locked by another process") => {}
            _ => return Err(db_open_error(error)),
        }
        if Instant::now() >= deadline {
            return Err(BarcaError::Db(format!(
                "{db_path} is in use by another program (waited {}s): {error}\n\
                 Something outside barca's own locking has the metadata DB open (a DB browser, \
                 a backup tool, a script using sqlite3, or a barca process from an older \
                 version). Close it and retry.",
                LOCAL_BUSY_WAIT.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// True when the database has barca's `runs` or `materializations` table.
async fn holds_barca_history(conn: &turso::Connection) -> Result<bool, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read the schema: {e}"));
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' \
             AND name IN ('runs', 'materializations')",
            (),
        )
        .await
        .map_err(failed)?;
    let mut tables = 0;
    while let Some(row) = rows.next().await.map_err(failed)? {
        tables = row.get::<i64>(0).unwrap_or(0);
    }
    Ok(tables > 0)
}

/// Steps 1 and 2 of [`replace_db`]. Afterwards `staged_path` and (when it is a database)
/// `db_path` are each one self-contained file with an empty or absent log.
async fn carry_and_fold(
    db_path: &str,
    staged_path: &str,
) -> Result<crate::state_carry::Carried, BarcaError> {
    let kept_local = |e: BarcaError| {
        BarcaError::Db(format!(
            "{e}\nThe local database was left as it was, and nothing was pulled. To go on with \
             the shared history alone, move {db_path} and {db_path}-wal out of the way; what \
             was recorded only on this machine is then not carried over."
        ))
    };

    // The local database first: if it cannot be read, nothing else is touched.
    let (_local_db, local) = match open_local(db_path).await.map_err(kept_local)? {
        LocalDb::Open(db, conn) => (db, conn),
        LocalDb::Damaged(why) => {
            return Ok(crate::state_carry::Carried {
                unreadable: Some(why),
                ..Default::default()
            });
        }
    };
    if !holds_barca_history(&local).await.map_err(kept_local)? {
        // A database, but not one barca wrote rows into: nothing in it is history.
        return Ok(crate::state_carry::Carried {
            unreadable: Some("it has no barca tables".to_string()),
            ..Default::default()
        });
    }
    init_schema(&local).await.map_err(kept_local)?;

    let (_pulled_db, pulled) = connect(staged_path)
        .await
        .map_err(pulled_is_not_a_database)?;
    init_schema(&pulled)
        .await
        .map_err(pulled_is_not_a_database)?;

    let mut carried = crate::state_carry::carry_unpushed(&local, &pulled)
        .await
        .map_err(kept_local)?;
    carried.compared = true;
    checkpoint(&local).await.map_err(kept_local)?;
    drop(local);
    drop(_local_db);
    // Its log must now be empty, or removing it in the swap would lose what it holds.
    if !wal_is_clean(db_path) {
        return Err(kept_local(BarcaError::Db(format!(
            "the write-ahead log of {db_path} is not empty after a checkpoint"
        ))));
    }

    checkpoint(&pulled).await.map_err(kept_local)?;
    drop(pulled);
    drop(_pulled_db);
    if !wal_is_clean(staged_path) {
        return Err(kept_local(BarcaError::Db(format!(
            "the write-ahead log of {staged_path} is not empty after a checkpoint"
        ))));
    }
    remove_sidecars(staged_path)?;
    Ok(carried)
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
            conn,
            _handle: handle,
            _guard: guard,
        })
    }

    pub fn conn(&self) -> &turso::Connection {
        &self.conn
    }
}

/// Open the database file at `path` and connect, taking no lock. For callers that already
/// hold the cross-process lock for the database this file is (or is about to become).
async fn connect(path: &str) -> Result<(turso::Database, turso::Connection), BarcaError> {
    let db = Builder::new_local(path)
        .build()
        .await
        .map_err(db_open_error)?;
    let conn = db
        .connect()
        .map_err(|e| BarcaError::Db(format!("failed to connect: {e}")))?;
    Ok((db, conn))
}

/// Open the database at `db_path` and connect. Callers must hold [`db_guard`]
/// for the duration of their work on the returned connection; the returned
/// handle holds the cross-process lock until it is dropped.
pub(crate) async fn open_conn(db_path: &str) -> Result<(DbHandle, turso::Connection), BarcaError> {
    let lock = acquire_file_lock(db_path, DB_FILE_LOCK_WAIT).await?;
    // Opening creates the database when it is not there. Whatever the base record says was
    // said about another file (deleted by hand, say): it must not be matched to the new one.
    // (The record is only ever trusted after checking the file's identity; this is the
    // second line of defence.)
    if !Path::new(db_path).exists() {
        crate::state_base::forget(db_path);
    }
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
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    init_schema(&conn).await
}

/// Create the tables and apply the migrations: afterwards the database has the current schema,
/// whatever version wrote it. Idempotent, and writes nothing to a database that is current.
async fn init_schema(conn: &turso::Connection) -> Result<(), BarcaError> {
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
    // These are safe no-ops if the columns already exist.
    for col in [
        "ALTER TABLE materializations ADD COLUMN artifact_path TEXT",
        "ALTER TABLE materializations ADD COLUMN artifact_format TEXT",
        "ALTER TABLE materializations ADD COLUMN artifact_size_bytes INTEGER",
        "ALTER TABLE materializations ADD COLUMN elapsed_seconds REAL",
        "ALTER TABLE materializations ADD COLUMN error_message TEXT",
        "ALTER TABLE materializations ADD COLUMN error_traceback TEXT",
        "ALTER TABLE materializations ADD COLUMN attempts INTEGER DEFAULT 1",
        "ALTER TABLE materializations ADD COLUMN sinks_json TEXT",
        "ALTER TABLE materializations ADD COLUMN cpu_seconds REAL",
        "ALTER TABLE materializations ADD COLUMN max_rss_bytes INTEGER",
        // Content hash of a sensor's output (#183): folded into its consumers' run hashes, and
        // what `--dry-run` / `barca status` assume the sensor returns next.
        "ALTER TABLE materializations ADD COLUMN output_hash TEXT",
        // The run that wrote the row (#214). Steps are recorded as they finish and again in the
        // end-of-run ledger (and its replay after a shared-state conflict); this is how the
        // later writes recognise what is already there. NULL on rows from older versions.
        "ALTER TABLE materializations ADD COLUMN run_id TEXT",
        "ALTER TABLE materializations ADD COLUMN error_type TEXT",
    ] {
        conn.execute(col, ()).await.ok();
    }
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
    for col in [
        "ALTER TABLE runs ADD COLUMN pid INTEGER",
        "ALTER TABLE runs ADD COLUMN host TEXT",
    ] {
        conn.execute(col, ()).await.ok();
    }

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
    // A run's lines are written once. They can be here already when this is the replay after
    // a shared-state conflict: the pull carried them over from the local database.
    if let Ok(mut rows) = conn
        .query("SELECT 1 FROM logs WHERE run_id = ?1 LIMIT 1", [run_id])
        .await
        && matches!(rows.next().await, Ok(Some(_)))
    {
        return Ok(());
    }
    for (seq, (node_id, line)) in lines.iter().enumerate() {
        conn.execute(
            "INSERT INTO logs (run_id, node_id, seq, line) VALUES (?1, ?2, ?3, ?4)",
            (
                run_id.to_string(),
                node_id.clone(),
                seq as i64,
                line.clone(),
            ),
        )
        .await
        .ok();
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

/// Create a new run record at the start of execution.
pub async fn create_run(
    db_path: &str,
    run_id: &str,
    command: &str,
    files: &str,
    target: Option<&str>,
    steps_total: Option<usize>,
) -> Result<(), BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "INSERT INTO runs (run_id, command, files, target, status, steps_total, pid, host) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
        [
            run_id.to_string(),
            command.to_string(),
            files.to_string(),
            target.unwrap_or("").to_string(),
            steps_total.map(|n| n.to_string()).unwrap_or_default(),
            std::process::id().to_string(),
            local_host(),
        ],
    )
    .await
    .ok();
    mark_interrupted_runs(&conn).await;
    Ok(())
}

/// This machine's host name ("" when it cannot be read). Stored on a run row next to the pid:
/// with shared remote state the database also holds other machines' runs, whose pids mean
/// nothing here.
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

/// The `running` runs started on this host whose process no longer exists (killed, out of
/// memory, power loss): they will never record an outcome themselves. Runs from other hosts,
/// and from versions that did not record a pid, are never listed. Best effort: empty when the
/// host name or the columns cannot be read.
async fn interrupted_runs(conn: &turso::Connection) -> Vec<String> {
    let mut dead: Vec<String> = Vec::new();
    let host = local_host();
    if host.is_empty() {
        return dead;
    }
    let Ok(mut rows) = conn
        .query(
            "SELECT run_id, pid FROM runs WHERE status = 'running' AND host = ?1 AND pid IS NOT NULL",
            [host],
        )
        .await
    else {
        return dead;
    };
    while let Ok(Some(row)) = rows.next().await {
        if let (Ok(run_id), Ok(pid)) = (row.get::<String>(0), row.get::<i64>(1))
            && !pid_alive(pid)
        {
            dead.push(run_id);
        }
    }
    dead
}

/// Record [`interrupted_runs`] as `interrupted`. Their `finished_at` and `elapsed_seconds`
/// stay NULL, since nobody saw them end. Only a run does this (it writes anyway, and with
/// shared state pushes what it wrote); reading history reports the same status without
/// writing it (see [`get_recent_runs`]).
async fn mark_interrupted_runs(conn: &turso::Connection) {
    for run_id in interrupted_runs(conn).await {
        conn.execute(
            "UPDATE runs SET status = 'interrupted' WHERE run_id = ?1 AND status = 'running'",
            [run_id],
        )
        .await
        .ok();
    }
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
    conn.execute(
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
    .ok();
    Ok(())
}

/// Retrieve recent run records, newest first.
pub async fn get_recent_runs(db_path: &str, limit: usize) -> Result<Vec<RunRecord>, BarcaError> {
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
    // A run whose process died is reported as `interrupted`, not as still `running`. Reported,
    // not written: reading history must not change the database (the next run records it).
    let interrupted = interrupted_runs(&conn).await;
    let mut rows = conn
        .query(
            "SELECT run_id, command, files, target, status, steps_total, steps_executed, steps_cached, started_at, finished_at, elapsed_seconds FROM runs ORDER BY id DESC LIMIT ?1",
            [limit.to_string()],
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
            started_at: row.get::<String>(8).unwrap_or_default(),
            finished_at: {
                let t = row.get::<String>(9).unwrap_or_default();
                if t.is_empty() { None } else { Some(t) }
            },
            elapsed_seconds: row.get::<f64>(10).ok(),
        });
    }
    Ok(records)
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

/// What the metadata DB knows about one node (all partition keys of a partitioned node).
#[derive(Debug, Clone, Default)]
pub struct NodeHistory {
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
        let mut rows = conn
            .query(
                &format!(
                    "SELECT node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
                     elapsed_seconds, status, error_message, created_at FROM materializations \
                     WHERE {filter} ORDER BY id DESC LIMIT 1"
                ),
                [base.clone(), prefix.clone()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            let opt = |i: usize| row.get::<String>(i).ok().filter(|s| !s.is_empty());
            h.latest = Some(MaterializationRecord {
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
    let mut result = HashMap::new();
    for nid in node_ids {
        let mut rows = conn
            .query(
                "SELECT AVG(elapsed_seconds) FROM materializations WHERE node_id = ?1 AND elapsed_seconds IS NOT NULL",
                [nid.clone()],
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

    // ── replace_db: what a pull does to the local database (#221) ──

    use crate::state_carry::testing::{add_run, add_step, artifact, exec, fresh_db, runs, steps};

    /// The shared state as another machine pushed it: two runs, one step each, in one file.
    async fn pushed_db(dir: &tempfile::TempDir, name: &str) -> String {
        let pushed = fresh_db(dir, name).await;
        let file = artifact(dir, "theirs.json");
        for run in ["theirs-1", "theirs-2"] {
            add_run(&pushed, run, "success").await;
            add_step(&pushed, run, "f.py:a", &file).await;
        }
        crate::state_sync::checkpoint_truncate(&pushed)
            .await
            .unwrap();
        pushed
    }

    /// A local database that pulled `theirs-1` earlier and then recorded a run that was never
    /// pushed, which is still in its write-ahead log.
    async fn local_db_with_an_unpushed_run(dir: &tempfile::TempDir) -> String {
        let local = fresh_db(dir, "local.db").await;
        let file = artifact(dir, "ours.json");
        add_run(&local, "theirs-1", "success").await;
        add_step(&local, "theirs-1", "f.py:a", &file).await;
        add_run(&local, "ours-unpushed", "running").await;
        add_step(&local, "ours-unpushed", "f.py:a", &file).await;
        add_step(&local, "ours-unpushed", "f.py:b", &file).await;
        assert!(
            !wal_is_clean(&local),
            "the test needs a non-empty local WAL"
        );
        local
    }

    const ALL_RUNS: [&str; 3] = [
        "ours-unpushed\trunning",
        "theirs-1\tsuccess",
        "theirs-2\tsuccess",
    ];
    const ALL_STEPS: [&str; 4] = [
        "ours-unpushed\tf.py:a\tsuccess",
        "ours-unpushed\tf.py:b\tsuccess",
        "theirs-1\tf.py:a\tsuccess",
        "theirs-2\tf.py:a\tsuccess",
    ];

    #[tokio::test]
    async fn replacing_the_db_keeps_unpushed_local_rows_and_no_old_log() {
        let dir = tempfile::tempdir().unwrap();
        let staged = pushed_db(&dir, "staged.db").await;
        let local = local_db_with_an_unpushed_run(&dir).await;

        let carried = pull_for_tests(&local, Path::new(&staged), "t").await;

        // The database is one file: the staged file was moved into place, and neither it nor
        // the old database left a log behind to be applied to the new file later.
        for gone in [
            staged.clone(),
            format!("{staged}-wal"),
            format!("{staged}-shm"),
            format!("{local}-wal"),
            format!("{local}-shm"),
        ] {
            assert!(!Path::new(&gone).exists(), "{gone} is still there");
        }
        // The pushed database plus the run only this machine had: nothing dropped on either
        // side, and the row both had is there once.
        assert_eq!((carried.runs, carried.steps), (1, 2), "{carried:?}");
        assert_eq!(runs(&local).await, ALL_RUNS);
        assert_eq!(steps(&local).await, ALL_STEPS);
    }

    #[tokio::test]
    async fn a_pull_that_dies_at_any_point_loses_nothing() {
        for stop in [
            ReplaceStage::Carried,
            ReplaceStage::SidecarsRemoved,
            ReplaceStage::Renamed,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let staged = pushed_db(&dir, "staged.db").await;
            let local = local_db_with_an_unpushed_run(&dir).await;
            let before = (runs(&local).await, steps(&local).await);

            // The process dies here: the staged file is abandoned where it is.
            let incoming = Incoming {
                staged: Path::new(&staged),
                token: "t",
                base_at_start: None,
            };
            replace_db_until(&local, incoming, Some(stop))
                .await
                .unwrap();

            // The local database is whole: what it was, unpushed run included, or already
            // the new one. Never a file under a log that belongs to another.
            let now = (runs(&local).await, steps(&local).await);
            if stop == ReplaceStage::Renamed {
                assert_eq!(now.0, ALL_RUNS);
                assert_eq!(now.1, ALL_STEPS);
            } else {
                assert_eq!(now, before, "{stop:?}");
            }

            // The next pull downloads again and ends where an undisturbed one would have.
            let again = pushed_db(&dir, "staged-again.db").await;
            pull_for_tests(&local, Path::new(&again), "t").await;
            assert_eq!(runs(&local).await, ALL_RUNS, "{stop:?}");
            assert_eq!(steps(&local).await, ALL_STEPS, "{stop:?}");
        }
    }

    #[tokio::test]
    async fn a_pull_after_a_completed_pull_adds_nothing_twice() {
        // The process dies right after the swap, before it pushes: the carried rows are local
        // only, and the next pull carries them again onto a fresh copy of the shared state.
        let dir = tempfile::tempdir().unwrap();
        let local = local_db_with_an_unpushed_run(&dir).await;
        for name in ["staged-1.db", "staged-2.db", "staged-3.db"] {
            let staged = pushed_db(&dir, name).await;
            pull_for_tests(&local, Path::new(&staged), "t").await;
            assert_eq!(runs(&local).await, ALL_RUNS, "{name}");
            assert_eq!(steps(&local).await, ALL_STEPS, "{name}");
        }
    }

    #[tokio::test]
    async fn a_pulled_file_that_is_not_a_database_does_not_replace_the_local_one() {
        // A download cut short, or a damaged blob.
        let dir = tempfile::tempdir().unwrap();
        let local = local_db_with_an_unpushed_run(&dir).await;
        let before = (runs(&local).await, steps(&local).await);
        let staged = dir.path().join("staged.db");
        fs::write(&staged, b"not a database").unwrap();

        let incoming = Incoming {
            staged: &staged,
            token: "t",
            base_at_start: None,
        };
        let err = replace_db(&local, incoming).await.unwrap_err().to_string();
        assert!(err.contains("left as it was"), "{err}");
        assert_eq!((runs(&local).await, steps(&local).await), before);
    }

    #[tokio::test]
    async fn with_no_local_database_the_pulled_one_is_moved_in() {
        for prepare in ["missing", "log without a main file"] {
            let dir = tempfile::tempdir().unwrap();
            let staged = pushed_db(&dir, "staged.db").await;
            let local = dir.path().join("local.db").to_string_lossy().to_string();
            if prepare == "log without a main file" {
                // What is left when only `metadata.db` was deleted by hand.
                let other = local_db_with_an_unpushed_run(&dir).await;
                fs::remove_file(&other).unwrap();
            }
            let carried = pull_for_tests(&local, Path::new(&staged), "t").await;
            assert!(!carried.wrote(), "{prepare}");
            // A leftover log is discarded, and that is said.
            assert_eq!(carried.note().is_some(), prepare != "missing", "{prepare}");
            assert_eq!(
                runs(&local).await,
                ["theirs-1\tsuccess", "theirs-2\tsuccess"],
                "{prepare}"
            );
        }
    }

    #[tokio::test]
    async fn a_local_file_that_is_not_a_database_is_replaced_and_reported() {
        for damage in ["garbage", "cut short"] {
            let dir = tempfile::tempdir().unwrap();
            let staged = pushed_db(&dir, "staged.db").await;
            let local = dir.path().join("local.db").to_string_lossy().to_string();
            if damage == "garbage" {
                fs::write(&local, b"garbage").unwrap();
                fs::write(format!("{local}-wal"), b"more garbage").unwrap();
            } else {
                // A real database that lost its tail.
                let whole = fs::read(pushed_db(&dir, "whole.db").await).unwrap();
                fs::write(&local, &whole[..whole.len() - 1000]).unwrap();
            }

            let carried = pull_for_tests(&local, Path::new(&staged), "t").await;
            assert!(carried.unreadable.is_some(), "{damage}: {carried:?}");
            assert!(carried.note().unwrap().contains("warning"), "{damage}");
            assert_eq!(
                runs(&local).await,
                ["theirs-1\tsuccess", "theirs-2\tsuccess"],
                "{damage}"
            );
        }
    }

    #[tokio::test]
    async fn a_local_database_that_cannot_be_read_for_another_reason_is_left_alone() {
        // Not positively damaged: it must never be replaced, whatever the reason turns out
        // to be. Here the file cannot be read at all.
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads anything
        }
        let dir = tempfile::tempdir().unwrap();
        let staged = pushed_db(&dir, "staged.db").await;
        let local = local_db_with_an_unpushed_run(&dir).await;
        let before = (runs(&local).await, steps(&local).await);
        fs::set_permissions(&local, fs::Permissions::from_mode(0o000)).unwrap();

        let incoming = Incoming {
            staged: Path::new(&staged),
            token: "t",
            base_at_start: None,
        };
        let result = replace_db(&local, incoming).await;
        fs::set_permissions(&local, fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err().to_string();
        assert!(err.contains("left as it was"), "{err}");
        assert_eq!((runs(&local).await, steps(&local).await), before);
        assert!(Path::new(&staged).exists(), "the download is not moved");
    }

    /// The shared state after another machine pushed once more: `pushed_db` plus a run whose
    /// step's artifact is on that machine only.
    async fn pushed_again_db(dir: &tempfile::TempDir, name: &str) -> String {
        let pushed = pushed_db(dir, name).await;
        let elsewhere = dir
            .path()
            .join("not-here.json")
            .to_string_lossy()
            .to_string();
        add_step(&pushed, "theirs-3", "f.py:b", &elsewhere).await;
        crate::state_carry::testing::add_run_from(&pushed, "theirs-3", "success", "machine-b")
            .await;
        crate::state_sync::checkpoint_truncate(&pushed)
            .await
            .unwrap();
        pushed
    }

    #[tokio::test]
    async fn a_download_overtaken_by_a_newer_pull_is_discarded() {
        // `status` on A downloads the shared state (T0). B pushes (T1). A `get` on A pulls T1
        // and swaps it in. Only then does the `status` reach its swap, holding T0. Swapping
        // it in would take B's step out of the local database (its run would be carried, its
        // step has no artifact here), and the `get` would then push that.
        let dir = tempfile::tempdir().unwrap();
        let local = local_db_with_an_unpushed_run(&dir).await;

        // status: reads the base record, downloads T0.
        let status_base = crate::state_base::read_raw(&local);
        let t0 = pushed_db(&dir, "t0.db").await;
        // get: pulls T1.
        let t1 = pushed_again_db(&dir, "t1.db").await;
        pull_for_tests(&local, Path::new(&t1), "t1").await;
        let after_get = (runs(&local).await, steps(&local).await);
        assert!(
            after_get
                .1
                .contains(&"theirs-3\tf.py:b\tsuccess".to_string())
        );

        // status: its swap is refused, and it is told what the local database is based on.
        let incoming = Incoming {
            staged: Path::new(&t0),
            token: "t0",
            base_at_start: status_base.as_deref(),
        };
        match replace_db(&local, incoming).await.unwrap() {
            Replaced::Superseded(Some(base)) => assert_eq!(base.token, "t1"),
            other => panic!("{other:?}"),
        }
        assert_eq!((runs(&local).await, steps(&local).await), after_get);
        assert!(Path::new(&t0).exists(), "the stale download is not moved");

        // get: pushes the local database. B's run and step are in both, once.
        crate::state_sync::checkpoint_truncate(&local)
            .await
            .unwrap();
        let shared = dir.path().join("shared.db").to_string_lossy().to_string();
        fs::copy(&local, &shared).unwrap();
        for db_path in [&local, &shared] {
            let theirs: Vec<String> = steps(db_path)
                .await
                .into_iter()
                .filter(|s| s.starts_with("theirs-3"))
                .collect();
            assert_eq!(theirs, ["theirs-3\tf.py:b\tsuccess"], "{db_path}");
            assert_eq!(runs(db_path).await.len(), 4, "{db_path}");
        }
    }

    #[tokio::test]
    async fn a_swap_cut_short_still_discards_downloads_begun_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let local = local_db_with_an_unpushed_run(&dir).await;
        let early_base = crate::state_base::read_raw(&local);
        let t0 = pushed_db(&dir, "t0.db").await;

        // Another pull swaps T1 in and dies before it records what the database is based on.
        let t1 = pushed_again_db(&dir, "t1.db").await;
        let incoming = Incoming {
            staged: Path::new(&t1),
            token: "t1",
            base_at_start: early_base.as_deref(),
        };
        replace_db_until(&local, incoming, Some(ReplaceStage::Renamed))
            .await
            .unwrap();

        // The earlier download is refused, with no token to adopt: the caller pulls again.
        let incoming = Incoming {
            staged: Path::new(&t0),
            token: "t0",
            base_at_start: early_base.as_deref(),
        };
        match replace_db(&local, incoming).await.unwrap() {
            Replaced::Superseded(Some(base)) => assert_eq!(base.token, ""),
            other => panic!("{other:?}"),
        }
        assert_eq!(runs(&local).await.len(), 4);
        // And that next pull goes through.
        let t1_again = pushed_again_db(&dir, "t1-again.db").await;
        pull_for_tests(&local, Path::new(&t1_again), "t1").await;
        assert_eq!(runs(&local).await.len(), 4);
    }

    #[tokio::test]
    async fn a_database_nothing_wrote_to_since_its_last_pull_is_replaced_without_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.db").to_string_lossy().to_string();
        let t0 = pushed_db(&dir, "t0.db").await;
        pull_for_tests(&local, Path::new(&t0), "t0").await;
        let base = crate::state_base::read(&local).unwrap();
        assert!(!base.unpushed);

        // Untouched since: the next download just takes its place.
        let t1 = pushed_again_db(&dir, "t1.db").await;
        let carried = pull_for_tests(&local, Path::new(&t1), "t1").await;
        assert!(!carried.compared, "the local database was opened");
        assert_eq!(runs(&local).await.len(), 3);

        // Written to since (a run recorded locally): compared, and the run is kept.
        add_run(&local, "ours", "running").await;
        let t1_again = pushed_again_db(&dir, "t1-again.db").await;
        let carried = pull_for_tests(&local, Path::new(&t1_again), "t1").await;
        assert!(carried.compared);
        assert_eq!(carried.runs, 1);
        assert!(crate::state_base::read(&local).unwrap().unpushed);

        // Until something pushes them, the same rows are kept by every pull, and said once.
        assert!(carried.note().is_some());
        let t2 = pushed_again_db(&dir, "t2.db").await;
        let again = pull_for_tests(&local, Path::new(&t2), "t2").await;
        assert_eq!((again.runs, again.compared), (1, true));
        assert_eq!(again.note(), None);
        assert_eq!(runs(&local).await.len(), 4);

        // After a push the database is a shared blob again.
        let copy = copy_for_push(&local, dir.path().join("push.db"))
            .await
            .unwrap();
        record_pushed(&local, &copy, "t3").await;
        assert!(crate::state_base::read(&local).unwrap().trusted(&local));
        let t4 = pushed_again_db(&dir, "t4.db").await;
        add_run(&t4, "ours", "running").await;
        crate::state_sync::checkpoint_truncate(&t4).await.unwrap();
        let carried = pull_for_tests(&local, Path::new(&t4), "t4").await;
        assert!(!carried.compared);
    }

    #[tokio::test]
    async fn an_empty_or_foreign_local_file_is_replaced_with_a_warning() {
        for kind in ["empty", "not barca's"] {
            let dir = tempfile::tempdir().unwrap();
            let staged = pushed_db(&dir, "staged.db").await;
            let local = dir.path().join("local.db").to_string_lossy().to_string();
            if kind == "empty" {
                fs::write(&local, b"").unwrap();
            } else {
                exec(&local, "CREATE TABLE notes (body TEXT)", vec![]).await;
                exec(&local, "INSERT INTO notes VALUES ('mine')", vec![]).await;
            }
            let carried = pull_for_tests(&local, Path::new(&staged), "t").await;
            let note = carried.note().expect(kind);
            assert!(note.contains("warning"), "{kind}: {note}");
            let why = if kind == "empty" {
                "it is empty"
            } else {
                "it has no barca tables"
            };
            assert!(note.contains(why), "{kind}: {note}");
            assert_eq!(runs(&local).await.len(), 2, "{kind}");
        }
    }

    #[tokio::test]
    async fn an_empty_main_file_beside_a_log_with_rows_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let staged = pushed_db(&dir, "staged.db").await;
        let local = local_db_with_an_unpushed_run(&dir).await;
        fs::write(&local, b"").unwrap();
        let wal = fs::read(format!("{local}-wal")).unwrap();

        let incoming = Incoming {
            staged: Path::new(&staged),
            token: "t",
            base_at_start: None,
        };
        let err = replace_db(&local, incoming).await.unwrap_err().to_string();
        assert!(err.contains("left as it was"), "{err}");
        assert_eq!(fs::read(format!("{local}-wal")).unwrap(), wal);
        assert_eq!(fs::metadata(&local).unwrap().len(), 0);
        assert!(Path::new(&staged).exists());
    }

    #[tokio::test]
    async fn a_database_created_where_one_was_deleted_is_never_taken_for_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.db").to_string_lossy().to_string();
        let t0 = pushed_db(&dir, "t0.db").await;
        pull_for_tests(&local, Path::new(&t0), "t0").await;
        let record = crate::state_base::read_raw(&local).unwrap();
        assert!(crate::state_base::read(&local).unwrap().trusted(&local));

        // Deleted by hand; the next command to open it creates an empty one.
        fs::remove_file(&local).unwrap();
        init_db(&local).await.unwrap();
        assert_eq!(
            crate::state_base::read_raw(&local),
            None,
            "creating a database forgets what the old one was based on"
        );
        // Even with the old record put back (by a tool, or a creation barca did not see), the
        // new file is not the one it describes.
        fs::write(crate::state_base::path(&local), &record).unwrap();
        let base = crate::state_base::read(&local).unwrap();
        assert!(!base.trusted(&local));
        // So a run recorded in it is compared and kept by the next pull.
        add_run(&local, "ours", "running").await;
        let t0_again = pushed_db(&dir, "t0-again.db").await;
        let carried = pull_for_tests(&local, Path::new(&t0_again), "t0").await;
        assert_eq!((carried.compared, carried.runs), (true, 1));
        assert_eq!(runs(&local).await.len(), 3);
    }

    #[tokio::test]
    async fn a_push_records_its_token_only_if_nothing_touched_the_database_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let local = local_db_with_an_unpushed_run(&dir).await;

        // Untouched during the upload: the database is the blob that was uploaded.
        let copy = copy_for_push(&local, dir.path().join("push-1.db"))
            .await
            .unwrap();
        assert_eq!(runs(&copy.path.to_string_lossy()).await.len(), 2);
        record_pushed(&local, &copy, "t1").await;
        let base = crate::state_base::read(&local).unwrap();
        assert_eq!(base.token, "t1");
        assert!(base.trusted(&local));

        // Another run records something while the upload is on its way: the database is no
        // longer what was uploaded, and nothing is claimed about it.
        let copy = copy_for_push(&local, dir.path().join("push-2.db"))
            .await
            .unwrap();
        add_run(&local, "during-upload", "running").await;
        record_pushed(&local, &copy, "t2").await;
        let base = crate::state_base::read(&local).unwrap();
        assert_eq!(base.token, "t1", "the record was not advanced");
        assert!(!base.trusted(&local));
        // The next pull therefore compares, and keeps that run.
        let staged = pushed_db(&dir, "staged.db").await;
        let carried = pull_for_tests(&local, Path::new(&staged), "t3").await;
        assert!(carried.compared);
        assert!(
            runs(&local)
                .await
                .contains(&"during-upload\trunning".to_string())
        );
    }

    /// The tables as barca 0.13 created them: no `run_id`, `pid`, `host`, `output_hash`,
    /// `error_type`.
    async fn old_schema_db(dir: &tempfile::TempDir, name: &str, run_id: &str) -> String {
        let path = dir.path().join(name).to_string_lossy().to_string();
        for sql in [
            "CREATE TABLE materializations (id INTEGER PRIMARY KEY AUTOINCREMENT, \
             node_id TEXT NOT NULL, run_hash TEXT, output_json TEXT, artifact_path TEXT, \
             artifact_format TEXT, artifact_size_bytes INTEGER, elapsed_seconds REAL, \
             status TEXT NOT NULL DEFAULT 'success', error_message TEXT, error_traceback TEXT, \
             attempts INTEGER DEFAULT 1, sinks_json TEXT, created_at TEXT DEFAULT (datetime('now')))",
            "CREATE TABLE runs (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT UNIQUE NOT NULL, \
             command TEXT NOT NULL, files TEXT NOT NULL, target TEXT, \
             status TEXT NOT NULL DEFAULT 'running', steps_total INTEGER, \
             steps_executed INTEGER DEFAULT 0, steps_cached INTEGER DEFAULT 0, \
             started_at TEXT DEFAULT (datetime('now')), finished_at TEXT, elapsed_seconds REAL)",
            "INSERT INTO materializations (node_id, run_hash, artifact_path) VALUES ('f.py:old', 'h', '/x')",
        ] {
            exec(&path, sql, vec![]).await;
        }
        exec(
            &path,
            "INSERT INTO runs (run_id, command, files, status) VALUES (?1, 'get', 'f.py', 'success')",
            vec![turso::Value::Text(run_id.into())],
        )
        .await;
        path
    }

    #[tokio::test]
    async fn databases_from_an_older_schema_are_migrated_before_rows_are_carried() {
        let dir = tempfile::tempdir().unwrap();

        // An old local database under a current shared one: its run is carried. Its step row
        // has no run id (0.17 added it), so it cannot be told from a pushed one: not carried.
        let staged = pushed_db(&dir, "staged.db").await;
        let old_local = old_schema_db(&dir, "old-local.db", "old-run").await;
        let carried = pull_for_tests(&old_local, Path::new(&staged), "t").await;
        assert_eq!((carried.runs, carried.steps), (1, 0), "{carried:?}");
        assert_eq!(
            runs(&old_local).await,
            ["old-run\tsuccess", "theirs-1\tsuccess", "theirs-2\tsuccess"]
        );

        // A current local database under a shared one an old barca pushed.
        let old_staged = old_schema_db(&dir, "old-staged.db", "old-theirs").await;
        crate::state_sync::checkpoint_truncate(&old_staged)
            .await
            .unwrap();
        let local = local_db_with_an_unpushed_run(&dir).await;
        let carried = pull_for_tests(&local, Path::new(&old_staged), "t").await;
        assert_eq!((carried.runs, carried.steps), (2, 3), "{carried:?}");
        assert_eq!(
            runs(&local).await,
            [
                "old-theirs\tsuccess",
                "ours-unpushed\trunning",
                "theirs-1\tsuccess"
            ]
        );
    }

    #[tokio::test]
    async fn a_running_run_whose_process_is_gone_is_reported_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // A pid that existed and is certainly gone: a child that has been reaped.
        let mut child = std::process::Command::new("true").spawn().unwrap();
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
}
