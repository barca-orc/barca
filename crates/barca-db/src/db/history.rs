//! Each node's history as `barca status` and `GET /state` summarize it: its age in runs
//! ([`node_ages`]), its last materialization and its run totals ([`node_histories`]), and the
//! nodes whose bytes changed in a run ([`changed_bytes_in_run`]).

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// One row of `materializations`, as `barca status` reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializationRecord {
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
    #[serde(default)]
    pub skip_reason: Option<String>,
    pub created_at: String,
    /// Rows of a successful tabular result, when its worker counted them (#925).
    #[serde(default)]
    pub rows: Option<i64>,
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
pub async fn changed_bytes_in_run(db_path: &str, run_id: &str) -> Result<Vec<String>, BarcaError> {
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
    /// First clean result after the most recent failed attempt.
    pub recovered_at: Option<String>,
    /// The first failed attempt after its last clean run: `None` unless its latest attempts failed.
    pub failing_since: Option<String>,
    /// How many runs in a row failed it, up to now. A run counts once however many keys failed.
    pub consecutive_failures: u32,
    /// Its newest run as a whole; `None` until a run recorded a row for it.
    pub last_run: Option<RunTotals>,
    /// The row counts of its latest successful results (#925); `None` until one succeeded.
    pub rows: Option<RowFacts>,
    /// When its result last changed, oldest first, as recorded (`created_at`): the
    /// successful rows whose run hash or observation differs from the
    /// previous success of that step, or whose bytes changed (including the first result). A sensor
    /// polled with the same observation, or a rerun that wrote the same bytes, adds a row and
    /// no change: what reads the node is not out of date against it.
    pub changed_at: std::sync::Arc<Vec<String>>,
}

/// What a node's latest successful results say about their rows: each key's (the node's own,
/// for a node without keys) latest success against the success before it (#925). Only row
/// counts a worker recorded are read; a key whose count is unknown is left out of every figure.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowFacts {
    /// Whether the rows are of partition keys (`node[k=v]`).
    pub partitioned: bool,
    /// Keys whose latest success recorded a row count.
    pub counted: u32,
    /// Of them, those that wrote no rows.
    pub empty: u32,
    /// Keys that were empty as of before the node's newest run (each key's latest success
    /// older than that run, or the success before it for a key that run wrote); `None` when no
    /// key had a counted success before it.
    pub previous_empty: Option<u32>,
    /// Keys whose latest count differs from the one before it by more than half of it
    /// ([`rows_changed`]).
    pub changed: u32,
    /// The largest of those changes, relative to the count before it.
    pub largest_change: Option<RowChange>,
    /// The newest successful row of any key, and its run: what "the node's newest run" is.
    pub newest: (i64, Option<String>),
}

/// One key's row count against the one before it.
#[derive(Debug, Clone, PartialEq)]
pub struct RowChange {
    /// The key (`k=a`); `None` for a node without keys.
    pub key: Option<String>,
    pub rows: i64,
    pub previous_rows: i64,
}

/// Whether a count moved from `previous` to `rows` by more than half of `previous` (any rows
/// after an empty result count; empty after empty does not).
pub fn rows_changed(rows: i64, previous: i64) -> bool {
    (rows - previous).abs() * 2 > previous
}

/// A step's newest successful row, as [`NodeAgesIndex`] keeps it for its row counts.
#[derive(Debug, Clone, PartialEq)]
struct Success {
    id: i64,
    run: Option<String>,
    rows: Option<i64>,
}

/// A step's two newest successful rows.
#[derive(Debug, Clone, Default)]
struct KeySuccesses {
    latest: Option<Success>,
    previous: Option<Success>,
    /// The newest successful row whose content was read ([`AgeRow::content`]): its id and
    /// content.
    content: Option<(i64, u64)>,
}

impl KeySuccesses {
    /// Fold in a row, in any order: the two with the highest ids are kept.
    fn add(&mut self, row: Success) {
        match &self.latest {
            Some(latest) if latest.id == row.id => {}
            Some(latest) if latest.id > row.id => {
                if self.previous.as_ref().is_none_or(|p| p.id < row.id) {
                    self.previous = Some(row);
                }
            }
            _ => self.previous = self.latest.replace(row),
        }
    }
}

impl RowFacts {
    /// From each step's two newest successes (`steps`: node id -> them).
    fn of<'a>(
        base: &str,
        steps: impl Iterator<Item = (&'a String, &'a KeySuccesses)>,
    ) -> Option<Self> {
        let steps: Vec<(&String, &KeySuccesses)> = steps.collect();
        let newest = steps
            .iter()
            .filter_map(|(_, s)| s.latest.as_ref())
            .max_by_key(|s| s.id)
            .map(|s| (s.id, s.run.clone()))?;
        let mut facts = RowFacts {
            partitioned: steps.iter().any(|(id, _)| id.len() > base.len()),
            newest: newest.clone(),
            ..Default::default()
        };
        let mut before_counted = 0u32;
        let mut before_empty = 0u32;
        let mut largest: Option<(f64, RowChange)> = None;
        for (id, s) in &steps {
            let Some(latest) = &s.latest else { continue };
            let key = (id.len() > base.len()).then(|| {
                id[base.len()..]
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string()
            });
            if let Some(rows) = latest.rows {
                facts.counted += 1;
                if rows == 0 {
                    facts.empty += 1;
                }
            }
            // As of before the newest run: this key's latest, unless the newest run wrote it.
            let in_newest = latest.id == newest.0 || (newest.1.is_some() && latest.run == newest.1);
            let before = if in_newest {
                s.previous.as_ref()
            } else {
                Some(latest)
            };
            if let Some(rows) = before.and_then(|b| b.rows) {
                before_counted += 1;
                if rows == 0 {
                    before_empty += 1;
                }
            }
            if let (Some(rows), Some(previous)) =
                (latest.rows, s.previous.as_ref().and_then(|p| p.rows))
                && rows_changed(rows, previous)
            {
                facts.changed += 1;
                let relative = (rows - previous).abs() as f64 / previous.max(1) as f64;
                if largest
                    .as_ref()
                    .is_none_or(|(r, c)| relative > *r || (relative == *r && key < c.key))
                {
                    largest = Some((
                        relative,
                        RowChange {
                            key: key.clone(),
                            rows,
                            previous_rows: previous,
                        },
                    ));
                }
            }
        }
        facts.previous_empty = (before_counted > 0).then_some(before_empty);
        facts.largest_change = largest.map(|(_, c)| c);
        Some(facts)
    }
}

