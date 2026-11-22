//! The run queue of `barca serve`: every run the server has accepted and not finished, in the
//! order it accepted them, with the request that describes each (`run_queue`), and whether an
//! operator paused the queue (`server_state`). Both survive a restart: a server that starts
//! resumes the runs it finds queued, in order, under their run ids.
//!
//! Additive tables of metadata format 1 (`specs/interfaces/metadata-schema.md`): `init_db` creates them, a
//! format-1 database from an earlier release gains them in the additive migration, and a reader
//! of a database without them (an older copy, a snapshot of one) finds the queue empty and not
//! paused. Only `barca serve` writes them; what a row's `spec` holds is the server's business
//! (`barca-server`), stored as text.

use barca_db::db;
use barca_model::BarcaError;
use libsql::Connection;

use barca_db::db::schema::ddl::{RUN_QUEUE as CREATE_QUEUE, SERVER_STATE as CREATE_SERVER_STATE};

/// The `server_state` key of a paused queue; its value is when it was paused (unix seconds).
const PAUSED_AT: &str = "queue_paused_at";

/// Where a queued run is: waiting to start, or started (until it records how it ended), or
/// started and in flight when its server began to drain (#628).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueState {
    Queued,
    Running,
    /// In flight when the server was told to stop (SIGTERM or SIGINT). A row still in this state
    /// when the next server starts was killed during the drain by whatever sent the signal (a
    /// supervisor whose grace period ran out), not by a crash of its own: it is put back in the
    /// queue, once.
    Draining,
}

impl QueueState {
    pub fn as_str(self) -> &'static str {
        match self {
            QueueState::Queued => "queued",
            QueueState::Running => "running",
            QueueState::Draining => "draining",
        }
    }
}

/// One row of `run_queue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRun {
    pub run_id: String,
    /// Its place in the order the server accepted runs (smaller first).
    pub seq: i64,
    pub state: QueueState,
    /// The server's description of the run (JSON).
    pub spec: String,
    /// When it was accepted, RFC 3339 UTC.
    pub queued_at: String,
}

fn failed(what: &'static str) -> impl Fn(libsql::Error) -> BarcaError {
    move |e| BarcaError::Db(format!("run queue: {what}: {e}"))
}

async fn has_table(conn: &Connection, name: &str) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
        )
        .await
        .map_err(failed("reading the schema"))?;
    let found = rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some();
    while rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some()
    {}
    Ok(found)
}

/// The tables, for a writer whose database was created before them (or replaced since by
/// `barca state reset`).
async fn ensure(conn: &Connection) -> Result<(), BarcaError> {
    for create in [CREATE_QUEUE, CREATE_SERVER_STATE] {
        conn.execute(create, ())
            .await
            .map_err(failed("creating the table"))?;
    }
    Ok(())
}

/// Add `run_id` at the end of the queue; returns its `seq`.
pub async fn push(
    db_path: &str,
    run_id: &str,
    spec: &str,
    queued_at: &str,
) -> Result<i64, BarcaError> {
    push_with_slot(db_path, run_id, spec, queued_at, None).await
}

/// A scheduled root and its restart cursor become durable at the same commit.
pub async fn push_with_slot(
    db_path: &str,
    run_id: &str,
    spec: &str,
    queued_at: &str,
    slot: Option<(&str, i64)>,
) -> Result<i64, BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    ensure(&conn).await?;
    db::with_tx(&conn, "BEGIN IMMEDIATE", "run queue write", async {
        let mut rows = conn
            .query("SELECT COALESCE(MAX(seq), 0) + 1 FROM run_queue", ())
            .await
            .map_err(failed("reading the order"))?;
        let seq = rows
            .next()
            .await
            .map_err(failed("reading the order"))?
            .and_then(|row| row.get::<i64>(0).ok())
            .unwrap_or(1);
        while rows
            .next()
            .await
            .map_err(failed("reading the order"))?
            .is_some()
        {}
        drop(rows);
        db::run_queue::insert_queued(&conn, run_id, seq, spec, queued_at)
            .await
            .map_err(failed("adding a run"))?;
        if let Some((job, epoch)) = slot {
            db::record_tick_on(
                &conn,
                db::NewTick {
                    schedule_id: job,
                    scheduled_at: epoch,
                    action: db::TickDecision::Fired,
                    skipped_for: None,
                    run_id: Some(run_id),
                },
            )
            .await?;
            db::advance_schedule_cursor(&conn, job, epoch).await?;
        }
        Ok(seq)
    })
    .await
}

