//! Writes of the `barca serve` queue tables, `run_queue` and `server_state`
//! (`barca_core::serve::run_queue` owns when each happens and the transaction around it).

use libsql::Connection;

/// Add `run_id` as queued at `seq`.
pub async fn insert_queued(
    conn: &Connection,
    run_id: &str,
    seq: i64,
    spec: &str,
    queued_at: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "INSERT INTO run_queue (run_id, seq, state, spec, queued_at) VALUES (?1, ?2, 'queued', ?3, ?4)",
        [
            run_id.to_string(),
            seq.to_string(),
            spec.to_string(),
            queued_at.to_string(),
        ],
    )
    .await
}

/// Mark a queued `run_id` running; one in another state keeps it.
pub async fn mark_running(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE run_queue SET state = 'running' WHERE run_id = ?1 AND state = 'queued'",
        [run_id],
    )
    .await
}

pub async fn mark_draining(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE run_queue SET state = 'draining' WHERE run_id = ?1",
        [run_id],
    )
    .await
}

pub async fn delete(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute("DELETE FROM run_queue WHERE run_id = ?1", [run_id])
        .await
}

pub async fn mark_queued(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE run_queue SET state = 'queued' WHERE run_id = ?1",
        [run_id],
    )
    .await
}

pub async fn set_spec(conn: &Connection, run_id: &str, spec: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE run_queue SET spec = ?2 WHERE run_id = ?1",
        [run_id, spec],
    )
    .await
}

/// Set the `server_state` entry `key` to `value`.
pub async fn set_state(conn: &Connection, key: &str, value: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "INSERT INTO server_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
        [key.to_string(), value.to_string()],
    )
    .await
}

pub async fn clear_state(conn: &Connection, key: &str) -> Result<u64, libsql::Error> {
    conn.execute("DELETE FROM server_state WHERE key = ?1", [key])
        .await
}
