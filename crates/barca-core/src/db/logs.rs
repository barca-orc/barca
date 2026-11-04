//! Reads and writes of `logs`, the output lines a run recorded (one row per line).
//! Deleting a run's lines is in [`super::runs`].

use super::*;
use serde::{Deserialize, Serialize};

/// One recorded output line of a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LogEntry {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
    pub line: String,
    /// `stdout` or `stderr`; lines recorded before barca captured stderr are `stdout`.
    #[serde(default)]
    pub stream: crate::LogStream,
    /// When the line arrived (RFC 3339, UTC, milliseconds); null on lines recorded before
    /// barca stamped them.
    #[serde(default)]
    pub ts: Option<String>,
}

impl LogEntry {
    /// The entry at `seq` of a run's recorded lines.
    pub fn at(seq: usize, line: crate::run_logs::RecordedLine) -> Self {
        Self {
            node_id: line.node_id,
            seq: seq as i64,
            line: line.line,
            stream: line.stream,
            ts: line.ts,
        }
    }
}

/// Persist a run's recorded lines, in order. Idempotent: a run that already has lines here is
/// left alone.
pub async fn insert_logs(
    db_path: &str,
    run_id: &str,
    lines: &[crate::run_logs::RecordedLine],
) -> Result<(), BarcaError> {
    if lines.is_empty() {
        return Ok(());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    // Store the entire ordered stream atomically. A failed write must not leave
    // a partial stream that the idempotence check would mistake for a complete one.
    let mut rows = conn
        .query("SELECT 1 FROM logs WHERE run_id = ?1 LIMIT 1", [run_id])
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect recorded logs: {e}")))?;
    if rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read recorded logs: {e}")))?
        .is_some()
    {
        return Ok(());
    }
    drop(rows);
    with_tx(&conn, "BEGIN", "log write", async {
        // Many rows per statement: one statement per row cost ~9µs a line, which a step that
        // prints 100k lines felt.
        for (chunk_no, chunk) in lines.chunks(LOG_INSERT_ROWS).enumerate() {
            let marks = (0..chunk.len())
                .map(|i| {
                    let p = i * 6;
                    format!(
                        "(?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                        p + 1,
                        p + 2,
                        p + 3,
                        p + 4,
                        p + 5,
                        p + 6
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() * 6);
            for (i, line) in chunk.iter().enumerate() {
                let seq = (chunk_no * LOG_INSERT_ROWS + i) as i64;
                params.push(libsql::Value::Text(run_id.to_string()));
                params.push(libsql::Value::Text(line.node_id.clone()));
                params.push(libsql::Value::Integer(seq));
                params.push(libsql::Value::Text(line.line.clone()));
                params.push(libsql::Value::Text(line.stream.as_str().to_string()));
                params.push(match &line.ts {
                    Some(ts) => libsql::Value::Text(ts.clone()),
                    None => libsql::Value::Null,
                });
            }
            conn.execute(
                &format!(
                    "INSERT INTO logs (run_id, node_id, seq, line, stream, ts) VALUES {marks}"
                ),
                libsql::params_from_iter(params),
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to record output: {e}")))?;
        }
        Ok(())
    })
    .await
}

/// Rows per `INSERT` of a run's lines (6 parameters each, well under SQLite's limit).
const LOG_INSERT_ROWS: usize = 500;

/// Fetch all persisted log lines for a run, in order. An older database without the
/// `stream` and `ts` columns reads them as stdout lines with no time.
pub async fn get_logs(db_path: &str, run_id: &str) -> Result<Vec<LogEntry>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let full = conn
        .query(
            "SELECT node_id, seq, line, stream, ts FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
            [run_id.to_string()],
        )
        .await;
    let (mut rows, columns) = match full {
        Ok(rows) => (rows, 5),
        Err(_) => (
            conn.query(
                "SELECT node_id, seq, line FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
                [run_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query logs: {e}")))?,
            3,
        ),
    };
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read log row: {e}")))?
    {
        let stream = if columns == 5 {
            row.get::<String>(3)
                .ok()
                .and_then(|s| crate::LogStream::parse(&s))
                .unwrap_or_default()
        } else {
            crate::LogStream::Stdout
        };
        out.push(LogEntry {
            node_id: row.get::<String>(0).unwrap_or_default(),
            seq: row.get::<i64>(1).unwrap_or_default(),
            line: row.get::<String>(2).unwrap_or_default(),
            stream,
            ts: if columns == 5 {
                row.get::<String>(4).ok()
            } else {
                None
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn logs_round_trip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let lines = vec![
            test_line("a.py:load", "loading…"),
            test_line("a.py:load", "read 30/150"),
            crate::run_logs::RecordedLine {
                stream: crate::LogStream::Stderr,
                ts: Some("2026-10-22T10:00:00.123Z".into()),
                ..test_line("a.py:load", "done")
            },
        ];
        insert_logs(&db_path, "run123", &lines).await.unwrap();

        let got = get_logs(&db_path, "run123").await.unwrap();
        assert_eq!(got.len(), 3);
        // Order preserved via the seq column.
        assert_eq!(got[0].seq, 0);
        assert_eq!(got[0].line, "loading…");
        assert_eq!(got[1].line, "read 30/150");
        assert_eq!(got[2].line, "done");
        assert_eq!(got[0].node_id, "a.py:load");
        assert_eq!(got[0].stream, crate::LogStream::Stdout);
        assert_eq!(got[0].ts, None);
        assert_eq!(got[2].stream, crate::LogStream::Stderr);
        assert_eq!(got[2].ts.as_deref(), Some("2026-10-22T10:00:00.123Z"));
    }

    fn test_line(node: &str, line: &str) -> crate::run_logs::RecordedLine {
        crate::run_logs::RecordedLine {
            node_id: node.into(),
            line: line.into(),
            stream: crate::LogStream::Stdout,
            ts: None,
        }
    }

    #[tokio::test]
    async fn logs_are_scoped_by_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        insert_logs(&db_path, "runA", &[test_line("n", "a-line")])
            .await
            .unwrap();
        insert_logs(&db_path, "runB", &[test_line("n", "b-line")])
            .await
            .unwrap();

        let a = get_logs(&db_path, "runA").await.unwrap();
        let b = get_logs(&db_path, "runB").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].line, "a-line");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].line, "b-line");
        // Unknown run yields no rows, not an error.
        assert!(get_logs(&db_path, "nope").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn insert_empty_logs_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        insert_logs(&db_path, "run", &[]).await.unwrap();
        assert!(get_logs(&db_path, "run").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_log_write_failure_rolls_back_the_stream_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.db").to_string_lossy().to_string();
        init_db(&path).await.unwrap();
        {
            let _guard = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute("CREATE UNIQUE INDEX injected_log_failure ON logs(line)", ())
                .await
                .unwrap();
        }
        let lines = vec![test_line("p.py:a", "same"), test_line("p.py:a", "same")];
        assert!(insert_logs(&path, "r", &lines).await.is_err());
        assert!(get_logs(&path, "r").await.unwrap().is_empty());
        {
            let _guard = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute("DROP INDEX injected_log_failure", ())
                .await
                .unwrap();
        }
        insert_logs(&path, "r", &lines).await.unwrap();
        insert_logs(&path, "r", &lines).await.unwrap();
        assert_eq!(get_logs(&path, "r").await.unwrap().len(), 2);
    }
}
