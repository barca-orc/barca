//! Configuration resolution — barca.toml, environment variables, CLI flags.
//!
//! The project root is the nearest directory at or above the cwd holding barca.toml
//! (`find_root`); without one, the cwd is the root. The CLI changes into the root before
//! anything else runs, so `.barca/`, relative paths in user code, and node ids are anchored
//! there wherever barca is invoked from. barca.toml is then read from the (new) cwd.
//!
//! Precedence for every value: CLI flag > environment variable > barca.toml
//! > built-in default.

use crate::BarcaError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub const CONFIG_FILE: &str = "barca.toml";
pub const DEFAULT_ENV: &str = "default";

// ─── Raw file shape ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BarcaToml {
    pub default_env: Option<String>,
    /// Every command that reads or writes an environment must name it with `--env` or
    /// `BARCA_ENV` (#627); `BARCA_REQUIRE_ENV` overrides it.
    pub require_env: Option<bool>,
    pub remote: Option<RemoteToml>,
    pub discovery: Option<DiscoveryToml>,
    pub cache: Option<CacheToml>,
    pub server: Option<ServerToml>,
    pub execution: Option<ExecutionToml>,
}

/// `[execution]`: how much runs at once (#436).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct ExecutionToml {
    /// The most the `weight=` of the steps running at once may add up to. The pool size when
    /// not set.
    pub capacity: Option<toml::Value>,
}

/// A capacity that was set (`BARCA_CAPACITY` > `[execution] capacity`), and where.
pub type CapacitySetting = Option<(usize, &'static str)>;

/// `BARCA_CAPACITY` > `[execution] capacity` > not set (the pool size applies). Anything but a
/// whole number of at least 1 is a usage error naming where it came from.
pub fn capacity_setting(file: Option<&ExecutionToml>) -> Result<CapacitySetting, BarcaError> {
    let invalid = |source: &str, got: &str| {
        BarcaError::Usage(format!(
            "{source}: expected a whole number of at least 1, got {got}"
        ))
    };
    if let Some(raw) = env_var("BARCA_CAPACITY") {
        return match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Some((n, "BARCA_CAPACITY"))),
            _ => Err(invalid("BARCA_CAPACITY", &format!("'{raw}'"))),
        };
    }
    match file.and_then(|e| e.capacity.as_ref()) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n >= 1 => Ok(Some((*n as usize, "[execution] capacity"))),
        Some(other) => Err(invalid("[execution] capacity", &other.to_string())),
    }
}

/// The capacity setting of the project at `cwd` (its barca.toml and the environment), for
/// commands that resolve nothing else (`barca plan`).
pub fn capacity_in(cwd: &Path) -> Result<CapacitySetting, BarcaError> {
    let file = load_toml(cwd)?.unwrap_or_default();
    capacity_setting(file.execution.as_ref())
}

/// `[cache]`: how much disk `.barca/` may use (RFC-0012, see `gc`).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct CacheToml {
    /// `"20GB"`, `"500MB"`, `"20GiB"`, a number of bytes, `"10%"` of the filesystem, or `"off"`.
    pub max_size: Option<toml::Value>,
}

/// `[server]`: the `barca serve` this project's commands talk to (RFC-0007), and how
/// `barca serve` behaves.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct ServerToml {
    /// The server's base URL, `http://` or `https://`, with any proxy prefix.
    pub url: Option<String>,
    /// Seconds a stopping server waits for in-flight runs before it cancels them
    /// (`--drain-timeout` overrides it). 0 cancels them at once.
    pub drain_timeout: Option<u64>,
    /// How many runs `barca serve` executes at once (`--max-runs` overrides it); the others
    /// wait in its run queue. At least 1; default one per core.
    pub max_runs: Option<usize>,
}

/// How long `barca serve` waits for in-flight runs on SIGTERM or SIGINT by default: the
/// grace period Kubernetes gives a pod.
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 30;

/// `[discovery]`: which files a walk of the project finds (see `discover`).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryToml {
    /// Root-relative globs; when set, only matching files are discovered.
    pub include: Option<Vec<String>>,
    /// Root-relative globs removed from what the walk finds.
    pub exclude: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RemoteToml {
    pub uri: Option<String>,
    pub artifacts_uri: Option<String>,
    /// Removed in 0.26.0 with the shared history (RFC-0009); read only to refuse it by name.
    pub state_uri: Option<toml::Value>,
    /// Removed in 0.26.0 with `optimistic` mode (RFC-0009); read only to refuse it by name.
    pub state: Option<toml::Value>,
    /// Removed in 0.26.0 with the conditional push (RFC-0009); read only to refuse it by name.
    pub push_retries: Option<toml::Value>,
    /// Concurrent artifact uploads/downloads by the transfer helper.
    pub transfer_concurrency: Option<usize>,
    /// Seconds one transfer attempt may run before it is failed as stalled.
    pub transfer_timeout: Option<u64>,
    /// Per-fsspec-protocol option tables, e.g. `[remote.storage_options.abfs]`.
    pub storage_options: Option<toml::Table>,
}

// ─── Resolved configuration ──────────────────────────────────────────────────

/// The release that removed the shared history: `optimistic` mode, `[remote] state`,
/// `BARCA_STATE` and the settings that went with them (RFC-0009).
pub const SHARED_HISTORY_REMOVED_IN: &str = "0.26.0";

/// How many metadata backups are kept, in the store and in the local backup directory each.
pub const BACKUPS_KEPT: usize = 5;

/// Where the environment name came from (`--env` > `BARCA_ENV` > `default_env` > built-in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    Flag,
    EnvVar,
    DefaultEnv,
    BuiltIn,
}

