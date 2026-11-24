//! The reader a cache decision asks "is this computed?": this machine's history, or, when the
//! command runs against a server, the answers its ledger gave ([`Ledger::prefetch`]). The
//! server answers `POST /ledger/lookup` ([`super::lookup`]) with the same lookup over its own
//! history, which is why the reader sits with the ledger and below the cache decisions
//! (`barca_model::cache` names it as `cache::CacheReader` and `cache::lookup_cached`).

use super::Ledger;
use barca_db::db;
use barca_model::step_result::OutputRef;

/// One phase's cache lookups: the metadata DB's [`db::CacheReader`] and, when this command runs
/// against a server, its ledger, which answers them in place of this machine's history
/// (RFC-0007 section 4.1). It is the database reader everywhere one is expected.
pub struct CacheReader {
    db: db::CacheReader,
    ledger: Option<Ledger>,
}

impl CacheReader {
    pub async fn for_config(
        cfg: &barca_model::config::ResolvedConfig,
    ) -> Result<Self, barca_model::BarcaError> {
        Ok(Self {
            db: db::CacheReader::for_config(cfg).await?,
            ledger: super::attached(cfg).cloned(),
        })
    }

    /// A reader of the database at `db_path`, with no ledger.
    #[doc(hidden)]
    pub async fn open(db_path: &str) -> Result<Self, barca_model::BarcaError> {
        Ok(Self {
            db: db::CacheReader::open(db_path).await?,
            ledger: None,
        })
    }

    pub async fn recorded_step_ids(
        &self,
        base: &str,
    ) -> Result<Vec<String>, barca_model::BarcaError> {
        match self.ledger() {
            Some(ledger) => ledger.recorded_step_ids(base).await,
            None => self.db.recorded_step_ids(base).await,
        }
    }

    /// The server's ledger, when this command runs against one.
    pub fn ledger(&self) -> Option<&Ledger> {
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
/// needs to read it (see [`barca_model::recover`]).
pub async fn lookup_cached(
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
    match cache.lookup_output(node_id, run_hash).await {
        Ok(output) => output,
        Err(error) => lookup_failed(node_id, error),
    }
}

/// A lookup the database could not answer is a miss: the step runs again, which costs time and
/// never gives a wrong result. A `--read-only` server saw this beside a writer's `VACUUM` while
/// barca dropped its own SQLite locks ([`barca_db::db::held_file`]).
fn lookup_failed(node_id: &str, e: libsql::Error) -> Option<OutputRef> {
    barca_model::errln!("[barca] warning: cache lookup of {node_id} failed: {e}");
    None
}
