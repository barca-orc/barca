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
    // `barca status` and `--dry-run` compare to say what changed. Those of the definition are in
    // `definition_components`, named by the `@definition` key; rows from before 0.37.1 hold them
    // here. NULL on older rows, failed rows, rows a server's ledger received from a client and
    // rows `crate::metadata_size` trimmed.
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
    // Status reads one observation per sensor key, independent of its history length.
    "CREATE INDEX IF NOT EXISTS idx_mat_output_latest ON materializations(node_id, id DESC) WHERE status = 'success' AND output_hash IS NOT NULL",
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
    // A run's first failed step, which `GET /runs` and `barca history` show for every run
    // listed: the rows in this index, not every row of the run (115k for a run of a 115k-key
    // node, read again on each poll of the runs list).
    "CREATE INDEX IF NOT EXISTS idx_mat_failed ON materializations(run_id, id) WHERE status = 'failed'",
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
    ("run_queue", crate::serve::run_queue::CREATE_QUEUE),
    // Settings `barca serve` keeps across restarts: whether its run queue is paused.
    ("server_state", crate::serve::run_queue::CREATE_SERVER_STATE),
    // The run hash each `@sink` of a step last published (#662, 0.34.0), so a cache hit
    // under another run hash republishes its sinks from the cached artifact.
    ("sink_publications", crate::sink_publications::CREATE),
    // Assets held at an earlier materialization (`barca pin`, 0.41.0).
    ("asset_pins", crate::db::pins::CREATE),
    // Materializations an operator uploaded to pin an asset to (`barca pin --from-file`).
    ("asset_uploads", crate::db::pins::CREATE_UPLOADS),
    // The nodes an operator paused (`barca pause`): scheduled ticks do not compute them.
    ("asset_pauses", crate::db::CREATE_ASSET_PAUSES),
    // The component digests of a definition, once: the rows of its keys name it (`crate::changes`).
    (
        "definition_components",
        "CREATE TABLE IF NOT EXISTS definition_components (
            definition_hash TEXT PRIMARY KEY,
            components TEXT NOT NULL
        )",
    ),
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
mod tests;
