//! The connection this process keeps to a metadata database ([`OWNED`]): that it is reused,
//! and that it is never used once it is no longer a connection to the database at its path.
//! `docs/engineering/metadata-owner.md` lists these beside the properties they hold.

use super::*;
use libsql::Builder;

fn db_in(dir: &tempfile::TempDir) -> String {
    dir.path().join("metadata.db").display().to_string()
}

async fn write_run(path: &str, run_id: &str) {
    create_run(path, run_id, "get", "[]", None, Some(1))
        .await
        .unwrap();
}

/// A connection of something that is not this process's owner: another process, as far as
/// SQLite's files are concerned.
async fn raw(path: &str) -> (libsql::Database, libsql::Connection) {
    let db = Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

async fn raw_run_ids(path: &str) -> Vec<String> {
    let (_db, conn) = raw(path).await;
    let mut rows = conn
        .query("SELECT run_id FROM runs ORDER BY run_id", ())
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get::<String>(0).unwrap());
    }
    ids
}

#[tokio::test]
async fn operations_share_one_connection() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    let kept = kept_serial(&path).expect("the owner keeps the connection that created it");
    for i in 0..20 {
        write_run(&path, &format!("r{i}")).await;
        assert!(get_run(&path, &format!("r{i}")).await.unwrap().is_some());
        init_db(&path).await.unwrap();
    }
    assert_eq!(
        kept_serial(&path),
        Some(kept),
        "one connection for all of it"
    );
    // What other processes see is what was committed, with the connection still open.
    assert_eq!(raw_run_ids(&path).await.len(), 20);
}

#[tokio::test]
async fn a_file_that_is_not_a_metadata_database_gets_a_connection_per_operation() {
    let dir = tempfile::tempdir().unwrap();
    // A backup copy, a staged restore: read once, then renamed or deleted.
    let path = dir.path().join("copy.db").display().to_string();
    init_db(&path).await.unwrap();
    write_run(&path, "r").await;
    assert_eq!(kept_serial(&path), None);
    // No connection is left: the close of the last one folded the log in and deleted it.
    assert!(wal_is_clean(&path));
    // Nor to a snapshot's copy, which is named as a metadata database.
    let live = db_in(&dir);
    init_db(&live).await.unwrap();
    let snapshot = DbSnapshot::take(&live).await.unwrap().unwrap();
    init_db_as(snapshot.path(), &live).await.unwrap();
    assert!(get_run(snapshot.path(), "r").await.unwrap().is_none());
    assert_eq!(kept_serial(snapshot.path()), None);
}

/// What `barca state restore` and `barca state reset` do while this process (a CLI in the
/// middle of a command, or with the fix that lets one run beside it, a server) keeps a
/// connection: the next operation must be on the database now at the path, and closing the
/// connection to the one moved aside must leave the new database's log alone. SQLite deletes
/// a log by name when the last connection to its database closes; it does not when the
/// database is no longer at the path it was opened on.
#[tokio::test]
async fn a_replaced_database_gets_a_new_connection_and_keeps_its_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    write_run(&path, "old").await;
    let before = kept_serial(&path).unwrap();
    // The copy a restore validated and is about to move into place.
    let staged = dir.path().join("staged.db").display().to_string();
    init_db(&staged).await.unwrap();
    write_run(&staged, "new").await;
    assert!(wal_is_clean(&staged));
    let pre = format!("{path}.pre-restore");
    {
        let _lock = lock_db(&path).await.unwrap();
        fold_log(&path).await.unwrap();
        std::fs::rename(&path, &pre).unwrap();
        remove_sidecars(&path).unwrap();
        std::fs::rename(&staged, &path).unwrap();
    }
    // Another process starts on the restored database and stays connected, with commits in
    // the log that are nowhere else yet.
    let (_other_db, other) = raw(&path).await;
    other
        .execute(
            "INSERT INTO runs (run_id, command, files, status) VALUES ('theirs', 'get', '[]', 'success')",
            (),
        )
        .await
        .unwrap();
    let wal = format!("{path}-wal");
    let log_before = std::fs::metadata(&wal).unwrap().len();
    assert!(log_before > 0, "their commit is in the log");

    // This process's next operations: on the restored database.
    assert!(get_run(&path, "new").await.unwrap().is_some());
    assert!(get_run(&path, "old").await.unwrap().is_none());
    assert!(get_run(&path, "theirs").await.unwrap().is_some());
    write_run(&path, "ours").await;
    let after = kept_serial(&path).unwrap();
    assert_ne!(
        after, before,
        "a new connection, to the file now at the path"
    );

    // The old connection closed on the way. The restored database still has its log, and
    // nothing of it was lost.
    assert!(std::fs::metadata(&wal).unwrap().len() >= log_before);
    assert_eq!(raw_run_ids(&path).await, ["new", "ours", "theirs"]);
    // Nothing was written to the database moved aside.
    assert_eq!(raw_run_ids(&pre).await, ["old"]);
}

/// A log deleted under the connection (what another process's closing connection does when
/// this process lost its locks on the file, [`HELD_FILES`]) leaves it writing to a file no
/// other process will ever read. The owner notices the log at the path is not the one the
/// connection has open.
#[tokio::test]
async fn a_log_deleted_under_the_connection_is_noticed() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    write_run(&path, "before").await;
    let before = kept_serial(&path).unwrap();
    {
        let _lock = lock_db(&path).await.unwrap();
        // Everything is in the main file, as it is when a last connection deletes the log.
        fold_log(&path).await.unwrap();
        remove_sidecars(&path).unwrap();
    }
    write_run(&path, "after").await;
    assert_ne!(kept_serial(&path), Some(before), "a new connection");
    assert_eq!(raw_run_ids(&path).await, ["after", "before"]);
}

