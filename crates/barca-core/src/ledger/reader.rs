//! The reader a cache decision asks "is this computed?": this machine's history, or, when the
//! command runs against a server, the answers its ledger gave ([`Ledger::prefetch`]). The
//! server answers `POST /ledger/lookup` ([`super::lookup`]) with the same lookup over its own
//! history, which is why the reader sits with the ledger and below the cache decisions
//! (`crate::cache` names it as `cache::CacheReader` and `cache::lookup_cached`).

use super::Ledger;
use crate::db;
use crate::step_result::OutputRef;

/// One phase's cache lookups: the metadata DB's [`db::CacheReader`] and, when this command runs
/// against a server, its ledger, which answers them in place of this machine's history
/// (RFC-0007 section 4.1). It is the database reader everywhere one is expected.
pub(crate) struct CacheReader {
    db: db::CacheReader,
    ledger: Option<Ledger>,
}

impl CacheReader {
    pub(crate) async fn for_config(
        cfg: &crate::config::ResolvedConfig,
    ) -> Result<Self, crate::BarcaError> {
        Ok(Self {
            db: db::CacheReader::for_config(cfg).await?,
            ledger: super::attached(cfg).cloned(),
        })
    }

    /// A reader of the database at `db_path`, with no ledger.
    #[cfg(test)]
    pub(crate) async fn open(db_path: &str) -> Result<Self, crate::BarcaError> {
        Ok(Self {
            db: db::CacheReader::open(db_path).await?,
            ledger: None,
        })
    }

    /// The server's ledger, when this command runs against one.
    pub(crate) fn ledger(&self) -> Option<&Ledger> {
        self.ledger.as_ref()
    }
}

impl std::ops::Deref for CacheReader {
    type Target = db::CacheReader;

    fn deref(&self) -> &db::CacheReader {
        &self.db
    }
}

/// The most recent successful materialization of `node_id` with this run hash, if any. The row
/// is the cache hit; whether its artifact can still be read is settled only when something
/// needs to read it (see [`crate::recover`]).
pub(crate) async fn lookup_cached(
    cache: &CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<OutputRef> {
    // With a server, its ledger answers, not this machine's history (RFC-0007 section 4.1).
    // Every key a decision looks up was asked for before the phase was decided; one that was
    // not is a miss, and the step runs here.
    if let Some(ledger) = cache.ledger() {
        return ledger.answer(node_id, run_hash).flatten();
    }
    // Prepared once per reader: a dry run of a 200k-key node asks 200k times, and compiling
    // the statement each time cost more than the lookup.
    const WITH_OUTPUT_HASH: &str = "SELECT artifact_path, artifact_format, artifact_size_bytes, \
         output_hash FROM materializations WHERE node_id = ?1 AND run_hash = ?2 \
         AND status = 'success' ORDER BY id DESC";
    // A database from before barca recorded output hashes has no such column.
    const WITHOUT: &str = "SELECT artifact_path, artifact_format, artifact_size_bytes \
         FROM materializations WHERE node_id = ?1 AND run_hash = ?2 \
         AND status = 'success' ORDER BY id DESC";
    let statement = cache.lookup_statement(&[WITH_OUTPUT_HASH, WITHOUT]).await;
    let mut rows = match statement
        .query([node_id.to_string(), run_hash.to_string()])
        .await
    {
        Ok(rows) => rows,
        Err(e) => return lookup_failed(node_id, e),
    };
    loop {
        let row = match rows.next().await {
            Ok(Some(row)) => row,
            Ok(None) => return None,
            Err(e) => return lookup_failed(node_id, e),
        };
        let Some(path) = row.get::<String>(0).ok() else {
            continue;
        };
        if !cache.allows_artifact(&path) {
            continue;
        }
        return Some(OutputRef {
            path,
            format: row.get::<String>(1).ok()?,
            size_bytes: row.get::<i64>(2).ok()? as u64,
            elapsed_seconds: None,
            content_hash: row.get::<String>(3).ok().filter(|h| !h.is_empty()),
        });
    }
}

/// A lookup the database could not answer is a miss: the step runs again, which costs time and
/// never gives a wrong result. A `--read-only` server saw this beside a writer's `VACUUM` while
/// barca dropped its own SQLite locks ([`crate::db::held_file`]).
fn lookup_failed(node_id: &str, e: libsql::Error) -> Option<OutputRef> {
    crate::errln!("[barca] warning: cache lookup of {node_id} failed: {e}");
    None
}
