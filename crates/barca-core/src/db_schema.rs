//! Version discipline for persistent history. Checking never migrates or resets data.

use crate::BarcaError;
use turso::Connection;

pub(crate) const VERSION: i64 = 1;

pub(crate) const RUN_COLUMNS: &[&str] = &[
    "pid INTEGER",
    "host TEXT",
    // What the coordinator knew about itself when it started the run (#290, JSON, see
    // `run_owner::Identity`): which kernel and pid namespace it was in, when it started,
    // and the marker it holds. It lets a reader tell a dead process from a live one
    // where pid and host cannot, in a container. NULL on rows from older versions.
    "owner TEXT",
];

pub(crate) const MATERIALIZATION_COLUMNS: &[&str] = &[
    "artifact_path TEXT",
    "artifact_format TEXT",
    "artifact_size_bytes INTEGER",
    "elapsed_seconds REAL",
    "error_message TEXT",
    "error_traceback TEXT",
    "attempts INTEGER DEFAULT 1",
    "sinks_json TEXT",
    "cpu_seconds REAL",
    "max_rss_bytes INTEGER",
    // Content hash of a sensor's output (#183): folded into its consumers' run hashes, and
    // what `--dry-run` / `barca status` assume the sensor returns next.
    "output_hash TEXT",
    // The run that wrote the row (#214). Steps are recorded as they finish and again in the
    // end-of-run ledger (and its replay after a shared-state conflict); this is how the
    // later writes recognise what is already there. NULL on rows from older versions.
    "run_id TEXT",
    "error_type TEXT",
];

/// Read SQLite's built-in format marker without mutating the database. Operational
/// failures stay separate from an unsupported version.
pub(crate) async fn read_version(conn: &Connection) -> Result<i64, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read metadata schema version: {e}"));
    let mut rows = conn
        .query("PRAGMA user_version", ())
        .await
        .map_err(failed)?;
    let version = rows
        .next()
        .await
        .map_err(failed)?
        .ok_or_else(|| BarcaError::Db("metadata schema version was not returned".into()))?
        .get::<i64>(0)
        .map_err(failed)?;
    while rows.next().await.map_err(failed)?.is_some() {}
    Ok(version)
}

pub(crate) fn compatible_version(version: i64) -> Result<i64, String> {
    if matches!(version, 0 | VERSION) {
        Ok(version)
    } else {
        Err(format!(
            "metadata schema version {version} is not supported by barca {} \
             (supports version {VERSION} and legacy version 0); use a compatible Barca \
             release. The database was not migrated or reset",
            env!("CARGO_PKG_VERSION")
        ))
    }
}

pub(crate) async fn check(conn: &Connection) -> Result<i64, BarcaError> {
    compatible_version(read_version(conn).await?).map_err(BarcaError::Db)
}