impl EnvSource {
    fn describe(self) -> &'static str {
        match self {
            EnvSource::Flag => "--env",
            EnvSource::EnvVar => "BARCA_ENV",
            EnvSource::DefaultEnv => "default_env in barca.toml",
            EnvSource::BuiltIn => "built-in default",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// Explicit process-wide remote-off override; historical remote results stay
    /// recorded but cannot be read by this process.
    pub remote_off: bool,
    /// Validated environment name (path/URI segment).
    pub env: String,
    /// Which setting chose [`Self::env`].
    pub env_source: EnvSource,
    /// Local metadata DB path for this env. Only `barca state restore` ever replaces it.
    pub db_path: String,
    /// Artifact store root: the local artifact dir, or a remote URI / shared
    /// directory. Recorded in the metadata DB; see [`Self::remote_artifacts`].
    pub artifact_root: String,
    /// Where workers write and read artifacts for this env. Always local —
    /// with a separate store it doubles as the local cache of that store.
    pub local_artifact_dir: String,
    /// Concurrent transfers to/from a separate artifact store.
    pub transfer_concurrency: usize,
    /// Per-attempt transfer limit, from when the attempt starts.
    pub transfer_timeout_secs: u64,
    /// Where this environment's metadata backups go in the store (`{uri}/{env}/state/backups`),
    /// when a remote store is configured with `[remote] uri` / `BARCA_REMOTE_URI` (RFC-0009).
    /// Backups are taken only when this is set.
    pub backup_uri: Option<String>,
    /// Merged storage options (toml ⊕ env, env keys win), serialized as the
    /// JSON that `BARCA_STORAGE_OPTIONS` carries to child processes.
    pub storage_options_json: Option<String>,
    /// How much disk `.barca/` may use: `BARCA_CACHE_MAX_SIZE` > `[cache] max_size` > 10% of
    /// the filesystem.
    pub cache_max_size: crate::gc::MaxSize,
    /// Where [`Self::cache_max_size`] came from, for messages.
    pub cache_max_size_source: &'static str,
    /// `[server] drain_timeout`: seconds `barca serve` waits for in-flight runs once told to
    /// stop, before it cancels them. Only `serve` reads it.
    pub drain_timeout_secs: u64,
    /// `[server] max_runs`: how many runs `barca serve` executes at once (`None`: one per
    /// core). Only `serve` reads it.
    pub server_max_runs: Option<usize>,
    /// The configured server (`BARCA_SERVER`, else `[server] url`): its ledger is what `get` and
    /// `run` use unless `--local` (RFC-0007 section 4.1). `None` when none is configured.
    pub server_url: Option<String>,
    /// `BARCA_CAPACITY` > `[execution] capacity`, when set: the most the `weight=` of the steps
    /// running at once may add up to. `None` leaves it at the pool size (#436).
    pub capacity: CapacitySetting,
    /// The ledger this command runs against: attached by the CLI's `get` and `run` from the
    /// configured server unless `--local`, never by `barca serve`, which is the ledger.
    pub ledger: Option<crate::ledger::Ledger>,
    /// Check the artifact store at the start of a run even when it was checked in the last 5
    /// minutes (#593): set by `get -o url`, which signs objects the store must hold.
    pub check_store: bool,
}

impl ResolvedConfig {
    /// This environment's local backup directory: `backups/` beside the metadata DB
    /// (`.barca/backups`, or `.barca/envs/<env>/backups` for a named environment).
    pub fn local_backup_dir(&self) -> PathBuf {
        Path::new(&self.db_path)
            .parent()
            .map_or_else(|| PathBuf::from("backups"), |dir| dir.join("backups"))
    }

    pub(crate) fn allows_artifact(&self, path: &str) -> bool {
        !self.remote_off || local_artifact(path, &self.local_artifact_dir)
    }

    /// True when the artifact store is separate from the local artifact dir,
    /// so artifacts are transferred in the background (upload after a step
    /// completes, download before a cached artifact is consumed).
    pub fn remote_artifacts(&self) -> bool {
        fn norm(p: &str) -> &str {
            p.trim_start_matches("./").trim_end_matches('/')
        }
        norm(&self.artifact_root) != norm(&self.local_artifact_dir)
    }

    /// For `get` and `run` on a machine with a server configured (with or without `--local`):
    /// take no metadata backups. The server is the ledger and backs up its own database; a
    /// client's copies would land in the server's backup set, where they rotate the server's
    /// own out and are candidates for a restore of the server (DIRECTION.md, "Store and
    /// backup": the store is a backup only for a machine without a server). `barca serve` does
    /// not call this, whatever its `[server] url`.
    pub fn as_server_client(&mut self) {
        if self.server_url.is_some() {
            self.backup_uri = None;
        }
    }

    /// The line `get`, `run` and `serve` print on stderr at startup, when the environment
    /// comes from `default_env` in barca.toml or a separate artifact store is configured:
    /// either can point a one-off command at another environment's data without a flag
    /// on the command line saying so (#433). `None` otherwise.
    pub fn startup_line(&self) -> Option<String> {
        if self.env_source != EnvSource::DefaultEnv && !self.remote_artifacts() {
            return None;
        }
        Some(self.env_and_store())
    }

    /// The line `serve` prints on stderr at startup, always: [`Self::startup_line`]'s env and
    /// store, then the metadata DB and its format, e.g. `; metadata .barca/metadata.db (db format
    /// 1)`. `format` is `None` when there is no database yet (`serve` creates none at startup)
    /// and `Some(Err(()))` when it cannot be used.
    pub fn serve_startup_line(&self, format: Option<Result<i64, ()>>) -> String {
        let format = match format {
            Some(Ok(n)) => format!("db format {n}"),
            Some(Err(())) => "unusable".to_string(),
            None => "not created yet".to_string(),
        };
        format!(
            "{}; metadata {} ({format})",
            self.env_and_store(),
            self.db_path
        )
    }

    fn env_and_store(&self) -> String {
        let store = if self.remote_artifacts() {
            crate::transfer::diagnostic_uri(&self.artifact_root)
        } else {
            format!("local ({})", self.local_artifact_dir)
        };
        format!(
            "[barca] env {} ({}); artifact store {store}",
            self.env,
            self.env_source.describe()
        )
    }
}

/// A path in this environment's local artifact directory, without accessing any store.
pub(crate) fn local_artifact(path: &str, artifact_dir: &str) -> bool {
    let Some(path) = crate::transfer::local_path(path) else {
        return false;
    };
    let root = Path::new(artifact_dir);
    if path.is_absolute() {
        let Ok(cwd) = std::env::current_dir() else {
            return false;
        };
        let path = normalize_lexically(path);
        let root = normalize_lexically(&cwd.join(root));
        path.starts_with(&root)
            || std::fs::canonicalize(&root).is_ok_and(|real| path.starts_with(real))
    } else {
        normalize_lexically(path).starts_with(normalize_lexically(root))
    }
}

// ─── Project root ────────────────────────────────────────────────────────────

/// The project root: the nearest directory at or above `start` that holds barca.toml.
/// `None` when there is none, in which case the caller's cwd is the root.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(CONFIG_FILE).is_file())
        .map(Path::to_path_buf)
}

