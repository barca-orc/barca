//! Step timing and resource statistics read from `materializations`: a node's [`AssetStats`],
//! the typical durations `GET /state` reports ([`recent_durations`]) and the average step times
//! a run starts with ([`get_avg_elapsed`]).

use super::*;
use serde::{Deserialize, Serialize};

/// Aggregated statistics for a single asset node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetStats {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_runs: i64,
    pub avg_elapsed_seconds: Option<f64>,
    pub median_elapsed_seconds: Option<f64>,
    pub max_elapsed_seconds: Option<f64>,
    pub p95_elapsed_seconds: Option<f64>,
    /// The share of executions that repeated an earlier execution's run hash (not cache hits,
    /// which execute nothing and are not recorded per asset).
    pub cache_hit_rate: f64,
    /// The largest peak RSS any recorded execution of this node reached, in bytes (#627): the
    /// worker process's peak while the step ran. `null` when none was recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_rss_bytes: Option<i64>,
    /// The median of the recorded executions' peak RSS, in bytes. `null` when none was
    /// recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub median_max_rss_bytes: Option<i64>,
    pub recent_runs: Vec<AssetRunEntry>,
}

/// One materialization entry for asset stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AssetRunEntry {
    pub elapsed_seconds: Option<f64>,
    pub status: String,
    pub created_at: String,
    /// Error message for `status='failed'` rows (None for successes).
    pub error_message: Option<String>,
    /// Number of attempts made.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub attempts: i64,
    /// The run that recorded this execution (`barca history RUN_ID`); `null` on rows from
    /// before barca recorded it.
    #[serde(default)]
    pub run_id: Option<String>,
    /// CPU seconds the step used (user and system, the worker process's own).
    #[serde(default)]
    pub cpu_seconds: Option<f64>,
    /// The worker process's peak RSS while the step ran, in bytes; `null` when not recorded.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_rss_bytes: Option<i64>,
    /// A partitioned node's entry is one run: the keys it recorded, and how many failed.
    /// Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub keys: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub failed_keys: Option<i64>,
}

