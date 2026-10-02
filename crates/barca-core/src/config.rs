//! Configuration resolution — barca.toml, environment variables, CLI flags.
//!
//! barca.toml is discovered in the current working directory only (no
//! walk-up): everything barca persists is already cwd-anchored (`.barca/`),
//! so the config governing that state is anchored the same way.
//!
//! Precedence for every value: CLI flag > environment variable > barca.toml
//! > built-in default.

use crate::BarcaError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const CONFIG_FILE: &str = "barca.toml";
pub const DEFAULT_ENV: &str = "default";

// ─── Raw file shape ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BarcaToml {
    pub default_env: Option<String>,
    pub remote: Option<RemoteToml>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RemoteToml {
    pub uri: Option<String>,
    pub artifacts_uri: Option<String>,
    pub state_uri: Option<String>,
    pub state: Option<String>,
    pub push_retries: Option<u32>,
    /// Concurrent artifact uploads/downloads by the transfer helper.
    pub transfer_concurrency: Option<usize>,
    /// Seconds one transfer attempt may run before it is failed as stalled.
    pub transfer_timeout: Option<u64>,
    /// Per-fsspec-protocol option tables, e.g. `[remote.storage_options.abfs]`.
    pub storage_options: Option<toml::Table>,
}

// ─── Resolved configuration ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateMode {
    /// Pull at run start, etag/generation-conditional push at run end.
    Optimistic,
    /// Metadata stays local (0.4.0 behavior).
    Off,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// Validated environment name (path/URI segment).
    pub env: String,
    /// Local metadata DB path for this env (state sync overwrites this file).
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
    /// Remote location of the shared metadata blob, when remote mode is on.
    pub state_uri: Option<String>,
    pub state: StateMode,
    pub push_retries: u32,
    /// Merged storage options (toml ⊕ env, env keys win), serialized as the
    /// JSON that `BARCA_STORAGE_OPTIONS` carries to child processes.
    pub storage_options_json: Option<String>,
}

impl ResolvedConfig {
    /// True when the artifact store is separate from the local artifact dir,
    /// so artifacts are transferred in the background (upload after a step
    /// completes, download before a cached artifact is consumed).
    pub fn remote_artifacts(&self) -> bool {
        fn norm(p: &str) -> &str {
            p.trim_start_matches("./").trim_end_matches('/')
        }
        norm(&self.artifact_root) != norm(&self.local_artifact_dir)
    }
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
        .map_err(|e| BarcaError::Other(format!("invalid {}: {e}", path.display())))?;
    Ok(Some(parsed))
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

    // env name: CLI > BARCA_ENV > default_env in toml > "default"
    let env = cli_env
        .map(str::to_string)
        .or_else(|| env_var("BARCA_ENV"))
        .or(file.default_env)
        .unwrap_or_else(|| DEFAULT_ENV.to_string());
    if !valid_env_name(&env) {
        return Err(BarcaError::Other(format!(
            "invalid environment name '{env}' — allowed characters: A-Z a-z 0-9 . _ -"
        )));
    }

    // remote root: BARCA_REMOTE_URI > [remote].uri
    let remote_uri = env_var("BARCA_REMOTE_URI").or(remote.uri);

    // Local layout (default env keeps the legacy paths — zero migration).
    let local = crate::db::env_local_paths(&env);

    // artifacts: BARCA_ARTIFACT_URI (0.4.0 back-compat, literal) >
    //            [remote].artifacts_uri (literal) > {uri}/{env}/artifacts > local
    let artifact_env_override = env_var("BARCA_ARTIFACT_URI");
    if artifact_env_override.is_some() && env != DEFAULT_ENV {
        eprintln!(
            "[barca] Warning: BARCA_ARTIFACT_URI is set — it is used literally and \
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

    // state uri: BARCA_STATE_URI > [remote].state_uri > {uri}/{env}/state/metadata.db
    let state_uri = env_var("BARCA_STATE_URI").or(remote.state_uri).or_else(|| {
        remote_uri
            .as_deref()
            .map(|u| join_uri(u, &format!("{env}/state/metadata.db")))
    });

    // state mode: BARCA_STATE > [remote].state > optimistic-if-state-uri-else-off
    let mode_str = env_var("BARCA_STATE").or(remote.state);
    let state = match mode_str.as_deref() {
        Some("optimistic") => StateMode::Optimistic,
        Some("off") => StateMode::Off,
        Some(other) => {
            return Err(BarcaError::Other(format!(
                "invalid state mode '{other}' (expected \"optimistic\" or \"off\")"
            )));
        }
        None => {
            if state_uri.is_some() {
                StateMode::Optimistic
            } else {
                StateMode::Off
            }
        }
    };

    let push_retries = match env_var("BARCA_PUSH_RETRIES") {
        Some(v) => v.parse::<u32>().map_err(|_| {
            BarcaError::Other(format!(
                "invalid BARCA_PUSH_RETRIES '{v}' (expected integer)"
            ))
        })?,
        None => remote.push_retries.unwrap_or(5),
    };

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
        env,
        db_path: local.db_path,
        artifact_root,
        local_artifact_dir: local.artifact_dir,
        transfer_concurrency,
        transfer_timeout_secs,
        state_uri,
        state,
        push_retries,
        storage_options_json,
    })
}

