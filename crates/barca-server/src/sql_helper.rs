//! The server's one DuckDB helper process (`python -m barca._sql_server`) and the protocol it
//! speaks: started by the first query, replaced when it dies or loses the protocol, killed
//! when the server's state is dropped. Which tables it is handed, and the route that asks it,
//! are [`crate::sql_query`].

use crate::error::ApiError;
use crate::read_cache::{STATUS_MAX_AGE, Stamp};
use barca_service::dag::Dag;
use barca_service::sql::tables;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;

/// A table as the helper is told of it: its name in the `barca` schema and its
/// (column, DuckDB type) pairs, in order.
pub(crate) type Shape = (&'static str, &'static [(&'static str, &'static str)]);

/// How long a query has, waiting for the helper and loading tables included.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// How long an interrupted query has to stop before the helper is killed.
pub(crate) const CANCEL_GRACE: Duration = Duration::from_secs(1);
/// How long the helper has to start (the interpreter, importing DuckDB).
pub(crate) const START_LIMIT: Duration = Duration::from_secs(30);
/// How long the helper has to take one table's rows. Generous: it holds the helper's lock,
/// and a helper that has not answered by then is replaced.
pub(crate) const LOAD_LIMIT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 60 });

/// The longest line read from the helper: its answer is at most 4MB of rows
/// (`MAX_ANSWER_BYTES` in `python/barca/_sql_server.py`) and the column names around them.
pub(crate) const MAX_LINE: usize = 16 * 1024 * 1024;
/// Queries admitted at once: one running and the rest waiting for the helper. One more is
/// answered `429`.
pub const MAX_PENDING: usize = 4;

/// The module the helper runs.
pub(crate) const MODULE: &str = "barca._sql_server";

/// Environment variables the helper inherits, by name: what an interpreter needs to start and
/// find `barca`. It gets none of the server's credentials (it reads no store and no file, and
/// what it is not given it cannot leak), and `HOME` is its own empty directory.
pub(crate) const ENV_KEPT: &[&str] = &[
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

/// What a loaded table was loaded for.
pub(crate) struct Loaded {
    pub(crate) db: Stamp,
    pub(crate) dag: Option<Arc<Dag>>,
    pub(crate) at: Instant,
    /// Said with every answer that names the table: rows left out.
    pub(crate) note: Option<String>,
}

impl Loaded {
    pub(crate) fn current(
        &self,
        graph: bool,
        live: bool,
        db: &Stamp,
        dag: Option<&Arc<Dag>>,
    ) -> bool {
        let same_graph = match (&self.dag, dag) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        !live && self.db == *db && same_graph && !(graph && self.at.elapsed() >= STATUS_MAX_AGE)
    }
}

/// The running helper and what it holds.
pub(crate) struct Process {
    // Killed when dropped.
    pub(crate) child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    answers: BufReader<tokio::process::ChildStdout>,
    /// The line being read: kept here so that a read given up at a deadline loses nothing.
    partial: Vec<u8>,
    next_id: u64,
    pub(crate) loaded: HashMap<&'static str, Loaded>,
    /// Server paths that an error message must not carry ([`Process::scrub`]).
    private: Vec<String>,
    /// The helper's working directory and `HOME`: empty, removed when the helper is dropped.
    pub(crate) _home: tempfile::TempDir,
}

/// Why a request to the helper has no answer.
pub(crate) enum Lost {
    /// The helper is gone, or said something that is not the protocol: it is replaced.
    Dead(String),
    /// DuckDB is not installed in the server's environment.
    NoDuckdb(String),
}

impl Lost {
    pub(crate) fn into_api(self) -> ApiError {
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
    pub(crate) async fn start(python: &std::path::Path, tables: &[Shape]) -> Result<Self, Lost> {
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
        barca_service::helper_proc::shield_from_ctrl_c(&mut cmd);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(barca_service::term::child_output())
            .kill_on_drop(true);
        let mut child = barca_service::helper_proc::spawn(&mut cmd)
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
        for table in tables {
            process.load(*table, None).await?;
        }
        Ok(process)
    }

    pub(crate) fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    pub(crate) async fn say(&mut self, message: &Value) -> Result<(), Lost> {
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
    pub(crate) async fn line(&mut self) -> Result<Value, Lost> {
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
    pub(crate) fn scrub(&self, message: &str) -> String {
        let mut out = message.to_string();
        for path in &self.private {
            out = out.replace(path.as_str(), "<server>");
        }
        out
    }

    /// The answer to request `id`.
    pub(crate) async fn answer(&mut self, id: u64) -> Result<Value, Lost> {
        loop {
            let answer = self.line().await?;
            if answer["id"] == id {
                return Ok(answer);
            }
        }
    }

    pub(crate) fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Replace `source`'s table with `rows`, or with a placeholder.
    pub(crate) async fn load(&mut self, table: Shape, rows: Option<&[Value]>) -> Result<(), Lost> {
        let id = self.id();
        self.say(&json!({"op": "load", "id": id, "table": {
            "schema": tables::SCHEMA,
            "name": table.0,
            "columns": table.1,
            "rows": rows,
        }}))
        .await?;
        let answer = tokio::time::timeout(LOAD_LIMIT, self.answer(id))
            .await
            .map_err(|_| {
                Lost::Dead(format!(
                    "it did not load barca.{} within {}s",
                    table.0,
                    LOAD_LIMIT.as_secs()
                ))
            })??;
        if answer["ok"] == true {
            return Ok(());
        }
        Err(Lost::Dead(format!(
            "it could not load barca.{}: {}",
            table.0,
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

/// The server's one query helper (`AppState::sql`).
pub struct QueryHelper {
    /// Held for a whole query: one at a time.
    pub(crate) process: tokio::sync::Mutex<Option<Process>>,
    /// One permit per admitted query ([`MAX_PENDING`]): the one running and those waiting.
    pub(crate) pending: tokio::sync::Semaphore,
    /// [`TIMEOUT`] in milliseconds (shorter in tests).
    pub(crate) timeout_ms: AtomicU64,
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
pub(crate) enum Ended {
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

impl From<barca_service::BarcaError> for Ended {
    fn from(e: barca_service::BarcaError) -> Self {
        Ended::Api(e.into())
    }
}

impl From<Lost> for Ended {
    fn from(lost: Lost) -> Self {
        Ended::Replace(lost.into_api())
    }
}

impl QueryHelper {
    pub(crate) fn timeout(&self) -> Duration {
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

    pub(crate) fn timed_out(&self) -> ApiError {
        ApiError::Timeout(format!(
            "the query did not finish within {}s and was cancelled\n\
             Narrow it (a filter, an aggregate over fewer rows, a smaller join) and send it \
             again.",
            self.timeout().as_secs_f64()
        ))
    }
}
