//! `POST /sql/query`: a query over barca's own tables (`barca.runs`, `barca.materializations`,
//! `barca.nodes`), answered by the server so that a browser, which cannot run DuckDB over the
//! server's records, can ask an aggregate question. `barca sql --server` sends a query that
//! names only those tables here too.
//!
//! The server never runs SQL over results or files: the database the query runs in holds
//! barca's tables and nothing else, and cannot open a file, a URL or an extension
//! (`python/barca/_sql_server.py` states how each of that is enforced).
//!
//! **One helper.** [`QueryHelper`] owns one `python -m barca._sql_server` per server: started
//! by the first query, started again when it is found dead, killed with the server (its stdin
//! is a pipe only this process holds, so it also exits when the server is killed outright).
//! Queries run one at a time; up to [`MAX_PENDING`] are admitted (one running, the others
//! waiting inside their own time limit) and a query beyond them is answered `429` at once, so
//! one client sending slow queries holds a bounded number of requests, not every one.
//! The helper runs outside the project: its working directory and `HOME` are an empty
//! directory of its own, removed with it.
//!
//! **Size.** The helper refuses a query whose rows pass a few megabytes
//! (`MAX_ANSWER_BYTES` in the helper), and this side reads no line of it longer than
//! [`MAX_LINE`]: a longer one is a helper that lost the protocol, and is replaced. DuckDB's
//! memory limit bounds only its own buffers; these bound what Python and the server hold.
//!
//! **Tables.** [`SOURCES`] lists them. A table is handed to the helper when a query names it
//! and it is not loaded for the current state: the metadata database on disk
//! ([`AppState::db_state`]) and, for a table computed from the graph, the inspected graph
//! ([`AppState::inspect_dag`]). A query on an unchanged server sends the helper nothing but
//! the query. A new table is one more entry in [`SOURCES`].
//!
//! **Time.** A query has [`TIMEOUT`], waiting for the helper and loading tables included. Past
//! it the helper is told to interrupt the query; if it has not answered [`CANCEL_GRACE`]
//! later it is killed. Either way the answer is `408` and no query keeps running.

use crate::error::ApiError;
use crate::handlers::SnapshotDb;
use crate::read_cache::{STATUS_MAX_AGE, Stamp};
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use barca_core::dag::Dag;
use barca_core::sql::{SqlResult, tables};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;

/// Rows answered when the request names no `limit`.
pub const DEFAULT_LIMIT: usize = 1_000;
/// The most rows one answer holds.
pub const MAX_LIMIT: usize = 10_000;
/// How long a query has, waiting for the helper and loading tables included.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// How long an interrupted query has to stop before the helper is killed.
const CANCEL_GRACE: Duration = Duration::from_secs(1);
/// How long the helper has to start (the interpreter, importing DuckDB).
const START_LIMIT: Duration = Duration::from_secs(30);
/// How long the helper has to take one table's rows. Generous: it holds the helper's lock,
/// and a helper that has not answered by then is replaced.
const LOAD_LIMIT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 60 });
/// The most rows of one table the helper holds: the newest ones. More are named in `notes`.
pub const TABLE_ROWS: usize = 100_000;
/// The longest line read from the helper: its answer is at most 4MB of rows
/// (`MAX_ANSWER_BYTES` in `python/barca/_sql_server.py`) and the column names around them.
const MAX_LINE: usize = 16 * 1024 * 1024;
/// Queries admitted at once: one running and the rest waiting for the helper. One more is
/// answered `429`.
pub const MAX_PENDING: usize = 4;
/// What a `429` tells the client to wait, in seconds.
const BUSY_RETRY_AFTER: u64 = 1;

/// The module the helper runs.
const MODULE: &str = "barca._sql_server";

/// Environment variables the helper inherits, by name: what an interpreter needs to start and
/// find `barca`. It gets none of the server's credentials (it reads no store and no file, and
/// what it is not given it cannot leak), and `HOME` is its own empty directory.
const ENV_KEPT: &[&str] = &[
    "PATH",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "VIRTUAL_ENV",
    "PYTHONPATH",
    "PYTHONHOME",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
];

/// What a table is loaded from.
pub(crate) struct Inputs {
    pub(crate) state: AppState,
    /// The metadata database to read.
    pub(crate) snapshot: Arc<SnapshotDb>,
    /// The state of the database on disk the rows are of.
    pub(crate) db: Stamp,
    /// The inspected graph: given to a [`Source`] with `graph` set.
    pub(crate) dag: Option<Arc<Dag>>,
}

/// A table's rows, newest first, and how many there are in all.
pub(crate) struct Rows {
    pub(crate) rows: Vec<Value>,
    pub(crate) total: usize,
}

type Load = for<'a> fn(&'a Inputs) -> BoxFuture<'a, Result<Rows, ApiError>>;

/// One of barca's tables as the route serves it.
pub(crate) struct Source {
    /// Its name in the `barca` schema.
    pub(crate) name: &'static str,
    /// (column, DuckDB type), in order: the columns `barca sql` gives the table.
    pub(crate) columns: &'static [(&'static str, &'static str)],
    /// Whether its rows are computed from the graph as well as the database. Such a table is
    /// loaded again when the inspected graph changes, and at least every [`STATUS_MAX_AGE`]
    /// (what it shows also moves with the clock: a schedule turning late).
    pub(crate) graph: bool,
    /// Read at most [`TABLE_ROWS`] rows, newest first.
    pub(crate) load: Load,
}

/// The tables of `POST /sql/query`. To serve another one, add its columns and row builder to
/// `barca_core::sql::tables` (where `barca sql` on a machine reads them too) and an entry
/// here that reads its rows from [`Inputs`]; nothing else names the tables.
pub(crate) const SOURCES: &[Source] = &[
    Source {
        name: tables::RUNS,
        columns: tables::RUN_COLUMNS,
        graph: false,
        load: runs,
    },
    Source {
        name: tables::MATERIALIZATIONS,
        columns: tables::MATERIALIZATION_COLUMNS,
        graph: false,
        load: materializations,
    },
    Source {
        name: tables::NODES,
        columns: tables::NODE_COLUMNS,
        graph: true,
        load: nodes,
    },
];

fn rows_of(table: tables::Table, total: usize) -> Rows {
    Rows {
        rows: table.rows.unwrap_or_default(),
        total,
    }
}

fn runs(inputs: &Inputs) -> BoxFuture<'_, Result<Rows, ApiError>> {
    Box::pin(async move {
        let path = &inputs.snapshot.path;
        let total = barca_core::db::count_runs(path).await?;
        let rows = barca_core::db::get_recent_runs(path, TABLE_ROWS).await?;
        Ok(rows_of(tables::runs(Some(&rows)), total))
    })
}

