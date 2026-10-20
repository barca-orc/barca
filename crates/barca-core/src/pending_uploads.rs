//! Results computed here whose upload to the artifact store has not succeeded yet (#390).
//!
//! When every step of a run succeeded and only an upload failed, the run is a success: its
//! rows are recorded with their store locations as usual, and each upload that did not reach
//! the store is kept here. `barca status` shows those nodes `unsynced` (`local_only`), and the
//! next command that opens the artifact store uploads them before its own work.
//!
//! The table is created the first time an upload is left pending, never by a reader: a
//! database that has never had one has no table, and every reader treats that as empty. It is
//! an additive table of metadata format 1 (`specs/metadata-schema.md`). Rows carry the host
//! that wrote them; only this host's rows are read, retried or shown, because the local file
//! they upload from exists only on that machine.

use crate::BarcaError;
use crate::db;
use turso::{Connection, Value};

const CREATE: &str = "CREATE TABLE IF NOT EXISTS pending_uploads (
    store_path TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    run_hash TEXT,
    run_id TEXT,
    local_path TEXT NOT NULL,
    host TEXT NOT NULL DEFAULT '',
    error TEXT,
    attempts INTEGER DEFAULT 0,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
)";

/// One result whose upload has not succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUpload {
    /// Where the object belongs in the store: the location its `materializations` row records.
    pub store_path: String,
    pub node_id: String,
    pub run_hash: Option<String>,
    pub run_id: Option<String>,
    /// The file the upload is made from, in this machine's artifact directory.
    pub local_path: String,
    /// The last upload error.
    pub error: Option<String>,
}

fn failed(what: &'static str) -> impl Fn(turso::Error) -> BarcaError {
    move |e| BarcaError::Db(format!("pending uploads: {what}: {e}"))
}

async fn has_table(conn: &Connection) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'pending_uploads'",
            (),
        )
        .await
        .map_err(failed("reading the schema"))?;
    let found = rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some();
    while rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some()
    {}
    Ok(found)
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s.clone()),
        _ => None,
    }
}

async fn read(conn: &Connection, host: &str) -> Result<Vec<PendingUpload>, BarcaError> {
    if !has_table(conn).await? {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT store_path, node_id, run_hash, run_id, local_path, error \
             FROM pending_uploads WHERE host = ?1 ORDER BY created_at, node_id",
            [host.to_string()],
        )
        .await
        .map_err(failed("reading"))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed("reading"))? {
        let value = |i| row.get_value(i).unwrap_or(Value::Null);
        out.push(PendingUpload {
            store_path: text(&value(0)).unwrap_or_default(),
            node_id: text(&value(1)).unwrap_or_default(),
            run_hash: text(&value(2)),
            run_id: text(&value(3)),
            local_path: text(&value(4)).unwrap_or_default(),
            error: text(&value(5)),
        });
    }
    Ok(out)
}

/// This host's pending uploads, oldest first. Empty when there is no database or no table;
/// nothing is created.
pub async fn list(db_path: &str) -> Result<Vec<PendingUpload>, BarcaError> {
    if !std::path::Path::new(db_path).exists() {
        return Ok(Vec::new());
    }
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    read(&conn, &db::local_host()).await
}

/// Keep `uploads` as pending (replacing an earlier row for the same object).
pub(crate) async fn record(db_path: &str, uploads: &[PendingUpload]) -> Result<(), BarcaError> {
    if uploads.is_empty() {
        return Ok(());
    }
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    conn.execute(CREATE, ())
        .await
        .map_err(failed("creating the table"))?;
    let host = db::local_host();
    for u in uploads {
        conn.execute(
            "INSERT INTO pending_uploads (store_path, node_id, run_hash, run_id, local_path, host, error, attempts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1) \
             ON CONFLICT(store_path) DO UPDATE SET node_id = ?2, run_hash = ?3, run_id = ?4, \
             local_path = ?5, host = ?6, error = ?7, attempts = attempts + 1, updated_at = datetime('now')",
            [
                Value::Text(u.store_path.clone()),
                Value::Text(u.node_id.clone()),
                u.run_hash.clone().map_or(Value::Null, Value::Text),
                u.run_id.clone().map_or(Value::Null, Value::Text),
                Value::Text(u.local_path.clone()),
                Value::Text(host.clone()),
                u.error.clone().map_or(Value::Null, Value::Text),
            ],
        )
        .await
        .map_err(failed("recording"))?;
    }
    Ok(())
}

/// What a retry found for one pending upload.
pub(crate) enum Settled {
    /// Uploaded; the SHA-256 of the bytes the store received, when the helper reported it.
    Uploaded(Option<String>),
    /// Failed again, with this error.
    Failed(String),
    /// Nothing left to upload (the local file is gone): forgotten. The row stays recorded;
    /// a run that needs the result finds no object and computes it again.
    Dropped,
}

