//! Reads and writes of the scheduler's durable state: `schedule_state` (the last tick each
//! scheduled node fired, and its pause) and `schedule_failures` (its failure streak).

use super::*;

/// Load the last-fired time (unix epoch seconds) for every scheduled node.
pub async fn get_schedule_state(db_path: &str) -> Result<HashMap<String, i64>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query("SELECT node_id, last_fired_at FROM schedule_state", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query schedule_state: {e}")))?;
    let mut out = HashMap::new();
    while let Ok(Some(row)) = rows.next().await {
        let node_id = row.get::<String>(0).unwrap_or_default();
        let last = row.get::<i64>(1).unwrap_or_default();
        if !node_id.is_empty() {
            out.insert(node_id, last);
        }
    }
    Ok(out)
}

/// Record that a scheduled node fired at `epoch_secs` (unix epoch seconds).
pub async fn upsert_schedule_state(
    db_path: &str,
    node_id: &str,
    epoch_secs: i64,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "INSERT INTO schedule_state (node_id, last_fired_at) VALUES (?1, ?2)
         ON CONFLICT(node_id) DO UPDATE SET last_fired_at = ?2",
        [node_id.to_string(), epoch_secs.to_string()],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to upsert schedule_state: {e}")))?;
    Ok(())
}

/// A schedule held back by `POST /schedule/{id}/pause`: since when, and until when (`None`:
/// until it is resumed). Unix epoch seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulePause {
    pub paused_at: i64,
    pub until: Option<i64>,
}

/// Every paused schedule, by node id. Expired pauses are included; the caller compares
/// `until` with its clock.
pub async fn get_schedule_pauses(
    db_path: &str,
) -> Result<HashMap<String, SchedulePause>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_column(&conn, "schedule_state", "paused_at").await? {
        return Ok(HashMap::new());
    }
    let mut rows = conn
        .query(
            "SELECT node_id, paused_at, paused_until FROM schedule_state WHERE paused_at IS NOT NULL",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query schedule pauses: {e}")))?;
    let mut out = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read schedule pause: {e}")))?
    {
        let node_id = row.get::<String>(0).unwrap_or_default();
        if let Ok(paused_at) = row.get::<i64>(1) {
            out.insert(
                node_id,
                SchedulePause {
                    paused_at,
                    until: row.get::<i64>(2).ok(),
                },
            );
        }
    }
    Ok(out)
}

/// Pause `node_id`'s schedule (`Some`) or resume it (`None`). A schedule with no row yet gets
/// one anchored at `now`, as the scheduler anchors a new job. Resuming moves the last-fired
/// record to `now`, so the ticks skipped while paused are not caught up at the next start.
pub async fn set_schedule_pause(
    db_path: &str,
    node_id: &str,
    pause: Option<SchedulePause>,
    now: i64,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let opt = |v: Option<i64>| v.map(|v| v.to_string()).unwrap_or_default();
    let sql = match pause {
        Some(_) => {
            "INSERT INTO schedule_state (node_id, last_fired_at, paused_at, paused_until) VALUES (?1, ?2, NULLIF(?3, ''), NULLIF(?4, ''))
             ON CONFLICT(node_id) DO UPDATE SET paused_at = NULLIF(?3, ''), paused_until = NULLIF(?4, '')"
        }
        None => {
            "INSERT INTO schedule_state (node_id, last_fired_at, paused_at, paused_until) VALUES (?1, ?2, NULLIF(?3, ''), NULLIF(?4, ''))
             ON CONFLICT(node_id) DO UPDATE SET paused_at = NULL, paused_until = NULL, last_fired_at = MAX(last_fired_at, ?2)"
        }
    };
    conn.execute(
        sql,
        [
            node_id.to_string(),
            now.to_string(),
            opt(pause.map(|p| p.paused_at)),
            opt(pause.and_then(|p| p.until)),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to record schedule pause: {e}")))?;
    Ok(())
}

/// Consecutive failed ticks of one scheduled node (`schedule_failures`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleFailures {
    /// Consecutive ticks whose run failed.
    pub failures: u32,
    /// The nodes that failed in the last counted tick.
    pub failed_nodes: Vec<String>,
    /// The highest materialization id when the last failure was counted.
    pub mark: i64,
}

/// Consecutive failed ticks for every scheduled node that has any.
pub async fn get_schedule_failures(
    db_path: &str,
) -> Result<HashMap<String, ScheduleFailures>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT node_id, failures, failed_nodes, mark FROM schedule_failures",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query schedule_failures: {e}")))?;
    let mut out = HashMap::new();
    while let Ok(Some(row)) = rows.next().await {
        let node_id = row.get::<String>(0).unwrap_or_default();
        if node_id.is_empty() {
            continue;
        }
        out.insert(
            node_id,
            ScheduleFailures {
                failures: u32::try_from(row.get::<i64>(1).unwrap_or_default()).unwrap_or(0),
                failed_nodes: row
                    .get::<String>(2)
                    .ok()
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
                mark: row.get::<i64>(3).unwrap_or_default(),
            },
        );
    }
    Ok(out)
}

