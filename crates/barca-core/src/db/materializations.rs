//! Reads of `materializations` outside a cache lookup (latest successes, output hashes, a
//! partitioned node's latest artifacts, evictions), the read times of the cache size limit, and
//! the writes that change or remove rows (and the `definition_components` they name) after they
//! were recorded: trimming, eviction marks and upload digests; and the statements that record
//! rows, which [`crate::persist`] drives (its transactions, its eligibility rules). One-key
//! cache lookups are [`super::CacheReader`]'s.

use super::*;
use crate::dispatch::OutputRef;
use libsql::Connection;

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Clear `components` of the rows `ids`. The number of rows changed.
pub(crate) async fn clear_components(conn: &Connection, ids: &[i64]) -> Result<u64, libsql::Error> {
    conn.execute(
        &format!(
            "UPDATE materializations SET components = NULL WHERE id IN ({})",
            id_list(ids)
        ),
        (),
    )
    .await
}

/// Delete the rows `ids`. The number of rows deleted.
pub(crate) async fn delete_rows(conn: &Connection, ids: &[i64]) -> Result<u64, libsql::Error> {
    conn.execute(
        &format!(
            "DELETE FROM materializations WHERE id IN ({})",
            id_list(ids)
        ),
        (),
    )
    .await
}

/// Delete the definitions no row's `components` names.
pub(crate) async fn delete_unnamed_definitions(conn: &Connection) -> Result<u64, libsql::Error> {
    conn.execute(
        "DELETE FROM definition_components WHERE definition_hash NOT IN (SELECT \
         json_extract(components, '$.\"@definition\"') FROM materializations \
         WHERE components IS NOT NULL AND json_valid(components) \
         AND json_extract(components, '$.\"@definition\"') IS NOT NULL)",
        (),
    )
    .await
}

/// Record `sha256` as the digest of the successful rows at `artifact_path` that have none.
pub(crate) async fn set_upload_digest(
    conn: &Connection,
    artifact_path: &str,
    sha256: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE materializations SET output_hash = ?2 \
         WHERE artifact_path = ?1 AND status = 'success' AND output_hash IS NULL",
        [
            libsql::Value::Text(artifact_path.to_string()),
            libsql::Value::Text(sha256.to_string()),
        ],
    )
    .await
}

/// Mark the successful rows at `artifact_path` evicted, now.
pub(crate) async fn mark_evicted(
    conn: &Connection,
    artifact_path: &str,
) -> Result<u64, libsql::Error> {
    conn.execute(
        "UPDATE materializations SET evicted_at = datetime('now') \
         WHERE artifact_path = ?1 AND status = 'success'",
        [artifact_path],
    )
    .await
}

/// The highest materialization id recorded so far (0 when there is none).
pub async fn max_materialization_id(db_path: &str) -> Result<i64, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT COALESCE((SELECT id FROM materializations ORDER BY id DESC LIMIT 1),0)",
            (),
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    Ok(match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0),
        _ => 0,
    })
}

