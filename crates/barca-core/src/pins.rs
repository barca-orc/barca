//! Pinning an asset to an earlier materialization (`barca pin`, `barca unpin`, the
//! `/assets/{name}/pin` routes). A pinned asset is read by every run as the materialization it
//! is pinned at and is never computed, whatever the cache policy, schedule or `--refresh`
//! ([`crate::freshness::walk_with`]); what reads it is hashed against that materialization's
//! run hash, so it is computed again (or served from an earlier cache hit) against the pinned
//! data. `barca unpin` removes the pin; the next run decides the asset as before.

use crate::BarcaError;
use crate::dag::Dag;
use crate::db::{self, AssetPin, Version};
use crate::targets::{find_target_id, short_name};
use serde::{Deserialize, Serialize};

/// The versions a list shows by default.
pub const DEFAULT_VERSIONS: usize = 20;

/// Which materialization to pin.
#[derive(Debug, Clone)]
pub enum Choice {
    /// The one with this run hash, or a unique prefix of it (at least 6 characters).
    RunHash(String),
    /// The newest version before the latest one.
    Previous,
}

/// What `barca pin` did (its `--json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "PinResult"))]
pub struct Pinned {
    pub node_id: String,
    pub pin: AssetPin,
    pub version: Version,
    /// The pin it replaced, if it was pinned already.
    pub replaced: Option<AssetPin>,
}

/// What `barca unpin` did (its `--json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "UnpinResult"))]
pub struct Unpinned {
    pub node_id: String,
    /// The pin removed; null when it was not pinned.
    pub removed: Option<AssetPin>,
}

/// `barca pin TARGET --versions` (its `--json`), `GET /assets/{name}/versions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, rename = "AssetVersions")
)]
pub struct Versions {
    pub node_id: String,
    /// Whether it can be pinned: an unpartitioned asset or one complete partition key.
    pub pinnable: bool,
    pub pin: Option<AssetPin>,
    pub versions: Vec<Version>,
}

/// The node `name` names, checked to be one that can be pinned.
fn pinnable(dag: &Dag, name: &str) -> Result<(String, bool), BarcaError> {
    // A literal node id can itself contain brackets in its source path.
    if let Ok(id) = find_target_id(dag, name) {
        let node = dag.get_node(&id).expect("resolved node exists");
        return Ok((
            id,
            node.kind() == crate::NodeKind::Asset && node.extracted.partitions.is_empty(),
        ));
    }
    let step = crate::StepId::parse(name);
    let id = find_target_id(dag, step.base_id())?;
    let node = dag.get_node(&id).expect("resolved node exists");
    if !step.partition.is_empty() {
        if !name.ends_with(']') || step.partition.0.keys().ne(node.extracted.partitions.keys()) {
            return Err(BarcaError::Usage(
                "name exactly one key with every partition dimension".into(),
            ));
        }
        for (dim, value) in &step.partition.0 {
            if let crate::PartitionSpec::Static { values } = &node.extracted.partitions[dim] {
                let known = values.iter().any(|v| match v {
                    crate::PartitionValue::Str(s) => s == value,
                    crate::PartitionValue::Int(n) => n.to_string() == *value,
                });
                if !known {
                    return Err(BarcaError::Usage(format!(
                        "unknown partition value {dim}={value}"
                    )));
                }
            }
        }
        return Ok((
            step.partition.display_id(&id),
            node.kind() == crate::NodeKind::Asset,
        ));
    }
    Ok((
        id,
        node.kind() == crate::NodeKind::Asset && node.extracted.partitions.is_empty(),
    ))
}

fn refuse(id: &str) -> BarcaError {
    BarcaError::Usage(format!(
        "'{}' cannot be pinned: name an asset or one complete partition key\nTasks and sensors cannot be pinned.",
        short_name(id)
    ))
}

/// Who is pinning from this machine: `user@host`.
pub fn local_actor() -> Option<String> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| !u.is_empty())?;
    let host = db::local_host();
    Some(if host.is_empty() {
        user
    } else {
        format!("{user}@{host}")
    })
}

/// The pins in the database at `db_path`.
pub async fn list(db_path: &str) -> Result<Vec<AssetPin>, BarcaError> {
    db::get_pins(db_path).await
}

/// The latest `limit` versions of `name`, and its pin.
pub async fn versions(
    db_path: &str,
    dag: &Dag,
    name: &str,
    limit: usize,
) -> Result<Versions, BarcaError> {
    let (id, ok) = pinnable(dag, name)?;
    let pin = db::get_pins(db_path)
        .await?
        .into_iter()
        .find(|p| p.node_id == id);
    let versions = db::get_versions(
        db_path,
        &id,
        limit,
        pin.as_ref().map(|p| p.run_hash.as_str()),
    )
    .await?;
    Ok(Versions {
        node_id: id,
        pinnable: ok,
        pin,
        versions,
    })
}