/// The elapsed seconds of each node's last `window` successful materializations, its partition
/// keys' rows folded into the node (newest first). What `GET /state` reports as typical
/// durations. Both the node's own rows and its keys' rows are read through the node id
/// index, so sparse or failed-only partition histories never scan unrelated nodes. Each
/// query returns at most `window` successes; merging them preserves global recency.
pub async fn recent_durations(
    db_path: &str,
    base_ids: &[String],
    window: usize,
) -> Result<HashMap<String, Vec<f64>>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_h, conn) = open_conn(db_path).await?;
    let mut out = HashMap::new();
    for base in base_ids {
        // (row id, elapsed)
        let mut found: Vec<(i64, f64)> = Vec::new();
        let mut rows = conn
            .query(
                concat!(
                    "SELECT id, elapsed_seconds FROM materializations ",
                    by_node_run!(),
                    " WHERE node_id = ?1 AND status = 'success' AND elapsed_seconds IS NOT NULL \
                     ORDER BY id DESC LIMIT ?2"
                ),
                vec![
                    libsql::Value::Text(base.clone()),
                    libsql::Value::Integer(window as i64),
                ],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query durations: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read durations: {e}")))?
        {
            if let (Ok(id), Ok(e)) = (row.get::<i64>(0), row.get::<f64>(1)) {
                found.push((id, e));
            }
        }
        drop(rows);
        let (lo, hi) = key_range(base);
        // Restrict the scan to this node's keys. Walking global row-id windows can
        // read the entire database once per sparse or failed-only partitioned node.
        let mut rows = conn
            .query(
                concat!(
                    "SELECT id, elapsed_seconds FROM materializations ",
                    by_node_run!(),
                    " WHERE node_id >= ?1 AND node_id < ?2 AND status = 'success' \
                     AND elapsed_seconds IS NOT NULL ORDER BY id DESC LIMIT ?3"
                ),
                libsql::params![lo, hi, window as i64],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query durations: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read durations: {e}")))?
        {
            if let (Ok(id), Ok(elapsed)) = (row.get::<i64>(0), row.get::<f64>(1)) {
                found.push((id, elapsed));
            }
        }
        if found.is_empty() {
            continue;
        }
        found.sort_unstable_by_key(|(id, _)| std::cmp::Reverse(*id));
        found.truncate(window);
        out.insert(base.clone(), found.into_iter().map(|(_, e)| e).collect());
    }
    Ok(out)
}

/// Get aggregated stats for a specific asset/node.
pub async fn get_asset_stats(db_path: &str, node_id: &str) -> Result<AssetStats, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    // A partitioned node's rows are its keys (`node[key]`): an execution of it is a run.
    if !node_id.contains('[') && has_key_rows(&conn, node_id).await? {
        return partitioned_stats(&conn, node_id).await;
    }

    // Fetch all elapsed times for percentile computation.
    let mut all_elapsed: Vec<f64> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT elapsed_seconds FROM materializations WHERE node_id = ?1 AND elapsed_seconds IS NOT NULL AND elapsed_seconds > 0 ORDER BY elapsed_seconds",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query elapsed: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            if let Ok(e) = row.get::<f64>(0) {
                all_elapsed.push(e);
            }
        }
    }

    let total_runs: i64;
    {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM materializations WHERE node_id = ?1",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query count: {e}")))?;
        total_runs = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);
    }

    let avg_elapsed = if all_elapsed.is_empty() {
        None
    } else {
        Some(all_elapsed.iter().sum::<f64>() / all_elapsed.len() as f64)
    };
    let median_elapsed = percentile(&all_elapsed, 50.0);
    let max_elapsed = all_elapsed.last().copied();
    let p95_elapsed = percentile(&all_elapsed, 95.0);

    // `cache_hit_rate` (the JSON key keeps its name): the share of this node's executions that
    // repeated an earlier execution's run hash, i.e. ran again with unchanged code and inputs
    // (`--refresh`, a retry after a failure, an evicted result). A cache hit executes nothing
    // and writes no row, so it is not counted here; `barca history` counts them per run (#496).
    let cache_hit_rate = if total_runs > 1 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) - COUNT(DISTINCT run_hash) FROM materializations WHERE node_id = ?1 AND run_hash IS NOT NULL AND run_hash != ''",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query repeated executions: {e}")))?;
        let cached_count: i64 = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            .map(|r| r.get::<i64>(0).unwrap_or(0))
            .unwrap_or(0);
        cached_count as f64 / total_runs as f64
    } else {
        0.0
    };

    // Peak RSS per execution, for the largest and the median (#627).
    let mut all_rss: Vec<i64> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT max_rss_bytes FROM materializations WHERE node_id = ?1 AND max_rss_bytes > 0 ORDER BY max_rss_bytes",
                [node_id.to_string()],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query peak memory: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            if let Ok(rss) = row.get::<i64>(0) {
                all_rss.push(rss);
            }
        }
    }
    let max_rss_bytes = all_rss.last().copied();
    let median_max_rss_bytes = (!all_rss.is_empty()).then(|| all_rss[(all_rss.len() - 1) / 2]);

    // Recent runs (last 10).
    let mut rows = conn
        .query(
            "SELECT elapsed_seconds, status, created_at, error_message, attempts, run_id, cpu_seconds, max_rss_bytes FROM materializations WHERE node_id = ?1 ORDER BY id DESC LIMIT 10",
            [node_id.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query recent runs: {e}")))?;
    let mut recent_runs = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        recent_runs.push(AssetRunEntry {
            elapsed_seconds: row.get::<f64>(0).ok(),
            status: row
                .get::<String>(1)
                .unwrap_or_else(|_| "success".to_string()),
            created_at: row.get::<String>(2).unwrap_or_default(),
            error_message: row.get::<String>(3).ok(),
            attempts: row.get::<i64>(4).unwrap_or(1),
            run_id: row.get::<String>(5).ok().filter(|id| !id.is_empty()),
            cpu_seconds: row.get::<f64>(6).ok(),
            max_rss_bytes: row.get::<i64>(7).ok().filter(|rss| *rss > 0),
            keys: None,
            failed_keys: None,
        });
    }

    Ok(AssetStats {
        node_id: node_id.to_string(),
        total_runs,
        avg_elapsed_seconds: avg_elapsed,
        median_elapsed_seconds: median_elapsed,
        max_elapsed_seconds: max_elapsed,
        p95_elapsed_seconds: p95_elapsed,
        cache_hit_rate,
        max_rss_bytes,
        median_max_rss_bytes,
        recent_runs,
    })
}

/// `WHERE` over the rows of a node's keys: `node[`..`node\\`, `\\` being the byte after `[`.
pub(super) const KEY_ROWS: &str = "node_id >= ?1 || '[' AND node_id < ?1 || '\\'";

async fn has_key_rows(conn: &libsql::Connection, node_id: &str) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            &format!("SELECT 1 FROM materializations WHERE {KEY_ROWS} LIMIT 1"),
            [node_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query partition keys: {e}")))?;
    Ok(rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read partition keys: {e}")))?
        .is_some())
}

