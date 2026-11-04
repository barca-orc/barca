//! Writes of `pending_uploads` ([`crate::pending_uploads`] owns when each happens).

use crate::db;
use crate::pending_uploads::PendingUpload;
use libsql::{Connection, Value};

/// Keeps an upload pending, replacing an earlier row for the same object and counting the
/// attempt. Prepared once and rebound for each upload.
pub(crate) struct RecordPending(db::Prepared);

impl RecordPending {
    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(
            conn,
            "INSERT INTO pending_uploads (store_path, node_id, run_hash, run_id, local_path, host, error, attempts) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1) \
                 ON CONFLICT(store_path) DO UPDATE SET node_id = ?2, run_hash = ?3, run_id = ?4, \
                 local_path = ?5, host = ?6, error = ?7, attempts = attempts + 1, updated_at = datetime('now')",
        )
        .await
        .map(Self)
    }

    pub(crate) async fn run(
        &mut self,
        u: &PendingUpload,
        host: &str,
    ) -> Result<u64, libsql::Error> {
        self.0
            .execute([
                Value::Text(u.store_path.clone()),
                Value::Text(u.node_id.clone()),
                u.run_hash.clone().map_or(Value::Null, Value::Text),
                u.run_id.clone().map_or(Value::Null, Value::Text),
                Value::Text(u.local_path.clone()),
                Value::Text(host.to_string()),
                u.error.clone().map_or(Value::Null, Value::Text),
            ])
            .await
    }
}

/// Forget the pending upload of `store_path`.
pub(crate) async fn delete(conn: &Connection, store_path: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "DELETE FROM pending_uploads WHERE store_path = ?1",
        [Value::Text(store_path.to_string())],
    )
    .await
}

/// A retry of `store_path` failed with `error`.
pub(crate) async fn record_failed_retry(
    conn: &Connection,
    store_path: &str,
    error: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE pending_uploads SET error = ?2, attempts = attempts + 1, \
         updated_at = datetime('now') WHERE store_path = ?1",
        [
            Value::Text(store_path.to_string()),
            Value::Text(error.to_string()),
        ],
    )
    .await
}