/// Whether the artifact at `path` can be read here: a local file that exists, or a store URI.
fn readable(path: &str) -> bool {
    path.contains("://") || std::path::Path::new(path).is_file()
}

/// Pin `name` to the materialization `choice` names. `db_path` must exist (initialized).
pub async fn pin(
    db_path: &str,
    dag: &Dag,
    name: &str,
    choice: &Choice,
    reason: Option<String>,
    by: Option<String>,
) -> Result<Pinned, BarcaError> {
    let (id, ok) = pinnable(dag, name)?;
    if !ok {
        return Err(refuse(&id));
    }
    let short = short_name(&id).to_string();
    let all = db::get_versions(db_path, &id, usize::MAX, None).await?;
    if all.is_empty() {
        return Err(BarcaError::Usage(format!(
            "'{short}' has no successful materialization to pin\nRun it first: barca get {short}"
        )));
    }
    let version = match choice {
        Choice::Previous => all.get(1).cloned().ok_or_else(|| {
            BarcaError::Usage(format!(
                "'{short}' has only one successful materialization: there is no previous one\n\
                 List them with: barca pin {short} --versions"
            ))
        })?,
        Choice::RunHash(hash) => {
            if hash.len() < 6 {
                return Err(BarcaError::Usage(format!(
                    "--run-hash '{hash}' is too short: give at least 6 characters"
                )));
            }
            let found: Vec<&Version> = all
                .iter()
                .filter(|v| v.run_hash.starts_with(hash.as_str()))
                .collect();
            match found.as_slice() {
                [one] => (*one).clone(),
                [] => {
                    return Err(BarcaError::Usage(format!(
                        "'{short}' has no successful materialization with run hash '{hash}'\n\
                         List them with: barca pin {short} --versions"
                    )));
                }
                _ => {
                    return Err(BarcaError::Usage(format!(
                        "--run-hash '{hash}' matches {} materializations of '{short}': give more \
                         characters",
                        found.len()
                    )));
                }
            }
        }
    };
    if !version.available || !readable(&version.artifact) {
        return Err(BarcaError::Usage(format!(
            "the artifact of run hash {} of '{short}' is no longer on disk ({}), so it cannot \
             be pinned\nPin another version: barca pin {short} --versions",
            version.run_hash, version.artifact
        )));
    }
    let replaced = db::get_pins(db_path)
        .await?
        .into_iter()
        .find(|p| p.node_id == id);
    let pin = AssetPin {
        node_id: id.clone(),
        run_hash: version.run_hash.clone(),
        pinned_at: crate::time::rfc3339(chrono::Utc::now().timestamp()),
        pinned_by: by,
        reason: reason.filter(|r| !r.trim().is_empty()),
        run_id: version.run_id.clone(),
        upload: version.upload.clone(),
    };
    db::set_pin(db_path, &pin).await?;
    let mut version = version;
    version.pinned = true;
    Ok(Pinned {
        node_id: id,
        pin,
        version,
        replaced,
    })
}

/// The largest file `barca pin --from-file` and `POST /assets/{name}/pin/upload` accept.
pub const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

/// Data an operator supplies to pin an asset to (`barca pin --from-file`).
#[derive(Debug, Clone)]
pub struct Supplied {
    pub bytes: Vec<u8>,
    /// The base name of the file supplied; its extension gives the format unless `format` does.
    pub file_name: String,
    /// `json`, `parquet`, `csv` or `pickle`.
    pub format: Option<String>,
}

/// The format of an upload: the one named, else the file name's extension.
pub fn upload_format(file_name: &str, format: Option<&str>) -> Result<String, BarcaError> {
    let known = ["json", "parquet", "csv", "pickle"];
    let fmt = match format.map(str::trim).filter(|f| !f.is_empty()) {
        Some("pkl") => "pickle".to_string(),
        Some(f) => f.to_ascii_lowercase(),
        None => crate::artifact_format::format_of(file_name).to_string(),
    };
    if known.contains(&fmt.as_str()) {
        Ok(fmt)
    } else {
        Err(BarcaError::Usage(format!(
            "cannot tell the format of '{file_name}': name a .json, .parquet, .csv or .pkl file, \
             or give the format (json, parquet, csv or pickle)"
        )))
    }
}

