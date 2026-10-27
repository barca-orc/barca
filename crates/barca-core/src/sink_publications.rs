//! Which run hash each `@sink` last published (#662).
//!
//! A sink is written by the step that computes its asset. A step served from cache computes
//! nothing, so a sink would keep whatever the last computed run hash wrote: after code
//! A -> B -> A, the third run is a cache hit on A while the sink still holds B. This table
//! records, per step (a partitioned asset's key is a step of its own) and per declared sink
//! path, the run hash whose output the sink holds. A cache hit whose run hash differs is
//! republished from the cached artifact without running the step
//! (`python -m barca._sinks republish`); one that matches writes nothing.
//!
//! Rows are written with the run's ledger: one per sink of every step that ran and wrote its
//! sinks, every cache hit republished, and every cache hit found to match. A database from
//! before this table has no rows: the latest successful `materializations` row of the step that
//! wrote sinks (`sinks_json`) stands in for them, since the sinks are part of the run hash.
//!
//! A sink path on this machine's disk is only trusted from a row this host wrote (a metadata DB
//! restored on another machine says nothing about that machine's files); a remote sink URI is
//! the same object from every host.
//!
//! It is an additive table of metadata format 1 (`specs/interfaces/metadata-schema.md`):
//! `init_db` creates it, a format-1 database from an earlier release gains it in the additive
//! migration, and a database without it reads as having no rows.

use crate::BarcaError;
use crate::db;
use libsql::{Connection, Value};
use std::collections::{BTreeMap, HashMap};

pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS sink_publications (
    node_id TEXT NOT NULL,
    sink TEXT NOT NULL,
    run_hash TEXT NOT NULL,
    host TEXT NOT NULL DEFAULT '',
    run_id TEXT,
    published_at TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (node_id, sink)
)";

/// A sink of a step that holds the output of `run_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Publication {
    /// The step's display id (`<node>` or `<node>[<key>]`).
    pub(crate) node_id: String,
    /// The sink path as declared in `@sink(...)`.
    pub(crate) sink: String,
    pub(crate) run_hash: String,
}

/// What a cached step's sink needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The sink holds this run hash's output: nothing to write.
    Current,
    /// The sink holds it according to the step's history, which predates this table: nothing
    /// to write, and a row is recorded so the history is not read again.
    Adopt,
    /// The sink holds another run hash's output, or nothing barca knows of: republish it.
    Stale,
}

/// What the sinks of some nodes last published, read once for a set of cached steps.
#[derive(Debug, Default)]
pub(crate) struct Published {
    /// (step, sink) -> (run hash, host that wrote the row).
    rows: HashMap<(String, String), (String, String)>,
    /// Step -> run hash of its latest successful row that wrote sinks.
    history: HashMap<String, String>,
}

/// A sink on this machine's disk (a plain path or `file://`), as opposed to a remote URI.
fn is_local(sink: &str) -> bool {
    sink.starts_with("file://") || !sink.contains("://")
}

impl Published {
    pub(crate) fn verdict(&self, host: &str, node_id: &str, sink: &str, run_hash: &str) -> Verdict {
        if let Some((hash, by)) = self.rows.get(&(node_id.to_string(), sink.to_string())) {
            if is_local(sink) && by != host {
                return Verdict::Stale;
            }
            return if hash == run_hash {
                Verdict::Current
            } else {
                Verdict::Stale
            };
        }
        match self.history.get(node_id) {
            Some(hash) if hash == run_hash => Verdict::Adopt,
            _ => Verdict::Stale,
        }
    }
}

fn failed(what: &'static str) -> impl Fn(libsql::Error) -> BarcaError {
    move |e| BarcaError::Db(format!("sink publications: {what}: {e}"))
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s.clone()),
        _ => None,
    }
}

async fn has_table(conn: &Connection) -> Result<bool, BarcaError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'sink_publications'",
            (),
        )
        .await
        .map_err(failed("reading the schema"))?;
    let found = rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some();
    while rows
        .next()
        .await
        .map_err(failed("reading the schema"))?
        .is_some()
    {}
    Ok(found)
}