/// Re-express a path typed in `cwd` relative to `root` (an ancestor of `cwd`, or `cwd`
/// itself), so it still names the same file once the process changes into `root`. An absolute
/// path inside the root becomes root-relative too, so node ids do not depend on how a file was
/// spelled; an absolute path outside the root is unchanged.
/// `.` and `..` are resolved lexically: `sub` + `../p.py` is `p.py`, the spelling a user in the
/// root would type, so node ids built from it match.
pub fn rebase_onto_root(path: &Path, cwd: &Path, root: &Path) -> PathBuf {
    if path.is_absolute() {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        return match canon(path).strip_prefix(canon(root)) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            Ok(_) => PathBuf::from("."),
            Err(_) => path.to_path_buf(),
        };
    }
    let rel_cwd = cwd.strip_prefix(root).unwrap_or(Path::new(""));
    normalize_lexically(&rel_cwd.join(path))
}

pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => out.push(c),
            },
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

// ─── Loading ─────────────────────────────────────────────────────────────────

/// Read ./barca.toml if present. Parse errors are hard errors.
pub fn load_toml(cwd: &Path) -> Result<Option<BarcaToml>, BarcaError> {
    let path = cwd.join(CONFIG_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| BarcaError::Other(format!("failed to read {}: {e}", path.display())))?;
    let parsed: BarcaToml = toml::from_str(&text)
        .map_err(|e| BarcaError::Usage(format!("invalid {}: {e}", path.display())))?;
    Ok(Some(parsed))
}

/// Whether `--env` (or `BARCA_ENV`) is required, and the setting that says so:
/// `BARCA_REQUIRE_ENV` (`1`/`true`/`yes` or `0`/`false`/`no`) > `require_env` in barca.toml.
fn require_env(file: Option<bool>) -> Result<Option<&'static str>, BarcaError> {
    if let Some(raw) = env_var("BARCA_REQUIRE_ENV") {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(Some("BARCA_REQUIRE_ENV")),
            "0" | "false" | "no" => Ok(None),
            _ => Err(BarcaError::Usage(format!(
                "invalid BARCA_REQUIRE_ENV '{raw}' (expected 1 or 0)"
            ))),
        };
    }
    Ok((file == Some(true)).then_some("require_env = true in barca.toml"))
}

