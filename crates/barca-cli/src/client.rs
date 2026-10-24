//! The CLI's client of a barca server (RFC-0007 §3.1, §4.1, §4.2).
//!
//! `--server [<URL>]` runs `get`, `run`, `status`, `list`, `history` or `stats` on a
//! `barca serve` and prints its answer here exactly as the command would print it locally: the
//! same stdout, the same stderr lines and the same exit code, rendered by the same
//! `barca_core::report` functions. `--local` ignores a configured server.
//!
//! Without `--server`, a configured server (`BARCA_SERVER`, else `[server] url`) is still the
//! ledger: `get` and `run` run here and read and post through it ([`Server`] implements
//! `barca_core::ledger::LedgerTransport`), and `status`, `history` and `stats` answer from it.
//! `--local` leaves the configured server untouched.
//!
//! The module is laid out by layer:
//!
//! 1. **Choosing the server** ([`choose`], [`configured`]): the flags, `BARCA_SERVER` and
//!    `[server] url`.
//! 2. **Transport** ([`Server`]): one HTTP client, JSON requests, and the errors every call can
//!    end with: a server that cannot be reached or answers unexpectedly is `infra` (exit 3)
//!    naming it; an error the server answers with is its envelope, printed as a local error.
//! 3. **Checks** ([`Server::health`], [`Server::deployed_files`]): the API version, `--env`, and
//!    the files named on the command line against the deployment.
//! 4. **Runs** ([`run`]): `POST /runs`, the event stream, Ctrl-C, the outcome; and the ledger
//!    of a local run (`GET /health`, `POST /ledger/lookup`, `POST /ledger/runs`).
//! 5. **Inspection** ([`list`], [`status`], [`history`], [`stats`]): the CLI's `--json` bodies,
//!    rendered here with this command's own `--limit`, `--fields` and output mode.
//! 6. **Doctor** ([`doctor_checks`]): `barca doctor`'s checks of the server.

// Every call here ends the command with at most one `CliError` (the envelope), on a path that
// is waiting on the network anyway: its size does not matter.
#![allow(clippy::result_large_err)]

use crate::args::ServerFlags;
use crate::error::{CliError, Context, ErrorKind, list_hint};
use barca_core::BarcaError;
use barca_core::config::{ServerSource, check_server_url, configured_server};
use barca_core::envelope::ErrorEnvelope;
use barca_core::outcome::{API_VERSION, Outcome, RunCommand, RunRequest};
use barca_core::{RunEvent, lines::LineWhen};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one ordinary request (not the event stream) may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one ledger request may take: a post waits for the server to check every object in
/// the store.
const LEDGER_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the event stream may stay silent. The server sends a keep-alive comment every 15s,
/// so a longer silence means the connection is gone.
const STREAM_IDLE: Duration = Duration::from_secs(60);

/// The whole event stream's limit; the server stops a run after 10 minutes.
const STREAM_LIMIT: Duration = Duration::from_secs(24 * 3600);

/// How long a client following a run waits for a server that went away (a restart, a
/// deploy) to come back, by default.
const RECONNECT_DEFAULT: Duration = Duration::from_secs(60);

/// The environment variable that sets it, in seconds (0: give up at once, as before 0.29.4).
pub(crate) const RECONNECT_ENV: &str = "BARCA_SERVER_RECONNECT";

/// How long to wait for a server that went away: [`RECONNECT_ENV`], else 60 seconds. A value
/// that is not a number of seconds is a usage error.
pub(crate) fn reconnect_window() -> Result<Duration, CliError> {
    match std::env::var(RECONNECT_ENV) {
        Err(_) => Ok(RECONNECT_DEFAULT),
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| {
                CliError::from_prose(
                    ErrorKind::Usage,
                    format!(
                        "error: {RECONNECT_ENV}={text} is not a number of seconds\nSet it to how \
                     long a client following a run waits for a restarting server (0: not at \
                     all), or unset it for 60."
                    ),
                )
            }),
    }
}

/// How long a reopened stream must stay up before a later break gets a fresh window.
const STREAM_PROVEN: Duration = Duration::from_secs(10);

/// Between two attempts to reach a server that went away.
const RECONNECT_PAUSE: Duration = Duration::from_millis(500);

// ─── 1. choosing the server ─────────────────────────────────────────────────

/// The commands that take `--server` and `--local`.
pub(crate) const SERVER_COMMANDS: &[&str] = &[
    "get", "run", "status", "list", "history", "stats", "logs", "sql", "doctor", "queue",
];

/// Where `--server` sends this command, or `None` to run it here. Bare `--server` uses the
/// configured server (`BARCA_SERVER`, then `[server] url`) and is a usage error when none is
/// configured. Without `--server`, see [`configured`].
pub(crate) fn choose(flags: &ServerFlags) -> Result<Option<Server>, CliError> {
    let usage = |e: BarcaError| CliError::from_barca(e, &Context::default());
    let Some(given) = flags.server.as_deref() else {
        return Ok(None);
    };
    let (url, source) = if given.is_empty() {
        let cwd = std::env::current_dir()
            .map_err(|e| usage(BarcaError::Other(format!("cannot determine cwd: {e}"))))?;
        match configured_server(&cwd).map_err(usage)? {
            Some((url, source)) => (
                check_server_url(&url, source.describe()).map_err(usage)?,
                Some(source),
            ),
            None => {
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    "error: --server has no URL and no server is configured\n\
                     Pass one (`--server https://barca.example.com`), set BARCA_SERVER, or add \
                     `[server] url = \"...\"` to barca.toml (barca docs server-mode).",
                ));
            }
        }
    } else {
        (check_server_url(given, "--server").map_err(usage)?, None)
    };
    let server = Server::new(url, source, false)?;
    server.announce();
    Ok(Some(server))
}

/// The configured server a command uses without `--server`: `None` with `--local` or when
/// none is configured. `get` and `run` use it as their ledger; `status`, `history` and `stats`
/// answer from it. Nothing is printed or sent here.
pub(crate) fn configured(flags: &ServerFlags) -> Result<Option<Server>, CliError> {
    if flags.local || flags.server.is_some() {
        return Ok(None);
    }
    let usage = |e: BarcaError| CliError::from_barca(e, &Context::default());
    let cwd = std::env::current_dir()
        .map_err(|e| usage(BarcaError::Other(format!("cannot determine cwd: {e}"))))?;
    let Some((url, source)) = configured_server(&cwd).map_err(usage)? else {
        return Ok(None);
    };
    let url = check_server_url(&url, source.describe()).map_err(usage)?;
    Server::new(url, Some(source), true).map(Some)
}

/// Rewrite argv so that `--server` followed by something that is not a URL (a target, a file,
/// another flag) is bare `--server`: `barca get --server total` means the configured server and
/// the target `total`, not a server named `total`. A word with `://` is taken as a URL, so a
/// mistyped scheme is reported as one.
pub(crate) fn bare_server_flags(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        if arg == "--" {
            out.push(arg);
            out.extend(it);
            break;
        }
        if arg == "--server" && !it.peek().is_some_and(|next| next.contains("://")) {
            out.push("--server=".to_string());
            continue;
        }
        out.push(arg);
    }
    out
}

/// `--server` or `--local` on a command that always runs on this machine: a usage error that
/// says so, instead of clap's "unexpected argument".
pub(crate) fn refuse_on_local_commands(args: &[String]) -> Option<CliError> {
    let sub = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    if SERVER_COMMANDS.contains(&sub.as_str()) || sub.ends_with(".py") {
        return None;
    }
    let flag = args
        .iter()
        .take_while(|a| *a != "--")
        .find(|a| *a == "--local" || *a == "--server" || a.starts_with("--server="))?;
    let flag = flag.split('=').next().unwrap_or(flag);
    Some(CliError::from_prose(
        ErrorKind::Usage,
        format!(
            "error: `barca {sub}` always runs on this machine; it does not take {flag}\n\
             --server and --local apply to get, run, status, list, history, stats, logs, sql \
             and doctor (barca docs server-mode)."
        ),
    ))
}

// ─── 2. transport ───────────────────────────────────────────────────────────

/// A barca server and the one HTTP client used to reach it.
pub(crate) struct Server {
    /// The base URL, with any proxy prefix and without a trailing `/`.
    url: String,
    http: reqwest::Client,
    /// Where the URL came from when it was not typed on the command line.
    source: Option<ServerSource>,
    /// Used because it is configured, without `--server`: errors point at `--local`.
    implicit: bool,
}

/// Why a request got no answer, in a few words (the URL is named by the caller).
fn transport_reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "the request timed out".to_string();
    }
    // reqwest's own message names the URL; the innermost cause says what happened.
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    let text = cause.to_string();
    if e.is_connect() {
        format!("could not connect ({text})")
    } else {
        text
    }
}

impl Server {
    fn new(url: String, source: Option<ServerSource>, implicit: bool) -> Result<Self, CliError> {
        let http = barca_core::http::client(REQUEST_TIMEOUT)
            .map_err(|e| CliError::from_prose(ErrorKind::Infra, e))?;
        Ok(Self {
            url,
            http,
            source,
            implicit,
        })
    }

    /// `[barca] server <url> (<source>)`, once per command, for a server the command line did
    /// not name.
    pub(crate) fn announce(&self) {
        if let Some(source) = self.source {
            barca_core::errln!("[barca] server {} ({})", self.url, source.describe());
        }
    }

    /// This server as the ledger of a local `get` / `run`.
    pub(crate) fn into_ledger(self) -> barca_core::ledger::Ledger {
        let source = self.source.map(ServerSource::describe);
        barca_core::ledger::Ledger::with_source(self, source)
    }

