//! Version discipline for persistent history. Checking never migrates or resets data.

use crate::BarcaError;
use libsql::Connection;

pub(crate) const VERSION: i64 = 1;

pub(crate) const RUN_COLUMNS: &[&str] = &[
    "pid INTEGER",
    "host TEXT",
    // What the coordinator knew about itself when it started the run (#290, JSON, see
    // `run_owner::Identity`): which kernel and pid namespace it was in, when it started,
    // and the marker it holds. It lets a reader tell a dead process from a live one
    // where pid and host cannot, in a container. NULL on rows from older versions.
    "owner TEXT",
    // The canonical resolved job ids of a run (`RunReport.job`), set when it starts and again
    // with its outcome. How a notification integration finds the job's previous run, and how
    // `barca serve` sees which nodes a running CLI run covers (#479). NULL on rows from older
    // versions and on runs recorded before they started.
    "job TEXT",
    // What started the run (`manual`, `api`, `schedule`, `client`), and for a scheduled run
    // the tick and the schedule (#393). NULL on rows from older versions.
    "triggered_by TEXT",
    "scheduled_at TEXT",
    "schedule_id TEXT",
    // A run `barca serve` started: its full result, so `GET /runs/{id}` answers after a
    // restart (#393). NULL for every other run.
    "outcome_json TEXT",
    // On a server: the run id a laptop gave the run it posted to the ledger
    // (`triggered_by: client`, RFC-0007 section 4.1). A repeated post of the same run is
    // recognised by it. NULL for every other run.
    "client_run_id TEXT",
];

/// A schedule held back by `POST /schedule/{id}/pause` (#435): since when and until when
/// (unix seconds; `paused_until` NULL means until it is resumed). NULL when not paused.
pub(crate) const SCHEDULE_STATE_COLUMNS: &[&str] = &["paused_at INTEGER", "paused_until INTEGER"];

/// Which stream a recorded line came from (`stdout` / `stderr`) and when it arrived (RFC 3339,
/// UTC, milliseconds). NULL on lines recorded by older versions: stdout, time unknown.
pub(crate) const LOG_COLUMNS: &[&str] = &["stream TEXT", "ts TEXT"];

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
    // A sensor observation revision, independent of its artifact bytes.
    "observation_hash TEXT",
    // Whether this recompute differed from the last digest for the same node/run hash.
    "bytes_changed INTEGER DEFAULT 0",
    // The run that wrote the row (#214). Steps are recorded as they finish and again in the
    // end-of-run ledger (and its replay after a shared-state conflict); this is how the
    // later writes recognise what is already there. NULL on rows from older versions.
    "run_id TEXT",
    "error_type TEXT",
    // The cache size limit (RFC-0012). When this machine deleted the artifact file to stay under
    // `[cache] max_size` (NULL while the file is kept), and when a step, `barca get` or
    // `barca sql` last read the result. Both describe this machine's disk: they are machine-local
    // and no other machine's history is required to have them (not in `state_validate::STEP_COLUMNS`).
    "evicted_at TEXT",
    "last_read_at TEXT",
    // A short digest of each component of the run hash, as a JSON object (#558, 0.29.0): what
    // `barca status` and `--dry-run` compare to say what changed. NULL on older rows, failed
    // rows and rows a server's ledger received from a client.
    "components TEXT",
    // The batched call (RFC-0013, 0.32.0) that computed the key: the keys of one call share it.
    // NULL for a step that ran on its own.
    "batch_id TEXT",
    // Where a failed step failed (#664): `load_inputs`, `user_code`, `split_output`,
    // `write_output` or `sink` (`dispatch::ErrorPhase`). NULL on success rows, on failures its
    // worker did not report one for (it died) and on rows from older versions.
    "error_phase TEXT",
];

