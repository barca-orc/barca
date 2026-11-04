//! Deletions from run history: a `runs` row and the `logs` lines recorded under it, and the
//! `outcome_json` of old runs. Recording runs is [`crate::persist`]'s.

use libsql::Connection;

pub(crate) async fn delete_run(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute("DELETE FROM runs WHERE run_id = ?1", [run_id])
        .await
}

pub(crate) async fn delete_logs(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute("DELETE FROM logs WHERE run_id = ?1", [run_id])
        .await
}

/// Clear `outcome_json` of up to `batch` of the oldest runs that have one, except the last
/// `kept` runs. The number of runs changed.
pub(crate) async fn clear_old_outcomes(
    conn: &Connection,
    kept: i64,
    batch: i64,
) -> Result<u64, libsql::Error> {
    conn.execute(
        &format!(
            "UPDATE runs SET outcome_json = NULL WHERE id IN (SELECT id FROM runs \
             WHERE outcome_json IS NOT NULL AND id < (SELECT id FROM runs ORDER BY \
             id DESC LIMIT 1 OFFSET {}) ORDER BY id LIMIT {batch})",
            kept - 1
        ),
        (),
    )
    .await
}
