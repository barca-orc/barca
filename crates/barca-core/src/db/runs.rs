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
