//! Server mode's ledger (RFC-0007 section 3.4 "The ledger" and section 4.1).
//!
//! With a server configured, `barca get` and `barca run` still run on this machine, but the
//! answer to "is this computed?" comes from the server, and what this machine computes is
//! recorded there:
//!
//! 1. [`Ledger::prepare`]: `GET /health` (API version, and the server's store must be this
//!    machine's `[remote]`), then every run this machine could not post before is posted again.
//! 2. [`Ledger::prefetch`]: `POST /ledger/lookup` with every run hash of the cone that is known
//!    before anything runs ([`crate::cache::cone_keys`]); a phase whose hashes depend on what
//!    ran before it asks for the rest ([`crate::cache::phase_keys`]). Cache decisions then read
//!    [`Ledger::answer`] instead of this machine's history. A hit is fetched from the store by
//!    the transfer helper like any store-backed cache hit, and a local copy whose digest differs
//!    from the server's is replaced by the store's copy.
//! 3. [`Ledger::record_run`]: after the run's uploads are confirmed, `POST /ledger/runs` with
//!    the run and its step rows. The server verifies each object before it writes anything. A
//!    rejected post (`409`) or a server that cannot be reached leaves the run a success here:
//!    a warning names what was not recorded, and the post is kept in this machine's database
//!    (`ledger_posts`, `unsynced`) until a later command sends it. A run with a result whose
//!    upload is pending (#390) is held there unsent, and [`Ledger::post_uploaded`] sends it
//!    once a retry has uploaded it (#507).
//!
//! The HTTP transport is behind [`LedgerTransport`], so the command decides how to reach the
//! server (the CLI's client) and this module stays free of HTTP.
//!
//! The server half, [`lookup`] and [`accept`], is what `barca serve` answers those requests
//! with: the existing cache lookup, and `persist`'s rows with a verification in front of them.

use crate::BarcaError;
use crate::db;
use crate::dispatch::OutputRef;
use crate::envelope::ErrorEnvelope;
pub use futures::future::BoxFuture;
use serde::Deserialize;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

// ─── Wire types ──────────────────────────────────────────────────────────────

/// One `(node id, run hash)` pair: a node id with a partition suffix for a partition key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LedgerKey {
    pub node_id: String,
    pub run_hash: String,
}

/// The body of `POST /ledger/lookup`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LookupRequest {
    pub hashes: Vec<LedgerKey>,
}

/// What the ledger holds for one pair: the latest successful result, or `exists: false`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LookupEntry {
    pub node_id: String,
    pub run_hash: String,
    pub exists: bool,
    /// The artifact's location in the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// SHA-256 of the artifact's bytes; `null` for a row recorded before digests were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// The answer to `POST /ledger/lookup`: one entry per pair asked, in the order asked.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LookupResponse {
    pub results: Vec<LookupEntry>,
}

/// The run row of a ledger post: a finished run on the machine that posts it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerRun {
    /// The run id on the posting machine. The server records it as `client_run_id` and
    /// answers a repeated post of the same run with the run it already recorded.
    pub run_id: String,
    pub command: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub target: Option<String>,
    pub status: String,
    #[serde(default)]
    pub steps_total: Option<i64>,
    #[serde(default)]
    pub steps_executed: i64,
    #[serde(default)]
    pub steps_cached: i64,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
    /// The posting machine's host name.
    #[serde(default)]
    pub host: Option<String>,
}

