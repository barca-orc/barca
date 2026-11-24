//! Reads and writes of `freshness_proofs`. `barca_run::incremental`'s proof publication owns the guarded
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

/// Current generation and one bounded proof. Invalid or oversized evidence is a cache miss.
pub async fn read(conn: &Connection, id: &str) -> Result<Option<(i64, String)>, libsql::Error> {
    let mut rows = conn.query(
        "SELECT generation, snapshot FROM freshness_proofs WHERE id = ?1 AND generation = COALESCE((SELECT id FROM materializations ORDER BY id DESC LIMIT 1), 0) AND length(snapshot) <= 524288",
        [id],
    ).await?;
    match rows.next().await? {
        Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
        None => Ok(None),
    }
}

/// Visit the selected successful paths with constant-size pages. Historical membership
/// beyond `expected` is deliberately rejected: only the full predictor can resolve it.
pub async fn inspect_paths(
    conn: &Connection,
    base: &str,
    generation: i64,
    expected: usize,
    selected: &std::collections::HashMap<String, String>,
    available: impl Fn(&str) -> bool,
) -> Result<bool, libsql::Error> {
    let mut after = format!("{base}[");
    let upper = format!("{base}\\");
    let mut count = 0;
    let page = conn.prepare(
        "SELECT w.node_id, (SELECT artifact_path FROM materializations m WHERE m.node_id = w.node_id AND m.id <= ?3 AND m.status = 'success' AND m.artifact_path IS NOT NULL ORDER BY m.id DESC LIMIT 1) FROM (SELECT DISTINCT node_id FROM materializations WHERE node_id > ?1 AND node_id < ?2 AND id <= ?3 ORDER BY node_id LIMIT 256) w"
    ).await?;
    let exception = conn.prepare("SELECT artifact_path FROM materializations WHERE node_id = ?1 AND id <= ?2 AND status = 'success' AND artifact_path IS NOT NULL AND run_hash = ?3 ORDER BY id DESC LIMIT 1").await?;
    loop {
        page.reset();
        let mut rows = page
            .query((after.clone(), upper.clone(), generation))
            .await?;
        let mut seen = 0;
        while let Some(row) = rows.next().await? {
            let id = row.get::<String>(0)?;
            count += 1;
            seen += 1;
            if count > expected {
                return Ok(false);
            }
            let path = if let Some(hash) = selected.get(&id) {
                exception.reset();
                let mut paths = exception
                    .query((id.clone(), generation, hash.clone()))
                    .await?;
                paths
                    .next()
                    .await?
                    .map(|row| row.get::<String>(0))
                    .transpose()?
            } else {
                row.get::<Option<String>>(1)?
            };
            if path.as_deref().is_none_or(|path| !available(path)) {
                return Ok(false);
            }
            after = id;
        }
        if seen == 0 {
            break;
        }
    }
    Ok(count == expected)
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