    /// The remediation of an error that leaves the server unused.
    fn instead(&self) -> &'static str {
        if self.implicit {
            "To use this machine's own records instead, pass --local."
        } else {
            "To run on this machine instead, pass --local instead of --server."
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    /// The server cannot be reached, or the connection broke.
    fn unreachable(&self, e: &reqwest::Error) -> CliError {
        CliError::from_prose(
            ErrorKind::Infra,
            format!(
                "error: cannot reach the barca server at {}: {}\n\
                 Check that `barca serve` is running at that URL and reachable from this \
                 machine. {}",
                self.url,
                transport_reason(e),
                self.instead()
            ),
        )
    }

    /// The server answered, but not with what barca expects.
    fn unexpected(&self, what: &str, detail: &str) -> CliError {
        CliError::from_prose(
            ErrorKind::Infra,
            format!(
                "error: the barca server at {} answered {what} unexpectedly: {detail}\n\
                 Check that the URL points at `barca serve` (including any proxy prefix) and \
                 that the server runs the same barca version as this machine.",
                self.url
            ),
        )
    }

    /// Send a request and read the whole answer. A non-2xx answer is the server's error
    /// envelope; one that is not an envelope is [`Server::unexpected`].
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<(reqwest::StatusCode, Vec<u8>), CliError> {
        let response = request.send().await.map_err(|e| self.unreachable(&e))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| self.unreachable(&e))?
            .to_vec();
        if status.is_success() {
            return Ok((status, body));
        }
        Err(self.refusal(status, &body, what))
    }

    /// A non-2xx answer: the server's envelope, or an unexpected answer.
    fn refusal(&self, status: reqwest::StatusCode, body: &[u8], what: &str) -> CliError {
        match serde_json::from_slice::<ErrorEnvelope>(body) {
            Ok(envelope) => envelope,
            Err(_) => {
                let text = String::from_utf8_lossy(body);
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let text: String = text.chars().take(200).collect();
                self.unexpected(what, &format!("HTTP {status}: {text}"))
            }
        }
    }

    async fn get_text(&self, path: &str) -> Result<String, CliError> {
        let what = format!("GET {path}");
        let (_, body) = self.send(self.http.get(self.endpoint(path)), &what).await?;
        String::from_utf8(body).map_err(|_| self.unexpected(&what, "the body is not UTF-8"))
    }

    async fn get_json(&self, path: &str) -> Result<Value, CliError> {
        let text = self.get_text(path).await?;
        serde_json::from_str(&text)
            .map_err(|e| self.unexpected(&format!("GET {path}"), &format!("not JSON: {e}")))
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<String, CliError> {
        let what = format!("POST {path}");
        let request = self
            .http
            .post(self.endpoint(path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        let (_, body) = self.send(request, &what).await?;
        String::from_utf8(body).map_err(|_| self.unexpected(&what, "the body is not UTF-8"))
    }

    // ─── 3. checks ──────────────────────────────────────────────────────────

    /// `GET /health`: the server speaks this API version, and `--env`, when given, is the
    /// server's environment.
    pub(crate) async fn health(&self, env: Option<&str>) -> Result<Value, CliError> {
        let health = self.get_json("/health").await?;
        match health["api_version"].as_u64() {
            Some(v) if v == u64::from(API_VERSION) => {}
            Some(v) => {
                return Err(CliError::from_prose(
                    ErrorKind::Infra,
                    format!(
                        "error: the barca server at {} speaks API version {v}; this barca \
                         ({}) speaks version {API_VERSION}\n\
                         Install the same barca version here and on the server.",
                        self.url,
                        env!("CARGO_PKG_VERSION")
                    ),
                ));
            }
            None => {
                let version = health["version"].as_str().unwrap_or("unknown");
                return Err(self.unexpected(
                    "GET /health",
                    &format!("it reports no api_version (barca {version} predates --server)"),
                ));
            }
        }
        if let (Some(env), Some(server_env)) = (env, health["env"].as_str())
            && env != server_env
        {
            return Err(CliError::from_prose(
                ErrorKind::Usage,
                format!(
                    "error: --env {env}: the server at {} runs environment '{server_env}'\n\
                     A server serves one environment. Leave out --env, or pass --env {server_env}.",
                    self.url
                ),
            ));
        }
        Ok(health)
    }

    /// Every deployed file (`GET /project`), root-relative.
    async fn deployed(&self) -> Result<Vec<String>, CliError> {
        let project = self.get_json("/project").await?;
        Ok(project["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["path"].as_str().map(str::to_string))
            .collect())
    }

    /// The files a command named, as the deployment spells them (`GET /project`): a `.py` file
    /// is one of the deployed files, a directory selects the deployed files under it. A file the
    /// deployment does not have is a usage error listing the deployed files. A named file that
    /// differs from the deployed one prints a note; the server runs its own.
    pub(crate) async fn deployed_files(&self, named: &[PathBuf]) -> Result<Vec<String>, CliError> {
        if named.is_empty() {
            return Ok(Vec::new());
        }
        let project = self.get_json("/project").await?;
        let deployed: Vec<(String, String)> = project["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                Some((
                    f["path"].as_str()?.to_string(),
                    f["sha256"].as_str()?.to_string(),
                ))
            })
            .collect();
        let root = project_root();
        let mut chosen: Vec<String> = Vec::new();
        for name in named {
            let rel = relative_to_root(name, root.as_deref());
            let text = rel.to_string_lossy().to_string();
            let matches: Vec<&(String, String)> = if text.ends_with(".py") {
                deployed.iter().filter(|(p, _)| *p == text).collect()
            } else {
                let dir = text.trim_end_matches('/');
                deployed
                    .iter()
                    .filter(|(p, _)| {
                        dir == "." || dir.is_empty() || p.starts_with(&format!("{dir}/"))
                    })
                    .collect()
            };
            if matches.is_empty() {
                let list: Vec<&str> = deployed.iter().map(|(p, _)| p.as_str()).collect();
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    format!(
                        "error: '{}' is not one of the files deployed on the server at {}\n\
                         The server runs its deployed files: {}. Name one of them, or leave out \
                         the files to use all of them.",
                        name.display(),
                        self.url,
                        list.join(", ")
                    ),
                ));
            }
            for (path, sha) in matches {
                if let Some(local) = local_sha256(path, root.as_deref())
                    && &local != sha
                {
                    barca_core::errln!(
                        "[barca] note: {path} differs from the deployment; the server runs the deployed version"
                    );
                }
                if !chosen.contains(path) {
                    chosen.push(path.clone());
                }
            }
        }
        Ok(chosen)
    }
}

/// This machine's project root: the nearest barca.toml at or above the cwd, else the cwd.
fn project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(barca_core::config::find_root(&cwd).unwrap_or(cwd))
}

/// A path typed in the cwd, relative to the project root (as the deployment's paths are).
fn relative_to_root(path: &Path, root: Option<&Path>) -> PathBuf {
    match (std::env::current_dir().ok(), root) {
        (Some(cwd), Some(root)) => barca_core::config::rebase_onto_root(path, &cwd, root),
        _ => path.to_path_buf(),
    }
}