/// The environments this project has data for under `cwd`: `default` when `.barca/metadata.db`
/// exists, and every directory of `.barca/envs/`, sorted.
pub fn known_envs(cwd: &Path) -> Vec<String> {
    let barca = cwd.join(".barca");
    let mut out: Vec<String> = std::fs::read_dir(barca.join("envs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| valid_env_name(name))
        .collect();
    if barca.join("metadata.db").is_file() && !out.iter().any(|e| e == DEFAULT_ENV) {
        out.push(DEFAULT_ENV.to_string());
    }
    out.sort();
    out
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn join_uri(base: &str, segment: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), segment)
}

/// Resolve the effective configuration. `cli_env` is the `--env` flag.
/// Does not create any directories — see `db::ensure_env_dirs`.
pub fn resolve(cli_env: Option<&str>) -> Result<ResolvedConfig, BarcaError> {
    let cwd = std::env::current_dir()
        .map_err(|e| BarcaError::Other(format!("cannot determine cwd: {e}")))?;
    resolve_in(cli_env, &cwd)
}

/// Testable variant with an explicit cwd for barca.toml discovery.
pub fn resolve_in(cli_env: Option<&str>, cwd: &Path) -> Result<ResolvedConfig, BarcaError> {
    let file = load_toml(cwd)?.unwrap_or_default();
    let remote = file.remote.unwrap_or_default();
    let drain_timeout_secs = file
        .server
        .as_ref()
        .and_then(|server| server.drain_timeout)
        .unwrap_or(DEFAULT_DRAIN_TIMEOUT_SECS);
    let server_max_runs = file.server.as_ref().and_then(|server| server.max_runs);
    if server_max_runs == Some(0) {
        return Err(BarcaError::Usage(
            "[server] max_runs must be at least 1 (the runs barca serve executes at once)"
                .to_string(),
        ));
    }

    // env name: CLI > BARCA_ENV > default_env in toml > "default"
    let (env, env_source) = match (cli_env, env_var("BARCA_ENV"), file.default_env) {
        (Some(flag), _, _) => (flag.to_string(), EnvSource::Flag),
        (None, Some(var), _) => (var, EnvSource::EnvVar),
        (None, None, Some(toml)) => (toml, EnvSource::DefaultEnv),
        (None, None, None) => (DEFAULT_ENV.to_string(), EnvSource::BuiltIn),
    };
    if !valid_env_name(&env) {
        return Err(BarcaError::Usage(format!(
            "invalid environment name '{env}' — allowed characters: A-Z a-z 0-9 . _ -"
        )));
    }
    let chosen = matches!(env_source, EnvSource::Flag | EnvSource::EnvVar);
    if !chosen {
        let known = known_envs(cwd);
        let named = |known: &[String]| {
            if known.is_empty() {
                "none yet".to_string()
            } else {
                known.join(", ")
            }
        };
        if let Some(source) = require_env(file.require_env)? {
            return Err(BarcaError::Usage(format!(
                "no environment given, and {source} requires one: pass --env NAME or set \
                 BARCA_ENV (environments here: {})",
                named(&known)
            )));
        }
        if known.len() > 1 {
            static NOTED: std::sync::Once = std::sync::Once::new();
            NOTED.call_once(|| {
                crate::errln!(
                    "[barca] note: no --env given, so this uses env {env} ({}); this project has \
                     {} environments ({}). Pass --env NAME or set BARCA_ENV; require_env = true \
                     in barca.toml (or BARCA_REQUIRE_ENV=1) makes it required",
                    env_source.describe(),
                    known.len(),
                    named(&known)
                );
            });
        }
    }

    let (cache_max_size, cache_max_size_source) = cache_max_size(file.cache.as_ref())?;
    let capacity = capacity_setting(file.execution.as_ref())?;
    removed_settings(&remote)?;

    match env_var("BARCA_REMOTE").as_deref() {
        Some("off") => {
            let local = crate::db::env_local_paths(&env);
            return Ok(ResolvedConfig {
                remote_off: true,
                env,
                env_source,
                db_path: local.db_path,
                artifact_root: local.artifact_dir.clone(),
                local_artifact_dir: local.artifact_dir,
                transfer_concurrency: 4,
                transfer_timeout_secs: 600,
                backup_uri: None,
                storage_options_json: None,
                cache_max_size,
                cache_max_size_source,
                drain_timeout_secs,
                server_max_runs,
                server_url: env_var("BARCA_SERVER").or_else(|| {
                    file.server
                        .as_ref()
                        .and_then(|s| s.url.clone())
                        .filter(|u| !u.is_empty())
                }),
                capacity,
                ledger: None,
                check_store: false,
            });
        }
        Some(other) => {
            return Err(BarcaError::Usage(format!(
                "invalid BARCA_REMOTE '{other}' (expected \"off\" or an unset/empty value)"
            )));
        }
        None => {}
    }

    // remote root: BARCA_REMOTE_URI > [remote].uri
    let remote_uri = env_var("BARCA_REMOTE_URI").or(remote.uri);

    // Local layout (default env keeps the legacy paths — zero migration).
    let local = crate::db::env_local_paths(&env);

    // artifacts: BARCA_ARTIFACT_URI (0.4.0 back-compat, literal) >
    //            [remote].artifacts_uri (literal) > {uri}/{env}/artifacts > local
    let artifact_env_override = env_var("BARCA_ARTIFACT_URI");
    if artifact_env_override.is_some() && env != DEFAULT_ENV {
        crate::errln!(
            "[barca] warning: BARCA_ARTIFACT_URI is set — it is used literally and \
             bypasses the '{env}' environment prefix for artifacts"
        );
    }
    let artifact_root = artifact_env_override
        .or(remote.artifacts_uri)
        .or_else(|| {
            remote_uri
                .as_deref()
                .map(|u| join_uri(u, &format!("{env}/artifacts")))
        })
        .unwrap_or_else(|| local.artifact_dir.clone());

    // metadata backups: {uri}/{env}/state/backups, only with a remote root (RFC-0009).
    let backup_uri = remote_uri
        .as_deref()
        .map(|u| join_uri(u, &format!("{env}/state/backups")));

    let server_url = env_var("BARCA_SERVER").or_else(|| {
        file.server
            .as_ref()
            .and_then(|s| s.url.clone())
            .filter(|u| !u.is_empty())
    });

    let transfer_concurrency = match env_var("BARCA_TRANSFER_CONCURRENCY") {
        Some(v) => v.parse::<usize>().ok().filter(|&n| n > 0).ok_or_else(|| {
            BarcaError::Other(format!(
                "invalid BARCA_TRANSFER_CONCURRENCY '{v}' (expected a positive integer)"
            ))
        })?,
        None => match remote.transfer_concurrency {
            Some(0) => {
                return Err(BarcaError::Other(
                    "[remote].transfer_concurrency must be at least 1".to_string(),
                ));
            }
            Some(n) => n,
            None => 4,
        },
    };

    let transfer_timeout_secs = match env_var("BARCA_TRANSFER_TIMEOUT") {
        Some(v) => v.parse::<u64>().ok().filter(|&n| n > 0).ok_or_else(|| {
            BarcaError::Other(format!(
                "invalid BARCA_TRANSFER_TIMEOUT '{v}' (expected a positive number of seconds)"
            ))
        })?,
        None => match remote.transfer_timeout {
            Some(0) => {
                return Err(BarcaError::Other(
                    "[remote].transfer_timeout must be at least 1 second".to_string(),
                ));
            }
            Some(n) => n,
            None => 600,
        },
    };

    let storage_options_json = merge_storage_options(remote.storage_options.as_ref())?;

    Ok(ResolvedConfig {
        remote_off: false,
        env,
        env_source,
        db_path: local.db_path,
        artifact_root,
        local_artifact_dir: local.artifact_dir,
        transfer_concurrency,
        transfer_timeout_secs,
        backup_uri,
        storage_options_json,
        cache_max_size,
        cache_max_size_source,
        drain_timeout_secs,
        server_max_runs,
        server_url,
        capacity,
        ledger: None,
        check_store: false,
    })
}

/// `BARCA_CACHE_MAX_SIZE` > `[cache] max_size` > [`crate::gc::size::DEFAULT`], and which one it
/// was. An invalid value is a usage error naming where it came from.
fn cache_max_size(
    file: Option<&CacheToml>,
) -> Result<(crate::gc::MaxSize, &'static str), BarcaError> {
    let parse = |raw: &str, source: &'static str| {
        crate::gc::size::parse(raw)
            .map(|size| (size, source))
            .map_err(|e| BarcaError::Usage(format!("{source}: {e}")))
    };
    if let Some(raw) = env_var("BARCA_CACHE_MAX_SIZE") {
        return parse(&raw, "BARCA_CACHE_MAX_SIZE");
    }
    match file.and_then(|c| c.max_size.as_ref()) {
        None => Ok((crate::gc::size::DEFAULT, "default")),
        Some(toml::Value::String(raw)) => parse(raw, "[cache] max_size"),
        Some(toml::Value::Integer(n)) => parse(&n.to_string(), "[cache] max_size"),
        Some(other) => Err(BarcaError::Usage(format!(
            "[cache] max_size: expected a string such as \"20GB\", \"10%\" or \"off\", or a \
             number of bytes, got {other}"
        ))),
    }
}

/// Refuse the settings of the shared history, which 0.26.0 removed (RFC-0009), by name: a
/// project that still sets one must learn that it no longer shares anything, rather than have
/// it silently ignored.
fn removed_settings(remote: &RemoteToml) -> Result<(), BarcaError> {
    let toml_keys = [
        ("state", remote.state.is_some()),
        ("state_uri", remote.state_uri.is_some()),
        ("push_retries", remote.push_retries.is_some()),
    ];
    let env_vars = ["BARCA_STATE", "BARCA_STATE_URI", "BARCA_PUSH_RETRIES"];
    let what = toml_keys
        .iter()
        .find(|(_, set)| *set)
        .map(|(key, _)| format!("`[remote] {key}` in {CONFIG_FILE}"))
        .or_else(|| {
            env_vars
                .iter()
                .find(|var| env_var(var).is_some())
                .map(|var| format!("the environment variable {var}"))
        });
    match what {
        None => Ok(()),
        Some(what) => Err(BarcaError::Usage(format!(
            "{what} is no longer supported: barca {SHARED_HISTORY_REMOVED_IN} removed the shared \
             history (`optimistic` mode, pull-before-run and the conditional push; RFC-0009).\n\
             Remove it: a remote store is now this machine's backup and read-through cache, with \
             no setting. To share state between machines, use a server (RFC-0007). See `barca \
             docs remote`."
        ))),
    }
}