/// One step row of a ledger post: a materialization the run recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerStep {
    pub node_id: String,
    pub run_hash: String,
    /// `success` or `failed`.
    pub status: String,
    /// The artifact's location in the store; required for a successful step.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// SHA-256 of the artifact's bytes; required for a successful step.
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
    #[serde(default)]
    pub attempts: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The body of `POST /ledger/runs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerPost {
    pub run: LedgerRun,
    pub steps: Vec<LedgerStep>,
}

/// The answer to an accepted post (`201`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PostAccepted {
    /// The run id the server recorded the run under.
    pub run_id: String,
    /// Steps whose run hash the ledger already held with a different digest. The ledger keeps
    /// its row; the posting machine prints the non-determinism warning for each.
    #[serde(default)]
    pub bytes_changed: Vec<String>,
}

/// What a client needs from `GET /health`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerHealth {
    #[serde(default)]
    pub api_version: Option<u32>,
    /// The artifact store the server writes to; `null` when it has none.
    #[serde(default)]
    pub store_uri: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
}

// ─── The transport ───────────────────────────────────────────────────────────

/// Why a request to the ledger did not succeed.
#[derive(Clone, Debug)]
pub enum LedgerError {
    /// `409`: the server refused a post (an object missing or not matching); the envelope
    /// names the node. Nothing from the run was recorded.
    Rejected(Box<ErrorEnvelope>),
    /// Any other answer with an error envelope (`400`, `503`, `500`).
    Refused {
        status: u16,
        envelope: Box<ErrorEnvelope>,
    },
    /// The server could not be reached, or answered something that is not the API.
    Unreachable(String),
}

impl LedgerError {
    /// One line saying what happened, for a warning or an error message.
    pub fn describe(&self) -> String {
        match self {
            LedgerError::Rejected(e) => format!("the server refused it: {}", e.error),
            LedgerError::Refused { status, envelope } => {
                format!("the server answered {status}: {}", envelope.error)
            }
            LedgerError::Unreachable(why) => format!("the server could not be reached: {why}"),
        }
    }
}

/// How a command reaches the server. The CLI implements it over HTTP; tests implement it in
/// memory.
pub trait LedgerTransport: Send + Sync {
    /// The server's base URL, as configured.
    fn url(&self) -> &str;
    /// `GET /health`.
    fn health(&self) -> BoxFuture<'_, Result<ServerHealth, LedgerError>>;
    /// `POST /ledger/lookup`.
    fn lookup<'a>(
        &'a self,
        request: &'a LookupRequest,
    ) -> BoxFuture<'a, Result<LookupResponse, LedgerError>>;
    /// `POST /ledger/runs`.
    fn post_run<'a>(
        &'a self,
        post: &'a LedgerPost,
    ) -> BoxFuture<'a, Result<PostAccepted, LedgerError>>;
}

// ─── The client ──────────────────────────────────────────────────────────────

type Answers = HashMap<(String, String), Option<OutputRef>>;

struct Inner {
    transport: Box<dyn LedgerTransport>,
    answers: Mutex<Answers>,
    prepared: tokio::sync::OnceCell<()>,
    /// Where the server's URL came from (`BARCA_SERVER`, `[server] url in barca.toml`), for
    /// the `[barca] server <url> (<source>)` line.
    source: Option<String>,
}

/// One command's link to a server's ledger. Cheap to clone; clones share the answers.
#[derive(Clone)]
pub struct Ledger {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledger").field("url", &self.url()).finish()
    }
}

impl Ledger {
    pub fn new(transport: impl LedgerTransport + 'static) -> Self {
        Self::with_source(transport, None)
    }

    /// A ledger whose URL came from `source` (e.g. `BARCA_SERVER`), which the
    /// `[barca] server <url> (<source>)` line names.
    pub fn with_source(transport: impl LedgerTransport + 'static, source: Option<&str>) -> Self {
        Self {
            inner: Arc::new(Inner {
                transport: Box::new(transport),
                answers: Mutex::new(HashMap::new()),
                prepared: tokio::sync::OnceCell::new(),
                source: source.map(str::to_string),
            }),
        }
    }

    pub fn url(&self) -> &str {
        self.inner.transport.url()
    }

    fn transport(&self) -> &dyn LedgerTransport {
        self.inner.transport.as_ref()
    }

    /// What the ledger said about this pair: `Some(Some(_))` a hit, `Some(None)` a miss, `None`
    /// when it was not asked.
    pub(crate) fn answer(&self, node_id: &str, run_hash: &str) -> Option<Option<OutputRef>> {
        self.inner
            .answers
            .lock()
            .unwrap()
            .get(&(node_id.to_string(), run_hash.to_string()))
            .cloned()
    }

    /// Once per command: print which server this command uses, check that it is one this
    /// machine can share results with, and (unless `retry` is false, as for a dry run, which
    /// writes nothing) post again the runs this machine could not post before.
    pub async fn prepare(
        &self,
        cfg: &crate::config::ResolvedConfig,
        retry: bool,
    ) -> Result<(), BarcaError> {
        self.inner
            .prepared
            .get_or_try_init(|| async {
                match &self.inner.source {
                    Some(source) => {
                        crate::run_line!(Always, "[barca] server {} ({source})", self.url())
                    }
                    None => crate::run_line!(Always, "[barca] server {}", self.url()),
                }
                self.check_health(cfg).await?;
                if retry {
                    self.retry_unsynced(cfg, None).await;
                }
                Ok::<(), BarcaError>(())
            })
            .await?;
        Ok(())
    }

    async fn check_health(&self, cfg: &crate::config::ResolvedConfig) -> Result<(), BarcaError> {
        let url = self.url();
        if !cfg.remote_artifacts() {
            return Err(BarcaError::Usage(format!(
                "a server is configured ({url}) but this project has no artifact store: the \
                 server never moves artifact bytes, so results are shared through the store\n\
                 Add a [remote] section with the uri the server uses (its GET /health \
                 reports it as store_uri), or pass --local to keep this run on this machine."
            )));
        }
        let health = self
            .transport()
            .health()
            .await
            .map_err(|e| unreachable_error(url, "GET /health", &e))?;
        match health.api_version {
            Some(crate::outcome::API_VERSION) => {}
            other => {
                return Err(BarcaError::Other(format!(
                    "the barca server at {url} speaks API version {}, this barca speaks {}\n\
                     Use a barca release that matches the server, or pass --local to run \
                     against this machine's ledger only.",
                    other.map_or("(none)".to_string(), |v| v.to_string()),
                    crate::outcome::API_VERSION
                )));
            }
        }
        let theirs = health.store_uri.as_deref().map(normalize_uri);
        let ours = normalize_uri(&cfg.artifact_root);
        if theirs.as_deref() != Some(ours.as_str()) {
            return Err(BarcaError::Usage(format!(
                "the barca server at {url} writes artifacts to {}, this project to {}: a \
                 result one of them records could not be read by the other\n\
                 Point [remote] uri at the server's store (and use the same environment), or \
                 pass --local to keep this run on this machine.",
                health
                    .store_uri
                    .as_deref()
                    .map_or("no store".to_string(), crate::transfer::diagnostic_uri),
                crate::transfer::diagnostic_uri(&cfg.artifact_root),
            )));
        }
        Ok(())
    }

    /// Ask the ledger about every pair in `keys` it has not been asked about yet, in one
    /// request. A server that cannot be reached ends the command (exit 3): a run that silently
    /// fell back to this machine's ledger would stop agreeing with everyone else's.
    pub(crate) async fn prefetch(&self, keys: Vec<(String, String)>) -> Result<(), BarcaError> {
        let ask: Vec<LedgerKey> = {
            let answers = self.inner.answers.lock().unwrap();
            let mut seen = HashSet::new();
            keys.into_iter()
                .filter(|k| !answers.contains_key(k) && seen.insert(k.clone()))
                .map(|(node_id, run_hash)| LedgerKey { node_id, run_hash })
                .collect()
        };
        if ask.is_empty() {
            return Ok(());
        }
        let request = LookupRequest { hashes: ask };
        let response = self
            .transport()
            .lookup(&request)
            .await
            .map_err(|e| unreachable_error(self.url(), "POST /ledger/lookup", &e))?;
        let mut answers = self.inner.answers.lock().unwrap();
        for key in &request.hashes {
            answers.insert((key.node_id.clone(), key.run_hash.clone()), None);
        }
        for entry in response.results {
            let hit = match (entry.exists, entry.path, entry.format) {
                (true, Some(path), Some(format)) => Some(OutputRef {
                    path,
                    format,
                    size_bytes: entry.size_bytes.unwrap_or(0),
                    elapsed_seconds: None,
                    content_hash: entry.digest.filter(|d| !d.is_empty()),
                }),
                _ => None,
            };
            answers.insert((entry.node_id, entry.run_hash), hit);
        }
        Ok(())
    }

    /// Post the finished run `run_id` to the ledger and say which of its steps the ledger
    /// already held with different bytes. Never fails the run: a post that is refused or does
    /// not arrive prints a warning and is kept `unsynced` for the next command.
    pub(crate) async fn record_run(
        &self,
        cfg: &crate::config::ResolvedConfig,
        run_id: &str,
        dag: &crate::dag::Dag,
    ) -> Vec<String> {
        // A run some of whose results are not in the store yet (#390: their upload failed and
        // is pending) is not posted now: the server would refuse objects it cannot find, and
        // the team must not be told of results only this machine holds. Its post is kept
        // `unsynced`, and the command whose retry uploads them sends it (#507).
        let post = match build_post(&cfg.db_path, run_id, dag).await {
            Ok(Some(post)) => post,
            Ok(None) => return Vec::new(),
            Err(e) => {
                crate::run_line!(
                    Always,
                    "[barca] warning: run {run_id} was not recorded on the server {}: {e}",
                    self.url()
                );
                return Vec::new();
            }
        };
        if is_held(&post, &pending_paths(cfg).await) {
            let kept = posts::record(&cfg.db_path, self.url(), &post, Err(NOT_UPLOADED)).await;
            crate::run_line!(
                Always,
                "[barca] warning: run {run_id} is not recorded on the server {} yet: some of its \
                 results are not in the artifact store yet (barca status shows them unsynced); \
                 the next command that uploads them sends it{}",
                self.url(),
                kept.err()
                    .map(|e| format!(" (could not keep the post: {e})"))
                    .unwrap_or_default()
            );
            return Vec::new();
        }
        match self.send(cfg, &post).await {
            Ok(accepted) => accepted.bytes_changed,
            Err(e) => {
                crate::run_line!(
                    Always,
                    "[barca] warning: run {run_id} was not recorded on the server {}: {}; it \
                     stays on this machine (unsynced) and the next command against the server \
                     sends it again",
                    self.url(),
                    e.describe()
                );
                Vec::new()
            }
        }
    }

    /// Post `post` and record the outcome in this machine's `ledger_posts`.
    async fn send(
        &self,
        cfg: &crate::config::ResolvedConfig,
        post: &LedgerPost,
    ) -> Result<PostAccepted, LedgerError> {
        let result = self.transport().post_run(post).await;
        if let Ok(accepted) = &result {
            self.remember(post, &accepted.bytes_changed);
        }
        let outcome = match &result {
            Ok(accepted) => Ok(accepted.run_id.clone()),
            Err(e) => Err(e.describe()),
        };
        let recorded = posts::record(
            &cfg.db_path,
            self.url(),
            post,
            outcome.as_deref().map_err(String::as_str),
        )
        .await;
        if let Err(e) = recorded {
            crate::run_line!(
                Always,
                "[barca] warning: could not record the ledger post of run {}: {e}",
                post.run.run_id
            );
        }
        result
    }

    /// The steps of an accepted post are hits for the rest of this command: a run whose post
    /// was held for its uploads, posted once the retry uploaded them, is not computed again.
    fn remember(&self, post: &LedgerPost, bytes_changed: &[String]) {
        let mut answers = self.inner.answers.lock().unwrap();
        for step in &post.steps {
            if step.status != "success" || bytes_changed.contains(&step.node_id) {
                continue;
            }
            let (Some(path), Some(format)) = (&step.path, &step.format) else {
                continue;
            };
            answers.insert(
                (step.node_id.clone(), step.run_hash.clone()),
                Some(OutputRef {
                    path: path.clone(),
                    format: format.clone(),
                    size_bytes: step.size_bytes.unwrap_or(0),
                    elapsed_seconds: None,
                    content_hash: step.digest.clone().filter(|d| !d.is_empty()),
                }),
            );
        }
    }

    /// Post the held runs with a result at one of `store_paths`, whose pending uploads a retry
    /// has just settled (#507), after the retry and before this command's own steps are
    /// decided.
    pub(crate) async fn post_uploaded(
        &self,
        cfg: &crate::config::ResolvedConfig,
        store_paths: &HashSet<String>,
    ) {
        if !store_paths.is_empty() {
            self.retry_unsynced(cfg, Some(store_paths)).await;
        }
    }

    /// Post again every run this machine could not post to this server before (those with a
    /// result at one of `only`, when given). A run with a result still pending upload stays
    /// held: the server would refuse it.
    async fn retry_unsynced(
        &self,
        cfg: &crate::config::ResolvedConfig,
        only: Option<&HashSet<String>>,
    ) {
        if !std::path::Path::new(&cfg.db_path).exists() {
            return;
        }
        let unsynced = match posts::unsynced(&cfg.db_path, self.url()).await {
            Ok(unsynced) => unsynced,
            Err(e) => {
                crate::run_line!(
                    Always,
                    "[barca] warning: could not read the runs waiting for the server: {e}"
                );
                return;
            }
        };
        let pending = pending_paths(cfg).await;
        for mut post in unsynced {
            let run_id = post.run.run_id.clone();
            if is_held(&post, &pending) || only.is_some_and(|only| !is_held(&post, only)) {
                continue;
            }
            // A held post was built before its uploads, which record the digests they confirm.
            if let Err(e) = posts::refresh_digests(&cfg.db_path, &mut post).await {
                crate::run_line!(
                    Always,
                    "[barca] warning: could not read the digests of run {run_id}: {e}"
                );
            }
            match self.send(cfg, &post).await {
                Ok(accepted) => {
                    crate::run_line!(
                        Always,
                        "[barca] recorded run {run_id} on the server (run {})",
                        accepted.run_id
                    );
                    for node in accepted.bytes_changed {
                        crate::run_line!(Always, "{}", bytes_changed_warning(&node));
                    }
                }
                Err(e) => crate::run_line!(
                    Always,
                    "[barca] warning: run {run_id} is still not recorded on the server {}: {}",
                    self.url(),
                    e.describe()
                ),
            }
        }
    }
}

/// Why a held post (#507) is `unsynced`: the run's uploads are pending.
const NOT_UPLOADED: &str = "some of its results are not in the artifact store yet";

/// The store locations of this host's results whose upload is pending (#390).
async fn pending_paths(cfg: &crate::config::ResolvedConfig) -> HashSet<String> {
    crate::pending_uploads::list(&cfg.db_path)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.store_path)
        .collect()
}

/// Whether one of `post`'s results is at one of `paths`.
fn is_held(post: &LedgerPost, paths: &HashSet<String>) -> bool {
    post.steps
        .iter()
        .any(|s| s.path.as_ref().is_some_and(|p| paths.contains(p)))
}

/// The warning for a step whose bytes differ from what was recorded for the same run hash
/// (`specs/materialization-digests.md`).
pub(crate) fn bytes_changed_warning(node: &str) -> String {
    format!(
        "[barca] warning: {node} produced different bytes for the same code and inputs; the \
         function is not deterministic"
    )
}

/// A store URI compared as text, without a trailing `/` or `file://`.
fn normalize_uri(uri: &str) -> String {
    let uri = uri.strip_prefix("file://").unwrap_or(uri);
    uri.trim_end_matches('/').to_string()
}