/// The sha256 of a root-relative file on this machine; `None` when it is not here.
fn local_sha256(path: &str, root: Option<&Path>) -> Option<String> {
    use sha2::Digest;
    let bytes = std::fs::read(root?.join(path)).ok()?;
    Some(
        sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

// ─── 4. runs ────────────────────────────────────────────────────────────────

/// A `get` or `run` to start on the server, with how this machine prints it.
pub(crate) struct RunArgs {
    pub(crate) command: RunCommand,
    pub(crate) targets: Vec<String>,
    /// The files as typed (rebased onto the root by [`Server::deployed_files`]).
    pub(crate) files: Vec<PathBuf>,
    pub(crate) refresh: Option<Vec<String>>,
    pub(crate) no_cascade: bool,
    pub(crate) refresh_all: bool,
    pub(crate) dry_run: bool,
    pub(crate) agent: bool,
    /// How the result prints: for `get`, `Json` is `--json`, `Pretty` the summary and paths,
    /// and `Value` the paths alone (`-o path`); for `run`, as its `-o` says.
    pub(crate) mode: barca_core::report::ResultFormat,
    /// `get -o value`: the server reads the values, and they print as locally.
    pub(crate) values: bool,
    /// `get --partition`, as typed.
    pub(crate) partition: Option<String>,
    /// `--batch`, as typed (each `V` or `NODE=V`).
    pub(crate) batch: Vec<String>,
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) env: Option<String>,
}

/// `barca get|run ... --server`: start the run on the server, print its lines as they come,
/// then its outcome, and end as a local run of the same command ends.
pub(crate) async fn run(server: &Server, args: RunArgs, ctx: &Context) -> Result<(), CliError> {
    let window = reconnect_window()?;
    server.health(args.env.as_deref()).await?;
    let files = server.deployed_files(&args.files).await?;
    let request = RunRequest {
        command: args.command,
        targets: args.targets.clone(),
        files,
        refresh: args.refresh.clone().unwrap_or_default(),
        refresh_all: args.refresh_all,
        cascade: !args.no_cascade,
        dry_run: args.dry_run,
        agent: args.agent,
        values: args.values,
        partition: args.partition.clone(),
        batch: args.batch.clone(),
    };
    let body = serde_json::to_value(&request).expect("a run request is JSON");
    let answer = server
        .post_json("/runs", &body)
        .await
        .map_err(|e| with_typed_files(e, ctx))?;
    if args.dry_run {
        return print_dry_run(server, &answer, &args, &request.files).await;
    }
    let run_id = serde_json::from_str::<Value>(&answer)
        .ok()
        .and_then(|v| v["run_id"].as_str().map(str::to_string))
        .ok_or_else(|| server.unexpected("POST /runs", "no run_id in the answer"))?;
    let outcome = stream(server, &run_id, &args, window).await?;
    let (rendered, error) = outcome
        .render(
            args.command,
            &args.targets,
            args.mode,
            args.values,
            args.fields.as_deref(),
        )
        .map_err(|e| server.unexpected("the run's outcome", &e))?;
    crate::commands::emit(rendered);
    match error {
        Some(envelope) => Err(with_typed_files(envelope, ctx)),
        None => Ok(()),
    }
}

/// A server's target error (unknown or ambiguous) ends with the `barca list` pointer naming the
/// files the user typed, as a local `get`/`run` ends it (`get_run_error`): the server names its
/// own files, or none. Other usage errors keep the server's own remediation (#492).
fn with_typed_files(mut error: CliError, ctx: &Context) -> CliError {
    if error.kind != ErrorKind::Usage {
        return error;
    }
    error.remediation = Some(match error.remediation.take() {
        Some(r) if r.starts_with("Run `barca list") => list_hint(&ctx.files),
        Some(r) => r,
        // The envelope's remediation is always a string (`barca docs contract`).
        None => format!("See `barca {} --help`.", ctx.command),
    });
    error
}

/// `--dry-run`: the server answers with the `--dry-run --json` body at once. The stderr lines a
/// local dry run prints before it are rebuilt here: the tasks a `get` without a target skips
/// (from the server's `/list`) and the plan warnings (from the body).
async fn print_dry_run(
    server: &Server,
    answer: &str,
    args: &RunArgs,
    files: &[String],
) -> Result<(), CliError> {
    let result: barca_core::results::ExplainResult = serde_json::from_str(answer)
        .map_err(|e| server.unexpected("POST /runs (dry run)", &e.to_string()))?;
    if args.command == RunCommand::Get && args.targets.is_empty() {
        let list = server.get_json("/list").await?;
        let nodes: Vec<barca_core::report::ListNode> =
            serde_json::from_value(list["nodes"].clone())
                .map_err(|e| server.unexpected("GET /list", &e.to_string()))?;
        let kinds: Vec<(&str, barca_core::NodeKind)> = nodes
            .iter()
            .filter(|n| in_files(&n.id, files))
            .map(|n| (n.id.as_str(), n.kind))
            .collect();
        let file_args = if files.is_empty() {
            server.deployed().await?
        } else {
            files.to_vec()
        };
        if let Some(note) = barca_core::targets::skipped_tasks_note_among(&kinds, &file_args) {
            barca_core::errln!("{note}");
        }
    }
    if let Some(note) = barca_core::selection::explain_note(&result) {
        barca_core::errln!("{note}");
    }
    barca_core::warnings::print(&result.warnings);
    crate::commands::emit(barca_core::report::render_explain(
        &result,
        args.command.as_str(),
        // A `get` dry run prints its plan as `--json` or as text, as locally (`-o path` alone
        // has no plan form of its own).
        match (args.command, args.mode) {
            (RunCommand::Get, barca_core::report::ResultFormat::Value) => {
                barca_core::report::ResultFormat::Pretty
            }
            (_, mode) => mode,
        },
        args.fields.as_deref(),
    ));
    Ok(())
}

/// Stop signals during a streamed run: the first cancels the run on the server and keeps
/// streaming until it ends; the second ends this command at once (exit 130).
fn stop_signals() -> tokio::sync::mpsc::UnboundedReceiver<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
        let Ok(mut stream) = signal(kind) else {
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }
    rx
}

/// The stream broke before the run ended: the run goes on on the server.
fn dropped(server: &Server, run_id: &str, why: &str) -> CliError {
    CliError::from_prose(
        ErrorKind::Infra,
        format!(
            "error: lost the event stream of run {run_id} on the barca server at {}: {why}\n\
             The run continues on the server. See how it ended with `barca history --server {}` \
             (run {run_id}), or GET {}/runs/{run_id}.",
            server.url, server.url, server.url
        ),
    )
}

/// Read `GET /runs/{run_id}/events` to its `run_finished`, printing `line` and `log` events as
/// a local run prints them in this mode. Returns the run's outcome. A stream that breaks is
/// followed again when the server comes back within `window` ([`Follow`]).
async fn stream(
    server: &Server,
    run_id: &str,
    args: &RunArgs,
    window: Duration,
) -> Result<Outcome, CliError> {
    let path = format!("/runs/{run_id}/events");
    let mut signals = stop_signals();
    let mut follow = Follow::new(server, run_id, window).await;
    if !follow.open().await? {
        // Not live: it already ended (a restart in between, or a very short run).
        return follow.recorded_outcome().await;
    }
    let mut cancelled = false;
    loop {
        let item = tokio::select! {
            item = follow.next() => item?,
            Some(()) = signals.recv() => {
                if cancelled {
                    return Err(CliError::from_barca(BarcaError::Cancelled, &Context::default()));
                }
                cancelled = true;
                cancel(server, run_id).await;
                continue;
            }
        };
        let event = match item {
            Followed::Event(event) => event,
            Followed::Ended(body) => return outcome_of(server, &path, &body),
        };
        match event {
            RunEvent::Line { line, when } => {
                if when == LineWhen::Always || args.agent {
                    barca_core::errln!("{line}");
                }
            }
            RunEvent::Log {
                node_id,
                line,
                stream,
                ..
            } => {
                // A step's stderr stays on the server's stderr, as a local run's stays on
                // this machine's; it is recorded with the run either way (`barca logs`).
                if args.mode != barca_core::report::ResultFormat::Json
                    && stream == barca_core::LogStream::Stdout
                {
                    barca_core::errln!("[{node_id}] {line}");
                }
            }
            RunEvent::RunFinished { outcome, .. } => {
                let outcome = outcome.ok_or_else(|| {
                    server.unexpected(&format!("GET {path}"), "run_finished has no outcome")
                })?;
                return Outcome::from_text(outcome.get().to_string())
                    .map_err(|e| server.unexpected(&format!("GET {path}"), &e));
            }
            RunEvent::RunStarted { .. } | RunEvent::StepFinished { .. } => {}
        }
    }
}

/// The outcome in a `GET /runs/{run_id}` body.
fn outcome_of(server: &Server, path: &str, body: &Value) -> Result<Outcome, CliError> {
    match &body["outcome"] {
        Value::Null => {
            Err(server.unexpected(&format!("GET {path}"), "the run ended without an outcome"))
        }
        outcome => Outcome::from_text(outcome.to_string())
            .map_err(|e| server.unexpected(&format!("GET {path}"), &e)),
    }
}

/// What following a run gives next.
enum Followed {
    Event(RunEvent),
    /// The run ended while the stream was down: its `GET /runs/{run_id}` body.
    Ended(Value),
}

/// A run's event stream that rides through a server restart. When the stream breaks (the
/// server closed it without `run_finished`, the connection dropped, nothing came for
/// [`STREAM_IDLE`]) it reconnects for up to the window ([`reconnect_window`]): every
/// [`RECONNECT_PAUSE`] it asks `GET /health` and opens the stream again. The same server
/// (`started_at` unchanged) replays the run from its first event, and the events already read
/// are skipped; a restarted server streams the run as it runs there now (a queued run from its
/// start, a run that was stopped and requeued from the start of its new attempt). A run that
/// ended meanwhile is read from `GET /runs/{run_id}`. A server that does not come back within
/// the window ends the command as before: exit 3, naming the run.
struct Follow<'a> {
    server: &'a Server,
    run_id: String,
    path: String,
    window: Duration,
    response: Option<reqwest::Response>,
    events: SseParser,
    ready: std::collections::VecDeque<RunEvent>,
    /// Events read from the current server.
    seen: usize,
    /// Events still to skip after reconnecting to the same server.
    skip: usize,
    /// The server's `started_at` (`None`: unknown, an older server).
    instance: Option<String>,
    /// Why the stream broke and until when to try again, while it is down.
    lost: Option<(String, tokio::time::Instant)>,
    /// The deadline of the last break and when the stream was opened again: a stream that
    /// breaks again within [`STREAM_PROVEN`] of a reconnect keeps that deadline, so a server
    /// that keeps cutting the stream does not get a fresh window each time.
    unproven: Option<(tokio::time::Instant, tokio::time::Instant)>,
    /// How many times the stream was opened again on a restarted server.
    restarts: usize,
}

impl<'a> Follow<'a> {
    async fn new(server: &'a Server, run_id: &str, window: Duration) -> Self {
        let instance = server.get_json("/health").await.ok().and_then(started_at);
        Self {
            server,
            run_id: run_id.to_string(),
            path: format!("/runs/{}/events", encode(run_id)),
            window,
            response: None,
            events: SseParser::default(),
            ready: Default::default(),
            seen: 0,
            skip: 0,
            instance,
            lost: None,
            unproven: None,
            restarts: 0,
        }
    }