/// Merge `[remote.storage_options.*]` with `BARCA_STORAGE_OPTIONS` (env keys
/// win per protocol, per key). Returns the merged JSON string, or None when
/// both sources are empty.
fn merge_storage_options(from_toml: Option<&toml::Table>) -> Result<Option<String>, BarcaError> {
    let mut merged: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();

    if let Some(table) = from_toml {
        for (protocol, opts) in table {
            let toml::Value::Table(opts) = opts else {
                return Err(BarcaError::Usage(format!(
                    "[remote.storage_options.{protocol}] must be a table of options"
                )));
            };
            let entry = merged.entry(protocol.clone()).or_default();
            for (k, v) in opts {
                let json = serde_json::to_value(v.clone()).map_err(|e| {
                    BarcaError::Usage(format!("storage_options.{protocol}.{k}: {e}"))
                })?;
                entry.insert(k.clone(), json);
            }
        }
    }

    if let Some(raw) = env_var("BARCA_STORAGE_OPTIONS") {
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            BarcaError::Usage(format!("BARCA_STORAGE_OPTIONS is not valid JSON: {e}"))
        })?;
        let serde_json::Value::Object(by_protocol) = parsed else {
            return Err(BarcaError::Usage(
                "BARCA_STORAGE_OPTIONS must be a JSON object keyed by protocol".to_string(),
            ));
        };
        for (protocol, opts) in by_protocol {
            let serde_json::Value::Object(opts) = opts else {
                return Err(BarcaError::Usage(format!(
                    "BARCA_STORAGE_OPTIONS[{protocol:?}] must be a JSON object"
                )));
            };
            let entry = merged.entry(protocol).or_default();
            for (k, v) in opts {
                entry.insert(k, v); // env wins
            }
        }
    }

    if merged.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&merged).map_err(|e| {
        BarcaError::Other(format!("storage options serialize: {e}"))
    })?))
}

// ─── Server ──────────────────────────────────────────────────────────────────

/// Where a configured server URL came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSource {
    /// `BARCA_SERVER`.
    EnvVar,
    /// `[server] url` in barca.toml.
    Toml,
}

impl ServerSource {
    pub fn describe(self) -> &'static str {
        match self {
            ServerSource::EnvVar => "BARCA_SERVER",
            ServerSource::Toml => "[server] url in barca.toml",
        }
    }
}

/// The configured server: `BARCA_SERVER` (non-empty) > `[server] url` in the barca.toml at the
/// project root above `cwd`. `None` when neither is set. The URL is not checked here; see
/// [`check_server_url`].
pub fn configured_server(cwd: &Path) -> Result<Option<(String, ServerSource)>, BarcaError> {
    if let Some(url) = env_var("BARCA_SERVER") {
        return Ok(Some((url, ServerSource::EnvVar)));
    }
    let root = find_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let url = load_toml(&root)?
        .and_then(|t| t.server)
        .and_then(|s| s.url)
        .filter(|u| !u.is_empty());
    Ok(url.map(|u| (u, ServerSource::Toml)))
}