fn unreachable_error(url: &str, route: &str, e: &LedgerError) -> BarcaError {
    match e {
        LedgerError::Refused { status, envelope } if (400..500).contains(status) => {
            BarcaError::Usage(format!(
                "the barca server at {url} refused {route}: {}{}",
                envelope.error,
                envelope
                    .remediation
                    .as_deref()
                    .map(|r| format!("\n{r}"))
                    .unwrap_or_default()
            ))
        }
        _ => BarcaError::Other(format!(
            "the barca server at {url} did not answer {route}: {}\n\
             Check that the server is running and reachable, or pass --local to run \
             against this machine's ledger only.",
            e.describe()
        )),
    }
}

/// The post for the finished run `run_id`: its row and the step rows of its assets, from this
/// machine's database. Sensors and tasks are never cache hits (and a sensor's recorded hash is
/// of its value, not its bytes), so their rows stay here. `None` for a run that is not posted:
/// one that did not succeed (its steps that did stay on this machine), or that computed no
/// asset.
async fn build_post(
    db_path: &str,
    run_id: &str,
    dag: &crate::dag::Dag,
) -> Result<Option<LedgerPost>, BarcaError> {
    let Some(run) = db::get_run(db_path, run_id).await? else {
        return Ok(None);
    };
    if run.status != "success" {
        return Ok(None);
    }
    let mut steps = posts::run_steps(db_path, run_id).await?;
    steps.retain(|step| {
        let id = crate::StepId::parse(&step.node_id);
        // A `parallel()` branch stays on this machine (it is never uploaded).
        !id.partition.0.contains_key("_branch")
            && dag
                .get_node(id.base_id())
                .is_some_and(|n| n.kind() == crate::NodeKind::Asset)
    });
    if steps.is_empty() {
        return Ok(None);
    }
    Ok(Some(LedgerPost {
        run: LedgerRun {
            run_id: run.run_id,
            command: run.command,
            files: run.files,
            target: run.target,
            status: run.status,
            steps_total: run.steps_total,
            steps_executed: run.steps_executed,
            steps_cached: run.steps_cached,
            started_at: run.started_at,
            finished_at: run.finished_at,
            elapsed_seconds: run.elapsed_seconds,
            host: Some(db::local_host()),
        },
        steps,
    }))
}