    /// Open the stream: `true` when the run is live. `false` (404) when it is not live: it
    /// ended, or this server never ran it. A server that cannot be reached is ridden through.
    async fn open(&mut self) -> Result<bool, CliError> {
        match self.request().await {
            Ok(Some(response)) => {
                self.response = Some(response);
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(Attempt::Refused(e)) => Err(e),
            Err(Attempt::Unreachable(why)) => {
                self.lose(why)?;
                Ok(true)
            }
        }
    }

    /// `GET /runs/{run_id}/events`: the stream, `None` for a 404, or why not.
    async fn request(&self) -> Result<Option<reqwest::Response>, Attempt> {
        let response = self
            .server
            .http
            .get(self.server.endpoint(&self.path))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .timeout(STREAM_LIMIT)
            .send()
            .await
            .map_err(|e| Attempt::Unreachable(transport_reason(&e)))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status.is_success() {
            return Ok(Some(response));
        }
        // A proxy in front of a server that is restarting answers 502/503/504.
        if status.is_server_error() {
            return Err(Attempt::Unreachable(format!("HTTP {status}")));
        }
        let body = response.bytes().await.unwrap_or_default();
        Err(Attempt::Refused(self.server.refusal(
            status,
            &body,
            &format!("GET {}", self.path),
        )))
    }

    /// The stream broke for `why`: start waiting for the server, or give up when the window
    /// is zero.
    fn lose(&mut self, why: String) -> Result<(), CliError> {
        self.response = None;
        if self.window.is_zero() {
            return Err(dropped(self.server, &self.run_id, &why));
        }
        if let Some((deadline, opened)) = self.unproven.take()
            && opened.elapsed() < STREAM_PROVEN
        {
            self.lost = Some((why, deadline));
            return Ok(());
        }
        barca_core::errln!(
            "[barca] lost the event stream of run {} ({why}); waiting up to {}s for the server \
             to come back",
            self.run_id,
            self.window.as_secs()
        );
        self.lost = Some((why, tokio::time::Instant::now() + self.window));
        Ok(())
    }

    /// The next event, riding through a broken stream.
    async fn next(&mut self) -> Result<Followed, CliError> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(Followed::Event(event));
            }
            if self.lost.is_some() {
                if let Some(ended) = self.reconnect().await? {
                    return Ok(ended);
                }
                continue;
            }
            let Some(response) = self.response.as_mut() else {
                self.lose("the stream was not open".into())?;
                continue;
            };
            let bytes = match tokio::time::timeout(STREAM_IDLE, response.chunk()).await {
                Err(_) => {
                    self.lose(format!("nothing received for {}s", STREAM_IDLE.as_secs()))?;
                    continue;
                }
                Ok(Err(e)) => {
                    self.lose(transport_reason(&e))?;
                    continue;
                }
                Ok(Ok(None)) => {
                    self.lose("the server closed the stream".into())?;
                    continue;
                }
                Ok(Ok(Some(bytes))) => bytes,
            };
            for data in self.events.push(&bytes) {
                let event = RunEvent::parse(&data).map_err(|e| {
                    self.server
                        .unexpected(&format!("GET {}", self.path), &e.to_string())
                })?;
                self.seen += 1;
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                self.ready.push_back(event);
            }
        }
    }

    /// Try, until the deadline, to follow the run again. `Some` when it ended meanwhile.
    async fn reconnect(&mut self) -> Result<Option<Followed>, CliError> {
        let Some((why, deadline)) = self.lost.clone() else {
            return Ok(None);
        };
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(dropped(
                    self.server,
                    &self.run_id,
                    &format!(
                        "{why}; the server did not come back within {}s ({RECONNECT_ENV})",
                        self.window.as_secs()
                    ),
                ));
            }
            tokio::time::sleep(RECONNECT_PAUSE).await;
            let Ok(health) = self.server.get_json("/health").await else {
                continue;
            };
            let instance = started_at(health);
            match self.request().await {
                Err(Attempt::Unreachable(_)) => continue,
                Err(Attempt::Refused(e)) => return Err(e),
                Ok(None) => {
                    let body = self.recorded().await?;
                    if matches!(body["status"].as_str(), Some("pending" | "running")) {
                        // Live in the history but not streamed yet: ask again.
                        continue;
                    }
                    self.lost = None;
                    if !(instance.is_some() && instance == self.instance) {
                        self.restarts += 1;
                    }
                    barca_core::errln!(
                        "[barca] the server is back; run {} ended meanwhile ({})",
                        self.run_id,
                        body["status"].as_str().unwrap_or("unknown")
                    );
                    return Ok(Some(Followed::Ended(body)));
                }
                Ok(Some(response)) => {
                    let same = instance.is_some() && instance == self.instance;
                    self.skip = if same { self.seen } else { 0 };
                    self.seen = 0;
                    self.events = SseParser::default();
                    self.response = Some(response);
                    self.lost = None;
                    self.unproven = Some((deadline, tokio::time::Instant::now()));
                    if same {
                        barca_core::errln!("[barca] reconnected; following run {}", self.run_id);
                    } else {
                        self.restarts += 1;
                        barca_core::errln!(
                            "[barca] the server restarted; following run {} there (a run \
                             that was stopped is shown again from its new start)",
                            self.run_id
                        );
                    }
                    self.instance = instance;
                    return Ok(None);
                }
            }
        }
    }

    /// `GET /runs/{run_id}`.
    async fn recorded(&self) -> Result<Value, CliError> {
        self.server
            .get_json(&format!("/runs/{}", encode(&self.run_id)))
            .await
    }

    /// The outcome of a run that is not live, from `GET /runs/{run_id}`.
    async fn recorded_outcome(&self) -> Result<Outcome, CliError> {
        let body = self.recorded().await?;
        outcome_of(self.server, &format!("/runs/{}", self.run_id), &body)
    }
}

/// Why an attempt to open a run's stream failed.
enum Attempt {
    /// No answer, or one from a proxy whose server is away: try again.
    Unreachable(String),
    /// The server's answer: stop.
    Refused(CliError),
}

/// `started_at` of a `GET /health` body.
fn started_at(health: Value) -> Option<String> {
    health["started_at"].as_str().map(str::to_string)
}

/// `DELETE /runs/{run_id}`. A failure is reported, not fatal: the stream still says how the
/// run ended.
async fn cancel(server: &Server, run_id: &str) {
    let path = format!("/runs/{run_id}");
    let request = server.http.delete(server.endpoint(&path));
    if let Err(e) = server.send(request, &format!("DELETE {path}")).await {
        barca_core::errln!(
            "[barca] warning: could not cancel run {run_id} on the server: {}",
            e.error
        );
    }
}

/// Server-sent events, split from the bytes as they arrive: each complete event's `data`.
#[derive(Default)]
struct SseParser {
    buffer: String,
    pending: Vec<u8>,
}

impl SseParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        // A chunk may end inside a UTF-8 sequence: keep the incomplete tail for the next one.
        self.pending.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(text) => text.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&self.pending[..valid]).replace("\r\n", "\n");
        self.pending.drain(..valid);
        self.buffer.push_str(&text);
        let mut out = Vec::new();
        while let Some(end) = self.buffer.find("\n\n") {
            let block: String = self.buffer.drain(..end + 2).collect();
            let data: Vec<&str> = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            if !data.is_empty() {
                out.push(data.join("\n"));
            }
        }
        out
    }
}

// ─── the ledger of a local run ──────────────────────────────────────────────

/// The ledger requests of a local `get` / `run`: the JSON body of a 2xx, or why there is none.
/// What the answers mean is `barca_core::ledger`'s; this is only the transport.
impl Server {
    async fn ledger_call<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        route: &str,
    ) -> Result<T, barca_core::ledger::LedgerError> {
        use barca_core::ledger::LedgerError;
        let response = request
            .timeout(LEDGER_TIMEOUT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| LedgerError::Unreachable(transport_reason(&e)))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| LedgerError::Unreachable(transport_reason(&e)))?;
        if status.is_success() {
            return serde_json::from_slice(&body).map_err(|e| {
                LedgerError::Unreachable(format!(
                    "{route} answered {status} with an unexpected body: {e}"
                ))
            });
        }
        match serde_json::from_slice::<ErrorEnvelope>(&body) {
            Ok(envelope) if status == reqwest::StatusCode::CONFLICT => {
                Err(LedgerError::Rejected(Box::new(envelope)))
            }
            Ok(envelope) => Err(LedgerError::Refused {
                status: status.as_u16(),
                envelope: Box::new(envelope),
            }),
            Err(_) => {
                let text = String::from_utf8_lossy(&body);
                let text: String = text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                Err(LedgerError::Unreachable(format!(
                    "{route} answered {status}: {text}"
                )))
            }
        }
    }

    fn ledger_post<'a, B: serde::Serialize + Sync, T: serde::de::DeserializeOwned + 'a>(
        &'a self,
        path: &'static str,
        body: &'a B,
    ) -> barca_core::ledger::BoxFuture<'a, Result<T, barca_core::ledger::LedgerError>> {
        Box::pin(async move {
            let body = serde_json::to_vec(body)
                .map_err(|e| barca_core::ledger::LedgerError::Unreachable(e.to_string()))?;
            let request = self
                .http
                .post(self.endpoint(path))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
            self.ledger_call(request, &format!("POST {path}")).await
        })
    }
}

impl barca_core::ledger::LedgerTransport for Server {
    fn url(&self) -> &str {
        &self.url
    }

    fn health(
        &self,
    ) -> barca_core::ledger::BoxFuture<
        '_,
        Result<barca_core::ledger::ServerHealth, barca_core::ledger::LedgerError>,
    > {
        Box::pin(self.ledger_call(self.http.get(self.endpoint("/health")), "GET /health"))
    }

    fn lookup<'a>(
        &'a self,
        request: &'a barca_core::ledger::LookupRequest,
    ) -> barca_core::ledger::BoxFuture<
        'a,
        Result<barca_core::ledger::LookupResponse, barca_core::ledger::LedgerError>,
    > {
        self.ledger_post("/ledger/lookup", request)
    }

    fn post_run<'a>(
        &'a self,
        post: &'a barca_core::ledger::LedgerPost,
    ) -> barca_core::ledger::BoxFuture<
        'a,
        Result<barca_core::ledger::PostAccepted, barca_core::ledger::LedgerError>,
    > {
        self.ledger_post("/ledger/runs", post)
    }
}