/// One run's rows for one node, keys folded into their base id: what the run cost on the node
/// as a whole, where `elapsed_seconds` of a key's row is that key's share alone. A node's
/// `last_run` in `GET /state` (`last_materialization.elapsed_seconds` is one key's share, this
/// is every key of the run added up, over the batched calls they ran in).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunTotals {
    pub run_id: String,
    /// Wall time summed over the rows that recorded one; `null` when none did (every key
    /// failed before finishing).
    pub elapsed_seconds: Option<f64>,
    /// Rows the run recorded for the node: its keys, or 1.
    pub keys: u32,
    /// Batched calls (RFC-0013) those keys ran in; `0` when each ran on its own.
    pub calls: u32,
    /// Keys that failed.
    pub failed: u32,
}

/// The base id of a materialization row's node id (`<base>[k=v]` for a key).
fn base_of(node_id: &str) -> &str {
    node_id.split_once('[').map_or(node_id, |(base, _)| base)
}

/// [`NodeAge`] of every node with history, read in full: [`NodeAgesIndex::read`] on a new index.
/// Read-only; the caller ensures the DB exists.
pub async fn node_ages(db_path: &str) -> Result<HashMap<String, NodeAge>, BarcaError> {
    NodeAgesIndex::default().read(db_path).await
}

/// The rows a [`NodeAgesIndex`] has folded in: those with an id up to `max_id`, `rows` of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    max_id: i64,
    rows: i64,
}

/// [`NodeAge`] of every node, kept between reads of one database (one index per database) so that a read after a run
/// recorded a few rows costs those rows, not the table: a busy server records results several
/// times a second, and reading all of them again took 300 ms for 470k rows on every poll of
/// `GET /state`.
///
/// An attempt is a run, and a run with a failed key is a failed attempt even when other keys
/// succeeded. Rows without a run id (an older barca's) are each their own attempt. The columns
/// read here are never updated, so rows added since the last read are all that can have
/// changed, unless some were deleted (`barca gc`, a reset): then the row count does not add up
/// and every row is read again.
#[derive(Debug, Default)]
pub struct NodeAgesIndex {
    mark: Option<Mark>,
    /// base id -> run -> (its newest failed row, its earliest failure's time).
    failed: HashMap<String, HashMap<String, (i64, String)>>,
    /// base id -> (its newest row, that row's run).
    newest: HashMap<String, (i64, Option<String>)>,
    /// base id -> (newest successful row and its time, newest row of a run that did not fail it).
    success: HashMap<String, (i64, String, i64)>,
    /// base id -> its newest run's totals ([`RunTotals`]).
    last_run: HashMap<String, RunTotals>,
    /// base id -> each of its steps (the node itself, or its keys) -> its two newest successes.
    successes: HashMap<String, HashMap<String, KeySuccesses>>,
    /// base id -> [`RowFacts`], computed again for the bases a read touched.
    rows: HashMap<String, RowFacts>,
    /// Latest failure row and the first clean success after it.
    recovery: HashMap<String, (i64, Option<String>)>,
    /// base id -> [`NodeAge::changed_at`].
    changes: HashMap<String, std::sync::Arc<Vec<String>>>,
}

/// One row as [`NodeAgesIndex`] folds it in.
struct AgeRow {
    id: i64,
    node: String,
    run: Option<String>,
    created_at: String,
    /// The `rows` column (#925); always `None` for a failed row.
    rows: Option<i64>,
    /// What a successful row holds, as one number: its run hash and observation hash. Two rows
    /// of a step with the same content are the same result. `None` for a failed row.
    content: Option<u64>,
    bytes_changed: bool,
}

impl AgeRow {
    /// Columns `id, node_id, run_id, created_at, rows`, then `run_hash, observation_hash, bytes_changed` when
    /// `content` (a successful row).
    fn of(row: &libsql::Row, content: bool) -> Self {
        use std::hash::{Hash, Hasher};
        Self {
            id: row.get::<i64>(0).unwrap_or_default(),
            node: row.get::<String>(1).unwrap_or_default(),
            run: row.get::<String>(2).ok().filter(|r| !r.is_empty()),
            created_at: row.get::<String>(3).unwrap_or_default(),
            rows: row.get::<i64>(4).ok(),
            bytes_changed: content && row.get::<i64>(7).unwrap_or(0) != 0,
            content: content.then(|| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                row.get::<String>(5).ok().hash(&mut hasher);
                row.get::<String>(6).ok().hash(&mut hasher);
                hasher.finish()
            }),
        }
    }
}

impl NodeAgesIndex {
    /// Every node's age in the database at `db_path` now, folding in what was added since the
    /// last read.
    pub async fn read(&mut self, db_path: &str) -> Result<HashMap<String, NodeAge>, BarcaError> {
        let _g = db_guard_for(db_path).await;
        let (_h, conn) = open_conn(db_path).await?;
        let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
        // One snapshot for the mark and the rows, so a row written in between is either in both
        // or in neither.
        conn.execute("BEGIN", ()).await.map_err(failure)?;
        let result = self.read_in(&conn, db_path).await;
        let _ = conn.execute("COMMIT", ()).await;
        if result.is_err() {
            // Partly folded in: the next read starts again from nothing.
            *self = Self::default();
        }
        result
    }

    async fn read_in(
        &mut self,
        conn: &libsql::Connection,
        db_path: &str,
    ) -> Result<HashMap<String, NodeAge>, BarcaError> {
        let now = read_mark(conn, db_path).await?;
        let added = match self.mark {
            Some(mark) if mark == now => Some(Vec::new()),
            Some(mark) if mark.max_id <= now.max_id => {
                let added = rows_between(conn, mark.max_id, now.max_id).await?;
                (mark.rows + added.len() as i64 == now.rows).then_some(added)
            }
            _ => None,
        };
        let touched = match added {
            Some(added) => self.fold_added(conn, added).await?,
            None => {
                *self = Self::default();
                self.fold_all(conn, now.max_id).await?
            }
        };
        self.mark = Some(now);
        self.read_last_runs(conn, &touched).await?;
        for base in &touched {
            match self
                .successes
                .get(base)
                .and_then(|steps| RowFacts::of(base, steps.iter()))
            {
                Some(facts) => self.rows.insert(base.clone(), facts),
                None => self.rows.remove(base),
            };
        }
        Ok(self.ages())
    }

