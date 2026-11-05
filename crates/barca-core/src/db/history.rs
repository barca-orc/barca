//! Each node's history as `barca status` and `GET /state` summarize it: its age in runs
//! ([`node_ages`]), its last materialization and its run totals ([`node_histories`]), and the
//! nodes whose bytes changed in a run ([`changed_bytes_in_run`]).

use super::*;
use serde::{Deserialize, Serialize};

/// One row of `materializations`, as `barca status` reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MaterializationRecord {
    pub content_hash: Option<String>,
    /// Exact node id, including the partition suffix for a partitioned step.
    pub node_id: String,
    pub run_hash: Option<String>,
    pub artifact_path: Option<String>,
    pub artifact_format: Option<String>,
    pub artifact_size_bytes: Option<i64>,
    pub elapsed_seconds: Option<f64>,
    /// `success` or `failed`.
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: String,
}

/// The last observed change in bytes for unchanged code and inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BytesChange {
    pub node_id: String,
    pub run_hash: String,
    pub content_hash: String,
    pub created_at: String,
}

/// Nodes whose materialization in this run differed from the preceding digest.
pub(crate) async fn changed_bytes_in_run(
    db_path: &str,
    run_id: &str,
) -> Result<Vec<String>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn.query(
        "SELECT node_id FROM materializations INDEXED BY idx_mat_run WHERE run_id = ?1 AND bytes_changed = 1 ORDER BY node_id",
        [run_id],
    ).await.map_err(|e| BarcaError::Db(format!("failed to read byte changes: {e}")))?;
    let mut nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read byte changes: {e}")))?
    {
        nodes.push(
            row.get::<String>(0)
                .map_err(|e| BarcaError::Db(format!("failed to read node id: {e}")))?,
        );
    }
    Ok(nodes)
}

/// How late a node is, from its materialization rows (keys of a partitioned node count toward
/// their base id). Timestamps are as recorded: UTC `YYYY-MM-DD HH:MM:SS`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeAge {
    /// When its newest successful row was written.
    pub last_success_at: Option<String>,
    /// The first failed attempt after its last clean run: `None` unless its latest attempts failed.
    pub failing_since: Option<String>,
    /// How many runs in a row failed it, up to now. A run counts once however many keys failed.
    pub consecutive_failures: u32,
    /// Its newest run as a whole; `None` until a run recorded a row for it.
    pub last_run: Option<RunTotals>,
}

/// One run's rows for one node, keys folded into their base id: what the run cost on the node
/// as a whole, where `elapsed_seconds` of a key's row is that key's share alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunTotals {
    pub run_id: String,
    /// Wall time summed over the rows that recorded one; `None` when none did (every key
    /// failed before finishing).
    pub elapsed_seconds: Option<f64>,
    /// Rows the run recorded for the node: its keys, or 1.
    pub keys: u32,
    /// Batched calls (RFC-0013) those keys ran in; 0 when each ran on its own.
    pub calls: u32,
    pub failed: u32,
}

/// The base id of a materialization row's node id (`<base>[k=v]` for a key).
fn base_of(node_id: &str) -> &str {
    node_id.split_once('[').map_or(node_id, |(base, _)| base)
}