/// [`AssetStats`] of a partitioned node: one execution per run, its time and CPU summed over
/// the run's keys and its memory the largest of them.
async fn partitioned_stats(
    conn: &libsql::Connection,
    node_id: &str,
) -> Result<AssetStats, BarcaError> {
    let failure = |e: libsql::Error| BarcaError::Db(format!("failed to query key history: {e}"));
    let mut rows = conn
        .query(
            &format!(
                "SELECT SUM(elapsed_seconds), MAX(created_at), MIN(error_message), MAX(attempts), MAX(NULLIF(run_id, '')), SUM(cpu_seconds), MAX(max_rss_bytes), COUNT(*), SUM(status = 'failed') \
                 FROM materializations WHERE {KEY_ROWS} \
                 GROUP BY COALESCE(NULLIF(run_id, ''), 'row:' || id) ORDER BY MAX(id) DESC"
            ),
            [node_id],
        )
        .await
        .map_err(failure)?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        let failed_keys = row.get::<i64>(8).unwrap_or(0);
        runs.push(AssetRunEntry {
            elapsed_seconds: row.get::<f64>(0).ok(),
            status: if failed_keys > 0 { "failed" } else { "success" }.to_string(),
            created_at: row.get::<String>(1).unwrap_or_default(),
            error_message: row.get::<String>(2).ok(),
            attempts: row.get::<i64>(3).unwrap_or(1),
            run_id: row.get::<String>(4).ok(),
            cpu_seconds: row.get::<f64>(5).ok(),
            max_rss_bytes: row.get::<i64>(6).ok().filter(|rss| *rss > 0),
            keys: row.get::<i64>(7).ok(),
            failed_keys: Some(failed_keys),
        });
    }
    drop(rows);
    let mut repeated = conn
        .query(
            &format!(
                "SELECT COUNT(*) - COUNT(DISTINCT node_id || ':' || run_hash), COUNT(*) FROM materializations WHERE {KEY_ROWS} AND run_hash IS NOT NULL AND run_hash != ''"
            ),
            [node_id],
        )
        .await
        .map_err(failure)?;
    let (repeats, executions) = match repeated.next().await.map_err(failure)? {
        Some(row) => (
            row.get::<i64>(0).unwrap_or(0),
            row.get::<i64>(1).unwrap_or(0),
        ),
        None => (0, 0),
    };
    let mut elapsed: Vec<f64> = runs
        .iter()
        .filter_map(|r| r.elapsed_seconds)
        .filter(|e| *e > 0.0)
        .collect();
    elapsed.sort_by(f64::total_cmp);
    let mut rss: Vec<i64> = runs.iter().filter_map(|r| r.max_rss_bytes).collect();
    rss.sort_unstable();
    let total_runs = runs.len() as i64;
    runs.truncate(10);
    Ok(AssetStats {
        node_id: node_id.to_string(),
        total_runs,
        avg_elapsed_seconds: (!elapsed.is_empty())
            .then(|| elapsed.iter().sum::<f64>() / elapsed.len() as f64),
        median_elapsed_seconds: percentile(&elapsed, 50.0),
        max_elapsed_seconds: elapsed.last().copied(),
        p95_elapsed_seconds: percentile(&elapsed, 95.0),
        cache_hit_rate: if executions > 0 {
            repeats as f64 / executions as f64
        } else {
            0.0
        },
        max_rss_bytes: rss.last().copied(),
        median_max_rss_bytes: (!rss.is_empty()).then(|| rss[(rss.len() - 1) / 2]),
        recent_runs: runs,
    })
}

/// Compute the p-th percentile from a sorted slice of values.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        Some(sorted[lower])
    } else {
        let frac = rank - lower as f64;
        Some(sorted[lower] * (1.0 - frac) + sorted[upper] * frac)
    }
}

/// How many values one `IN (...)` list binds: well under SQLite's parameter limit.
pub(super) const IN_LIST_CHUNK: usize = 500;

/// [`get_avg_elapsed`] on an open connection: one statement per 500 nodes, not one per node.
pub(super) async fn avg_elapsed_on(
    conn: &libsql::Connection,
    node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    let mut result = HashMap::new();
    for chunk in node_ids.chunks(IN_LIST_CHUNK) {
        let marks = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut rows = conn
            .query(
                &format!(
                    "SELECT node_id, AVG(elapsed_seconds) FROM materializations \
                     WHERE node_id IN ({marks}) AND elapsed_seconds IS NOT NULL GROUP BY node_id"
                ),
                libsql::params_from_iter(chunk.iter().cloned()),
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query avg elapsed: {e}")))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            if let (Ok(node_id), Ok(avg)) = (row.get::<String>(0), row.get::<f64>(1)) {
                result.insert(node_id, avg);
            }
        }
    }
    Ok(result)
}

