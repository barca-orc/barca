//! What a downloaded copy of the shared state must be before it may replace the local
//! database (RFC-0006 §4.1, #243).
//!
//! The shared state is one object that every machine downloads and puts in the place of its
//! own history. An object that is not a database barca can use (cut short in transit,
//! overwritten with something else, damaged in the store, another program's file) must stop
//! there: the command fails and the local database stays as it was.
//!
//! A download is **valid** when all of these hold. They are checked in this order, on the
//! downloaded file itself and before anything is written to it:
//!
//! 1. **It is a whole SQLite file** ([`whole_file`]): not empty, starts with the SQLite header,
//!    and its length is a whole number of pages. barca never uploads an empty file (a push
//!    folds the write-ahead log into the main file first), so an empty object is not "no
//!    history yet"; that is the object being absent.
//! 2. **The engine can read every page of it** ([`open`]): it opens, its schema can be read, and
//!    `PRAGMA integrity_check` answers `ok`. This is what finds a file cut short at a page
//!    boundary, pages overwritten in the middle, and an index that no longer matches its table.
//!    It reads the whole file, so it is skipped in exactly one case: the download is
//!    byte-for-byte the local database ([`same_bytes`]), where putting it in place changes
//!    nothing.
//! 3. **It holds barca's history** ([`open`]): it has both the `runs` and the `materializations`
//!    table. Every version of barca that could upload a shared state created both before it
//!    did, so a database without them was not written by barca.
//! 4. **This version of barca can use it** ([`usable_schema`]), checked after barca's own
//!    additive migrations have been applied to the download (never to the local database):
//!    each of barca's tables has every column this version reads or writes ([`TABLES`]), and no
//!    other column that an insert by this version could not fill (`NOT NULL` without a default).
//!    A database written by an older barca passes, because the migrations bring it up to date.
//!    One written by a newer barca passes exactly when this version can still read and write
//!    it: tables and nullable columns this version does not know are left alone, which is how
//!    machines on different versions share one history. There is no schema version number to
//!    compare yet (#82); when there is, that comparison belongs here.
//!
//! Anything else that goes wrong while checking (the disk is full, the file cannot be written)
//! is an error of this machine, not a verdict on the object, and is reported as such.

use crate::BarcaError;
use crate::state_carry::{LOG_COLUMNS, RUN_COLUMNS, STEP_COLUMNS};
use std::fs;
use std::io::Read;
use turso::{Connection, Database, Value};

/// barca's tables and the columns this version reads or writes, besides the `id` row id of
/// the first three. [`crate::db::init_db`] creates exactly these (a test holds the two together).
pub(crate) const TABLES: &[(&str, &[&str])] = &[
    ("runs", RUN_COLUMNS),
    ("materializations", STEP_COLUMNS),
    ("logs", LOG_COLUMNS),
    (
        "cost_estimates",
        &[
            "node_id",
            "base_id",
            "estimate_seconds",
            "cpu_seconds",
            "max_rss_bytes",
            "samples",
            "updated_at",
        ],
    ),
    ("schedule_state", &["node_id", "last_fired_at"]),
];

/// The tables whose rows are identified by an `id` row id column.
const WITH_ROW_ID: [&str; 3] = ["runs", "materializations", "logs"];

/// The tables a database must already have to be a barca history at all (rule 3).
const HISTORY_TABLES: [&str; 2] = ["runs", "materializations"];

/// Rule 1. `Err` says why the file at `path` is not a whole SQLite file.
pub(crate) fn whole_file(path: &str) -> Result<(), String> {
    match fs::metadata(path) {
        Err(e) => Err(format!("it cannot be read: {e}")),
        Ok(m) if m.len() == 0 => Err("it is empty (0 bytes)".to_string()),
        Ok(_) => match crate::db::not_a_database(path) {
            Some(why) => Err(why),
            None => Ok(()),
        },
    }
}

/// True when the two files hold the same bytes. False when either cannot be read.
pub(crate) fn same_bytes(a: &str, b: &str) -> bool {
    let same = || -> std::io::Result<bool> {
        if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
            return Ok(false);
        }
        let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
        let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
        loop {
            let n = a.read(&mut x)?;
            if n == 0 {
                return Ok(b.read(&mut y)? == 0);
            }
            b.read_exact(&mut y[..n])?;
            if x[..n] != y[..n] {
                return Ok(false);
            }
        }
    };
    same().unwrap_or(false)
}

/// Whether [`open`] reads every page of the file (rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pages {
    /// Run `PRAGMA integrity_check`.
    Check,
    /// The file is byte-for-byte the local database: nothing to find that is not already there.
    SameAsLocal,
}

/// A downloaded shared state that passed every rule, open for the carry.
pub(crate) type Valid = (Database, Connection);

