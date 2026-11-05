//! Opening a metadata DB for use: initialization under its lock ([`init_db_as`]), the format
//! probe of a read-only server ([`probe_metadata`]), the migration of a legacy or older
//! format-1 schema ([`init_schema`]) and column and index checks of a database read as it is.
//! The table and index definitions are in [`ddl`].

use super::*;

/// Tables, columns, indexes and the format version: what [`init_schema`] creates and
/// the migrations add.
pub(crate) mod ddl;

pub async fn init_db(db_path: &str) -> Result<(), BarcaError> {
    init_db_as(db_path, db_path).await.map(|_| ())
}

/// [`init_db`] on the file at `path`, naming `shown` when the database cannot be used (a
/// snapshot names the database it copies). Returns the format version (`PRAGMA user_version`,
/// [`crate::db::schema::ddl::VERSION`] once initialized).
///
/// An empty file (0 bytes, no write-ahead log) holds no history, so it is initialized as a new
/// database, with a note naming it: a `touch`, a backup tool or a reader that opened the path
/// before barca created the database leaves one (#537). A database this release cannot use is
/// [`BarcaError::MetadataUnusable`]: a file that is not SQLite, a newer format, or a malformed
/// schema. A lock or I/O failure is [`BarcaError::Db`].
pub async fn init_db_as(path: &str, shown: &str) -> Result<i64, BarcaError> {
    let _g = db_guard().await;
    initialized(path, shown)
        .await
        .map(|(handle, conn, version)| {
            // The connection keeps the file open (and its SQLite locks) until it is dropped,
            // so it goes before the handle releases barca's lock (#537).
            drop(conn);
            drop(handle);
            version
        })
        .map_err(|e| named(e, shown))
}

/// The database at `path` opened under its lock and initialized ([`init_db_as`]), with its
/// format version. The caller holds the in-process guard, and drops the connection before the
/// handle.
pub(super) async fn initialized(
    path: &str,
    shown: &str,
) -> Result<(DbHandle, libsql::Connection, i64), BarcaError> {
    // Under the lock: a database another process is creating is empty until it commits.
    let lock = acquire_file_lock(path, DB_FILE_LOCK_WAIT).await?;
    if empty_file(path) {
        // Replaced rather than opened in place: nothing in it, and a new file is what a
        // missing database gets.
        fs::remove_file(path)?;
        // A snapshot's copy is initialized silently: the note is for the file itself.
        if path == shown {
            crate::errln!(
                "[barca] note: the metadata DB {shown} was empty (0 bytes); initialized it \
                 as a new database"
            );
        }
    } else if let Some(why) = unusable_file(path) {
        return Err(unusable(why));
    }
    if Path::new(path).exists() {
        // A read-write SQLite connection may checkpoint on close, even after a failed
        // schema check. Judge compatibility read-only before opening the writer.
        let (_db, read) = connect_read_only(path).await?;
        let version = crate::db::schema::ddl::check(&read).await?;
        if version == crate::db::schema::ddl::VERSION {
            crate::state_validate::Schema::read(&read)
                .await?
                .columns_fit(true)
                .map_err(unusable)?;
        } else {
            crate::state_validate::legacy_schema(&read)
                .await?
                .map_err(unusable)?;
        }
    }
    let (db, conn) = connect_write(path).await?;
    let handle = DbHandle {
        _db: db,
        _lock: Some(lock),
    };
    let version = match init_schema(&conn).await {
        Ok(()) => crate::db::schema::ddl::read_version(&conn).await,
        Err(e) => Err(e),
    };
    match version {
        Ok(version) => {
            // Preserve the existing WAL metadata format for newly created databases too.
            let wal = async {
                let mut rows = conn.query("PRAGMA journal_mode=WAL", ()).await?;
                while rows.next().await?.is_some() {}
                Ok::<_, libsql::Error>(())
            }
            .await;
            if let Err(error) = wal {
                drop(conn);
                drop(handle);
                return Err(BarcaError::Db(format!(
                    "failed to enable metadata WAL: {error}"
                )));
            }
            Ok((handle, conn, version))
        }
        Err(e) => {
            drop(conn);
            drop(handle);
            Err(e)
        }
    }
}