/// Apply only missing additive columns. Existing columns are not a reason to swallow errors.
pub(crate) async fn add_columns(
    conn: &Connection,
    table: &str,
    definitions: &[&str],
) -> Result<(), BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to migrate `{table}`: {e}"));
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .map_err(failed)?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed)? {
        columns.push(row.get::<String>(1).map_err(failed)?);
    }
    drop(rows);
    for definition in definitions {
        let name = definition
            .split_whitespace()
            .next()
            .expect("static column definition");
        if !columns.iter().any(|column| column == name) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {definition}"), ())
                .await
                .map_err(failed)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, state_validate};
    use std::path::Path;

    async fn raw(path: &str) -> (turso::Database, Connection) {
        let db = turso::Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        (db, conn)
    }

    async fn scalar(conn: &Connection, sql: &str) -> i64 {
        let mut rows = conn.query(sql, ()).await.unwrap();
        let value = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert!(rows.next().await.unwrap().is_none());
        value
    }

    async fn populated(path: &str, id: &str) {
        db::init_db(path).await.unwrap();
        let (_db, conn) = raw(path).await;
        conn.execute(
            "INSERT INTO runs (run_id,command,files,status) VALUES (?1,'get','p.py','success')",
            [id],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO logs (run_id,node_id,seq,line) VALUES (?1,'p.py:a',0,'keep me')",
            [id],
        )
        .await
        .unwrap();
        conn.execute("INSERT INTO schedule_state VALUES ('p.py:a',123)", ())
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO materializations (node_id,run_id,output_json) VALUES ('p.py:a',?1,'42')",
            [id],
        )
        .await
        .unwrap();
        db::checkpoint(&conn).await.unwrap();
    }

    #[tokio::test]
    async fn legacy_migration_preserves_history_and_is_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db").display().to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            conn.execute("PRAGMA user_version=0", ()).await.unwrap();
        }
        db::init_db(&path).await.unwrap();
        db::init_db(&path).await.unwrap();
        assert_eq!(
            db::get_run(&path, "saved").await.unwrap().unwrap().status,
            "success"
        );
        assert_eq!(
            db::get_logs(&path, "saved").await.unwrap()[0].line,
            "keep me"
        );
        let (_db, conn) = raw(&path).await;
        assert_eq!(scalar(&conn, "PRAGMA user_version").await, VERSION);
        assert_eq!(
            scalar(&conn, "SELECT last_fired_at FROM schedule_state").await,
            123
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM materializations WHERE output_json='42'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn failed_migration_rolls_back_all_ddl_and_preserves_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken-legacy.db").display().to_string();
        {
            let (_db, conn) = raw(&path).await;
            conn.execute(
                "CREATE TABLE materializations (node_id TEXT,run_hash TEXT)",
                (),
            )
            .await
            .unwrap();
            conn.execute("INSERT INTO materializations VALUES ('saved','hash')", ())
                .await
                .unwrap();
        }
        assert!(db::init_db(&path).await.is_err());
        let (_db, conn) = raw(&path).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM materializations WHERE node_id='saved'"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('runs','idx_mat_node_run')"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM pragma_table_info('materializations')"
            )
            .await,
            2
        );
    }

    #[tokio::test]
    async fn interrupted_additive_migration_can_be_reopened_and_completed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interrupted.db").display().to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            conn.execute("PRAGMA user_version=0", ()).await.unwrap();
            conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
            conn.execute("ALTER TABLE runs ADD COLUMN tentative TEXT", ())
                .await
                .unwrap();
            conn.execute("PRAGMA user_version=1", ()).await.unwrap();
            // Dropping the connection before commit models cancellation of migration.
        }
        {
            let (_db, conn) = raw(&path).await;
            assert_eq!(read_version(&conn).await.unwrap(), 0);
        }
        db::init_db(&path).await.unwrap();
        let (_db, conn) = raw(&path).await;
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM pragma_table_info('runs') WHERE name='tentative'"
            )
            .await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM runs WHERE run_id='saved'").await,
            1
        );
        assert_eq!(check(&conn).await.unwrap(), VERSION);
    }

    #[tokio::test]
    async fn future_versions_are_refused_by_readers_writers_snapshots_and_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.db").display().to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            conn.execute("PRAGMA user_version=2", ()).await.unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let errors = [
            db::init_db(&path).await.unwrap_err(),
            db::get_run(&path, "saved").await.unwrap_err(),
            db::create_run(&path, "new", "get", "p.py", None, None)
                .await
                .unwrap_err(),
            db::CacheReader::open(&path).await.err().unwrap(),
            db::copy_for_push(&path, dir.path().join("upload.db"))
                .await
                .err()
                .unwrap(),
        ];
        for error in errors {
            assert!(error.to_string().contains("schema version 2"), "{error}");
        }
        let snapshot = db::DbSnapshot::take(&path).await.unwrap().unwrap();
        assert!(
            db::get_run(snapshot.path(), "saved")
                .await
                .unwrap_err()
                .to_string()
                .contains("schema version 2")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let (_db, conn) = raw(&path).await;
        assert_eq!(scalar(&conn, "SELECT COUNT(*) FROM runs").await, 1);
    }

    #[tokio::test]
    async fn negative_versions_never_trigger_a_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("negative.db").display().to_string();
        let (_db, conn) = raw(&path).await;
        conn.execute("PRAGMA user_version=-1", ()).await.unwrap();
        let error = db::init_schema(&conn).await.unwrap_err();
        assert!(error.to_string().contains("schema version -1"), "{error}");
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='runs'"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn future_download_and_future_local_history_are_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.db").display().to_string();
        let incoming = dir.path().join("incoming.db").display().to_string();
        populated(&local, "ours").await;
        populated(&incoming, "theirs").await;
        {
            let (_db, conn) = raw(&incoming).await;
            conn.execute("PRAGMA user_version=2", ()).await.unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        let before = std::fs::read(&local).unwrap();
        let downloaded = std::fs::read(&incoming).unwrap();
        let result = db::replace_db(
            &local,
            db::Incoming {
                staged: Path::new(&incoming),
                base_at_start: None,
                version: Some("future"),
            },
        )
        .await
        .unwrap();
        assert!(matches!(result, db::Replaced::Invalid(_)));
        assert_eq!(std::fs::read(&local).unwrap(), before);
        assert_eq!(std::fs::read(&incoming).unwrap(), downloaded);
        // Reverse the roles: a compatible download cannot downgrade future local history.
        let error = db::replace_db(
            &incoming,
            db::Incoming {
                staged: Path::new(&local),
                base_at_start: None,
                version: Some("legacy"),
            },
        )
        .await
        .unwrap_err();
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("schema version 2"), "{diagnostic}");
        assert!(
            diagnostic.contains("use a compatible Barca release"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("Keep the local history"),
            "{diagnostic}"
        );
        assert!(!diagnostic.contains("move "), "{diagnostic}");
        assert!(!diagnostic.contains("out of the way"), "{diagnostic}");
        assert!(!diagnostic.contains("not carried over"), "{diagnostic}");
        assert_eq!(std::fs::read(&incoming).unwrap(), downloaded);
        assert!(
            state_validate::open(&incoming, state_validate::Pages::Check)
                .await
                .unwrap()
                .is_err()
        );
    }
    #[tokio::test]
    async fn compatible_legacy_migration_failure_remains_an_operational_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("legacy-ddl-failure.db")
            .display()
            .to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            conn.execute("PRAGMA user_version=0", ()).await.unwrap();
            conn.execute("DROP INDEX idx_logs_run", ()).await.unwrap();
            // Deterministic DDL failure after compatibility validation, like an I/O
            // failure in that stage: it must not become a damaged-history verdict.
            conn.execute("CREATE TABLE idx_logs_run (v TEXT)", ())
                .await
                .unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        let error = state_validate::open(&path, state_validate::Pages::Check)
            .await
            .expect_err("migration error must be the outer operational error");
        assert!(error.to_string().contains("could not migrate"), "{error}");
        let (_db, conn) = raw(&path).await;
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM runs WHERE run_id='saved'").await,
            1
        );
        assert_eq!(scalar(&conn, "PRAGMA user_version").await, 0);
    }
}
