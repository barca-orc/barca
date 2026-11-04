//! Writes that change or remove `materializations` rows (and the `definition_components` they
//! name) after they were recorded: trimming, eviction marks and upload digests. Recording rows
//! is [`crate::persist`]'s.

use libsql::Connection;

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Clear `components` of the rows `ids`. The number of rows changed.
pub(crate) async fn clear_components(conn: &Connection, ids: &[i64]) -> Result<u64, libsql::Error> {
    conn.execute(
        &format!(
            "UPDATE materializations SET components = NULL WHERE id IN ({})",
            id_list(ids)
        ),
        (),
    )
    .await
}

/// Delete the rows `ids`. The number of rows deleted.
pub(crate) async fn delete_rows(conn: &Connection, ids: &[i64]) -> Result<u64, libsql::Error> {
    conn.execute(
        &format!(
            "DELETE FROM materializations WHERE id IN ({})",
            id_list(ids)
        ),
        (),
    )
    .await
}

/// Delete the definitions no row's `components` names.
pub(crate) async fn delete_unnamed_definitions(conn: &Connection) -> Result<u64, libsql::Error> {
    conn.execute(
        "DELETE FROM definition_components WHERE definition_hash NOT IN (SELECT \
         json_extract(components, '$.\"@definition\"') FROM materializations \
         WHERE components IS NOT NULL AND json_valid(components) \
         AND json_extract(components, '$.\"@definition\"') IS NOT NULL)",
        (),
    )
    .await
}

/// Record `sha256` as the digest of the successful rows at `artifact_path` that have none.
pub(crate) async fn set_upload_digest(
    conn: &Connection,
    artifact_path: &str,
    sha256: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE materializations SET output_hash = ?2 \
         WHERE artifact_path = ?1 AND status = 'success' AND output_hash IS NULL",
        [
            libsql::Value::Text(artifact_path.to_string()),
            libsql::Value::Text(sha256.to_string()),
        ],
    )
    .await
}

/// Mark the successful rows at `artifact_path` evicted, now.
pub(crate) async fn mark_evicted(
    conn: &Connection,
    artifact_path: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE materializations SET evicted_at = datetime('now') \
         WHERE artifact_path = ?1 AND status = 'success'",
        [artifact_path],
    )
    .await
}
