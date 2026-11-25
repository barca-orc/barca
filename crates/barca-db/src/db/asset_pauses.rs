//! Reads and writes of `asset_pauses`: the assets, sensors and tasks an operator held back
//! (`barca pause`, `POST /pauses`). A paused node is not computed by a scheduled tick until
//! it is resumed (`barca_service::pauses`).
//!
//! A node can be held by several pauses at once: an operator holds one step, then an incident
//! pauses a step above it with `--downstream`. `asset_pauses` keeps one row per paused node
//! (what ticks, requests and older releases read), and `asset_pause_vias` keeps one row per
//! node and pause target. A resume lifts only the pause it names, so the node stays paused
//! while another pause holds it.

use super::*;

/// The table, as `init_db` creates it (`ddl::ADDED_TABLES`).
pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS asset_pauses (
    node_id TEXT PRIMARY KEY,
    paused_at INTEGER NOT NULL,
    via TEXT NOT NULL
)";

/// Every pause of each paused node: one row per node and target (`via`), as `init_db`
/// creates it (`ddl::ADDED_TABLES`). `asset_pauses` holds the earliest of them.
pub(crate) const CREATE_VIAS: &str = "CREATE TABLE IF NOT EXISTS asset_pause_vias (
    node_id TEXT NOT NULL,
    via TEXT NOT NULL,
    paused_at INTEGER NOT NULL,
    PRIMARY KEY (node_id, via)
)";

/// Make `asset_pause_vias` agree with `asset_pauses`. A database paused by a release older than
/// the table, or changed by one since, has paused nodes without a row here (they get their
/// recorded pause) and rows for nodes that release resumed (they go).
const RECONCILE: [&str; 2] = [
    "INSERT OR IGNORE INTO asset_pause_vias (node_id, via, paused_at)
     SELECT node_id, via, paused_at FROM asset_pauses",
    "DELETE FROM asset_pause_vias WHERE node_id NOT IN (SELECT node_id FROM asset_pauses)",
];

/// One paused node: since when (unix seconds) and what was paused to pause it (`via`: the
/// node itself, the node a `--downstream` pause started from, or a file). `paused_at` and
/// `via` are the earliest pause that holds it, and `vias` every one, earliest first.
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
    /// Every pause target that holds the node, earliest first. The node stays paused until a
    /// resume lifts the last of them.
    #[serde(default)]
    pub vias: Vec<String>,
}

async fn has_table(conn: &libsql::Connection, name: &str) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
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
/// file a run is about to initialize, or none at all) has none. A node without rows in
/// `asset_pause_vias` (paused by an older release) is held by its recorded `via` alone.
pub async fn get_asset_pauses(db_path: &str) -> Result<Vec<AssetPause>, BarcaError> {
    if fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Vec::new());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn, "asset_pauses").await? {
        return Ok(Vec::new());
    }
    let mut out = query_pauses(
        &conn,
        "SELECT node_id, paused_at, via FROM asset_pauses ORDER BY node_id",
    )
    .await?;
    if has_table(&conn, "asset_pause_vias").await? {
        let vias = query_pauses(
            &conn,
            "SELECT node_id, paused_at, via FROM asset_pause_vias ORDER BY node_id, paused_at, via",
        )
        .await?;
        for pause in &mut out {
            pause.vias = vias
                .iter()
                .filter(|v| v.id == pause.id)
                .map(|v| v.via.clone())
                .collect();
        }
    }
    for pause in &mut out {
        if pause.vias.is_empty() {
            pause.vias = vec![pause.via.clone()];
        }
    }
    Ok(out)
}

/// The `(node_id, paused_at, via)` rows `sql` selects, `vias` left empty.
async fn query_pauses(conn: &libsql::Connection, sql: &str) -> Result<Vec<AssetPause>, BarcaError> {
    let mut rows = conn
        .query(sql, ())
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
            vias: Vec::new(),
        });
    }
    Ok(out)
}

