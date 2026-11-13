//! [`CacheReader`], the short-lived handle of one phase's cache lookups, and [`KeyRows`], a
//! partitioned node's successful rows read in one pass.

use super::*;
use crate::step_result::OutputRef;

/// A short-lived handle for one phase's cache lookups. It holds the in-process guard
/// *and* the cross-process lock for as long as it lives, so open it for the lookups
/// and drop it before any step runs: another barca process can then use the DB while
/// this run executes Python. (Fields drop in order: connection, database + file lock,
/// then the in-process guard.)
pub(crate) struct CacheReader {
    /// The statement of a one-key cache lookup, compiled on first use and kept for the
    /// reader's life ([`crate::cache::lookup_cached`]): compiling it cost more than the lookup.
    lookup: tokio::sync::Mutex<Option<libsql::Statement>>,
    /// The statement of [`CacheReader::lookup_many`], kept the same way.
    lookup_many: tokio::sync::Mutex<Option<libsql::Statement>>,
    local_artifact_dir: Option<String>,
    /// A server's ledger, which answers cache lookups in place of this database.
    ledger: Option<crate::ledger::Ledger>,
    /// The successful rows of a partitioned node's keys, read in one pass the first time a
    /// node with many keys is looked up ([`CacheReader::key_rows`]); `None` for a node whose
    /// keys are looked up one by one.
    key_rows: std::sync::Mutex<HashMap<String, Option<std::sync::Arc<KeyRows>>>>,
    conn: libsql::Connection,
    _handle: DbHandle,
    /// None for a [`ReadView`] ([`db_guard_for`]).
    _guard: Option<MutexGuard<'static, ()>>,
}

impl CacheReader {
    pub async fn open(db_path: &str) -> Result<Self, BarcaError> {
        // Same order as every other helper: in-process guard first, then the file lock.
        let guard = db_guard_for(db_path).await;
        let (handle, conn) = open_conn(db_path).await?;
        Ok(Self {
            lookup: Default::default(),
            lookup_many: Default::default(),
            local_artifact_dir: None,
            ledger: None,
            key_rows: Default::default(),
            conn,
            _handle: handle,
            _guard: guard,
        })
    }

    pub(crate) async fn for_config(
        cfg: &crate::config::ResolvedConfig,
    ) -> Result<Self, BarcaError> {
        let mut cache = Self::open(&cfg.db_path).await?;
        cache.local_artifact_dir = cfg.remote_off.then(|| cfg.local_artifact_dir.clone());
        cache.ledger = cfg.ledger.clone();
        Ok(cache)
    }

    /// The server's ledger, when this command runs against one.
    pub(crate) fn ledger(&self) -> Option<&crate::ledger::Ledger> {
        self.ledger.as_ref()
    }

    pub(crate) fn allows_artifact(&self, path: &str) -> bool {
        self.local_artifact_dir
            .as_deref()
            .is_none_or(|root| crate::config::local_artifact(path, root))
    }

    pub(crate) fn conn(&self) -> &libsql::Connection {
        &self.conn
    }

