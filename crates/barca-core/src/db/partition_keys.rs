//! A partitioned node's keys with a recorded attempt: each key's latest attempt, as
//! `GET /assets/{name}/partitions` lists them ([`partition_keys`]), and the keys whose latest
//! attempt failed, which `barca status` reports ([`failed_partition_keys`]).

use super::*;
use serde::{Deserialize, Serialize};

/// A partitioned node's keys whose latest recorded attempt failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailedKeys {
    pub count: usize,
    /// The first of them in key order, at most 20.
    pub keys: Vec<String>,
}

/// [`FailedKeys`] of every partitioned node with some. Reads `idx_mat_failed`, then one
/// `idx_mat_node_latest` probe per failed key, so a node's healthy keys cost nothing.
pub async fn failed_partition_keys(
    db_path: &str,
) -> Result<HashMap<String, FailedKeys>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failure = |e: libsql::Error| BarcaError::Db(format!("failed to query failed keys: {e}"));
    let mut rows = conn
        .query(
            "SELECT DISTINCT f.node_id FROM materializations f \
             WHERE f.status = 'failed' AND instr(f.node_id, '[') > 0 AND NOT EXISTS ( \
               SELECT 1 FROM materializations s WHERE s.node_id = f.node_id AND s.status = 'success' \
               AND s.artifact_path IS NOT NULL AND s.id > f.id) \
             ORDER BY f.node_id",
            (),
        )
        .await
        .map_err(failure)?;
    let mut out: HashMap<String, FailedKeys> = HashMap::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        let node = row.get::<String>(0).unwrap_or_default();
        let id = crate::StepId::parse(&node);
        let entry = out.entry(id.base_id().to_string()).or_default();
        entry.count += 1;
        if entry.keys.len() < 20 {
            entry.keys.push(id.partition.suffix());
        }
    }
    Ok(out)
}

/// A key's latest recorded attempt (`GET /assets/{name}/partitions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionKeyRow {
    /// `retailer=acct_0001`.
    pub key: String,
    pub status: String,
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub error: Option<String>,
    pub run_id: Option<String>,
    pub artifact_path: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
}

/// How many keys last failed with one error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionErrorGroup {
    /// The error's first line, with the key's own values replaced by `<key>`.
    pub error: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub keys: usize,
}

/// One page of a node's keys that have a recorded attempt, in key order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionKeys {
    pub keys: Vec<PartitionKeyRow>,
    /// Keys matching the filters; `keys` is at most a page of them.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    /// Over every recorded key, whatever the filters.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub recorded: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed: usize,
    /// The errors of the failed keys, most common first, at most 20.
    pub errors: Vec<PartitionErrorGroup>,
}

/// Which keys [`partition_keys`] lists: `status` is a key's latest attempt, `query` a substring
/// of the key.
#[derive(Debug, Clone, Default)]
pub struct PartitionKeysFilter {
    pub status: Option<String>,
    pub query: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// The latest attempt of each of `node_id`'s keys. Reads the node's rows through
/// `idx_mat_node_run`, so it grows with the node's history, not the database's.
pub async fn partition_keys(
    db_path: &str,
    node_id: &str,
    filter: &PartitionKeysFilter,
) -> Result<PartitionKeys, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failure = |e: libsql::Error| BarcaError::Db(format!("failed to query partition keys: {e}"));
    // SQLite takes a bare column from the row that holds the `MAX`: each key's latest attempt.
    let mut rows = conn
        .query(
            &format!(
                "SELECT node_id, MAX(id), status, created_at, elapsed_seconds, error_message, run_id, artifact_path, artifact_size_bytes \
                 FROM materializations WHERE {KEY_ROWS} GROUP BY node_id ORDER BY node_id"
            ),
            [node_id],
        )
        .await
        .map_err(failure)?;
    let mut out = PartitionKeys::default();
    let mut errors: HashMap<String, usize> = HashMap::new();
    let needle = filter.query.as_deref().map(str::to_lowercase);
    while let Some(row) = rows.next().await.map_err(failure)? {
        out.recorded += 1;
        let status = row.get::<String>(2).unwrap_or_default();
        let error = row.get::<String>(5).ok();
        let id = crate::StepId::parse(&row.get::<String>(0).unwrap_or_default());
        if status == "failed" {
            out.failed += 1;
            let first = error.as_deref().unwrap_or("").lines().next().unwrap_or("");
            // "no plan for acct_0010" and "no plan for acct_0020" are one problem.
            let shape = id
                .partition
                .0
                .values()
                .fold(first.to_string(), |text, value| {
                    text.replace(value, "<key>")
                });
            *errors.entry(shape).or_default() += 1;
        }
        let key = id.partition.suffix();
        if filter.status.as_deref().is_some_and(|s| s != status)
            || needle
                .as_deref()
                .is_some_and(|n| !key.to_lowercase().contains(n))
        {
            continue;
        }
        out.total += 1;
        if out.total > filter.offset && out.keys.len() < filter.limit {
            out.keys.push(PartitionKeyRow {
                key,
                status,
                created_at: row.get::<String>(3).unwrap_or_default(),
                elapsed_seconds: row.get::<f64>(4).ok(),
                error,
                run_id: row.get::<String>(6).ok().filter(|r| !r.is_empty()),
                artifact_path: row.get::<String>(7).ok().filter(|p| !p.is_empty()),
                size_bytes: row.get::<i64>(8).ok(),
            });
        }
    }
    let mut groups: Vec<PartitionErrorGroup> = errors
        .into_iter()
        .map(|(error, keys)| PartitionErrorGroup { error, keys })
        .collect();
    groups.sort_by(|a, b| b.keys.cmp(&a.keys).then_with(|| a.error.cmp(&b.error)));
    groups.truncate(20);
    out.errors = groups;
    Ok(out)
}