/// The run hash of an upload of `bytes`: derived from the content, never equal to a computed
/// run hash.
pub fn upload_run_hash(bytes: &[u8]) -> String {
    let mut tagged = b"barca-upload\0".to_vec();
    tagged.extend_from_slice(bytes);
    crate::hash::sha256_hex(&tagged)
}

/// Load the artifact at `path` with the reader runs use (`barca._value` `check`); why not.
async fn check_loads(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
    path: &str,
    format: &str,
) -> Result<(), String> {
    let request = serde_json::json!({
        "op": "check",
        "artifacts": [{"path": path, "format": format}],
    });
    let cmd = crate::helper_proc::python_module(
        python,
        "barca._value",
        cfg.storage_options_json.as_deref(),
    );
    let answers = crate::store_inspect::run_json_helper(cmd, python, &request)
        .await
        .map_err(|e| format!("could not run the artifact reader: {e}"))?;
    match answers.first() {
        Some(a) if a.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => Ok(()),
        Some(a) => Err(a
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the artifact reader returned an unexpected result")
            .to_string()),
        None => Err("the artifact reader returned an unexpected result".into()),
    }
}

/// Validate runtime membership before recording replacement bytes. Read the same current
/// source (including a source pin) as execution; never create a key by uploading to a typo.
async fn check_runtime_key(
    cfg: &crate::config::ResolvedConfig,
    python: &std::path::Path,
    dag: &Dag,
    id: &str,
) -> Result<(), BarcaError> {
    let step = crate::StepId::parse(id);
    if step.partition.is_empty() {
        return Ok(());
    }
    let node = dag.get_node(step.base_id()).expect("resolved node exists");
    for (dim, value) in &step.partition.0 {
        let crate::PartitionSpec::DerivedFrom { source_ref } = &node.extracted.partitions[dim]
        else {
            continue;
        };
        let source = dag
            .partition_sources(step.base_id())
            .into_iter()
            .find(|id| short_name(id) == source_ref.resolution_name())
            .ok_or_else(|| {
                BarcaError::Usage(format!("cannot resolve the partition source for {dim}"))
            })?;
        let reader = db::CacheReader::for_config(cfg).await?;
        let pins = db::pins_on(reader.conn()).await?;
        let output = match pins.get(source) {
            Some(pin) => db::success_at(&reader, source, &pin.run_hash).await,
            None => db::latest_success(&reader, source)
                .await
                .map(|row| row.output),
        };
        drop(reader);
        let output = output.ok_or_else(|| {
            BarcaError::Usage(format!(
                "partition keys are unknown: run {} first",
                short_name(source)
            ))
        })?;
        let cmd = crate::helper_proc::python_module(
            python,
            "barca._value",
            cfg.storage_options_json.as_deref(),
        );
        let answers = crate::store_inspect::run_json_helper(
            cmd,
            python,
            &serde_json::json!({
                "artifacts": [{"path": output.path, "format": output.format}]
            }),
        )
        .await
        .map_err(|e| BarcaError::Usage(format!("cannot read partition membership: {e}")))?;
        let source_value = answers
            .first()
            .and_then(|a| a.get("value"))
            .ok_or_else(|| {
                BarcaError::Usage("cannot read partition membership from its source".into())
            })?;
        let known = match source_value {
            serde_json::Value::Object(values) => values.contains_key(value),
            serde_json::Value::Array(values) => values.iter().any(|v| {
                v.as_str().map_or_else(
                    || {
                        v.as_number().is_some_and(|number| {
                            value
                                .parse::<serde_json::Number>()
                                .is_ok_and(|parsed| parsed == *number)
                        })
                    },
                    |v| v == value,
                )
            }),
            _ => false,
        };
        if !known {
            return Err(BarcaError::Usage(format!(
                "unknown partition value {dim}={value}"
            )));
        }
    }
    Ok(())
}