    /// Fold in every row up to `max_id`: the failed ones (`idx_mat_failed`), then one pass over
    /// the successful ones.
    async fn fold_all(
        &mut self,
        conn: &libsql::Connection,
        max_id: i64,
    ) -> Result<HashSet<String>, BarcaError> {
        let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
        let mut touched = HashSet::new();
        let mut rows = conn
            .query(
                "SELECT id, node_id, run_id, created_at, NULL FROM materializations \
                 WHERE status = 'failed' ORDER BY run_id, id",
                (),
            )
            .await
            .map_err(failure)?;
        while let Some(row) = rows.next().await.map_err(failure)? {
            let row = AgeRow::of(&row, false);
            if row.id <= max_id {
                self.fold_failed(row, &mut touched);
            }
        }
        drop(rows);
        let mut rows = conn
            .query(
                // In row order: a change is a row against the one before it.
                "SELECT id, node_id, run_id, created_at, rows, run_hash, observation_hash, bytes_changed \
                 FROM materializations WHERE status = 'success' AND id <= ?1 ORDER BY id",
                [max_id],
            )
            .await
            .map_err(failure)?;
        while let Some(row) = rows.next().await.map_err(failure)? {
            self.fold_success(AgeRow::of(&row, true), &mut touched);
        }
        Ok(touched)
    }

    /// Fold in rows added since the last read: the failed ones first, as [`Self::fold_all`]
    /// does, so that a success is judged against every failure. A run that newly failed a node
    /// can make the node's newest clean success an older one: that node is read again.
    async fn fold_added(
        &mut self,
        conn: &libsql::Connection,
        added: Vec<(AgeRow, bool)>,
    ) -> Result<HashSet<String>, BarcaError> {
        let mut touched = HashSet::new();
        let mut reclean = HashSet::new();
        let (failed, succeeded): (Vec<_>, Vec<_>) = added.into_iter().partition(|(_, f)| *f);
        for (row, _) in failed {
            let base = base_of(&row.node).to_string();
            let run = attempt(row.run.clone(), row.id);
            let new_failure = self
                .failed
                .get(&base)
                .is_none_or(|runs| !runs.contains_key(&run));
            if new_failure && self.success.get(&base).is_some_and(|s| s.2 > 0) {
                reclean.insert(base);
            }
            self.fold_failed(row, &mut touched);
        }
        for (row, _) in succeeded {
            self.fold_success(row, &mut touched);
        }
        for base in reclean {
            let clean = self.newest_clean_success(conn, &base).await?;
            if let Some(entry) = self.success.get_mut(&base) {
                entry.2 = clean;
            }
        }
        Ok(touched)
    }

    fn fold_failed(&mut self, row: AgeRow, touched: &mut HashSet<String>) {
        let base = base_of(&row.node);
        note_newest(&mut self.newest, base, row.id, row.run.clone());
        let run = attempt(row.run, row.id);
        let id = row.id;
        let recovery = self.recovery.entry(base.to_string()).or_default();
        if id > recovery.0 {
            *recovery = (id, None);
        }
        self.failed
            .entry(base.to_string())
            .or_default()
            .entry(run)
            .and_modify(|seen| seen.0 = seen.0.max(id))
            .or_insert((id, row.created_at));
        if !touched.contains(base) {
            touched.insert(base.to_string());
        }
    }

    fn fold_success(&mut self, row: AgeRow, touched: &mut HashSet<String>) {
        let base = base_of(&row.node);
        note_newest(&mut self.newest, base, row.id, row.run.clone());
        let steps = match self.successes.get_mut(base) {
            Some(steps) => steps,
            None => self.successes.entry(base.to_string()).or_default(),
        };
        let success = Success {
            id: row.id,
            run: row.run.clone(),
            rows: row.rows,
        };
        let step = match steps.get_mut(&row.node) {
            Some(step) => step,
            None => steps.entry(row.node.clone()).or_default(),
        };
        step.add(success);
        // Rows are folded in row order; one out of order is not compared.
        if let Some(content) = row.content
            && step.content.is_none_or(|(id, _)| id < row.id)
        {
            let changed =
                row.bytes_changed || step.content.is_none_or(|(_, before)| before != content);
            step.content = Some((row.id, content));
            if changed {
                let changes = match self.changes.get_mut(base) {
                    Some(changes) => changes,
                    None => self.changes.entry(base.to_string()).or_default(),
                };
                if changes.last() != Some(&row.created_at) {
                    let changes = std::sync::Arc::make_mut(changes);
                    changes.push(row.created_at.clone());
                }
            }
        }
        let clean = self
            .failed
            .get(base)
            .is_none_or(|runs| row.run.as_ref().is_none_or(|r| !runs.contains_key(r)));
        if clean
            && let Some((failure, recovered)) = self.recovery.get_mut(base)
            && row.id > *failure
            && recovered.is_none()
        {
            *recovered = Some(row.created_at.clone());
        }
        let entry = match self.success.get_mut(base) {
            Some(entry) => entry,
            None => self.success.entry(base.to_string()).or_default(),
        };
        if row.id > entry.0 {
            entry.0 = row.id;
            entry.1 = row.created_at;
        }
        if clean {
            entry.2 = entry.2.max(row.id);
        }
        if !touched.contains(base) {
            touched.insert(base.to_string());
        }
    }

    /// `base`'s newest successful row of a run that did not fail it (0 if none), read from its
    /// own rows and its keys' range.
    async fn newest_clean_success(
        &self,
        conn: &libsql::Connection,
        base: &str,
    ) -> Result<i64, BarcaError> {
        let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
        let (lo, hi) = key_range(base);
        let mut rows = conn
            .query(
                "SELECT id, run_id FROM ( \
                 SELECT id, run_id FROM materializations WHERE node_id = ?1 AND status = 'success' \
                 UNION ALL \
                 SELECT id, run_id FROM materializations \
                 WHERE node_id >= ?2 AND node_id < ?3 AND status = 'success') ORDER BY id DESC",
                libsql::params![base, lo, hi],
            )
            .await
            .map_err(failure)?;
        let failed = self.failed.get(base);
        while let Some(row) = rows.next().await.map_err(failure)? {
            let run = row.get::<String>(1).ok().filter(|r| !r.is_empty());
            if failed.is_none_or(|runs| run.is_none_or(|r| !runs.contains_key(&r))) {
                return Ok(row.get::<i64>(0).unwrap_or_default());
            }
        }
        Ok(0)
    }