// ─── This machine's posts ────────────────────────────────────────────────────

/// The runs this machine posted to a server's ledger, or has yet to (`ledger_posts`).
///
/// The table is created the first time a run is posted, never by a reader: a database that has
/// never been used with a server has no table, and every reader treats that as empty. It is an
/// additive table of metadata format 1 (`specs/metadata-schema.md`).
pub mod posts {
    use super::{LedgerPost, LedgerStep};
    use crate::BarcaError;
    use crate::db;
    use std::collections::HashSet;

    const CREATE: &str = "CREATE TABLE IF NOT EXISTS ledger_posts (
        client_run_id TEXT PRIMARY KEY,
        server TEXT NOT NULL,
        body TEXT NOT NULL,
        state TEXT NOT NULL,
        server_run_id TEXT,
        error TEXT,
        attempts INTEGER DEFAULT 0,
        created_at TEXT DEFAULT (datetime('now')),
        updated_at TEXT DEFAULT (datetime('now'))
    )";

    /// A post that has not been accepted yet.
    pub const UNSYNCED: &str = "unsynced";
    /// A post the server accepted.
    pub const SHARED: &str = "shared";

    fn failed(what: &'static str) -> impl Fn(turso::Error) -> BarcaError {
        move |e| BarcaError::Db(format!("ledger posts: {what}: {e}"))
    }

    async fn has_table(conn: &turso::Connection) -> Result<bool, BarcaError> {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'ledger_posts'",
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

    /// Record the outcome of posting `post` to `server`: accepted under `Ok(server run id)`,
    /// or not, with why.
    pub(crate) async fn record(
        db_path: &str,
        server: &str,
        post: &LedgerPost,
        outcome: Result<&str, &str>,
    ) -> Result<(), BarcaError> {
        let body = serde_json::to_string(post)
            .map_err(|e| BarcaError::Other(format!("ledger post: {e}")))?;
        let (state, server_run_id, error) = match outcome {
            Ok(id) => (super::posts::SHARED, id.to_string(), String::new()),
            Err(why) => (super::posts::UNSYNCED, String::new(), why.to_string()),
        };
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await?;
        conn.execute(CREATE, ())
            .await
            .map_err(failed("creating the table"))?;
        conn.execute(
            "INSERT INTO ledger_posts (client_run_id, server, body, state, server_run_id, error, attempts) \
             VALUES (?1, ?2, ?3, ?4, NULLIF(?5, ''), NULLIF(?6, ''), 1) \
             ON CONFLICT(client_run_id) DO UPDATE SET server = excluded.server, \
             body = excluded.body, state = excluded.state, \
             server_run_id = excluded.server_run_id, error = excluded.error, \
             attempts = ledger_posts.attempts + 1, updated_at = datetime('now')",
            [
                post.run.run_id.clone(),
                server.to_string(),
                body,
                state.to_string(),
                server_run_id,
                error,
            ],
        )
        .await
        .map_err(failed("recording a post"))?;
        Ok(())
    }

    /// The posts to `server` that were not accepted, oldest first.
    pub async fn unsynced(db_path: &str, server: &str) -> Result<Vec<LedgerPost>, BarcaError> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await?;
        if !has_table(&conn).await? {
            return Ok(Vec::new());
        }
        let mut rows = conn
            .query(
                "SELECT body FROM ledger_posts WHERE state = ?1 AND server = ?2 \
                 ORDER BY created_at, client_run_id",
                [super::posts::UNSYNCED, server],
            )
            .await
            .map_err(failed("reading posts"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(failed("reading posts"))? {
            let body = row.get::<String>(0).map_err(failed("reading posts"))?;
            if let Ok(post) = serde_json::from_str(&body) {
                out.push(post);
            }
        }
        Ok(out)
    }

    /// One row of `ledger_posts`, as `barca status` and the tests read it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct PostRecord {
        pub client_run_id: String,
        pub server: String,
        pub state: String,
        pub server_run_id: Option<String>,
        pub error: Option<String>,
        /// The step ids the post carries.
        pub nodes: Vec<String>,
    }

    /// Every post this machine made or has yet to make, oldest first.
    pub async fn all(db_path: &str) -> Result<Vec<PostRecord>, BarcaError> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await?;
        if !has_table(&conn).await? {
            return Ok(Vec::new());
        }
        let mut rows = conn
            .query(
                "SELECT client_run_id, server, state, server_run_id, error, body FROM ledger_posts \
                 ORDER BY created_at, client_run_id",
                (),
            )
            .await
            .map_err(failed("reading posts"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(failed("reading posts"))? {
            let text = |i| row.get::<String>(i).ok().filter(|s| !s.is_empty());
            let nodes = text(5)
                .and_then(|b| serde_json::from_str::<LedgerPost>(&b).ok())
                .map(|p| p.steps.into_iter().map(|s| s.node_id).collect())
                .unwrap_or_default();
            out.push(PostRecord {
                client_run_id: text(0).unwrap_or_default(),
                server: text(1).unwrap_or_default(),
                state: text(2).unwrap_or_default(),
                server_run_id: text(3),
                error: text(4),
                nodes,
            });
        }
        Ok(out)
    }

    /// Fill in the digest of each successful step of `post` that has none from this
    /// machine's rows: a retried upload records the digest it confirmed (#507).
    pub(crate) async fn refresh_digests(
        db_path: &str,
        post: &mut LedgerPost,
    ) -> Result<(), BarcaError> {
        if !post
            .steps
            .iter()
            .any(|s| s.status == "success" && s.digest.is_none())
        {
            return Ok(());
        }
        let recorded = run_steps(db_path, &post.run.run_id).await?;
        for step in &mut post.steps {
            if step.digest.is_none()
                && let Some(row) = recorded.iter().find(|r| r.node_id == step.node_id)
            {
                step.digest.clone_from(&row.digest);
            }
        }
        Ok(())
    }

    /// Take the steps whose result is at one of `store_paths` out of the posts still waiting
    /// to be sent: a pending upload that was dropped (its local file is gone) leaves an object
    /// the server would never find. A post with no step left is removed.
    pub(crate) async fn forget_steps(
        db_path: &str,
        store_paths: &HashSet<String>,
    ) -> Result<(), BarcaError> {
        if store_paths.is_empty() {
            return Ok(());
        }
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await?;
        if !has_table(&conn).await? {
            return Ok(());
        }
        let mut rows = conn
            .query("SELECT body FROM ledger_posts WHERE state = ?1", [UNSYNCED])
            .await
            .map_err(failed("reading posts"))?;
        let mut waiting: Vec<LedgerPost> = Vec::new();
        while let Some(row) = rows.next().await.map_err(failed("reading posts"))? {
            if let Some(post) = row
                .get::<String>(0)
                .ok()
                .and_then(|b| serde_json::from_str(&b).ok())
            {
                waiting.push(post);
            }
        }
        for mut post in waiting {
            let before = post.steps.len();
            post.steps
                .retain(|s| !s.path.as_ref().is_some_and(|p| store_paths.contains(p)));
            if post.steps.len() == before {
                continue;
            }
            let run = post.run.run_id.clone();
            if post.steps.is_empty() {
                conn.execute("DELETE FROM ledger_posts WHERE client_run_id = ?1", [run])
                    .await
                    .map_err(failed("removing a post"))?;
            } else {
                let body = serde_json::to_string(&post)
                    .map_err(|e| BarcaError::Other(format!("ledger post: {e}")))?;
                conn.execute(
                    "UPDATE ledger_posts SET body = ?2, updated_at = datetime('now') \
                     WHERE client_run_id = ?1",
                    [run, body],
                )
                .await
                .map_err(failed("updating a post"))?;
            }
        }
        Ok(())
    }

    /// The step rows `run_id` recorded, as a ledger post carries them.
    pub(crate) async fn run_steps(
        db_path: &str,
        run_id: &str,
    ) -> Result<Vec<LedgerStep>, BarcaError> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await?;
        let mut rows = conn
            .query(
                "SELECT node_id, run_hash, status, artifact_path, artifact_format, \
                 artifact_size_bytes, output_hash, elapsed_seconds, attempts, error_message \
                 FROM materializations WHERE run_id = ?1 ORDER BY id",
                [run_id],
            )
            .await
            .map_err(failed("reading the run's steps"))?;
        let mut steps: Vec<LedgerStep> = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(failed("reading the run's steps"))?
        {
            let text = |i| row.get::<String>(i).ok().filter(|s| !s.is_empty());
            let step = LedgerStep {
                node_id: text(0).unwrap_or_default(),
                run_hash: text(1).unwrap_or_default(),
                status: text(2).unwrap_or_else(|| "success".to_string()),
                path: text(3),
                format: text(4),
                size_bytes: row.get::<i64>(5).ok().map(|n| n.max(0) as u64),
                digest: text(6),
                elapsed_seconds: row.get::<f64>(7).ok(),
                attempts: row.get::<i64>(8).ok(),
                error: text(9),
            };
            // A step is recorded as it finishes and again at the end of the run: the last
            // row of a node is its outcome.
            match steps.iter_mut().find(|s| s.node_id == step.node_id) {
                Some(existing) => *existing = step,
                None => steps.push(step),
            }
        }
        Ok(steps)
    }
}

