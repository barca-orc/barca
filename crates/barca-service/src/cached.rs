//! Read the latest completed results, independently of execution and publication.
use crate::{
    BarcaError,
    config::ResolvedConfig,
    model::{PartitionKey, PartitionSpec, PartitionValue},
};
use barca_db::db;
pub use barca_model::step_result::OutputRef;
use barca_store::transfer::{ArtifactLayout, TransferClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Store,
    Local,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    pub tier: Tier,
    pub partition: Option<String>,
    #[serde(default)]
    pub values: bool,
    pub url_expires: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    pub outcomes: Vec<Value>,
    /// Ordinary-get presentation counts; never part of the cached-only wire contract.
    #[serde(skip)]
    pub summaries: BTreeMap<String, usize>,
    pub final_output: Option<Value>,
    pub path_scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::envelope::ErrorEnvelope>,
}
impl Answer {
    pub fn check(&self) -> Result<(), BarcaError> {
        if self.outcomes.iter().any(|o| o["status"] == "miss") {
            Err(BarcaError::CacheMiss(self.outcomes.clone()))
        } else {
            Ok(())
        }
    }
}

/// Literal domains can include never-run keys. Unknown domains use recorded keys, unless
/// the caller explicitly supplies their coordinates. No expression is evaluated here.
fn keys(
    dims: &HashMap<String, PartitionSpec>,
    selection: Option<&crate::selection::PartitionSelection>,
    recorded: impl Iterator<Item = PartitionKey>,
) -> Result<Vec<PartitionKey>, BarcaError> {
    if let Some(s) = selection
        && let Some(dim) = s.dimensions.keys().find(|d| !dims.contains_key(*d))
    {
        return Err(BarcaError::Usage(format!(
            "--partition: target has no dimension '{dim}'"
        )));
    }
    if let Some(exact) = selection.and_then(|s| s.exact_keys())
        && exact
            .first()
            .is_some_and(|k| dims.keys().all(|d| k.contains_key(d)))
    {
        return Ok(exact.iter().cloned().map(PartitionKey).collect());
    }
    if dims.is_empty() {
        return Ok(vec![PartitionKey::empty()]);
    }
    let known: Vec<_> = recorded.collect();
    let mut domains = HashMap::new();
    for (dim, spec) in dims {
        let values = match selection.and_then(|s| s.dimensions.get(dim)) {
            Some(v) => v.iter().cloned().collect(),
            None => match spec {
                PartitionSpec::Static { values } => values
                    .iter()
                    .map(|v| match v {
                        PartitionValue::Str(s) => s.clone(),
                        PartitionValue::Int(n) => n.to_string(),
                    })
                    .collect(),
                _ => known
                    .iter()
                    .filter_map(|k| k.0.get(dim).cloned())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            },
        };
        domains.insert(dim.clone(), values);
    }
    let mut out: Vec<_> = crate::planner::expand_partition_combos(&domains)
        .into_iter()
        .map(|k| PartitionKey(k.into_iter().collect()))
        .filter(|k| selection.is_none_or(|s| s.matches(k)))
        .collect();
    // Unknown dimensions must not invent combinations not recorded unless explicitly selected.
    out.retain(|k| {
        dims.iter().all(|(d, spec)| {
            matches!(spec, PartitionSpec::Static { .. })
                || selection.is_some_and(|s| s.dimensions.contains_key(d))
                || known.iter().any(|v| v == k)
        })
    });
    if out.is_empty()
        && dims
            .values()
            .any(|s| !matches!(s, PartitionSpec::Static { .. }))
    {
        return Err(BarcaError::Usage("cached-only cannot discover this target's keys without executing code; select explicit coordinates with --partition".into()));
    }
    out.sort_by_key(PartitionKey::suffix);
    Ok(out)
}

pub async fn get(cfg: &ResolvedConfig, python: &Path, req: &Request) -> Result<Answer, BarcaError> {
    Ok(read(cfg, python, req, false, &[])
        .await?
        .expect("latest read always answers")
        .0)
}