/// [`NodeAge`] of every node with history. One read of `idx_mat_failed`, then one pass over the
/// successful rows: an attempt is a run, and a run with a failed key is a failed attempt even
/// when other keys succeeded. Rows without a run id (an older barca's) are each their own attempt.
/// Read-only; the caller ensures the DB exists.
pub async fn node_ages(db_path: &str) -> Result<HashMap<String, NodeAge>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_h, conn) = open_conn(db_path).await?;
    let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
    let attempt = |run: Option<String>, id: i64| run.unwrap_or_else(|| format!("#{id}"));
    // base id -> run -> (its newest failed row, its earliest failure's time).
    let mut failed: HashMap<String, HashMap<String, (i64, String)>> = HashMap::new();
    // base id -> (its newest row, that row's run).
    let mut newest: HashMap<String, (i64, Option<String>)> = HashMap::new();
    let mut note_newest = |base: &str, id: i64, run: Option<String>| {
        let entry = newest.entry(base.to_string()).or_insert((id, run.clone()));
        if id >= entry.0 {
            *entry = (id, run);
        }
    };
    let mut rows = conn
        .query(
            "SELECT id, node_id, run_id, created_at FROM materializations \
             WHERE status = 'failed' ORDER BY run_id, id",
            (),
        )
        .await
        .map_err(failure)?;
    while let Some(row) = rows.next().await.map_err(failure)? {
        let id = row.get::<i64>(0).unwrap_or_default();
        let node = row.get::<String>(1).unwrap_or_default();
        let recorded = row.get::<String>(2).ok().filter(|r| !r.is_empty());
        note_newest(base_of(&node), id, recorded.clone());
        let run = attempt(recorded, id);
        let at = row.get::<String>(3).unwrap_or_default();
        failed
            .entry(base_of(&node).to_string())
            .or_default()
            .entry(run)
            .and_modify(|seen| seen.0 = seen.0.max(id))
            .or_insert((id, at));
    }
    drop(rows);
    // base id -> (newest successful row and its time, newest row of a run that did not fail it).
    let mut success: HashMap<String, (i64, String, i64)> = HashMap::new();
    let mut rows = conn
        .query(
            "SELECT id, node_id, run_id, created_at FROM materializations \
             WHERE status = 'success'",
            (),
        )
        .await
        .map_err(failure)?;
    while let Some(row) = rows.next().await.map_err(failure)? {
        let id = row.get::<i64>(0).unwrap_or_default();
        let node = row.get::<String>(1).unwrap_or_default();
        let base = base_of(&node);
        let run = row.get::<String>(2).ok().filter(|r| !r.is_empty());
        note_newest(base, id, run.clone());
        let clean = failed
            .get(base)
            .is_none_or(|runs| run.is_none_or(|r| !runs.contains_key(&r)));
        let entry = match success.get_mut(base) {
            Some(entry) => entry,
            None => success.entry(base.to_string()).or_default(),
        };
        if id > entry.0 {
            entry.0 = id;
            entry.1 = row.get::<String>(3).unwrap_or_default();
        }
        if clean {
            entry.2 = entry.2.max(id);
        }
    }
    let mut out: HashMap<String, NodeAge> = HashMap::with_capacity(success.len());
    for (base, runs) in failed {
        let clean = success.get(&base).map_or(0, |s| s.2);
        let trailing = runs.values().filter(|(newest, _)| *newest > clean);
        out.insert(
            base,
            NodeAge {
                last_success_at: None,
                failing_since: trailing.clone().map(|(_, at)| at.clone()).min(),
                consecutive_failures: trailing.count() as u32,
                last_run: None,
            },
        );
    }
    for (base, (_, at, _)) in success {
        out.entry(base).or_default().last_success_at = Some(at);
    }
    // The newest run's rows, one index seek per node (`idx_mat_run`). A run requeued onto new
    // code can hold two rows for a key, one per run hash (#794), so keys are counted distinct.
    // Rows without a run id (an older barca's) leave it unknown.
    for (base, (_, run)) in newest {
        let Some(run) = run else { continue };
        let (lo, hi) = key_range(&base);
        let mut rows = conn
            .query(
                "SELECT sum(elapsed_seconds), count(DISTINCT node_id), count(DISTINCT batch_id), \
                 sum(status = 'failed') FROM materializations INDEXED BY idx_mat_run \
                 WHERE run_id = ?1 AND (node_id = ?2 OR (node_id >= ?3 AND node_id < ?4))",
                libsql::params![run.clone(), base.clone(), lo, hi],
            )
            .await
            .map_err(failure)?;
        if let Some(row) = rows.next().await.map_err(failure)? {
            out.entry(base).or_default().last_run = Some(RunTotals {
                run_id: run,
                elapsed_seconds: row.get::<f64>(0).ok(),
                keys: row.get::<i64>(1).unwrap_or_default() as u32,
                calls: row.get::<i64>(2).unwrap_or_default() as u32,
                failed: row.get::<i64>(3).unwrap_or_default() as u32,
            });
        }
    }
    Ok(out)
}

/// What the metadata DB knows about one node (all partition keys of a partitioned node).
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeHistory {
    pub last_bytes_change: Option<BytesChange>,
    /// The most recent materialization attempt, successful or not.
    pub latest: Option<MaterializationRecord>,
    /// Whether any attempt ever succeeded.
    pub ever_succeeded: bool,
}

