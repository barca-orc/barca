//! Prerequisite diagnostics. Source checks are static; existing metadata is snapshotted.

use crate::{config, helper_proc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub data: Value,
}

#[derive(Debug, Default, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<Check>,
}

impl DoctorReport {
    fn add(&mut self, name: &str, ok: bool, detail: impl Into<String>, data: Value) {
        self.checks.push(Check {
            name: name.into(),
            ok,
            detail: detail.into(),
            data,
        });
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeReport {
    python: PythonInfo,
    stores: Vec<StoreProbe>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PythonInfo {
    executable: String,
    version: String,
    barca_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreProbe {
    name: String,
    ok: bool,
    detail: String,
    data: Value,
}

/// Inspect a rooted project. This never loads user Python or synchronizes remote state.
/// `.barca/` against its size limit (RFC-0012), and free space on its filesystem. Informational:
/// over the limit is what eviction is for, not a failed check.
fn cache_size(report: &mut DoctorReport, cfg: &config::ResolvedConfig) {
    use crate::gc::size::human;
    match crate::gc::usage(cfg) {
        Ok(usage) => {
            let limit = match usage.limit_bytes {
                Some(limit) => format!(
                    "{} of its {} limit ({}, {})",
                    human(usage.size_bytes),
                    human(limit),
                    usage.max_size,
                    usage.source
                ),
                None => format!(
                    "{}, no limit ([cache] max_size is off; {})",
                    human(usage.size_bytes),
                    usage.source
                ),
            };
            let free = match (usage.available_bytes, usage.filesystem_bytes) {
                (Some(free), Some(total)) => {
                    format!(
                        "; {} free of {} on its filesystem",
                        human(free),
                        human(total)
                    )
                }
                _ => String::new(),
            };
            report.add(
                "cache_size",
                true,
                format!("{limit}{free}"),
                serde_json::to_value(&usage).unwrap_or(Value::Null),
            );
        }
        Err(e) => report.add(
            "cache_size",
            true,
            format!("not measured: {e}"),
            Value::Null,
        ),
    }
}

pub async fn doctor(files: &[PathBuf], env: Option<&str>, python: &Path) -> DoctorReport {
    let mut report = DoctorReport::default();
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            report.add("project_root", false, error.to_string(), Value::Null);
            return report;
        }
    };
    let found = root.join(config::CONFIG_FILE).is_file();
    report.add(
        "project_root",
        true,
        format!(
            "{} (found from {})",
            root.display(),
            if found { "nearest barca.toml" } else { "cwd" }
        ),
        json!({
            "path": root, "source": if found { "barca.toml" } else { "cwd" }
        }),
    );
    let discovery = match config::load_toml(&root) {
        Ok(toml) => {
            report.add(
                "configuration",
                true,
                if found {
                    "barca.toml parsed"
                } else {
                    "no barca.toml; defaults apply"
                },
                json!({"found":found}),
            );
            toml.and_then(|file| file.discovery).unwrap_or_default()
        }
        Err(error) => {
            report.add(
                "configuration",
                false,
                error
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("invalid configuration")
                    .to_owned(),
                json!({"found":found}),
            );
            config::DiscoveryToml::default()
        }
    };
    let cfg = match config::resolve(env) {
        Ok(cfg) => {
            report.add(
                "environment",
                true,
                cfg.env.clone(),
                json!({"name":cfg.env}),
            );
            let mode = if cfg.remote_off {
                "remote_off"
            } else if cfg.state == config::StateMode::Off {
                "off"
            } else {
                "shared"
            };
            report.add("state", true, mode, json!({"mode":mode,"uri":cfg.state_uri.as_deref().map(crate::transfer::diagnostic_uri)}));
            let local = Path::new(&cfg.local_artifact_dir);
            let writable = std::fs::create_dir_all(local)
                .and_then(|_| tempfile::NamedTempFile::new_in(local).map(drop));
            report.add(
                "local_cache",
                writable.is_ok(),
                writable.err().map_or_else(
                    || format!("{} is writable", local.display()),
                    |e| e.to_string(),
                ),
                json!({"artifacts":cfg.local_artifact_dir,"metadata":cfg.db_path}),
            );
            let health = metadata(&cfg.db_path).await;
            report.add(
                "metadata",
                health.is_ok(),
                health.unwrap_or_else(|e| e),
                json!({"path":cfg.db_path}),
            );
            cache_size(&mut report, &cfg);
            Some(cfg)
        }
        Err(error) => {
            // Config parsing errors belong to their check; resolution has its own diagnostics
            // for bad env names, URI/mode conflicts and storage options.
            report.add(
                "environment",
                false,
                error
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("invalid configuration")
                    .to_owned(),
                Value::Null,
            );
            None
        }
    };
    match crate::discover::discover(&root, files, &discovery) {
        Ok(files) => {
            report.add(
                "discovery",
                true,
                format!("{} source files", files.len()),
                json!({"files":files}),
            );
            for file in files {
                let result = std::fs::read_to_string(root.join(&file))
                    .map_err(|error| error.to_string())
                    .and_then(|source| {
                        crate::parse::extract_nodes(&source, &file).map_err(|e| e.to_string())
                    });
                match result {
                    Ok(nodes) => report.add(
                        &format!("source:{file}"),
                        true,
                        format!("{} definitions parsed without imports", nodes.len()),
                        json!({"file":file,"nodes":nodes.len()}),
                    ),
                    Err(error) => report.add(
                        &format!("source:{file}"),
                        false,
                        format!("{file}: {error}"),
                        json!({"file":file}),
                    ),
                }
            }
        }
        Err(error) => report.add("discovery", false, error.to_string(), Value::Null),
    }
    let mut command = crate::helper_proc::python_module(
        python,
        "barca._doctor",
        cfg.as_ref().and_then(|c| c.storage_options_json.as_deref()),
    );
    let stores = cfg.as_ref().map_or_else(Vec::new, |cfg| {
        if cfg.remote_off {
            return Vec::new();
        }
        let mut stores = Vec::new();
        if cfg.remote_artifacts() {
            stores.push(json!({"name":"artifacts", "uri":cfg.artifact_root}));
        }
        if cfg.state == config::StateMode::Optimistic
            && let Some(uri) = &cfg.state_uri
        {
            stores.push(json!({"name":"state", "uri":uri}));
        }
        stores
    });
    command.env("BARCA_DOCTOR_REQUEST", json!({"stores":stores}).to_string());
    match helper_proc::run_to_output(command, None, Some(Duration::from_secs(30))).await {
        Ok(output) if output.status.success() => {
            let parsed = serde_json::from_slice::<ProbeReport>(&output.stdout)
                .map_err(|error| error.to_string())
                .and_then(|result| {
                    let expected: std::collections::BTreeSet<_> = stores
                        .iter()
                        .filter_map(|store| store["name"].as_str())
                        .collect();
                    let actual: std::collections::BTreeSet<_> = result
                        .stores
                        .iter()
                        .map(|store| store.name.as_str())
                        .collect();
                    if actual != expected || actual.len() != result.stores.len() {
                        Err("missing, duplicated or unsolicited store checks".into())
                    } else {
                        Ok(result)
                    }
                });
            match parsed {
                Ok(result) => {
                    let compatible = result.python.barca_version == env!("CARGO_PKG_VERSION");
                    let detail = if compatible {
                        result.python.executable.clone()
                    } else {
                        format!(
                            "Python barca {} differs from CLI {}; reinstall matching packages",
                            result.python.barca_version,
                            env!("CARGO_PKG_VERSION")
                        )
                    };
                    report.add(
                        "python",
                        compatible,
                        detail,
                        serde_json::to_value(&result.python).unwrap(),
                    );
                    if stores.is_empty() && cfg.is_some() {
                        report.add(
                            "store:artifacts",
                            true,
                            "local artifacts; no remote probe",
                            json!({"enabled":false}),
                        );
                    }
                    for store in result.stores {
                        report.add(
                            &format!("store:{}", store.name),
                            store.ok,
                            store.detail,
                            store.data,
                        );
                    }
                }
                Err(error) => report.add(
                    "python",
                    false,
                    format!("invalid doctor helper output: {error}"),
                    Value::Null,
                ),
            }
        }
        Ok(output) => report.add(
            "python",
            false,
            String::from_utf8_lossy(&output.stderr).trim(),
            json!({"selected":python}),
        ),
        Err(helper_proc::OutputFailed::Spawn(error)) => report.add(
            "python",
            false,
            error.to_string(),
            json!({"selected":python}),
        ),
        Err(helper_proc::OutputFailed::TimedOut(_)) => report.add(
            "python",
            false,
            "doctor helper exceeded 30 seconds; process stopped",
            json!({"selected":python}),
        ),
        Err(helper_proc::OutputFailed::Cancelled) => report.add(
            "python",
            false,
            "doctor helper cancelled",
            json!({"selected":python}),
        ),
    }
    for store in &stores {
        let name = format!("store:{}", store["name"].as_str().unwrap_or_default());
        if !report.checks.iter().any(|check| check.name == name) {
            report.add(
                &name,
                false,
                "not checked because the Python helper failed",
                json!({"enabled":true}),
            );
        }
    }
    if cfg.is_none() {
        report.add(
            "store:artifacts",
            false,
            "not checked because configuration is invalid",
            json!({"enabled":null}),
        );
    }
    report.ok = report.checks.iter().all(|check| check.ok);
    report
}

async fn metadata(path: &str) -> Result<String, String> {
    let Some(snapshot) = crate::db::DbSnapshot::take(path)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok("no metadata database yet; a run will create it".into());
    };
    match crate::state_validate::open(snapshot.path(), crate::state_validate::Pages::Check)
        .await
        .map_err(|e| e.to_string())?
    {
        Ok(_) => Ok("metadata snapshot integrity and format are valid".into()),
        Err(error) => Err(error.why),
    }
}
