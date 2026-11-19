//! The metadata DB as a read-only request reads it.

use barca_core::{BarcaError, db};

/// A metadata DB a read-only request may query: the real one read in place
/// ([`db::ReadView`]) when this release uses it as it is; otherwise a private
/// snapshot of it, or — when there is no DB yet — an empty scratch DB, so the
/// absence is preserved. A copy has the schema ensured on it, never on the
/// original. Dropping it deletes the copy.
pub(crate) struct SnapshotDb {
    _view: Option<db::ReadView>,
    _snapshot: Option<db::DbSnapshot>,
    _scratch: Option<tempfile::TempDir>,
    pub(crate) path: String,
}

/// Read a compatible WAL database in place, including on a `--read-only` server.
/// SQLite can create `-wal` and `-shm`; the reader never migrates or checkpoints the DB.
pub(crate) async fn snapshot_of(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    db::directory_check(db_path)?;
    if let Some(view) = db::ReadView::open(db_path).await? {
        return Ok(SnapshotDb {
            path: view.path().to_string(),
            _view: Some(view),
            _snapshot: None,
            _scratch: None,
        });
    }
    let db_path = db_path.to_owned();
    // Schema/index additions on the private copy can also take seconds on a large DB.
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(snapshot_copy(&db_path))
    })
    .await
    .map_err(|e| BarcaError::Db(format!("snapshot task failed: {e}")))?
}

async fn snapshot_copy(db_path: &str) -> Result<SnapshotDb, BarcaError> {
    let (snapshot, scratch, path) = match db::DbSnapshot::take(db_path).await? {
        Some(s) => {
            let path = s.path().to_string();
            (Some(s), None, path)
        }
        None => {
            let dir = tempfile::tempdir()
                .map_err(|e| BarcaError::Db(format!("failed to create scratch dir: {e}")))?;
            let path = dir.path().join("metadata.db").display().to_string();
            (None, Some(dir), path)
        }
    };
    db::init_db_as(&path, db_path).await?;
    Ok(SnapshotDb {
        _view: None,
        _snapshot: snapshot,
        _scratch: scratch,
        path,
    })
}