/// Latest materialization and success flag for each base node id. Rows of partitioned steps
/// (`<base>[k=v]`) count toward their base id. Read-only; the caller ensures the DB exists.
pub(crate) async fn node_histories(
    db_path: &str,
    base_ids: &[String],
) -> Result<HashMap<String, NodeHistory>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut out = HashMap::new();
    for base in base_ids {
        out.insert(base.clone(), node_history(&conn, base).await?);
    }
    Ok(out)
}

/// [`node_histories`] of one node. Each question is asked of the node's own rows and of its keys'
/// rows (`base[...]`, a range of `idx_mat_node_run`), so every query is an index seek or an
/// index-only scan. A `substr(node_id, ...)` filter cannot use the index: it scanned the whole
/// table four times per node, minutes for a project with a 200k-key node.
async fn node_history(conn: &libsql::Connection, base: &str) -> Result<NodeHistory, BarcaError> {
    let (lo, hi) = key_range(base);
    let exact = ("node_id = ?1", vec![libsql::Value::Text(base.to_string())]);
    let keys = (
        "node_id >= ?1 AND node_id < ?2",
        vec![libsql::Value::Text(lo), libsql::Value::Text(hi)],
    );
    let mut h = NodeHistory::default();
    let mut latest: Option<i64> = None;
    let mut bytes: Option<i64> = None;
    for (filter, params) in [&exact, &keys] {
        let id = query_i64(
            conn,
            &format!("SELECT max(id) FROM materializations {BY_NODE_RUN} WHERE {filter}"),
            params.clone(),
        )
        .await?;
        latest = latest.max(id);
        if !h.ever_succeeded {
            h.ever_succeeded = query_i64(
                conn,
                &format!(
                    "SELECT 1 FROM materializations {BY_NODE_RUN} \
                     WHERE {filter} AND status = 'success' LIMIT 1"
                ),
                params.clone(),
            )
            .await?
            .is_some();
        }
        // `idx_mat_changed` holds only the rows whose bytes changed.
        let changed = query_i64(
            conn,
            &format!("SELECT max(id) FROM materializations WHERE bytes_changed = 1 AND {filter}"),
            params.clone(),
        )
        .await?;
        bytes = bytes.max(changed);
    }
    // The last materialization is the latest run's (by row id). A partitioned node writes one
    // row per key in completion order, so within that run the first key in sort order is
    // reported, not whichever finished last (#475). Rows without a run id (an older barca's)
    // keep the latest row.
    let latest = match latest {
        Some(id) => Some(first_key_of_latest_run(conn, id, &exact, &keys).await?),
        None => None,
    };
    if let Some(id) = latest {
        let mut rows = conn
            .query(
                "SELECT node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
                 elapsed_seconds, status, error_message, created_at, output_hash \
                 FROM materializations WHERE id = ?1",
                [id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            let opt = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
            h.latest = Some(MaterializationRecord {
                content_hash: opt(9),
                node_id: row.get::<String>(0).unwrap_or_default(),
                run_hash: opt(1),
                artifact_path: opt(2),
                artifact_format: opt(3),
                artifact_size_bytes: row.get::<i64>(4).ok(),
                elapsed_seconds: row.get::<f64>(5).ok(),
                status: opt(6).unwrap_or_else(|| "success".to_string()),
                error_message: opt(7),
                created_at: row.get::<String>(8).unwrap_or_default(),
            });
        }
    }
    if let Some(id) = bytes {
        let mut rows = conn
            .query(
                "SELECT node_id, run_hash, output_hash, created_at FROM materializations \
                 WHERE id = ?1",
                [id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query byte change: {e}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read byte change: {e}")))?
        {
            h.last_bytes_change = Some(BytesChange {
                node_id: row.get::<String>(0).unwrap_or_default(),
                run_hash: row.get::<String>(1).unwrap_or_default(),
                content_hash: row.get::<String>(2).unwrap_or_default(),
                created_at: row.get::<String>(3).unwrap_or_default(),
            });
        }
    }
    Ok(h)
}

/// The row [`node_history`] reports for a node whose latest row is `latest`: the row of the
/// first node id in sort order (the base id, then its keys) in that row's run, that node's
/// latest in the run. `latest` itself when it has no run id.
async fn first_key_of_latest_run(
    conn: &libsql::Connection,
    latest: i64,
    exact: &(&str, Vec<libsql::Value>),
    keys: &(&str, Vec<libsql::Value>),
) -> Result<i64, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT run_id FROM materializations WHERE id = ?1",
            [latest],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    let run = match rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        Some(row) => row.get::<String>(0).ok().filter(|s| !s.is_empty()),
        None => None,
    };
    drop(rows);
    let Some(run) = run else {
        return Ok(latest);
    };
    let with_run = |(filter, params): &(&str, Vec<libsql::Value>)| {
        let mut params = params.clone();
        params.push(libsql::Value::Text(run.clone()));
        let n = params.len();
        (format!("{filter} AND run_id = ?{n}"), params)
    };
    // The base id sorts before every key of it.
    let (filter, params) = with_run(exact);
    if let Some(id) = query_i64(
        conn,
        &format!("SELECT max(id) FROM materializations {BY_NODE_RUN} WHERE {filter}"),
        params,
    )
    .await?
    {
        return Ok(id);
    }
    // The first key in the run, walking the index in key order.
    let (filter, params) = with_run(keys);
    let mut rows = conn
        .query(
            &format!(
                "SELECT node_id FROM materializations {BY_NODE_RUN} WHERE {filter} \
                 ORDER BY node_id LIMIT 1"
            ),
            params,
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    let first = match rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        Some(row) => row.get::<String>(0).ok(),
        None => None,
    };
    drop(rows);
    let Some(first) = first else {
        return Ok(latest);
    };
    Ok(query_i64(
        conn,
        concat!(
            "SELECT max(id) FROM materializations ",
            by_node_run!(),
            " WHERE node_id = ?1 AND run_id = ?2"
        ),
        vec![libsql::Value::Text(first), libsql::Value::Text(run)],
    )
    .await?
    .unwrap_or(latest))
}

/// The first column of the first row as an integer; `None` for no row or a NULL.
pub(super) async fn query_i64(
    conn: &libsql::Connection,
    sql: &str,
    params: Vec<libsql::Value>,
) -> Result<Option<i64>, BarcaError> {
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    Ok(rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        .and_then(|row| row.get::<i64>(0).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn last_materialization_of_a_partitioned_node_does_not_depend_on_completion_order() {
        let mut picked = Vec::new();
        for order in [["b", "a"], ["a", "b"]] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("metadata.db").display().to_string();
            super::init_db(&path).await.unwrap();
            {
                let _guard = super::db_guard().await;
                let (_db, conn) = super::open_conn(&path).await.unwrap();
                let mut rows = vec![("r0", "p.py:t[region=0]".to_string())];
                rows.extend(order.iter().map(|k| ("r1", format!("p.py:t[region={k}]"))));
                for (run, node) in rows {
                    conn.execute(
                        "INSERT INTO materializations (run_id, node_id, run_hash, status) VALUES (?1, ?2, 'h', 'success')",
                        [run, node.as_str()],
                    )
                    .await
                    .unwrap();
                }
            }
            let ids = vec!["p.py:t".to_string()];
            let first = super::node_histories(&path, &ids).await.unwrap();
            let again = super::node_histories(&path, &ids).await.unwrap();
            let node = |h: &HashMap<String, super::NodeHistory>| {
                h["p.py:t"].latest.as_ref().unwrap().node_id.clone()
            };
            assert_eq!(node(&first), node(&again));
            picked.push(node(&first));
        }
        assert_eq!(picked, ["p.py:t[region=a]", "p.py:t[region=a]"]);

        // Rows an older barca wrote without a run id: the latest row, as before.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for node in ["p.py:t[region=a]", "p.py:t[region=b]"] {
                conn.execute(
                    "INSERT INTO materializations (node_id, run_hash) VALUES (?1, 'h')",
                    [node],
                )
                .await
                .unwrap();
            }
        }
        let h = super::node_histories(&path, &["p.py:t".to_string()])
            .await
            .unwrap();
        assert_eq!(
            h["p.py:t"].latest.as_ref().unwrap().node_id,
            "p.py:t[region=b]"
        );
    }
}

