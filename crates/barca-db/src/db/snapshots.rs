//! Copies and views of the metadata DB for readers and backups: a [`ReadView`] reads the live
//! database in place, a [`DbSnapshot`] reads a private copy, and [`copy_for_backup`] copies it
//! in committed page batches. Direct filesystem reads of the live file go through [`held_file`].

use super::*;

/// The live metadata DB, opened for reading only: a path (a symbolic link to it in a temp
/// dir, removed on drop) that every reader in this module accepts in place of the database.
///
/// A [`DbSnapshot`] copies the database, which costs its size on every change (gigabytes for
/// a long history, written on Linux in full) and starts every read with a cold page cache. A
/// view instead reads the database in place: in WAL mode SQLite lets readers in any process
/// see the last commit while one writer appends, so a view takes neither barca's
/// cross-process lock nor the process-wide guard, and never waits for a run's writes or makes
/// one wait. Its connections are read-only (they never write, migrate or checkpoint) and run
/// each query in its own transaction, so a checkpoint ([`fold_log`]) waits at most for one
/// query. SQLite resolves the link, so the view shares the database's `-wal` and `-shm`.
///
/// Only a database this release uses as it is can be read in place: [`ReadView::open`]
/// answers `None` for one that is missing, not in WAL mode or would be migrated, and the
/// caller takes a [`DbSnapshot`] instead.
pub struct ReadView {
    path: String,
    _dir: tempfile::TempDir,
}