// ─── 5. inspection ──────────────────────────────────────────────────────────

/// Whether `id` (a node id, `file.py:name` or `file.py:name[key]`) is defined in one of `files`.
fn in_files(id: &str, files: &[String]) -> bool {
    files.is_empty()
        || id
            .split_once(':')
            .is_some_and(|(file, _)| files.iter().any(|f| f == file))
}

/// `barca list --server`: `GET /list` (or `/groups`), with this command's files, limit, fields
/// and output mode.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn list(
    server: &Server,
    files: &[PathBuf],
    groups: bool,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
) -> Result<(), CliError> {
    server.health(None).await?;
    let files = server.deployed_files(files).await?;
    if groups {
        let body = server.get_json("/groups").await?;
        let groups: Vec<barca_core::groups::NodeGroup> = serde_json::from_value(body)
            .map_err(|e| server.unexpected("GET /groups", &e.to_string()))?;
        crate::commands::print_groups(&groups, json);
        return Ok(());
    }
    let body = server.get_json("/list").await?;
    let bad = |e: String| server.unexpected("GET /list", &e);
    let nodes: Vec<barca_core::report::ListNode> =
        serde_json::from_value(body["nodes"].clone()).map_err(|e| bad(e.to_string()))?;
    let mut assets = Vec::new();
    let mut next_fires = std::collections::HashMap::new();
    for node in nodes.into_iter().filter(|n| in_files(&n.id, &files)) {
        let freshness: barca_core::Freshness = match &node.schedule {
            Some(cron) => serde_json::from_value(json!({"type": "Schedule", "value": cron})),
            None => serde_json::from_value(json!({"type": match node.freshness {
                barca_core::report::ListFreshness::Manual => "Manual",
                _ => "Always",
            }})),
        }
        .map_err(|e| bad(e.to_string()))?;
        if let Some(next) = node.next_fire {
            next_fires.insert(node.id.clone(), next);
        }
        assets.push(barca_core::results::AssetSummary {
            id: node.id,
            kind: node.kind,
            freshness,
            inputs: node.inputs,
            env: node.env,
        });
    }
    let total = assets.len();
    assets.truncate(limit.unwrap_or(total));
    crate::commands::emit(barca_core::report::render_list(
        &assets,
        total,
        &next_fires,
        json,
        fields,
        body["root"].as_str(),
    ));
    Ok(())
}

/// `barca status --server`: `GET /nodes`, with this command's files, limit, fields and mode.
pub(crate) async fn status(
    server: &Server,
    targets: &[String],
    files: &[PathBuf],
    opts: crate::commands::StatusOpts<'_>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let files = server.deployed_files(files).await?;
    let mut path = format!("/nodes?sample={}", opts.sample);
    if !targets.is_empty() {
        path.push_str(&format!("&targets={}", encode(&targets.join(","))));
    }
    let body = server.get_json(&path).await?;
    let mut result: barca_core::status::StatusResult = serde_json::from_value(body.clone())
        .map_err(|e| server.unexpected("GET /nodes", &e.to_string()))?;
    if !files.is_empty() {
        result.nodes.retain(|n| in_files(&n.id, &files));
        result.summary = barca_core::status::StatusSummary::of(&result.nodes);
    }
    if server.implicit {
        server.annotate_here(&mut result, env).await?;
    }
    // Filtered here, after this machine's own states are known (`not_in_ledger`); the summary
    // keeps counting every node.
    opts.filter.apply(&mut result);
    crate::commands::emit(barca_core::report::render_status(
        result,
        opts.json,
        opts.limit,
        opts.fields,
        body["root"].as_str(),
    ));
    Ok(())
}

impl Server {
    /// `barca status` with a configured server: mark each node with whether this machine has a
    /// copy of its result (`local`), and show a result whose ledger post is pending as
    /// `unsynced` (`not_in_ledger`).
    async fn annotate_here(
        &self,
        result: &mut barca_core::status::StatusResult,
        env: Option<&str>,
    ) -> Result<(), CliError> {
        let here = |e: BarcaError| CliError::from_barca(e, &Context::default());
        let cfg = barca_core::config::resolve(env).map_err(here)?;
        let layout = barca_core::transfer::ArtifactLayout::new(
            cfg.local_artifact_dir.clone(),
            &cfg.artifact_root,
        );
        let pending = pending_posts(&cfg, &self.url).await.map_err(here)?;
        for node in &mut result.nodes {
            // The server's `store_only` (RFC-0009) is about the server's own disk: its result is
            // in the ledger and its object in the store, which is `cached` for this machine,
            // whether or not a copy is here (`local` says).
            if node.cache.state == "unsynced" && node.cache.reason == "store_only" {
                node.cache.state = "cached".to_string();
                node.cache.reason = "materialized".to_string();
                node.cache.detail =
                    "a successful materialization matches this code and these inputs".to_string();
            }
            node.local = Some(has_copy(&layout, node.cache.artifact.as_deref()));
            barca_core::status::not_in_ledger(node, &pending);
        }
        result.summary = barca_core::status::StatusSummary::of(&result.nodes);
        Ok(())
    }
}

/// Whether this machine holds the artifact the server recorded at `artifact`: its mirror of
/// that store path exists under this machine's artifact directory. A path outside this
/// project's store (a server without one) is never here.
fn has_copy(layout: &barca_core::transfer::ArtifactLayout, artifact: Option<&str>) -> bool {
    artifact
        .and_then(|a| layout.local_for(a))
        .is_some_and(|p| p.is_file())
}

/// `barca history --server`: `GET /history`, with the filters as query parameters.
pub(crate) async fn history(
    server: &Server,
    limit: Option<usize>,
    filter: &barca_core::history_filter::HistoryFilter,
    json: bool,
    fields: Option<&[String]>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let mut query = vec![match limit {
        Some(n) => format!("limit={n}"),
        None => "all=1".to_string(),
    }];
    for (key, value) in filter.query_pairs() {
        query.push(format!("{key}={}", encode(&value)));
    }
    let path = format!("/history?{}", query.join("&"));
    let body = server.get_json(&path).await?;
    let bad = |e: serde_json::Error| server.unexpected("GET /history", &e.to_string());
    let runs: Vec<barca_core::db::RunRecord> =
        serde_json::from_value(body["runs"].clone()).map_err(bad)?;
    let total = body["total"].as_u64().map_or(runs.len(), |t| t as usize);
    crate::commands::emit(barca_core::report::render_history(
        &runs, total, json, fields,
    ));
    Ok(())
}

/// `barca history RUN_ID` against a server: `GET /runs/{run_id}`, the run and the steps it
/// recorded. A run the server does not know but this machine does gets the command that
/// reads it here.
pub(crate) async fn run_detail(
    server: &Server,
    run_id: &str,
    limit: Option<usize>,
    json: bool,
    select: &str,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let page = match limit {
        Some(n) => format!("?limit={n}"),
        None => "?all=true".to_string(),
    };
    let path = format!("/runs/{}{page}", encode(run_id));
    let body = match server.get_json(&path).await {
        Ok(body) => body,
        Err(e) => {
            let here = format!("barca history {run_id} --local");
            return Err(here_instead(e, run_id, env, &here).await);
        }
    };
    let bad = |e: serde_json::Error| server.unexpected(&format!("GET {path}"), &e.to_string());
    let run: barca_core::db::RunRecord = serde_json::from_value(body.clone()).map_err(bad)?;
    let mut steps: Vec<barca_core::db::RunStep> =
        serde_json::from_value(body["steps"].clone()).map_err(bad)?;
    // The steps the run recorded, as on this machine: a finished run the server started also
    // lists the cached steps it streamed, until the server restarts.
    let listed = steps.len();
    if !matches!(run.status.as_str(), "pending" | "running") {
        steps.retain(|step| step.status != "cached");
    }
    // A server older than 0.30.2 lists every step and sends no page. The cached steps left
    // out above are not counted either.
    let steps_page = match body.get("steps_page") {
        Some(page) => {
            let mut page: barca_core::db::StepsPage =
                serde_json::from_value(page.clone()).map_err(bad)?;
            page.total -= listed - steps.len();
            page
        }
        None => barca_core::db::StepsPage {
            total: steps.len(),
            truncated: false,
        },
    };
    let detail = barca_core::queries::RunDetail {
        run,
        steps,
        steps_page,
    };
    crate::commands::emit(barca_core::report::render_run_detail(&detail, json, select));
    Ok(())
}