    /// The newest run's rows of each of `bases`, two index seeks per node (`idx_mat_run_node`):
    /// the node's own row and its keys' range, apart, since SQLite seeks one range per index
    /// and an `OR` of the two reads every row of the run instead. A run requeued onto new code
    /// can hold two rows for a key, one per run hash (#794), so keys are counted distinct. Rows
    /// without a run id (an older barca's) leave it unknown.
    async fn read_last_runs(
        &mut self,
        conn: &libsql::Connection,
        bases: &HashSet<String>,
    ) -> Result<(), BarcaError> {
        let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
        for base in bases {
            self.last_run.remove(base);
            let Some((_, Some(run))) = self.newest.get(base) else {
                continue;
            };
            let (lo, hi) = key_range(base);
            let mut rows = conn
                .query(
                    LAST_RUN_TOTALS,
                    libsql::params![run.clone(), base.clone(), lo, hi],
                )
                .await
                .map_err(failure)?;
            if let Some(row) = rows.next().await.map_err(failure)? {
                self.last_run.insert(
                    base.clone(),
                    RunTotals {
                        run_id: run.clone(),
                        elapsed_seconds: row.get::<f64>(0).ok(),
                        keys: row.get::<i64>(1).unwrap_or_default() as u32,
                        calls: row.get::<i64>(2).unwrap_or_default() as u32,
                        failed: row.get::<i64>(3).unwrap_or_default() as u32,
                    },
                );
            }
        }
        Ok(())
    }

    fn ages(&self) -> HashMap<String, NodeAge> {
        let mut out: HashMap<String, NodeAge> = HashMap::with_capacity(self.success.len());
        for (base, runs) in &self.failed {
            let clean = self.success.get(base).map_or(0, |s| s.2);
            let trailing = runs.values().filter(|(newest, _)| *newest > clean);
            out.insert(
                base.clone(),
                NodeAge {
                    recovered_at: self.recovery.get(base).and_then(|(_, at)| at.clone()),
                    last_success_at: None,
                    failing_since: trailing.clone().map(|(_, at)| at.clone()).min(),
                    consecutive_failures: trailing.count() as u32,
                    ..NodeAge::default()
                },
            );
        }
        for (base, (_, at, _)) in &self.success {
            out.entry(base.clone()).or_default().last_success_at = Some(at.clone());
        }
        for (base, totals) in &self.last_run {
            out.entry(base.clone()).or_default().last_run = Some(totals.clone());
        }
        for (base, facts) in &self.rows {
            out.entry(base.clone()).or_default().rows = Some(facts.clone());
        }
        for (base, changes) in &self.changes {
            out.entry(base.clone()).or_default().changed_at = changes.clone();
        }
        out
    }
}

/// A node's rows in one run, totalled: `?1` the run, `?2` the node, `?3`..`?4` its keys' range.
const LAST_RUN_TOTALS: &str = "SELECT sum(elapsed_seconds), count(DISTINCT node_id), \
     count(DISTINCT batch_id), count(DISTINCT CASE WHEN status = 'failed' THEN node_id END) FROM ( \
     SELECT node_id, elapsed_seconds, batch_id, status FROM materializations \
     WHERE run_id = ?1 AND node_id = ?2 UNION ALL \
     SELECT node_id, elapsed_seconds, batch_id, status FROM materializations \
     WHERE run_id = ?1 AND node_id >= ?3 AND node_id < ?4)";

/// Rows with an id in `(after, upto]`, each with whether it failed: a range of the row id.
async fn rows_between(
    conn: &libsql::Connection,
    after: i64,
    upto: i64,
) -> Result<Vec<(AgeRow, bool)>, BarcaError> {
    let failure = |e| BarcaError::Db(format!("failed to read node ages: {e}"));
    let mut rows = conn
        .query(
            "SELECT id, node_id, run_id, created_at, rows, run_hash, observation_hash, bytes_changed, \
             status = 'failed' FROM materializations \
             WHERE id > ?1 AND id <= ?2 AND status IN ('success', 'failed') ORDER BY id",
            [after, upto],
        )
        .await
        .map_err(failure)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failure)? {
        let failed = row.get::<i64>(8).unwrap_or(0) != 0;
        out.push((AgeRow::of(&row, !failed), failed));
    }
    Ok(out)
}

/// An attempt's identity: its run, or for a row without one (an older barca's) the row itself.
fn attempt(run: Option<String>, id: i64) -> String {
    run.unwrap_or_else(|| format!("#{id}"))
}

fn note_newest(
    newest: &mut HashMap<String, (i64, Option<String>)>,
    base: &str,
    id: i64,
    run: Option<String>,
) {
    match newest.get_mut(base) {
        Some(entry) if id >= entry.0 => *entry = (id, run),
        Some(_) => {}
        None => {
            newest.insert(base.to_string(), (id, run));
        }
    }
}

/// What the metadata DB knows about one node (all partition keys of a partitioned node).
#[derive(Debug, Clone, Default)]
pub struct NodeHistory {
    pub last_bytes_change: Option<BytesChange>,
    /// The most recent materialization attempt, successful or not.
    pub latest: Option<MaterializationRecord>,
    /// Whether any attempt ever succeeded.
    pub ever_succeeded: bool,
    /// The row ids of `latest` and `last_bytes_change`, which a kept history reads again
    /// ([`node_histories`]): a digest can be recorded after the row is written (an upload's,
    /// #576), and another writer can change any column.
    pub latest_id: Option<i64>,
    pub bytes_id: Option<i64>,
}

/// Latest materialization and success flag for each base node id. Rows of partitioned steps
/// (`<base>[k=v]`) count toward their base id. Read-only; the caller ensures the DB exists.
pub async fn node_histories(
    db_path: &str,
    base_ids: &[String],
) -> Result<HashMap<String, NodeHistory>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let failure = |e| BarcaError::Db(format!("failed to read node histories: {e}"));
    // A read view is a link to the database: one index per file it names.
    let file = fs::canonicalize(db_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| db_path.to_string());
    let mut kept = HISTORIES.lock().await;
    make_room(&mut kept, &file);
    let index = kept.entry(file).or_default();
    // One snapshot for the mark and the rows, so a row written in between is either in both
    // or in neither.
    conn.execute("BEGIN", ()).await.map_err(failure)?;
    let result = index.read(&conn, db_path, base_ids).await;
    let _ = conn.execute("COMMIT", ()).await;
    result
}

