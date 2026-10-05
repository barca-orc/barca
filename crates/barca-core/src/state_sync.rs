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
use std::path::Path;
use tokio::process::Command;

/// Opaque concurrency token for the remote state blob (etag / generation /
/// sha256, depending on backend). `None` means the remote object is absent.
#[derive(Debug, Clone)]
pub struct StateToken(pub Option<String>);

#[derive(Debug)]
pub enum PushOutcome {
    /// Uploaded; carries the new token.
    Pushed(String),
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

/// Replace the local database at `cfg.db_path` with the shared state blob. Returns its token,
/// or `StateToken(None)` when the remote object doesn't exist yet (the local database is left
/// untouched for bootstrap).
///
/// Afterwards the local database is exactly the pulled blob: the blob is downloaded next to
/// the database and swapped in by [`crate::db::replace_db`], which also removes the old
/// database's write-ahead log so it is never applied to the new file (#221). Local rows that
/// were never pushed are discarded. The download itself holds no lock, so other barca
/// processes are not kept waiting on the network.
pub async fn pull_state(python: &Path, cfg: &ResolvedConfig) -> Result<StateToken, BarcaError> {
    let uri = cfg
        .state_uri
        .as_deref()
        .ok_or_else(|| BarcaError::Other("pull_state called without a state uri".into()))?;
    // Same directory as the database, so the swap is a rename on one filesystem.
    let staged = std::path::PathBuf::from(format!("{}.pull-{}", cfg.db_path, std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let result = pull_into(python, cfg, uri, &staged).await;
    // Gone already when it was swapped in; left behind only when the pull failed part-way.
    let _ = std::fs::remove_file(&staged);
    result
}

async fn pull_into(
    python: &Path,
    cfg: &ResolvedConfig,
    uri: &str,
    staged: &Path,
) -> Result<StateToken, BarcaError> {
    let out = state_cmd(python, cfg)
        .arg("pull")
        .arg(uri)
        .arg(staged)
        .output()
        .await
        .map_err(|e| BarcaError::Other(format!("failed to spawn state helper: {e}")))?;
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
    let token = parsed
        .get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    // The helper writes the file exactly when the remote object exists (it then has a token).
    if token.is_some() {
        if !staged.exists() {
            return Err(BarcaError::Other(format!(
                "shared state pull from {uri}: the helper reported a state object but wrote no file"
            )));
        }
        crate::db::replace_db(&cfg.db_path, staged).await?;
    }
    Ok(StateToken(token))
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

/// Conditionally upload `cfg.db_path` over the shared state blob.
/// Call `checkpoint_truncate` first — the WAL must be folded in.
pub async fn push_state(
    python: &Path,
    cfg: &ResolvedConfig,
    token: &StateToken,
) -> Result<PushOutcome, BarcaError> {
    let uri = cfg
        .state_uri
        .as_deref()
        .ok_or_else(|| BarcaError::Other("push_state called without a state uri".into()))?;
    let _g = crate::db::db_guard().await;
    let mut cmd = state_cmd(python, cfg);
    cmd.arg("push").arg(uri).arg(&cfg.db_path);
    if let Some(ref t) = token.0 {
        cmd.arg("--token").arg(t);
    }
    let out = cmd
        .output()
        .await
        .map_err(|e| BarcaError::Other(format!("failed to spawn state helper: {e}")))?;
    if out.status.code() == Some(EXIT_CONFLICT) {
        return Ok(PushOutcome::Conflict);
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
    Ok(PushOutcome::Pushed(new_token.to_string()))
}

/// Checkpoint the WAL into the main database file and verify nothing is
/// left behind. Must be called with no other connections open on the file
/// (the caller drops all handles first) and before any upload.
pub async fn checkpoint_truncate(db_path: &str) -> Result<(), BarcaError> {
    {
        let _g = crate::db::db_guard().await;
        let (_db, conn) = crate::db::open_conn(db_path).await?;
        // The pragma returns a (busy, log_pages, checkpointed_pages) row — use
        // query and drain it.
        let mut rows = conn
            .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
            .await
            .map_err(|e| BarcaError::Db(format!("wal_checkpoint(TRUNCATE) failed: {e}")))?;
        while let Some(_row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("wal_checkpoint(TRUNCATE) failed: {e}")))?
        {}
    }

    // Backstop: an upload of the main file is only valid if the WAL is gone.
    let wal = format!("{db_path}-wal");
    if let Ok(meta) = std::fs::metadata(&wal)
        && meta.len() > 0
    {
        return Err(BarcaError::Db(format!(
            "WAL not empty after checkpoint ({} bytes remain in {wal}) — \
                 refusing to upload a torn database",
            meta.len()
        )));
    }
    Ok(())
}

/// True when the sidecar WAL file is absent or empty.
pub fn wal_is_clean(db_path: &str) -> bool {
    match std::fs::metadata(format!("{db_path}-wal")) {
        Err(_) => true,
        Ok(m) => m.len() == 0,
    }
}

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