/// Record `node_id`'s consecutive failed ticks; zero failures removes its row.
pub async fn set_schedule_failures(
    db_path: &str,
    node_id: &str,
    state: &ScheduleFailures,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failed = |e| BarcaError::Db(format!("failed to write schedule_failures: {e}"));
    if state.failures == 0 {
        conn.execute(
            "DELETE FROM schedule_failures WHERE node_id = ?1",
            [node_id.to_string()],
        )
        .await
        .map_err(failed)?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO schedule_failures (node_id, failures, failed_nodes, mark)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(node_id) DO UPDATE SET failures = ?2, failed_nodes = ?3, mark = ?4",
        [
            node_id.to_string(),
            state.failures.to_string(),
            serde_json::to_string(&state.failed_nodes).unwrap_or_else(|_| "[]".into()),
            state.mark.to_string(),
        ],
    )
    .await
    .map_err(failed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn schedule_state_round_trips_and_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // Empty to start.
        assert!(get_schedule_state(&db_path).await.unwrap().is_empty());

        upsert_schedule_state(&db_path, "test.py:daily", 1_000)
            .await
            .unwrap();
        upsert_schedule_state(&db_path, "test.py:poll", 2_000)
            .await
            .unwrap();
        let state = get_schedule_state(&db_path).await.unwrap();
        assert_eq!(state.get("test.py:daily"), Some(&1_000));
        assert_eq!(state.get("test.py:poll"), Some(&2_000));

        // Upsert overwrites the same node rather than inserting a duplicate.
        upsert_schedule_state(&db_path, "test.py:daily", 3_000)
            .await
            .unwrap();
        let state = get_schedule_state(&db_path).await.unwrap();
        assert_eq!(state.len(), 2);
        assert_eq!(state.get("test.py:daily"), Some(&3_000));
    }

    #[tokio::test]
    async fn a_pause_is_kept_and_a_resume_moves_the_last_fired_record() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        upsert_schedule_state(&db_path, "test.py:daily", 1_000)
            .await
            .unwrap();
        let pause = SchedulePause {
            paused_at: 1_500,
            until: Some(9_000),
        };
        set_schedule_pause(&db_path, "test.py:daily", Some(pause), 1_500)
            .await
            .unwrap();
        // A job that never fired gets a row anchored at the pause.
        let forever = SchedulePause {
            paused_at: 1_600,
            until: None,
        };
        set_schedule_pause(&db_path, "test.py:new", Some(forever), 1_600)
            .await
            .unwrap();
        let pauses = get_schedule_pauses(&db_path).await.unwrap();
        assert_eq!(pauses["test.py:daily"], pause);
        assert_eq!(pauses["test.py:new"], forever);
        let state = get_schedule_state(&db_path).await.unwrap();
        assert_eq!(state["test.py:daily"], 1_000, "pausing does not fire");
        assert_eq!(state["test.py:new"], 1_600);
        set_schedule_pause(&db_path, "test.py:daily", None, 2_000)
            .await
            .unwrap();
        assert!(
            !get_schedule_pauses(&db_path)
                .await
                .unwrap()
                .contains_key("test.py:daily")
        );
        assert_eq!(
            get_schedule_state(&db_path).await.unwrap()["test.py:daily"],
            2_000,
            "the skipped ticks are not caught up"
        );
    }
}