/// [`node_histories`] kept per database file in this process, so that a `barca serve` status
/// read after a run recorded a few rows reads the histories of the nodes those rows belong to,
/// not of all of them (six queries per node: 200 ms for 650 nodes, on every poll of a busy
/// server). Rows deleted since (`barca gc`, a reset) make the row count not add up, and every
/// history is read again.
static HISTORIES: std::sync::LazyLock<tokio::sync::Mutex<HashMap<String, HistoriesIndex>>> =
    std::sync::LazyLock::new(Default::default);

#[derive(Default)]
struct HistoriesIndex {
    mark: Option<Mark>,
    nodes: HashMap<String, NodeHistory>,
}

impl HistoriesIndex {
    async fn read(
        &mut self,
        conn: &libsql::Connection,
        db_path: &str,
        base_ids: &[String],
    ) -> Result<HashMap<String, NodeHistory>, BarcaError> {
        match changed_since(conn, db_path, &mut self.mark).await? {
            Some(touched) => {
                for base in touched {
                    self.nodes.remove(&base);
                }
            }
            None => self.nodes.clear(),
        }
        self.answer(conn, base_ids).await
    }

    /// The histories of `base_ids`: kept ones with the rows they report read again (a digest is
    /// recorded on a row after it is written, #576, and another writer may change any column),
    /// the others read in full.
    async fn answer(
        &mut self,
        conn: &libsql::Connection,
        base_ids: &[String],
    ) -> Result<HashMap<String, NodeHistory>, BarcaError> {
        let mut out = HashMap::with_capacity(base_ids.len());
        let mut kept_ids = Vec::new();
        let mut was_kept = HashSet::new();
        for base in base_ids {
            match self.nodes.get(base) {
                Some(h) => {
                    was_kept.insert(base.as_str());
                    kept_ids.extend(h.latest_id);
                    kept_ids.extend(h.bytes_id);
                }
                None => {
                    let h = node_history(conn, base).await?;
                    self.nodes.insert(base.clone(), h);
                }
            }
        }
        let rows = reported_rows(conn, kept_ids).await?;
        for base in base_ids {
            let Some(h) = self.nodes.get_mut(base) else {
                continue;
            };
            if was_kept.contains(base.as_str()) {
                h.report(&rows);
            }
            out.insert(base.clone(), h.clone());
        }
        Ok(out)
    }
}

/// The base ids with rows added to `materializations` since `mark`, which is moved to now; `None`
/// when rows were deleted or `mark` is unset, and whatever was kept must be read again. Call it
/// inside a transaction, so that the rows read after it are those it counted.
pub(crate) async fn changed_since(
    conn: &libsql::Connection,
    db_path: &str,
    mark: &mut Option<Mark>,
) -> Result<Option<HashSet<String>>, BarcaError> {
    let now = read_mark(conn, db_path).await?;
    let touched = match *mark {
        Some(old) if old == now => Some(HashSet::new()),
        Some(old) if old.max_id <= now.max_id => {
            let (added, bases) = bases_between(conn, old.max_id, now.max_id).await?;
            (old.rows + added == now.rows).then_some(bases)
        }
        _ => None,
    };
    *mark = Some(now);
    Ok(touched)
}

/// The rows of `materializations` now: its highest row id and how many there are. Call it
/// first in a transaction, so the transaction reads what it counted.
///
/// Counting reads every row (20-35 ms for 470k), and one `GET /state` reads three indexes
/// that each need the count: a count is reused when the database files did not change while it
/// was taken nor since ([`FileStamp`]), and the highest id agrees.
async fn read_mark(conn: &libsql::Connection, db_path: &str) -> Result<Mark, BarcaError> {
    let failure = |e| BarcaError::Db(format!("failed to count materializations: {e}"));
    let file = std::fs::canonicalize(db_path).ok();
    let stamp = || file.as_deref().map(FileStamp::of);
    let before = stamp();
    // The transaction's first read: its view of the database is the files' state from here.
    let mut rows = conn
        .query("SELECT COALESCE(max(id), 0) FROM materializations", ())
        .await
        .map_err(failure)?;
    let row = rows.next().await.map_err(failure)?;
    let max_id = row.and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0);
    drop(rows);
    let unchanged = before.is_some() && stamp() == before;
    if unchanged
        && let Some(file) = &file
        && let Some((at, mark)) = COUNTED.lock().unwrap().get(file)
        && Some(at) == before.as_ref()
        && mark.max_id == max_id
    {
        return Ok(*mark);
    }
    let mut rows = conn
        .query("SELECT count(*) FROM materializations", ())
        .await
        .map_err(failure)?;
    let row = rows.next().await.map_err(failure)?;
    let mark = Mark {
        max_id,
        rows: row.and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0),
    };
    if unchanged
        && stamp() == before
        && let (Some(file), Some(at)) = (file, before)
    {
        let mut counted = COUNTED.lock().unwrap();
        make_room(&mut counted, &file);
        counted.insert(file, (at, mark));
    }
    Ok(mark)
}

/// How many database files a process keeps an index of. A server reads one (two with a view
/// and its file); a server that reads copies of its database names a new file with each copy,
/// and one whose indexes were all kept would grow without end.
const KEPT_FILES: usize = 4;

/// Make room for `key` in a map of per-file indexes: empty it when it holds [`KEPT_FILES`]
/// others.
pub(crate) fn make_room<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, V>, key: &K) {
    if !map.contains_key(key) && map.len() >= KEPT_FILES {
        map.clear();
    }
}

/// The last [`Mark`] counted in each database file, and the files' state it was counted in.
static COUNTED: std::sync::LazyLock<
    std::sync::Mutex<HashMap<std::path::PathBuf, (FileStamp, Mark)>>,
> = std::sync::LazyLock::new(Default::default);

/// A database file and its write-ahead log: identity, size, and modification and change
/// times, and the log's header. A commit appends to the log, which grows it, or (after a
/// checkpoint) writes it again from the start under a new salt in its header; times alone can
/// be too coarse to tell two commits apart.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    files: [Option<FileState>; 2],
    log_header: Option<[u8; 32]>,
    /// The first copy of the log index's header in `-shm`: its frame count moves on every
    /// commit, also one that rewrites the log in place after a checkpoint.
    index_header: Option<[u8; 48]>,
}

