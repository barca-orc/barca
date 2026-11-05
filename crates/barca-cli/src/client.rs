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
//! This file is the façade: choosing the server and the one client of it. The rest is laid
//! out by layer, one module each:
//!
//! 1. **Choosing the server** ([`choose`], [`configured`]): the flags, `BARCA_SERVER` and
//!    `[server] url`.
//! 2. **Transport** ([`Server`], [`transport`]): one HTTP client, JSON requests, and the errors
//!    every call can end with: a server that cannot be reached or answers unexpectedly is
//!    `infra` (exit 3) naming it; an error the server answers with is its envelope, printed as
//!    a local error. The credentials and request scope are [`transport`]'s.
//! 3. **Checks** ([`Server::health`], [`Server::deployed_files`]): the API version, `--env`, and
//!    the files named on the command line against the deployment.
//! 4. **Runs** ([`runs`]: [`run`]): `POST /runs`, the event stream ([`follow`] rides it through
//!    a server restart, [`sse`] parses its frames), Ctrl-C, the outcome. The ledger of a local
//!    run (`GET /health`, `POST /ledger/lookup`, `POST /ledger/runs`) is [`ledger`].
//! 5. **Inspection** ([`inspect`]: [`list`], [`status`], [`history`], [`run_detail`],
//!    [`stats`], [`queue`], [`sql`]; [`logs`]): the CLI's `--json` bodies, rendered here with
//!    this command's own `--limit`, `--fields` and output mode.
//! 6. **Doctor** ([`doctor`]: [`doctor_checks`]): `barca doctor`'s checks of the server.

use crate::args::ServerFlags;
use crate::error::{CliError, Context, ErrorKind};
use barca_core::BarcaError;
use barca_core::config::{ServerSource, check_server_url, configured_server};
use barca_core::envelope::ErrorEnvelope;
use barca_core::outcome::API_VERSION;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod doctor;
mod follow;
mod inspect;
mod ledger;
mod logs;
mod runs;
mod sse;
#[cfg(test)]
mod tests;
mod transport;

pub(crate) use doctor::doctor_checks;
pub(crate) use inspect::{history, list, queue, run_detail, sql, stats, status};
pub(crate) use logs::logs;
pub(crate) use runs::{RunArgs, run};

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
    /// Safe to print; the original URL is retained for requests and ledger-post identity.
    shown_url: String,
    http: transport::Http,
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
        let token = match std::env::var("BARCA_SERVER_TOKEN") {
            Ok(token) => Some(token),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => {
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    "BARCA_SERVER_TOKEN must be UTF-8",
                ));
            }
        };
        let http = transport::Http::new(&url, token, REQUEST_TIMEOUT)?;
        Ok(Self {
            shown_url: http.shown_url().into(),
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
            barca_core::errln!("[barca] server {} ({})", self.shown_url, source.describe());
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

    /// The server cannot be reached, or the connection broke.
    fn unreachable(&self, e: &reqwest::Error) -> CliError {
        CliError::from_prose(
            ErrorKind::Infra,
            format!(
                "error: cannot reach the barca server at {}: {}\n\
                 Check that `barca serve` is running at that URL and reachable from this \
                 machine. {}",
                self.shown_url,
                self.http.redact(&transport_reason(e)),
                self.instead()
            ),
        )
    }

    /// The server answered, but not with what barca expects.
    fn unexpected(&self, what: &str, detail: &str) -> CliError {
        let detail = self.http.redact(detail);
        CliError::from_prose(
            ErrorKind::Infra,
            format!(
                "error: the barca server at {} answered {what} unexpectedly: {detail}\n\
                 Check that the URL points at `barca serve` (including any proxy prefix) and \
                 that the server runs the same barca version as this machine.",
                self.shown_url
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
        let body = self.http.error_body(body);
        match serde_json::from_slice::<ErrorEnvelope>(&body) {
            Ok(envelope) => envelope,
            Err(_) => {
                let text = String::from_utf8_lossy(&body);
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let text: String = text.chars().take(200).collect();
                self.unexpected(what, &format!("HTTP {status}: {text}"))
            }
        }
    }

    async fn get_text(&self, path: &str) -> Result<String, CliError> {
        let what = format!("GET {path}");
        let (_, body) = self
            .send(self.http.request(reqwest::Method::GET, path)?, &what)
            .await?;
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
            .request(reqwest::Method::POST, path)?
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
                        self.shown_url,
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
                    self.shown_url
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
                        self.shown_url,
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
    let bytes = std::fs::read(root?.join(path)).ok()?;
    Some(barca_core::hash::sha256_hex(&bytes))
}

// ─── 5. inspection ──────────────────────────────────────────────────────────

/// Whether `id` (a node id, `file.py:name` or `file.py:name[key]`) is defined in one of `files`.
fn in_files(id: &str, files: &[String]) -> bool {
    files.is_empty()
        || id
            .split_once(':')
            .is_some_and(|(file, _)| files.iter().any(|f| f == file))
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