/// Ordinary get may reuse only a plan that needs no execution, under request rules.
/// None means admission must perform the ordinary get, including its error handling.
pub async fn fresh(
    cfg: &ResolvedConfig,
    python: &Path,
    req: &Request,
    batch: &[String],
) -> Result<Option<(Answer, crate::results::ExplainResult)>, BarcaError> {
    read(cfg, python, req, true, batch).await
}

async fn read(
    cfg: &ResolvedConfig,
    python: &Path,
    req: &Request,
    fresh_only: bool,
    batch: &[String],
) -> Result<Option<(Answer, crate::results::ExplainResult)>, BarcaError> {
    if req.tier == Tier::Local && req.url_expires.is_some() {
        return Err(BarcaError::Usage(
            "--cached-only=local cannot sign URLs (signing contacts the store)".into(),
        ));
    }
    if req.url_expires.is_some_and(|s| s == 0 || s > 604800) {
        return Err(BarcaError::Usage(
            "URL lifetime must be between 1 and 604800 seconds".into(),
        ));
    }
    let protection = barca_store::gc::pin_at("cached-read", &cfg.project_root);
    let mut cfg = cfg.clone();
    cfg.ledger = None;
    let dag = crate::load::build_static_dag(&req.files).await?;
    if fresh_only {
        crate::batch::BatchOverride::parse(batch)
            .map_err(BarcaError::Usage)?
            .check(&dag)
            .map_err(BarcaError::Usage)?;
    }
    let mut targets = crate::targets::resolve_targets(&dag, &req.targets, "get")?;
    if targets.is_empty() {
        targets = dag
            .topo_order()
            .into_iter()
            .filter(|id| !crate::targets::is_task(&dag, id))
            .map(|id| (id.to_string(), id.to_string()))
            .collect();
    }
    let selection = req
        .partition
        .as_deref()
        .map(crate::selection::PartitionSelection::parse)
        .transpose()
        .map_err(BarcaError::Usage)?;
    let read = crate::db::read_metadata(&cfg, false).await?;
    // Prediction is static and has no ledger attached. Sensors' live values are unknown.
    let mut expected_hashes = HashMap::new();
    let (predicted, members, will_run, _) = barca_decide::predict::explain_dag_with_hashes(
        &dag,
        read.cfg(),
        &req.targets,
        crate::cache::CachePolicy::CacheAware,
        if fresh_only { "get" } else { "status" },
        if fresh_only { selection.as_ref() } else { None },
        &if fresh_only {
            crate::freshness::RunRules::request()
        } else {
            crate::freshness::RunRules::status()
        },
        &req.files,
        Some(&mut expected_hashes),
    )
    .await?;
    if fresh_only
        && (predicted.steps.is_empty()
            || predicted.steps.iter().any(|step| {
                step.action.as_deref() != Some(crate::step_status::StepStatus::Cached.as_str())
                    || dag.get_node(&step.id).is_none_or(|node| {
                        node.decl
                            .partitions
                            .values()
                            .any(|spec| !matches!(spec, PartitionSpec::Static { .. }))
                    })
            }))
    {
        return Ok(None);
    }
    if fresh_only && req.targets.is_empty() {
        let plan = crate::targets::plan_for_targets(
            &dag,
            &[],
            &crate::planner::ResourceConfig {
                pool_size: crate::sizing::sizing_for(&cfg, None).pool_size,
                concurrency_groups: HashMap::new(),
            },
            "get",
            selection.as_ref(),
        );
        let Some(last) = crate::planner::returned_step(&plan) else {
            return Ok(None);
        };
        let id = last.step_id.base_id().to_string();
        targets = vec![(id.clone(), id)];
    }
    let pending = barca_db::pending_uploads::list(&read.cfg().db_path).await?;
    let cache = if Path::new(&read.cfg().db_path).exists() {
        Some(db::CacheReader::for_config(read.cfg()).await?)
    } else {
        None
    };
    let mut selected = Vec::new();
    for (name, id) in targets {
        let rows = match &cache {
            Some(c) if !fresh_only => db::materializations::cached_successes(c, &id).await?,
            Some(_) => BTreeMap::new(),
            None => BTreeMap::new(),
        };
        let node = dag.get_node(&id).expect("resolved node");
        for key in keys(
            &node.decl.partitions,
            selection.as_ref(),
            rows.keys().map(|id| crate::StepId::parse(id).partition),
        )? {
            let display = key.display_id(&id);
            let hash_matches = rows
                .get(&display)
                .is_some_and(|row| expected_hashes.get(&display) == Some(&row.run_hash));
            let fresh = hash_matches
                && if key.is_empty() {
                    predicted
                        .steps
                        .iter()
                        .find(|s| s.id == id)
                        .is_some_and(|s| {
                            s.action.as_deref()
                                == Some(crate::step_status::StepStatus::Cached.as_str())
                        })
                } else {
                    members.get(&id).is_some_and(|m| m.contains(&display))
                        && !will_run.get(&id).is_some_and(|m| m.contains(&display))
                };
            selected.push((
                name.clone(),
                id.clone(),
                key,
                fresh,
                rows.get(&display).cloned(),
            ));
        }
    }
    if fresh_only && selected.is_empty() {
        return Ok(None);
    }
    let mut summaries = BTreeMap::new();
    if fresh_only && req.partition.is_none() {
        for (name, _, key, _, _) in &selected {
            if !key.is_empty() {
                *summaries.entry(name.clone()).or_insert(0) += 1;
            }
        }
        summaries.retain(|_, count| *count > crate::results::LISTED_KEYS);
    }
    protection.nodes(selected.iter().map(|(_, node, _, _, _)| node.clone()));
    protection.run_hashes(
        selected
            .iter()
            .filter_map(|(_, _, _, _, row)| row.as_ref().map(|r| &r.run_hash)),
    );
    drop(read);
    let local_root = std::fs::canonicalize(&cfg.local_artifact_dir)
        .unwrap_or(std::env::current_dir()?.join(&cfg.local_artifact_dir));
    let layout = ArtifactLayout::new(&local_root, &cfg.artifact_root);
    let mut answer = Answer {
        outcomes: vec![],
        summaries,
        final_output: None,
        path_scope: "local".into(),
        error: None,
    };
    let mut values: BTreeMap<String, Vec<(Option<String>, Value)>> = BTreeMap::new();
    let mut reads = Vec::new();
    let mut selected = selected.into_iter();
    loop {
        // Artifact metadata and retained inodes are traversed independently of the report
        // limit. Explicit selections can return every key without unbounded I/O fan-out.
        let page: Vec<_> = selected.by_ref().take(128).collect();
        if page.is_empty() {
            break;
        }
        let ids: Vec<_> = page
            .iter()
            .map(|(_, node, key, _, _)| key.display_id(node))
            .collect();
        let mut rows = if fresh_only {
            let Some(cache) = &cache else {
                return Ok(None);
            };
            db::materializations::cached_page(cache, &ids).await?
        } else {
            BTreeMap::new()
        };
        protection.run_hashes(rows.values().map(|r| &r.run_hash));
        for (name, node, key, mut fresh, mut row) in page {
            if fresh_only {
                let display = key.display_id(&node);
                row = rows.remove(&display);
                fresh = row
                    .as_ref()
                    .is_some_and(|r| expected_hashes.get(&display) == Some(&r.run_hash));
                if !fresh {
                    return Ok(None);
                }
            }
            let summarized = answer.summaries.contains_key(&name);
            let partition = (!key.is_empty()).then(|| key.suffix());
            let mut outcome =
                json!({"node":node,"partition":partition,"status":"miss","reason":"never_run"});
            let Some(row) = row else {
                answer.outcomes.push(outcome);
                continue;
            };
            let stored = row.output.path.clone();
            if crate::artifact_format::format_of(&stored) != row.output.format {
                return Err(BarcaError::Other(format!(
                    "cached artifact corruption: invalid format for {node}"
                )));
            }
            let local = if cfg.remote_artifacts() {
                layout.local_for(&stored).or_else(|| {
                    Path::new(&stored)
                        .starts_with(&local_root)
                        .then(|| PathBuf::from(&stored))
                })
            } else {
                Some(PathBuf::from(&stored))
            }
            .ok_or_else(|| {
                BarcaError::Other(format!(
                    "cached artifact is outside this environment: {stored}"
                ))
            })?;
            let absolute = if local.is_absolute() {
                local
            } else {
                std::env::current_dir()?.join(local)
            };
            if !barca_model::config::local_artifact(
                &absolute.to_string_lossy(),
                &cfg.local_artifact_dir,
            ) {
                return Err(BarcaError::Other(format!(
                    "cached artifact is outside this environment: {}",
                    absolute.display()
                )));
            }
            let mut source = "disk";
            let exists = match std::fs::metadata(&absolute) {
                Ok(m) if m.is_file() => true,
                Ok(_) => {
                    return Err(BarcaError::Other(
                        "cached artifact is not a regular file".into(),
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            };
            if !exists {
                outcome["reason"] = json!("evicted");
                if cfg.remote_artifacts()
                    && layout.local_for(&stored).is_some()
                    && !pending.iter().any(|p| p.store_path == stored)
                {
                    if req.tier == Tier::Local {
                        outcome["reason"] = json!("not_fetched");
                    } else {
                        let mut transfer = TransferClient::deferred(
                            python,
                            &cfg,
                            &format!(
                                "cached-{}-{}",
                                std::process::id(),
                                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
                            ),
                        )?;
                        let path =
                            transfer.fetch(&node, &stored, row.output.content_hash.as_deref());
                        let report = transfer
                            .await_fetches(&path.into_iter().collect::<Vec<_>>())
                            .await;
                        transfer.shutdown().await;
                        if !report.mismatched.is_empty() {
                            return Err(BarcaError::Other(format!(
                                "cached artifact corruption: hash mismatch for {node}"
                            )));
                        }
                        if let Some(f) = report.failures.first() {
                            if !f.missing {
                                return Err(BarcaError::Other(format!(
                                    "cached artifact store read failed: {}",
                                    f.message
                                )));
                            }
                        } else {
                            source = "store";
                            db::materializations::touch_read(
                                &cfg.db_path,
                                std::slice::from_ref(&stored),
                            )
                            .await;
                        }
                    }
                }
                if source != "store" {
                    answer.outcomes.push(outcome);
                    continue;
                }
            }
            // Hard-link the completed inode before checking or reading. Atomic replacement and
            // GC can now remove the original name without changing the bytes this call reads.
            let staging = cfg
                .project_root
                .join(".barca/staging")
                .join(std::process::id().to_string());
            std::fs::create_dir_all(&staging)?;
            let snapshot = tempfile::Builder::new()
                .prefix("cached-")
                .tempdir_in(&staging)?;
            let path = snapshot.path().join(
                absolute
                    .file_name()
                    .ok_or_else(|| BarcaError::Other("invalid artifact path".into()))?,
            );
            if let Ok(real) = absolute.canonicalize()
                && !real.starts_with(&local_root)
            {
                return Err(BarcaError::Other(
                    "cached artifact resolves outside this environment".into(),
                ));
            }
            match retain_inode(&absolute, &path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    outcome["reason"] = json!("evicted");
                    answer.outcomes.push(outcome);
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            let meta = std::fs::symlink_metadata(&path)?;
            if !meta.is_file() || meta.len() != row.output.size_bytes {
                return Err(BarcaError::Other(format!(
                    "cached artifact corruption: invalid type or size for {node}"
                )));
            }
            if let Some(hash) = &row.output.content_hash
                && crate::hash::file_sha256(&path)? != *hash
            {
                return Err(BarcaError::Other(format!(
                    "cached artifact corruption: hash mismatch for {node}"
                )));
            }
            let mut output = row.output.clone();
            output.path = path.to_string_lossy().into_owned();
            let read_value = if req.values && !summarized {
                Some(crate::values::read_cached_value(python, &cfg, &output).await?)
            } else {
                None
            };
            // Active reads stay in process-owned staging, protected regardless of duration.
            // Only the finished reference enters the ordinary cache's finite grace period.
            if summarized && values.contains_key(&name) {
                reads.push(stored);
                continue;
            }
            let references = cfg.project_root.join(".barca/sql-cache");
            std::fs::create_dir_all(&references)?;
            let reference = tempfile::Builder::new()
                .prefix("cached-")
                .tempdir_in(&references)?;
            let reference_path = reference
                .path()
                .join(path.file_name().expect("artifact file name"));
            std::fs::write(
                format!("{}.version", reference_path.display()),
                b"cached-read",
            )?;
            retain_inode(&path, &reference_path)?;
            output.path = reference_path.to_string_lossy().into_owned();
            let value = read_value.unwrap_or_else(|| crate::report::artifact_metadata(&output));
            let mut value = value;
            if let Some(expires) = req.url_expires {
                let signed = crate::values::sign_urls(
                    python,
                    &cfg,
                    &[crate::values::SignRequest {
                        uri: stored.clone(),
                        filename: absolute.file_name().unwrap().to_string_lossy().into_owned(),
                    }],
                    expires,
                )
                .await
                .remove(0)
                .map_err(|e| {
                    BarcaError::Other(format!("cached artifact signing failed: {}", e.message))
                })?;
                value = json!({"_barca_artifact":{"path":output.path,"format":output.format,"size_bytes":output.size_bytes,"url":signed.url,"url_expires_at":signed.expires_at}});
            }
            let _ = reference.keep();
            reads.push(stored);
            outcome = json!({"node":node,"partition":partition,"status":"hit","run_hash":row.run_hash,
            "format":output.format,"size_bytes":output.size_bytes,"path":output.path,"fresh":fresh,"source":source});
            if let Some(url) = value.get("_barca_artifact").and_then(|v| v.get("url")) {
                outcome["url"] = url.clone();
            }
            values.entry(name).or_default().push((partition, value));
            answer.outcomes.push(outcome);
        }
        db::materializations::touch_read(&cfg.db_path, &reads).await;
        reads.clear();
    }
    let outputs: BTreeMap<String, Value> = values
        .into_iter()
        .map(|(name, items)| {
            let v = if let Some(count) = answer.summaries.get(&name) {
                crate::report::pointer_output(
                    None,
                    None,
                    Some(&crate::results::KeySummary {
                        keys: *count,
                        computed: 0,
                        listed: vec![],
                    }),
                )
                .unwrap()
            } else if items.iter().any(|(k, _)| k.is_some()) {
                let entries: BTreeMap<_, _> = items
                    .into_iter()
                    .map(|(k, v)| (k.unwrap_or_default(), v))
                    .collect();
                json!(entries)
            } else {
                items
                    .into_iter()
                    .next()
                    .map(|(_, v)| v)
                    .unwrap_or(Value::Null)
            };
            (name, v)
        })
        .collect();
    answer.final_output = if outputs.len() == 1 {
        outputs.into_values().next()
    } else {
        Some(json!(outputs))
    };
    if let Err(error) = answer.check() {
        answer.error = Some(crate::envelope::ErrorEnvelope::from_barca(
            error,
            &crate::envelope::Context {
                command: "get",
                files: req.files.clone(),
            },
        ));
    }
    if fresh_only && answer.error.is_some() {
        return Ok(None);
    }
    Ok(Some((answer, predicted)))
}

/// Retain the completed inode cheaply where possible. Across mounts, copy from one open
/// handle into private staging; replacement/unlink of the source name cannot switch bytes.
fn retain_inode(source: &Path, dest: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(source, dest) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            let mut input = std::fs::File::open(source)?;
            if !input.metadata()?.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "artifact is not a regular file",
                ));
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)?;
            std::io::copy(&mut input, &mut output)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_explicit_coordinates_do_not_expand_a_cross_product() {
        let exact: Vec<_> = (0..1000).map(|i| json!({"a":i,"b":i})).collect();
        let selection =
            crate::selection::PartitionSelection::parse(&json!(exact).to_string()).unwrap();
        let dims = ["a", "b"]
            .into_iter()
            .map(|d| {
                (
                    d.into(),
                    PartitionSpec::Dynamic {
                        source_text: "must_not_execute()".into(),
                    },
                )
            })
            .collect();
        let selected = keys(&dims, Some(&selection), std::iter::empty()).unwrap();
        assert_eq!(selected.len(), 1000);
        assert!(selected.iter().all(|k| k.0["a"] == k.0["b"]));
    }
}