/// The first `N` bytes of the file at `path`, read through its held descriptor.
fn header<const N: usize>(path: &std::path::Path) -> Option<[u8; N]> {
    use std::os::unix::fs::FileExt;
    let file = super::held_file(path).ok()?;
    let mut header = [0u8; N];
    file.read_exact_at(&mut header, 0).ok().map(|()| header)
}

/// One file's identity, size, and modification and change times (seconds, nanoseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileState {
    dev: u64,
    ino: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    fn of(file: &std::path::Path) -> Self {
        use std::os::unix::fs::MetadataExt;
        let state = |path: &std::path::Path| {
            std::fs::metadata(path).ok().map(|m| FileState {
                dev: m.dev(),
                ino: m.ino(),
                len: m.len(),
                modified: (m.mtime(), m.mtime_nsec()),
                changed: (m.ctime(), m.ctime_nsec()),
            })
        };
        let mut wal = file.as_os_str().to_owned();
        wal.push("-wal");
        let wal = std::path::PathBuf::from(wal);
        // Through held descriptors: closing one of a file drops this process's SQLite locks on
        // it ([`held_file`]), which let another process's writer reset the log index under
        // this one's connections.
        let log_header = header::<32>(&wal);
        // An empty log holds nothing, the same as none: a reader recreates one where the last
        // connection to close removed it (as `barca serve`'s read cache counts it).
        let log = state(&wal).filter(|s| s.len > 0);
        let mut shm = file.as_os_str().to_owned();
        shm.push("-shm");
        let index_header = header::<48>(std::path::Path::new(&shm));
        Self {
            files: [state(file), log],
            log_header: log.and(log_header),
            index_header: log.and(index_header),
        }
    }
}

/// How many rows have an id in `(after, upto]`, and the base ids they belong to.
async fn bases_between(
    conn: &libsql::Connection,
    after: i64,
    upto: i64,
) -> Result<(i64, HashSet<String>), BarcaError> {
    let failure = |e| BarcaError::Db(format!("failed to read new rows: {e}"));
    let mut rows = conn
        .query(
            "SELECT node_id FROM materializations WHERE id > ?1 AND id <= ?2",
            [after, upto],
        )
        .await
        .map_err(failure)?;
    let (mut n, mut bases) = (0, HashSet::new());
    while let Some(row) = rows.next().await.map_err(failure)? {
        n += 1;
        let node = row.get::<String>(0).unwrap_or_default();
        bases.insert(base_of(&node).to_string());
    }
    Ok((n, bases))
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
    h.latest_id = latest;
    h.bytes_id = bytes;
    let rows = reported_rows(conn, h.latest_id.iter().chain(&h.bytes_id).copied()).await?;
    h.report(&rows);
    Ok(h)
}

/// One row as [`node_histories`] reports it: the columns of its [`MaterializationRecord`], and
/// of its [`BytesChange`] when it is a node's last change.
struct ReportedRow {
    record: MaterializationRecord,
    run_hash: String,
}

impl NodeHistory {
    /// `latest` and `last_bytes_change` as the rows `latest_id` and `bytes_id` read now.
    fn report(&mut self, rows: &HashMap<i64, ReportedRow>) {
        self.latest = self
            .latest_id
            .and_then(|id| rows.get(&id))
            .map(|r| r.record.clone());
        self.last_bytes_change = self
            .bytes_id
            .and_then(|id| rows.get(&id))
            .map(|r| BytesChange {
                node_id: r.record.node_id.clone(),
                run_hash: r.run_hash.clone(),
                content_hash: r.record.content_hash.clone().unwrap_or_default(),
                created_at: r.record.created_at.clone(),
            });
    }
}

