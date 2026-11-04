//! Writes of `freshness_proofs`. [`crate::incremental`]'s proof publication owns the guarded
//! transaction (the generation and foreign-work checks) these run in.

use crate::db;
use libsql::Connection;

/// Delete the proofs of every generation but `generation`.
pub(crate) async fn delete_other_generations(
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
pub(crate) struct DeleteProof(db::Prepared);

impl DeleteProof {
    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(conn, "DELETE FROM freshness_proofs WHERE id = ?1")
            .await
            .map(Self)
    }

    pub(crate) async fn run(&mut self, id: String) -> Result<u64, libsql::Error> {
        self.0.execute([id]).await
    }
}

/// Writes one proof, replacing any under its id. Prepared once and rebound for each.
pub(crate) struct ReplaceProof(db::Prepared);

impl ReplaceProof {
    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(
            conn,
            "INSERT OR REPLACE INTO freshness_proofs (id, generation, snapshot) VALUES (?1, ?2, ?3)",
        )
        .await
        .map(Self)
    }

    pub(crate) async fn run(
        &mut self,
        id: String,
        generation: i64,
        snapshot: String,
    ) -> Result<u64, libsql::Error> {
        self.0.execute((id, generation, snapshot)).await
    }
}
