//! Reads and writes of `asset_pauses`: the assets, sensors and tasks an operator held back
//! (`barca pause`, `POST /pauses`). A paused node is not computed by a scheduled tick until
//! it is resumed (`barca_service::pauses`).

use super::*;

/// The table, as `init_db` creates it (`ddl::ADDED_TABLES`).
pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS asset_pauses (
    node_id TEXT PRIMARY KEY,
    paused_at INTEGER NOT NULL,
    via TEXT NOT NULL
)";

/// One paused node: since when (unix seconds) and what was paused to pause it (`via`: the
/// node itself, the node a `--downstream` pause started from, or a file).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetPause {
    pub id: String,
    /// Unix seconds.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub paused_at: i64,
    /// What the pause named: the node, the node a downstream pause started from, or a file.
    #[serde(default)]
    pub via: String,
}

async fn has_table(conn: &libsql::Connection) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'asset_pauses'",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read the schema: {e}")))?;
    let found = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read the schema: {e}")))?
        .is_some();
    Ok(found)
}

/// Every paused node, by id order. A database without the table (one older than it, an empty
/// file a run is about to initialize, or none at all) has none.
pub async fn get_asset_pauses(db_path: &str) -> Result<Vec<AssetPause>, BarcaError> {
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Vec::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT node_id, paused_at, via FROM asset_pauses ORDER BY node_id",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query asset_pauses: {e}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read asset_pauses: {e}")))?
    {
        out.push(AssetPause {
            id: row.get::<String>(0).unwrap_or_default(),
            paused_at: row.get::<i64>(1).unwrap_or_default(),
            via: row.get::<String>(2).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Pause `ids`, each `via` the given target, at `now`. A node already paused keeps its
/// first pause time and `via`.
pub async fn pause_assets(
    db_path: &str,
    ids: &[String],
    via: &str,
    now: i64,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(CREATE, ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to create asset_pauses: {e}")))?;
    for id in ids {
        conn.execute(
            "INSERT INTO asset_pauses (node_id, paused_at, via) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO NOTHING",
            [id.clone(), now.to_string(), via.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record a pause: {e}")))?;
    }
    Ok(())
}

/// Resume `ids` (`None`: every paused node). Resuming a node that is not paused removes no
/// pause. The freshness proofs are forgotten, so the next run decides every node through the
/// cache: it recomputes what changed while the node was held and nothing else.
pub async fn resume_assets(db_path: &str, ids: Option<&[String]>) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(());
    }
    // Ticks read a paused node as it is, whatever changed above it. No run records a proof
    // of a node it held (`barca_service::incremental`), but a proof written by an earlier
    // version, or by a run that started before the pause, may say it is fresh: forget them
    // before the pause is lifted, so a crash in between leaves a pause without proofs and
    // never a resumed node with a stale one (#988).
    super::pins::forget_proofs(&conn).await?;
    match ids {
        None => {
            conn.execute("DELETE FROM asset_pauses", ())
                .await
                .map_err(|e| BarcaError::Db(format!("failed to resume: {e}")))?;
        }
        Some(ids) => {
            for id in ids {
                conn.execute("DELETE FROM asset_pauses WHERE node_id = ?1", [id.clone()])
                    .await
                    .map_err(|e| BarcaError::Db(format!("failed to resume: {e}")))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A proof on record when a node is resumed may have been taken while it was held (#988).
    #[tokio::test]
    async fn resuming_forgets_the_freshness_proofs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        let ids = ["p.py:a".to_string()];
        pause_assets(&path, &ids, "p.py:a", 1).await.unwrap();
        {
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute(
                "INSERT INTO freshness_proofs (id, generation, snapshot) VALUES ('k:p.py:a', 0, '{}')",
                (),
            )
            .await
            .unwrap();
        }
        resume_assets(&path, Some(&ids)).await.unwrap();
        assert!(get_asset_pauses(&path).await.unwrap().is_empty());
        let (_db, conn) = open_conn(&path).await.unwrap();
        let mut rows = conn
            .query("SELECT count(*) FROM freshness_proofs", ())
            .await
            .unwrap();
        let left: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(left, 0);
    }
}