/// A server URL as barca uses it: `http://` or `https://`, without a trailing `/` (a proxy
/// prefix such as `https://host/barca` is kept). `what` names where it came from, for the error.
pub fn check_server_url(url: &str, what: &str) -> Result<String, BarcaError> {
    let trimmed = url.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"));
    match rest {
        Some(host) if !host.is_empty() && !host.starts_with('/') => Ok(trimmed.to_string()),
        _ => Err(BarcaError::Usage(format!(
            "{what}: '{url}' is not a server URL\nGive the URL of a running `barca serve`, starting with http:// or https://, e.g. https://barca.example.com."
        ))),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_root_walks_up_to_the_nearest_barca_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root(&deep), None);
        std::fs::write(root.join(CONFIG_FILE), "").unwrap();
        assert_eq!(find_root(&deep), Some(root.clone()));
        assert_eq!(find_root(&root), Some(root.clone()));
        std::fs::write(root.join("a").join(CONFIG_FILE), "").unwrap();
        assert_eq!(find_root(&deep), Some(root.join("a")));
    }

    #[test]
    fn find_root_ignores_a_directory_named_barca_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(find_root(tmp.path()), None);
    }

    #[test]
    fn paths_typed_in_a_subdirectory_are_rebased_onto_the_root() {
        let root = Path::new("/p");
        let rebase =
            |path: &str, cwd: &str| rebase_onto_root(Path::new(path), Path::new(cwd), root);
        assert_eq!(rebase("../x.py", "/p/sub"), PathBuf::from("x.py"));
        assert_eq!(rebase("x.py", "/p/sub"), PathBuf::from("sub/x.py"));
        assert_eq!(rebase("./x.py", "/p"), PathBuf::from("x.py"));
        assert_eq!(rebase("../../x.py", "/p/a/b"), PathBuf::from("x.py"));
        assert_eq!(rebase("../../x.py", "/p/a"), PathBuf::from("../x.py"));
        assert_eq!(rebase("/abs/x.py", "/p/sub"), PathBuf::from("/abs/x.py"));
        assert_eq!(rebase("/p/q/x.py", "/p/sub"), PathBuf::from("q/x.py"));
        assert_eq!(rebase("/p", "/p/sub"), PathBuf::from("."));
        assert_eq!(rebase("..", "/p/sub"), PathBuf::from("."));
    }
    use std::sync::{Mutex, MutexGuard};

    /// Env-var tests mutate process state — serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _g: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    const VARS: &[&str] = &[
        "BARCA_ENV",
        "BARCA_REMOTE",
        "BARCA_REMOTE_URI",
        "BARCA_ARTIFACT_URI",
        "BARCA_STATE_URI",
        "BARCA_STATE",
        "BARCA_PUSH_RETRIES",
        "BARCA_STORAGE_OPTIONS",
        "BARCA_TRANSFER_CONCURRENCY",
        "BARCA_TRANSFER_TIMEOUT",
        "BARCA_CACHE_MAX_SIZE",
        "BARCA_SERVER",
        "BARCA_CAPACITY",
        "BARCA_REQUIRE_ENV",
    ];

    fn clean_env() -> EnvGuard {
        let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = VARS
            .iter()
            .map(|&v| {
                let old = std::env::var(v).ok();
                unsafe { std::env::remove_var(v) };
                (v, old)
            })
            .collect();
        EnvGuard { _g: g, saved }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, old) in &self.saved {
                match old {
                    Some(v) => unsafe { std::env::set_var(var, v) },
                    None => unsafe { std::env::remove_var(var) },
                }
            }
        }
    }

    fn write_toml(dir: &Path, body: &str) {
        std::fs::write(dir.join(CONFIG_FILE), body).unwrap();
    }

    #[test]
    fn remote_off_overrides_uris_state_and_remote_options() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "[remote]\nuri = \"s3://toml/p\"\ntransfer_concurrency = 0\n",
        );
        unsafe {
            std::env::set_var("BARCA_REMOTE", "off");
            std::env::set_var("BARCA_REMOTE_URI", "s3://env/p");
            std::env::set_var("BARCA_ARTIFACT_URI", "s3://literal/artifacts");
            std::env::set_var("BARCA_STORAGE_OPTIONS", "invalid JSON");
            std::env::set_var("BARCA_TRANSFER_TIMEOUT", "invalid");
        }
        let cfg = resolve_in(Some("dev"), dir.path()).unwrap();
        assert!(cfg.remote_off);
        assert!(!cfg.remote_artifacts());
        assert!(cfg.backup_uri.is_none());
        assert!(cfg.storage_options_json.is_none());
        assert!(cfg.artifact_root.contains(".barca/envs/dev/artifacts"));
        assert!(cfg.db_path.contains(".barca/envs/dev/metadata.db"));
    }

    #[test]
    fn the_server_resolves_env_over_toml_and_an_empty_env_is_unset() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(configured_server(&sub).unwrap().is_none());
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[server]\nurl = \"https://toml.example.com/\"\n",
        )
        .unwrap();
        let (url, source) = configured_server(&sub).unwrap().unwrap();
        assert_eq!(
            (url.as_str(), source),
            ("https://toml.example.com/", ServerSource::Toml)
        );
        unsafe { std::env::set_var("BARCA_SERVER", "") };
        assert_eq!(
            configured_server(&sub).unwrap().unwrap().1,
            ServerSource::Toml
        );
        unsafe { std::env::set_var("BARCA_SERVER", "http://env:8274") };
        let (url, source) = configured_server(&sub).unwrap().unwrap();
        assert_eq!(
            (url.as_str(), source),
            ("http://env:8274", ServerSource::EnvVar)
        );
        std::fs::write(dir.path().join(CONFIG_FILE), "[server]\nport = 1\n").unwrap();
        unsafe { std::env::remove_var("BARCA_SERVER") };
        assert!(
            configured_server(&sub).is_err(),
            "unknown [server] keys are refused"
        );
    }

    #[test]
    fn server_urls_are_http_or_https_and_keep_a_proxy_prefix() {
        assert_eq!(
            check_server_url("https://host.example.com/barca/", "--server").unwrap(),
            "https://host.example.com/barca"
        );
        assert_eq!(
            check_server_url("http://127.0.0.1:8274", "--server").unwrap(),
            "http://127.0.0.1:8274"
        );
        for bad in ["host:8274", "ftp://x", "http://", "https:///x", ""] {
            let err = check_server_url(bad, "BARCA_SERVER")
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("BARCA_SERVER") && err.contains("not a server URL"),
                "{err}"
            );
        }
    }

    #[test]
    fn empty_remote_and_uri_values_retain_configured_storage() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[remote]\nuri = \"s3://toml/p\"\n");
        unsafe {
            std::env::set_var("BARCA_REMOTE", "");
            std::env::set_var("BARCA_REMOTE_URI", "");
            std::env::set_var("BARCA_ARTIFACT_URI", "");
        }
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert!(!cfg.remote_off);
        assert!(cfg.artifact_root.starts_with("s3://toml/p/"));
        assert_eq!(
            cfg.backup_uri.as_deref(),
            Some("s3://toml/p/default/state/backups")
        );
    }

    #[test]
    fn remote_switch_rejects_unrequested_modes() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        for value in ["on", "false", "OFF", "local"] {
            unsafe { std::env::set_var("BARCA_REMOTE", value) };
            assert!(matches!(
                resolve_in(None, dir.path()),
                Err(BarcaError::Usage(_))
            ));
        }
    }

    #[test]
    fn local_artifact_scope_handles_relative_absolute_file_and_parent_paths() {
        let root = ".barca/artifacts";
        assert!(local_artifact("./.barca/artifacts/n/h.json", root));
        let abs = std::env::current_dir()
            .unwrap()
            .join(".barca/artifacts/n/h.json");
        assert!(local_artifact(&abs.to_string_lossy(), root));
        assert!(local_artifact(&format!("file://{}", abs.display()), root));
        for path in [
            "s3://b/a/n/h.json",
            ".barca/artifacts-other/n/h.json",
            ".barca/artifacts/../../remote.json",
            "/store/n/h.json",
        ] {
            assert!(!local_artifact(path, root), "{path}");
        }
    }

    #[test]
    fn defaults_without_toml_or_env() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.env, "default");
        assert!(cfg.backup_uri.is_none());
        assert_eq!(cfg.local_backup_dir(), Path::new(".barca/backups"));
        assert!(cfg.storage_options_json.is_none());
        assert!(cfg.artifact_root.ends_with(".barca/artifacts"));
        assert!(cfg.db_path.ends_with(".barca/metadata.db"));
    }

    #[test]
    fn the_capacity_is_unset_by_default_and_the_env_var_wins_over_the_toml() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_in(None, dir.path()).unwrap().capacity, None);
        write_toml(dir.path(), "[execution]\ncapacity = 3\n");
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().capacity,
            Some((3, "[execution] capacity"))
        );
        assert_eq!(
            capacity_in(dir.path()).unwrap(),
            Some((3, "[execution] capacity"))
        );
        unsafe { std::env::set_var("BARCA_CAPACITY", "5") };
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().capacity,
            Some((5, "BARCA_CAPACITY"))
        );
        for bad in ["0", "-1", "two"] {
            unsafe { std::env::set_var("BARCA_CAPACITY", bad) };
            let err = resolve_in(None, dir.path()).unwrap_err();
            assert!(matches!(err, BarcaError::Usage(_)), "{bad}: {err}");
            assert!(err.to_string().contains("BARCA_CAPACITY"), "{err}");
        }
        unsafe { std::env::remove_var("BARCA_CAPACITY") };
        for bad in ["capacity = 0", "capacity = \"4\"", "capacity = 1.5"] {
            write_toml(dir.path(), &format!("[execution]\n{bad}\n"));
            let err = resolve_in(None, dir.path()).unwrap_err().to_string();
            assert!(err.contains("[execution] capacity"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_drain_timeout_defaults_to_30_seconds_and_server_drain_timeout_sets_it() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_in(None, dir.path()).unwrap().drain_timeout_secs, 30);
        write_toml(dir.path(), "[server]\ndrain_timeout = 120\n");
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().drain_timeout_secs,
            120
        );
        unsafe { std::env::set_var("BARCA_REMOTE", "off") };
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().drain_timeout_secs,
            120
        );
        unsafe { std::env::remove_var("BARCA_REMOTE") };
        assert_eq!(resolve_in(None, dir.path()).unwrap().server_max_runs, None);
        write_toml(dir.path(), "[server]\nmax_runs = 2\n");
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().server_max_runs,
            Some(2)
        );
        write_toml(dir.path(), "[server]\nmax_runs = 0\n");
        let err = resolve_in(None, dir.path()).unwrap_err().to_string();
        assert!(err.contains("max_runs"), "{err}");
        write_toml(dir.path(), "[server]\ndrain = 1\n");
        let err = resolve_in(None, dir.path()).unwrap_err().to_string();
        assert!(err.contains("drain"), "{err}");
    }

    #[test]
    fn toml_remote_uri_derives_env_prefixed_layout() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
