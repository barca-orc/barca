//! Writes of `sink_publications` ([`crate::sink_publications`] owns the transaction).

use crate::db;
use libsql::{Connection, Value};

/// Records that `run_id` on `host` published `run_hash` of `node_id` to `sink`, replacing the
/// sink's earlier publication. Prepared once and rebound for each.
pub(crate) struct UpsertPublication(db::Prepared);

impl UpsertPublication {
    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(
            conn,
            "INSERT INTO sink_publications (node_id, sink, run_hash, host, run_id) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(node_id, sink) DO UPDATE SET run_hash = ?3, host = ?4, run_id = ?5, \
             published_at = datetime('now')",
        )
        .await
        .map(Self)
    }

    pub(crate) async fn run(
        &mut self,
        node_id: &str,
        sink: &str,
        run_hash: &str,
        host: &str,
        run_id: &str,
    ) -> Result<u64, libsql::Error> {
        self.0
            .execute([
                Value::Text(node_id.to_string()),
                Value::Text(sink.to_string()),
                Value::Text(run_hash.to_string()),
                Value::Text(host.to_string()),
                Value::Text(run_id.to_string()),
            ])
            .await
    }
}
