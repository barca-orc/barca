//! Last actual writer, independent of the schema format. Connection-local triggers follow
//! every table family's writes and roll back with them; merely reading never changes it.
use barca_model::BarcaError;
use libsql::Connection;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn failed(error: libsql::Error) -> BarcaError {
    BarcaError::Db(format!("metadata writer version: {error}"))
}

async fn exists(conn: &Connection) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='metadata_info'",
            (),
        )
        .await
        .map_err(failed)?;
    Ok(rows.next().await.map_err(failed)?.is_some())
}

pub(super) async fn record(conn: &Connection) -> Result<(), BarcaError> {
    conn.execute("INSERT INTO metadata_info (key, value) VALUES ('last_writer_version', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [VERSION]).await.map_err(failed)?;
    Ok(())
}

pub(in crate::db) async fn install(conn: &Connection) -> Result<(), BarcaError> {
    if !exists(conn).await? {
        return Ok(());
    }
    // Initialization can revisit this connection after adding tables. Ask for each missing
    // event, so an interrupted installation is repaired without reparsing existing triggers.
    let mut rows = conn.query("SELECT t.name, e.event FROM sqlite_master AS t CROSS JOIN (SELECT 'INSERT' AS event UNION ALL SELECT 'UPDATE' UNION ALL SELECT 'DELETE') AS e WHERE t.type='table' AND t.name NOT LIKE 'sqlite_%' AND t.name != 'metadata_info' AND NOT EXISTS (SELECT 1 FROM sqlite_temp_master WHERE type='trigger' AND name = 'barca_writer_' || t.name || '_' || e.event)", ()).await.map_err(failed)?;
    let mut missing = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed)? {
        let name: String = row.get(0).map_err(failed)?;
        if crate::state_validate::TABLES
            .iter()
            .any(|(table, _)| *table == name)
        {
            missing.push((name, row.get::<String>(1).map_err(failed)?));
        }
    }
    // Finish reading both schemas before changing the TEMP schema.
    drop(rows);
    for (name, event) in missing {
        let name = name.replace('"', "\"\"");
        // Keep only one statement alive: execute_batch copies each remaining SQL tail.
        // The conflict predicate avoids rewriting an already-current marker without a
        // separate SELECT in every row trigger. Both insert and update roll back with the row.
        let sql = format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS \"barca_writer_{name}_{event}\" AFTER {event} ON main.\"{name}\" BEGIN INSERT INTO metadata_info (key,value) VALUES ('last_writer_version','{VERSION}') ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE value != excluded.value; END;"
        );
        conn.execute(&sql, ()).await.map_err(failed)?;
    }
    Ok(())
}

pub(super) async fn warn(conn: &Connection) -> Result<(), BarcaError> {
    if !exists(conn).await? {
        return Ok(());
    }
    let mut rows = conn
        .query(
            "SELECT value FROM metadata_info WHERE key='last_writer_version'",
            (),
        )
        .await
        .map_err(failed)?;
    let Some(row) = rows.next().await.map_err(failed)? else {
        return Ok(());
    };
    let writer: String = row.get(0).map_err(failed)?;
    let newer = semver::Version::parse(&writer)
        .ok()
        .zip(semver::Version::parse(VERSION).ok())
        .is_some_and(|(writer, running)| writer.cmp_precedence(&running).is_gt());
    if !newer {
        return Ok(());
    }
    let mut databases = conn
        .query("PRAGMA database_list", ())
        .await
        .map_err(failed)?;
    let path: String = databases
        .next()
        .await
        .map_err(failed)?
        .unwrap()
        .get(2)
        .map_err(failed)?;
    static WARNED: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashSet<(String, String)>>,
    > = std::sync::LazyLock::new(Default::default);
    if WARNED.lock().unwrap().insert((path, writer.clone())) {
        barca_model::errln!(
            "[barca] warning: metadata was last written by barca {writer}; running older barca {VERSION}; upgrade barca to match the writer"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reinstall_repairs_missing_triggers_and_missing_writer_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let path = path.to_str().unwrap();
        crate::db::init_db(path).await.unwrap();
        let _guard = crate::db::db_guard().await;
        let (_handle, conn) = crate::db::open_conn(path).await.unwrap();
        conn.execute("DROP TRIGGER barca_writer_server_state_INSERT", ())
            .await
            .unwrap();
        conn.execute("UPDATE metadata_info SET value='99.0.0'", ())
            .await
            .unwrap();
        install(&conn).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT value FROM metadata_info WHERE key='last_writer_version'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "99.0.0"
        );
        drop(rows);
        conn.execute(
            "INSERT INTO server_state (key,value) VALUES ('test','first')",
            (),
        )
        .await
        .unwrap();
        let mut rows = conn
            .query(
                "SELECT value FROM metadata_info WHERE key='last_writer_version'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            VERSION
        );
        drop(rows);
        // An already-current writer needs no marker write for each row mutation.
        let before = conn.total_changes();
        conn.execute(
            "UPDATE server_state SET value='second' WHERE key='test'",
            (),
        )
        .await
        .unwrap();
        assert_eq!(conn.total_changes() - before, 1);
        conn.execute(
            "DELETE FROM metadata_info WHERE key='last_writer_version'",
            (),
        )
        .await
        .unwrap();
        conn.execute("DELETE FROM server_state WHERE key='test'", ())
            .await
            .unwrap();
        let mut rows = conn
            .query(
                "SELECT value FROM metadata_info WHERE key='last_writer_version'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            VERSION
        );
    }

    #[tokio::test]
    async fn writer_tracks_committed_mutations_not_reads_or_rollbacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let path = path.to_str().unwrap();
        crate::db::init_db(path).await.unwrap();
        let _guard = crate::db::db_guard().await;
        let (_handle, conn) = crate::db::open_conn(path).await.unwrap();
        conn.execute(
            "INSERT INTO server_state (key,value) VALUES ('test','first')",
            (),
        )
        .await
        .unwrap();
        conn.execute("UPDATE metadata_info SET value='99.0.0'", ())
            .await
            .unwrap();
        install(&conn).await.unwrap();
        async fn version(conn: &Connection) -> String {
            let mut rows = conn
                .query(
                    "SELECT value FROM metadata_info WHERE key='last_writer_version'",
                    (),
                )
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get(0).unwrap()
        }
        assert_eq!(version(&conn).await, "99.0.0");
        conn.execute("BEGIN", ()).await.unwrap();
        conn.execute(
            "UPDATE server_state SET value='second' WHERE key='test'",
            (),
        )
        .await
        .unwrap();
        assert_eq!(version(&conn).await, VERSION);
        conn.execute("ROLLBACK", ()).await.unwrap();
        assert_eq!(version(&conn).await, "99.0.0");
        conn.execute("DELETE FROM server_state WHERE key='test'", ())
            .await
            .unwrap();
        assert_eq!(version(&conn).await, VERSION);
    }
}