/// Rules 1 to 4 on the downloaded file at `path`. The outer error is this machine failing to
/// check; the inner `Err` says why the download is not valid. Afterwards the file has barca's
/// current schema (the migrations are applied to it between rules 3 and 4).
pub(crate) async fn open(path: &str, pages: Pages) -> Result<Result<Valid, String>, BarcaError> {
    if let Err(why) = whole_file(path) {
        return Ok(Err(why));
    }
    // Reading only: whatever the engine cannot read here is the file's fault.
    let read = async {
        let db = turso::Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        let mut tables = Vec::new();
        let mut rows = conn
            .query("SELECT name FROM sqlite_schema WHERE type = 'table'", ())
            .await?;
        while let Some(row) = rows.next().await? {
            tables.push(row.get::<String>(0)?);
        }
        drop(rows);
        let mut problems = Vec::new();
        if pages == Pages::Check {
            let mut rows = conn.query("PRAGMA integrity_check", ()).await?;
            while let Some(row) = rows.next().await? {
                let line = row.get::<String>(0)?;
                if line != "ok" {
                    problems.push(line);
                }
            }
        }
        Ok::<_, turso::Error>((db, conn, tables, problems))
    };
    let (db, conn, tables, problems) = match read.await {
        Ok(read) => read,
        Err(e) => return Ok(Err(format!("it cannot be read as a database: {e}"))),
    };
    if !problems.is_empty() {
        let more = match problems.len() {
            1 => String::new(),
            n => format!(" (and {} more)", n - 1),
        };
        return Ok(Err(format!(
            "its integrity check failed: {}{more}",
            problems[0]
        )));
    }
    if let Some(missing) = HISTORY_TABLES
        .iter()
        .find(|t| !tables.iter().any(|have| have == *t))
    {
        return Ok(Err(format!(
            "it is a SQLite database but not a barca history: it has no `{missing}` table"
        )));
    }
    let migrated = crate::db::init_schema(&conn).await;
    // Tables with barca's names and another program's columns can make a migration fail; then
    // the schema is the reason, and it is the download's fault.
    if let Err(why) = usable_schema(&conn).await? {
        return Ok(Err(why));
    }
    migrated.map_err(|e| {
        BarcaError::Db(format!(
            "could not bring the downloaded shared state {path} up to this version's schema: {e}"
        ))
    })?;
    Ok(Ok((db, conn)))
}