    /// The one-key lookup statement, compiled from the first of `sql` this database accepts
    /// on first use and reset for the next query.
    pub(crate) async fn lookup_statement(
        &self,
        sql: &[&str],
    ) -> tokio::sync::MappedMutexGuard<'_, libsql::Statement> {
        Self::kept_statement(&self.conn, &self.lookup, sql)
            .await
            .expect("the materializations table accepts a lookup")
    }

    /// The statement kept in `slot`, compiled from the first of `sql` that `conn` accepts on
    /// first use, and reset for the next query; `None` when it accepts none.
    async fn kept_statement<'a>(
        conn: &libsql::Connection,
        slot: &'a tokio::sync::Mutex<Option<libsql::Statement>>,
        sql: &[&str],
    ) -> Option<tokio::sync::MappedMutexGuard<'a, libsql::Statement>> {
        let mut slot = slot.lock().await;
        if slot.is_none() {
            for candidate in sql {
                #[cfg(test)]
                PREPARES.with(|n| n.set(n.get() + 1));
                if let Ok(statement) = conn.prepare(candidate).await {
                    *slot = Some(statement);
                    break;
                }
            }
        }
        let statement = tokio::sync::MutexGuard::try_map(slot, Option::as_mut).ok()?;
        statement.reset();
        Some(statement)
    }

    /// [`crate::cache::lookup_cached`] of each `(node id, run hash)`, in order, a few thousand
    /// at a time: one query (compiled once, the keys passed as one JSON array) instead of one
    /// query and one transaction per key. `None` when this database cannot answer them so (a
    /// server's ledger answers lookups, or the table predates output hashes); the caller then
    /// looks them up one by one.
    pub(crate) async fn lookup_many(
        &self,
        keys: &[(&str, &str)],
    ) -> Option<Vec<Option<OutputRef>>> {
        if self.ledger.is_some() {
            return None;
        }
        const CHUNK: usize = 4096;
        const SQL: &str = concat!(
            "SELECT k.key, m.artifact_path, m.artifact_format, \
             m.artifact_size_bytes, m.output_hash FROM json_each(?1) AS k \
             JOIN materializations m ",
            by_node_run!(),
            " ON m.node_id = json_extract(k.value, '$[0]') \
             AND m.run_hash = json_extract(k.value, '$[1]') \
             WHERE m.status = 'success' ORDER BY k.key, m.id DESC"
        );
        let mut out: Vec<Option<OutputRef>> = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(CHUNK) {
            let pairs = serde_json::to_string(chunk).ok()?;
            let statement = Self::kept_statement(&self.conn, &self.lookup_many, &[SQL]).await?;
            let mut rows = statement.query([pairs]).await.ok()?;
            // Each key's rows newest first: its first row with an artifact this run may read
            // decides, as in a lookup.
            let mut found: Vec<Option<Option<OutputRef>>> = vec![None; chunk.len()];
            while let Some(row) = rows.next().await.ok()? {
                let Ok(i) = row.get::<i64>(0) else { continue };
                let Some(slot) = found.get_mut(i as usize) else {
                    continue;
                };
                if slot.is_some() {
                    continue;
                }
                let Ok(path) = row.get::<String>(1) else {
                    continue;
                };
                if !self.allows_artifact(&path) {
                    continue;
                }
                *slot = Some((|| {
                    Some(OutputRef {
                        path,
                        format: row.get::<String>(2).ok()?,
                        size_bytes: row.get::<i64>(3).ok()? as u64,
                        elapsed_seconds: None,
                        content_hash: row.get::<String>(4).ok().filter(|h| !h.is_empty()),
                    })
                })());
            }
            drop(rows);
            drop(statement);
            out.extend(found.into_iter().map(Option::flatten));
            tokio::task::yield_now().await;
        }
        Some(out)
    }

    /// Every successful row of `base`'s partition keys, by key id, when reading them in one
    /// pass is cheaper than looking each key up: a dry run of a 200k-key node asked 200k
    /// times, at about 5µs a lookup. The pass reads the whole table (an index-ordered read of
    /// the keys' rows costs a random page read per row), so it is taken only when the table has
    /// at most [`ROWS_PER_KEY`] rows for each of the node's `keys`, the keys the plan has;
    /// otherwise `None`, and the keys are looked up one by one. Asked once per node; the reader
    /// is short-lived, so the rows are as current as one lookup's.
    pub(crate) async fn key_rows(
        &self,
        base: &str,
        keys: usize,
    ) -> Option<std::sync::Arc<KeyRows>> {
        if self.ledger.is_some() {
            return None;
        }
        if let Some(known) = self.key_rows.lock().unwrap().get(base) {
            return known.clone();
        }
        let rows = self
            .read_key_rows(base, keys)
            .await
            .ok()
            .flatten()
            .map(std::sync::Arc::new);
        self.key_rows
            .lock()
            .unwrap()
            .insert(base.to_string(), rows.clone());
        rows
    }

    async fn read_key_rows(&self, base: &str, keys: usize) -> Result<Option<KeyRows>, BarcaError> {
        let (lo, hi) = key_range(base);
        let table = query_i64(
            &self.conn,
            "SELECT max(id) FROM materializations",
            Vec::new(),
        )
        .await?
        .unwrap_or(0);
        if table > (keys as i64).saturating_mul(ROWS_PER_KEY) {
            return Ok(None);
        }
        let mut rows = self
            .conn
            .query(
                &format!(
                    "SELECT id, node_id, run_hash, artifact_path, artifact_format, \
                     artifact_size_bytes, output_hash, evicted_at IS NULL AND \
                     last_read_at >= datetime('now', '{}') FROM materializations NOT INDEXED \
                     WHERE node_id >= ?1 AND node_id < ?2 AND status = 'success' \
                     AND artifact_path IS NOT NULL",
                    crate::cache::READ_STAMP_REFRESH
                ),
                [lo, hi],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to query materializations: {e}")))?;
        let mut out = KeyRows::default();
        let mut read = 0usize;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        {
            // A long read that never waits: let the thread's other tasks run now and then.
            read += 1;
            if read.is_multiple_of(4096) {
                tokio::task::yield_now().await;
            }
            let (Ok(id), Ok(node), Ok(run_hash), Ok(path)) = (
                row.get::<i64>(0),
                row.get::<String>(1),
                row.get::<String>(2),
                row.get::<String>(3),
            ) else {
                continue;
            };
            if !self.allows_artifact(&path) {
                continue;
            }
            let format = row.get::<String>(4).ok();
            let size = row.get::<i64>(5).ok();
            let content = row.get::<String>(6).ok().filter(|h| !h.is_empty());
            // Read within the hour (`cache::READ_STAMP_REFRESH`): a read now need not be stamped.
            let recent = row.get::<i64>(7).is_ok_and(|r| r == 1);
            out.add(
                id,
                &node,
                &run_hash,
                &path,
                format.as_deref(),
                size,
                content.as_deref(),
                recent,
            );
        }
        Ok(Some(out))
    }
}