// ─── The server ──────────────────────────────────────────────────────────────

/// `POST /ledger/lookup` on the server: the existing cache lookup for each pair.
pub async fn lookup(
    cfg: &crate::config::ResolvedConfig,
    request: &LookupRequest,
) -> Result<LookupResponse, BarcaError> {
    if request.hashes.is_empty() || !std::path::Path::new(&cfg.db_path).exists() {
        return Ok(LookupResponse {
            results: request
                .hashes
                .iter()
                .map(|k| miss(&k.node_id, &k.run_hash))
                .collect(),
        });
    }
    let cache = db::CacheReader::for_config(cfg).await?;
    let mut results = Vec::with_capacity(request.hashes.len());
    for key in &request.hashes {
        results.push(
            match crate::cache::lookup_cached(&cache, &key.node_id, &key.run_hash).await {
                Some(found) => LookupEntry {
                    node_id: key.node_id.clone(),
                    run_hash: key.run_hash.clone(),
                    exists: true,
                    path: Some(found.path),
                    format: Some(found.format),
                    size_bytes: Some(found.size_bytes),
                    digest: found.content_hash,
                },
                None => miss(&key.node_id, &key.run_hash),
            },
        );
    }
    Ok(LookupResponse { results })
}

fn miss(node_id: &str, run_hash: &str) -> LookupEntry {
    LookupEntry {
        node_id: node_id.to_string(),
        run_hash: run_hash.to_string(),
        exists: false,
        path: None,
        format: None,
        size_bytes: None,
        digest: None,
    }
}