/// Every table's columns added within format 1, by table. The one list that the pre-migration
/// check ([`crate::state_validate::Schema::columns_fit`]), [`additions_missing_in`] and the
/// additive migration all read, so a column added to one of the lists above is accepted as
/// missing, detected and added together (#629: `logs` was left out of the check, so a database
/// from before 0.29.2 was refused before its `stream` and `ts` columns could be added).
pub(crate) const ADDED_COLUMNS: &[(&str, &[&str])] = &[
    ("materializations", MATERIALIZATION_COLUMNS),
    ("runs", RUN_COLUMNS),
    ("schedule_state", SCHEDULE_STATE_COLUMNS),
    ("logs", LOG_COLUMNS),
];

/// The columns of `table` added within format 1 (none for a table not in [`ADDED_COLUMNS`]).
pub(crate) fn added_columns(table: &str) -> &'static [&'static str] {
    ADDED_COLUMNS
        .iter()
        .find(|(name, _)| *name == table)
        .map_or(&[], |(_, columns)| columns)
}

/// Columns of `materializations` that describe this machine's disk, not the result: shared
/// history is not required to have them, so `state_validate::STEP_COLUMNS` leaves them out.
#[cfg(test)]
pub(crate) const LOCAL_MATERIALIZATION_COLUMNS: &[&str] = &["evicted_at", "last_read_at"];

/// Indexes added after version 1 was introduced; created with the additive columns.
pub(crate) const INDEXES: &[&str] = &[
    // A sensor reads only its latest successful observation, across definition changes.
    "CREATE INDEX IF NOT EXISTS idx_mat_node_latest ON materializations(node_id, id DESC) WHERE status = 'success' AND artifact_path IS NOT NULL",
    // Eviction and `last_read_at` address results by artifact path.
    "CREATE INDEX IF NOT EXISTS idx_mat_path ON materializations(artifact_path)",
    // A notification integration finds a job's previous finished run through this index.
    "CREATE INDEX IF NOT EXISTS idx_runs_job ON runs(job, id)",
    // `barca status` finds a node's last change in bytes for the same run hash through this
    // index: only the rows whose bytes changed are in it, so it costs nothing to keep.
    "CREATE INDEX IF NOT EXISTS idx_mat_changed ON materializations(node_id) WHERE bytes_changed = 1",
    // `barca history` sums a run's attempts from `idx_mat_run`'s row count and the rows in
    // this index, the few whose attempts are not 1, instead of reading every row of the run.
    "CREATE INDEX IF NOT EXISTS idx_mat_attempts ON materializations(run_id, attempts) WHERE attempts IS NULL OR attempts != 1",
    // Every dry run asks which results the cache size limit deleted: the rows in this index,
    // not a scan of the table.
    "CREATE INDEX IF NOT EXISTS idx_mat_evicted ON materializations(artifact_path) WHERE evicted_at IS NOT NULL",
];

/// Tables added after version 1 was introduced: (name, its `CREATE TABLE IF NOT EXISTS`).
/// Created by `init_db`, and by the additive migration in a version-1 database that lacks one.
pub(crate) const ADDED_TABLES: &[(&str, &str)] = &[
    // Derived per-node freshness witnesses; never a second ledger.
    ("freshness_proofs", crate::incremental::CREATE),
    // Results whose upload failed after their step succeeded (#390).
    ("pending_uploads", crate::pending_uploads::CREATE),
    // The last post a notification integration (Slack) made about a job, so a job that keeps
    // failing is announced once and reminded once a day, across processes and restarts.
    // `message_ref` is reserved for an integration whose posts have ids (empty for a webhook).
    (
        "notifications",
        "CREATE TABLE IF NOT EXISTS notifications (
            job TEXT NOT NULL,
            integration TEXT NOT NULL,
            posted_at TEXT NOT NULL,
            state TEXT NOT NULL,
            message_ref TEXT,
            PRIMARY KEY (job, integration)
        )",
    ),
    // Consecutive failed ticks per scheduled node (RFC-0008 rule 9): how many, which nodes
    // failed in the last one, and the highest materialization id when it was counted. A
    // success recorded after `mark` for one of those nodes, for one of their direct inputs or
    // for the scheduled node itself (an explicit request, or an input change) resets it.
    (
        "schedule_failures",
        "CREATE TABLE IF NOT EXISTS schedule_failures (
            node_id TEXT PRIMARY KEY,
            failures INTEGER NOT NULL,
            failed_nodes TEXT NOT NULL,
            mark INTEGER NOT NULL
        )",
    ),
    // The run queue of `barca serve` (0.29.4): every run it accepted and has not finished, in
    // order, with the request that describes it, so a restart resumes them.
    ("run_queue", crate::run_queue::CREATE_QUEUE),
    // Settings `barca serve` keeps across restarts: whether its run queue is paused.
    ("server_state", crate::run_queue::CREATE_SERVER_STATE),
    // The run hash each `@sink` of a step last published (#662, 0.34.0), so a cache hit
    // under another run hash republishes its sinks from the cached artifact.
    ("sink_publications", crate::sink_publications::CREATE),
];