/// How many rows of the table per key [`CacheReader::key_rows`] reads at most in its pass: a
/// lookup costs about as much as reading this many rows in order.
const ROWS_PER_KEY: i64 = 8;

/// A partitioned node's successful rows: for each key id and run hash, the latest row's id and
/// output. Every string is kept in one buffer, so that building and dropping the rows of 400k
/// keys costs a few allocations, not millions.
#[derive(Default)]
pub(crate) struct KeyRows {
    text: String,
    rows: Vec<KeyRow>,
    /// The first row of each (key id, run hash) digest; further rows chain through `next`.
    first: HashMap<u64, u32>,
}

/// A span of [`KeyRows::text`].
#[derive(Clone, Copy)]
struct Span(u32, u32);

struct KeyRow {
    id: i64,
    node: Span,
    run_hash: Span,
    path: Span,
    /// `None` when the latest row has no format or size: as for a lookup, no hit.
    format_size: Option<(Span, u64)>,
    content: Option<Span>,
    /// Its `last_read_at` is less than an hour old.
    recently_read: bool,
    next: Option<u32>,
}

impl KeyRows {
    fn key(node: &str, run_hash: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (node, run_hash).hash(&mut h);
        h.finish()
    }

    fn span(&mut self, s: &str) -> Span {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        Span(start, self.text.len() as u32)
    }

    fn text(&self, span: Span) -> &str {
        &self.text[span.0 as usize..span.1 as usize]
    }

