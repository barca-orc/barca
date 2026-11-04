//! Reads and writes of `cost_estimates`, the measured-cost EWMA per node ([`crate::persist`]
//! writes it inside the terminal ledger's transaction; a run reads it as it starts).

use super::*;
use crate::db;
use libsql::Connection;

/// Replaces a node's estimate. Prepared once and rebound for each node (#460).
pub(crate) struct UpsertEstimate(db::Prepared);

impl UpsertEstimate {
    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        db::prepare(
            conn,
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(node_id) DO UPDATE SET
             estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
             samples = ?6, updated_at = datetime('now')",
        )
        .await
        .map(Self)
    }

    pub(crate) async fn run(
        &mut self,
        node_id: &str,
        base_id: &str,
        est: &crate::cost::NodeEstimate,
    ) -> Result<u64, libsql::Error> {
        self.0
            .execute([
                node_id.to_string(),
                base_id.to_string(),
                est.estimate_seconds.to_string(),
                est.cpu_seconds.to_string(),
                est.max_rss_bytes.to_string(),
                est.samples.to_string(),
            ])
            .await
    }
}

/// Load every persisted per-node cost estimate (run start — seeds the
/// in-memory `CostModel` so batch sizing starts pre-warmed).
pub async fn load_cost_estimates(
    db_path: &str,
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    cost_estimates_on(&conn, None).await
}

pub(crate) async fn cost_estimates_for(
    conn: &libsql::Connection,
    ids: &[String],
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    cost_estimates_on(conn, Some(ids)).await
}

pub(super) async fn cost_estimates_on(
    conn: &libsql::Connection,
    ids: Option<&[String]>,
) -> Result<Vec<(String, crate::cost::NodeEstimate)>, BarcaError> {
    let columns =
        "SELECT node_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples FROM cost_estimates";
    let queries = match ids {
        None => vec![(columns.to_owned(), Vec::new())],
        Some(ids) => ids
            .chunks(IN_LIST_CHUNK)
            .map(|chunk| {
                let marks = (1..=chunk.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                (
                    format!("{columns} WHERE node_id IN ({marks})"),
                    chunk.to_vec(),
                )
            })
            .collect(),
    };
    let mut out = Vec::new();
    for (sql, params) in queries {
        let failed = |e| BarcaError::Db(format!("failed to query cost_estimates: {e}"));
        let mut rows = conn.query(&sql, params).await.map_err(failed)?;
        while let Some(row) = rows.next().await.map_err(failed)? {
            let node_id = row.get::<String>(0).unwrap_or_default();
            if node_id.is_empty() {
                continue;
            }
            out.push((
                node_id,
                crate::cost::NodeEstimate {
                    estimate_seconds: row.get::<f64>(1).unwrap_or(0.0),
                    cpu_seconds: row.get::<f64>(2).unwrap_or(0.0),
                    max_rss_bytes: row.get::<i64>(3).unwrap_or(0).max(0) as u64,
                    samples: row.get::<i64>(4).unwrap_or(0).max(0) as u64,
                },
            ));
        }
    }
    Ok(out)
}

/// Upsert per-node cost estimates (run end — persists the EWMA so the next
/// run skips the cold-start probe entirely).
pub async fn upsert_cost_estimates(
    db_path: &str,
    estimates: &[(String, crate::cost::NodeEstimate)],
) -> Result<(), BarcaError> {
    if estimates.is_empty() {
        return Ok(());
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    for (node_id, est) in estimates {
        let base = crate::StepId::parse(node_id).base_id().to_string();
        conn.execute(
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(node_id) DO UPDATE SET
                 estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
                 samples = ?6, updated_at = datetime('now')",
            [
                node_id.clone(),
                base,
                est.estimate_seconds.to_string(),
                est.cpu_seconds.to_string(),
                est.max_rss_bytes.to_string(),
                est.samples.to_string(),
            ],
        )
        .await
        .ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cost_estimates_round_trip_and_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        assert!(load_cost_estimates(&db_path).await.unwrap().is_empty());

        let est = |s: f64, n: u64| crate::cost::NodeEstimate {
            estimate_seconds: s,
            cpu_seconds: s * 0.9,
            max_rss_bytes: 1024,
            samples: n,
        };
        upsert_cost_estimates(
            &db_path,
            &[
                ("f.py:fetch[t=A]".to_string(), est(0.25, 3)),
                ("f.py:report".to_string(), est(2.0, 1)),
            ],
        )
        .await
        .unwrap();

        let loaded = load_cost_estimates(&db_path).await.unwrap();
        assert_eq!(loaded.len(), 2);
        let fetch = loaded.iter().find(|(n, _)| n == "f.py:fetch[t=A]").unwrap();
        assert!((fetch.1.estimate_seconds - 0.25).abs() < 1e-9);
        assert_eq!(fetch.1.samples, 3);
        assert_eq!(fetch.1.max_rss_bytes, 1024);

        // Upsert overwrites the same node rather than inserting a duplicate.
        upsert_cost_estimates(&db_path, &[("f.py:report".to_string(), est(1.5, 2))])
            .await
            .unwrap();
        let loaded = load_cost_estimates(&db_path).await.unwrap();
        assert_eq!(loaded.len(), 2);
        let report = loaded.iter().find(|(n, _)| n == "f.py:report").unwrap();
        assert!((report.1.estimate_seconds - 1.5).abs() < 1e-9);
    }
}
