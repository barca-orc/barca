//! A partitioned node's keys with a recorded attempt: each key's latest attempt, as
//! `GET /assets/{name}/partitions` lists them ([`partition_keys`]), and the keys whose latest
//! attempt failed, which `barca status` reports ([`failed_partition_keys`]).

use super::*;
use serde::{Deserialize, Serialize};

/// A partitioned node's keys whose latest recorded attempt failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FailedKeys {
    pub count: usize,
    /// The first of them in key order, at most 20.
    pub keys: Vec<String>,
}

/// [`FailedKeys`] of every partitioned node with some. Reads `idx_mat_failed`, then one
/// `idx_mat_node_latest` probe per failed key, so a node's healthy keys cost nothing.
pub(crate) async fn failed_partition_keys(
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
    /// The error's first line, with each of the key's own values replaced by its dimension's
    /// name in angle brackets (`<region>`).
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

/// A key's latest recorded attempt, as the database holds it ([`latest_attempts`]).
#[derive(Debug, Clone)]
pub struct LatestAttempt {
    pub key: crate::model::PartitionKey,
    pub status: String,
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub error: Option<String>,
    pub run_id: Option<String>,
    pub artifact_path: Option<String>,
    pub size_bytes: Option<i64>,
}

/// The latest attempt of each of `node_id`'s keys, in key order. Reads the node's rows through
/// `idx_mat_node_run`, so it grows with the node's history, not the database's.
pub async fn latest_attempts(
    db_path: &str,
    node_id: &str,
) -> Result<Vec<LatestAttempt>, BarcaError> {
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
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        out.push(LatestAttempt {
            key: crate::StepId::parse(&row.get::<String>(0).unwrap_or_default()).partition,
            status: row.get::<String>(2).unwrap_or_default(),
            created_at: row.get::<String>(3).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(4).ok(),
            error: row.get::<String>(5).ok(),
            run_id: row.get::<String>(6).ok().filter(|r| !r.is_empty()),
            artifact_path: row.get::<String>(7).ok().filter(|p| !p.is_empty()),
            size_bytes: row.get::<i64>(8).ok(),
        });
    }
    Ok(out)
}

/// An error's first line with the key's own values replaced by their dimension's name:
/// "no rows for apac on 2026-03-01" and "no rows for emea on 2026-03-02" are one problem,
/// "no rows for <region> on <day>".
pub fn error_shape(error: Option<&str>, key: &crate::model::PartitionKey) -> String {
    let first = error.unwrap_or("").lines().next().unwrap_or("");
    // Longest values first, so that a value inside another is not replaced in it.
    let mut values: Vec<(&String, &String)> = key.0.iter().collect();
    values.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
    values
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .fold(first.to_string(), |text, (dim, value)| {
            replace_word(&text, value, &format!("<{dim}>"))
        })
}

/// `text` with each occurrence of `word` that is not part of a longer word replaced: the key
/// `a` is replaced in "no rows for a", not in "ValueError".
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut before: Option<char> = None;
    while let Some(at) = rest.find(word) {
        let prev = rest[..at].chars().next_back().or(before);
        let next = rest[at + word.len()..].chars().next();
        let bounded = !prev.is_some_and(is_word) && !next.is_some_and(is_word);
        out.push_str(&rest[..at]);
        out.push_str(if bounded { with } else { word });
        before = word.chars().next_back();
        rest = &rest[at + word.len()..];
    }
    out.push_str(rest);
    out
}

/// The latest attempt of each of `node_id`'s keys that match `filter`, one page of them.
pub async fn partition_keys(
    db_path: &str,
    node_id: &str,
    filter: &PartitionKeysFilter,
) -> Result<PartitionKeys, BarcaError> {
    let mut out = PartitionKeys::default();
    let mut errors: HashMap<String, usize> = HashMap::new();
    let needle = filter.query.as_deref().map(str::to_lowercase);
    for row in latest_attempts(db_path, node_id).await? {
        out.recorded += 1;
        if row.status == "failed" {
            out.failed += 1;
            *errors
                .entry(error_shape(row.error.as_deref(), &row.key))
                .or_default() += 1;
        }
        let key = row.key.suffix();
        if filter.status.as_deref().is_some_and(|s| s != row.status)
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
                status: row.status,
                created_at: row.created_at,
                elapsed_seconds: row.elapsed_seconds,
                error: row.error,
                run_id: row.run_id,
                artifact_path: row.artifact_path,
                size_bytes: row.size_bytes,
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

/// One recorded attempt of one key (`GET /assets/{name}/partitions/key`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeyAttempt {
    pub status: String,
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub error: Option<String>,
    pub run_id: Option<String>,
    pub artifact_path: Option<String>,
    pub format: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
}

/// The latest `limit` attempts of one step (`node[key]`), newest first.
pub async fn key_attempts(
    db_path: &str,
    step_id: &str,
    limit: usize,
) -> Result<Vec<KeyAttempt>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failure =
        |e: libsql::Error| BarcaError::Db(format!("failed to query a key's attempts: {e}"));
    let mut rows = conn
        .query(
            "SELECT status, created_at, elapsed_seconds, error_message, run_id, artifact_path, artifact_format, artifact_size_bytes \
             FROM materializations WHERE node_id = ?1 ORDER BY id DESC LIMIT ?2",
            libsql::params![step_id, limit as i64],
        )
        .await
        .map_err(failure)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        out.push(KeyAttempt {
            status: row.get::<String>(0).unwrap_or_default(),
            created_at: row.get::<String>(1).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(2).ok(),
            error: row.get::<String>(3).ok(),
            run_id: row.get::<String>(4).ok().filter(|r| !r.is_empty()),
            artifact_path: row.get::<String>(5).ok().filter(|p| !p.is_empty()),
            format: row.get::<String>(6).ok().filter(|f| !f.is_empty()),
            size_bytes: row.get::<i64>(7).ok(),
        });
    }
    Ok(out)
}