    fn find(&self, node: &str, run_hash: &str) -> Option<usize> {
        let mut at = self.first.get(&Self::key(node, run_hash)).copied();
        while let Some(i) = at {
            let row = &self.rows[i as usize];
            if self.text(row.node) == node && self.text(row.run_hash) == run_hash {
                return Some(i as usize);
            }
            at = row.next;
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        id: i64,
        node: &str,
        run_hash: &str,
        path: &str,
        format: Option<&str>,
        size: Option<i64>,
        content: Option<&str>,
        recently_read: bool,
    ) {
        if let Some(i) = self.find(node, run_hash)
            && self.rows[i].id > id
        {
            return;
        }
        let path_span = self.span(path);
        let format_size = match (format, size) {
            (Some(format), Some(size)) => Some((self.span(format), size as u64)),
            _ => None,
        };
        let content = content.map(|c| self.span(c));
        if let Some(i) = self.find(node, run_hash) {
            let row = &mut self.rows[i];
            row.id = id;
            row.path = path_span;
            row.format_size = format_size;
            row.content = content;
            row.recently_read = recently_read;
            return;
        }
        let node_span = self.span(node);
        let hash_span = self.span(run_hash);
        let key = Self::key(node, run_hash);
        let next = self.first.get(&key).copied();
        self.first.insert(key, self.rows.len() as u32);
        self.rows.push(KeyRow {
            id,
            node: node_span,
            run_hash: hash_span,
            path: path_span,
            format_size,
            content,
            recently_read,
            next,
        });
    }

    /// Whether the row a lookup of `node_id` at `run_hash` finds was read within the hour.
    pub(crate) fn recently_read(&self, node_id: &str, run_hash: &str) -> bool {
        self.find(node_id, run_hash)
            .is_some_and(|i| self.rows[i].recently_read)
    }

    /// What a lookup of `node_id` at `run_hash` finds.
    pub(crate) fn get(&self, node_id: &str, run_hash: &str) -> Option<OutputRef> {
        let row = &self.rows[self.find(node_id, run_hash)?];
        let (format, size_bytes) = row.format_size?;
        Some(OutputRef {
            path: self.text(row.path).to_string(),
            format: self.text(format).to_string(),
            size_bytes,
            elapsed_seconds: None,
            content_hash: row.content.map(|c| self.text(c).to_string()),
        })
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    /// A reader compiles its lookup statements once, however many keys it looks up: 0.34.0
    /// compiled the one-key lookup again for every key, a third of `barca status` over a
    /// million keys.
    #[tokio::test(flavor = "current_thread")]
    async fn a_reader_compiles_each_lookup_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        let cache = CacheReader::open(&path).await.unwrap();
        let before = prepares();
        for k in 0..100 {
            crate::cache::lookup_cached(&cache, &format!("p.py:a[k={k}]"), "h").await;
        }
        assert_eq!(prepares() - before, 1, "one-key lookups");
        let keys: Vec<(String, String)> = (0..5000)
            .map(|k| (format!("p.py:a[k={k}]"), "h".to_string()))
            .collect();
        let pairs: Vec<(&str, &str)> = keys.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
        let found = cache.lookup_many(&pairs).await.unwrap();
        assert_eq!(found.len(), 5000);
        assert_eq!(
            prepares() - before,
            2,
            "and the lookup of many keys, in two chunks"
        );
    }

    fn db_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("metadata.db").display().to_string()
    }

