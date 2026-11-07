//! `asset_pins`: assets held at a chosen earlier materialization (`barca pin`, `POST
//! /assets/{name}/pin`), and the reads a pin is chosen from (a node's versions: its successful
//! materializations with an artifact, one per run hash). While a node has a row, every run reads
//! it as that materialization and never computes it ([`crate::freshness::load_pins`]).

use super::*;
use crate::dispatch::OutputRef;
use serde::{Deserialize, Serialize};

/// The table, created by `init_db` and by the additive migration (`schema::ddl::ADDED_TABLES`).
pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS asset_pins (
    node_id TEXT PRIMARY KEY,
    run_hash TEXT NOT NULL,
    pinned_at TEXT NOT NULL,
    pinned_by TEXT,
    reason TEXT
)";

/// One pinned asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetPin {
    /// The node id (`file.py:name`).
    pub node_id: String,
    /// The run hash of the materialization it is held at.
    pub run_hash: String,
    /// When it was pinned (RFC 3339, UTC).
    pub pinned_at: String,
    /// Who pinned it, when that is known: `user@host` for the CLI, `token:<name>` for a request
    /// with a project token.
    pub pinned_by: Option<String>,
    /// Why, as given with `--reason`.
    pub reason: Option<String>,
}

/// One successful materialization of a node with an artifact: what a pin can choose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "AssetVersion"))]
pub struct Version {
    pub run_hash: String,
    /// The run that recorded it (null on rows older than 0.11).
    pub run_id: Option<String>,
    /// When it was recorded (RFC 3339, UTC).
    pub materialized_at: String,
    pub elapsed_seconds: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
    pub format: Option<String>,
    pub artifact: String,
    /// False when the cache size limit deleted the local file (a store may still hold it).
    pub available: bool,
    /// The node's latest successful materialization.
    pub current: bool,
    /// The materialization the node is pinned at.
    pub pinned: bool,
}

async fn has_table(conn: &libsql::Connection) -> Result<bool, BarcaError> {
    has_column(conn, "asset_pins", "node_id").await
}

fn pin_from(row: &libsql::Row) -> AssetPin {
    AssetPin {
        node_id: row.get::<String>(0).unwrap_or_default(),
        run_hash: row.get::<String>(1).unwrap_or_default(),
        pinned_at: row.get::<String>(2).unwrap_or_default(),
        pinned_by: row.get::<String>(3).ok(),
        reason: row.get::<String>(4).ok(),
    }
}

const SELECT: &str = "SELECT node_id, run_hash, pinned_at, pinned_by, reason FROM asset_pins";

/// Every pin on `conn`, by node id. A database without the table has none.
pub(crate) async fn pins_on(
    conn: &libsql::Connection,
) -> Result<std::collections::BTreeMap<String, AssetPin>, BarcaError> {
    let mut out = std::collections::BTreeMap::new();
    if !has_table(conn).await? {
        return Ok(out);
    }
    let mut rows = conn
        .query(SELECT, ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query asset pins: {e}")))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read an asset pin: {e}")))?
    {
        let pin = pin_from(&row);
        out.insert(pin.node_id.clone(), pin);
    }
    Ok(out)
}