/// The rows `ids` as [`node_histories`] reports them, by id; one query per thousand ids.
async fn reported_rows(
    conn: &libsql::Connection,
    ids: impl IntoIterator<Item = i64>,
) -> Result<HashMap<i64, ReportedRow>, BarcaError> {
    let failure = |e| BarcaError::Db(format!("failed to query materializations: {e}"));
    let skip_reason = if schema::has_column(conn, "materializations", "skip_reason").await? {
        "m.skip_reason"
    } else {
        "NULL"
    };
    let ids: Vec<i64> = ids.into_iter().collect();
    let mut out = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(1000) {
        let list = serde_json::to_string(chunk).unwrap_or_else(|_| "[]".to_string());
        let mut rows = conn
            .query(
                &format!(
                    "SELECT m.id, m.node_id, m.run_hash, m.artifact_path, m.artifact_format, \
                 m.artifact_size_bytes, m.elapsed_seconds, m.status, m.error_message, \
                 m.created_at, m.output_hash, m.rows, {skip_reason} \
                 FROM json_each(?1) j JOIN materializations m ON m.id = j.value"
                ),
                [list],
            )
            .await
            .map_err(failure)?;
        while let Some(row) = rows.next().await.map_err(failure)? {
            let opt = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
            out.insert(
                row.get::<i64>(0).unwrap_or_default(),
                ReportedRow {
                    run_hash: row.get::<String>(2).unwrap_or_default(),
                    record: MaterializationRecord {
                        content_hash: opt(10),
                        node_id: row.get::<String>(1).unwrap_or_default(),
                        run_hash: opt(2),
                        artifact_path: opt(3),
                        artifact_format: opt(4),
                        artifact_size_bytes: row.get::<i64>(5).ok(),
                        elapsed_seconds: row.get::<f64>(6).ok(),
                        status: opt(7).unwrap_or_else(|| "success".to_string()),
                        error_message: opt(8),
                        skip_reason: opt(12),
                        created_at: row.get::<String>(9).unwrap_or_default(),
                        rows: row.get::<i64>(11).ok(),
                    },
                },
            );
        }
    }
    Ok(out)
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

    /// A small deterministic generator, so a failing seed can be replayed.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// A count is reused while the database files stay as they were, and taken again after any
    /// commit, also a delete that leaves the highest id and (after a checkpoint) the log's size
    /// as they were.
    #[tokio::test]
    async fn a_count_is_reused_only_while_the_database_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        let file = std::fs::canonicalize(&path).unwrap();
        // Open throughout, as a server's are: the last connection to close folds the log in.
        let held = libsql::Builder::new_local(&path).build().await.unwrap();
        let held = held.connect().unwrap();
        held.query("SELECT 1 FROM materializations", ())
            .await
            .unwrap();
        let sql = |q: &'static str| {
            let path = path.clone();
            async move {
                let _guard = super::db_guard().await;
                let (_db, conn) = super::open_conn(&path).await.unwrap();
                conn.execute_batch(q).await.unwrap();
            }
        };
        let mark = || async {
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            conn.execute("BEGIN", ()).await.unwrap();
            let mark = read_mark(&conn, &path).await.unwrap();
            conn.execute("COMMIT", ()).await.unwrap();
            mark
        };
        let insert = "INSERT INTO materializations (node_id, run_hash, status, created_at) \
                      VALUES ('p.py:a', 'rh', 'success', 't');";
        for _ in 0..4 {
            sql(insert).await;
        }
        assert_eq!(mark().await, Mark { max_id: 4, rows: 4 });
        // Proof that an unchanged database is answered from the kept count, not counted.
        let pretend = |rows| {
            let mut counted = COUNTED.lock().unwrap();
            let kept = counted.get_mut(&file).expect("the count was kept");
            kept.1.rows = rows;
        };
        pretend(99);
        assert_eq!(mark().await.rows, 99);

        sql("DELETE FROM materializations WHERE id = 2;").await;
        assert_eq!(mark().await, Mark { max_id: 4, rows: 3 });

        sql("PRAGMA wal_checkpoint(RESTART);").await;
        assert_eq!(mark().await, Mark { max_id: 4, rows: 3 });
        pretend(99);
        sql("DELETE FROM materializations WHERE id = 3;").await;
        assert_eq!(mark().await, Mark { max_id: 4, rows: 2 });
    }

    /// Kept histories, ages and durations answer what reading every row answers, after each of
    /// many random changes: rows added (plain and keyed nodes, several runs, rows without a run,
    /// failures after successes in the same run), rows deleted, and digests recorded on old rows.
    #[tokio::test]
    async fn kept_reads_match_reading_every_row_after_random_changes() {
        let bases = ["p.py:a", "p.py:b", "p.py:k"];
        let ids: Vec<String> = bases.iter().map(|b| b.to_string()).collect();
        for seed in 1..=12u64 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("metadata.db").display().to_string();
            super::init_db(&path).await.unwrap();
            let mut rng = Lcg(seed);
            let mut ages = NodeAgesIndex::default();
            for step in 0..60 {
                {
                    let _guard = super::db_guard().await;
                    let (_db, conn) = super::open_conn(&path).await.unwrap();
                    let roll = rng.next(10);
                    if roll == 0 {
                        conn.execute(
                            "DELETE FROM materializations WHERE id = (SELECT id FROM materializations \
                             ORDER BY id LIMIT 1 OFFSET ?1)",
                            [rng.next(8) as i64],
                        )
                        .await
                        .unwrap();
                    } else if roll == 1 {
                        // A digest recorded later on the row a history reports (the newest of a
                        // node, or its newest whose bytes changed), and a column another writer
                        // changed on it.
                        let changed = if rng.next(2) == 0 {
                            ""
                        } else {
                            "AND bytes_changed = 1"
                        };
                        let base = bases[rng.next(3) as usize];
                        conn.execute(
                            &format!(
                                "UPDATE materializations SET output_hash = ?1, \
                                 artifact_size_bytes = length(?1) WHERE id = (SELECT \
                                 max(id) FROM materializations WHERE node_id >= ?2 AND node_id < ?3 \
                                 {changed})"
                            ),
                            libsql::params![
                                format!("d{step}"),
                                base,
                                format!("{base}\\")
                            ],
                        )
                        .await
                        .unwrap();
                    } else {
                        for _ in 0..=rng.next(5) {
                            let base = bases[rng.next(3) as usize];
                            let node = if base == "p.py:k" {
                                format!("{base}[r={}]", rng.next(4))
                            } else {
                                base.to_string()
                            };
                            let run = match rng.next(6) {
                                0 => libsql::Value::Null,
                                n => {
                                    libsql::Value::Text(format!("run{}", step / 4 + n as usize % 2))
                                }
                            };
                            let status = if rng.next(5) == 0 {
                                "failed"
                            } else {
                                "success"
                            };
                            let elapsed = match rng.next(4) {
                                0 => libsql::Value::Null,
                                n => libsql::Value::Real(n as f64 + step as f64 / 10.0),
                            };
                            let hash = match rng.next(3) {
                                0 => libsql::Value::Null,
                                n => libsql::Value::Text(format!("h{n}")),
                            };
                            conn.execute(
                                "INSERT INTO materializations (node_id, run_id, run_hash, status, \
                                 elapsed_seconds, output_hash, bytes_changed, batch_id, artifact_path, \
                                 created_at) VALUES (?1, ?2, 'rh', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                                vec![
                                    libsql::Value::Text(node.clone()),
                                    run,
                                    libsql::Value::Text(status.into()),
                                    elapsed,
                                    hash,
                                    libsql::Value::Integer(rng.next(3) as i64 / 2),
                                    libsql::Value::Text(format!("b{}", rng.next(2))),
                                    libsql::Value::Text(format!("art/{node}/{step}")),
                                    libsql::Value::Text(format!("2026-11-13 10:{:02}:00", step)),
                                ],
                            )
                            .await
                            .unwrap();
                        }
                    }
                }
                let context = format!("seed {seed}, step {step}");
                assert_eq!(
                    ages.read(&path).await.unwrap(),
                    NodeAgesIndex::default().read(&path).await.unwrap(),
                    "ages, {context}"
                );
                let kept = super::node_histories(&path, &ids).await.unwrap();
                let durations = super::recent_durations(&path, &ids, 3).await.unwrap();
                let (_db, conn) = super::open_conn(&path).await.unwrap();
                for base in &ids {
                    let fresh = node_history(&conn, base).await.unwrap();
                    assert_eq!(
                        format!("{:?}", kept.get(base)),
                        format!("{:?}", Some(&fresh)),
                        "history of {base}, {context}"
                    );
                }
                for base in &ids {
                    let fresh = super::super::stats::durations_of(&conn, base, 3)
                        .await
                        .unwrap();
                    assert_eq!(
                        durations.get(base).cloned().unwrap_or_default(),
                        fresh,
                        "durations of {base}, {context}"
                    );
                }
            }
        }
    }

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

    async fn insert_rows(path: &str, rows: &[(&str, &str, &str, Option<i64>)]) {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(path).await.unwrap();
        for (run, node, status, count) in rows {
            conn.execute(
                "INSERT INTO materializations (run_id, node_id, status, artifact_path, rows) \
                 VALUES (?1, ?2, ?3, 'a.parquet', ?4)",
                libsql::params![*run, *node, *status, *count],
            )
            .await
            .unwrap();
        }
    }

    /// Row counts (#925): each key's latest success against the one before it, read in full
    /// and again after a run recorded more rows; unknown counts are left out; the newest tick
    /// of each schedule is read from `runs`.
    #[tokio::test]
    async fn row_facts_compare_each_keys_latest_success_with_the_one_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        insert_rows(
            &path,
            &[
                ("r1", "p.py:a", "success", Some(100)),
                ("r1", "p.py:k[d=1]", "success", Some(10)),
                ("r1", "p.py:k[d=2]", "success", Some(0)),
                ("r1", "p.py:k[d=3]", "success", Some(10)),
                ("r1", "p.py:unknown", "success", None),
                ("r2", "p.py:a", "success", Some(140)),
                ("r2", "p.py:k[d=1]", "success", Some(0)),
                ("r2", "p.py:k[d=2]", "success", Some(4)),
                ("r2", "p.py:k[d=4]", "success", Some(0)),
                ("r2", "p.py:k[d=3]", "failed", None),
            ],
        )
        .await;
        let mut index = NodeAgesIndex::default();
        let ages = index.read(&path).await.unwrap();
        let a = ages["p.py:a"].rows.clone().unwrap();
        assert_eq!((a.partitioned, a.counted, a.empty), (false, 1, 0));
        assert_eq!(a.previous_empty, Some(0));
        assert_eq!(
            (a.changed, a.largest_change),
            (0, None),
            "+40% is not a change"
        );
        let k = ages["p.py:k"].rows.clone().unwrap();
        // d=1 and d=4 empty now; before r2 only d=2 was (d=4 is new, d=3 kept r1's 10).
        assert_eq!((k.partitioned, k.counted, k.empty), (true, 4, 2));
        assert_eq!(k.previous_empty, Some(1));
        // d=1 10 -> 0 and d=2 0 -> 4 both moved; 0 -> 4 is the larger relative change.
        assert_eq!(k.changed, 2);
        assert_eq!(
            k.largest_change,
            Some(RowChange {
                key: Some("d=2".into()),
                rows: 4,
                previous_rows: 0
            })
        );
        let unknown = ages["p.py:unknown"].rows.clone().unwrap();
        assert_eq!(
            (unknown.counted, unknown.empty, unknown.previous_empty),
            (0, 0, None)
        );

        // A third run halves `a` and fills d=1: only the touched node is folded again.
        insert_rows(
            &path,
            &[
                ("r3", "p.py:a", "success", Some(60)),
                ("r3", "p.py:k[d=1]", "success", Some(12)),
            ],
        )
        .await;
        let ages = index.read(&path).await.unwrap();
        let a = ages["p.py:a"].rows.clone().unwrap();
        assert_eq!(
            (a.changed, a.largest_change),
            (
                1,
                Some(RowChange {
                    key: None,
                    rows: 60,
                    previous_rows: 140
                })
            )
        );
        assert_eq!(a.newest.1.as_deref(), Some("r3"));
        let k = ages["p.py:k"].rows.clone().unwrap();
        // Before r3: d=1 empty (r2) and d=4 empty; now only d=4.
        assert_eq!((k.empty, k.previous_empty), (1, Some(2)));
        // Read in full, the same facts.
        let full = node_ages(&path).await.unwrap();
        assert_eq!(full["p.py:k"].rows, ages["p.py:k"].rows);
    }

    #[test]
    fn a_change_is_more_than_half_of_the_count_before() {
        assert!(!rows_changed(150, 100));
        assert!(rows_changed(151, 100));
        assert!(rows_changed(49, 100));
        assert!(!rows_changed(50, 100));
        assert!(rows_changed(1, 0), "rows after an empty result");
        assert!(!rows_changed(0, 0));
    }

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
                (
                    ages[id].last_success_at.as_deref(),
                    ages[id].failing_since.as_deref(),
                    ages[id].consecutive_failures
                ),
                (ok, since, n),
                "{id}"
            );
        };
        assert_eq!(
            ages["p.py:recovered"].recovered_at.as_deref(),
            Some("2026-01-02 00:00:00")
        );
        assert_eq!(ages["p.py:keys"].recovered_at, None);
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

    /// The plan of `q`'s details, as `EXPLAIN QUERY PLAN` lists them.
    async fn plan(conn: &libsql::Connection, q: &str) -> Vec<String> {
        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {q}"),
                libsql::params!["r", "n", "n[", "n\\"],
            )
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        details
    }

    /// A node's newest run is totalled from `idx_mat_run_node`, one seek for the node and one
    /// for its keys: with `idx_mat_run` alone it read every row of the run, once per node (1.3 s
    /// per `GET /state` for a run of 95k rows across 295 nodes).
    #[tokio::test]
    async fn a_nodes_last_run_is_read_by_seeks_of_the_run_and_node_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        let (_db, conn) = open_conn(&path).await.unwrap();
        let details = plan(&conn, LAST_RUN_TOTALS).await;
        let seeks = details
            .iter()
            .filter(|d| d.contains("idx_mat_run_node (run_id=? AND node_id"))
            .count();
        assert_eq!(seeks, 2, "{details:?}");
        assert!(
            !details
                .iter()
                .any(|d| d.starts_with("SCAN materializations")),
            "{details:?}"
        );
    }

    /// Failed keys are found through `idx_mat_failed`, the failed rows only, not a walk of a
    /// `node_id` index over every row (160 ms for 470k rows on each status read).
    #[tokio::test]
    async fn failed_keys_are_read_from_the_failure_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        let (_db, conn) = open_conn(&path).await.unwrap();
        let sql = super::super::partition_keys::FAILED_KEYS;
        let mut rows = conn
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        assert!(
            details
                .iter()
                .any(|d| d.contains("SCAN f USING INDEX idx_mat_failed")),
            "{details:?}"
        );
    }
}