/// `barca logs` against a server: the run's recorded lines from `GET /runs/{run_id}/logs`, and
/// with `--follow` a running run's lines from `GET /runs/{run_id}/events` as they arrive.
/// Without a run id, the newest run in the server's history (with `--follow`, the newest
/// running one).
pub(crate) async fn logs(
    server: &Server,
    run_id: Option<&str>,
    env: Option<&str>,
    args: crate::commands::logs::LogsArgs<'_>,
) -> Result<(), CliError> {
    use crate::commands::logs::{follow_ended, follow_exit_code, name_run};
    let window = reconnect_window()?;
    server.health(env).await?;
    let run_id = match run_id {
        Some(id) => id.to_string(),
        None => {
            let limit = barca_core::queries::RUNNING_LOOKBACK;
            let body = server.get_json(&format!("/history?limit={limit}")).await?;
            let runs: Vec<barca_core::db::RunRecord> = serde_json::from_value(body["runs"].clone())
                .map_err(|e| server.unexpected("GET /history", &e.to_string()))?;
            let Some(run) = barca_core::queries::pick_run(runs, args.follow) else {
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    format!(
                        "error: no run is recorded on the barca server at {} yet
                         Start one (`barca run --server ...`), or read this machine's runs \
                         with --local.",
                        server.url
                    ),
                ));
            };
            name_run(&run, args.follow);
            // A configured server answers, but a newer run of this machine's is said once.
            if server.implicit
                && let Some(here) = local_run(env, None).await
                && here.run_id != run.run_id
                && started_key(&here.started_at) > started_key(&run.started_at)
            {
                barca_core::errln!(
                    "[barca] note: this machine has a newer run {} ({} {}, {}); barca logs \
                     --local reads it",
                    here.run_id,
                    here.command,
                    barca_core::report::history_target(&here),
                    here.status
                );
            }
            run.run_id
        }
    };
    let engine = |e: BarcaError| CliError::from_barca(e, &Context::default());
    if args.follow {
        let mut follow = Follow::new(server, &run_id, window).await;
        // Not live (404): the run ended, or it is not one the server started; its recorded
        // lines answer.
        if follow.open().await? {
            let status = follow_events(&mut follow, args).await?;
            follow_ended(&run_id, &status);
            barca_core::term::exit(follow_exit_code(&status));
        }
    }
    let (status, lines) = match recorded_logs(server, &run_id, args).await {
        Ok(found) => found,
        Err(e) => {
            let here = format!("barca logs {run_id} --local");
            return Err(here_instead(e, &run_id, env, &here).await);
        }
    };
    args.print_all(&lines).map_err(engine)?;
    if args.follow {
        follow_ended(&run_id, &status);
        barca_core::term::exit(follow_exit_code(&status));
    }
    Ok(())
}

/// This machine's record of `run_id` (with `None`, its newest run), read from a private copy
/// of its metadata DB; `None` when there is none or it cannot be read. Used only for hints.
async fn local_run(env: Option<&str>, run_id: Option<&str>) -> Option<barca_core::db::RunRecord> {
    let cfg = barca_core::config::resolve(env).ok()?;
    let snapshot = barca_core::db::DbSnapshot::take(&cfg.db_path)
        .await
        .ok()??;
    match run_id {
        Some(id) => barca_core::db::get_run(snapshot.path(), id).await.ok()?,
        None => barca_core::db::get_recent_runs(snapshot.path(), 1)
            .await
            .ok()?
            .into_iter()
            .next(),
    }
}

/// A stored start time as a sortable key (`datetime('now')` or RFC 3339, both UTC).
fn started_key(ts: &str) -> String {
    let ts = ts.trim();
    ts.strip_suffix('Z').unwrap_or(ts).replacen('T', " ", 1)
}

/// A server that does not know `run_id`, when this machine does: the same error, with the
/// command that reads it here (`here`) as its remediation.
async fn here_instead(e: CliError, run_id: &str, env: Option<&str>, here: &str) -> CliError {
    if e.kind != ErrorKind::Usage
        || !e.error.contains("not found")
        || local_run(env, Some(run_id)).await.is_none()
    {
        return e;
    }
    CliError::from_prose(
        ErrorKind::Usage,
        format!("error: {}\nthis run is on this machine: {here}", e.error),
    )
}

/// `GET /runs/{run_id}/logs`: the run's status and its recorded lines, filtered as asked.
async fn recorded_logs(
    server: &Server,
    run_id: &str,
    args: crate::commands::logs::LogsArgs<'_>,
) -> Result<(String, Vec<barca_core::db::LogEntry>), CliError> {
    let mut query = Vec::new();
    if let Some(step) = args.step {
        query.push(format!("step={}", encode(step)));
    }
    if let Some(stream) = args.stream {
        query.push(format!("stream={}", stream.as_str()));
    }
    let mut path = format!("/runs/{}/logs", encode(run_id));
    if !query.is_empty() {
        path = format!("{path}?{}", query.join("&"));
    }
    let body = server.get_json(&path).await?;
    let bad = |e: String| server.unexpected(&format!("GET {path}"), &e);
    let status = body["status"]
        .as_str()
        .ok_or_else(|| bad("no status".into()))?
        .to_string();
    let lines = serde_json::from_value(body["logs"].clone()).map_err(|e| bad(e.to_string()))?;
    Ok((status, lines))
}

/// Read a live run's event stream to its end, printing the lines `args` selects as they arrive,
/// through a server restart ([`Follow`]). Returns the run's final status.
async fn follow_events(
    follow: &mut Follow<'_>,
    args: crate::commands::logs::LogsArgs<'_>,
) -> Result<String, CliError> {
    let mut seq = 0usize;
    // Lines printed of the run as the current server runs it.
    let mut printed = 0usize;
    let mut restarts = 0usize;
    loop {
        let item = follow.next().await?;
        if follow.restarts != restarts {
            restarts = follow.restarts;
            printed = 0;
        }
        match item {
            Followed::Ended(body) => {
                // Ended while the stream was down: the lines it printed since are recorded
                // (on a restarted server, every line is of the attempt that ended there).
                let status = body["status"].as_str().unwrap_or("failed").to_string();
                let (_, lines) = recorded_logs(follow.server, &follow.run_id, args).await?;
                let missed: Vec<_> = lines.into_iter().skip(printed).collect();
                args.print_all(&missed)
                    .map_err(|e| CliError::from_barca(e, &Context::default()))?;
                return Ok(status);
            }
            Followed::Event(RunEvent::Log {
                node_id,
                line,
                stream,
                ts,
            }) => {
                let entry = barca_core::db::LogEntry {
                    node_id,
                    seq: seq as i64,
                    line,
                    stream,
                    ts,
                };
                seq += 1;
                if args.selects(&entry.node_id, entry.stream) {
                    args.print_one(&entry);
                    printed += 1;
                }
            }
            Followed::Event(RunEvent::RunFinished { ok, outcome, .. }) => {
                let status = outcome
                    .and_then(|raw| Outcome::from_text(raw.get().to_string()).ok())
                    .map(|o| o.status().as_str().to_string())
                    .unwrap_or_else(|| if ok { "success" } else { "failed" }.to_string());
                return Ok(status);
            }
            Followed::Event(
                RunEvent::RunStarted { .. } | RunEvent::Line { .. } | RunEvent::StepFinished { .. },
            ) => {}
        }
    }
}

/// `barca queue [pause|resume]`: `GET /queue`, `POST /queue/pause` or `POST /queue/resume`, and
/// the queue as JSON or a table.
pub(crate) async fn queue(
    server: &Server,
    action: Option<crate::args::QueueAction>,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    use crate::args::QueueAction;
    server.health(env).await?;
    let (path, text) = match action {
        None => ("/queue", server.get_text("/queue").await?),
        Some(QueueAction::Pause) => (
            "/queue/pause",
            server.post_json("/queue/pause", &json!({})).await?,
        ),
        Some(QueueAction::Resume) => (
            "/queue/resume",
            server.post_json("/queue/resume", &json!({})).await?,
        ),
    };
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| server.unexpected(&format!("GET {path}"), &format!("not JSON: {e}")))?;
    match action {
        Some(QueueAction::Pause) => barca_core::errln!(
            "[barca] run queue paused on {}: queued runs wait until `barca queue resume`",
            server.url
        ),
        Some(QueueAction::Resume) => {
            barca_core::errln!("[barca] run queue resumed on {}", server.url)
        }
        None => {}
    }
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string_pretty(&body).expect("a JSON value prints")
        );
    } else {
        barca_core::term::stdout_str(&render_queue(&body));
    }
    Ok(())
}

/// The queue as a table: a summary line, then the running and the queued runs.
fn render_queue(body: &Value) -> String {
    let list = |key: &str| body[key].as_array().cloned().unwrap_or_default();
    let (running, queued) = (list("running"), list("queued"));
    let mut state = Vec::new();
    if body["draining"].as_bool() == Some(true) {
        state.push("draining (the server is stopping; the queue resumes at its next start)".into());
    }
    if body["paused"].as_bool() == Some(true) {
        state.push(format!(
            "paused since {}",
            body["paused_at"].as_str().unwrap_or("?")
        ));
    }
    if state.is_empty() {
        state.push("open".to_string());
    }
    let mut out = format!(
        "queue: {} · {} of {} running · {} queued\n",
        state.join(", "),
        running.len(),
        body["max_runs"].as_u64().unwrap_or(0),
        queued.len()
    );
    if running.is_empty() && queued.is_empty() {
        return out;
    }
    out.push_str(&format!(
        "\n{:<4} {:<14} {:<8} {:<7} {:<28} {:<9} {}\n",
        "POS", "RUN_ID", "STATUS", "COMMAND", "TARGET", "TRIGGER", "QUEUED_AT"
    ));
    for run in running.iter().chain(queued.iter()) {
        let target = run["target"].as_str().unwrap_or("(all)");
        out.push_str(&format!(
            "{:<4} {:<14} {:<8} {:<7} {:<28} {:<9} {}\n",
            run["position"]
                .as_u64()
                .map_or("-".to_string(), |p| p.to_string()),
            run["run_id"].as_str().unwrap_or(""),
            run["status"].as_str().unwrap_or(""),
            run["command"].as_str().unwrap_or(""),
            target,
            run["triggered_by"].as_str().unwrap_or(""),
            run["queued_at"].as_str().unwrap_or(""),
        ));
    }
    out
}

