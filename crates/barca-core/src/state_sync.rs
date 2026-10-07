//! Shared remote state — pull/checkpoint/push of the metadata DB blob.
//!
//! The metadata DB is a turso-managed SQLite file. To share it across
//! machines it is stored as a single blob object: pulled at run start,
//! conditionally uploaded (etag/generation match) at run end. Blob transfer
//! is delegated to `python -m barca._state` so the fsspec extras and their
//! credential chains are the only cloud-auth surface; Python never opens
//! the database.
//!
//! Critical invariant: turso is WAL-only and barca's normal operation
//! leaves most data in `metadata.db-wal`. Before any upload the WAL must
//! be checkpointed into the main file (`PRAGMA wal_checkpoint(TRUNCATE)`)
//! — uploading the main file alone without this would upload an empty
//! database.

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::state_carry::Carried;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::process::Command;

/// Opaque concurrency token for the remote state blob (etag / generation /
/// sha256, depending on backend). `None` means the remote object is absent.
#[derive(Debug, Clone)]
pub struct StateToken(pub Option<String>);

#[derive(Debug)]
pub enum PushOutcome {
    /// Uploaded; carries the new token.
    Pushed {
        token: String,
        /// False when something was written to the local database while the upload was on
        /// its way (or a pull replaced it): what was uploaded is complete as of the copy, and
        /// the later rows are local only until the next push.
        local_unchanged: bool,
    },
    /// The remote changed since our token was read — re-pull and replay.
    Conflict,
}

const EXIT_CONFLICT: i32 = 3;

fn state_cmd(python: &Path, cfg: &ResolvedConfig) -> Command {
    let mut cmd = Command::new(python);
    cmd.arg("-m").arg("barca._state");
    if let Some(ref opts) = cfg.storage_options_json {
        cmd.env("BARCA_STORAGE_OPTIONS", opts);
    }
    cmd
}

/// What a pull did: the token of the shared state blob it downloaded, and what it kept of
/// the local database it replaced.
#[derive(Debug)]
pub struct Pulled {
    /// `StateToken(None)` when the remote object does not exist yet.
    pub token: StateToken,
    pub carried: Carried,
}