/// Whether a successful materialization of one of `nodes` (or of one of their partitions) was
/// recorded after materialization id `mark`.
pub async fn succeeded_since(
    db_path: &str,
    mark: i64,
    nodes: &[String],
) -> Result<bool, BarcaError> {
    if nodes.is_empty() {
        return Ok(false);
    }
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT DISTINCT node_id FROM materializations WHERE id > ?1 AND status = 'success'",
            [mark.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    while let Ok(Some(row)) = rows.next().await {
        let id = row.get::<String>(0).unwrap_or_default();
        let base = id.split('[').next().unwrap_or(&id);
        if nodes.iter().any(|n| n == base) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The latest successful materialization of one step (a display id) that has an artifact.
#[derive(Debug, Clone)]
pub(crate) struct LatestSuccess {
    /// Its row id: a later success of another step has a higher one.
    pub(crate) id: i64,
    pub(crate) run_hash: String,
    pub(crate) output: OutputRef,
}

/// The latest successful materialization of `node_id` (a display id) whose artifact this run may
/// read; `None` when it never succeeded.
pub(crate) async fn latest_success(cache: &CacheReader, node_id: &str) -> Option<LatestSuccess> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
             output_hash FROM materializations WHERE node_id = ?1 AND status = 'success' \
             AND artifact_path IS NOT NULL ORDER BY id DESC",
            [node_id.to_string()],
        )
        .await
        .ok()?;
    while let Ok(Some(row)) = rows.next().await {
        let Ok(path) = row.get::<String>(2) else {
            continue;
        };
        if !cache.allows_artifact(&path) {
            continue;
        }
        return Some(LatestSuccess {
            id: row.get::<i64>(0).ok()?,
            run_hash: row.get::<String>(1).ok().unwrap_or_default(),
            output: OutputRef {
                path,
                format: row.get::<String>(3).ok()?,
                size_bytes: row.get::<i64>(4).ok()? as u64,
                elapsed_seconds: None,
                content_hash: row.get::<String>(5).ok().filter(|h| !h.is_empty()),
            },
        });
    }
    None
}

/// The key of a recorded row's components that names its definition's
/// (`definition_components`, [`crate::changes`]).
pub(crate) const DEFINITION_KEY: &str = "@definition";

/// The run-hash components the latest successful materialization of `node_id` (a display id)
/// recorded ([`crate::changes`]); `None` when it recorded none (before 0.29) or never succeeded.
pub(crate) async fn last_success_components(cache: &CacheReader, node_id: &str) -> Option<String> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT components FROM materializations WHERE node_id = ?1 AND status = 'success' \
             ORDER BY id DESC LIMIT 1",
            [node_id.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let recorded = row.get::<String>(0).ok().filter(|c| !c.is_empty())?;
    drop(rows);
    let mut own: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&recorded).ok()?;
    let Some(definition) = own.remove(DEFINITION_KEY) else {
        return Some(recorded);
    };
    let mut rows = cache
        .conn()
        .query(
            "SELECT components FROM definition_components WHERE definition_hash = ?1",
            [definition.as_str()?.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let mut all: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&row.get::<String>(0).ok()?).ok()?;
    all.append(&mut own);
    serde_json::to_string(&all).ok()
}

/// Record the definitions a starting run may name ([`crate::changes::definition_rows`]). Most runs
/// find them all there and write nothing. Without its row a step says a plain `changed`, so a
/// failure here is no reason to fail the run.
pub(super) async fn record_definitions(
    conn: &libsql::Connection,
    definitions: &[(String, String)],
) {
    let Ok(mut rows) = conn
        .query("SELECT definition_hash FROM definition_components", ())
        .await
    else {
        return;
    };
    let mut known = std::collections::HashSet::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(hash) = row.get::<String>(0) {
            known.insert(hash);
        }
    }
    drop(rows);
    for (hash, components) in definitions.iter().filter(|(h, _)| !known.contains(h)) {
        let _ = conn
            .execute(
                "INSERT OR IGNORE INTO definition_components (definition_hash, components) \
                 VALUES (?1, ?2)",
                [hash.as_str(), components.as_str()],
            )
            .await;
    }
}