/// The format version of the metadata DB at `path`, read in place when compatible, otherwise
/// from a private snapshot without writing the file: `None` when there is no database yet. A database this release cannot use
/// is [`BarcaError::MetadataUnusable`] naming `path`. For a read-only server, which never
/// migrates the file itself.
pub async fn probe_metadata(path: &str) -> Result<Option<i64>, BarcaError> {
    directory_check(path)?;
    if ReadView::open(path).await?.is_some() {
        return Ok(Some(crate::db::schema::ddl::VERSION));
    }
    let Some(snapshot) = DbSnapshot::take(path).await? else {
        return Ok(None);
    };
    // The copy is private, so it can be judged without a lock. An empty file holds no history:
    // the same as none, which a writer initializes ([`init_db_as`]).
    if empty_file(snapshot.path()) {
        crate::errln!("[barca] note: the metadata DB {path} is empty (0 bytes): no history yet");
        return Ok(None);
    }
    if let Some(why) = unusable_file(snapshot.path()) {
        return Err(BarcaError::MetadataUnusable {
            path: path.to_string(),
            why,
        });
    }
    // The format the file is in, before the copy is migrated.
    let version = {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(snapshot.path())
            .await
            .map_err(|e| named(e, path))?;
        crate::db::schema::ddl::read_version(&conn)
            .await
            .map_err(|e| named(e, path))?
    };
    init_db_as(snapshot.path(), path).await?;
    Ok(Some(version))
}