/// The rows of `base` and of its partition keys (`<base>[...]`, the range of ids between
/// `<base>[` and `<base>\`).
const OF_NODE: &str = "(node_id = ?1 OR (node_id >= ?1 || '[' AND node_id < ?1 || '\\'))";

/// What the sinks of `wanted` last published: by base node id, the (step, sink) pairs asked
/// about. The history of a node is read only when one of its pairs has no row. Nothing is
/// created: a database without the table reads as having no rows.
pub(crate) async fn published(
    db_path: &str,
    wanted: &BTreeMap<String, Vec<(String, String)>>,
) -> Result<Published, BarcaError> {
    let mut out = Published::default();
    if wanted.is_empty() || !std::path::Path::new(db_path).exists() {
        return Ok(out);
    }
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    let table = has_table(&conn).await?;
    for (base, pairs) in wanted {
        if table {
            let mut rows = conn
                .query(
                    &format!(
                        "SELECT node_id, sink, run_hash, host FROM sink_publications WHERE {OF_NODE}"
                    ),
                    [base.clone()],
                )
                .await
                .map_err(failed("reading"))?;
            while let Some(row) = rows.next().await.map_err(failed("reading"))? {
                let value = |i| text(&row.get_value(i).unwrap_or(Value::Null)).unwrap_or_default();
                out.rows.insert((value(0), value(1)), (value(2), value(3)));
            }
        }
        if pairs.iter().all(|pair| out.rows.contains_key(pair)) {
            continue;
        }
        // Latest last: a later row replaces an earlier one.
        let mut rows = conn
            .query(
                &format!(
                    "SELECT node_id, run_hash FROM materializations WHERE {OF_NODE} \
                     AND status = 'success' AND artifact_path IS NOT NULL \
                     AND sinks_json IS NOT NULL ORDER BY id"
                ),
                [base.clone()],
            )
            .await
            .map_err(failed("reading the history"))?;
        while let Some(row) = rows.next().await.map_err(failed("reading the history"))? {
            let value = |i| text(&row.get_value(i).unwrap_or(Value::Null)).unwrap_or_default();
            out.history.insert(value(0), value(1));
        }
    }
    Ok(out)
}