/// Pin `name` to data an operator supplies: the bytes are checked by loading them as runs do,
/// written as a new artifact of the asset (run hash [`upload_run_hash`]), recorded as a
/// materialization marked as an upload, and pinned. `db_path` must exist (initialized).
pub async fn pin_upload(
    cfg: &crate::config::ResolvedConfig,
    python: &std::path::Path,
    dag: &Dag,
    name: &str,
    supplied: Supplied,
    reason: Option<String>,
    by: Option<String>,
) -> Result<Pinned, BarcaError> {
    let (id, ok) = pinnable(dag, name)?;
    if !ok {
        return Err(refuse(&id));
    }
    check_runtime_key(cfg, python, dag, &id).await?;
    let short = short_name(&id).to_string();
    if supplied.bytes.is_empty() {
        return Err(BarcaError::Usage(format!(
            "'{}' is empty: there is nothing to pin '{short}' to",
            supplied.file_name
        )));
    }
    if supplied.bytes.len() > MAX_UPLOAD_BYTES {
        return Err(BarcaError::Usage(format!(
            "'{}' is {} bytes, more than the {} an upload may be",
            supplied.file_name,
            supplied.bytes.len(),
            crate::gc::size::human(MAX_UPLOAD_BYTES as u64)
        )));
    }
    let format = upload_format(&supplied.file_name, supplied.format.as_deref())?;
    let run_hash = upload_run_hash(&supplied.bytes);
    let ext = if format == "pickle" { "pkl" } else { &format };
    let dir = std::path::Path::new(&cfg.local_artifact_dir).join(crate::safe_node_id(&id));
    let dest = dir.join(format!("{run_hash}.{ext}"));
    let io = |e: std::io::Error| {
        BarcaError::Io(std::io::Error::new(
            e.kind(),
            format!("could not write the upload to {}: {e}", dest.display()),
        ))
    };
    std::fs::create_dir_all(&dir).map_err(io)?;
    let tmp = dir.join(format!(".{run_hash}.upload.tmp"));
    std::fs::write(&tmp, &supplied.bytes).map_err(io)?;
    let tmp_str = tmp.display().to_string();
    if let Err(why) = check_loads(python, cfg, &tmp_str, &format).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(BarcaError::Usage(format!(
            "'{}' does not load as {format}: {why}\nNothing was pinned.",
            supplied.file_name
        )));
    }
    std::fs::rename(&tmp, &dest).map_err(io)?;
    let upload = db::Upload {
        uploaded_at: crate::time::rfc3339(chrono::Utc::now().timestamp()),
        uploaded_by: by.clone(),
        file_name: Some(supplied.file_name.clone()).filter(|n| !n.is_empty()),
        reason: reason.clone().filter(|r| !r.trim().is_empty()),
    };
    db::record_upload(
        &cfg.db_path,
        &db::UploadedArtifact {
            node_id: id.clone(),
            run_hash: run_hash.clone(),
            path: dest.display().to_string(),
            format,
            size_bytes: supplied.bytes.len() as u64,
            upload,
        },
    )
    .await?;
    pin(
        &cfg.db_path,
        dag,
        name,
        &Choice::RunHash(run_hash),
        reason,
        by,
    )
    .await
}

/// Remove `name`'s pin. Unpinning an asset that is not pinned changes nothing.
pub async fn unpin(db_path: &str, dag: &Dag, name: &str) -> Result<Unpinned, BarcaError> {
    // A pin of a node that is no longer defined is removed by its node id.
    let id = match pinnable(dag, name) {
        Ok((id, _)) => id,
        Err(e) => match db::get_pins(db_path)
            .await?
            .into_iter()
            .find(|p| p.node_id == name)
        {
            Some(p) => p.node_id,
            None => return Err(e),
        },
    };
    let removed = db::clear_pin(db_path, &id).await?;
    Ok(Unpinned {
        node_id: id,
        removed,
    })
}