/// `barca stats --server`: `GET /stats/{name}`.
pub(crate) async fn stats(
    server: &Server,
    target: &str,
    files: &[PathBuf],
    json: bool,
    fields: Option<&[String]>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    server.deployed_files(files).await?;
    let mut body = server
        .get_json(&format!("/stats/{}", encode(target)))
        .await?;
    // The body is `barca stats --json`, which names the node `id`.
    if let Some(obj) = body.as_object_mut()
        && let Some(id) = obj.remove("id")
    {
        obj.insert("node_id".into(), id);
    }
    let stats: barca_core::db::AssetStats = serde_json::from_value(body)
        .map_err(|e| server.unexpected("GET /stats", &e.to_string()))?;
    crate::commands::emit(barca_core::report::render_stats(&stats, json, fields));
    Ok(())
}

/// `barca sql` with a server: the views resolved from the server's ledger (`GET /sql/views`),
/// barca's tables from its records (`GET /history`, `GET /materializations`, `GET /nodes`), and
/// the query run on this machine (the server runs no SQL). The artifacts are read from the
/// store this project shares with the server, as a local `barca sql` reads them.
pub(crate) async fn sql(
    server: &Server,
    query: &str,
    files: &[PathBuf],
    limit: Option<usize>,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    use barca_core::sql::tables;
    let here = |e: BarcaError| CliError::from_barca(e, &Context::default());
    let health = server.health(env).await?;
    let cfg = barca_core::config::resolve(env).map_err(here)?;
    let instead = if server.implicit {
        "pass --local to query this machine's own results"
    } else {
        "leave out --server to query this machine's own results"
    };
    barca_core::ledger::same_store(&cfg, &server.url, health["store_uri"].as_str(), instead)
        .map_err(here)?;
    let files = server.deployed_files(files).await?;
    let mut path = "/sql/views".to_string();
    if !files.is_empty() {
        path.push_str(&format!("?files={}", encode(&files.join(","))));
    }
    let body = server.get_json(&path).await?;
    let catalog: barca_core::sql::Catalog = serde_json::from_value(body)
        .map_err(|e| server.unexpected("GET /sql/views", &e.to_string()))?;

    let pending = if server.implicit {
        pending_posts(&cfg, &server.url).await.map_err(here)?
    } else {
        Vec::new()
    };
    let cap = tables::SERVER_ROWS;
    let mut runs = tables::runs(None);
    if tables::named(query, tables::RUNS) {
        let route = format!("/history?limit={cap}");
        let body = server.get_json(&route).await?;
        let rows: Vec<barca_core::db::RunRecord> = serde_json::from_value(body["runs"].clone())
            .map_err(|e| server.unexpected("GET /history", &e.to_string()))?;
        let total = body["total"].as_u64().map_or(rows.len(), |t| t as usize);
        runs = tables::runs(Some(&rows));
        runs.note = tables::truncated_note(tables::RUNS, rows.len(), total, &server.url);
    }
    let mut steps = tables::materializations(None);
    if tables::named(query, tables::MATERIALIZATIONS) {
        let route = format!("/materializations?limit={cap}");
        let body = server.get_json(&route).await?;
        let rows: Vec<barca_core::db::StepRow> =
            serde_json::from_value(body["materializations"].clone())
                .map_err(|e| server.unexpected("GET /materializations", &e.to_string()))?;
        let total = body["total"].as_u64().map_or(rows.len(), |t| t as usize);
        steps = tables::materializations(Some(&rows));
        steps.note =
            tables::truncated_note(tables::MATERIALIZATIONS, rows.len(), total, &server.url);
    }
    let mut nodes = tables::nodes(None);
    if tables::named(query, tables::NODES) {
        let body = server.get_json("/nodes?sample=0").await?;
        let mut result: barca_core::status::StatusResult = serde_json::from_value(body)
            .map_err(|e| server.unexpected("GET /nodes", &e.to_string()))?;
        if !files.is_empty() {
            result.nodes.retain(|n| in_files(&n.id, &files));
        }
        if server.implicit {
            server.annotate_here(&mut result, env).await?;
        }
        nodes = tables::nodes(Some(&result.nodes));
    }

    let python = barca_core::commands::find_python();
    let origin = barca_core::sql::Origin::Server {
        url: server.url.clone(),
        pending,
    };
    let result = barca_core::sql::run(
        &cfg,
        query,
        limit,
        catalog,
        vec![runs, steps, nodes],
        Vec::new(),
        &python,
        &origin,
    )
    .await
    .map_err(here)?;
    crate::commands::emit(barca_core::report::render_sql(result, json));
    Ok(())
}

/// This machine's results whose post to the server at `url` is pending (`unsynced`).
async fn pending_posts(
    cfg: &barca_core::config::ResolvedConfig,
    url: &str,
) -> Result<Vec<barca_core::status::PendingPost>, BarcaError> {
    if !Path::new(&cfg.db_path).exists() {
        return Ok(Vec::new());
    }
    Ok(barca_core::ledger::posts::unsynced(&cfg.db_path, url)
        .await?
        .into_iter()
        .flat_map(|post| post.steps)
        .map(|step| barca_core::status::PendingPost {
            node_id: step.node_id,
            run_hash: step.run_hash,
            error: None,
        })
        .collect())
}

/// Percent-encode a query value or path segment (node ids may hold `/`, `:` and `[`).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' | b':' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ─── 6. doctor ──────────────────────────────────────────────────────────────