/// The object this machine knows to be in the artifact store for `node_id` (a display id) at
/// `run_hash`: the store location and digest of its latest successful row, unless an upload to
/// that location is pending (#390) or the row has no digest. A row with a store location is
/// recorded only once its upload was confirmed (`specs/interfaces/artifact-upload-receipts.md`), so a
/// step that produces the same digest again need not upload it again (#554).
pub(crate) async fn confirmed_object(
    cache: &CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<(String, String)> {
    let mut rows = cache
        .conn()
        .query(
            concat!(
                "SELECT artifact_path, output_hash FROM materializations ",
                by_node_run!(),
                " WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' \
                 AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1"
            ),
            [node_id.to_string(), run_hash.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let path = row.get::<String>(0).ok()?;
    let digest = row.get::<String>(1).ok().filter(|d| !d.is_empty())?;
    drop(rows);
    // Any pending upload to that location (this host's or another's), or a database that
    // cannot say: the object is not known to be there.
    let mut pending = cache
        .conn()
        .query(
            "SELECT 1 FROM pending_uploads WHERE store_path = ?1",
            [path.clone()],
        )
        .await
        .ok()?;
    match pending.next().await {
        Ok(None) => Some((path, digest)),
        _ => None,
    }
}

/// The id of the latest successful materialization of `node_id` (a display id), artifact or
/// not; `None` when it never succeeded.
pub(crate) async fn latest_success_id(cache: &CacheReader, node_id: &str) -> Option<i64> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT MAX(id) FROM materializations WHERE node_id = ?1 AND status = 'success'",
            [node_id.to_string()],
        )
        .await
        .ok()?;
    rows.next().await.ok()??.get::<i64>(0).ok()
}

/// The output hash of the latest successful materialization of each step of `base_ids` (the
/// base id itself, or any of its partitions), keyed by display id. Steps never recorded with an
/// output hash (never ran, or ran before barca recorded sensor outputs) are absent; so is
/// everything when the database predates the `output_hash` column.
pub(crate) async fn last_output_hashes(
    cache: &CacheReader,
    base_ids: &[&str],
) -> Result<HashMap<String, String>, BarcaError> {
    let mut out = HashMap::new();
    for base in base_ids {
        // One seek for the base and two per recorded partition: skip directly to the next
        // node id instead of scanning every observation in each key's history.
        let (lo, hi) = key_range(base);
        for (sql, params) in [
            (
                "SELECT node_id, COALESCE(observation_hash, output_hash) FROM materializations \
                 INDEXED BY idx_mat_output_latest WHERE node_id = ?1 AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1",
                vec![base.to_string()],
            ),
            (
                "WITH RECURSIVE nodes(node_id) AS ( \
                 SELECT (SELECT node_id FROM materializations INDEXED BY idx_mat_output_latest \
                 WHERE node_id >= ?1 AND node_id < ?2 AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY node_id LIMIT 1) UNION ALL \
                 SELECT (SELECT node_id FROM materializations INDEXED BY idx_mat_output_latest \
                 WHERE node_id > nodes.node_id AND node_id < ?2 AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY node_id LIMIT 1) \
                 FROM nodes WHERE node_id IS NOT NULL) \
                 SELECT node_id, (SELECT COALESCE(observation_hash, output_hash) \
                 FROM materializations INDEXED BY idx_mat_output_latest \
                 WHERE node_id = nodes.node_id AND status = 'success' \
                 AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1) \
                 FROM nodes WHERE node_id IS NOT NULL",
                vec![lo, hi],
            ),
        ] {
            let Ok(mut rows) = cache.conn().query(sql, params).await else {
                return Ok(HashMap::new());
            };
            while let Ok(Some(row)) = rows.next().await {
                if let (Ok(node), Ok(hash)) = (row.get::<String>(0), row.get::<String>(1)) {
                    out.insert(node, hash);
                }
            }
        }
    }
    Ok(out)
}

/// Record `outputs` as materializations with no run (the legacy, run-less row), one connection
/// for all of them; a row that cannot be written is skipped.
pub async fn persist_outputs(
    db_path: &str,
    outputs: &HashMap<String, OutputRef>,
    run_hashes: &HashMap<String, String>,
) -> Result<(), BarcaError> {
    if outputs.is_empty() {
        return Ok(());
    }
    let _g = db_guard().await;
    let (_db, conn) = open_conn(db_path).await?;
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

// ─── Recording (driven by `crate::persist`) ──────────────────────────────────

/// One successful step as a `materializations` row. Built when the step finishes (for the
/// [`StepRecorder`]) and again from the ledger at the end of the run.
#[derive(Debug, Clone)]
pub(crate) struct NewMaterialization {
    pub(crate) node_id: String,
    pub(crate) run_hash: String,
    pub(crate) path: String,
    pub(crate) format: String,
    pub(crate) size_bytes: u64,
    pub(crate) elapsed_seconds: Option<f64>,
    pub(crate) attempts: u32,
    pub(crate) sinks_json: Option<String>,
    pub(crate) cpu_seconds: Option<f64>,
    pub(crate) max_rss_bytes: Option<u64>,
    /// Digest of the producer-owned serialized bytes.
    pub(crate) output_hash: Option<String>,
    pub(crate) observation_hash: Option<String>,
    /// The run hash's components ([`crate::changes::step_components_json`]).
    pub(crate) components: Option<String>,
    /// The batched call that computed it (RFC-0013).
    pub(crate) batch: Option<String>,
}

impl NewMaterialization {
    /// From the artifact a worker reported for a finished step. Reads the same fields the
    /// end-of-run ledger is built from, so both writers produce the same row.
    pub(crate) fn from_artifact(
        node_id: &str,
        run_hash: &str,
        artifact: &serde_json::Value,
        attempts: u32,
    ) -> Self {
        let str_of = |key: &str| artifact.get(key).and_then(|v| v.as_str());
        Self {
            node_id: node_id.to_string(),
            run_hash: run_hash.to_string(),
            path: str_of("path").unwrap_or("").to_string(),
            format: str_of("format").unwrap_or("json").to_string(),
            size_bytes: artifact
                .get("size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            elapsed_seconds: artifact.get("elapsed_seconds").and_then(|v| v.as_f64()),
            attempts,
            sinks_json: artifact
                .get("sinks")
                .and_then(|v| v.as_array())
                .filter(|sinks| !sinks.is_empty())
                .map(|sinks| serde_json::Value::from(sinks.clone()).to_string()),
            cpu_seconds: artifact.get("cpu_seconds").and_then(|v| v.as_f64()),
            max_rss_bytes: artifact.get("max_rss_bytes").and_then(|v| v.as_u64()),
            output_hash: str_of("content_hash").map(str::to_string),
            observation_hash: None,
            components: None,
            batch: str_of("batch").map(str::to_string),
        }
    }

    pub(crate) fn with_observation(mut self, revision: Option<String>) -> Self {
        self.observation_hash = revision;
        self
    }

    /// With the run hash's components recorded ([`crate::changes`]).
    pub(crate) fn with_components(mut self, components: Option<String>) -> Self {
        self.components = components;
        self
    }
}

/// Appends successful steps' rows, unless the run already has a row for the node under the same
/// run hash. A run requeued and resumed on new code keeps its run id, so a step it re-executes
/// under a new run hash gets a new row rather than leaving the earlier attempt's (#794).
/// Prepared once per transaction and rebound for each row (#460).
pub(crate) struct StepInsert(Prepared);

impl StepInsert {
    // Turso otherwise intersects the node and run indexes, scanning this run
    // for each partition. Bound deduplication to the node's own history.
    const SQL: &'static str = concat!(
        "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes, output_hash, bytes_changed, run_id, components, batch_id, observation_hash) SELECT ?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), COALESCE((SELECT output_hash FROM materializations ",
        by_node_run!(),
        " WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1) != NULLIF(?11, ''), 0), ?12, NULLIF(?13, ''), NULLIF(?14, ''), NULLIF(?15, '') WHERE NOT EXISTS (SELECT 1 FROM materializations ",
        by_node_run!(),
        " WHERE node_id = ?1 AND run_hash = ?2 AND run_id = ?12)"
    );

    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        prepare(conn, Self::SQL).await.map(Self)
    }

    /// Rows written: 1, or 0 when the run already recorded the node under this run hash.
    pub(crate) async fn run(
        &mut self,
        row: &NewMaterialization,
        run_id: &str,
    ) -> Result<u64, libsql::Error> {
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

/// Appends a permanently failed step as a `status = 'failed'` row (artifact columns NULL);
/// failed rows are never served as cache hits. Prepared once per transaction.
pub(crate) struct FailedStepInsert(Prepared);

impl FailedStepInsert {
    const SQL: &'static str = "INSERT INTO materializations (node_id, run_hash, status, error_type, error_message, error_traceback, attempts, run_id, batch_id, error_phase) VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6, ?7, NULLIF(?8, ''), NULLIF(?9, ''))";

    pub(crate) async fn prepare(conn: &Connection) -> Result<Self, libsql::Error> {
        prepare(conn, Self::SQL).await.map(Self)
    }

    /// `batch`: the batched call that computed the key, if any. The failure's own attempt
    /// count is recorded: dispatches for a worker failure, transfer attempts for an upload.
    pub(crate) async fn run(
        &mut self,
        failure: &crate::dispatch::StepFailure,
        run_hash: String,
        run_id: &str,
        batch: String,
    ) -> Result<u64, libsql::Error> {
        self.0
            .execute([
                failure.node_id.clone(),
                run_hash,
                failure.error.error_type.clone(),
                failure.error.message.clone(),
                failure.error.traceback.clone(),
                failure.error.attempts.to_string(),
                run_id.to_string(),
                batch,
                failure.error.phase.map_or("", |p| p.as_str()).to_string(),
            ])
            .await
    }
}

/// The digests the latest successful row of `node_id` under `run_hash` holds.
pub(crate) struct HeldHashes {
    /// Its output hash, when it recorded one (rows before digests were recorded have none).
    pub(crate) output: Option<String>,
    pub(crate) observation: Option<String>,
}

/// The latest successful row of `node_id` under `run_hash`, if any.
pub(crate) async fn held_hashes(
    conn: &Connection,
    node_id: &str,
    run_hash: &str,
) -> Result<Option<HeldHashes>, libsql::Error> {
    let mut rows = conn
        .query(
            "SELECT output_hash, observation_hash FROM materializations WHERE node_id = ?1 AND \
             run_hash = ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
            [node_id.to_string(), run_hash.to_string()],
        )
        .await?;
    let held = rows.next().await?.map(|row| HeldHashes {
        output: row.get::<String>(0).ok().filter(|h| !h.is_empty()),
        observation: row.get::<String>(1).ok(),
    });
    while rows.next().await?.is_some() {}
    Ok(held)
}

/// Record one step of a verified ledger post under `run_id`.
pub(crate) async fn insert_posted_step(
    conn: &Connection,
    run_id: &str,
    step: &crate::ledger_post::LedgerStep,
) -> Result<u64, libsql::Error> {
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
            run_id.to_string(),
            step.observation_hash.clone().unwrap_or_default(),
        ],
    )
    .await
}

// ─── Cache size limit (RFC-0012) ─────────────────────────────────────────────

/// Record that the results at `paths` (recorded artifact paths) were read now: by a step that
/// took them as inputs, by `barca get` returning them, or by `barca sql`. A result that is read
/// is on disk again, so an eviction recorded for it no longer holds. Best effort: a database
/// that cannot be written is left as it is.
pub(crate) async fn touch_read(db_path: &str, paths: &[String]) {
    if paths.is_empty() || !Path::new(db_path).exists() {
        return;
    }
    let _g = db_guard_for(db_path).await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return;
    };
    touch_read_on(&conn, paths).await;
}