/// Create both tables if they are missing and make them agree ([`RECONCILE`]).
async fn prepare(conn: &libsql::Connection) -> Result<(), BarcaError> {
    for sql in [CREATE, CREATE_VIAS].into_iter().chain(RECONCILE) {
        conn.execute(sql, ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to prepare asset_pauses: {e}")))?;
    }
    Ok(())
}

/// Pause `ids`, each `via` the given target, at `now`. A node already paused keeps its
/// first pause time and `via` in `asset_pauses`, and gains this pause in `asset_pause_vias`
/// (a repeated pause through the same target keeps its first time).
pub async fn pause_assets(
    db_path: &str,
    ids: &[String],
    via: &str,
    now: i64,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    prepare(&conn).await?;
    for id in ids {
        for sql in [
            "INSERT INTO asset_pauses (node_id, paused_at, via) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO NOTHING",
            "INSERT INTO asset_pause_vias (node_id, paused_at, via) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id, via) DO NOTHING",
        ] {
            conn.execute(sql, [id.clone(), now.to_string(), via.to_string()])
                .await
                .map_err(|e| BarcaError::Db(format!("failed to record a pause: {e}")))?;
        }
    }
    Ok(())
}

/// Resume `ids` (`None`: every paused node), whatever pauses hold them. Resuming a node that
/// is not paused removes no pause. The freshness proofs are forgotten, so the next run decides
/// every node through the cache: it recomputes what changed while the node was held and
/// nothing else.
pub async fn resume_assets(db_path: &str, ids: Option<&[String]>) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn, "asset_pauses").await? {
        return Ok(());
    }
    prepare(&conn).await?;
    forget_proofs_before_resume(&conn).await?;
    let fail = |e: libsql::Error| BarcaError::Db(format!("failed to resume: {e}"));
    match ids {
        None => {
            for sql in ["DELETE FROM asset_pause_vias", "DELETE FROM asset_pauses"] {
                conn.execute(sql, ()).await.map_err(fail)?;
            }
        }
        Some(ids) => {
            for id in ids {
                for sql in [
                    "DELETE FROM asset_pause_vias WHERE node_id = ?1",
                    "DELETE FROM asset_pauses WHERE node_id = ?1",
                ] {
                    conn.execute(sql, [id.clone()]).await.map_err(fail)?;
                }
            }
        }
    }
    Ok(())
}

/// Lift the pauses `lifts` names, each a `(node_id, via)`. A node whose last pause is lifted is
/// resumed (as [`resume_assets`]); a node another pause still holds stays paused, with its
/// earliest remaining pause as its `paused_at` and `via`.
pub async fn lift_pauses(db_path: &str, lifts: &[(String, String)]) -> Result<(), BarcaError> {
    if lifts.is_empty() {
        return Ok(());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_table(&conn, "asset_pauses").await? {
        return Ok(());
    }
    prepare(&conn).await?;
    forget_proofs_before_resume(&conn).await?;
    let fail = |e: libsql::Error| BarcaError::Db(format!("failed to resume: {e}"));
    for (id, via) in lifts {
        conn.execute(
            "DELETE FROM asset_pause_vias WHERE node_id = ?1 AND via = ?2",
            [id.clone(), via.clone()],
        )
        .await
        .map_err(fail)?;
    }
    let mut nodes: Vec<&String> = lifts.iter().map(|(id, _)| id).collect();
    nodes.sort();
    nodes.dedup();
    for id in nodes {
        // The earliest pause left, if any, becomes the node's recorded pause.
        conn.execute(
            "UPDATE asset_pauses SET (paused_at, via) = (
                 SELECT paused_at, via FROM asset_pause_vias WHERE node_id = ?1
                 ORDER BY paused_at, via LIMIT 1)
             WHERE node_id = ?1
               AND EXISTS (SELECT 1 FROM asset_pause_vias WHERE node_id = ?1)",
            [id.clone()],
        )
        .await
        .map_err(fail)?;
        conn.execute(
            "DELETE FROM asset_pauses WHERE node_id = ?1
               AND NOT EXISTS (SELECT 1 FROM asset_pause_vias WHERE node_id = ?1)",
            [id.clone()],
        )
        .await
        .map_err(fail)?;
    }
    Ok(())
}

