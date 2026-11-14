//! Reads and writes of `asset_pauses`: the assets, sensors and tasks an operator held back
//! (`barca pause`, `POST /pauses`). A paused node is not computed by a scheduled tick until
//! it is resumed (`barca_core::pauses`).

use super::*;

/// The table, as `init_db` creates it (`ddl::ADDED_TABLES`).
pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS asset_pauses (
    node_id TEXT PRIMARY KEY,
    paused_at INTEGER NOT NULL,
    via TEXT NOT NULL
)";

/// One paused node: since when (unix seconds) and what was paused to pause it (`via`: the
/// node itself, the node a `--downstream` pause started from, or a file).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AssetPause {
    pub id: String,
    pub paused_at: i64,
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

/// Resume `ids` (`None`: every paused node). Resuming a node that is not paused changes nothing.
pub async fn resume_assets(db_path: &str, ids: Option<&[String]>) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn).await? {
        return Ok(());
    }
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