/// Record the digest of the store's copy as the digest of the result stored at each location
/// (store location, SHA-256), on its latest successful row. A fetch found that copy without the
/// recorded digest and the run used it (`barca docs remote`, "Checking a local copy against the
/// store"): an artifact's path names the computation, so the store's copy is the result, and
/// the recorded digest names bytes that may exist nowhere (a `--refresh` killed between its
/// upload and its row, #576). Once recorded, this machine's copy, the store's and the row agree
/// and the next `get` or `status` finds nothing to report. Best effort.
pub(crate) async fn adopt_store_digests(db_path: &str, adopted: &[(String, String)]) {
    if adopted.is_empty() || !Path::new(db_path).exists() {
        return;
    }
    let _g = db_guard_for(db_path).await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return;
    };
    for (store, sha256) in adopted {
        let _ = conn
            .execute(
                "UPDATE materializations SET output_hash = ?2 WHERE id = (SELECT id FROM \
                 materializations WHERE artifact_path = ?1 AND status = 'success' \
                 ORDER BY id DESC LIMIT 1)",
                [store.clone(), sha256.clone()],
            )
            .await;
    }
}

/// [`touch_read`] on an open connection, in a transaction of its own. Best effort.
pub(crate) async fn touch_read_on(conn: &libsql::Connection, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let _ = with_tx(conn, "BEGIN IMMEDIATE", "read times", async {
        if touch_read_in(conn, paths).await {
            Ok(())
        } else {
            Err(BarcaError::Db("a read time was not recorded".into()))
        }
    })
    .await;
}

