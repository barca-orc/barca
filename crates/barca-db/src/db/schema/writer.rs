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
    let mut rows = conn.query("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'metadata_info'", ()).await.map_err(failed)?;
    let mut sql = String::new();
    while let Some(row) = rows.next().await.map_err(failed)? {
        let name: String = row.get(0).map_err(failed)?;
        if !crate::state_validate::TABLES
            .iter()
            .any(|(table, _)| *table == name)
        {
            continue;
        }
        let name = name.replace('"', "\"\"");
        for event in ["INSERT", "UPDATE", "DELETE"] {
            sql.push_str(&format!("CREATE TEMP TRIGGER IF NOT EXISTS \"barca_writer_{name}_{event}\" AFTER {event} ON main.\"{name}\" WHEN COALESCE((SELECT value FROM metadata_info WHERE key='last_writer_version'), '') != '{VERSION}' BEGIN INSERT INTO metadata_info (key,value) VALUES ('last_writer_version','{VERSION}') ON CONFLICT(key) DO UPDATE SET value=excluded.value; END;"));
        }
    }
    conn.execute_batch(&sql).await.map_err(failed)?;
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