    #[tokio::test]
    async fn lookups_together_answer_what_lookups_one_by_one_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_in(&dir);
        init_db(&path).await.unwrap();
        for (node, hash, path_, format, status) in [
            // The newest successful row decides, even without a format: no hit.
            (
                "p.py:a[k=1]",
                "h1",
                Some("/x/a1.json"),
                Some("json"),
                "success",
            ),
            ("p.py:a[k=1]", "h1", Some("/x/a1b.json"), None, "success"),
            // An older row with an artifact under a newer one without.
            (
                "p.py:a[k=2]",
                "h2",
                Some("/x/a2.json"),
                Some("json"),
                "success",
            ),
            ("p.py:a[k=2]", "h2", None, None, "success"),
            // Failed only, and another run hash.
            (
                "p.py:a[k=3]",
                "h3",
                Some("/x/a3.json"),
                Some("json"),
                "failed",
            ),
            (
                "p.py:a[k=4]",
                "other",
                Some("/x/a4.json"),
                Some("json"),
                "success",
            ),
            (
                "p.py:a[k=5]",
                "h5",
                Some("/x/a5.pkl"),
                Some("pickle"),
                "success",
            ),
        ] {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, \
                 artifact_format, artifact_size_bytes, status, output_hash) \
                 VALUES (?1, ?2, ?3, ?4, 7, ?5, 'c')",
                libsql::params![node, hash, path_, format, status],
            )
            .await
            .unwrap();
        }
        let keys: Vec<(String, String)> = (1..=6)
            .map(|k| (format!("p.py:a[k={k}]"), format!("h{k}")))
            .collect();
        let pairs: Vec<(&str, &str)> = keys.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
        let cache = CacheReader::open(&path).await.unwrap();
        let together = cache.lookup_many(&pairs).await.expect("answered together");
        let mut one_by_one = Vec::new();
        for (node, hash) in &pairs {
            one_by_one.push(crate::cache::lookup_cached(&cache, node, hash).await);
        }
        let paths = |found: &[Option<OutputRef>]| -> Vec<Option<String>> {
            found
                .iter()
                .map(|o| o.as_ref().map(|o| o.path.clone()))
                .collect()
        };
        assert_eq!(paths(&together), paths(&one_by_one));
        assert_eq!(
            paths(&together),
            [
                None,
                Some("/x/a2.json".into()),
                None,
                None,
                Some("/x/a5.pkl".into()),
                None
            ]
        );
    }
}

const PREVIOUS_OBSERVATION: &str = "SELECT COALESCE(observation_hash, output_hash), artifact_path, artifact_format, artifact_size_bytes, output_hash FROM materializations INDEXED BY idx_mat_node_latest WHERE node_id = ?1 AND status = 'success' AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1";

impl CacheReader {
    /// A sensor's latest observation: its freshness revision and the output it last reported
    /// ([`crate::observation`] decides the next revision from it).
    pub(crate) async fn previous_observation(
        &self,
        node_id: &str,
    ) -> Result<Option<(String, OutputRef)>, BarcaError> {
        let fail = |e| BarcaError::Db(format!("failed to read sensor observation: {e}"));
        let mut rows = self
            .conn()
            .query(PREVIOUS_OBSERVATION, [node_id.to_owned()])
            .await
            .map_err(fail)?;
        let Some(row) = rows.next().await.map_err(fail)? else {
            return Ok(None);
        };
        let Ok(revision) = row.get::<String>(0) else {
            return Ok(None);
        };
        Ok(Some((
            revision,
            OutputRef {
                path: row.get::<String>(1).map_err(fail)?,
                format: row.get::<String>(2).map_err(fail)?,
                size_bytes: row.get::<i64>(3).unwrap_or(0).max(0) as u64,
                elapsed_seconds: None,
                content_hash: row.get::<String>(4).ok(),
            },
        )))
    }
}

#[cfg(test)]
mod observation_tests {
    #[tokio::test]
    async fn the_latest_sensor_observation_does_not_sort_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        crate::db::init_db(&path).await.unwrap();
        let cache = crate::db::CacheReader::open(&path).await.unwrap();
        for (definition, digest) in [("old", "first"), ("new", "second"), ("old", "latest")] {
            cache.conn().execute(
                "INSERT INTO materializations (node_id,run_hash,status,artifact_path,artifact_format,artifact_size_bytes,output_hash) VALUES ('p.py:source',?1,'success',?2,'json',1,?2)",
                (definition, digest),
            ).await.unwrap();
        }
        let (revision, output) = cache
            .previous_observation("p.py:source")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(revision, "latest");
        assert_eq!(output.path, "latest");
        let mut plan = cache
            .conn()
            .query(
                &format!("EXPLAIN QUERY PLAN {}", super::PREVIOUS_OBSERVATION),
                ["p.py:source"],
            )
            .await
            .unwrap();
        while let Some(row) = plan.next().await.unwrap() {
            let detail = row.get::<String>(3).unwrap();
            assert!(
                !detail.contains("TEMP B-TREE"),
                "latest observation must not sort every prior poll: {detail}"
            );
        }
    }
}