/// Merge `[remote.storage_options.*]` with `BARCA_STORAGE_OPTIONS` (env keys
/// win per protocol, per key). Returns the merged JSON string, or None when
/// both sources are empty.
fn merge_storage_options(from_toml: Option<&toml::Table>) -> Result<Option<String>, BarcaError> {
    let mut merged: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();

    if let Some(table) = from_toml {
        for (protocol, opts) in table {
            let toml::Value::Table(opts) = opts else {
                return Err(BarcaError::Other(format!(
                    "[remote.storage_options.{protocol}] must be a table of options"
                )));
            };
            let entry = merged.entry(protocol.clone()).or_default();
            for (k, v) in opts {
                let json = serde_json::to_value(v.clone()).map_err(|e| {
                    BarcaError::Other(format!("storage_options.{protocol}.{k}: {e}"))
                })?;
                entry.insert(k.clone(), json);
            }
        }
    }

    if let Some(raw) = env_var("BARCA_STORAGE_OPTIONS") {
        let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            BarcaError::Other(format!("BARCA_STORAGE_OPTIONS is not valid JSON: {e}"))
        })?;
        let serde_json::Value::Object(by_protocol) = parsed else {
            return Err(BarcaError::Other(
                "BARCA_STORAGE_OPTIONS must be a JSON object keyed by protocol".to_string(),
            ));
        };
        for (protocol, opts) in by_protocol {
            let serde_json::Value::Object(opts) = opts else {
                return Err(BarcaError::Other(format!(
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Env-var tests mutate process state — serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _g: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    const VARS: &[&str] = &[
        "BARCA_ENV",
        "BARCA_REMOTE_URI",
        "BARCA_ARTIFACT_URI",
        "BARCA_STATE_URI",
        "BARCA_STATE",
        "BARCA_PUSH_RETRIES",
        "BARCA_STORAGE_OPTIONS",
        "BARCA_TRANSFER_CONCURRENCY",
        "BARCA_TRANSFER_TIMEOUT",
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
    fn defaults_without_toml_or_env() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(cfg.env, "default");
        assert_eq!(cfg.state, StateMode::Off);
        assert!(cfg.state_uri.is_none());
        assert_eq!(cfg.push_retries, 5);
        assert!(cfg.storage_options_json.is_none());
        assert!(cfg.artifact_root.ends_with(".barca/artifacts"));
        assert!(cfg.db_path.ends_with(".barca/metadata.db"));
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
            cfg.state_uri.as_deref(),
            Some("abfss://cont@acct.dfs.core.windows.net/proj/dev/state/metadata.db")
        );
        assert_eq!(cfg.state, StateMode::Optimistic);
        // named env uses env-scoped local db
        assert!(cfg.db_path.contains(".barca/envs/dev/"));
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
    fn env_var_overrides_toml_uri_and_state() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[remote]
uri = "s3://toml-bucket/proj"
state = "optimistic"
push_retries = 2
"#,
        );
        unsafe {
            std::env::set_var("BARCA_REMOTE_URI", "s3://env-bucket/proj");
            std::env::set_var("BARCA_STATE", "off");
            std::env::set_var("BARCA_PUSH_RETRIES", "9");
        }
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert!(cfg.artifact_root.starts_with("s3://env-bucket/proj/"));
        assert_eq!(cfg.state, StateMode::Off);
        assert_eq!(cfg.push_retries, 9);
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
        assert!(cfg.state_uri.is_none());
        assert_eq!(cfg.state, StateMode::Off);
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
    fn invalid_state_mode_rejected() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BARCA_STATE", "pessimistic") };
        let err = resolve_in(None, dir.path()).unwrap_err();
        assert!(err.to_string().contains("invalid state mode"));
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

    #[test]
    fn explicit_state_uri_without_root_enables_optimistic() {
        let _e = clean_env();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BARCA_STATE_URI", "s3://bucket/custom/state.db") };
        let cfg = resolve_in(None, dir.path()).unwrap();
        assert_eq!(
            cfg.state_uri.as_deref(),
            Some("s3://bucket/custom/state.db")
        );
        assert_eq!(cfg.state, StateMode::Optimistic);
    }
}
