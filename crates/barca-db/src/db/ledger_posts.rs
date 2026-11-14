//! Writes of `ledger_posts`, this machine's record of run posts to a server
//! (`barca_core::ledger::posts` owns when each happens).

use libsql::Connection;

/// Record the outcome of posting `client_run_id` to `server` (`state`, the server's run id or
/// the error, empty for none), counting the attempt.
pub async fn upsert(
    conn: &Connection,
    client_run_id: &str,
    server: &str,
    body: &str,
    state: &str,
    server_run_id: &str,
    error: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "INSERT INTO ledger_posts (client_run_id, server, body, state, server_run_id, error, attempts) \
         VALUES (?1, ?2, ?3, ?4, NULLIF(?5, ''), NULLIF(?6, ''), 1) \
         ON CONFLICT(client_run_id) DO UPDATE SET server = excluded.server, \
         body = excluded.body, state = excluded.state, \
         server_run_id = excluded.server_run_id, error = excluded.error, \
         attempts = ledger_posts.attempts + 1, updated_at = datetime('now')",
        [
            client_run_id.to_string(),
            server.to_string(),
            body.to_string(),
            state.to_string(),
            server_run_id.to_string(),
            error.to_string(),
        ],
    )
    .await
}

pub async fn delete(conn: &Connection, client_run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute(
        "DELETE FROM ledger_posts WHERE client_run_id = ?1",
        [client_run_id],
    )
    .await
}

/// Replace the body of the post of `client_run_id`.
pub async fn set_body(
    conn: &Connection,
    client_run_id: &str,
    body: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE ledger_posts SET body = ?2, updated_at = datetime('now') \
         WHERE client_run_id = ?1",
        [client_run_id, body],
    )
    .await
}