/// Apply the outcome of a retry, by store location.
pub(crate) async fn settle(
    db_path: &str,
    outcomes: &[(String, Settled)],
) -> Result<(), BarcaError> {
    if outcomes.is_empty() {
        return Ok(());
    }
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(());
    }
    for (store_path, outcome) in outcomes {
        let at = Value::Text(store_path.clone());
        match outcome {
            Settled::Uploaded(sha256) => {
                conn.execute(
                    "DELETE FROM pending_uploads WHERE store_path = ?1",
                    [at.clone()],
                )
                .await
                .map_err(failed("clearing an upload"))?;
                if let Some(sha256) = sha256 {
                    conn.execute(
                        "UPDATE materializations SET output_hash = ?2 \
                         WHERE artifact_path = ?1 AND status = 'success' AND output_hash IS NULL",
                        [at, Value::Text(sha256.clone())],
                    )
                    .await
                    .map_err(failed("recording an upload's digest"))?;
                }
            }
            Settled::Failed(error) => {
                conn.execute(
                    "UPDATE pending_uploads SET error = ?2, attempts = attempts + 1, \
                     updated_at = datetime('now') WHERE store_path = ?1",
                    [at, Value::Text(error.clone())],
                )
                .await
                .map_err(failed("recording a failed retry"))?;
            }
            Settled::Dropped => {
                conn.execute("DELETE FROM pending_uploads WHERE store_path = ?1", [at])
                    .await
                    .map_err(failed("dropping an upload"))?;
            }
        }
    }
    Ok(())
}

/// The run JSON's `uploads` object (and the dry run's): this host's pending uploads after the
/// run, the error that left them pending, and the nodes they belong to. `None` when nothing is
/// pending and nothing failed.
pub(crate) fn report(
    pending: &[PendingUpload],
    error: Option<String>,
) -> Option<crate::results::UploadsReport> {
    if pending.is_empty() && error.is_none() {
        return None;
    }
    let mut nodes: Vec<String> = pending.iter().map(|u| u.node_id.clone()).collect();
    nodes.sort();
    nodes.dedup();
    Some(crate::results::UploadsReport {
        pending: pending.len(),
        error: error.or_else(|| pending.iter().rev().find_map(|u| u.error.clone())),
        nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(store: &str, node: &str) -> PendingUpload {
        PendingUpload {
            store_path: store.to_string(),
            node_id: node.to_string(),
            run_hash: Some("h".to_string()),
            run_id: Some("r".to_string()),
            local_path: format!("/local/{node}.json"),
            error: Some("denied".to_string()),
        }
    }

    async fn fresh(dir: &tempfile::TempDir, name: &str) -> String {
        let path = dir.path().join(name).display().to_string();
        db::init_db(&path).await.unwrap();
        path
    }

    #[tokio::test]
    async fn a_database_without_the_table_reads_empty_and_is_not_changed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            list(&dir.path().join("none.db").display().to_string())
                .await
                .unwrap()
                .is_empty()
        );
        let path = fresh(&dir, "m.db").await;
        assert!(list(&path).await.unwrap().is_empty());
        let (_db, conn) = db::open_conn(&path).await.unwrap();
        assert!(!has_table(&conn).await.unwrap());
    }

    #[tokio::test]
    async fn recorded_uploads_are_listed_retried_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(&dir, "m.db").await;
        record(
            &path,
            &[
                upload("s/a/h.json", "p.py:a"),
                upload("s/b/h.json", "p.py:b"),
            ],
        )
        .await
        .unwrap();
        // Recording the same object again replaces it.
        record(&path, &[upload("s/a/h.json", "p.py:a")])
            .await
            .unwrap();
        assert_eq!(list(&path).await.unwrap().len(), 2);
        settle(
            &path,
            &[
                ("s/a/h.json".to_string(), Settled::Uploaded(None)),
                (
                    "s/b/h.json".to_string(),
                    Settled::Failed("still denied".into()),
                ),
            ],
        )
        .await
        .unwrap();
        let left = list(&path).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].node_id, "p.py:b");
        assert_eq!(left[0].error.as_deref(), Some("still denied"));
        let report = report(&left, None).unwrap();
        assert_eq!(report.pending, 1);
        assert_eq!(report.error.as_deref(), Some("still denied"));
        assert_eq!(report.nodes, ["p.py:b"]);
        settle(&path, &[("s/b/h.json".to_string(), Settled::Dropped)])
            .await
            .unwrap();
        assert!(list(&path).await.unwrap().is_empty());
    }
}

/// Upload this host's pending results among `store_paths` now (see
/// [`crate::execution::upload_pending`]): `barca get -o url` calls it before signing, so a
/// `local only` result is uploaded and then signed rather than reported missing.
pub async fn upload_now(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
    store_paths: &[String],
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), BarcaError> {
    crate::execution::upload_pending(python, cfg, store_paths, cancel).await
}
