//! Periodic WAL maintenance on writable servers. Take only the per-file lock to
//! coordinate with restore and private snapshots. Never enter the process-wide DB_LOCK.
use super::*;

pub const WAL_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WalCheckpoint {
    pub blocker: Option<String>,
    pub busy: bool,
    pub checkpointed: i32,
    pub frames: i32,
    pub over_threshold: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub threshold_bytes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub wal_bytes: u64,
}

pub async fn maintain_wal(path: &str, threshold: u64) -> Result<Option<WalCheckpoint>, BarcaError> {
    if !Path::new(path).exists() {
        return Ok(None);
    }
    let wait = Duration::from_millis(100);
    let _lock = tokio::time::timeout(wait, acquire_file_lock(path, DB_FILE_LOCK_WAIT))
        .await
        .map_err(|_| {
            BarcaError::Db("metadata WAL checkpoint waiting for a database operation".into())
        })??;
    let db = Builder::new_local(path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .build()
        .await
        .map_err(db_open_error)?;
    let conn = db.connect().map_err(db_open_error)?;
    conn.busy_timeout(wait).map_err(db_open_error)?;
    let size = || match fs::metadata(format!("{path}-wal")) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(BarcaError::Db(format!("cannot measure metadata WAL: {e}"))),
    };
    let before = size()?;
    let over_threshold = before > threshold;
    let mut result = checkpoint_row(&conn, "PRAGMA main.wal_checkpoint(PASSIVE)").await?;
    if over_threshold {
        result = checkpoint_row(&conn, "PRAGMA main.wal_checkpoint(TRUNCATE)").await?;
    }
    let (busy, frames, checkpointed) = result;
    let busy = busy || checkpointed < frames;
    let blocker = busy.then(|| {
        if let Some(local) = super::read_activity::active(path) {
            local
        } else if frames >= 0 {
            "blocked by a reader outside this process".into()
        } else {
            "another writer or checkpoint is active".into()
        }
    });
    Ok(Some(WalCheckpoint {
        blocker,
        busy,
        checkpointed,
        frames,
        over_threshold,
        threshold_bytes: threshold,
        wal_bytes: size()?,
    }))
}

pub(super) async fn checkpoint_row(
    conn: &libsql::Connection,
    sql: &str,
) -> Result<(bool, i32, i32), BarcaError> {
    let mut rows = conn.query(sql, ()).await.map_err(db_open_error)?;
    let row = rows
        .next()
        .await
        .map_err(db_open_error)?
        .ok_or_else(|| BarcaError::Db("checkpoint returned no result".into()))?;
    Ok((
        row.get::<i32>(0).map_err(db_open_error)? != 0,
        row.get(1).map_err(db_open_error)?,
        row.get(2).map_err(db_open_error)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[tokio::test]
    async fn external_reader_blocks_truncation_until_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        let mut command = std::process::Command::new("python3");
        command.args(["-c", "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1]); c.execute('BEGIN'); c.execute('SELECT count(*) FROM runs').fetchone(); print('ready', flush=True); sys.stdin.read(); c.close()", &path])
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        let mut child = barca_model::spawn_lock::spawn_std(&mut command).unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        create_run(&path, "new", "get", "[]", None, None)
            .await
            .unwrap();
        {
            let _guard = db_guard().await;
            let (_handle, conn) = open_conn(&path).await.unwrap();
            conn.busy_timeout(Duration::from_secs(5)).unwrap();
            let started = Instant::now();
            assert!(checkpoint_once(&conn).await.unwrap());
            assert!(started.elapsed() < Duration::from_secs(1));
            let mut rows = conn.query("PRAGMA busy_timeout", ()).await.unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                5000
            );
        }
        let blocked = maintain_wal(&path, 1).await.unwrap().unwrap();
        assert!(blocked.busy && blocked.over_threshold && blocked.wal_bytes > 1);
        assert!(blocked.frames > blocked.checkpointed);
        assert_eq!(
            blocked.blocker.as_deref(),
            Some("blocked by a reader outside this process")
        );
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        let clear = maintain_wal(&path, 1).await.unwrap().unwrap();
        assert!(!clear.busy && clear.blocker.is_none());
        assert_eq!(clear.wal_bytes, 0);
    }

    #[tokio::test]
    async fn checkpoint_can_truncate_between_backup_steps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        let (_db, conn) = connect(&path).await.unwrap();
        conn.execute(
            "CREATE TABLE payload AS SELECT zeroblob(4194304) AS bytes",
            (),
        )
        .await
        .unwrap();
        let stage = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let mut backup = super::super::online_backup::Backup::open(Path::new(&path), stage.path())
            .await
            .unwrap();
        assert_eq!(backup.step().unwrap(), libsql::ffi::SQLITE_OK);
        conn.execute("INSERT INTO payload VALUES (123)", ())
            .await
            .unwrap();
        let report = maintain_wal(&path, 1).await.unwrap().unwrap();
        assert!(!report.busy && report.over_threshold);
        assert_eq!(report.wal_bytes, 0);
        while backup.step().unwrap() != libsql::ffi::SQLITE_DONE {}
        drop(backup);
        let db = Builder::new_local(stage.path()).build().await.unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query("SELECT count(*) FROM payload", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn blocked_checkpoint_names_live_local_transaction_and_ignores_idle_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        let view = ReadView::open(&path).await.unwrap().unwrap();
        let (_db, conn) = open_conn(view.path()).await.unwrap();
        conn.execute("BEGIN", ()).await.unwrap();
        let mut rows = conn.query("SELECT count(*) FROM runs", ()).await.unwrap();
        rows.next().await.unwrap();
        drop(rows);
        create_run(&path, "after", "get", "[]", None, None)
            .await
            .unwrap();
        let result = maintain_wal(&path, 1).await.unwrap().unwrap();
        assert!(result.busy);
        assert!(result.blocker.unwrap().contains("ReadView runs"));
        conn.execute("COMMIT", ()).await.unwrap();
        assert!(super::super::read_activity::active(&path).is_none());
        assert!(!maintain_wal(&path, 1).await.unwrap().unwrap().busy);
    }
}