/// Set `last_read_at` of `paths` inside the caller's transaction. Returns whether every update
/// went through; it stops at the first that does not (the earlier ones stay in the transaction).
pub(crate) async fn touch_read_in(conn: &libsql::Connection, paths: &[String]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut ok = true;
    match prepare(
        conn,
        "UPDATE materializations SET last_read_at = datetime('now'), evicted_at = NULL \
         WHERE artifact_path = ?1 AND status = 'success'",
    )
    .await
    {
        Ok(mut touch) => {
            for path in paths {
                ok &= touch.execute([path.as_str()]).await.is_ok();
                if !ok {
                    break;
                }
            }
        }
        Err(_) => ok = false,
    }
    ok
}

/// Of `paths` (recorded artifact paths), those whose latest successful row was evicted to stay
/// under the cache limit: their missing file is an eviction, not a deletion behind barca's back.
pub(crate) async fn evicted_among(
    db_path: &str,
    paths: &[&str],
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if paths.is_empty() || !Path::new(db_path).exists() {
        return out;
    }
    let _g = db_guard_for(db_path).await;
    let Ok((_db, conn)) = open_conn(db_path).await else {
        return out;
    };
    for path in paths {
        let Ok(mut rows) = conn
            .query(
                "SELECT evicted_at IS NOT NULL FROM materializations WHERE artifact_path = ?1 \
                 AND status = 'success' ORDER BY id DESC LIMIT 1",
                [*path],
            )
            .await
        else {
            return out;
        };
        if let Ok(Some(row)) = rows.next().await
            && row.get::<i64>(0).unwrap_or(0) != 0
        {
            out.insert(path.to_string());
        }
    }
    out
}