/// Every queued run, in order. A database without the table has none.
pub async fn list(db_path: &str) -> Result<Vec<QueuedRun>, BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    if !has_table(&conn, "run_queue").await? {
        return Ok(Vec::new());
    }
    let mut rows = conn
        .query(
            "SELECT run_id, seq, state, spec, queued_at FROM run_queue ORDER BY seq",
            (),
        )
        .await
        .map_err(failed("reading the queue"))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed("reading the queue"))? {
        out.push(QueuedRun {
            run_id: row.get::<String>(0).unwrap_or_default(),
            seq: row.get::<i64>(1).unwrap_or_default(),
            state: match row.get::<String>(2).unwrap_or_default().as_str() {
                "running" => QueueState::Running,
                "draining" => QueueState::Draining,
                _ => QueueState::Queued,
            },
            spec: row.get::<String>(3).unwrap_or_default(),
            queued_at: row.get::<String>(4).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Mark `run_id` started. A run the drain already marked `draining` (it was leaving the queue
/// as the drain began) stays so.
pub async fn mark_running(db_path: &str, run_id: &str) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    ensure(&conn).await?;
    // No writer has started yet. A killed attempt's spool was committed by requeue;
    // remove it here as well so a crash immediately after that commit is recoverable.
    let spool = barca_db::run_logs::spool_path(db_path, run_id);
    match std::fs::remove_file(spool) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    db::run_queue::mark_running(&conn, run_id)
        .await
        .map_err(failed("marking a run"))?;
    Ok(())
}

/// Mark the runs in flight `draining`: the server was told to stop (#628). One that is still
/// so when the next server starts was killed during the drain.
pub async fn mark_draining(db_path: &str, run_ids: &[String]) -> Result<(), BarcaError> {
    if run_ids.is_empty() {
        return Ok(());
    }
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    ensure(&conn).await?;
    db::with_tx(&conn, "BEGIN IMMEDIATE", "run queue drain", async {
        for run_id in run_ids {
            db::run_queue::mark_draining(&conn, run_id)
                .await
                .map_err(failed("marking a run draining"))?;
        }
        Ok(())
    })
    .await
}

/// Take `run_id` off the queue: it finished (or was cancelled before it started).
pub async fn remove(db_path: &str, run_id: &str) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    if !has_table(&conn, "run_queue").await? {
        return Ok(());
    }
    db::run_queue::delete(&conn, run_id)
        .await
        .map_err(failed("removing a run"))?;
    Ok(())
}

/// Put a started run back in the queue, in its place, to start again from the beginning under
/// the same run id: the stopping server stopped it. The stopped execution and its logs and
/// materializations become a linked attempt record, preserving all diagnostics and cache
/// timestamps. The queue and archive change in one transaction.
///
/// `spec`, when given, replaces the row's description (the server counts there how often a
/// run was put back after a drain was killed).
pub async fn requeue(db_path: &str, run_id: &str, spec: Option<&str>) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    ensure(&conn).await?;
    db::with_tx(&conn, "BEGIN IMMEDIATE", "run requeue", async {
        db::run_queue::mark_queued(&conn, run_id)
            .await
            .map_err(failed("requeueing a run"))?;
        if let Some(spec) = spec {
            db::run_queue::set_spec(&conn, run_id, spec)
                .await
                .map_err(failed("requeueing a run"))?;
        }
        archive_attempt(&conn, db_path, run_id).await?;
        Ok(())
    })
    .await
}

/// Keep a stopped execution under an inspectable id. Updating the existing row preserves
/// every diagnostic column (including columns introduced by later additive migrations).
async fn archive_attempt(conn: &Connection, db_path: &str, run_id: &str) -> Result<(), BarcaError> {
    let mut rows = conn
        .query("SELECT 1 FROM runs WHERE run_id = ?1", [run_id])
        .await
        .map_err(failed("reading stopped execution"))?;
    if rows
        .next()
        .await
        .map_err(failed("reading stopped execution"))?
        .is_none()
    {
        return Ok(()); // A repeated recovery, or an execution that never recorded itself.
    }
    drop(rows);
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(run_attempt), 0) + 1 FROM runs WHERE parent_run_id = ?1",
            [run_id],
        )
        .await
        .map_err(failed("numbering attempt"))?;
    let attempt: i64 = rows
        .next()
        .await
        .map_err(failed("numbering attempt"))?
        .unwrap()
        .get(0)
        .unwrap();
    drop(rows);
    let archived = format!("{run_id}-attempt-{attempt}");
    // A killed coordinator has only a spool. Commit its complete lines before the next
    // execution can truncate that spool. Already-persisted logs are authoritative.
    let mut rows = conn
        .query("SELECT 1 FROM logs WHERE run_id = ?1 LIMIT 1", [run_id])
        .await
        .map_err(failed("reading attempt logs"))?;
    let recorded = rows
        .next()
        .await
        .map_err(failed("reading attempt logs"))?
        .is_some();
    drop(rows);
    if !recorded {
        let (lines, _) =
            barca_db::run_logs::read_spool(&barca_db::run_logs::spool_path(db_path, run_id), 0)?;
        for (seq, line) in lines.into_iter().enumerate() {
            conn.execute("INSERT INTO logs (run_id, seq, node_id, line, stream, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                libsql::params![run_id, seq as i64, line.node_id, line.line, line.stream.as_str(), line.ts]).await.map_err(failed("archiving spool"))?;
        }
    }
    conn.execute("UPDATE runs SET run_id = ?1, parent_run_id = ?2, run_attempt = ?3, stop_reason = 'server restart (requeued)', status = 'interrupted', finished_at = COALESCE(finished_at, datetime('now')) WHERE run_id = ?2",
        libsql::params![archived.clone(), run_id, attempt]).await.map_err(failed("archiving stopped execution"))?;
    for table in ["logs", "materializations"] {
        conn.execute(
            &format!("UPDATE {table} SET run_id = ?1 WHERE run_id = ?2"),
            [archived.as_str(), run_id],
        )
        .await
        .map_err(failed("archiving attempt evidence"))?;
    }
    Ok(())
}