/// The writer of another process that closes its connection is not the last one while this
/// process keeps one: it must not checkpoint for itself and delete the log, and this process's
/// connection stays the connection to the database.
#[tokio::test]
async fn another_process_closing_its_connection_leaves_the_owned_one_its_log() {
    if let Some(path) = barca_model::test_hooks::last_connection_writer() {
        // The other process: one write, then its connections close as a command's do.
        let path = path.to_string_lossy().into_owned();
        write_run(&path, "theirs").await;
        close_connections();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    write_run(&path, "ours").await;
    let kept = kept_serial(&path).unwrap();
    let wal = format!("{path}-wal");
    let log = file_id(&wal).expect("the kept connection has a log");
    let mut writer = std::process::Command::new(std::env::current_exe().unwrap());
    writer
        .args([
            "--exact",
            "db::owner_tests::another_process_closing_its_connection_leaves_the_owned_one_its_log",
            "--nocapture",
        ])
        .env(barca_model::test_hooks::LAST_CONNECTION_WRITER, &path);
    let output = barca_model::spawn_lock::output_std(&mut writer).unwrap();
    assert!(
        output.status.success(),
        "the writer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(file_id(&wal), Some(log), "the log is the one it was");
    write_run(&path, "ours-again").await;
    assert_eq!(kept_serial(&path), Some(kept), "and the connection is kept");
    assert_eq!(raw_run_ids(&path).await, ["ours", "ours-again", "theirs"]);
}

/// A process killed between `BEGIN` and `COMMIT`: its lock is gone with it, nothing of the
/// transaction is in the database, and the next process reads and writes at once.
#[tokio::test]
async fn a_process_killed_mid_transaction_leaves_nothing_of_it() {
    if let Some(path) = barca_model::test_hooks::last_connection_writer() {
        let path = path.to_string_lossy().into_owned();
        let _g = db_guard().await;
        let (_handle, conn) = open_conn(&path).await.unwrap();
        conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        for i in 0..500 {
            conn.execute(
                "INSERT INTO runs (run_id, command, files, status) VALUES (?1, 'get', '[]', 'success')",
                [format!("half-{i}")],
            )
            .await
            .unwrap();
        }
        std::fs::write(format!("{path}.in-transaction"), b"").unwrap();
        // Killed here, holding the lock and the transaction.
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    write_run(&path, "committed").await;
    let mut writer = std::process::Command::new(std::env::current_exe().unwrap());
    writer
        .args([
            "--exact",
            "db::owner_tests::a_process_killed_mid_transaction_leaves_nothing_of_it",
            "--nocapture",
        ])
        .env(barca_model::test_hooks::LAST_CONNECTION_WRITER, &path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = barca_model::spawn_lock::spawn_std(&mut writer).unwrap();
    let marker = format!("{path}.in-transaction");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !Path::new(&marker).exists() {
        assert!(Instant::now() < deadline, "the writer never began");
        assert!(child.try_wait().unwrap().is_none(), "the writer exited");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let started = Instant::now();
    write_run(&path, "after").await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "no lock left behind"
    );
    assert_eq!(raw_run_ids(&path).await, ["after", "committed"]);
    assert_eq!(count_runs(&path).await.unwrap(), 2);
}

/// An operation whose future is dropped between `BEGIN` and `COMMIT` (a cancelled request)
/// used to close its own connection, and SQLite rolled back. The kept connection must not
/// carry that transaction into the next operation, in either order the operation's handle and
/// connection are dropped.
#[tokio::test]
async fn a_transaction_left_open_is_not_handed_to_the_next_operation() {
    for handle_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        write_run(&path, "committed").await;
        let before = kept_serial(&path).unwrap();
        {
            let _g = db_guard().await;
            let (handle, conn) = open_conn(&path).await.unwrap();
            conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
            conn.execute(
                "INSERT INTO runs (run_id, command, files, status) VALUES ('half', 'get', '[]', 'success')",
                (),
            )
            .await
            .unwrap();
            if handle_first {
                drop(handle);
                drop(conn);
            } else {
                drop(conn);
                drop(handle);
            }
        }
        // Another process can write at once: the transaction did not outlive the operation.
        let (_other_db, other) = raw(&path).await;
        other
            .execute(
                "INSERT INTO runs (run_id, command, files, status) VALUES ('theirs', 'get', '[]', 'success')",
                (),
            )
            .await
            .expect("no write lock was left on the database");
        write_run(&path, "next").await;
        assert_ne!(
            kept_serial(&path),
            Some(before),
            "not the connection it was"
        );
        assert_eq!(
            raw_run_ids(&path).await,
            ["committed", "next", "theirs"],
            "handle dropped first: {handle_first}"
        );
    }
}

/// The CLI's last act: the kept connection closes under the lock, and as the last connection
/// it folds the log into the main file, which is then the whole database.
#[tokio::test]
async fn closing_the_kept_connections_folds_the_log_in() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    for i in 0..50 {
        write_run(&path, &format!("r{i}")).await;
    }
    assert!(!wal_is_clean(&path), "commits are in the log");
    // This database's only: other tests of this process keep connections of their own.
    close_kept(|kept| kept == path);
    assert_eq!(kept_serial(&path), None);
    assert!(wal_is_clean(&path));
    let copy = dir.path().join("copy.db").display().to_string();
    std::fs::copy(&path, &copy).unwrap();
    assert_eq!(raw_run_ids(&copy).await.len(), 50);
}