/// The name of the file a pull downloads into, next to the database (so the swap is a rename
/// on one filesystem): `<db>.pull-<host>-<pid>-<n>`. A push uploads a copy named the same way,
/// `<db>.push-…`.
fn staged_path(db_path: &str, kind: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    PathBuf::from(format!(
        "{db_path}.{kind}-{}-{}-{}",
        host_tag(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// This host's name as it appears in a staged file name (no `-`, which separates the fields).
fn host_tag() -> String {
    let host: String = crate::db::local_host()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if host.is_empty() {
        "host".to_string()
    } else {
        host
    }
}

/// A download nobody has touched for this long is abandoned whoever made it.
const ABANDONED_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Remove what pulls and pushes that died part-way left next to the database: `<db>.pull-…`
/// and `<db>.push-…` files (and their sidecars) made on this host by a process that is gone, or older than
/// [`ABANDONED_AFTER`]. A process id alone means nothing across machines or containers that
/// share the project directory, so another host's recent file is left alone. They are
/// abandoned downloads; the local database never depended on them.
fn remove_abandoned_pulls(db_path: &str) {
    let db = Path::new(db_path);
    let (Some(dir), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefixes = [format!("{name}.pull-"), format!("{name}.push-")];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let ours = host_tag();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(rest) = file_name
            .to_str()
            .and_then(|n| prefixes.iter().find_map(|p| n.strip_prefix(p.as_str())))
        else {
            continue;
        };
        let mut fields = rest.split('-');
        let (host, pid) = (
            fields.next(),
            fields.next().and_then(|p| p.parse::<i64>().ok()),
        );
        let dead_here =
            host == Some(ours.as_str()) && pid.is_some_and(|p| !crate::db::pid_alive(p));
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > ABANDONED_AFTER);
        if dead_here || old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How long the download of a pull's last attempt may take. That attempt holds the database's
/// lock, so every other barca command in the project waits for it; they give up after 60
/// seconds ([`crate::db`]'s lock wait). Shorter than that, so that a stalled store fails this
/// one pull and lets the others go on, instead of failing all of them.
const LOCKED_DOWNLOAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(45);

/// How many downloads a pull makes without holding the database's lock before it takes the
/// lock for the whole of one. A download is discarded when another barca process replaced or
/// pushed the local database while it was on its way.
const UNLOCKED_ATTEMPTS: u32 = 2;

/// Bring the local database at `cfg.db_path` up to the shared state blob. When the remote
/// object does not exist yet the token is `StateToken(None)` and the local database is left
/// untouched (the first push creates the shared state from it).
///
/// There is one path, whatever the local database is: the blob is downloaded next to the
/// database, with no lock held, and swapped in by [`crate::db::replace_db`], which carries
/// over the local rows the download lacks, never lets the old write-ahead log be applied to
/// the new file (#221), and refuses a download that another process's pull or push has
/// overtaken. The pull then starts again, the last time holding the lock throughout so that
/// nothing can overtake it. Afterwards the local database holds every row of the blob the
/// returned token names, plus the local rows that were never pushed.
pub async fn pull_state(python: &Path, cfg: &ResolvedConfig) -> Result<Pulled, BarcaError> {
    let uri = cfg
        .state_uri
        .as_deref()
        .ok_or_else(|| BarcaError::Other("pull_state called without a state uri".into()))?;
    remove_abandoned_pulls(&cfg.db_path);
    for attempt in 0..=UNLOCKED_ATTEMPTS {
        let lock = if attempt == UNLOCKED_ATTEMPTS {
            Some(crate::db::lock_db(&cfg.db_path).await?)
        } else {
            None
        };
        let staged = staged_path(&cfg.db_path, "pull");
        let limit = lock.is_some().then_some(LOCKED_DOWNLOAD_LIMIT);
        let result = pull_into(python, cfg, uri, &staged, lock.as_ref(), limit).await;
        // Gone already when it was swapped in; left behind when the pull failed part-way or
        // the download was discarded.
        let _ = std::fs::remove_file(&staged);
        let _ = crate::db::remove_sidecars(&staged.to_string_lossy());
        if let Some(pulled) = result? {
            return Ok(pulled);
        }
    }
    Err(BarcaError::Other(format!(
        "shared state pull from {uri}: the local database {} changed while its lock was held",
        cfg.db_path
    )))
}

/// One download and swap. `None` when the download was overtaken: try again.
async fn pull_into(
    python: &Path,
    cfg: &ResolvedConfig,
    uri: &str,
    staged: &Path,
    held: Option<&crate::db::DbLock>,
    limit: Option<std::time::Duration>,
) -> Result<Option<Pulled>, BarcaError> {
    // Read before the download begins: a change by the time of the swap means the download
    // may be older than the local database.
    let base_raw = crate::state_base::read_raw(&cfg.db_path);
    let mut download = state_cmd(python, cfg);
    download.arg("pull").arg(uri).arg(staged);
    let out = helper_output(download, limit).await.map_err(|e| match e {
        HelperFailed::Spawn(e) => BarcaError::Other(format!("failed to spawn state helper: {e}")),
        HelperFailed::TimedOut(limit) => BarcaError::Other(format!(
            "shared state pull from {uri}: the download did not finish within {}s and was \
             stopped. Other barca commands in this project were changing the local history at \
             the same time, so this download was made while holding its lock, which cannot be \
             held for longer.\nThe local history {} was left as it was. Run the command again; \
             if the store is slow or unreachable, BARCA_STATE=off runs with local history only.",
            limit.as_secs(),
            cfg.db_path
        )),
    })?;
    if !out.status.success() {
        return Err(BarcaError::Other(format!(
            "shared state pull from {uri} failed: {}\n\
             Fix the connection or credentials (barca docs remote), or set BARCA_STATE=off to \
             run with local history only.",
            helper_cause(&out.stderr)
        )));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| BarcaError::Other(format!("state pull: bad helper output: {e}")))?;
    let Some(token) = parsed.get("token").and_then(|t| t.as_str()) else {
        return Ok(Some(Pulled {
            token: StateToken(None),
            carried: Carried::default(),
        }));
    };
    // The helper writes the file exactly when the remote object exists (it then has a token).
    if !staged.exists() {
        return Err(BarcaError::Other(format!(
            "shared state pull from {uri}: the helper reported a state object but wrote no file"
        )));
    }
    let incoming = crate::db::Incoming {
        staged,
        base_at_start: base_raw.as_deref(),
        version: Some(token),
    };
    let replaced = match held {
        Some(lock) => crate::db::replace_db_holding(lock, &cfg.db_path, incoming).await?,
        None => crate::db::replace_db(&cfg.db_path, incoming).await?,
    };
    Ok(match replaced {
        crate::db::Replaced::Swapped(carried) => Some(Pulled {
            token: StateToken(Some(token.to_string())),
            carried,
        }),
        crate::db::Replaced::Superseded => None,
        crate::db::Replaced::Invalid(why) => {
            return Err(invalid_shared_state(uri, &cfg.db_path, &why));
        }
    })
}

/// The error for a shared state object that is not a database this version of barca can use
/// ([`crate::state_validate`]). It is not a connection or credentials problem and retrying
/// does not help, so it says what the object is, that nothing was changed, and how to put a
/// good history back.
fn invalid_shared_state(uri: &str, db_path: &str, why: &str) -> BarcaError {
    BarcaError::Other(format!(
        "the shared history {uri} is not a database barca can use: {why}.\n\
         The local history {db_path} was left as it was, and nothing was uploaded.\n\
         To repair it, put back an earlier copy of that object (a bucket version or a backup), \
         or remove the object and run `barca get` on the machine whose local history is the \
         most complete: that run uploads its history as the new shared history. See `barca \
         docs remote`, \"If the shared history is damaged\". Until then, BARCA_STATE=off runs \
         with local history only."
    ))
}

enum HelperFailed {
    Spawn(std::io::Error),
    TimedOut(std::time::Duration),
}

/// Run a state helper to its end and collect its output. With a `limit`, a helper still
/// running after that long is killed and the call fails.
async fn helper_output(
    mut cmd: Command,
    limit: Option<std::time::Duration>,
) -> Result<std::process::Output, HelperFailed> {
    // Dropping the future (the limit passed, or the run was cancelled) must not leave a
    // download running that nobody waits for.
    cmd.kill_on_drop(true);
    let output = cmd.output();
    match limit {
        None => output.await.map_err(HelperFailed::Spawn),
        Some(limit) => match tokio::time::timeout(limit, output).await {
            Ok(out) => out.map_err(HelperFailed::Spawn),
            Err(_) => Err(HelperFailed::TimedOut(limit)),
        },
    }
}

/// The reason a state helper failed: its last `error: ...` line (what `python -m barca._state`
/// prints), else its last non-empty line. Library warnings printed before it (google-auth's
/// quota-project notice, deprecation warnings) are dropped so they never reach the error.
fn helper_cause(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("error: "))
        .or_else(|| lines.last().copied())
        .unwrap_or("the state helper exited with an error")
        .to_string()
}

/// Conditionally upload the local database over the shared state blob.
///
/// What is uploaded is a copy taken under the database's lock, right after its write-ahead
/// log was folded in ([`crate::db::copy_for_push`]). The upload itself holds no lock, so a
/// slow one keeps no other barca command waiting. [`PushOutcome::Pushed`] says whether the
/// local database is still what was uploaded.
pub async fn push_state(
    python: &Path,
    cfg: &ResolvedConfig,
    token: &StateToken,
) -> Result<PushOutcome, BarcaError> {
    let uri = cfg
        .state_uri
        .as_deref()
        .ok_or_else(|| BarcaError::Other("push_state called without a state uri".into()))?;
    let copy = crate::db::copy_for_push(&cfg.db_path, staged_path(&cfg.db_path, "push")).await?;
    let uploaded = upload(python, cfg, uri, &copy.path, token).await;
    let _ = std::fs::remove_file(&copy.path);
    Ok(match uploaded? {
        Some(token) => PushOutcome::Pushed {
            local_unchanged: crate::db::record_pushed(&cfg.db_path, &copy).await,
            token,
        },
        None => PushOutcome::Conflict,
    })
}

/// The new token, or None when the remote no longer matches `token` (a conflict).
async fn upload(
    python: &Path,
    cfg: &ResolvedConfig,
    uri: &str,
    file: &Path,
    token: &StateToken,
) -> Result<Option<String>, BarcaError> {
    let mut cmd = state_cmd(python, cfg);
    cmd.arg("push").arg(uri).arg(file);
    if let Some(ref t) = token.0 {
        cmd.arg("--token").arg(t);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| BarcaError::Other(format!("failed to spawn state helper: {e}")))?;
    if out.status.code() == Some(EXIT_CONFLICT) {
        return Ok(None);
    }
    if !out.status.success() {
        return Err(BarcaError::Other(format!(
            "shared state push to {uri} failed: {}\n\
             Results were computed but the shared history was not updated: re-run, or set \
             BARCA_STATE=off (barca docs remote).",
            helper_cause(&out.stderr)
        )));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| BarcaError::Other(format!("state push: bad helper output: {e}")))?;
    let new_token = parsed
        .get("token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| BarcaError::Other("state push: helper returned no token".into()))?;
    Ok(Some(new_token.to_string()))
}

/// Checkpoint the WAL into the main database file and verify nothing is
/// left behind. Must be called with no other connections open on the file
/// (the caller drops all handles first).
pub async fn checkpoint_truncate(db_path: &str) -> Result<(), BarcaError> {
    crate::db::fold_log_locked(db_path).await
}

pub use crate::db::wal_is_clean;

#[allow(dead_code)]
fn _path_exists(p: &str) -> bool {
    Path::new(p).exists()
}

#[cfg(test)]
mod tests {
    #[test]
    fn helper_cause_drops_library_warnings() {
        let stderr = b"/x/google/auth/_default.py:113: UserWarning: Your application has \
authenticated using end user credentials\n  warnings.warn(_CLOUD_SDK_CREDENTIALS_WARNING)\n\
error: RefreshError: Reauthentication is needed.\n";
        assert_eq!(
            super::helper_cause(stderr),
            "RefreshError: Reauthentication is needed."
        );
        assert_eq!(super::helper_cause(b"\n  boom  \n"), "boom");
        assert_eq!(
            super::helper_cause(b""),
            "the state helper exited with an error"
        );
    }

    use super::*;
    use turso::Builder;

    /// The last attempt of a pull downloads while holding the database's lock. A helper that
    /// stalls is stopped at the limit, so the lock is given back; one that finishes in time
    /// is not disturbed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_helper_that_outlives_its_limit_is_killed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pid_file.display());
        let mut stalled = Command::new("sh");
        stalled.arg("-c").arg(&script);
        let started = std::time::Instant::now();
        let limit = std::time::Duration::from_millis(500);
        let failed = helper_output(stalled, Some(limit)).await;
        assert!(matches!(failed, Err(HelperFailed::TimedOut(l)) if l == limit));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // The process is gone, not left downloading.
        let pid: i64 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while crate::db::pid_alive(pid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !crate::db::pid_alive(pid),
            "the stalled helper is still running"
        );

        let mut quick = Command::new("sh");
        quick.arg("-c").arg("echo done");
        let out = helper_output(quick, Some(std::time::Duration::from_secs(30))).await;
        assert_eq!(out.ok().map(|o| o.stdout), Some(b"done\n".to_vec()));
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

    /// The load-bearing spike for shared remote state: after
    /// wal_checkpoint(TRUNCATE), the -wal sidecar must be empty/absent and
    /// all rows must be readable from the main file alone.
    #[tokio::test]
    async fn checkpoint_truncate_collapses_wal_into_main_file() {
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

        checkpoint_truncate(&db_path).await.unwrap();
        assert!(wal_is_clean(&db_path), "WAL must be empty after checkpoint");

        // The main file alone (simulate the uploaded blob: copy it without the WAL)
        // must contain every row.
        let copy = dir.path().join("uploaded.db").to_string_lossy().to_string();
        std::fs::copy(&db_path, &copy).unwrap();
        assert_eq!(open_and_count(&copy, "t").await, 200);
    }
}