/// Record `publications`, written by `run_id` on this host, inside the caller's transaction.
pub(crate) async fn write_in(
    conn: &Connection,
    run_id: &str,
    publications: &[Publication],
) -> Result<(), BarcaError> {
    if publications.is_empty() {
        return Ok(());
    }
    conn.execute(CREATE, ())
        .await
        .map_err(failed("creating the table"))?;
    let host = db::local_host();
    let mut upsert = db::prepare(
        conn,
        "INSERT INTO sink_publications (node_id, sink, run_hash, host, run_id) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(node_id, sink) DO UPDATE SET run_hash = ?3, host = ?4, run_id = ?5, \
         published_at = datetime('now')",
    )
    .await
    .map_err(failed("recording"))?;
    for p in publications {
        upsert
            .execute([
                Value::Text(p.node_id.clone()),
                Value::Text(p.sink.clone()),
                Value::Text(p.run_hash.clone()),
                Value::Text(host.clone()),
                Value::Text(run_id.to_string()),
            ])
            .await
            .map_err(failed("recording"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(node: &str, sink: &str, hash: &str) -> Publication {
        Publication {
            node_id: node.to_string(),
            sink: sink.to_string(),
            run_hash: hash.to_string(),
        }
    }

    fn wanted() -> BTreeMap<String, Vec<(String, String)>> {
        let pair = |node: &str, sink: &str| (node.to_string(), sink.to_string());
        [
            ("p.py:a".to_string(), vec![pair("p.py:a", "out.json")]),
            (
                "p.py:k".to_string(),
                vec![pair("p.py:k[k=1]", "ds/"), pair("p.py:k[k=2]", "ds/")],
            ),
        ]
        .into()
    }

    async fn record(path: &str, run_id: &str, publications: &[Publication]) {
        let (_db, conn) = db::open_conn(path).await.unwrap();
        write_in(&conn, run_id, publications).await.unwrap();
    }

    #[tokio::test]
    async fn the_last_publication_of_each_step_and_sink_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        db::init_db(&path).await.unwrap();
        record(
            &path,
            "r1",
            &[
                publication("p.py:a", "out.json", "A"),
                publication("p.py:k[k=1]", "ds/", "A1"),
                publication("p.py:k[k=2]", "ds/", "A2"),
                publication("p.py:ka[k=1]", "ds/", "other node"),
            ],
        )
        .await;
        record(&path, "r2", &[publication("p.py:a", "out.json", "B")]).await;
        let read = published(&path, &wanted()).await.unwrap();
        let host = db::local_host();
        assert_eq!(
            read.verdict(&host, "p.py:a", "out.json", "B"),
            Verdict::Current
        );
        assert_eq!(
            read.verdict(&host, "p.py:a", "out.json", "A"),
            Verdict::Stale
        );
        assert_eq!(
            read.verdict(&host, "p.py:k[k=1]", "ds/", "A1"),
            Verdict::Current
        );
        assert_eq!(
            read.verdict(&host, "p.py:k[k=2]", "ds/", "A1"),
            Verdict::Stale
        );
        // `p.py:ka` is not a key of `p.py:k`.
        assert!(
            !read
                .rows
                .keys()
                .any(|(node, _)| node.starts_with("p.py:ka"))
        );
        // Never published: written.
        assert_eq!(
            read.verdict(&host, "p.py:k[k=3]", "ds/", "A3"),
            Verdict::Stale
        );
    }

    #[tokio::test]
    async fn a_local_sink_is_trusted_only_from_this_host() {
        let mut read = Published::default();
        for sink in ["out.json", "file:///x/out.json", "s3://b/out.json"] {
            read.rows.insert(
                ("p.py:a".into(), sink.into()),
                ("A".into(), "another-host".into()),
            );
        }
        assert_eq!(
            read.verdict("here", "p.py:a", "out.json", "A"),
            Verdict::Stale
        );
        assert_eq!(
            read.verdict("here", "p.py:a", "file:///x/out.json", "A"),
            Verdict::Stale
        );
        assert_eq!(
            read.verdict("here", "p.py:a", "s3://b/out.json", "A"),
            Verdict::Current
        );
    }

    /// A database from before the table: the step's latest successful row that wrote sinks
    /// says what they hold.
    #[tokio::test]
    async fn without_a_row_the_history_stands_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db").display().to_string();
        db::init_db(&path).await.unwrap();
        {
            let (_db, conn) = db::open_conn(&path).await.unwrap();
            conn.execute("DROP TABLE sink_publications", ())
                .await
                .unwrap();
            for (node, hash, sinks) in [
                ("p.py:a", "A", "'[]'"),
                ("p.py:a", "B", "'[]'"),
                // A later row that wrote no sinks (it had none) does not count.
                ("p.py:a", "C", "NULL"),
                ("p.py:k[k=1]", "K", "'[]'"),
            ] {
                conn.execute(
                    &format!(
                        "INSERT INTO materializations (node_id, run_hash, artifact_path, status, sinks_json) \
                         VALUES ('{node}', '{hash}', 'x', 'success', {sinks})"
                    ),
                    (),
                )
                .await
                .unwrap();
            }
        }
        let read = published(&path, &wanted()).await.unwrap();
        assert_eq!(read.verdict("h", "p.py:a", "out.json", "B"), Verdict::Adopt);
        assert_eq!(read.verdict("h", "p.py:a", "out.json", "A"), Verdict::Stale);
        assert_eq!(read.verdict("h", "p.py:k[k=1]", "ds/", "K"), Verdict::Adopt);
        // Reading created nothing.
        let (_db, conn) = db::open_conn(&path).await.unwrap();
        assert!(!has_table(&conn).await.unwrap());
        // No database at all reads as empty.
        let none = dir.path().join("none.db").display().to_string();
        assert_eq!(
            published(&none, &wanted())
                .await
                .unwrap()
                .verdict("h", "p.py:a", "o", "A"),
            Verdict::Stale
        );
        assert!(!std::path::Path::new(&none).exists());
    }
}