/// The predecessor's queue ownership is authoritative even when its host/PID cannot be
/// checked here. Never overwrite a completed outcome. Remove the queue row atomically.
pub async fn interrupt_predecessor(db_path: &str, run_id: &str) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    db::with_tx(&conn, "BEGIN IMMEDIATE", "interrupt predecessor", async {
        conn.execute("UPDATE runs SET status = 'interrupted', stop_reason = 'server stopped before completion', finished_at = COALESCE(finished_at, datetime('now')) WHERE run_id = ?1 AND status = 'running'", [run_id]).await.map_err(failed("interrupting predecessor"))?;
        db::run_queue::delete(&conn, run_id).await.map_err(failed("removing predecessor"))?;
        Ok(())
    }).await
}

/// When the queue was paused (unix seconds), if it is.
pub async fn paused_at(db_path: &str) -> Result<Option<i64>, BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    if !has_table(&conn, "server_state").await? {
        return Ok(None);
    }
    let mut rows = conn
        .query("SELECT value FROM server_state WHERE key = ?1", [PAUSED_AT])
        .await
        .map_err(failed("reading the pause"))?;
    let value = rows
        .next()
        .await
        .map_err(failed("reading the pause"))?
        .and_then(|row| row.get::<String>(0).ok())
        .and_then(|v| v.parse::<i64>().ok());
    while rows
        .next()
        .await
        .map_err(failed("reading the pause"))?
        .is_some()
    {}
    Ok(value)
}

/// Pause the queue as of `at` (`Some`), or resume it (`None`).
pub async fn set_paused(db_path: &str, at: Option<i64>) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    ensure(&conn).await?;
    match at {
        Some(at) => db::run_queue::set_state(&conn, PAUSED_AT, &at.to_string())
            .await
            .map_err(failed("pausing"))?,
        None => db::run_queue::clear_state(&conn, PAUSED_AT)
            .await
            .map_err(failed("resuming"))?,
    };
    Ok(())
}