impl ReadView {
    pub async fn open(db_path: &str) -> Result<Option<Self>, BarcaError> {
        let Ok(target) = fs::canonicalize(db_path) else {
            return Ok(None);
        };
        if unusable_file(db_path).is_some() || empty_file(db_path) {
            return Ok(None);
        }
        let dir = tempfile::tempdir()
            .map_err(|e| BarcaError::Db(format!("failed to create read view dir: {e}")))?;
        let link = dir.path().join("metadata.db");
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|e| BarcaError::Db(format!("failed to create read view: {e}")))?;
        let path = link.display().to_string();
        let (db, conn) = connect_view(&path).await.map_err(|e| named(e, db_path))?;
        let usable = view_usable(&conn).await;
        drop(conn);
        drop(db);
        if !usable {
            return Ok(None);
        }
        READ_VIEWS
            .lock()
            .unwrap()
            .insert(path.clone(), target.display().to_string());
        Ok(Some(Self { path, _dir: dir }))
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for ReadView {
    fn drop(&mut self) {
        READ_VIEWS.lock().unwrap().remove(&self.path);
    }
}

/// Whether the database behind `conn` is read as it is: this release's format with nothing
/// to add, in WAL mode.
async fn view_usable(conn: &libsql::Connection) -> bool {
    if !matches!(crate::db::schema::ddl::read_version(conn).await, Ok(v) if v == crate::db::schema::ddl::VERSION)
    {
        return false;
    }
    let Ok(schema) = crate::state_validate::Schema::read(conn).await else {
        return false;
    };
    if crate::db::schema::ddl::additions_missing_in(&schema) {
        return false;
    }
    let Ok(mut rows) = conn.query("PRAGMA journal_mode", ()).await else {
        return false;
    };
    let mode = match rows.next().await {
        Ok(Some(row)) => row.get::<String>(0).ok(),
        _ => None,
    };
    mode.is_some_and(|mode| mode.eq_ignore_ascii_case("wal"))
}

/// Copy a consistent committed database in small page batches. Only the private
/// destination is held across yields and while calculating its digest.
pub async fn copy_for_backup(
    db_path: &str,
    copy: &Path,
    unless: Option<&str>,
    until: barca_model::process::Until<'_>,
) -> Result<Option<String>, BarcaError> {
    until.check()?;
    let failed = |e| BarcaError::Db(format!("metadata backup staging: {e}"));
    let parent = copy
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Contain any SQLite sidecars as well, including on cancellation.
    let dir = tempfile::tempdir_in(parent).map_err(failed)?;
    let stage = tempfile::NamedTempFile::new_in(dir.path()).map_err(failed)?;
    {
        let mut backup = {
            // Serialize the source open with restore/reset, then yield the lock.
            let _lock = lock_db_with_until(db_path, until).await?;
            super::online_backup::Backup::open(Path::new(db_path), stage.path())?
        };
        let started = Instant::now();
        loop {
            until.check()?;
            if started.elapsed() > Duration::from_secs(300) {
                return Err(BarcaError::Db(
                    "online metadata backup exceeded five minutes".into(),
                ));
            }
            match backup.step()? {
                libsql::ffi::SQLITE_DONE => break,
                libsql::ffi::SQLITE_OK => tokio::task::yield_now().await,
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }
    // Validate the exact committed image that will be published.
    let stage_path = stage
        .path()
        .to_str()
        .ok_or_else(|| BarcaError::Db("backup path is not UTF-8".into()))?;
    drop(
        connect_read_only(stage_path)
            .await
            .map_err(|e| named(e, db_path))?,
    );
    let mut reader = std::io::BufReader::new(stage.reopen().map_err(failed)?);
    let mut buf = vec![0; 64 * 1024];
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut hash = Sha256::new();
    loop {
        until.check()?;
        let n = reader.read(&mut buf).map_err(failed)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        tokio::task::yield_now().await;
    }
    let digest = format!("{:x}", hash.finalize());
    if unless == Some(&digest) {
        return Ok(None);
    }
    stage.persist(copy).map_err(|e| failed(e.error))?;
    Ok(Some(digest))
}

/// Each chunk of the database file at `path`, in order, read through [`held_file`].
fn read_held(
    path: &Path,
    mut each: impl FnMut(&[u8]) -> Result<(), BarcaError>,
) -> Result<(), BarcaError> {
    use std::os::unix::fs::FileExt;
    let failed =
        |e: std::io::Error| BarcaError::Db(format!("failed to read {}: {e}", path.display()));
    let file = held_file(path).map_err(failed)?;
    let mut buf = vec![0u8; 1 << 16];
    let mut offset = 0u64;
    loop {
        let n = file.read_at(&mut buf, offset).map_err(failed)?;
        if n == 0 {
            return Ok(());
        }
        each(&buf[..n])?;
        offset += n as u64;
    }
}

/// Copy the database file at `path` to a new file `target`, through [`held_file`].
fn copy_held(path: &Path, target: &Path) -> Result<(), BarcaError> {
    use std::io::Write;
    let failed = |e: std::io::Error| {
        BarcaError::Db(format!(
            "failed to copy {} to {}: {e}",
            path.display(),
            target.display()
        ))
    };
    let mut out = fs::File::create(target).map_err(failed)?;
    out.set_permissions(fs::metadata(path).map_err(failed)?.permissions())
        .map_err(failed)?;
    read_held(path, |chunk| out.write_all(chunk).map_err(failed))
}

/// A private, point-in-time copy of a metadata DB (main file + WAL) in a temp
/// dir, deleted on drop. Read-only consumers query the copy, so the original —
/// possibly being written by another barca process — is never opened, locked
/// for longer than the copy takes, created, migrated, or checkpointed.
pub struct DbSnapshot {
    _dir: tempfile::TempDir,
    path: String,
}

impl DbSnapshot {
    /// Copy `db_path` (and its `-wal`) into a fresh temp dir. `None` when there
    /// is no DB yet — the absence is preserved, not filled in.
    ///
    /// If barca's cross-process lock file exists, the copy is taken under that
    /// lock so it can't interleave with another process's write. The lock file
    /// is opened, never created: an older barca that doesn't lock leaves no
    /// file, and the copy proceeds unlocked (WAL recovery then stops at the
    /// last complete commit).
    pub async fn take(db_path: &str) -> Result<Option<Self>, BarcaError> {
        if !Path::new(db_path).exists() {
            return Ok(None);
        }
        let lock_path = format!("{db_path}.lock");
        let _lock = if Path::new(&lock_path).exists() {
            Some(acquire_file_lock(db_path, DB_FILE_LOCK_WAIT).await?)
        } else {
            None
        };

        let db_path = db_path.to_owned();
        tokio::task::spawn_blocking(move || {
            // Keep the writer lock until both files have been copied, even if the caller
            // is cancelled while the blocking operation is running.
            let _lock = _lock;
            let dir = tempfile::tempdir()
                .map_err(|e| BarcaError::Db(format!("failed to create snapshot dir: {e}")))?;
            let copy = dir.path().join("metadata.db");
            // Not `fs::copy`: closing a descriptor of the database drops this process's locks
            // on it ([`HELD_FILES`]).
            copy_held(Path::new(&db_path), &copy)
                .map_err(|e| BarcaError::Db(format!("failed to snapshot DB: {e}")))?;
            let wal = format!("{db_path}-wal");
            if Path::new(&wal).exists() {
                fs::copy(&wal, dir.path().join("metadata.db-wal"))
                    .map_err(|e| BarcaError::Db(format!("failed to snapshot DB WAL: {e}")))?;
            }
            let path = copy.display().to_string();
            // A file named as a metadata database, but gone with this snapshot: the owner
            // opens it per operation and keeps no connection to it ([`owns`]).
            SNAPSHOT_COPIES.lock().unwrap().insert(path.clone());
            Ok(Some(Self { path, _dir: dir }))
        })
        .await
        .map_err(|e| BarcaError::Db(format!("snapshot task failed: {e}")))?
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for DbSnapshot {
    fn drop(&mut self) {
        SNAPSHOT_COPIES.lock().unwrap().remove(&self.path);
    }
}

#[cfg(test)]
mod read_view_tests {
    use super::*;

    fn db_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("metadata.db").display().to_string()
    }

    async fn execute(path: &str, sql: &str) {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(path).await.unwrap();
        // A pragma that sets a mode answers a row.
        let mut rows = conn.query(sql, ()).await.unwrap();
        while rows.next().await.unwrap().is_some() {}
    }

    #[tokio::test]
    async fn a_read_view_reads_the_live_database_while_a_writer_holds_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        create_run(&path, "before", "get", "[]", None, Some(1))
            .await
            .unwrap();
        let view = ReadView::open(&path)
            .await
            .unwrap()
            .expect("a current WAL database");
        assert!(is_read_view(view.path()));
        // A commit after the view opened is seen: it reads the database, not a copy.
        create_run(&path, "after", "get", "[]", None, Some(1))
            .await
            .unwrap();
        // Neither the process-wide guard nor the cross-process lock is taken by a view.
        let held = lock_db(&path).await.unwrap();
        let read = tokio::time::timeout(Duration::from_secs(2), async {
            (
                get_run(view.path(), "before").await.unwrap(),
                get_run(view.path(), "after").await.unwrap(),
            )
        })
        .await
        .expect("a view does not wait for the writer's lock");
        drop(held);
        assert!(read.0.is_some() && read.1.is_some());
        let link = view.path().to_string();
        drop(view);
        assert!(!is_read_view(&link));
    }

    /// Readers in place and a writer meet at SQLite's own locks: the first connection after the
    /// last one closed rebuilds the log's index under a lock for a moment. Every connection
    /// waits for it; without that, a run's open failed with `database is locked` while the
    /// server read the database.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn readers_in_place_and_a_writer_wait_for_each_other_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let reads = tokio::spawn({
            let view_path = view.path().to_string();
            async move {
                for _ in 0..200 {
                    get_recent_runs(&view_path, 5)
                        .await
                        .expect("a read in place");
                }
            }
        });
        for i in 0..100 {
            create_run(&path, &format!("r{i}"), "get", "[]", None, Some(1))
                .await
                .expect("a write beside readers in place");
        }
        reads.await.unwrap();
        assert_eq!(get_recent_runs(view.path(), 1000).await.unwrap().len(), 100);
    }