fn materializations(inputs: &Inputs) -> BoxFuture<'_, Result<Rows, ApiError>> {
    Box::pin(async move {
        let (rows, total) =
            barca_core::db::read_step_rows(&inputs.snapshot.path, TABLE_ROWS, 0).await?;
        Ok(rows_of(tables::materializations(Some(&rows)), total))
    })
}

/// The nodes of `GET /nodes`: the kept status result, so a table loaded for an unchanged graph
/// and database computes no status.
fn nodes(inputs: &Inputs) -> BoxFuture<'_, Result<Rows, ApiError>> {
    Box::pin(async move {
        let dag = inputs
            .dag
            .as_ref()
            .expect("a graph table is given the graph");
        let status = crate::inspect::node_status(
            &inputs.state,
            dag,
            &inputs.db,
            &inputs.snapshot,
            Vec::new(),
            0,
        )
        .await?;
        let total = status.nodes.len();
        Ok(rows_of(tables::nodes(Some(&status.nodes)), total))
    })
}

fn table_list() -> String {
    SOURCES
        .iter()
        .map(|s| format!("barca.{}", s.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a loaded table was loaded for.
struct Loaded {
    db: Stamp,
    dag: Option<Arc<Dag>>,
    at: Instant,
    /// Said with every answer that names the table: rows left out.
    note: Option<String>,
}

impl Loaded {
    fn current(&self, source: &Source, db: &Stamp, dag: Option<&Arc<Dag>>) -> bool {
        let same_graph = match (&self.dag, dag) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        self.db == *db && same_graph && !(source.graph && self.at.elapsed() >= STATUS_MAX_AGE)
    }
}

/// The running helper and what it holds.
struct Process {
    // Killed when dropped.
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    answers: BufReader<tokio::process::ChildStdout>,
    /// The line being read: kept here so that a read given up at a deadline loses nothing.
    partial: Vec<u8>,
    next_id: u64,
    loaded: HashMap<&'static str, Loaded>,
    /// Server paths that an error message must not carry ([`Process::scrub`]).
    private: Vec<String>,
    /// The helper's working directory and `HOME`: empty, removed when the helper is dropped.
    _home: tempfile::TempDir,
}

/// Why a request to the helper has no answer.
enum Lost {
    /// The helper is gone, or said something that is not the protocol: it is replaced.
    Dead(String),
    /// DuckDB is not installed in the server's environment.
    NoDuckdb(String),
}

impl Lost {
    fn into_api(self) -> ApiError {
        match self {
            Lost::Dead(why) => ApiError::NoHelper(format!(
                "the query helper stopped: {why}\n\
                 Send the query again: the server starts a new helper. The other routes are \
                 not affected."
            )),
            Lost::NoDuckdb(message) => ApiError::NoHelper(format!(
                "{message}\n\
                 Install it there (`uv add 'barca[sql]'`) and send the query again: the helper \
                 starts with the next query. The other routes are not affected."
            )),
        }
    }
}

impl Process {
    async fn start(python: &std::path::Path) -> Result<Self, Lost> {
        let home = tempfile::Builder::new()
            .prefix("barca-sql-")
            .tempdir()
            .map_err(|e| Lost::Dead(format!("no directory to run it in ({e})")))?;
        // The interpreter by an absolute path: the helper does not start in the project.
        let python = match python.components().count() > 1 && python.is_relative() {
            true => std::env::current_dir()
                .map(|here| here.join(python))
                .unwrap_or_else(|_| python.to_path_buf()),
            false => python.to_path_buf(),
        };
        // `-P`: nothing is put in front of `sys.path` (with `-m` that is the working directory).
        let mut cmd = tokio::process::Command::new(&python);
        cmd.args(["-P", "-m", MODULE]);
        cmd.env_clear();
        for (name, value) in std::env::vars_os() {
            if name.to_str().is_some_and(|n| ENV_KEPT.contains(&n)) {
                cmd.env(name, value);
            }
        }
        // Not the project root (the server's working directory), so nothing in the project is
        // importable or relative to the helper, and not the server user's home, which DuckDB
        // looks under for secrets and extensions.
        cmd.current_dir(home.path()).env("HOME", home.path());
        // The terminal's Ctrl-C is the server's to act on. Stdin is the request channel and
        // the lifeline at once: only this process holds its other end.
        barca_core::helper_proc::shield_from_ctrl_c(&mut cmd);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(barca_core::term::child_output())
            .kill_on_drop(true);
        let mut child = barca_core::helper_proc::spawn(&mut cmd)
            .map_err(|e| Lost::Dead(format!("cannot run {}: {e}", python.display())))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut process = Process {
            child,
            stdin,
            answers: BufReader::new(stdout),
            partial: Vec::new(),
            next_id: 0,
            loaded: HashMap::new(),
            private: private_paths(&python, home.path()),
            _home: home,
        };
        let hello = tokio::time::timeout(START_LIMIT, process.line())
            .await
            .map_err(|_| {
                Lost::Dead(format!(
                    "it did not start within {}s",
                    START_LIMIT.as_secs()
                ))
            })??;
        if hello["error"]["kind"] == "no_duckdb" {
            let message = hello["error"]["message"].as_str().unwrap_or_default();
            return Err(Lost::NoDuckdb(message.to_string()));
        }
        if hello["ready"] != true {
            return Err(Lost::Dead(format!("unexpected first line: {hello}")));
        }
        // Every table exists from the start, as a placeholder until it is loaded.
        for source in SOURCES {
            process.load(source, None).await?;
        }
        Ok(process)
    }

    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    async fn say(&mut self, message: &Value) -> Result<(), Lost> {
        let mut line = message.to_string();
        line.push('\n');
        let sent = async {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.flush().await
        };
        sent.await
            .map_err(|e| Lost::Dead(format!("it does not take requests ({e})")))
    }

    /// The helper's next line, at most [`MAX_LINE`] bytes of it. Cancel-safe: what was read
    /// of a line stays in `partial`.
    async fn line(&mut self) -> Result<Value, Lost> {
        loop {
            let read = self.answers.fill_buf().await;
            let buffer =
                read.map_err(|e| Lost::Dead(format!("its answer cannot be read ({e})")))?;
            if buffer.is_empty() {
                return Err(Lost::Dead("it exited".into()));
            }
            let end = buffer.iter().position(|b| *b == b'\n');
            let taken = end.unwrap_or(buffer.len());
            self.partial.extend_from_slice(&buffer[..taken]);
            self.answers.consume(taken + usize::from(end.is_some()));
            if self.partial.len() > MAX_LINE {
                return Err(Lost::Dead(format!(
                    "it answered a line longer than {}MB",
                    MAX_LINE / (1024 * 1024)
                )));
            }
            if end.is_some() {
                let line = std::mem::take(&mut self.partial);
                return serde_json::from_slice(&line).map_err(|e| {
                    Lost::Dead(format!("it answered something that is not JSON ({e})"))
                });
            }
        }
    }

    /// `message` without the server's own paths (its project root and home, the interpreter's
    /// environment, the helper's directory): DuckDB names paths in some errors, and an error
    /// goes to whoever sent the query.
    fn scrub(&self, message: &str) -> String {
        let mut out = message.to_string();
        for path in &self.private {
            out = out.replace(path.as_str(), "<server>");
        }
        out
    }

    /// The answer to request `id`.
    async fn answer(&mut self, id: u64) -> Result<Value, Lost> {
        loop {
            let answer = self.line().await?;
            if answer["id"] == id {
                return Ok(answer);
            }
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Replace `source`'s table with `rows`, or with a placeholder.
    async fn load(&mut self, source: &Source, rows: Option<&[Value]>) -> Result<(), Lost> {
        let id = self.id();
        self.say(&json!({"op": "load", "id": id, "table": {
            "schema": tables::SCHEMA,
            "name": source.name,
            "columns": source.columns,
            "rows": rows,
        }}))
        .await?;
        let answer = tokio::time::timeout(LOAD_LIMIT, self.answer(id))
            .await
            .map_err(|_| {
                Lost::Dead(format!(
                    "it did not load barca.{} within {}s",
                    source.name,
                    LOAD_LIMIT.as_secs()
                ))
            })??;
        if answer["ok"] == true {
            return Ok(());
        }
        Err(Lost::Dead(format!(
            "it could not load barca.{}: {}",
            source.name,
            answer["error"]["message"].as_str().unwrap_or("no reason")
        )))
    }
}

/// The server paths an answer must not name, longest first: the project root, the server
/// user's home, the Python environment and the helper's directory, each also as the
/// filesystem resolves it (`/var` is `/private/var` on macOS).
fn private_paths(python: &std::path::Path, helper_home: &std::path::Path) -> Vec<String> {
    let mut paths: Vec<std::path::PathBuf> = vec![helper_home.to_path_buf()];
    paths.extend(std::env::current_dir().ok());
    paths.extend(std::env::var_os("HOME").map(Into::into));
    paths.extend(std::env::var_os("VIRTUAL_ENV").map(Into::into));
    // `<environment>/bin/python`: the environment.
    paths.extend(python.parent().and_then(|bin| bin.parent()).map(Into::into));
    let resolved: Vec<_> = paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
    paths.extend(resolved);
    let mut paths: Vec<String> = paths
        .iter()
        .map(|p| p.display().to_string())
        .filter(|p| p.len() > 1)
        .collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.len()));
    paths.dedup();
    paths
}

/// The server's one query helper ([`AppState::sql`]).
pub struct QueryHelper {
    /// Held for a whole query: one at a time.
    process: tokio::sync::Mutex<Option<Process>>,
    /// One permit per admitted query ([`MAX_PENDING`]): the one running and those waiting.
    pending: tokio::sync::Semaphore,
    /// [`TIMEOUT`] in milliseconds (shorter in tests).
    timeout_ms: AtomicU64,
    /// How many helpers were started and how many tables were handed over (tests).
    pub(crate) starts: AtomicUsize,
    pub(crate) loads: AtomicUsize,
}

impl Default for QueryHelper {
    fn default() -> Self {
        Self {
            process: tokio::sync::Mutex::new(None),
            pending: tokio::sync::Semaphore::new(MAX_PENDING),
            timeout_ms: AtomicU64::new(TIMEOUT.as_millis() as u64),
            starts: AtomicUsize::new(0),
            loads: AtomicUsize::new(0),
        }
    }
}

/// What ends a query without rows.
enum Ended {
    /// An answer for the client; the helper is fine.
    Api(ApiError),
    /// The helper is replaced before the next query.
    Replace(ApiError),
}

impl From<ApiError> for Ended {
    fn from(e: ApiError) -> Self {
        Ended::Api(e)
    }
}

impl From<barca_core::BarcaError> for Ended {
    fn from(e: barca_core::BarcaError) -> Self {
        Ended::Api(e.into())
    }
}

impl From<Lost> for Ended {
    fn from(lost: Lost) -> Self {
        Ended::Replace(lost.into_api())
    }
}

impl QueryHelper {
    fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) fn set_timeout(&self, timeout: Duration) {
        self.timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    /// The helper's process id, when one is running (tests).
    #[cfg(test)]
    pub(crate) async fn pid(&self) -> Option<u32> {
        self.process
            .lock()
            .await
            .as_ref()
            .and_then(|p| p.child.id())
    }

    fn timed_out(&self) -> ApiError {
        ApiError::Timeout(format!(
            "the query did not finish within {}s and was cancelled\n\
             Narrow it (a filter, an aggregate over fewer rows, a smaller join) and send it \
             again.",
            self.timeout().as_secs_f64()
        ))
    }

    /// Run `query` over barca's tables as they are now, for at most `limit` rows.
    pub(crate) async fn run(
        &self,
        state: &AppState,
        query: &str,
        limit: usize,
    ) -> Result<SqlResult, ApiError> {
        let Ok(_admitted) = self.pending.try_acquire() else {
            return Err(ApiError::Busy {
                message: format!(
                    "the server is answering {MAX_PENDING} queries already (one at a time, \
                     the others waiting)\n\
                     Send the query again in a moment."
                ),
                retry_after: BUSY_RETRY_AFTER,
            });
        };
        let deadline = Instant::now() + self.timeout();
        let Ok(mut slot) = tokio::time::timeout_at(deadline, self.process.lock()).await else {
            return Err(self.timed_out());
        };
        if slot.as_mut().is_some_and(Process::exited) {
            *slot = None;
        }
        if slot.is_none() {
            self.starts.fetch_add(1, Ordering::Relaxed);
            *slot = Some(
                Process::start(&state.config.python)
                    .await
                    .map_err(Lost::into_api)?,
            );
        }
        let process = slot.as_mut().expect("a helper was started");
        match self.serve(process, state, query, limit, deadline).await {
            Ok(result) => Ok(result),
            Err(Ended::Api(e)) => Err(e),
            Err(Ended::Replace(e)) => {
                // Dropped: killed. The next query starts another.
                *slot = None;
                Err(e)
            }
        }
    }

    /// Hand the helper the tables `query` names that it does not hold for the current state.
    async fn refresh(
        &self,
        process: &mut Process,
        state: &AppState,
        query: &str,
    ) -> Result<Vec<String>, Ended> {
        // `query('...')` and `query_table(...)` read a table by a name built at run time, which
        // the text does not show: a query that calls either gets every table.
        let indirect = ["query", "query_table"]
            .iter()
            .any(|function| barca_core::sql::names(query, function));
        let named: Vec<&Source> = SOURCES
            .iter()
            .filter(|s| indirect || tables::named(query, s.name))
            .collect();
        if named.is_empty() {
            return Ok(Vec::new());
        }
        let db = state.db_state();
        let dag = match named.iter().any(|s| s.graph) {
            true => Some(state.inspect_dag().await?),
            false => None,
        };
        let mut inputs: Option<Inputs> = None;
        let mut notes = Vec::new();
        for source in named {
            let graph = dag.as_ref().filter(|_| source.graph);
            let current = process
                .loaded
                .get(source.name)
                .is_some_and(|l| l.current(source, &db, graph));
            if !current {
                if inputs.is_none() {
                    inputs = Some(Inputs {
                        state: state.clone(),
                        snapshot: crate::handlers::snapshot_db(state).await?,
                        db: db.clone(),
                        dag: dag.clone(),
                    });
                }
                let Rows { rows, total } =
                    (source.load)(inputs.as_ref().expect("just set")).await?;
                process.loaded.remove(source.name);
                process.load(source, Some(&rows)).await?;
                self.loads.fetch_add(1, Ordering::Relaxed);
                let note = (total > rows.len()).then(|| {
                    format!(
                        "barca.{} holds the newest {} of its {total} rows (the server's query \
                         route loads at most {TABLE_ROWS} rows of each table)",
                        source.name,
                        rows.len()
                    )
                });
                // A database written within the last moment may not show its next write in
                // the stamp: answer from these rows, and read them again for the next query.
                if db.db_settled() {
                    process.loaded.insert(
                        source.name,
                        Loaded {
                            db: db.clone(),
                            dag: graph.cloned(),
                            at: Instant::now(),
                            note: note.clone(),
                        },
                    );
                }
                notes.extend(note);
            } else if let Some(note) = process.loaded.get(source.name).and_then(|l| l.note.clone())
            {
                notes.push(note);
            }
        }
        Ok(notes)
    }

    async fn serve(
        &self,
        process: &mut Process,
        state: &AppState,
        query: &str,
        limit: usize,
        deadline: Instant,
    ) -> Result<SqlResult, Ended> {
        let notes = self.refresh(process, state, query).await?;
        // The tables stay loaded: the same query sent again starts with its whole time.
        if Instant::now() >= deadline {
            return Err(Ended::Api(self.timed_out()));
        }
        let id = process.id();
        process
            .say(&json!({"op": "query", "id": id, "query": query, "limit": limit}))
            .await?;
        let answer = match tokio::time::timeout_at(deadline, process.answer(id)).await {
            Ok(answer) => answer?,
            Err(_) => {
                // Interrupt it. A helper that does not stop is killed: either way nothing
                // keeps running for a client that is no longer waiting.
                process.say(&json!({"op": "cancel", "id": id})).await?;
                return Err(
                    match tokio::time::timeout(CANCEL_GRACE, process.answer(id)).await {
                        Ok(Ok(_)) => Ended::Api(self.timed_out()),
                        _ => Ended::Replace(self.timed_out()),
                    },
                );
            }
        };
        let Some(error) = answer.get("error") else {
            let columns = serde_json::from_value(answer["columns"].clone()).unwrap_or_default();
            let rows = match &answer["rows"] {
                Value::Array(rows) => rows.clone(),
                _ => Vec::new(),
            };
            return Ok(SqlResult {
                columns,
                rows,
                total: answer["total"].as_u64().unwrap_or_default(),
                truncated: answer["truncated"].as_bool().unwrap_or_default(),
                notes,
            });
        };
        let message = process.scrub(error["message"].as_str().unwrap_or("SQL error"));
        let first = message.lines().next().unwrap_or("SQL error");
        let only = format!(
            "The server answers one SELECT over barca's tables ({}). It reads no result view \
             and no file: run such a query with `barca sql`, on a machine with the project.",
            table_list()
        );
        Err(match error["kind"].as_str().unwrap_or_default() {
            "refused" => Ended::Api(ApiError::BadRequest(format!(
                "the server does not run this query: {first}\n{only}"
            ))),
            "missing_table" => Ended::Api(ApiError::BadRequest(format!(
                "no table named '{}' on the server\n{only}",
                process.scrub(error["table"].as_str().unwrap_or_default())
            ))),
            "interrupted" => Ended::Api(self.timed_out()),
            // The transaction is rolled back and the memory released: the helper and the
            // tables it holds stay.
            "out_of_memory" => Ended::Api(ApiError::BadRequest(format!(
                "the query needs more memory than the server gives a query: {first}\n\
                 Narrow it (a filter, an aggregate over fewer rows, fewer or narrower columns) \
                 and send it again."
            ))),
            "internal" => Ended::Replace(Lost::Dead(first.to_string()).into_api()),
            _ => Ended::Api(ApiError::BadRequest(format!("{first}\n{only}"))),
        })
    }
}

/// The body of `POST /sql/query`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SqlQuery {
    /// One `SELECT` over barca's tables (DuckDB dialect).
    pub query: String,
    /// Rows to answer: 1000 when absent, at most 10000 (a larger value is lowered to it).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<usize>,
}

/// The answer of `POST /sql/query`: the document `barca sql --json` prints.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SqlAnswer {
    /// The first `limit` rows, one object per row, keyed by column.
    #[cfg_attr(feature = "ts", ts(type = "Array<Record<string, unknown>>"))]
    pub rows: Vec<Value>,
    /// How many rows the query returns in all.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: u64,
    /// True when `rows` holds fewer than `total`.
    pub truncated: bool,
    /// What to send to see more; only when truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub hint: Option<String>,
    /// Column names, in order.
    pub columns: Vec<String>,
    /// What the rows leave out (a table cut at the server's row limit); absent when nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub notes: Option<Vec<String>>,
}

/// `POST /sql/query` — one `SELECT` over barca's tables, answered as `barca sql --json`
/// prints it. A read: a `--read-only` server serves it.
pub async fn query(
    State(state): State<AppState>,
    body: Result<Json<SqlQuery>, JsonRejection>,
) -> Result<Json<SqlAnswer>, ApiError> {
    let Json(body) = body.map_err(|e| {
        ApiError::BadRequest(format!(
            "expected a JSON body {{\"query\": \"...\", \"limit\": N}}: {}",
            e.body_text()
        ))
    })?;
    if body.query.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "the query is empty\nSend one SELECT over barca's tables in `query`.".into(),
        ));
    }
    let limit = body.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    // In a task of its own: a client that goes away must not leave a request half sent to the
    // helper, whose next answer would then be read as someone else's.
    let (helper, server) = (state.sql.clone(), state.clone());
    let result = tokio::spawn(async move { helper.run(&server, &body.query, limit).await })
        .await
        .map_err(|e| ApiError::Barca(barca_core::BarcaError::Other(format!("sql query: {e}"))))??;
    let truncated = (result.rows.len() as u64) < result.total;
    Ok(Json(SqlAnswer {
        hint: truncated.then(|| {
            format!(
                "send a larger limit (at most {MAX_LIMIT}) for more of the {} rows, or \
                 aggregate in the query",
                result.total
            )
        }),
        rows: result.rows,
        total: result.total,
        truncated,
        columns: result.columns,
        notes: (!result.notes.is_empty()).then_some(result.notes),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const PIPELINE: &str = "from barca import asset\n\n\
        @asset()\ndef first() -> dict:\n    return {\"n\": 1}\n\n\
        @asset(inputs={\"first\": first})\ndef second(first: dict) -> dict:\n    return first\n";

    /// A Python that imports this checkout's `barca`, and with `shadow` a `duckdb` that cannot
    /// be imported.
    fn python(dir: &Path, without_duckdb: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
        let mut path = tree.display().to_string();
        if without_duckdb {
            let shadow = dir.join("shadow");
            std::fs::create_dir_all(&shadow).unwrap();
            std::fs::write(
                shadow.join("duckdb.py"),
                "raise ImportError(\"No module named 'duckdb'\")\n",
            )
            .unwrap();
            path = format!("{}:{path}", shadow.display());
        }
        // The checkout's environment has DuckDB (the `test` extra); a bare `python3` may not.
        let venv = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.venv/bin/python");
        let interpreter = match venv.exists() {
            true => venv,
            false => barca_core::commands::find_python(),
        };
        let launcher = dir.join("query-python");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nPYTHONPATH=\"{path}\" exec \"{}\" \"$@\"\n",
                interpreter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        launcher
    }

    async fn server(dir: &Path, read_only: bool, without_duckdb: bool) -> AppState {
        let file = dir.join("pipeline.py");
        std::fs::write(&file, PIPELINE).unwrap();
        let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
        std::fs::create_dir_all(dir.join(".barca")).unwrap();
        resolved.db_path = dir.join(".barca/metadata.db").display().to_string();
        barca_core::db::init_db(&resolved.db_path).await.unwrap();
        let state = AppState::new(crate::ServeConfig {
            files: vec![file.display().to_string()],
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: python(dir, without_duckdb),
            resolved,
            read_only,
        });
        // The database and the sources are trusted once they have been still for a moment.
        tokio::time::sleep(crate::read_cache::SOURCES_SETTLED + Duration::from_millis(100)).await;
        state
    }

    async fn ask(state: &AppState, query: &str) -> Result<SqlAnswer, (u16, Value)> {
        let body = SqlQuery {
            query: query.to_string(),
            limit: None,
        };
        match super::query(State(state.clone()), Ok(Json(body))).await {
            Ok(Json(answer)) => Ok(answer),
            Err(e) => {
                let (status, envelope) = e.parts();
                Err((status.as_u16(), envelope.to_json()))
            }
        }
    }

    /// Whether the Python the tests run has DuckDB: without it (a checkout whose environment
    /// has no `sql` extra) only the `503` is checked, here as in `no_duckdb_is_a_503`.
    async fn has_duckdb(state: &AppState) -> bool {
        match ask(state, "select 1 as one").await {
            Ok(_) => true,
            Err((503, body)) => {
                assert!(body["error"].as_str().unwrap().contains("duckdb"), "{body}");
                false
            }
            Err(other) => panic!("{other:?}"),
        }
    }

    fn counts(state: &AppState) -> (usize, usize, usize) {
        (
            state.sql.starts.load(Ordering::Relaxed),
            state.sql.loads.load(Ordering::Relaxed),
            state.reads.status_computes.load(Ordering::Relaxed),
        )
    }

    #[tokio::test]
    async fn an_aggregate_over_barcas_tables_is_answered_and_an_unchanged_server_loads_nothing() {
        for read_only in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let state = server(dir.path(), read_only, false).await;
            if !has_duckdb(&state).await {
                return;
            }
            let runs = ask(&state, "select count(*) as n from barca.runs")
                .await
                .unwrap();
            assert_eq!(runs.columns, ["n"]);
            assert_eq!(runs.rows, [json!({"n": 0})]);
            assert!(!runs.truncated && runs.hint.is_none() && runs.notes.is_none());
            let nodes = ask(
                &state,
                "select kind, count(*) as n from barca.nodes group by all",
            )
            .await
            .unwrap();
            assert_eq!(nodes.rows, [json!({"kind": "asset", "n": 2})]);
            // One helper; `runs` and `nodes` handed over once each; one status computed.
            let after = counts(&state);
            assert_eq!(after, (1, 2, 1), "read_only = {read_only}");
            for _ in 0..3 {
                ask(&state, "select count(*) from barca.runs r, barca.nodes n")
                    .await
                    .unwrap();
            }
            assert_eq!(counts(&state), after, "an unchanged server loads nothing");
            // A table the query does not name is not loaded, and reading it says so.
            let listed = ask(&state, "show tables").await.unwrap();
            assert_eq!(listed.total, SOURCES.len() as u64);
            // A database write is seen by the next query that names a table.
            barca_core::db::create_run(
                &state.config.resolved.db_path,
                "run-1",
                "get",
                "[]",
                None,
                Some(1),
            )
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            let runs = ask(
                &state,
                "select run_id, command from barca.runs where steps_total = 1",
            )
            .await
            .unwrap();
            assert_eq!(runs.rows, [json!({"run_id": "run-1", "command": "get"})]);
            // Only the table the query names was handed over again.
            assert_eq!(counts(&state).1, after.1 + 1);
        }
    }

    #[tokio::test]
    async fn rows_past_the_limit_are_counted_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        let body = SqlQuery {
            query: "select * from range(50000) t(i)".into(),
            limit: Some(1_000_000),
        };
        let Json(answer) = super::query(State(state.clone()), Ok(Json(body)))
            .await
            .map_err(|e| format!("{e:?}"))
            .unwrap();
        assert_eq!(answer.rows.len(), MAX_LIMIT);
        assert_eq!(answer.total, 50_000);
        assert!(answer.truncated && answer.hint.is_some());
        let answer = ask(&state, "select * from range(50000) t(i)")
            .await
            .unwrap();
        assert_eq!(answer.rows.len(), DEFAULT_LIMIT);
    }

    /// INV-HEALTH-09: every way around "one SELECT over barca's tables" is a `400` with the
    /// usage envelope, and none of them changes what the tables hold.
    #[tokio::test]
    async fn everything_but_one_select_over_barcas_tables_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        let outside = dir.path().join("outside.csv");
        std::fs::write(&outside, "a\n1\n").unwrap();
        let written = dir.path().join("written.csv");
        let (outside, written) = (outside.display(), written.display());
        let attempts = [
            // A result view, or any relation that is not one of barca's tables.
            "select * from first".to_string(),
            "select * from \"pipeline.py:first\"".to_string(),
            // Files, by every reader and by name.
            format!("select * from read_parquet('{outside}')"),
            format!("select * from read_csv('{outside}')"),
            format!("select * from read_csv_auto('{outside}')"),
            format!("select * from read_json('{outside}')"),
            format!("select * from read_text('{outside}')"),
            format!("select * from read_blob('{outside}')"),
            format!("select * from '{outside}'"),
            "select * from glob('*')".to_string(),
            "select * from 'https://example.invalid/a.parquet'".to_string(),
            // Writes and other statements.
            format!("COPY barca.runs TO '{written}'"),
            format!("COPY (select 1) TO '{written}'"),
            format!("ATTACH '{written}' AS other"),
            format!("EXPORT DATABASE '{written}'"),
            // DuckDB 1.x parses a table-function pragma as a SELECT (2.x as a PRAGMA).
            "PRAGMA database_list".to_string(),
            "PRAGMA table_info('barca.runs')".to_string(),
            "PRAGMA show_tables".to_string(),
            "PRAGMA version".to_string(),
            "PRAGMA database_size".to_string(),
            "PRAGMA storage_info('barca.runs')".to_string(),
            "/* a query */ PRAGMA database_list".to_string(),
            "-- a query\nPRAGMA database_list".to_string(),
            "(PRAGMA database_list)".to_string(),
            format!("PRAGMA import_database('{}')", dir.path().display()),
            "PRAGMA copy_database('memory', 'memory')".to_string(),
            "PRAGMA enable_profiling".to_string(),
            "CALL duckdb_tables()".to_string(),
            "PIVOT barca.runs ON status USING count(*)".to_string(),
            "SET enable_external_access = true".to_string(),
            "RESET enable_external_access".to_string(),
            "INSTALL httpfs".to_string(),
            "LOAD httpfs".to_string(),
            "CREATE TABLE barca.mine AS SELECT 1".to_string(),
            "DROP TABLE barca.runs".to_string(),
            "DELETE FROM barca.runs".to_string(),
            "INSERT INTO barca.runs (run_id) VALUES ('mine')".to_string(),
            "UPDATE barca.runs SET status = 'mine'".to_string(),
            "CALL pragma_version()".to_string(),
            "EXPLAIN ANALYZE select 1".to_string(),
            "CHECKPOINT".to_string(),
            // More than one statement.
            "select 1; select 2".to_string(),
            "select 1; DROP TABLE barca.runs".to_string(),
            format!("select 1; COPY barca.runs TO '{written}'"),
            // A write hidden in a SELECT.
            "WITH gone AS (DELETE FROM barca.runs RETURNING *) SELECT * FROM gone".to_string(),
            "WITH new AS (INSERT INTO barca.runs (run_id) VALUES ('mine') RETURNING run_id) \
             SELECT * FROM new"
                .to_string(),
            "select * from query('DROP TABLE barca.runs')".to_string(),
            // The server's environment.
            "select getenv('HOME')".to_string(),
            "not sql at all".to_string(),
        ];
        for attempt in &attempts {
            let (status, body) = ask(&state, attempt)
                .await
                .expect_err(&format!("answered: {attempt}"));
            assert_eq!(status, 400, "{attempt}: {body}");
            assert_eq!(body["kind"], "usage", "{attempt}");
            assert!(
                body["remediation"]
                    .as_str()
                    .is_some_and(|r| r.contains("barca.runs")),
                "{attempt}: {body}"
            );
        }
        assert!(!dir.path().join("written.csv").exists());
        // The helper was never replaced, external access is still off, and the tables are
        // what they were.
        assert_eq!(state.sql.starts.load(Ordering::Relaxed), 1);
        let setting = ask(
            &state,
            "select value from duckdb_settings() where name = 'enable_external_access'",
        )
        .await
        .unwrap();
        assert_eq!(setting.rows, [json!({"value": "false"})]);
        let runs = ask(&state, "select count(*) as n from barca.runs")
            .await
            .unwrap();
        assert_eq!(runs.rows, [json!({"n": 0})]);
        let mine = ask(&state, "show tables").await.unwrap();
        assert_eq!(mine.total, SOURCES.len() as u64, "{:?}", mine.rows);
        // An empty query and a body that is not the request are usage errors too.
        let (status, _) = ask(&state, "  ").await.unwrap_err();
        assert_eq!(status, 400);
    }

    /// Days of CPU in little memory: a cross product of small tables.
    const RUNAWAY: &str = "select count(*) from range(100000) a, range(100000) b, range(100000) c";

    #[tokio::test]
    async fn a_query_past_its_time_is_a_408_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        state.sql.set_timeout(Duration::from_millis(500));
        let started = std::time::Instant::now();
        let (status, body) = ask(&state, RUNAWAY).await.unwrap_err();
        assert_eq!(status, 408, "{body}");
        assert_eq!(body["kind"], "usage");
        assert!(started.elapsed() < Duration::from_millis(500) + CANCEL_GRACE * 2);
        // It was interrupted, not left running: the same helper answers the next query at once.
        state.sql.set_timeout(TIMEOUT);
        let started = std::time::Instant::now();
        ask(&state, "select 1").await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(state.sql.starts.load(Ordering::Relaxed), 1);
        // A query that waits for the helper longer than its time is a 408 as well.
        state.sql.set_timeout(Duration::from_millis(300));
        let (first, second) = tokio::join!(ask(&state, RUNAWAY), ask(&state, "select 1"));
        assert_eq!(first.unwrap_err().0, 408);
        assert_eq!(second.unwrap_err().0, 408);
    }

    #[tokio::test]
    async fn a_killed_helper_is_a_503_for_its_query_and_is_started_again() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        ask(&state, "select count(*) from barca.runs")
            .await
            .unwrap();
        let pid = state.sql.pid().await.expect("a helper runs");
        let kill = |pid: u32| {
            // SAFETY: plain syscall on the helper this test started.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        };
        // Killed between queries: the next one starts another and loads its tables again.
        kill(pid);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let runs = ask(&state, "select count(*) as n from barca.runs")
            .await
            .unwrap();
        assert_eq!(runs.rows, [json!({"n": 0})]);
        assert_eq!(state.sql.starts.load(Ordering::Relaxed), 2);
        // Killed under a query: that query is a 503 naming what to do, the next is answered.
        let second = state.sql.pid().await.unwrap();
        assert_ne!(second, pid);
        let (running, _) = tokio::join!(ask(&state, RUNAWAY), async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            kill(second);
        });
        let (status, body) = running.unwrap_err();
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["kind"], "infra");
        assert!(body["remediation"].as_str().unwrap().contains("again"));
        ask(&state, "select 1").await.unwrap();
        assert_eq!(state.sql.starts.load(Ordering::Relaxed), 3);
    }

    /// A query that needs more memory than DuckDB is given fails as a usage error: nothing is
    /// spilled to the server's working directory (DuckDB's default is `.tmp/` there), and the
    /// helper and the tables it holds stay, so nobody can make the server reload them at will.
    #[tokio::test]
    async fn a_query_that_needs_too_much_memory_is_a_400_writes_no_file_and_keeps_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        state.sql.set_timeout(Duration::from_secs(60));
        ask(&state, "select count(*) from barca.runs")
            .await
            .unwrap();
        let before = counts(&state);
        for hungry in [
            "select count(*) from (select distinct i, i * 2 as j from range(200000000) t(i))",
            "select string_agg(repeat('x', 1000), '') from range(100000000)",
        ] {
            let (status, body) = ask(&state, hungry).await.unwrap_err();
            assert_eq!(status, 400, "{body}");
            assert_eq!(body["kind"], "usage");
            assert!(body["error"].as_str().unwrap().contains("memory"), "{body}");
        }
        for at in [std::env::current_dir().unwrap(), dir.path().to_path_buf()] {
            assert!(!at.join(".tmp").exists(), "{}", at.display());
        }
        let runs = ask(&state, "select count(*) as n from barca.runs")
            .await
            .unwrap();
        assert_eq!(runs.rows, [json!({"n": 0})]);
        assert_eq!(counts(&state), before, "the helper and its tables stayed");
    }

    /// An answer is a few megabytes at most: wide rows are refused in the helper before they
    /// are read to the end, so neither it nor the server holds a gigabyte of them.
    #[tokio::test]
    async fn rows_past_the_answer_size_are_refused_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        for wide in [
            "select repeat('x', 100000) as s from range(10000)",
            "select range(3000) as l from range(10000)",
        ] {
            let body = SqlQuery {
                query: wide.into(),
                limit: Some(MAX_LIMIT),
            };
            let started = std::time::Instant::now();
            let (status, envelope) = super::query(State(state.clone()), Ok(Json(body)))
                .await
                .unwrap_err()
                .parts();
            let envelope = envelope.to_json();
            assert_eq!(status.as_u16(), 400, "{wide}: {envelope}");
            assert_eq!(envelope["kind"], "usage");
            let error = envelope["error"].as_str().unwrap();
            assert!(
                error.contains("4MB") && error.contains("narrower"),
                "{error}"
            );
            assert!(started.elapsed() < Duration::from_secs(4), "{wide}");
        }
        // Under the size, the same rows are answered, and counted to the end.
        let body = SqlQuery {
            query: "select repeat('x', 100000) as s from range(10000)".into(),
            limit: Some(10),
        };
        let Json(answer) = super::query(State(state.clone()), Ok(Json(body)))
            .await
            .map_err(|e| format!("{e:?}"))
            .unwrap();
        assert_eq!((answer.rows.len(), answer.total), (10, 10_000));
        assert_eq!(state.sql.starts.load(Ordering::Relaxed), 1);
    }

    /// A stand-in for the helper that runs `script` (Python) instead of the module.
    fn impostor(dir: &Path, script: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let code = dir.join("impostor.py");
        std::fs::write(&code, script).unwrap();
        let launcher = dir.join("impostor-python");
        std::fs::write(
            &launcher,
            format!("#!/bin/sh\nexec python3 \"{}\"\n", code.display()),
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        launcher
    }

    /// The server reads no line longer than [`MAX_LINE`] from the helper, whatever the helper
    /// sends: it stops reading and replaces it.
    #[tokio::test]
    async fn a_helper_line_past_the_cap_is_a_dead_helper() {
        let dir = tempfile::tempdir().unwrap();
        let python = impostor(
            dir.path(),
            "import sys\nprint('{\"ready\": true}', flush=True)\n\
             chunk = 'x' * (1024 * 1024)\n\
             for _ in range(4096):\n    sys.stdout.write(chunk)\n",
        );
        let started = std::time::Instant::now();
        let Err(lost) = Process::start(&python).await else {
            panic!("a helper that sends 4GB on one line was accepted");
        };
        let Lost::Dead(why) = lost else {
            panic!("not a dead helper");
        };
        assert!(why.contains("longer than 16MB"), "{why}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    /// A helper that does not take a table within its time is replaced: the lock it holds is
    /// released.
    #[tokio::test]
    async fn a_table_load_past_its_time_is_a_dead_helper() {
        let dir = tempfile::tempdir().unwrap();
        let python = impostor(
            dir.path(),
            "import time\nprint('{\"ready\": true}', flush=True)\ntime.sleep(600)\n",
        );
        let started = std::time::Instant::now();
        let Err(Lost::Dead(why)) = Process::start(&python).await else {
            panic!("a helper that never loads was accepted");
        };
        assert!(why.contains("did not load barca.runs within"), "{why}");
        assert!(started.elapsed() < LOAD_LIMIT + Duration::from_secs(5));
    }

    /// One client cannot hold the route for everyone: beyond [`MAX_PENDING`] queries a new
    /// one is answered at once.
    #[tokio::test]
    async fn queries_beyond_the_admitted_ones_are_a_429_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        state.sql.set_timeout(Duration::from_millis(1500));
        let slow: Vec<_> = (0..MAX_PENDING)
            .map(|_| {
                let state = state.clone();
                tokio::spawn(async move { ask(&state, RUNAWAY).await })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(300)).await;
        for _ in 0..5 {
            let started = std::time::Instant::now();
            let body = SqlQuery {
                query: "select 1".into(),
                limit: None,
            };
            let response = axum::response::IntoResponse::into_response(
                super::query(State(state.clone()), Ok(Json(body)))
                    .await
                    .unwrap_err(),
            );
            // (`into_parts`: the guard on child processes reads a `status()` call as one.)
            let (parts, _) = response.into_parts();
            assert_eq!(parts.status.as_u16(), 429);
            assert_eq!(parts.headers["retry-after"], "1");
            assert!(started.elapsed() < Duration::from_millis(200));
        }
        for held in slow {
            assert_eq!(held.await.unwrap().unwrap_err().0, 408);
        }
        // They are done: the route admits again.
        state.sql.set_timeout(TIMEOUT);
        ask(&state, "select 1").await.unwrap();
    }

    /// What an answer or an error says about the server: nothing of its home, its project or
    /// its Python environment.
    #[tokio::test]
    async fn no_answer_names_a_directory_of_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        let mut private: Vec<String> = [
            std::env::var("HOME").ok(),
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            Some(std::env::temp_dir().display().to_string()),
            Some(dir.path().display().to_string()),
        ]
        .into_iter()
        .flatten()
        .collect();
        private.push(".duckdb".into());
        for probe in [
            "select * from duckdb_secrets()",
            "select * from duckdb_extensions()",
            "select current_setting('secret_directory') as d",
            "select current_setting('extension_directory') as d",
            "select current_setting('home_directory') as d",
            "select current_setting('temp_directory') as d",
            "select name, value from duckdb_settings()",
            "select * from read_csv('secrets.csv')",
            "select * from read_csv('~/secrets.csv')",
            "select * from glob('~/*')",
            "select * from test_all_types()",
            "select * from duckdb_databases()",
            "INSTALL httpfs",
        ] {
            let said = match ask(&state, probe).await {
                Ok(answer) => serde_json::to_string(&answer).unwrap(),
                Err((_, body)) => body.to_string(),
            };
            for path in &private {
                assert!(
                    !said.contains(path.as_str()),
                    "{probe} names {path}: {said}"
                );
            }
        }
        let secrets = ask(&state, "select current_setting('secret_directory') as d")
            .await
            .unwrap();
        assert_eq!(secrets.rows, [json!({"d": ""})]);
        // The helper's own directory is scrubbed from an error that carries it.
        let slot = state.sql.process.lock().await;
        let process = slot.as_ref().unwrap();
        let home = process._home.path().display().to_string();
        assert_eq!(
            process.scrub(&format!("Cannot access directory \"{home}/.duckdb\"")),
            "Cannot access directory \"<server>/.duckdb\""
        );
    }

    /// A query is run as it was sent, never pasted inside another: a comment or a `;` at its
    /// end changes nothing, also when the rows are cut and counted.
    #[tokio::test]
    async fn a_trailing_comment_or_semicolon_does_not_break_a_truncated_query() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        for query in [
            "select * from range(5000) -- c",
            "select * from range(5000); -- c",
            "select * from range(5000) /* c */ ;",
            "select * from range(5000) -- c\n;",
            "select * from range(5000)\n-- c ) ;",
        ] {
            let answer = ask(&state, query)
                .await
                .unwrap_or_else(|e| panic!("{query}: {e:?}"));
            assert_eq!((answer.rows.len(), answer.total), (1000, 5000), "{query}");
            assert!(answer.truncated);
        }
        let short = ask(&state, "select ';' as a -- ;").await.unwrap();
        assert_eq!(short.rows, [json!({"a": ";"})]);
        // Every way a query starts is still one: a comment or a parenthesis first, and the
        // statements DuckDB reads as a SELECT.
        for query in [
            "/* how many */ select count(*) as n from barca.runs",
            "-- how many\nselect count(*) as n from barca.runs",
            "/* a /* nested */ comment */ (select count(*) as n from barca.runs)",
            "with r as (select * from barca.runs) select count(*) as n from r",
            "from barca.runs select count(*) as n",
            "values (0)",
            "table barca.runs",
            "describe barca.runs",
            "summarize barca.runs",
            "show tables",
        ] {
            ask(&state, query)
                .await
                .unwrap_or_else(|e| panic!("{query}: {e:?}"));
        }
    }

    /// `query_table` and `query` reach a table by a name the text does not show: every table
    /// is loaded for a query that calls one, so it never reads a placeholder or old rows.
    #[tokio::test]
    async fn a_table_reached_without_being_named_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, false).await;
        if !has_duckdb(&state).await {
            return;
        }
        let runs = ask(
            &state,
            "select count(*) as n from query_table('barca.' || 'ru' || 'ns')",
        )
        .await
        .unwrap();
        assert_eq!(runs.rows, [json!({"n": 0})]);
        assert_eq!(state.sql.loads.load(Ordering::Relaxed), SOURCES.len());
        barca_core::db::create_run(
            &state.config.resolved.db_path,
            "run-1",
            "get",
            "[]",
            None,
            Some(1),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let runs = ask(
            &state,
            "select * from query('select count(*) as n from barca.' || 'ru' || 'ns')",
        )
        .await
        .unwrap();
        assert_eq!(runs.rows, [json!({"n": 1})]);
    }

    #[tokio::test]
    async fn no_duckdb_is_a_503_that_names_the_extra_and_no_other_route_is_affected() {
        let dir = tempfile::tempdir().unwrap();
        let state = server(dir.path(), false, true).await;
        let (status, body) = ask(&state, "select 1").await.unwrap_err();
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["kind"], "infra");
        assert!(body["error"].as_str().unwrap().contains("duckdb"), "{body}");
        assert!(
            body["remediation"].as_str().unwrap().contains("barca[sql]"),
            "{body}"
        );
        let nodes = crate::handlers::state(
            State(state.clone()),
            axum::extract::Query(Default::default()),
        )
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();
        assert_eq!(nodes.0.len(), 2);
    }

    /// The helper gets none of the server's credentials, and no `PYTHON*` variable but the two
    /// an interpreter may need to find `barca`.
    #[test]
    fn the_helper_keeps_no_credential_of_the_server() {
        for name in [
            "AWS_SECRET_ACCESS_KEY",
            "AZURE_STORAGE_KEY",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "BARCA_SERVER_TOKEN",
            "BARCA_STORAGE_OPTIONS",
            "HOME",
            "PYTHONSTARTUP",
            "PYTHONINSPECT",
            "PYTHONWARNINGS",
            "PYTHONBREAKPOINT",
        ] {
            assert!(!ENV_KEPT.contains(&name), "{name}");
        }
        for name in ["PYTHONPATH", "PYTHONHOME"] {
            assert!(ENV_KEPT.contains(&name), "{name}");
        }
    }
}