/// Create every table of [`ADDED_TABLES`] that is missing.
pub(crate) async fn add_tables(conn: &Connection) -> Result<(), BarcaError> {
    for (name, create) in ADDED_TABLES {
        conn.execute(create, ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to create the {name} table: {e}")))?;
    }
    Ok(())
}

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

/// The format version, or [`BarcaError::MetadataUnusable`] (naming no path) for one this
/// release cannot read.
pub(crate) async fn check(conn: &Connection) -> Result<i64, BarcaError> {
    compatible_version(read_version(conn).await?).map_err(crate::db::unusable)
}

/// Whether a version-1 database lacks an additive column, table or index of this release, so that
/// [`crate::db::init_schema`] adds them (in one transaction) before using it. Read-only.
#[cfg(test)]
pub(crate) async fn additions_missing(conn: &Connection) -> Result<bool, BarcaError> {
    Ok(additions_missing_in(
        &crate::state_validate::Schema::read(conn).await?,
    ))
}

/// [`additions_missing`], judged from a schema already read.
pub(crate) fn additions_missing_in(schema: &crate::state_validate::Schema) -> bool {
    for (table, definitions) in ADDED_COLUMNS {
        let missing = definitions.iter().any(|definition| {
            let name = definition.split_whitespace().next().unwrap_or_default();
            !schema.columns(table).any(|column| column == name)
        });
        if missing {
            return true;
        }
    }
    let tables = ADDED_TABLES.iter().map(|(name, _)| ("table", *name));
    let indexes = INDEXES.iter().map(|index| {
        let name = index
            .split_whitespace()
            .nth(5)
            .expect("CREATE INDEX IF NOT EXISTS <name> ...");
        ("index", name)
    });
    tables
        .chain(indexes)
        .any(|(kind, name)| !schema.has(kind, name))
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

    async fn raw(path: &str) -> (libsql::Database, Connection) {
        let db = libsql::Builder::new_local(path).build().await.unwrap();
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
        conn.execute(
            "INSERT INTO schedule_state (node_id,last_fired_at) VALUES ('p.py:a',123)",
            (),
        )
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

    /// A format-1 database written by 0.21-0.23, before `bytes_changed` existed, opens: it gains
    /// the column in place and keeps its rows. 0.24.0-0.24.2 refused it.
    #[tokio::test]
    async fn a_format_1_database_from_before_bytes_changed_gains_it_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("format1.db").display().to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            // An index on the column came later still.
            conn.execute("DROP INDEX idx_mat_changed", ())
                .await
                .unwrap();
            conn.execute("ALTER TABLE materializations DROP COLUMN bytes_changed", ())
                .await
                .unwrap();
            assert_eq!(scalar(&conn, "PRAGMA user_version").await, VERSION);
            db::checkpoint(&conn).await.unwrap();
        }
        db::init_db(&path).await.unwrap();
        db::init_db(&path).await.unwrap();
        assert_eq!(
            db::get_run(&path, "saved").await.unwrap().unwrap().status,
            "success"
        );
        let (_db, conn) = raw(&path).await;
        assert_eq!(scalar(&conn, "PRAGMA user_version").await, VERSION);
        assert_eq!(
            scalar(
                &conn,
                "SELECT COUNT(*) FROM pragma_table_info('materializations') WHERE name = 'bytes_changed'"
            )
            .await,
            1
        );
    }

    /// Every table, column and index of a fresh database, to compare schemas.
    async fn schema_of(path: &str) -> Vec<String> {
        let (_db, conn) = raw(path).await;
        let mut out = Vec::new();
        let mut rows = conn
            .query(
                "SELECT type, name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
                (),
            )
            .await
            .unwrap();
        let mut tables = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let (kind, name) = (row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap());
            if kind == "table" {
                tables.push(name.clone());
            }
            out.push(format!("{kind} {name}"));
        }
        drop(rows);
        for table in tables {
            let mut rows = conn
                .query(&format!("PRAGMA table_info({table})"), ())
                .await
                .unwrap();
            while let Some(row) = rows.next().await.unwrap() {
                out.push(format!("column {table}.{}", row.get::<String>(1).unwrap()));
            }
        }
        out.sort();
        out
    }

    /// A format-1 database last written by 0.24.3 lacks everything format 1 gained since: the
    /// cache size limit's columns and index (0.25.0), and `runs.job`, `idx_runs_job`,
    /// `notifications` and `pending_uploads` (0.26.0), `schedule_failures` (0.28.0), and the
    /// partial indexes `idx_mat_changed`, `idx_mat_attempts` and `idx_mat_evicted` (0.29.3). Opening it adds all of
    /// them in place,
    /// keeps its rows, and leaves exactly the schema a new database gets.
    #[tokio::test]
    async fn a_format_1_database_from_0_24_3_gains_everything_added_since() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.db").display().to_string();
        db::init_db(&fresh).await.unwrap();
        let path = dir.path().join("format1.db").display().to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            for statement in [
                "DROP TABLE pending_uploads",
                "DROP TABLE notifications",
                "DROP TABLE schedule_failures",
                "DROP TABLE sink_publications",
                "DROP INDEX idx_runs_job",
                "DROP INDEX idx_mat_path",
                "DROP INDEX idx_mat_changed",
                "DROP INDEX idx_mat_attempts",
                "DROP INDEX idx_mat_evicted",
                "ALTER TABLE runs DROP COLUMN job",
                "ALTER TABLE materializations DROP COLUMN evicted_at",
                "ALTER TABLE materializations DROP COLUMN last_read_at",
            ] {
                conn.execute(statement, ()).await.unwrap();
            }
            assert_eq!(scalar(&conn, "PRAGMA user_version").await, VERSION);
            assert!(additions_missing(&conn).await.unwrap());
            db::checkpoint(&conn).await.unwrap();
        }
        db::init_db(&path).await.unwrap();
        db::init_db(&path).await.unwrap();
        assert_eq!(schema_of(&path).await, schema_of(&fresh).await);
        assert_eq!(
            db::get_run(&path, "saved").await.unwrap().unwrap().status,
            "success"
        );
        let (_db, conn) = raw(&path).await;
        assert_eq!(scalar(&conn, "PRAGMA user_version").await, VERSION);
        assert!(!additions_missing(&conn).await.unwrap());
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM runs WHERE job IS NULL").await,
            1
        );
        assert_eq!(scalar(&conn, "SELECT COUNT(*) FROM notifications").await, 0);
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM pending_uploads").await,
            0
        );
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM schedule_failures").await,
            0
        );
    }

    /// A format-1 database written before 0.29.2 has a `logs` table without `stream` and `ts`
    /// (#629). Writers, the read-only probe and the backup check open it: it gains the columns in
    /// place, and its lines read as stdout with no time. 0.29.2-0.30.0 refused it.
    #[tokio::test]
    async fn a_format_1_database_from_before_log_streams_gains_them_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.db").display().to_string();
        db::init_db(&fresh).await.unwrap();
        let path = dir.path().join("format1.db").display().to_string();
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            for statement in [
                "ALTER TABLE logs DROP COLUMN stream",
                "ALTER TABLE logs DROP COLUMN ts",
            ] {
                conn.execute(statement, ()).await.unwrap();
            }
            assert!(additions_missing(&conn).await.unwrap());
            db::checkpoint(&conn).await.unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert_eq!(db::probe_metadata(&path).await.unwrap(), Some(VERSION));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the probe writes nothing"
        );
        let logs = db::get_logs(&path, "saved").await.unwrap();
        assert_eq!(logs[0].line, "keep me");
        assert_eq!(logs[0].stream, crate::LogStream::Stdout);
        db::init_db(&path).await.unwrap();
        db::init_db(&path).await.unwrap();
        assert_eq!(schema_of(&path).await, schema_of(&fresh).await);
        let logs = db::get_logs(&path, "saved").await.unwrap();
        assert_eq!(
            (logs[0].line.as_str(), logs[0].stream, logs[0].ts.clone()),
            ("keep me", crate::LogStream::Stdout, None)
        );
        let (_db, conn) = raw(&path).await;
        assert!(!additions_missing(&conn).await.unwrap());
        assert_eq!(
            scalar(&conn, "SELECT COUNT(*) FROM logs WHERE stream IS NULL").await,
            1
        );
    }

    #[tokio::test]
    async fn a_format_1_database_from_before_an_added_column_gets_it_and_keeps_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("format1.db").display().to_string();
        populated(&path, "saved").await;
        {
            // As a 0.24 release left it: format 1 without the columns added since.
            let (_db, conn) = raw(&path).await;
            for (table, name) in [
                ("runs", "triggered_by"),
                ("runs", "scheduled_at"),
                ("runs", "schedule_id"),
                ("runs", "outcome_json"),
                ("schedule_state", "paused_at"),
                ("schedule_state", "paused_until"),
            ] {
                conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {name}"), ())
                    .await
                    .unwrap();
            }
            assert!(additions_missing(&conn).await.unwrap());
        }
        db::init_db(&path).await.unwrap();
        db::init_db(&path).await.unwrap();
        let (_db, conn) = raw(&path).await;
        assert!(!additions_missing(&conn).await.unwrap());
        assert_eq!(scalar(&conn, "PRAGMA user_version").await, VERSION);
        let run = db::get_run(&path, "saved").await.unwrap().unwrap();
        assert_eq!(run.status, "success");
        assert_eq!(run.triggered_by, None, "older rows do not say");
        assert_eq!(
            scalar(&conn, "SELECT last_fired_at FROM schedule_state").await,
            123
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
            db::copy_for_backup(
                &path,
                &dir.path().join("backup.db"),
                None,
                crate::helper_proc::Until::done(),
            )
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
    async fn a_backup_in_a_future_format_is_refused_as_incompatible_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let incoming = dir.path().join("incoming.db").display().to_string();
        populated(&incoming, "theirs").await;
        {
            let (_db, conn) = raw(&incoming).await;
            conn.execute("PRAGMA user_version=2", ()).await.unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        let before = std::fs::read(&incoming).unwrap();
        let Err(refused) = state_validate::open(&incoming, state_validate::Pages::Check)
            .await
            .unwrap()
        else {
            panic!("a future format is not valid here");
        };
        assert_eq!(refused.kind, state_validate::InvalidKind::Compatibility);
        assert!(refused.why.contains("schema version 2"), "{}", refused.why);
        assert_eq!(std::fs::read(&incoming).unwrap(), before);
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

    fn unusable(error: BarcaError) -> (String, String) {
        match error {
            BarcaError::MetadataUnusable { path, why } => (path, why),
            e => panic!("not MetadataUnusable: {e:?}"),
        }
    }

    /// A database this release cannot use is `MetadataUnusable` naming it (#513), with the
    /// `metadata_unusable` reason and a remediation in its envelope, and is left as it was.
    #[tokio::test]
    async fn a_database_barca_cannot_use_is_named_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.db").display().to_string();
        std::fs::write(&junk, "not sqlite ".repeat(100)).unwrap();
        let future = dir.path().join("future.db").display().to_string();
        populated(&future, "saved").await;
        {
            let (_db, conn) = raw(&future).await;
            conn.execute("PRAGMA user_version=7", ()).await.unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        let malformed = dir.path().join("malformed.db").display().to_string();
        {
            let (_db, conn) = raw(&malformed).await;
            conn.execute("CREATE TABLE runs (run_id TEXT)", ())
                .await
                .unwrap();
            conn.execute("CREATE TABLE materializations (node_id TEXT)", ())
                .await
                .unwrap();
            conn.execute(&format!("PRAGMA user_version={VERSION}"), ())
                .await
                .unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        for (path, says) in [
            (&junk, "does not start with a SQLite header"),
            (&future, "metadata schema version 7 is not supported"),
            (&malformed, "runs"),
        ] {
            let before = std::fs::read(path).unwrap();
            let error = db::init_db(path).await.unwrap_err();
            let envelope = crate::outcome::envelope_of(error, &Default::default()).to_json();
            assert_eq!(envelope["reason"], "metadata_unusable", "{envelope}");
            assert_eq!(envelope["code"], 3);
            assert!(
                envelope["remediation"]
                    .as_str()
                    .unwrap()
                    .contains("barca state restore")
            );
            let (named, why) = unusable(db::init_db(path).await.unwrap_err());
            assert_eq!(&named, path);
            assert!(why.contains(says), "{path}: {why}");
            // A read-only probe names the same file, and migrates only its private copy.
            let (named, _) = unusable(db::probe_metadata(path).await.unwrap_err());
            assert_eq!(&named, path);
            assert_eq!(&std::fs::read(path).unwrap(), &before, "{path} was changed");
        }
    }

    /// An empty file holds no history: a writer initializes it as a new database, and the
    /// read-only probe reports it as none yet, leaving it as it is (#537).
    #[tokio::test]
    async fn an_empty_file_is_initialized_as_a_new_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(db::probe_metadata(&path).await.unwrap(), None);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the probe writes nothing"
        );
        db::init_db(&path).await.unwrap();
        assert_eq!(db::probe_metadata(&path).await.unwrap(), Some(VERSION));
        assert!(
            !db::empty_file(&path),
            "a database barca creates is never left empty"
        );
        // Once initialized it is an ordinary database: a second open keeps what is in it.
        assert_eq!(db::init_db_as(&path, &path).await.unwrap(), VERSION);
    }

    #[tokio::test]
    async fn the_read_only_probe_reports_the_format_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        assert_eq!(db::probe_metadata(&path).await.unwrap(), None);
        assert!(!std::path::Path::new(&path).exists(), "nothing is created");
        populated(&path, "saved").await;
        {
            let (_db, conn) = raw(&path).await;
            conn.execute("PRAGMA user_version=0", ()).await.unwrap();
            db::checkpoint(&conn).await.unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert_eq!(db::probe_metadata(&path).await.unwrap(), Some(0));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(db::init_db_as(&path, &path).await.unwrap(), VERSION);
        assert_eq!(db::probe_metadata(&path).await.unwrap(), Some(VERSION));
    }

    #[test]
    fn an_error_envelope_without_a_reason_has_no_reason_key() {
        let envelope =
            crate::outcome::envelope_of(BarcaError::Db("locked".into()), &Default::default())
                .to_json();
        assert!(envelope.get("reason").is_none(), "{envelope}");
    }
}