    /// Every connection holds a shared lock on the database file, and a writer that finds none
    /// but its own when it closes takes the database as its alone: it checkpoints without
    /// asking the readers, deletes the log and its index, and a reader still in the middle of a
    /// query reads pages overwritten under it (`database disk image is malformed`, seen on a
    /// `--read-only` server beside a writer's `VACUUM`). The locks are POSIX locks, which a
    /// process loses, all of them, when it closes any descriptor of the file: so barca's own
    /// look at the file (the header check every open makes) must not close one.
    #[tokio::test]
    async fn looking_at_the_database_file_keeps_this_process_s_locks() {
        const WRITER: &str = barca_model::test_hooks::LAST_CONNECTION_WRITER;
        if let Some(path) = barca_model::test_hooks::last_connection_writer() {
            // The other process: one write, then its connection closes.
            let path = path.to_string_lossy().into_owned();
            create_run(&path, "theirs", "get", "[]", None, Some(1))
                .await
                .unwrap();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        create_run(&path, "ours", "get", "[]", None, Some(1))
            .await
            .unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let (_db, reader) = open_conn(view.path()).await.unwrap();
        let mut rows = reader.query("SELECT count(*) FROM runs", ()).await.unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
        drop(rows);
        // What every request of a read-only server does while others read: open a view, and
        // with it look at the file.
        let _second = ReadView::open(&path).await.unwrap().unwrap();
        assert!(unusable_file(&path).is_none());
        let mut writer = std::process::Command::new(std::env::current_exe().unwrap());
        writer
            .args([
                "--exact",
                "db::snapshots::read_view_tests::looking_at_the_database_file_keeps_this_process_s_locks",
                "--nocapture",
            ])
            .env(WRITER, &path);
        let output = barca_model::spawn_lock::output_std(&mut writer).unwrap();
        assert!(
            output.status.success(),
            "the writer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            Path::new(&format!("{path}-wal")).exists(),
            "the writer took itself for the last connection and deleted the log under a reader"
        );
        let mut rows = reader.query("SELECT count(*) FROM runs", ()).await.unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn a_read_view_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let (_db, conn) = open_conn(view.path()).await.unwrap();
        let wrote = conn
            .execute(
                "INSERT INTO logs (run_id, node_id, seq, line) VALUES ('r', 'n', 0, 'l')",
                (),
            )
            .await;
        assert!(wrote.is_err(), "a view's connection is read-only");
    }

    #[tokio::test]
    async fn no_read_view_of_a_database_that_is_missing_old_or_not_in_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        assert!(ReadView::open(&path).await.unwrap().is_none(), "missing");
        init_db(&path).await.unwrap();
        // An index this release adds is missing until a writer opens the database.
        execute(&path, "DROP INDEX idx_mat_failed").await;
        assert!(
            ReadView::open(&path).await.unwrap().is_none(),
            "would be migrated"
        );
        init_db(&path).await.unwrap();
        assert!(ReadView::open(&path).await.unwrap().is_some());
        execute(&path, "PRAGMA journal_mode=DELETE").await;
        assert!(
            ReadView::open(&path).await.unwrap().is_none(),
            "not in WAL mode"
        );
    }

    #[tokio::test]
    async fn a_checkpoint_waits_for_a_read_view_query_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        create_run(&path, "r", "get", "[]", None, Some(1))
            .await
            .unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let (_db, reader) = open_conn(view.path()).await.unwrap();
        // A read in progress: its transaction holds the log.
        reader.execute("BEGIN", ()).await.unwrap();
        let mut rows = reader.query("SELECT count(*) FROM runs", ()).await.unwrap();
        rows.next().await.unwrap();
        let folding = tokio::spawn({
            let path = path.clone();
            async move {
                let _lock = lock_db(&path).await.unwrap();
                fold_log(&path).await
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(rows);
        reader.execute("COMMIT", ()).await.unwrap();
        folding
            .await
            .unwrap()
            .expect("the checkpoint finishes once the read ends");
        assert!(wal_is_clean(&path));
    }
}
