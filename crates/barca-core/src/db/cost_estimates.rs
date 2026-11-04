//! Writes of `cost_estimates`, the measured-cost EWMA per node ([`crate::persist`] writes it
//! inside the terminal ledger's transaction).

use crate::db;
use libsql::Connection;

/// Replaces a node's estimate. Prepared once and rebound for each node (#460).
pub(crate) struct UpsertEstimate(db::Prepared);

impl UpsertEstimate {
    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(
            conn,
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(node_id) DO UPDATE SET
             estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
             samples = ?6, updated_at = datetime('now')",
        )
        .await
        .map(Self)
    }

    pub(crate) async fn run(
        &mut self,
        node_id: &str,
        base_id: &str,
        est: &crate::cost::NodeEstimate,
    ) -> Result<u64, libsql::Error> {
        self.0
            .execute([
                node_id.to_string(),
                base_id.to_string(),
                est.estimate_seconds.to_string(),
                est.cpu_seconds.to_string(),
                est.max_rss_bytes.to_string(),
                est.samples.to_string(),
            ])
            .await
    }
}