/// Ticks read a paused node as it is, whatever changed above it. No run records a proof of a
/// node it held (`barca_service::incremental`), but a proof written by an earlier version, or
/// by a run that started before the pause, may say it is fresh: forget them before the pause
/// is lifted, so a crash in between leaves a pause without proofs and never a resumed node
/// with a stale one (#988).
async fn forget_proofs_before_resume(conn: &libsql::Connection) -> Result<(), BarcaError> {
    super::pins::forget_proofs(conn).await
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

    async fn db() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        (dir, path)
    }

    fn lift(id: &str, via: &str) -> (String, String) {
        (id.to_string(), via.to_string())
    }

    /// An operator holds `held`, an incident pauses `root --downstream` over it, and the
    /// incident's resume must leave the operator's hold in place.
    #[tokio::test]
    async fn a_resume_lifts_only_the_pause_it_names() {
        let (_dir, path) = db().await;
        let held = ["p.py:held".to_string()];
        let below_root = ["p.py:root".to_string(), "p.py:held".to_string()];
        pause_assets(&path, &held, "held", 1).await.unwrap();
        pause_assets(&path, &below_root, "root", 2).await.unwrap();
        let paused = get_asset_pauses(&path).await.unwrap();
        let held_row = paused.iter().find(|p| p.id == "p.py:held").unwrap();
        assert_eq!(held_row.vias, ["held", "root"]);

        lift_pauses(
            &path,
            &[lift("p.py:root", "root"), lift("p.py:held", "root")],
        )
        .await
        .unwrap();
        let paused = get_asset_pauses(&path).await.unwrap();
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].id, "p.py:held");
        assert_eq!((paused[0].paused_at, paused[0].via.as_str()), (1, "held"));
        assert_eq!(paused[0].vias, ["held"]);
    }

    /// When the earliest pause goes, the node keeps the next one as its recorded pause.
    #[tokio::test]
    async fn the_earliest_remaining_pause_becomes_the_recorded_one() {
        let (_dir, path) = db().await;
        let ids = ["p.py:a".to_string()];
        pause_assets(&path, &ids, "first", 1).await.unwrap();
        pause_assets(&path, &ids, "second", 2).await.unwrap();
        pause_assets(&path, &ids, "first", 3).await.unwrap();
        lift_pauses(&path, &[lift("p.py:a", "first")])
            .await
            .unwrap();
        let paused = get_asset_pauses(&path).await.unwrap();
        assert_eq!((paused[0].paused_at, paused[0].via.as_str()), (2, "second"));
        lift_pauses(&path, &[lift("p.py:a", "second")])
            .await
            .unwrap();
        assert!(get_asset_pauses(&path).await.unwrap().is_empty());
    }

    /// A node paused before `asset_pause_vias` existed is held by its recorded pause alone.
    #[tokio::test]
    async fn a_pause_from_an_older_release_is_held_by_its_recorded_via() {
        let (_dir, path) = db().await;
        {
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute("DROP TABLE asset_pause_vias", ())
                .await
                .unwrap();
            conn.execute(
                "INSERT INTO asset_pauses (node_id, paused_at, via) VALUES ('p.py:a', 1, 'a')",
                (),
            )
            .await
            .unwrap();
        }
        let paused = get_asset_pauses(&path).await.unwrap();
        assert_eq!(paused[0].vias, ["a"]);
        pause_assets(&path, &["p.py:a".to_string()], "root", 2)
            .await
            .unwrap();
        lift_pauses(&path, &[lift("p.py:a", "root")]).await.unwrap();
        let paused = get_asset_pauses(&path).await.unwrap();
        assert_eq!(paused[0].vias, ["a"]);
        lift_pauses(&path, &[lift("p.py:a", "a")]).await.unwrap();
        assert!(get_asset_pauses(&path).await.unwrap().is_empty());
    }

    /// A node that an older release resumed (its `asset_pauses` row gone) loses the pauses this
    /// release recorded for it, so a later pause starts from nothing.
    #[tokio::test]
    async fn a_resume_by_an_older_release_drops_the_other_pauses() {
        let (_dir, path) = db().await;
        let ids = ["p.py:a".to_string()];
        pause_assets(&path, &ids, "first", 1).await.unwrap();
        pause_assets(&path, &ids, "second", 2).await.unwrap();
        {
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute("DELETE FROM asset_pauses", ()).await.unwrap();
        }
        pause_assets(&path, &ids, "third", 3).await.unwrap();
        let paused = get_asset_pauses(&path).await.unwrap();
        assert_eq!(paused[0].vias, ["third"]);
    }

    /// `resume --all` and a resume of named nodes lift every pause that holds them.
    #[tokio::test]
    async fn resume_assets_lifts_every_pause_of_its_nodes() {
        let (_dir, path) = db().await;
        let ids = ["p.py:a".to_string(), "p.py:b".to_string()];
        pause_assets(&path, &ids, "first", 1).await.unwrap();
        pause_assets(&path, &ids, "second", 2).await.unwrap();
        resume_assets(&path, Some(&ids[..1])).await.unwrap();
        let paused = get_asset_pauses(&path).await.unwrap();
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].vias, ["first", "second"]);
        resume_assets(&path, None).await.unwrap();
        assert!(get_asset_pauses(&path).await.unwrap().is_empty());
        pause_assets(&path, &ids[..1], "third", 3).await.unwrap();
        assert_eq!(get_asset_pauses(&path).await.unwrap()[0].vias, ["third"]);
    }
}