/// Every recorded artifact path whose latest successful row is evicted (`barca status`,
/// `--dry-run`). Empty when the database predates the column.
pub(crate) async fn evicted_paths(cache: &CacheReader) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut rows) = cache
        .conn()
        .query(
            "WITH evicted AS (SELECT artifact_path, MAX(id) AS id \
             FROM materializations INDEXED BY idx_mat_evicted WHERE evicted_at IS NOT NULL \
             AND status = 'success' GROUP BY artifact_path) \
             SELECT e.artifact_path FROM evicted e WHERE NOT EXISTS ( \
             SELECT 1 FROM materializations n INDEXED BY idx_mat_path \
             WHERE n.artifact_path = e.artifact_path AND n.status = 'success' AND n.id > e.id)",
            (),
        )
        .await
    else {
        return out;
    };
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            out.insert(path);
        }
    }
    out
}

/// The node ids of `base`'s partition keys, `base[...]`, as a half-open range of the
/// `node_id` index: from `base[` up to `base\` (the character after `[`).
pub(crate) fn key_range(base: &str) -> (String, String) {
    (format!("{base}["), format!("{base}\\"))
}

/// The latest successful result of one key of a partitioned node.
#[derive(Debug, Clone)]
pub(crate) struct PartitionArtifact {
    /// `<base>[key]`.
    pub node_id: String,
    pub path: String,
    pub format: String,
    pub content_hash: Option<String>,
}

pub(crate) async fn partition_artifacts_for_config(
    cfg: &crate::config::ResolvedConfig,
    base_id: &str,
) -> Result<Vec<PartitionArtifact>, BarcaError> {
    latest_partition_artifacts_scoped(
        &cfg.db_path,
        base_id,
        cfg.remote_off.then_some(cfg.local_artifact_dir.as_str()),
    )
    .await
}