/// Every pin in the database at `db_path`, ordered by node id.
pub async fn get_pins(db_path: &str) -> Result<Vec<AssetPin>, BarcaError> {
    if !Path::new(db_path).exists() {
        return Ok(Vec::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    Ok(pins_on(&conn).await?.into_values().collect())
}

/// A pin changes what runs read without a new materialization, so the freshness proofs
/// ([`crate::incremental`], valid for one materialization generation) no longer hold.
async fn forget_proofs(conn: &libsql::Connection) -> Result<(), BarcaError> {
    if has_column(conn, "freshness_proofs", "id").await? {
        conn.execute("DELETE FROM freshness_proofs", ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to clear freshness proofs: {e}")))?;
    }
    Ok(())
}

/// Record `pin`, replacing the node's earlier one.
pub async fn set_pin(db_path: &str, pin: &AssetPin) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let opt = |v: &Option<String>| v.clone().map_or(libsql::Value::Null, libsql::Value::Text);
    conn.execute(
        "INSERT INTO asset_pins (node_id, run_hash, pinned_at, pinned_by, reason) \
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(node_id) DO UPDATE SET \
         run_hash = ?2, pinned_at = ?3, pinned_by = ?4, reason = ?5",
        vec![
            libsql::Value::Text(pin.node_id.clone()),
            libsql::Value::Text(pin.run_hash.clone()),
            libsql::Value::Text(pin.pinned_at.clone()),
            opt(&pin.pinned_by),
            opt(&pin.reason),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to record the pin: {e}")))?;
    forget_proofs(&conn).await
}

/// Remove `node_id`'s pin. The pin it had, if any.
pub async fn clear_pin(db_path: &str, node_id: &str) -> Result<Option<AssetPin>, BarcaError> {
    if !Path::new(db_path).exists() {
        return Ok(None);
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let had = pins_on(&conn).await?.remove(node_id);
    if had.is_some() {
        conn.execute("DELETE FROM asset_pins WHERE node_id = ?1", [node_id])
            .await
            .map_err(|e| BarcaError::Db(format!("failed to remove the pin: {e}")))?;
        forget_proofs(&conn).await?;
    }
    Ok(had)
}

/// `created_at` (`YYYY-MM-DD HH:MM:SS`, UTC) as RFC 3339.
fn rfc3339(sqlite: &str) -> String {
    if sqlite.contains('T') {
        return sqlite.to_string();
    }
    format!("{}Z", sqlite.replacen(' ', "T", 1))
}

/// The latest `limit` versions of `node_id` (a display id), newest first: one per run hash (its
/// newest row), each with its artifact. `pinned` is the run hash it is pinned at, if any.
pub async fn get_versions(
    db_path: &str,
    node_id: &str,
    limit: usize,
    pinned: Option<&str>,
) -> Result<Vec<Version>, BarcaError> {
    if !Path::new(db_path).exists() {
        return Ok(Vec::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT run_hash, run_id, created_at, elapsed_seconds, artifact_size_bytes, \
             artifact_format, artifact_path, evicted_at FROM materializations \
             WHERE id IN (SELECT max(id) FROM materializations WHERE node_id = ?1 \
             AND status = 'success' AND artifact_path IS NOT NULL AND run_hash IS NOT NULL \
             GROUP BY run_hash) ORDER BY id DESC LIMIT ?2",
            vec![
                libsql::Value::Text(node_id.to_string()),
                libsql::Value::Integer(limit.min(i64::MAX as usize) as i64),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query versions: {e}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read a version: {e}")))?
    {
        let run_hash = row.get::<String>(0).unwrap_or_default();
        out.push(Version {
            pinned: pinned == Some(run_hash.as_str()),
            current: out.is_empty(),
            run_hash,
            run_id: row.get::<String>(1).ok(),
            materialized_at: rfc3339(&row.get::<String>(2).unwrap_or_default()),
            elapsed_seconds: row.get::<f64>(3).ok(),
            size_bytes: row.get::<i64>(4).ok(),
            format: row.get::<String>(5).ok(),
            artifact: row.get::<String>(6).unwrap_or_default(),
            available: row.get::<String>(7).is_err(),
        });
    }
    Ok(out)
}

/// The newest successful materialization of `node_id` under `run_hash` whose artifact this run
/// may read: what a pin at `run_hash` reads.
pub(crate) async fn success_at(
    cache: &CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<OutputRef> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT artifact_path, artifact_format, artifact_size_bytes, output_hash \
             FROM materializations WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' \
             AND artifact_path IS NOT NULL ORDER BY id DESC",
            [node_id.to_string(), run_hash.to_string()],
        )
        .await
        .ok()?;
    while let Ok(Some(row)) = rows.next().await {
        let Ok(path) = row.get::<String>(0) else {
            continue;
        };
        if !cache.allows_artifact(&path) {
            continue;
        }
        return Some(OutputRef {
            path,
            format: row.get::<String>(1).ok()?,
            size_bytes: row.get::<i64>(2).ok().unwrap_or(0) as u64,
            elapsed_seconds: None,
            content_hash: row.get::<String>(3).ok().filter(|h| !h.is_empty()),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        (dir, path)
    }

    async fn success(path: &str, node: &str, hash: &str, evicted: bool) {
        let (_db, conn) = open_conn(path).await.unwrap();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, \
             artifact_size_bytes, elapsed_seconds, status, run_id, evicted_at) \
             VALUES (?1, ?2, ?3, 'json', 7, 0.5, 'success', 'r-' || ?2, ?4)",
            vec![
                libsql::Value::Text(node.into()),
                libsql::Value::Text(hash.into()),
                libsql::Value::Text(format!(".barca/artifacts/{node}/{hash}.json")),
                if evicted {
                    libsql::Value::Text("2026-11-07 00:00:00".into())
                } else {
                    libsql::Value::Null
                },
            ],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_pin_is_recorded_replaced_and_cleared() {
        let (_dir, path) = db().await;
        assert!(get_pins(&path).await.unwrap().is_empty());
        let mut pin = AssetPin {
            node_id: "p.py:a".into(),
            run_hash: "h1".into(),
            pinned_at: "2026-11-07T10:00:00Z".into(),
            pinned_by: Some("me@host".into()),
            reason: None,
        };
        set_pin(&path, &pin).await.unwrap();
        pin.run_hash = "h2".into();
        pin.reason = Some("bad upstream feed".into());
        set_pin(&path, &pin).await.unwrap();
        assert_eq!(get_pins(&path).await.unwrap(), vec![pin.clone()]);
        assert_eq!(clear_pin(&path, "p.py:a").await.unwrap(), Some(pin));
        assert_eq!(clear_pin(&path, "p.py:a").await.unwrap(), None);
        assert!(get_pins(&path).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn versions_are_one_per_run_hash_newest_first() {
        let (_dir, path) = db().await;
        success(&path, "p.py:a", "h1", false).await;
        success(&path, "p.py:a", "h2", true).await;
        success(&path, "p.py:a", "h1", false).await;
        success(&path, "p.py:b", "h9", false).await;
        let versions = get_versions(&path, "p.py:a", 10, Some("h2")).await.unwrap();
        let hashes: Vec<&str> = versions.iter().map(|v| v.run_hash.as_str()).collect();
        assert_eq!(hashes, ["h1", "h2"]);
        assert!(versions[0].current && !versions[0].pinned && versions[0].available);
        assert!(!versions[1].current && versions[1].pinned && !versions[1].available);
        assert!(versions[0].materialized_at.ends_with('Z'));
        assert_eq!(
            get_versions(&path, "p.py:a", 1, None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn a_database_without_the_table_has_no_pins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        assert!(get_pins(&path).await.unwrap().is_empty());
        let (_db, conn) = open_conn(&path).await.unwrap();
        assert!(pins_on(&conn).await.unwrap().is_empty());
    }
}