/// Why the server did not accept a post.
#[derive(Debug)]
pub enum Rejection {
    /// `409`: an object is missing or does not match; nothing was recorded.
    Conflict(String),
    /// `400`: the post itself is wrong, or this server cannot take posts.
    Usage(String),
    /// `500`: the store or the database failed.
    Infra(BarcaError),
}

impl From<BarcaError> for Rejection {
    fn from(e: BarcaError) -> Self {
        Rejection::Infra(e)
    }
}

pub use crate::status::StoreObject;

/// How the server asks its store about the objects a post names. `barca serve` uses
/// [`crate::status::stat_objects`]; tests use a closure.
pub trait ObjectStat: Send + Sync {
    fn stat<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<Vec<StoreObject>, String>>;
}

/// [`ObjectStat`] through `python -m barca._state stat` with the server's storage options.
pub struct HelperStat<'a> {
    pub python: &'a std::path::Path,
    pub cfg: &'a crate::config::ResolvedConfig,
}

impl ObjectStat for HelperStat<'_> {
    fn stat<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<Vec<StoreObject>, String>> {
        Box::pin(async move {
            let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
            crate::status::stat_objects(self.python, self.cfg, &paths).await
        })
    }
}

/// `POST /ledger/runs` on the server: verify every object the post names, then record the run
/// (`triggered_by: client`) and its steps. A missing or mismatched object rejects the whole
/// post, and nothing from it is recorded.
pub async fn accept(
    cfg: &crate::config::ResolvedConfig,
    store: &dyn ObjectStat,
    post: &LedgerPost,
) -> Result<PostAccepted, Rejection> {
    if !cfg.remote_artifacts() {
        return Err(Rejection::Usage(
            "this barca server has no artifact store, so it cannot verify a ledger post\n\
             Configure [remote] on the server (and the same on every client)."
                .to_string(),
        ));
    }
    let client_run_id = post.run.run_id.trim();
    if client_run_id.is_empty() {
        return Err(Rejection::Usage("the post's run has no run_id".to_string()));
    }
    if let Some(dir) = std::path::Path::new(&cfg.db_path).parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| BarcaError::Db(format!("failed to create {}: {e}", dir.display())))?;
    }
    db::init_db(&cfg.db_path).await?;
    if let Some(run_id) = recorded_post(&cfg.db_path, client_run_id).await? {
        // The client did not see the answer to an earlier post of this run.
        return Ok(PostAccepted {
            run_id,
            bytes_changed: Vec::new(),
        });
    }

    // Every successful step with an artifact is checked; a step without one is never a cache
    // hit (`lookup` skips it) and is recorded as history only.
    let artifacts: Vec<&LedgerStep> = post
        .steps
        .iter()
        .filter(|s| s.status == "success" && s.path.as_deref().is_some_and(|p| !p.is_empty()))
        .collect();
    let root = normalize_uri(&cfg.artifact_root);
    for step in &artifacts {
        let node = &step.node_id;
        let path = step.path.as_deref().unwrap_or_default();
        if !normalize_uri(path).starts_with(&format!("{root}/")) {
            return Err(Rejection::Conflict(format!(
                "{node}: {path} is not in this server's artifact store ({}); nothing from run \
                 {client_run_id} was recorded\n\
                 Point [remote] uri at the server's store.",
                crate::transfer::diagnostic_uri(&cfg.artifact_root)
            )));
        }
        if step.digest.as_deref().is_none_or(str::is_empty) {
            return Err(Rejection::Conflict(format!(
                "{node}: the post has no content digest for {path}; nothing from run \
                 {client_run_id} was recorded"
            )));
        }
    }
    let paths: Vec<String> = artifacts.iter().filter_map(|s| s.path.clone()).collect();
    if !paths.is_empty() {
        let objects = store.stat(&paths).await.map_err(|why| {
            Rejection::Infra(BarcaError::Other(format!(
                "could not check the posted objects in the store {}: {why}",
                crate::transfer::diagnostic_uri(&cfg.artifact_root)
            )))
        })?;
        for (step, object) in artifacts.iter().zip(objects) {
            let node = &step.node_id;
            let path = step.path.as_deref().unwrap_or_default();
            let digest = step.digest.as_deref().unwrap_or_default();
            let problem = if !object.exists {
                Some(format!("there is no object at {path}"))
            } else if let Some(sha) = object.sha256.as_deref() {
                (sha != digest).then(|| {
                    format!("the object at {path} has digest {sha}, the post says {digest}")
                })
            } else {
                match (object.size_bytes, step.size_bytes) {
                    (Some(have), Some(said)) if have != said => Some(format!(
                        "the object at {path} has {have} bytes, the post says {said}"
                    )),
                    _ => None,
                }
            };
            if let Some(problem) = problem {
                return Err(Rejection::Conflict(format!(
                    "{node}: {problem}; nothing from run {client_run_id} was recorded\n\
                     The next barca command on that machine posts the run again; a result \
                     that is still missing is computed again there."
                )));
            }
        }
    }
    Ok(write_post(&cfg.db_path, post).await?)
}