async fn latest_partition_artifacts_scoped(
    db_path: &str,
    base_id: &str,
    local_dir: Option<&str>,
) -> Result<Vec<PartitionArtifact>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let (lo, hi) = key_range(base_id);
    let mut rows = conn
        .query(
            concat!(
                "SELECT node_id, artifact_path, artifact_format, output_hash FROM materializations ",
                by_node_run!(),
                " WHERE node_id >= ?1 AND node_id < ?2 \
                 AND status = 'success' AND artifact_path IS NOT NULL AND artifact_path != '' \
                 ORDER BY id DESC"
            ),
            [lo, hi],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
    let mut seen = std::collections::BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        if local_dir.is_some_and(|root| {
            !crate::config::local_artifact(&row.get::<String>(1).unwrap_or_default(), root)
        }) {
            continue;
        }
        let node_id = row.get::<String>(0).unwrap_or_default();
        seen.entry(node_id).or_insert_with(|| PartitionArtifact {
            node_id: String::new(),
            path: row.get::<String>(1).unwrap_or_default(),
            format: row.get::<String>(2).unwrap_or_default(),
            content_hash: row.get::<String>(3).ok().filter(|h| !h.is_empty()),
        });
    }
    Ok(seen
        .into_iter()
        .map(|(node_id, a)| PartitionArtifact { node_id, ..a })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_history_queries_are_bounded_and_keep_each_partition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        // Filled outside the process-wide guard: the 500k rows take seconds to write, and a
        // concurrent test's timed read must not wait for them.
        let db = libsql::Builder::new_local(&path).build().await.unwrap();
        let fill = db.connect().unwrap();
        fill.execute(
            "WITH RECURSIVE history(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM history WHERE n < 500000) \
             INSERT INTO materializations(node_id, run_hash, output_hash, artifact_path, evicted_at) \
             SELECT 'sensor', 'run', 'old', 'restored', 'yesterday' FROM history", (),
        ).await.unwrap();
        fill.execute(
            "INSERT INTO materializations(node_id, output_hash, observation_hash, artifact_path, evicted_at) VALUES \
             ('sensor', 'bytes', 'revision', 'restored', NULL), \
             ('sensor[key=a]', 'a-old', NULL, 'a', NULL), \
             ('sensor[key=b]', 'b', NULL, 'b', 'yesterday'), \
             ('sensor[key=a]', 'a', NULL, 'a', NULL), \
             ('other', 'other', NULL, 'other', NULL)", (),
        ).await.unwrap();
        drop(fill);
        drop(db);
        let cache = super::CacheReader::open(&path).await.unwrap();
        let started = std::time::Instant::now();
        let hashes = super::last_output_hashes(&cache, &["sensor"])
            .await
            .unwrap();
        assert_eq!(
            hashes,
            std::collections::HashMap::from([
                ("sensor".into(), "revision".into()),
                ("sensor[key=a]".into(), "a".into()),
                ("sensor[key=b]".into(), "b".into()),
            ])
        );
        assert_eq!(
            super::evicted_paths(&cache).await,
            std::collections::HashSet::from(["b".into()])
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "status queries took {:?} for 500k historical rows",
            started.elapsed()
        );
    }

    /// #576: the store copy's digest is recorded on the latest successful row at its location,
    /// and on no other row.
    #[tokio::test]
    async fn an_adopted_store_digest_is_recorded_on_the_latest_row_at_that_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        let rows = [
            ("s3://b/a/n/h.json", "success", "first"),
            ("s3://b/a/n/h.json", "success", "second"),
            ("s3://b/a/n/h.json", "failed", ""),
            ("s3://b/a/m/h.json", "success", "other"),
        ];
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for (at, status, hash) in rows {
                conn.execute(
                    "INSERT INTO materializations (node_id, run_hash, artifact_path, status, \
                         output_hash) VALUES ('p.py:n', 'h', ?1, ?2, NULLIF(?3, ''))",
                    [at, status, hash],
                )
                .await
                .unwrap();
            }
        }
        let adopted = [("s3://b/a/n/h.json".to_string(), "new".to_string())];
        super::adopt_store_digests(&path, &adopted).await;
        let _guard = super::db_guard().await;
        let (_db, conn) = super::open_conn(&path).await.unwrap();
        let mut found = conn
            .query(
                "SELECT COALESCE(output_hash, '') FROM materializations ORDER BY id",
                (),
            )
            .await
            .unwrap();
        let mut hashes = Vec::new();
        while let Some(row) = found.next().await.unwrap() {
            hashes.push(row.get::<String>(0).unwrap());
        }
        assert_eq!(hashes, ["first", "new", "", "other"]);
    }

    #[tokio::test]
    async fn round_trip_persist_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();

        init_db(&db_path).await.unwrap();

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "test.py:foo".to_string(),
            OutputRef {
                path: ".barca/artifacts/test.py--foo.json".to_string(),
                format: "json".to_string(),
                size_bytes: 15,
                elapsed_seconds: None,
                content_hash: None,
            },
        );
        persist_outputs(&db_path, &outputs, &HashMap::new())
            .await
            .unwrap();

        // Read it back.
        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1",
                ["test.py:foo".to_string()],
            )
            .await
            .unwrap();
        let result = rows.next().await.unwrap().map(|row| {
            (
                row.get::<String>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
            )
        });

        let (path, format, size) = result.unwrap();
        assert_eq!(path, ".barca/artifacts/test.py--foo.json");
        assert_eq!(format, "json");
        assert_eq!(size, 15);
    }

    // ─── OutputRef artifact persistence tests ────────────────────────────────

    #[tokio::test]
    async fn round_trip_persist_output_ref() {
        use crate::dispatch::OutputRef;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "test.py:foo".to_string(),
            OutputRef {
                path: ".barca/artifacts/test.py--foo.json".to_string(),
                format: "json".to_string(),
                size_bytes: 42,
                elapsed_seconds: None,
                content_hash: None,
            },
        );

        persist_outputs(&db_path, &outputs, &HashMap::new())
            .await
            .unwrap();

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1",
                ["test.py:foo".to_string()],
            )
            .await
            .unwrap();
        let result = rows.next().await.unwrap().map(|row| {
            (
                row.get::<String>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
            )
        });

        let (path, format, size) = result.unwrap();
        assert_eq!(path, ".barca/artifacts/test.py--foo.json");
        assert_eq!(format, "json");
        assert_eq!(size, 42);
    }

    #[tokio::test]
    async fn persist_multiple_output_refs() {
        use crate::dispatch::OutputRef;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let mut outputs: HashMap<String, OutputRef> = HashMap::new();
        outputs.insert(
            "a.py:json_asset".to_string(),
            OutputRef {
                path: ".barca/artifacts/a--json_asset.json".to_string(),
                format: "json".to_string(),
                size_bytes: 100,
                elapsed_seconds: None,
                content_hash: None,
            },
        );
        outputs.insert(
            "a.py:df_asset".to_string(),
            OutputRef {
                path: ".barca/artifacts/a--df_asset.parquet".to_string(),
                format: "parquet".to_string(),
                size_bytes: 8192,
                elapsed_seconds: None,
                content_hash: None,
            },
        );
        outputs.insert(
            "a.py:obj_asset".to_string(),
            OutputRef {
                path: ".barca/artifacts/a--obj_asset.pkl".to_string(),
                format: "pickle".to_string(),
                size_bytes: 512,
                elapsed_seconds: None,
                content_hash: None,
            },
        );

        persist_outputs(&db_path, &outputs, &HashMap::new())
            .await
            .unwrap();

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query("SELECT COUNT(*) FROM materializations", ())
            .await
            .unwrap();
        let count = rows
            .next()
            .await
            .unwrap()
            .map(|row| row.get::<i64>(0).unwrap())
            .unwrap();

        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn failed_row_round_trips_and_is_excluded_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        let (_db, conn) = open_conn(&db_path).await.unwrap();
        // Insert a failed materialization.
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, status, error_message, error_traceback, attempts) VALUES (?1, ?2, 'failed', ?3, ?4, ?5)",
            ["f:boom".to_string(), "rh1".to_string(), "kaboom".to_string(), "Traceback…".to_string(), "3".to_string()],
        )
        .await
        .unwrap();

        // Read it back.
        let mut rows = conn
            .query(
                "SELECT status, error_message, attempts FROM materializations WHERE node_id = ?1",
                ["f:boom".to_string()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let status = row.get::<String>(0).unwrap();
        let msg = row.get::<String>(1).unwrap();
        let attempts = row.get::<i64>(2).unwrap();

        // The cache lookup filters on status='success' — a failed row is never served.
        let mut hit_rows = conn
            .query(
                "SELECT artifact_path FROM materializations WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
                ["f:boom".to_string(), "rh1".to_string()],
            )
            .await
            .unwrap();
        let success_hits = hit_rows.next().await.unwrap().is_some();

        assert_eq!(status, "failed");
        assert_eq!(msg, "kaboom");
        assert_eq!(attempts, 3);
        assert!(
            !success_hits,
            "failed rows must not satisfy the cache lookup"
        );
    }

    #[tokio::test]
    async fn cache_lookup_returns_output_ref() {
        use crate::dispatch::OutputRef;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // Persist with a run_hash.
        {
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
                [
                    "test.py:foo".to_string(),
                    "abc123".to_string(),
                    ".barca/artifacts/test.py--foo.parquet".to_string(),
                    "parquet".to_string(),
                    "4096".to_string(),
                ],
            )
            .await
            .unwrap();
        }

        // Look it up by node_id + run_hash — should return OutputRef.
        let (_db, conn) = open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1 AND run_hash = ?2 ORDER BY id DESC LIMIT 1",
                ["test.py:foo".to_string(), "abc123".to_string()],
            )
            .await
            .unwrap();
        let result = rows.next().await.unwrap().map(|row| OutputRef {
            path: row.get::<String>(0).unwrap(),
            format: row.get::<String>(1).unwrap(),
            size_bytes: row.get::<i64>(2).unwrap() as u64,
            elapsed_seconds: None,
            content_hash: None,
        });

        let output_ref = result.unwrap();
        assert_eq!(output_ref.path, ".barca/artifacts/test.py--foo.parquet");
        assert_eq!(output_ref.format, "parquet");
        assert_eq!(output_ref.size_bytes, 4096);
    }
}
