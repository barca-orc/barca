//! Writes of `freshness_proofs`. `barca_service::incremental`'s proof publication owns the guarded
//! transaction (the generation and foreign-work checks) these run in.

use crate::db;
use libsql::Connection;

/// Delete the proofs of every generation but `generation`.
pub async fn delete_other_generations(
    conn: &Connection,
    generation: i64,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "DELETE FROM freshness_proofs WHERE generation != ?1",
        [generation],
    )
    .await
}

/// Deletes one proof by id. Prepared once and rebound for each.
pub struct DeleteProof(db::Prepared);

impl DeleteProof {
    pub async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(conn, "DELETE FROM freshness_proofs WHERE id = ?1")
            .await
            .map(Self)
    }

    pub async fn run(&mut self, id: String) -> Result<u64, libsql::Error> {
        self.0.execute([id]).await
    }
}

/// Writes one proof, replacing any under its id. Prepared once and rebound for each.
pub struct ReplaceProof(db::Prepared);

impl ReplaceProof {
    pub async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(
            conn,
            "INSERT OR REPLACE INTO freshness_proofs (id, generation, snapshot) VALUES (?1, ?2, ?3)",
        )
        .await
        .map(Self)
    }

    pub async fn run(
        &mut self,
        id: String,
        generation: i64,
        snapshot: String,
    ) -> Result<u64, libsql::Error> {
        self.0.execute((id, generation, snapshot)).await
    }
}

/// Refresh changes cache eligibility without inserting a materialization. Advance a separate
/// revision in the caller's transaction so old in-flight proof publishers cannot make their
/// evidence current again. Drop obsolete proofs to keep retained evidence bounded.
pub async fn invalidate(conn: &Connection) -> Result<(), libsql::Error> {
    conn.execute("INSERT INTO metadata_info (key, value) VALUES ('freshness_revision', '1') ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1", ()).await?;
    conn.execute("DELETE FROM freshness_proofs", ()).await?;
    Ok(())
}

/// The revision of eligibility changes that do not append materializations.
pub async fn revision(conn: &Connection) -> Result<i64, libsql::Error> {
    let mut rows = conn
        .query(
            "SELECT CAST(value AS INTEGER) FROM metadata_info WHERE key = 'freshness_revision'",
            (),
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}