/// Rule 4, on a database the migrations have been applied to.
pub(crate) async fn usable_schema(conn: &Connection) -> Result<Result<(), String>, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read the downloaded schema: {e}"));
    for (table, columns) in TABLES {
        // (name, must be given a value by every insert)
        let mut found: Vec<(String, bool)> = Vec::new();
        let mut rows = conn
            .query(&format!("PRAGMA table_info({table})"), ())
            .await
            .map_err(failed)?;
        while let Some(row) = rows.next().await.map_err(failed)? {
            let name = row.get::<String>(1).map_err(failed)?;
            let not_null = row.get::<i64>(3).unwrap_or(0) != 0;
            let no_default = matches!(row.get_value(4), Ok(Value::Null) | Err(_));
            let primary_key = row.get::<i64>(5).unwrap_or(0) != 0;
            found.push((name, not_null && no_default && !primary_key));
        }
        let row_id = WITH_ROW_ID.contains(table).then_some("id");
        if let Some(missing) = row_id
            .iter()
            .chain(columns.iter())
            .find(|c| !found.iter().any(|(name, _)| name == *c))
        {
            return Ok(Err(format!(
                "its `{table}` table has no `{missing}` column, which this version of barca needs"
            )));
        }
        if let Some((unknown, _)) = found
            .iter()
            .find(|(name, required)| *required && !columns.contains(&name.as_str()))
        {
            return Ok(Err(format!(
                "its `{table}` table has a column `{unknown}` that must be filled and that this \
                 version of barca ({}) does not know: it was written by a newer barca",
                env!("CARGO_PKG_VERSION")
            )));
        }
    }
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn new_db(path: &str, sql: &[&str]) {
        let db = turso::Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        for statement in sql {
            conn.execute(statement, ()).await.unwrap();
        }
        crate::db::checkpoint(&conn).await.unwrap();
    }

    /// A barca history with `runs` runs of one step each, as one file.
    async fn history(path: &str, runs: usize) {
        crate::db::init_db(path).await.unwrap();
        let (_h, conn) = crate::db::open_conn(path).await.unwrap();
        for i in 0..runs {
            conn.execute(
                "INSERT INTO runs (run_id, command, files, status) VALUES (?1, 'get', '[]', 'success')",
                [format!("run-{i}")],
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, run_id) VALUES (?1, ?2, ?3)",
                [format!("p.py:n{i}"), format!("{i:064}"), format!("run-{i}")],
            )
            .await
            .unwrap();
        }
        crate::db::checkpoint(&conn).await.unwrap();
    }

    async fn verdict(path: &str) -> Result<(), String> {
        open(path, Pages::Check).await.unwrap().map(|_| ())
    }

    fn tmp() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pulled.db").to_string_lossy().to_string();
        (dir, path)
    }

    #[tokio::test]
    async fn a_history_written_by_this_version_is_valid_and_is_not_changed_by_the_check() {
        let (_dir, path) = tmp();
        history(&path, 3).await;
        crate::db::remove_sidecars(&path).unwrap();
        let before = fs::read(&path).unwrap();
        for pages in [Pages::SameAsLocal, Pages::Check] {
            assert!(open(&path, pages).await.unwrap().is_ok());
        }
        // Nothing was written to a database that is current: the log holds no frame (the
        // engine writes its 32-byte header on open).
        let log = fs::metadata(format!("{path}-wal")).map_or(0, |m| m.len());
        assert!(log <= 32, "the check wrote {log} bytes to the log");
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    /// `TABLES` is exactly what `init_db` creates: a column added to one and not the other
    /// would make every machine refuse (or wrongly accept) the shared state.
    #[tokio::test]
    async fn the_table_list_is_exactly_the_schema_init_db_creates() {
        let (_dir, path) = tmp();
        crate::db::init_db(&path).await.unwrap();
        let (_h, conn) = crate::db::open_conn(&path).await.unwrap();
        for (table, columns) in TABLES {
            let mut rows = conn
                .query(&format!("PRAGMA table_info({table})"), ())
                .await
                .unwrap();
            let mut have = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                have.push(row.get::<String>(1).unwrap());
            }
            let mut want: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
            if WITH_ROW_ID.contains(table) {
                want.push("id".to_string());
            }
            have.sort();
            want.sort();
            assert_eq!(have, want, "table {table}");
        }
    }

    #[tokio::test]
    async fn files_that_are_not_whole_sqlite_files_are_refused_by_name() {
        let (_dir, path) = tmp();
        for (bytes, why) in [
            (&b""[..], "empty (0 bytes)"),
            (&b"garbage"[..], "too short to be a database"),
            (&[b'x'; 5000][..], "does not start with a SQLite header"),
        ] {
            fs::write(&path, bytes).unwrap();
            let refused = verdict(&path).await.unwrap_err();
            assert!(refused.contains(why), "{refused}");
        }

        // Cut short in the middle of a page.
        fs::remove_file(&path).unwrap();
        history(&path, 40).await;
        crate::db::remove_sidecars(&path).unwrap();
        let whole = fs::read(&path).unwrap();
        fs::write(&path, &whole[..whole.len() - 1000]).unwrap();
        let refused = verdict(&path).await.unwrap_err();
        assert!(refused.contains("cut short"), "{refused}");
    }

    /// The header and the length are both right; only reading every page finds these.
    #[tokio::test]
    async fn a_file_cut_at_a_page_boundary_or_overwritten_inside_is_refused() {
        let (_dir, path) = tmp();
        history(&path, 300).await;
        crate::db::remove_sidecars(&path).unwrap();
        let whole = fs::read(&path).unwrap();
        let page = 4096;
        assert!(whole.len() > 12 * page, "{}", whole.len());

        fs::write(&path, &whole[..whole.len() - 4 * page]).unwrap();
        assert_eq!(whole_file(&path), Ok(()));
        let refused = verdict(&path).await.unwrap_err();
        assert!(
            refused.contains("cannot be read as a database") || refused.contains("integrity"),
            "{refused}"
        );

        let mut damaged = whole.clone();
        let middle = (whole.len() / page / 2) * page;
        damaged[middle..middle + 2 * page].fill(0);
        fs::write(&path, &damaged).unwrap();
        crate::db::remove_sidecars(&path).unwrap();
        assert_eq!(whole_file(&path), Ok(()));
        let refused = verdict(&path).await.unwrap_err();
        assert!(
            refused.contains("cannot be read as a database") || refused.contains("integrity"),
            "{refused}"
        );

        // Whole again, it is valid again.
        fs::write(&path, &whole).unwrap();
        crate::db::remove_sidecars(&path).unwrap();
        assert_eq!(verdict(&path).await, Ok(()));
    }

    #[tokio::test]
    async fn sqlite_databases_that_are_not_a_barca_history_are_refused() {
        let (_dir, path) = tmp();
        // No tables at all.
        new_db(&path, &["CREATE TABLE t (v TEXT)", "DROP TABLE t"]).await;
        let refused = verdict(&path).await.unwrap_err();
        assert!(refused.contains("not a barca history"), "{refused}");

        // Another program's database.
        let (_dir, path) = tmp();
        new_db(&path, &["CREATE TABLE notes (body TEXT)"]).await;
        let refused = verdict(&path).await.unwrap_err();
        assert!(refused.contains("no `runs` table"), "{refused}");

        // One of the two tables is not enough.
        let (_dir, path) = tmp();
        new_db(
            &path,
            &["CREATE TABLE runs (id INTEGER PRIMARY KEY, run_id TEXT)"],
        )
        .await;
        let refused = verdict(&path).await.unwrap_err();
        assert!(refused.contains("no `materializations` table"), "{refused}");

        // Tables with barca's names and other columns: the migrations cannot add what the
        // first version of the table already had.
        let (_dir, path) = tmp();
        new_db(
            &path,
            &[
                "CREATE TABLE runs (id INTEGER PRIMARY KEY, name TEXT)",
                "CREATE TABLE materializations (id INTEGER PRIMARY KEY, what TEXT)",
            ],
        )
        .await;
        let refused = verdict(&path).await.unwrap_err();
        assert!(refused.contains("has no `run_id` column"), "{refused}");
    }

    #[tokio::test]
    async fn an_older_barca_schema_is_valid_once_migrated() {
        let (_dir, path) = tmp();
        // The tables as 0.5.0 (the first version with shared state) created them.
        new_db(
            &path,
            &[
                "CREATE TABLE materializations (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                 node_id TEXT NOT NULL, run_hash TEXT, output_json TEXT, \
                 status TEXT NOT NULL DEFAULT 'success', created_at TEXT DEFAULT (datetime('now')))",
                "CREATE TABLE runs (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                 run_id TEXT UNIQUE NOT NULL, command TEXT NOT NULL, files TEXT NOT NULL, \
                 target TEXT, status TEXT NOT NULL DEFAULT 'running', steps_total INTEGER, \
                 steps_executed INTEGER DEFAULT 0, steps_cached INTEGER DEFAULT 0, \
                 started_at TEXT DEFAULT (datetime('now')), finished_at TEXT, elapsed_seconds REAL)",
                "INSERT INTO runs (run_id, command, files) VALUES ('old', 'get', 'a.py')",
            ],
        )
        .await;
        assert_eq!(verdict(&path).await, Ok(()));
    }

    #[tokio::test]
    async fn a_newer_schema_is_valid_only_while_this_version_can_write_to_it() {
        // A table and nullable or defaulted columns this version does not know: usable.
        let (_dir, path) = tmp();
        history(&path, 1).await;
        new_db(
            &path,
            &[
                "CREATE TABLE schema_version (version INTEGER NOT NULL)",
                "ALTER TABLE runs ADD COLUMN origin TEXT",
                "ALTER TABLE materializations ADD COLUMN tier INTEGER NOT NULL DEFAULT 0",
            ],
        )
        .await;
        assert_eq!(verdict(&path).await, Ok(()));

        // A column every insert must fill, which this version's inserts do not name.
        let (_dir, path) = tmp();
        new_db(
            &path,
            &[
                "CREATE TABLE materializations (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                 node_id TEXT NOT NULL, run_hash TEXT, output_json TEXT, \
                 status TEXT NOT NULL DEFAULT 'success', created_at TEXT, tenant TEXT NOT NULL)",
                "CREATE TABLE runs (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                 run_id TEXT UNIQUE NOT NULL, command TEXT NOT NULL, files TEXT NOT NULL, \
                 target TEXT, status TEXT NOT NULL DEFAULT 'running', steps_total INTEGER, \
                 steps_executed INTEGER DEFAULT 0, steps_cached INTEGER DEFAULT 0, \
                 started_at TEXT, finished_at TEXT, elapsed_seconds REAL)",
            ],
        )
        .await;
        let refused = verdict(&path).await.unwrap_err();
        assert!(
            refused.contains("`tenant`") && refused.contains("newer barca"),
            "{refused}"
        );
    }

    #[test]
    fn same_bytes_compares_content_not_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_string_lossy().to_string();
        let big = vec![7u8; 200_000];
        fs::write(p("a"), &big).unwrap();
        fs::write(p("b"), &big).unwrap();
        assert!(same_bytes(&p("a"), &p("b")));
        let mut other = big.clone();
        *other.last_mut().unwrap() = 8;
        fs::write(p("b"), &other).unwrap();
        assert!(!same_bytes(&p("a"), &p("b")));
        fs::write(p("b"), &big[..big.len() - 1]).unwrap();
        assert!(!same_bytes(&p("a"), &p("b")));
        assert!(!same_bytes(&p("a"), &p("missing")));
    }
}
