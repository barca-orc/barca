//! Durable result history. Progress/terminal transactions and receipt eligibility remain
//! with their callers; verified-post and legacy writes retain their own connection lifetimes.
//! Recording successful history is distinct from selecting the current observation: these
//! operations do not establish concurrent publication ordering or stable input-byte lifetime.

use super::{RunLedger, StepRow};
use crate::ledger::{LedgerPost, PostAccepted};
use crate::{BarcaError, db, dispatch::OutputRef};
use std::collections::{HashMap, HashSet};

/// Appends successful steps' rows, unless the run already has a row for the node. Prepared once
/// per transaction and rebound for each row (#460).
pub(super) struct StepInsert(db::Prepared);

impl StepInsert {
    // Turso otherwise intersects the node and run indexes, scanning this run
    // for each partition. Bound deduplication to the node's own history.
    const SQL: &'static str = "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes, output_hash, bytes_changed, run_id, components, batch_id, observation_hash) SELECT ?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), COALESCE((SELECT output_hash FROM materializations INDEXED BY idx_mat_node_run WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1) != NULLIF(?11, ''), 0), ?12, NULLIF(?13, ''), NULLIF(?14, ''), NULLIF(?15, '') WHERE NOT EXISTS (SELECT 1 FROM materializations INDEXED BY idx_mat_node_run WHERE node_id = ?1 AND run_id = ?12)";

    pub(super) async fn prepare(conn: &libsql::Connection) -> Result<Self, libsql::Error> {
        db::prepare(conn, Self::SQL).await.map(Self)
    }

    /// Rows written: 1, or 0 when the run already recorded the node.
    pub(super) async fn run(&mut self, row: &StepRow, run_id: &str) -> Result<u64, libsql::Error> {
        let opt = |v: Option<String>| v.unwrap_or_default();
        self.0
            .execute([
                row.node_id.clone(),
                row.run_hash.clone(),
                row.path.clone(),
                row.format.clone(),
                row.size_bytes.to_string(),
                opt(row.elapsed_seconds.map(|e| e.to_string())),
                row.attempts.to_string(),
                opt(row.sinks_json.clone()),
                opt(row.cpu_seconds.map(|c| c.to_string())),
                opt(row.max_rss_bytes.map(|r| r.to_string())),
                opt(row.output_hash.clone()),
                run_id.to_string(),
                opt(row.components.clone()),
                opt(row.batch.clone()),
                opt(row.observation_hash.clone()),
            ])
            .await
    }
}

/// The steps of `run_id` already recorded, by node id.
async fn steps_of_run(
    conn: &libsql::Connection,
    run_id: &str,
) -> Result<HashSet<String>, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read recorded steps: {e}"));
    let mut out = HashSet::new();
    let mut rows = conn
        .query(
            "SELECT node_id FROM materializations WHERE run_id = ?1",
            [run_id.to_string()],
        )
        .await
        .map_err(failed)?;
    while let Some(row) = rows.next().await.map_err(failed)? {
        out.insert(row.get::<String>(0).map_err(failed)?);
    }
    Ok(out)
}