/// Remove work that is only read by held partition keys. Unknown memberships are left to
/// runtime expansion; never guess a key set from a historical materialization.
pub(crate) fn prune_partition_inputs(
    dag: &Dag,
    plan: &mut crate::planner::ExecutionPlan,
    targets: &[&str],
    pins: &[AssetPin],
) {
    use std::collections::{HashMap, HashSet};
    let held: HashSet<&str> = pins.iter().map(|p| p.node_id.as_str()).collect();
    if held.is_empty()
        || plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .any(|s| !s.pending_partitions.is_empty())
    {
        return;
    }
    let mut members: HashMap<String, HashSet<String>> = HashMap::new();
    for step in plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
    {
        let set = members.entry(step.step_id.base.to_string()).or_default();
        if step.partition_keys.is_empty() {
            set.insert(step.step_id.display());
        } else {
            set.extend(
                step.partition_keys
                    .iter()
                    .map(|k| k.display_id(step.step_id.base_id())),
            );
        }
    }
    let mut needed: HashMap<String, HashSet<String>> = targets
        .iter()
        .filter_map(|id| members.get(*id).map(|keys| (id.to_string(), keys.clone())))
        .collect();
    for id in dag.topo_order().into_iter().rev() {
        let Some(keys) = needed.get(id).cloned() else {
            continue;
        };
        let node = dag.get_node(id).expect("topological node exists");
        for display in keys {
            if held.contains(display.as_str()) {
                continue;
            }
            let key = crate::StepId::parse(&display).partition;
            for up in dag.upstream(id) {
                let Some(all) = members.get(up) else { continue };
                let upstream = dag.get_node(up).expect("input node exists");
                let dims = &upstream.extracted.partitions;
                let wanted = needed.entry(up.to_string()).or_default();
                if dims.is_empty() {
                    wanted.insert(up.to_string());
                } else if !node.resolved_collected.values().any(|v| v == up)
                    && dims.keys().all(|dim| key.0.contains_key(dim))
                {
                    let projected = crate::PartitionKey(
                        dims.keys()
                            .map(|dim| (dim.clone(), key.0[dim].clone()))
                            .collect(),
                    );
                    let projected = projected.display_id(up);
                    if all.contains(&projected) {
                        wanted.insert(projected);
                    }
                } else {
                    wanted.extend(all.iter().cloned());
                }
            }
        }
    }
    for phase in &mut plan.phases {
        for stream in &mut phase.streams {
            stream.steps.retain_mut(|step| {
                let Some(wanted) = needed.get(step.step_id.base_id()) else {
                    return false;
                };
                if step.partition_keys.is_empty() {
                    return wanted.contains(&step.step_id.display());
                }
                step.partition_keys
                    .retain(|k| wanted.contains(&k.display_id(step.step_id.base_id())));
                if step.partition_keys.is_empty() {
                    return false;
                }
                if step
                    .partition_keys
                    .iter()
                    .all(|k| held.contains(k.display_id(step.step_id.base_id()).as_str()))
                {
                    step.inputs.clear();
                }
                true
            });
        }
        phase.streams.retain(|s| !s.steps.is_empty());
    }
    plan.phases.retain(|p| !p.streams.is_empty());
    plan.total_steps = plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .map(|s| s.partition_keys.len().max(1))
        .sum();
}

/// Prune expanded runtime keys whose every path to a requested target stops at a pin.
/// Membership sources still run: a pin does not invent or retain a removed partition key.
pub(crate) fn prune_expanded_inputs(
    dag: &Dag,
    phase: &mut crate::planner::Phase,
    targets: &[&str],
    pins: &[AssetPin],
) {
    use std::collections::{HashMap, HashSet};
    struct Demand<'a> {
        dag: &'a Dag,
        targets: HashSet<&'a str>,
        held: HashSet<&'a str>,
        memo: HashMap<String, bool>,
        relevant: HashSet<String>,
    }
    impl Demand<'_> {
        fn needed(&mut self, id: &str, key: &crate::PartitionKey) -> bool {
            let display = key.display_id(id);
            if let Some(needed) = self.memo.get(&display) {
                return *needed;
            }
            let needed = self.targets.contains(id)
                || self.dag.downstream(id).into_iter().any(|down| {
                    if !self.relevant.contains(down) {
                        return false;
                    }
                    let node = self.dag.get_node(down).expect("downstream node exists");
                    // A dimension not carried by this input may fan out to unpinned keys.
                    if !node
                        .extracted
                        .partitions
                        .keys()
                        .all(|dim| key.0.contains_key(dim))
                    {
                        return true;
                    }
                    let projected = crate::PartitionKey(
                        node.extracted
                            .partitions
                            .keys()
                            .map(|dim| (dim.clone(), key.0[dim].clone()))
                            .collect(),
                    );
                    !self.held.contains(projected.display_id(down).as_str())
                        && self.needed(down, &projected)
                });
            self.memo.insert(display, needed);
            needed
        }
    }
    if pins.is_empty() {
        return;
    }
    let mut relevant = HashSet::new();
    let mut pending: Vec<String> = targets.iter().map(|id| id.to_string()).collect();
    while let Some(id) = pending.pop() {
        if relevant.insert(id.clone()) {
            pending.extend(dag.upstream(&id).into_iter().map(str::to_string));
        }
    }
    let mut demand = Demand {
        relevant,
        dag,
        targets: targets.iter().copied().collect(),
        held: pins.iter().map(|p| p.node_id.as_str()).collect(),
        memo: HashMap::new(),
    };
    for stream in &mut phase.streams {
        stream.steps.retain_mut(|step| {
            if step.partition_keys.is_empty() {
                return true;
            }
            let base = step.step_id.base_id();
            step.partition_keys.retain(|key| demand.needed(base, key));
            if step.partition_keys.is_empty() {
                return false;
            }
            if step
                .partition_keys
                .iter()
                .all(|key| demand.held.contains(key.display_id(base).as_str()))
            {
                step.inputs.clear();
            }
            true
        });
    }
    phase.streams.retain(|stream| !stream.steps.is_empty());
}