/// `barca doctor` with a server (`--server`, or a configured one without `--local`): one check
/// per thing that has to be right for this machine to use it. A failed check's detail ends with
/// a line saying what to do. Nothing is changed on the server or here.
pub(crate) async fn doctor_checks(
    server: &Server,
    report: &mut barca_core::diagnostics::DoctorReport,
    env: Option<&str>,
) {
    let source = server.source.map(ServerSource::describe);
    let named = match source {
        Some(source) => format!("{} ({source})", server.url),
        None => server.url.clone(),
    };
    let cfg = barca_core::config::resolve(env).ok();
    let health = match server.get_json("/health").await {
        Ok(health) => health,
        Err(e) => {
            let fix = e.remediation.clone().unwrap_or_default();
            let fix = if server.implicit {
                format!("{fix} To check this machine only, pass --local.")
            } else {
                fix
            };
            report.add(
                "server",
                false,
                format!("{}\n{}", e.error.trim_start_matches("error: "), fix.trim()),
                json!({"url": server.url, "source": source, "reachable": false}),
            );
            unsynced_posts(server, report, cfg.as_ref()).await;
            return;
        }
    };
    report.add(
        "server",
        true,
        format!("{named} answers"),
        json!({"url": server.url, "source": source, "reachable": true}),
    );

    // Versions: the API version must match; another barca version with the same API works.
    let version = health["version"].as_str().unwrap_or("unknown");
    let api = health["api_version"].as_u64();
    let mine = env!("CARGO_PKG_VERSION");
    let api_ok = api == Some(u64::from(API_VERSION));
    let detail = if !api_ok {
        format!(
            "the server runs barca {version} (API version {}), this client barca {mine} (API \
             version {API_VERSION}): this client cannot use it\nInstall the same barca version \
             here and on the server.",
            api.map_or("none".to_string(), |v| v.to_string())
        )
    } else if version != mine {
        format!(
            "barca {version} on the server, {mine} here, both API version {API_VERSION}; the same \
             version on both prints the same output"
        )
    } else {
        format!("barca {version}, API version {API_VERSION}, the same as this client")
    };
    report.add(
        "server_version",
        api_ok,
        detail,
        json!({"version": version, "api_version": api, "client_version": mine,
               "client_api_version": API_VERSION}),
    );

    // The store: a result one side records must be readable by the other.
    let theirs = health["store_uri"].as_str();
    let ours = cfg
        .as_ref()
        .filter(|c| c.remote_artifacts())
        .map(|c| c.artifact_root.clone());
    let shown = |uri: Option<&str>| {
        uri.map_or("no store".to_string(), barca_core::transfer::diagnostic_uri)
    };
    let store_ok = match (theirs, ours.as_deref()) {
        (Some(t), Some(o)) => {
            barca_core::ledger::normalize_uri(t) == barca_core::ledger::normalize_uri(o)
        }
        _ => false,
    };
    let detail = match (theirs, ours.as_deref()) {
        _ if store_ok => format!("both write to {}", shown(theirs)),
        (None, _) => format!(
            "the server has no artifact store, so it cannot share results with this machine\n\
             Give the server's barca.toml a [remote] section (the same uri as this project's: \
             {}) and restart barca serve.",
            shown(ours.as_deref())
        ),
        (Some(t), None) => format!(
            "the server writes artifacts to {}, this project has no [remote] store\nAdd a \
             [remote] section to barca.toml with the uri the server's barca.toml has (barca docs \
             remote).",
            shown(Some(t))
        ),
        (Some(t), Some(o)) => format!(
            "the server writes artifacts to {}, this project to {}: a result one of them \
             records could not be read by the other\nSet [remote] uri in barca.toml to the uri \
             the server's barca.toml has, and use the server's environment.",
            shown(Some(t)),
            shown(Some(o))
        ),
    };
    report.add(
        "server_store",
        store_ok,
        detail,
        json!({"server": theirs.map(barca_core::transfer::diagnostic_uri),
               "project": ours.as_deref().map(barca_core::transfer::diagnostic_uri),
               "server_env": health["env"], "project_env": cfg.as_ref().map(|c| c.env.clone())}),
    );

    // Draining, and a metadata DB the server cannot use.
    let draining = health["status"].as_str() == Some("draining");
    let metadata_error = health["metadata_error"].as_str();
    let (ok, detail) = match (draining, metadata_error) {
        (true, _) => (
            false,
            "the server is shutting down (draining): it finishes its runs, starts none and \
             refuses posts\nStart barca serve again (or wait for the deploy that stopped it)."
                .to_string(),
        ),
        (false, Some(error)) => (
            false,
            format!(
                "the server cannot use its metadata DB: {}\nOn the server, put a backup back \
                 with `barca state restore` or start over with `barca state reset`.",
                error.lines().next().unwrap_or(error)
            ),
        ),
        (false, None) => (
            true,
            format!(
                "serving{}",
                if health["read_only"].as_bool() == Some(true) {
                    " read-only (no runs, no scheduler)"
                } else {
                    ""
                }
            ),
        ),
    };
    report.add(
        "server_health",
        ok,
        detail,
        json!({"status": health["status"], "read_only": health["read_only"],
               "metadata_error": health["metadata_error"]}),
    );

    // Files the server did not load: their nodes are missing from everything it answers.
    let errors: Vec<&Value> = health["load_errors"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let detail = if errors.is_empty() {
        "every deployed file loaded".to_string()
    } else {
        let files: Vec<String> = errors
            .iter()
            .map(|e| {
                format!(
                    "{}: {}",
                    e["file"].as_str().unwrap_or("?"),
                    e["error"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                )
            })
            .collect();
        format!(
            "the server did not load {}, so its nodes are missing there: {}\nFix the file \
             and deploy it again: a `barca serve --watch` reloads it, otherwise restart barca \
             serve.",
            barca_core::report::plural(errors.len() as u64, "file"),
            files.join("; "),
        )
    };
    report.add(
        "server_sources",
        errors.is_empty(),
        detail,
        json!({"load_errors": health["load_errors"]}),
    );

    // The scheduler, and the schedules that need attention.
    scheduler(server, report, &health).await;
    unsynced_posts(server, report, cfg.as_ref()).await;
}

/// The `server_scheduler` check: running or not, and how many schedules are paused or failing
/// (`GET /schedule`, the numbers the web UI shows).
async fn scheduler(
    server: &Server,
    report: &mut barca_core::diagnostics::DoctorReport,
    health: &Value,
) {
    let wanted = health["scheduler"].as_bool().unwrap_or(false);
    let running = health["scheduler_enabled"].as_bool().unwrap_or(false);
    let jobs = match server.get_json("/schedule").await {
        Ok(Value::Array(jobs)) => jobs,
        Ok(_) | Err(_) if !running => Vec::new(),
        Ok(other) => {
            report.add(
                "server_scheduler",
                false,
                format!(
                    "GET /schedule answered {other}\nCheck that the URL points at barca serve."
                ),
                Value::Null,
            );
            return;
        }
        Err(e) => {
            report.add(
                "server_scheduler",
                false,
                format!("{}\n{}", e.error, e.remediation.unwrap_or_default()),
                Value::Null,
            );
            return;
        }
    };
    let ids = |pred: &dyn Fn(&Value) -> bool| -> Vec<String> {
        jobs.iter()
            .filter(|j| pred(j))
            .filter_map(|j| j["id"].as_str().map(str::to_string))
            .collect()
    };
    let paused = ids(&|j| j["paused"].as_bool() == Some(true));
    let failing = ids(&|j| j["failing"].as_bool() == Some(true));
    let data = json!({"running": running, "schedules": jobs.len(), "paused": paused,
                      "failing": failing});
    if wanted && !running {
        report.add(
            "server_scheduler",
            false,
            "the scheduler stopped on an error: no schedule fires\nRead the server's stderr \
             (and metadata_error in GET /health), fix the cause and restart barca serve.",
            data,
        );
        return;
    }
    if !wanted {
        report.add(
            "server_scheduler",
            true,
            "not running on this server (--no-schedule or --read-only): schedules do not fire \
             there",
            data,
        );
        return;
    }
    let mut detail = format!(
        "running; {}",
        barca_core::report::plural(jobs.len() as u64, "schedule")
    );
    if !paused.is_empty() {
        detail.push_str(&format!(
            ", {} paused ({})",
            paused.len(),
            paused.join(", ")
        ));
    }
    if failing.is_empty() {
        report.add("server_scheduler", true, detail, data);
        return;
    }
    let first = failing[0].as_str();
    let job = jobs.iter().find(|j| j["id"].as_str() == Some(first));
    let count = |key: &str| job.and_then(|j| j[key].as_u64()).unwrap_or(0);
    let command = match job.and_then(|j| j["kind"].as_str()) {
        Some("task") => "run",
        _ => "get",
    };
    detail.push_str(&format!(
        ", {} failing ({}): {first} failed {} ticks in a row (max_failures={}) and its ticks are \
         skipped\nRun `barca status --problems` for the error and fix it, then `barca {command} \
         {first} --server`: a success resets the count and the ticks resume.",
        failing.len(),
        failing.join(", "),
        count("failures"),
        count("max_failures"),
    ));
    report.add("server_scheduler", false, detail, data);
}

/// The `server_posts` check: runs this machine computed against the server's ledger whose post
/// did not arrive (`ledger_posts`, state `unsynced`), read from a snapshot of the metadata DB.
async fn unsynced_posts(
    server: &Server,
    report: &mut barca_core::diagnostics::DoctorReport,
    cfg: Option<&barca_core::config::ResolvedConfig>,
) {
    let Some(cfg) = cfg else {
        return;
    };
    let posts = match barca_core::db::DbSnapshot::take(&cfg.db_path).await {
        Ok(Some(snapshot)) => barca_core::ledger::posts::all(snapshot.path())
            .await
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let pending: Vec<_> = posts
        .into_iter()
        .filter(|p| p.state == barca_core::ledger::posts::UNSYNCED && p.server == server.url)
        .collect();
    let runs: Vec<&str> = pending.iter().map(|p| p.client_run_id.as_str()).collect();
    let data = json!({"unsynced": runs.len(), "runs": runs,
                      "last_error": pending.last().and_then(|p| p.error.clone())});
    if pending.is_empty() {
        report.add(
            "server_posts",
            true,
            "every run this machine computed against the server is recorded there",
            data,
        );
        return;
    }
    let why = pending
        .last()
        .and_then(|p| p.error.as_deref())
        .map(|e| format!(" (last: {})", e.lines().next().unwrap_or(e)))
        .unwrap_or_default();
    report.add(
        "server_posts",
        false,
        format!(
            "{} computed here {} not recorded on the server yet: {}{why}\nThe next `barca get` or \
             `barca run` against the server sends {} first; `barca status --state unsynced` lists \
             the results.",
            barca_core::report::plural(runs.len() as u64, "run"),
            if runs.len() == 1 { "is" } else { "are" },
            runs.join(", "),
            if runs.len() == 1 { "it" } else { "them" },
        ),
        data,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn bare_server_before_a_positional_is_the_configured_server() {
        assert_eq!(
            bare_server_flags(argv("barca get --server total p.py")),
            argv("barca get --server= total p.py")
        );
        assert_eq!(
            bare_server_flags(argv("barca get total --server")),
            argv("barca get total --server=")
        );
        assert_eq!(
            bare_server_flags(argv("barca get total --server https://h/x --json")),
            argv("barca get total --server https://h/x --json")
        );
        assert_eq!(
            bare_server_flags(argv("barca get total --server --json")),
            argv("barca get total --server= --json")
        );
        assert_eq!(
            bare_server_flags(argv("barca get total --server ftp://x")),
            argv("barca get total --server ftp://x")
        );
        assert_eq!(
            bare_server_flags(argv("barca get -- --server")),
            argv("barca get -- --server")
        );
    }

    #[test]
    fn local_commands_refuse_server_and_local() {
        for line in [
            "barca serve p.py --server",
            "barca plan --server=http://h",
            "barca plan p.py --local",
            "barca docs --server",
            "barca version --server",
        ] {
            let err = refuse_on_local_commands(&argv(line)).expect(line);
            assert_eq!(err.kind, ErrorKind::Usage, "{line}");
            assert!(err.error.contains("always runs on this machine"), "{line}");
        }
        for line in [
            "barca get x --server",
            "barca status --local",
            "barca sql q --local",
            "barca sql q --server",
            "barca p.py --server",
            "barca serve p.py",
        ] {
            assert!(refuse_on_local_commands(&argv(line)).is_none(), "{line}");
        }
    }

    #[test]
    fn events_are_split_across_chunks_and_comments_are_skipped() {
        let mut sse = SseParser::default();
        assert!(
            sse.push(b": keep-alive\n\nid: 0\ndata: {\"type\":")
                .is_empty()
        );
        let got = sse.push("\"line\",\"line\":\"é\"}\n\nid: 1\r\ndata: {}\r\n\r\n".as_bytes());
        assert_eq!(got, vec![r#"{"type":"line","line":"é"}"#, "{}"]);
        // A multibyte character split between chunks.
        let bytes = "data: \u{e9}\n\n".as_bytes();
        assert!(sse.push(&bytes[..7]).is_empty());
        assert_eq!(sse.push(&bytes[7..]), vec!["\u{e9}"]);
    }

    #[test]
    fn node_ids_are_filtered_by_file_and_encoded() {
        let files = vec!["a.py".to_string()];
        assert!(in_files("a.py:x", &files));
        assert!(in_files("a.py:x[k=1]", &files));
        assert!(!in_files("b.py:x", &files));
        assert!(in_files("b.py:x", &[]));
        assert_eq!(encode("sub/a.py:x y"), "sub%2Fa.py:x%20y");
    }
}