/// The `server_state` entry `key`, when there is one: what a server recorded of itself
/// ([`barca_model::liveness`]). A database without the table has none.
pub async fn state_value(db_path: &str, key: &str) -> Result<Option<String>, BarcaError> {
    if std::fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(None);
    }
    let _g = db::db_guard_for(db_path).await;
    let (_db, conn) = db::open_conn(db_path).await?;
    if !has_table(&conn, "server_state").await? {
        return Ok(None);
    }
    let mut rows = conn
        .query("SELECT value FROM server_state WHERE key = ?1", [key])
        .await
        .map_err(failed("reading the server's record"))?;
    let value = rows
        .next()
        .await
        .map_err(failed("reading the server's record"))?
        .and_then(|row| row.get::<String>(0).ok());
    while rows
        .next()
        .await
        .map_err(failed("reading the server's record"))?
        .is_some()
    {}
    Ok(value)
}

/// Start and most recent step completion of the queue's executing runs, in Unix seconds.
pub async fn progress(
    db_path: &str,
) -> Result<std::collections::HashMap<String, (i64, Option<i64>)>, BarcaError> {
    let _guard = db::db_guard_for(db_path).await;
    let (_db, conn) = db::open_conn(db_path).await?;
    let mut rows = conn.query(
        "SELECT q.run_id, unixepoch(r.started_at),
                (SELECT unixepoch(MAX(m.created_at)) FROM materializations m WHERE m.run_id = q.run_id)
         FROM run_queue q JOIN runs r ON r.run_id = q.run_id WHERE q.state != 'queued'", ())
        .await.map_err(failed("reading run progress"))?;
    let mut progress = std::collections::HashMap::new();
    while let Some(row) = rows.next().await.map_err(failed("reading run progress"))? {
        if let (Ok(id), Ok(at)) = (row.get::<String>(0), row.get::<i64>(1)) {
            progress.insert(id, (at, row.get::<i64>(2).ok()));
        }
    }
    Ok(progress)
}

/// Set the `server_state` entry `key` to `value`.
pub async fn set_state_value(db_path: &str, key: &str, value: &str) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    ensure(&conn).await?;
    db::run_queue::set_state(&conn, key, value)
        .await
        .map_err(failed("recording the server"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        db::init_db(&path).await.unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn runs_keep_their_order_and_state_across_reads() {
        let (_dir, path) = fresh().await;
        assert!(list(&path).await.unwrap().is_empty());
        for (n, id) in ["a", "b", "c"].iter().enumerate() {
            let seq = push(&path, id, &format!("{{\"n\":{n}}}"), "2026-10-22T10:00:00Z")
                .await
                .unwrap();
            assert_eq!(seq, n as i64 + 1);
        }
        mark_running(&path, "a").await.unwrap();
        let rows = list(&path).await.unwrap();
        let ids: Vec<_> = rows.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(rows[0].state, QueueState::Running);
        assert_eq!(rows[1].state, QueueState::Queued);
        assert_eq!(rows[2].spec, "{\"n\":2}");
        remove(&path, "b").await.unwrap();
        // A run added after a removal still goes last.
        assert_eq!(push(&path, "d", "{}", "t").await.unwrap(), 4);
        let ids: Vec<_> = list(&path)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.run_id)
            .collect();
        assert_eq!(ids, ["a", "c", "d"]);
    }

    #[tokio::test]
    async fn a_drain_marks_runs_in_flight_and_a_late_start_does_not_unmark_them() {
        let (_dir, path) = fresh().await;
        push(&path, "a", "{}", "t").await.unwrap();
        push(&path, "b", "{}", "t").await.unwrap();
        mark_running(&path, "a").await.unwrap();
        // b left the queue as the drain began: marked before it marks itself running.
        mark_draining(&path, &["a".into(), "b".into()])
            .await
            .unwrap();
        mark_running(&path, "b").await.unwrap();
        let states: Vec<_> = list(&path).await.unwrap().iter().map(|r| r.state).collect();
        assert_eq!(states, [QueueState::Draining, QueueState::Draining]);
    }

    #[tokio::test]
    async fn a_requeued_run_keeps_its_place_and_preserves_its_stopped_attempt() {
        let (_dir, path) = fresh().await;
        push(&path, "first", "{}", "t").await.unwrap();
        push(&path, "second", "{}", "t").await.unwrap();
        mark_running(&path, "first").await.unwrap();
        db::create_run(&path, "first", "get", "[]", Some("slow"), Some(1))
            .await
            .unwrap();
        db::insert_logs(
            &path,
            "first",
            &[barca_db::run_logs::RecordedLine {
                node_id: "p.py:slow".into(),
                line: "attempt one".into(),
                stream: barca_model::LogStream::Stdout,
                ts: None,
            }],
        )
        .await
        .unwrap();
        {
            let (_db, conn) = db::open_conn(&path).await.unwrap();
            conn.execute("ALTER TABLE runs ADD COLUMN worker_failures TEXT", ())
                .await
                .unwrap();
            conn.execute(r#"UPDATE runs SET worker_failures = '[{"node":"bad"}]', plan_reasons = '{"test":1}', cancellation = '{"started":true}' WHERE run_id = 'first'"#, ()).await.unwrap();
            conn.execute("INSERT INTO materializations (node_id, run_id, run_hash, status, created_at) VALUES ('p.py:done', 'first', 'hash', 'success', '2026-11-22T12:00:00.123Z')", ()).await.unwrap();
        }
        requeue(&path, "first", Some("{\"again\":1}"))
            .await
            .unwrap();
        let rows = list(&path).await.unwrap();
        assert_eq!(rows[0].run_id, "first");
        assert_eq!(rows[0].state, QueueState::Queued);
        assert_eq!(rows[0].spec, "{\"again\":1}");
        assert!(db::get_run(&path, "first").await.unwrap().is_none());
        assert!(
            !db::get_logs(&path, "first-attempt-1")
                .await
                .unwrap()
                .is_empty()
        );
        // Retry after a crash between archive commit and startup is idempotent.
        requeue(&path, "first", None).await.unwrap();
        assert_eq!(
            db::runs::previous_attempts(&path, "first")
                .await
                .unwrap()
                .len(),
            1
        );
        let archived = db::get_run(&path, "first-attempt-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(archived.parent_run_id.as_deref(), Some("first"));
        assert_eq!(
            archived.stop_reason.as_deref(),
            Some("server restart (requeued)")
        );
        assert_eq!(archived.status, "interrupted");
        assert_eq!(db::count_runs(&path).await.unwrap(), 0);
        {
            let (_db, conn) = db::open_conn(&path).await.unwrap();
            let mut rows = conn.query("SELECT worker_failures, plan_reasons, cancellation FROM runs WHERE run_id = 'first-attempt-1'", ()).await.unwrap();
            let row = rows.next().await.unwrap().unwrap();
            assert_eq!(row.get::<String>(0).unwrap(), r#"[{"node":"bad"}]"#);
            assert_eq!(row.get::<String>(1).unwrap(), r#"{"test":1}"#);
            assert_eq!(row.get::<String>(2).unwrap(), r#"{"started":true}"#);
            drop(rows);
            let mut rows = conn
                .query(
                    "SELECT run_id, run_hash, created_at FROM materializations",
                    (),
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            assert_eq!(row.get::<String>(0).unwrap(), "first-attempt-1");
            assert_eq!(row.get::<String>(1).unwrap(), "hash");
            assert_eq!(row.get::<String>(2).unwrap(), "2026-11-22T12:00:00.123Z");
        }
        db::create_run(&path, "first", "get", "[]", None, Some(1))
            .await
            .unwrap();
        requeue(&path, "first", None).await.unwrap();
        assert_eq!(
            db::runs::previous_attempts(&path, "first")
                .await
                .unwrap()
                .len(),
            2
        );
        db::create_run(&path, "first", "get", "[]", None, Some(1))
            .await
            .unwrap();
        assert_eq!(db::count_runs(&path).await.unwrap(), 1);
        assert_eq!(db::get_recent_runs(&path, 10).await.unwrap().len(), 1);
        assert!(db::get_run_steps(&path, "first").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_pause_is_kept_until_resumed_and_a_database_without_the_tables_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db").display().to_string();
        // A database from before the tables: nothing queued, not paused.
        {
            let db = libsql::Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute("CREATE TABLE runs (run_id TEXT)", ())
                .await
                .unwrap();
        }
        assert!(list(&path).await.unwrap().is_empty());
        assert_eq!(paused_at(&path).await.unwrap(), None);
        remove(&path, "x").await.unwrap();
        let (_dir, path) = fresh().await;
        set_paused(&path, Some(1_000)).await.unwrap();
        assert_eq!(paused_at(&path).await.unwrap(), Some(1_000));
        set_paused(&path, Some(2_000)).await.unwrap();
        assert_eq!(paused_at(&path).await.unwrap(), Some(2_000));
        set_paused(&path, None).await.unwrap();
        assert_eq!(paused_at(&path).await.unwrap(), None);
    }
}
