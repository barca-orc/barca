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
    /// Add one check. A failed check's `detail` ends with a line saying what to do.
    pub fn add(&mut self, name: &str, ok: bool, detail: impl Into<String>, data: Value) {
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
    #[serde(default)]
    markers: Option<pep508_rs::MarkerEnvironment>,
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
    use crate::text::human;
    match crate::gc::usage(cfg) {
        Ok(usage) => {
            let limit = match usage.limit_bytes {
                Some(limit) => format!(
                    "{} of its {} limit ({}, {})",
                    human(usage.budget_bytes),
                    human(limit),
                    usage.max_size,
                    usage.source
                ),
                None => format!(
                    "{}, no limit ([cache] max_size is off; {})",
                    human(usage.budget_bytes),
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
                format!(
                    "{limit}; {} metadata backups outside the budget; {} physical total{free}",
                    human(usage.backup_bytes),
                    human(usage.size_bytes)
                ),
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

/// How many workers a run starts and the capacity their steps' `weight=` shares (#436).
/// Informational.
fn capacity(report: &mut DoctorReport, cfg: &config::ResolvedConfig) {
    let sizing = crate::sizing::sizing_for(cfg, None);
    let (capacity, source, pool_size, pool_source) = (
        sizing.capacity,
        sizing.capacity_source,
        sizing.pool_size,
        sizing.pool_source,
    );
    report.add(
        "capacity",
        true,
        format!(
            "steps running at once total at most weight {capacity} ({source}); pool of {pool_size} worker{} ({pool_source})",
            if pool_size == 1 { "" } else { "s" }
        ),
        json!({
            "capacity": capacity,
            "pool_size": pool_size,
            "source": source,
            "pool_source": pool_source,
        }),
    );
    // What a run under `barca serve` gets, when the server shares a capacity between its runs.
    if let Some((total, source)) = cfg.server_capacity {
        let served = crate::sizing(cfg.capacity, cfg.pool_size, Some((total, source)));
        report.add(
            "server_capacity",
            true,
            format!(
                "under barca serve, the steps of every run together total at most weight \
                 {total} ({source}); each run's pool is {} worker{} ({})",
                served.pool_size,
                if served.pool_size == 1 { "" } else { "s" },
                served.pool_source
            ),
            json!({
                "capacity": total,
                "pool_size": served.pool_size,
                "pool_source": served.pool_source,
            }),
        );
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
            // Where the metadata DB is backed up (RFC-0009): `backup` with a store, `local`
            // (no backups) without one, `remote_off` under BARCA_REMOTE=off.
            let mode = if cfg.remote_off {
                "remote_off"
            } else if cfg.backup_uri.is_some() {
                "backup"
            } else {
                "local"
            };
            report.add("state", true, mode, json!({"mode":mode,"uri":cfg.backup_uri.as_deref().map(crate::transfer::diagnostic_uri),"local_backups":cfg.local_backup_dir()}));
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
            capacity(&mut report, &cfg);
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
        if let Some(uri) = &cfg.backup_uri {
            stores.push(json!({"name":"backups", "uri":uri}));
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
                    project_version(&mut report, &root, result.markers.as_ref());
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
    // In place when the database can be read so: no copy of it, and no lock that a run or a
    // server would wait for while every page is checked.
    if let Some(view) = crate::db::ReadView::open(path)
        .await
        .map_err(|e| e.to_string())?
    {
        return match crate::state_validate::check_in_place(view.path())
            .await
            .map_err(|e| e.to_string())?
        {
            Ok(()) => Ok("metadata snapshot integrity and format are valid".into()),
            Err(error) => Err(error.why),
        };
    }
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

/// The project's base dependencies, with PEP 508 markers evaluated for the interpreter
/// doctor actually probed. Optional extras and dependency groups are not selected here.
fn project_version(
    report: &mut DoctorReport,
    root: &Path,
    markers: Option<&pep508_rs::MarkerEnvironment>,
) {
    let path = root.join("pyproject.toml");
    if !path.exists() {
        return;
    }
    let check = || -> Result<String, String> {
        let source = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let project: toml::Value = toml::from_str(&source).map_err(|e| e.to_string())?;
        let Some(dependencies) = project.get("project").and_then(|p| p.get("dependencies")) else {
            return Ok("no declared project.dependencies requirement for barca".into());
        };
        let dependencies = dependencies
            .as_array()
            .ok_or("project.dependencies must be an array")?;
        let running = env!("CARGO_PKG_VERSION")
            .parse()
            .map_err(|e| format!("invalid running version: {e}"))?;
        let mut matched = Vec::new();
        for dependency in dependencies {
            let text = dependency
                .as_str()
                .ok_or("project.dependencies entries must be strings")?;
            let requirement: pep508_rs::Requirement = text.parse().map_err(|_| {
                "invalid project.dependencies requirement; use PEP 508 syntax".to_string()
            })?;
            if requirement.name.as_ref() != "barca" {
                continue;
            }
            let markers =
                markers.ok_or("Python marker environment was not reported; reinstall barca")?;
            if !requirement.evaluate_markers(markers, &[]) {
                continue;
            }
            match &requirement.version_or_url {
                Some(pep508_rs::VersionOrUrl::VersionSpecifier(spec))
                    if !spec.contains(&running) =>
                {
                    return Err(format!(
                        "running barca {} does not satisfy {text}; sync the project environment and run its barca",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                Some(pep508_rs::VersionOrUrl::Url(_)) => {
                    return Err(format!(
                        "cannot verify a direct URL barca requirement against running barca {}; declare a version requirement to check it",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                _ => matched.push(text.to_owned()),
            }
        }
        Ok(if matched.is_empty() {
            "no active project.dependencies requirement for barca".into()
        } else {
            format!(
                "running barca {} satisfies {}",
                env!("CARGO_PKG_VERSION"),
                matched.join(", ")
            )
        })
    };
    let result = check();
    let ok = result.is_ok();
    report.add(
        "project_version",
        ok,
        result.unwrap_or_else(|e| e),
        Value::Null,
    );
}
