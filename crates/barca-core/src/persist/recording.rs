//! Durable result history: which rows a run's end, a verified ledger post and the in-run
//! recorder write, in whose transaction, and when a row is not written again. The statements
//! are `crate::db`'s ([`db::materializations`], [`db::runs`]); nothing here spells SQL that
//! writes. Recording successful history is distinct from selecting the current observation:
//! these operations do not establish concurrent publication ordering or stable input-byte
//! lifetime.

use super::RunLedger;
use crate::db::materializations::{FailedStepInsert, NewMaterialization, StepInsert};
use crate::ledger::{LedgerPost, PostAccepted};
use crate::step_status::StepStatus;
use crate::{BarcaError, db};
use std::collections::HashSet;

/// The steps of `run_id` already recorded, by node id and run hash: an earlier attempt of a
/// requeued run may have recorded a node under a run hash this attempt no longer computes.
async fn steps_of_run(
    conn: &libsql::Connection,
    run_id: &str,
) -> Result<HashSet<(String, String)>, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read recorded steps: {e}"));
    let mut out = HashSet::new();
    let mut rows = conn
        .query(
            "SELECT node_id, COALESCE(run_hash, '') FROM materializations WHERE run_id = ?1",
            [run_id.to_string()],
        )
        .await
        .map_err(failed)?;
    while let Some(row) = rows.next().await.map_err(failed)? {
        out.insert((
            row.get::<String>(0).map_err(failed)?,
            row.get::<String>(1).map_err(failed)?,
        ));
    }
    Ok(out)
}

/// Required outcome rows and terminal status, inside the caller's transaction.
pub(super) async fn write_terminal_ledger(
    conn: &libsql::Connection,
    l: &RunLedger<'_>,
) -> Result<(), BarcaError> {
    db::runs::insert_run_if_absent(conn, l.run_id, l.command, &l.files, l.target, l.steps_total)
        .await
        .map_err(|e| BarcaError::Db(format!("failed to create terminal run: {e}")))?;
    // What the [`StepRecorder`] wrote during the run.
    let already = steps_of_run(conn, l.run_id).await?;

    let mut insert = None;
    for (node_id, oref) in l.all_outputs {
        if l.cached_node_ids.contains(node_id) {
            continue;
        }
        let Some(run_h) = l.run_hashes.get(node_id) else {
            continue;
        };
        if already.contains(&(node_id.clone(), run_h.clone())) {
            continue;
        }
        let base = crate::StepId::parse(node_id).base_id().to_string();
        let (cpu, rss) = l.all_timings.get(node_id).copied().unwrap_or((None, None));
        let mut row =
            NewMaterialization::from_artifact(node_id, run_h, &serde_json::Value::Null, 1);
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
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        if already.contains(&(node_id.clone(), run_h.clone())) {
            continue;
        }
        let failed = |e| BarcaError::Db(format!("failed to record failed step: {e}"));
        let insert = match &mut insert_failed {
            Some(insert) => insert,
            None => insert_failed.insert(FailedStepInsert::prepare(conn).await.map_err(failed)?),
        };
        let batch = l.batches.get(node_id).cloned().unwrap_or_default();
        insert
            .run(failure, run_h, l.run_id, batch)
            .await
            .map_err(failed)?;
    }
    drop(insert_failed);
    crate::sink_publications::write_in(conn, l.run_id, l.sink_publications).await?;
    let updated = db::runs::finish_run_in(
        conn,
        l.run_id,
        l.status,
        l.steps_executed,
        l.steps_cached,
        l.elapsed,
        l.job,
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
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    let bytes_changed = db::with_tx(&conn, "BEGIN IMMEDIATE", "ledger post", async {
        db::runs::insert_posted_run(&conn, &run_id, &post.run)
            .await
            .map_err(failed)?;
        let mut bytes_changed = Vec::new();
        for step in &post.steps {
            if StepStatus::Success.is(Some(&step.status)) {
                let held = db::materializations::held_hashes(&conn, &step.node_id, &step.run_hash)
                    .await
                    .map_err(failed)?;
                match held.as_ref().map(|h| h.output.as_deref()) {
                    // Held, with the same bytes: nothing to add.
                    Some(Some(digest)) if Some(digest) == step.digest.as_deref() => {
                        if step.observation_hash.is_none()
                            || held
                                .as_ref()
                                .is_some_and(|h| h.observation == step.observation_hash)
                        {
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
            db::materializations::insert_posted_step(&conn, &run_id, step)
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