/// The server run id an earlier post of `client_run_id` was recorded under.
async fn recorded_post(db_path: &str, client_run_id: &str) -> Result<Option<String>, BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT run_id FROM runs WHERE client_run_id = ?1 LIMIT 1",
            [client_run_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read posted runs: {e}")))?;
    let found = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read posted runs: {e}")))?
        .and_then(|row| row.get::<String>(0).ok());
    while rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read posted runs: {e}")))?
        .is_some()
    {}
    Ok(found)
}

/// Record a verified post in one transaction: the run, then each step the ledger does not
/// already hold. A step whose run hash the ledger holds with the same digest adds nothing; with
/// a different digest the ledger keeps its row and the step is reported in `bytes_changed`.
async fn write_post(db_path: &str, post: &LedgerPost) -> Result<PostAccepted, BarcaError> {
    let failed = |e: turso::Error| BarcaError::Db(format!("failed to record a ledger post: {e}"));
    let run_id = db::generate_run_id();
    let run = &post.run;
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    let bytes_changed = db::with_tx(&conn, "BEGIN IMMEDIATE", "ledger post", async {
        conn.execute(
            "INSERT INTO runs (run_id, command, files, target, status, steps_total, \
             steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, host, \
             triggered_by, client_run_id) \
             VALUES (?1, ?2, ?3, NULLIF(?4, ''), ?5, NULLIF(?6, ''), ?7, ?8, ?9, NULLIF(?10, ''), \
             NULLIF(?11, ''), NULLIF(?12, ''), 'client', ?13)",
            [
                run_id.clone(),
                run.command.clone(),
                db::encode_files(&run.files),
                run.target.clone().unwrap_or_default(),
                run.status.clone(),
                run.steps_total.map(|n| n.to_string()).unwrap_or_default(),
                run.steps_executed.to_string(),
                run.steps_cached.to_string(),
                run.started_at.clone(),
                run.finished_at.clone().unwrap_or_default(),
                run.elapsed_seconds
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                run.host.clone().unwrap_or_default(),
                run.run_id.clone(),
            ],
        )
        .await
        .map_err(failed)?;
        let mut bytes_changed = Vec::new();
        for step in &post.steps {
            if step.status == "success" {
                let mut rows = conn
                    .query(
                        "SELECT output_hash FROM materializations WHERE node_id = ?1 AND \
                         run_hash = ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
                        [step.node_id.clone(), step.run_hash.clone()],
                    )
                    .await
                    .map_err(failed)?;
                let held = rows
                    .next()
                    .await
                    .map_err(failed)?
                    .map(|row| row.get::<String>(0).ok().filter(|h| !h.is_empty()));
                while rows.next().await.map_err(failed)?.is_some() {}
                match held {
                    // Held, with the same bytes: nothing to add.
                    Some(Some(digest)) if Some(digest.as_str()) == step.digest.as_deref() => {
                        continue;
                    }
                    // Held with other bytes: the ledger's row stays the truth.
                    Some(Some(_)) => {
                        bytes_changed.push(step.node_id.clone());
                        continue;
                    }
                    // Held without a digest (recorded before digests were), or not held.
                    Some(None) | None => {}
                }
            }
            conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, \
                 artifact_format, artifact_size_bytes, elapsed_seconds, status, error_message, \
                 attempts, output_hash, run_id) \
                 VALUES (?1, ?2, NULLIF(?3, ''), NULLIF(?4, ''), NULLIF(?5, ''), NULLIF(?6, ''), \
                 ?7, NULLIF(?8, ''), ?9, NULLIF(?10, ''), ?11)",
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
                    run_id.clone(),
                ],
            )
            .await
            .map_err(failed)?;
        }
        Ok::<_, BarcaError>(bytes_changed)
    })
    .await?;
    Ok(PostAccepted {
        run_id,
        bytes_changed,
    })
}