/// Required outcome rows and terminal status, inside the caller's transaction.
pub(super) async fn write_terminal_ledger(
    conn: &libsql::Connection,
    l: &RunLedger<'_>,
) -> Result<(), BarcaError> {
    conn.execute(
            "INSERT OR IGNORE INTO runs (run_id, command, files, target, status, steps_total, pid, host) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
            [
                l.run_id.to_string(),
                l.command.to_string(),
                l.files.clone(),
                l.target.unwrap_or("").to_string(),
                l.steps_total.to_string(),
                std::process::id().to_string(),
                db::local_host(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to create terminal run: {e}")))?;
    // What the [`StepRecorder`] wrote during the run.
    let already = steps_of_run(conn, l.run_id).await?;

    let mut insert = None;
    for (node_id, oref) in l.all_outputs {
        if l.cached_node_ids.contains(node_id) || already.contains(node_id) {
            continue;
        }
        let Some(run_h) = l.run_hashes.get(node_id) else {
            continue;
        };
        let base = crate::StepId::parse(node_id).base_id().to_string();
        let (cpu, rss) = l.all_timings.get(node_id).copied().unwrap_or((None, None));
        let mut row = StepRow::from_artifact(node_id, run_h, &serde_json::Value::Null, 1);
        row.path = l.store_paths.get(node_id).unwrap_or(&oref.path).clone();
        row.format = oref.format.clone();
        row.size_bytes = oref.size_bytes;
        row.elapsed_seconds = oref.elapsed_seconds;
        row.attempts = l
            .all_attempts
            .get(node_id)
            .or_else(|| l.all_attempts.get(&base))
            .copied()
            .unwrap_or(1);
        row.sinks_json = l.all_sinks.get(node_id).cloned();
        row.cpu_seconds = cpu;
        row.max_rss_bytes = rss;
        row.output_hash = oref.content_hash.clone();
        row.observation_hash = l.output_hashes.get(node_id).cloned();
        row.components = l.components.get(node_id).cloned();
        row.batch = l.batches.get(node_id).cloned();
        let failed = |e| BarcaError::Db(format!("failed to record terminal step: {e}"));
        let insert = match &mut insert {
            Some(insert) => insert,
            None => insert.insert(StepInsert::prepare(conn).await.map_err(failed)?),
        };
        insert.run(&row, l.run_id).await.map_err(failed)?;
    }
    drop(insert);

    // Persist permanently-failed steps as `status='failed'` rows (artifact
    // columns NULL). Failed rows are never served as cache hits.
    let mut insert_failed = None;
    for failure in l.all_failures {
        let node_id = &failure.node_id;
        if already.contains(node_id) {
            continue;
        }
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        let failed = |e| BarcaError::Db(format!("failed to record failed step: {e}"));
        let insert = match &mut insert_failed {
            Some(insert) => insert,
            None => insert_failed.insert(
                db::prepare(
                    conn,
                    "INSERT INTO materializations (node_id, run_hash, status, error_type, error_message, error_traceback, attempts, run_id, batch_id, error_phase) VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6, ?7, NULLIF(?8, ''), NULLIF(?9, ''))",
                )
                .await
                .map_err(failed)?,
            ),
        };
        // Each failure carries its own attempt count: dispatches for a worker
        // failure, transfer attempts for an upload failure.
        insert
            .execute([
                node_id.clone(),
                run_h,
                failure.error.error_type.clone(),
                failure.error.message.clone(),
                failure.error.traceback.clone(),
                failure.error.attempts.to_string(),
                l.run_id.to_string(),
                l.batches.get(node_id).cloned().unwrap_or_default(),
                failure.error.phase.map_or("", |p| p.as_str()).to_string(),
            ])
            .await
            .map_err(failed)?;
    }
    drop(insert_failed);
    let updated = conn.execute(
            "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, elapsed_seconds = ?4, finished_at = datetime('now'), job = NULLIF(?6, '') WHERE run_id = ?5",
            [
                l.status.to_string(),
                l.steps_executed.to_string(),
                l.steps_cached.to_string(),
                l.elapsed.to_string(),
                l.run_id.to_string(),
                l.job.to_string(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to finish run: {e}")))?;
    if updated != 1 {
        return Err(BarcaError::Db(
            "terminal run row was not written".to_string(),
        ));
    }
    Ok(())
}

/// Record a verified post in one transaction: the run, then each step the ledger does not
/// already hold. A step whose run hash the ledger holds with the same digest adds nothing; with
/// a different digest the ledger keeps its row and the step is reported in `bytes_changed`.
pub(crate) async fn record_post(
    db_path: &str,
    post: &LedgerPost,
) -> Result<PostAccepted, BarcaError> {
    let failed = |e: libsql::Error| BarcaError::Db(format!("failed to record a ledger post: {e}"));
    let run_id = db::generate_run_id();
    let run = &post.run;
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    let bytes_changed = db::with_tx(&conn, "BEGIN IMMEDIATE", "ledger post", async {
        conn.execute(
            "INSERT INTO runs (run_id, command, files, target, status, steps_total, \
             steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, host, \
             triggered_by, client_run_id) \
             VALUES (?1, ?2, ?3, NULLIF(?4, ''), ?5, NULLIF(?6, ''), ?7, ?8, ?9, NULLIF(?10, ''), \
             NULLIF(?11, ''), NULLIF(?12, ''), 'client', ?13)",
            [
                run_id.clone(),
                run.command.clone(),
                db::encode_files(&run.files),
                run.target.clone().unwrap_or_default(),
                run.status.clone(),
                run.steps_total.map(|n| n.to_string()).unwrap_or_default(),
                run.steps_executed.to_string(),
                run.steps_cached.to_string(),
                run.started_at.clone(),
                run.finished_at.clone().unwrap_or_default(),
                run.elapsed_seconds
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                run.host.clone().unwrap_or_default(),
                run.run_id.clone(),
            ],
        )
        .await
        .map_err(failed)?;
        let mut bytes_changed = Vec::new();
        for step in &post.steps {
            if step.status == "success" {
                let mut rows = conn
                    .query(
                        "SELECT output_hash, observation_hash FROM materializations WHERE node_id = ?1 AND \
                         run_hash = ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
                        [step.node_id.clone(), step.run_hash.clone()],
                    )
                    .await
                    .map_err(failed)?;
                let held = rows.next().await.map_err(failed)?;
                let held_observation = held.as_ref().and_then(|row| row.get::<String>(1).ok());
                let held = held.map(|row| row.get::<String>(0).ok().filter(|h| !h.is_empty()));
                while rows.next().await.map_err(failed)?.is_some() {}
                match held {
                    // Held, with the same bytes: nothing to add.
                    Some(Some(digest)) if Some(digest.as_str()) == step.digest.as_deref() => {
                        if step.observation_hash.is_none() || held_observation == step.observation_hash {
                            continue;
                        }
                    }
                    // Held with other bytes: the ledger's row stays the truth.
                    Some(Some(_)) => {
                        bytes_changed.push(step.node_id.clone());
                        continue;
                    }
                    // Held without a digest (recorded before digests were), or not held.
                    Some(None) | None => {}
                }
            }
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, \
                 artifact_format, artifact_size_bytes, elapsed_seconds, status, error_message, \
                 attempts, output_hash, run_id, observation_hash) \
                 VALUES (?1, ?2, NULLIF(?3, ''), NULLIF(?4, ''), NULLIF(?5, ''), NULLIF(?6, ''), \
                 ?7, NULLIF(?8, ''), ?9, NULLIF(?10, ''), ?11, NULLIF(?12, ''))",
                [
                    step.node_id.clone(),
                    step.run_hash.clone(),
                    step.path.clone().unwrap_or_default(),
                    step.format.clone().unwrap_or_default(),
                    step.size_bytes.map(|n| n.to_string()).unwrap_or_default(),
                    step.elapsed_seconds
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    step.status.clone(),
                    step.error.clone().unwrap_or_default(),
                    step.attempts.unwrap_or(1).to_string(),
                    step.digest.clone().unwrap_or_default(),
                    run_id.clone(),
                    step.observation_hash.clone().unwrap_or_default(),
                ],
            )
            .await
            .map_err(failed)?;
        }
        Ok::<_, BarcaError>(bytes_changed)
    })
    .await?;
    Ok(PostAccepted {
        run_id,
        bytes_changed,
    })
}

pub(crate) async fn record_legacy_outputs(
    db_path: &str,
    outputs: &HashMap<String, OutputRef>,
    run_hashes: &HashMap<String, String>,
) -> Result<(), BarcaError> {
    if outputs.is_empty() {
        return Ok(());
    }
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    for (node_id, oref) in outputs {
        let run_hash = run_hashes.get(node_id).cloned().unwrap_or_default();
        let elapsed_str = oref
            .elapsed_seconds
            .map(|e| e.to_string())
            .unwrap_or_default();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds) VALUES (?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''))",
            [
                node_id.clone(),
                run_hash,
                oref.path.clone(),
                oref.format.clone(),
                oref.size_bytes.to_string(),
                elapsed_str,
            ],
        )
        .await
        .ok();
    }
    Ok(())
}