/// Migrate legacy metadata atomically, preserving authoritative history. Unknown versions
/// are refused before any write; complete current databases are left unchanged.
///
/// Format 1 is additive: a format-1 database written by an earlier release may lack a
/// nullable column, a table or an index added since (`materializations.bytes_changed` in
/// 0.24.0; see `ddl::ADDED_TABLES` and `ddl::INDEXES`). It gets the same additive
/// migration as a legacy database, in one transaction, and an older release still reads the
/// result.
pub(crate) async fn init_schema(conn: &libsql::Connection) -> Result<(), BarcaError> {
    let version = crate::db::schema::ddl::check(conn).await?;
    if version == crate::db::schema::ddl::VERSION {
        // Read once and judged twice: this runs at the start of every run.
        let schema = crate::state_validate::Schema::read(conn).await?;
        if crate::db::schema::ddl::additions_missing_in(&schema) {
            // A table that is there but lacks a column the additions do not bring is malformed,
            // not behind: refuse it before any write.
            schema.columns_fit(true).map_err(unusable)?;
            return add_to_current_schema(conn).await;
        }
        return schema.columns_fit(false).map_err(unusable);
    }
    crate::state_validate::legacy_schema(conn)
        .await?
        .map_err(unusable)?;
    with_tx(conn, "BEGIN IMMEDIATE", "schema migration", async {
        migrate_legacy_schema(conn).await?;
        crate::state_validate::usable_schema(conn)
            .await?
            .map_err(unusable)?;
        conn.execute(
            &format!("PRAGMA user_version = {}", crate::db::schema::ddl::VERSION),
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record schema version: {e}")))?;
        Ok(())
    })
    .await
}

/// Add this release's additive columns and indexes to a version-1 database written by an
/// earlier release, in one transaction: rows are untouched, and an error leaves the schema as it
/// was. The format stays version 1 (see `specs/interfaces/metadata-schema.md`).
async fn add_to_current_schema(conn: &libsql::Connection) -> Result<(), BarcaError> {
    with_tx(conn, "BEGIN IMMEDIATE", "schema migration", async {
        for (table, definitions) in crate::db::schema::ddl::ADDED_COLUMNS {
            crate::db::schema::ddl::add_columns(conn, table, definitions).await?;
        }
        crate::db::schema::ddl::add_tables(conn).await?;
        for index in crate::db::schema::ddl::INDEXES {
            conn.execute(index, ())
                .await
                .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;
        }
        crate::state_validate::usable_schema(conn)
            .await?
            .map_err(unusable)?;
        Ok(())
    })
    .await
}

async fn migrate_legacy_schema(conn: &libsql::Connection) -> Result<(), BarcaError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS materializations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id TEXT NOT NULL,
            run_hash TEXT,
            output_json TEXT,
            artifact_path TEXT,
            artifact_format TEXT,
            artifact_size_bytes INTEGER,
            elapsed_seconds REAL,
            status TEXT NOT NULL DEFAULT 'success',
            error_message TEXT,
            error_traceback TEXT,
            attempts INTEGER DEFAULT 1,
            sinks_json TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create materializations table: {e}")))?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_mat_node_run ON materializations(node_id, run_hash)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;

    // Migrate existing databases: add artifact columns if missing.
    crate::db::schema::ddl::add_columns(
        conn,
        "materializations",
        crate::db::schema::ddl::MATERIALIZATION_COLUMNS,
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_mat_run ON materializations(run_id)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;

    // Per-node cost estimates: the persisted EWMA that seeds the next run's
    // batch sizing, so the 30s cold-start probe is paid once ever per stable
    // node, not once per run. One row per exact node id (including partition
    // suffix) — current estimate only, no history vector.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cost_estimates (
            node_id TEXT PRIMARY KEY,
            base_id TEXT NOT NULL,
            estimate_seconds REAL NOT NULL,
            cpu_seconds REAL,
            max_rss_bytes INTEGER,
            samples INTEGER DEFAULT 0,
            updated_at TEXT DEFAULT (datetime('now'))
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create cost_estimates table: {e}")))?;

    // Run history table.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT UNIQUE NOT NULL,
            command TEXT NOT NULL,
            files TEXT NOT NULL,
            target TEXT,
            status TEXT NOT NULL DEFAULT 'running',
            steps_total INTEGER,
            steps_executed INTEGER DEFAULT 0,
            steps_cached INTEGER DEFAULT 0,
            started_at TEXT DEFAULT (datetime('now')),
            finished_at TEXT,
            elapsed_seconds REAL
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create runs table: {e}")))?;
    // Who is executing a `running` run (#214): the coordinator's pid and host, so a run whose
    // process died without recording an outcome can be told from one still in progress.
    crate::db::schema::ddl::add_columns(conn, "runs", crate::db::schema::ddl::RUN_COLUMNS).await?;
    // Indexes and tables added within format 1 (`ddl::INDEXES`, `ADDED_TABLES`).
    for index in crate::db::schema::ddl::INDEXES {
        conn.execute(index, ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;
    }
    crate::db::schema::ddl::add_tables(conn).await?;

    // Unfinished runs are looked up by status (`interrupted_runs`): through this index that
    // costs the number of such runs, not the length of the history.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create index: {e}")))?;

    // Scheduler durability: the last time each scheduled node was fired, as
    // unix epoch seconds. Lets `barca serve` catch up a single missed tick
    // after downtime instead of silently skipping it.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schedule_state (
            node_id TEXT PRIMARY KEY,
            last_fired_at INTEGER NOT NULL
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create schedule_state table: {e}")))?;
    crate::db::schema::ddl::add_columns(
        conn,
        "schedule_state",
        crate::db::schema::ddl::SCHEDULE_STATE_COLUMNS,
    )
    .await?;

    // Captured user stdout, one row per line. Rust owns persistence; workers
    // stream lines over the socket and the coordinator writes them here.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            line TEXT NOT NULL,
            created_at TEXT DEFAULT (datetime('now'))
        )",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create logs table: {e}")))?;
    crate::db::schema::ddl::add_columns(conn, "logs", crate::db::schema::ddl::LOG_COLUMNS).await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_logs_run ON logs(run_id, seq)",
        (),
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to create logs index: {e}")))?;
    Ok(())
}

/// Whether `table` has `column` (an inspected snapshot may predate it).
/// Whether the database has the index `name` (a snapshot of an older database may not).
pub(super) async fn has_index(conn: &libsql::Connection, name: &str) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1",
            [name],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect indexes: {e}")))?;
    Ok(rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect indexes: {e}")))?
        .is_some())
}

pub(super) async fn has_column(
    conn: &libsql::Connection,
    table: &str,
    column: &str,
) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect {table} columns: {e}")))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read {table} columns: {e}")))?
    {
        if row.get::<String>(1).ok().as_deref() == Some(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Create barca's tables in a new database at `db_path`, for a caller that holds its lock.
pub(crate) async fn init_locked(db_path: &str) -> Result<(), BarcaError> {
    let (_db, conn) = connect(db_path).await?;
    init_schema(&conn).await
}

#[cfg(test)]
mod tests;