default_env = "dev"
[remote]
uri = "abfss://cont@acct.dfs.core.windows.net/proj"
"#,
        );
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.env, "dev");
        assert_eq!(
            cfg.artifact_root,
            "abfss://cont@acct.dfs.core.windows.net/proj/dev/artifacts"
        );
        assert_eq!(
            cfg.backup_uri.as_deref(),
            Some("abfss://cont@acct.dfs.core.windows.net/proj/dev/state/backups")
        );
        // named env uses env-scoped local db and backups
        assert!(cfg.db_path.contains(".barca/envs/dev/"));
        assert_eq!(cfg.local_backup_dir(), Path::new(".barca/envs/dev/backups"));
    }

    #[test]
    fn require_env_refuses_a_command_without_env_or_barca_env() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".barca/envs/prod")).unwrap();
        std::fs::create_dir_all(dir.path().join(".barca/envs/staging")).unwrap();
        assert_eq!(known_envs(dir.path()), ["prod", "staging"]);
        write_toml(
            dir.path(),
            "default_env = \"staging\"\nrequire_env = true\n",
        );
        let err = resolve_in(None, dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("require_env = true in barca.toml requires one"),
            "{err}"
        );
        assert!(err.contains("environments here: prod, staging"), "{err}");
        assert_eq!(resolve_in(Some("prod"), dir.path()).unwrap().env, "prod");
        unsafe { std::env::set_var("BARCA_ENV", "prod") };
        assert_eq!(resolve_in(None, dir.path()).unwrap().env, "prod");
        unsafe { std::env::remove_var("BARCA_ENV") };
        unsafe { std::env::set_var("BARCA_REQUIRE_ENV", "0") };
        assert_eq!(resolve_in(None, dir.path()).unwrap().env, "staging");
        unsafe { std::env::set_var("BARCA_REQUIRE_ENV", "sometimes") };
        assert!(resolve_in(None, dir.path()).is_err());
        write_toml(dir.path(), "");
        unsafe { std::env::set_var("BARCA_REQUIRE_ENV", "1") };
        let err = resolve_in(None, dir.path()).unwrap_err().to_string();
        assert!(err.contains("BARCA_REQUIRE_ENV requires one"), "{err}");
    }

    #[test]
    fn cli_env_beats_env_var_beats_toml() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "default_env = \"from-toml\"\n");

        unsafe { std::env::set_var("BARCA_ENV", "from-env") };
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.env, "from-env");

        let cfg = resolve_in(Some("from-cli"), dir.path()).unwrap();
        assert_eq!(cfg.env, "from-cli");
    }

    #[test]
    fn the_startup_line_names_an_env_from_barca_toml_or_a_store() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(
            cfg.startup_line(),
            None,
            "nothing configured: nothing to say"
        );
        let cfg = resolve_in(Some("prod"), dir.path()).unwrap();
        assert_eq!(
            cfg.startup_line(),
            None,
            "--env on the command line is visible"
        );

        write_toml(dir.path(), "default_env = \"prod\"\n");
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(
            cfg.startup_line().unwrap(),
            "[barca] env prod (default_env in barca.toml); artifact store local (.barca/envs/prod/artifacts)"
        );

        write_toml(
            dir.path(),
            "[remote]\nuri = \"s3://user:secret@bucket/proj\"\n",
        );
        let cfg = resolve_in(Some("dev"), dir.path()).unwrap();
        assert_eq!(
            cfg.startup_line().unwrap(),
            "[barca] env dev (--env); artifact store s3://<redacted>@bucket/proj/dev/artifacts"
        );
    }

    #[test]
    fn the_serve_startup_line_adds_the_metadata_db_and_its_format() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        // `serve` always prints it, even with nothing configured.
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(
            cfg.serve_startup_line(Some(Ok(1))),
            "[barca] env default (built-in default); artifact store local (.barca/artifacts); \
             metadata .barca/metadata.db (db format 1)"
        );
        let cfg = resolve_in(Some("prod"), dir.path()).unwrap();
        assert_eq!(
            cfg.serve_startup_line(None),
            "[barca] env prod (--env); artifact store local (.barca/envs/prod/artifacts); \
             metadata .barca/envs/prod/metadata.db (not created yet)"
        );
        assert!(
            cfg.serve_startup_line(Some(Err(())))
                .ends_with("(unusable)")
        );
    }

    #[test]
    fn env_var_overrides_toml_uri() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[remote]\nuri = \"s3://toml-bucket/proj\"\n");
        unsafe { std::env::set_var("BARCA_REMOTE_URI", "s3://env-bucket/proj") };
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert!(cfg.artifact_root.starts_with("s3://env-bucket/proj/"));
        assert!(
            cfg.backup_uri
                .as_deref()
                .is_some_and(|u| u.starts_with("s3://env-bucket/proj/"))
        );
    }

    /// The shared history's settings are refused by name, with the release that removed them,
    /// even under BARCA_REMOTE=off (RFC-0009).
    #[test]
    fn removed_shared_history_settings_are_usage_errors_naming_the_release() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        for key in [
            "state = \"optimistic\"",
            "state = \"off\"",
            "state_uri = \"s3://b/h.db\"",
            "push_retries = 3",
        ] {
            write_toml(
                dir.path(),
                &format!("[remote]\nuri = \"s3://b/p\"\n{key}\n"),
            );
            let name = key.split(' ').next().unwrap();
            for remote in [None, Some("off")] {
                match remote {
                    Some(v) => unsafe { std::env::set_var("BARCA_REMOTE", v) },
                    None => unsafe { std::env::remove_var("BARCA_REMOTE") },
                }
                let err = resolve_in(None, dir.path()).unwrap_err();
                assert!(matches!(err, BarcaError::Usage(_)), "{key}");
                let msg = err.to_string();
                assert!(msg.contains(&format!("`[remote] {name}`")), "{msg}");
                assert!(msg.contains(SHARED_HISTORY_REMOVED_IN), "{msg}");
            }
        }
        unsafe { std::env::remove_var("BARCA_REMOTE") };
        write_toml(dir.path(), "[remote]\nuri = \"s3://b/p\"\n");
        for var in ["BARCA_STATE", "BARCA_STATE_URI", "BARCA_PUSH_RETRIES"] {
            unsafe { std::env::set_var(var, "off") };
            let msg = resolve_in(None, dir.path()).unwrap_err().to_string();
            assert!(
                msg.contains(var) && msg.contains(SHARED_HISTORY_REMOVED_IN),
                "{msg}"
            );
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn local_artifacts_are_not_remote_by_default() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.local_artifact_dir, cfg.artifact_root);
        assert!(!cfg.remote_artifacts());
        assert_eq!(cfg.transfer_concurrency, 4);
    }

    #[test]
    fn remote_uri_makes_artifacts_remote_with_env_scoped_local_dir() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[remote]\nuri = \"s3://b/p\"\n");
        let cfg = resolve_in(Some("dev"), dir.path()).unwrap();
        assert!(cfg.remote_artifacts());
        assert!(
            cfg.local_artifact_dir
                .ends_with(".barca/envs/dev/artifacts")
        );
    }

    #[test]
    fn plain_path_remote_root_is_remote() {
        // A shared local/NFS directory is a store too — workers still write
        // to .barca/artifacts and the transfer helper copies.
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[remote]\nuri = \"/mnt/shared/proj\"\n");
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.artifact_root, "/mnt/shared/proj/default/artifacts");
        assert!(cfg.remote_artifacts());
    }

    #[test]
    fn artifact_uri_pointing_at_local_dir_is_not_remote() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        for v in [
            ".barca/artifacts",
            "./.barca/artifacts",
            ".barca/artifacts/",
        ] {
            unsafe { std::env::set_var("BARCA_ARTIFACT_URI", v) };
            let cfg = resolve_in(None, dir.path()).unwrap();
            assert!(!cfg.remote_artifacts(), "{v} should be the local store");
        }
    }

    #[test]
    fn artifact_uri_env_makes_artifacts_remote() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BARCA_ARTIFACT_URI", "memory://arts") };
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert!(cfg.remote_artifacts());
    }

    #[test]
    fn transfer_concurrency_toml_then_env() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "[remote]\nuri = \"s3://b/p\"\ntransfer_concurrency = 8\n",
        );
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().transfer_concurrency,
            8
        );
        unsafe { std::env::set_var("BARCA_TRANSFER_CONCURRENCY", "16") };
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().transfer_concurrency,
            16
        );
    }

    #[test]
    fn transfer_timeout_default_toml_and_env() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().transfer_timeout_secs,
            600
        );
        write_toml(
            dir.path(),
            "[remote]\nuri = \"s3://b/p\"\ntransfer_timeout = 120\n",
        );
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().transfer_timeout_secs,
            120
        );
        unsafe { std::env::set_var("BARCA_TRANSFER_TIMEOUT", "30") };
        assert_eq!(
            resolve_in(None, dir.path()).unwrap().transfer_timeout_secs,
            30
        );
        for bad in ["0", "soon"] {
            unsafe { std::env::set_var("BARCA_TRANSFER_TIMEOUT", bad) };
            assert!(
                resolve_in(None, dir.path()).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn transfer_concurrency_rejects_zero_and_garbage() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        for bad in ["0", "lots"] {
            unsafe { std::env::set_var("BARCA_TRANSFER_CONCURRENCY", bad) };
            assert!(
                resolve_in(None, dir.path()).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn artifact_uri_back_compat_is_literal() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BARCA_ARTIFACT_URI", "memory://arts") };
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.artifact_root, "memory://arts"); // no env prefix
        assert!(cfg.backup_uri.is_none());
    }

    #[test]
    fn storage_options_merge_env_wins_per_key() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[remote.storage_options.abfs]
account_name = "toml-acct"
anon = false
[remote.storage_options.s3]
region = "us-east-1"
"#,
        );
        unsafe {
            std::env::set_var(
                "BARCA_STORAGE_OPTIONS",
                r#"{"abfs": {"account_name": "env-acct"}}"#,
            )
        };
        let cfg = resolve_in(None, dir.path()).unwrap();
        let merged: serde_json::Value =
            serde_json::from_str(cfg.storage_options_json.as_deref().unwrap()).unwrap();
        assert_eq!(merged["abfs"]["account_name"], "env-acct"); // env wins
        assert_eq!(merged["abfs"]["anon"], false); // toml key preserved
        assert_eq!(merged["s3"]["region"], "us-east-1");
    }

    #[test]
    fn invalid_env_name_rejected() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_in(Some("bad/name"), dir.path()).unwrap_err();
        assert!(err.to_string().contains("invalid environment name"));
    }

    #[test]
    fn malformed_toml_is_hard_error() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[remote\nuri = 3");
        let err = resolve_in(None, dir.path()).unwrap_err();
        assert!(err.to_string().contains("barca.toml"));
    }

    #[test]
    fn unknown_toml_key_is_hard_error() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[remote]\nurii = \"typo\"\n");
        assert!(resolve_in(None, dir.path()).is_err());
    }
}
