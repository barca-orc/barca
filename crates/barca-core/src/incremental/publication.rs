//! Replacement of generation-scoped freshness evidence, separate from history recording.
//! The caller retains its CacheReader guards through preparation and this transaction.
use super::{Proof, db_error, generation};
use crate::{BarcaError, db};
use std::collections::HashMap;

/// Publish only evidence prepared against an unchanged metadata generation. The write
/// reservation covers validation and replacement together; another process cannot insert
/// materializations between checking for interference and committing the proof.
/// This does not order cache-only observations: they still need explicit revision ownership.
pub(super) async fn publish_proofs(
    conn: &libsql::Connection,
    key: &str,
    validated_through: i64,
    generation: i64,
    run_id: &str,
    proofs: &HashMap<String, Proof>,
    discarded: &[String],
) -> Result<bool, BarcaError> {
    db::with_tx(conn, "BEGIN IMMEDIATE", "freshness proofs", async {
        if self::generation(conn).await? != generation {
            return Ok(false);
        }
        let mut rows = conn.query(
            "SELECT 1 FROM materializations NOT INDEXED WHERE id > ?1 AND (run_id IS NULL OR run_id != ?2) LIMIT 1",
            (validated_through, run_id.to_owned()),
        ).await.map_err(db_error)?;
        if rows.next().await.map_err(db_error)?.is_some() {
            return Ok(false);
        }
        drop(rows);
        conn.execute("DELETE FROM freshness_proofs WHERE generation != ?1", [generation]).await.map_err(db_error)?;
        // Cache-only reversion can keep the same metadata generation. Explicitly
        // remove discarded proofs so an oversized selection cannot revive one.
        let mut remove = db::prepare(conn, "DELETE FROM freshness_proofs WHERE id = ?1").await.map_err(db_error)?;
        for id in discarded {
            remove.execute([format!("{key}:{id}")]).await.map_err(db_error)?;
        }
        let mut insert = db::prepare(conn, "INSERT OR REPLACE INTO freshness_proofs (id, generation, snapshot) VALUES (?1, ?2, ?3)").await.map_err(db_error)?;
        for (id, proof) in proofs {
            let snapshot = serde_json::to_string(proof).map_err(|e| BarcaError::Other(e.to_string()))?;
            insert.execute((format!("{key}:{id}"), generation, snapshot)).await.map_err(db_error)?;
        }
        Ok(true)
    }).await
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    async fn database() -> (tempfile::TempDir, db::DbHandle, libsql::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proofs.db").to_str().unwrap().to_owned();
        db::init_db(&path).await.unwrap();
        let (database, conn) = db::open_conn(&path).await.unwrap();
        (dir, database, conn)
    }

    async fn materialize(conn: &libsql::Connection, run: &str) {
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, run_id) VALUES ('p.py:a', 'h', ?1)",
            [run],
        )
        .await
        .unwrap();
    }

    fn proofs() -> HashMap<String, Proof> {
        HashMap::from([("p.py:a".to_owned(), Proof::default())])
    }

    async fn count(conn: &libsql::Connection) -> i64 {
        conn.query("SELECT count(*) FROM freshness_proofs", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap()
    }

    #[tokio::test]
    async fn a_materialization_after_preparation_rejects_the_proof_without_deleting_prior_evidence()
    {
        let (_dir, _db, conn) = database().await;
        let generation = generation(&conn).await.unwrap();
        assert!(
            publish_proofs(
                &conn,
                "scope",
                generation,
                generation,
                "mine",
                &proofs(),
                &[]
            )
            .await
            .unwrap()
        );
        // A separate connection commits after this run prepared its proof.
        let other_db = libsql::Builder::new_local(_dir.path().join("proofs.db"))
            .build()
            .await
            .unwrap();
        let other = other_db.connect().unwrap();
        materialize(&other, "other").await;
        assert!(
            !publish_proofs(
                &conn,
                "scope",
                generation,
                generation,
                "mine",
                &HashMap::new(),
                &["p.py:a".to_owned()]
            )
            .await
            .unwrap()
        );
        assert_eq!(count(&conn).await, 1);
    }

    #[tokio::test]
    async fn foreign_work_is_rejected_even_if_it_precedes_the_generation_read() {
        let (_dir, _db, conn) = database().await;
        let validated = generation(&conn).await.unwrap();
        materialize(&conn, "other").await;
        let prepared = generation(&conn).await.unwrap();
        assert!(
            !publish_proofs(&conn, "scope", validated, prepared, "mine", &proofs(), &[])
                .await
                .unwrap()
        );
        assert_eq!(count(&conn).await, 0);
    }

    #[tokio::test]
    async fn own_progress_and_cache_only_discard_publish_at_the_same_generation() {
        let (_dir, _db, conn) = database().await;
        let validated = generation(&conn).await.unwrap();
        materialize(&conn, "mine").await;
        let prepared = generation(&conn).await.unwrap();
        assert!(
            publish_proofs(&conn, "scope", validated, prepared, "mine", &proofs(), &[])
                .await
                .unwrap()
        );
        assert_eq!(count(&conn).await, 1);
        assert!(
            publish_proofs(
                &conn,
                "scope",
                prepared,
                prepared,
                "mine",
                &HashMap::new(),
                &["p.py:a".to_owned()]
            )
            .await
            .unwrap()
        );
        assert_eq!(count(&conn).await, 0);
    }

    #[tokio::test]
    async fn a_failed_replacement_rolls_back_proof_deletions() {
        let (_dir, _db, conn) = database().await;
        let prepared = generation(&conn).await.unwrap();
        assert!(
            publish_proofs(&conn, "scope", prepared, prepared, "mine", &proofs(), &[])
                .await
                .unwrap()
        );
        conn.execute("CREATE TRIGGER reject_proof BEFORE INSERT ON freshness_proofs BEGIN SELECT RAISE(ABORT, 'injected proof failure'); END", ()).await.unwrap();
        assert!(
            publish_proofs(
                &conn,
                "scope",
                prepared,
                prepared,
                "mine",
                &proofs(),
                &["p.py:a".to_owned()]
            )
            .await
            .is_err()
        );
        assert_eq!(count(&conn).await, 1);
    }
}