pub(super) async fn partitioned_avg_elapsed_on(
    conn: &libsql::Connection,
    base_node_ids: &[String],
) -> Result<HashMap<String, f64>, BarcaError> {
    let mut result = HashMap::new();
    // A node split into one step per worker is named once per step: ask once per node.
    let mut asked = std::collections::HashSet::new();
    for nid in base_node_ids {
        if !asked.insert(nid.as_str()) {
            continue;
        }
        // Every key of the node, `nid[...]`, as a range of the node id index (a `LIKE` scanned
        // the whole table).
        let (lo, hi) = key_range(nid);
        let mut rows = conn
            .query(
                concat!(
                    "SELECT AVG(elapsed_seconds) FROM materializations ",
                    by_node_run!(),
                    " WHERE node_id >= ?1 AND node_id < ?2 AND elapsed_seconds IS NOT NULL"
                ),
                [lo, hi],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query avg elapsed: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
            && let Ok(avg) = row.get::<f64>(0)
        {
            result.insert(nid.clone(), avg);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod duration_tests {
    /// A sparse partitioned node must not walk other nodes' entire histories.
    #[tokio::test]
    async fn recent_durations_skip_unrelated_history_for_sparse_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        let ids: Vec<String> = (0..24).map(|i| format!("p.py:sparse_{i}")).collect();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            conn.execute(
                "WITH RECURSIVE history(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM history WHERE n < 200000) \
                 INSERT INTO materializations(node_id, status, elapsed_seconds) \
                 SELECT 'p.py:other', 'success', 0.5 FROM history",
                (),
            ).await.unwrap();
            for (i, id) in ids.iter().enumerate() {
                conn.execute(
                    "INSERT INTO materializations(node_id, status, elapsed_seconds) VALUES (?1, ?2, ?3)",
                    libsql::params![format!("{id}[key=a]"), if i % 2 == 0 { "success" } else { "failed" }, 7.0],
                ).await.unwrap();
            }
        }
        let started = std::time::Instant::now();
        let durations = super::recent_durations(&path, &ids, 20).await.unwrap();
        let elapsed = started.elapsed();
        for (i, id) in ids.iter().enumerate() {
            if i % 2 == 0 {
                assert_eq!(durations[id], vec![7.0]);
            } else {
                assert!(!durations.contains_key(id));
            }
        }
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "duration lookup walked unrelated history: {elapsed:?}"
        );
    }

    /// `GET /state`'s typical durations: the last successes of a node, its keys folded in,
    /// found below the latest key row however many other rows were written since.
    #[tokio::test]
    async fn recent_durations_take_the_last_successes_and_fold_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            let insert = |node: &str, status: &str, elapsed: f64| {
                let conn = &conn;
                let (node, status, elapsed) = (node.to_string(), status.to_string(), elapsed);
                async move {
                    conn.execute(
                        "INSERT INTO materializations (node_id, status, elapsed_seconds) \
                         VALUES (?1, ?2, ?3)",
                        libsql::params![node, status, elapsed],
                    )
                    .await
                    .unwrap();
                }
            };
            for _ in 0..5 {
                insert("p.py:a", "success", 1.0).await;
            }
            for k in 0..30 {
                insert(&format!("p.py:f[k={k}]"), "success", k as f64).await;
            }
            for _ in 0..20 {
                insert("p.py:a", "success", 10.0).await;
            }
            insert("p.py:a", "failed", 999.0).await;
            // A key row far below the latest one, and many rows of other nodes since.
            insert("p.py:g[k=old]", "success", 7.0).await;
            for _ in 0..3000 {
                insert("p.py:other", "success", 0.5).await;
            }
            insert("p.py:g[k=new]", "failed", 99.0).await;
            insert("p.py:failed_only[k=1]", "failed", 99.0).await;
            // `p.py:a_b` is not a key of `p.py:a`.
            insert("p.py:a_b[k=1]", "success", 5.0).await;
        }
        let ids: Vec<String> = [
            "p.py:a",
            "p.py:f",
            "p.py:g",
            "p.py:missing",
            "p.py:failed_only",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let d = super::recent_durations(&path, &ids, 20).await.unwrap();
        assert_eq!(
            d["p.py:a"],
            vec![10.0; 20],
            "the last 20 successes, failures left out"
        );
        let f: Vec<f64> = (10..30).rev().map(|k| k as f64).collect();
        assert_eq!(d["p.py:f"], f, "the latest 20 key rows, newest first");
        assert_eq!(d["p.py:g"], vec![7.0], "found below 3000 other rows");
        assert!(!d.contains_key("p.py:missing"));
        assert!(!d.contains_key("p.py:failed_only"));
    }
}
