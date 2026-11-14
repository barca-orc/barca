//! Writes of `notifications`, the last post each integration made about a job
//! (`barca_core::telemetry` owns when).

use libsql::Connection;

/// Record that `integration` posted `state` about `job` at `posted_at`, replacing its previous
/// post. `message_ref` stays empty.
pub async fn record_post(
    conn: &Connection,
    job: &str,
    integration: &str,
    posted_at: &str,
    state: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "INSERT INTO notifications (job, integration, posted_at, state, message_ref) \
         VALUES (?1, ?2, ?3, ?4, '') ON CONFLICT(job, integration) DO UPDATE SET \
         posted_at = ?3, state = ?4, message_ref = ''",
        [
            job.to_string(),
            integration.to_string(),
            posted_at.to_string(),
            state.to_string(),
        ],
    )
    .await
}