#[cfg(test)]
mod failure_index_tests {
    use super::*;

    /// Age is counted in runs: a run with a failed key is one failed attempt, a clean run ends
    /// the streak, and a key's row counts toward its base node.
    #[tokio::test]
    async fn a_nodes_age_counts_failed_runs_since_its_last_clean_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            for (run, node, status, at) in [
                ("r1", "p.py:recovered", "failed", "2026-01-01 00:00:00"),
                ("r2", "p.py:recovered", "success", "2026-01-02 00:00:00"),
                ("r1", "p.py:failing", "success", "2026-01-01 00:00:00"),
                ("r2", "p.py:failing", "failed", "2026-01-02 00:00:00"),
                ("r3", "p.py:failing", "failed", "2026-01-03 00:00:00"),
                ("r1", "p.py:keys[d=1]", "success", "2026-01-01 00:00:00"),
                ("r1", "p.py:keys[d=2]", "success", "2026-01-01 00:00:00"),
                ("r2", "p.py:keys[d=1]", "failed", "2026-01-02 00:00:00"),
                ("r2", "p.py:keys[d=2]", "success", "2026-01-02 12:00:00"),
                ("r2", "p.py:keys[d=3]", "failed", "2026-01-02 13:00:00"),
                ("r1", "p.py:never", "failed", "2026-01-01 00:00:00"),
            ] {
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, status, created_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    [run, node, status, at],
                )
                .await
                .unwrap();
            }
        }
        let ages = node_ages(&path).await.unwrap();
        let age = |id: &str, ok: Option<&str>, since: Option<&str>, n: u32| {
            assert_eq!(
                NodeAge {
                    last_run: None,
                    ..ages[id].clone()
                },
                NodeAge {
                    last_success_at: ok.map(str::to_string),
                    failing_since: since.map(str::to_string),
                    consecutive_failures: n,
                    last_run: None,
                },
                "{id}"
            );
        };
        age("p.py:recovered", Some("2026-01-02 00:00:00"), None, 0);
        age(
            "p.py:failing",
            Some("2026-01-01 00:00:00"),
            Some("2026-01-02 00:00:00"),
            2,
        );
        age(
            "p.py:keys",
            Some("2026-01-02 12:00:00"),
            Some("2026-01-02 00:00:00"),
            1,
        );
        age("p.py:never", None, Some("2026-01-01 00:00:00"), 1);
        // The newest run as a whole: its keys, how many failed, and (none recorded a time) no
        // duration rather than zero.
        assert_eq!(
            ages["p.py:keys"].last_run,
            Some(RunTotals {
                run_id: "r2".into(),
                elapsed_seconds: None,
                keys: 3,
                calls: 0,
                failed: 2,
            })
        );
        assert_eq!(
            ages["p.py:recovered"].last_run.as_ref().unwrap().run_id,
            "r2"
        );
    }

    /// A batched node's last run costs what its keys' rows add up to, over the calls they ran in.
    #[tokio::test]
    async fn a_nodes_last_run_sums_its_keys_over_their_batched_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            for (run, node, elapsed, batch) in [
                ("r1", "p.py:keys[d=1]", 9.0, "old"),
                ("r2", "p.py:keys[d=1]", 0.5, "b1"),
                ("r2", "p.py:keys[d=2]", 0.25, "b1"),
                ("r2", "p.py:keys[d=3]", 1.0, "b2"),
                ("r2", "p.py:plain", 2.0, ""),
            ] {
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, status, elapsed_seconds, \
                     batch_id) VALUES (?1, ?2, 'success', ?3, NULLIF(?4, ''))",
                    libsql::params![run, node, elapsed, batch],
                )
                .await
                .unwrap();
            }
        }
        let ages = node_ages(&path).await.unwrap();
        assert_eq!(
            ages["p.py:keys"].last_run,
            Some(RunTotals {
                run_id: "r2".into(),
                elapsed_seconds: Some(1.75),
                keys: 3,
                calls: 2,
                failed: 0,
            })
        );
        assert_eq!(
            ages["p.py:plain"].last_run,
            Some(RunTotals {
                run_id: "r2".into(),
                elapsed_seconds: Some(2.0),
                keys: 1,
                calls: 0,
                failed: 0,
            })
        );
    }
}
