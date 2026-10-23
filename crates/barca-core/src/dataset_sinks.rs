//! Deleting the keys a dataset sink's asset no longer has.
//!
//! A dataset sink (`@sink("exports/scored/")`, `barca docs sinks`) mirrors its asset: each key
//! is a directory, written by the worker when the key runs (`barca/_sinks.py`). A key that
//! disappears from the asset never runs again, so no worker deletes it. The run collects every
//! key of each partitioned asset with a dataset sink as its phase is expanded, and after the
//! steps have run `python -m barca._sinks prune` deletes the directories of the other keys.
//!
//! A run that narrows the asset (`--partition`) does not know its whole key set and prunes
//! nothing of it. The prune records a digest of the key set in the dataset's
//! `_barca_sink.json`; a local dataset whose recorded digest matches is left alone without
//! starting the helper (workers only ever write keys of the current set, so nothing can need
//! deleting).

use std::collections::{BTreeSet, HashMap};

use sha2::{Digest, Sha256};

use crate::model::SinkDecl;
use crate::planner::Phase;

/// The marker a dataset sink keeps at its prefix (`barca/_sinks.py`).
pub(crate) const MARKER: &str = "_barca_sink.json";

/// One asset's dataset sinks and every key the run knows of it, values in dimension order.
#[derive(Debug, Default)]
pub(crate) struct DatasetKeys {
    sinks: Vec<SinkDecl>,
    keys: BTreeSet<Vec<String>>,
}

/// Add the keys of `phase` (expanded, not yet filtered by the cache) for each partitioned
/// asset with a dataset sink. `narrowed` names the assets a `--partition` selection narrowed.
pub(crate) fn collect(
    datasets: &mut HashMap<String, DatasetKeys>,
    phase: &Phase,
    narrowed: impl Fn(&str) -> bool,
) {
    for step in phase.streams.iter().flat_map(|s| &s.steps) {
        if step.partition_keys.is_empty() {
            continue;
        }
        let sinks: Vec<&SinkDecl> = step
            .sinks
            .iter()
            .filter(|s| s.is_dataset() && !s.dims.is_empty())
            .collect();
        let base = step.step_id.base_id();
        if sinks.is_empty() || narrowed(base) {
            continue;
        }
        let entry = datasets.entry(base.to_string()).or_default();
        if entry.sinks.is_empty() {
            entry.sinks = sinks.into_iter().cloned().collect();
        }
        let dims = &entry.sinks[0].dims;
        for key in &step.partition_keys {
            let values: Option<Vec<String>> = dims.iter().map(|d| key.0.get(d).cloned()).collect();
            if let Some(values) = values {
                entry.keys.insert(values);
            }
        }
    }
}

/// What the prune records of a key set: SHA-256 over the sorted JSON arrays of its values.
pub(crate) fn keys_digest(keys: &BTreeSet<Vec<String>>) -> String {
    let mut lines: Vec<String> = keys
        .iter()
        .map(|k| serde_json::to_string(k).unwrap_or_default())
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// The local directory of a dataset prefix, or `None` for an object store.
fn local_prefix(path: &str) -> Option<std::path::PathBuf> {
    let trimmed = path.trim_end_matches('/');
    if let Some(local) = trimmed.strip_prefix("file://") {
        return Some(local.into());
    }
    (!trimmed.contains("://")).then(|| trimmed.into())
}

/// Whether a local dataset was last pruned to exactly this key set by this asset, or does not
/// exist (nothing to delete either way).
fn up_to_date(path: &str, asset: &str, digest: &str) -> bool {
    let Some(dir) = local_prefix(path) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(dir.join(MARKER)) else {
        return !dir.exists();
    };
    let Ok(marker) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    marker.get("asset").and_then(|v| v.as_str()) == Some(asset)
        && marker.get("keys_digest").and_then(|v| v.as_str()) == Some(digest)
}

#[derive(Debug, serde::Deserialize)]
struct PruneAnswer {
    datasets: Vec<PruneOutcome>,
}

#[derive(Debug, serde::Deserialize)]
struct PruneOutcome {
    path: String,
    #[serde(default)]
    removed: u64,
    #[serde(default)]
    error: Option<String>,
}

/// The prune request for the datasets that may hold removed keys.
pub(crate) fn request(datasets: HashMap<String, DatasetKeys>) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut assets: Vec<_> = datasets.into_iter().collect();
    assets.sort_by(|a, b| a.0.cmp(&b.0));
    for (asset, ds) in assets {
        if ds.keys.is_empty() {
            continue;
        }
        let digest = keys_digest(&ds.keys);
        for sink in &ds.sinks {
            if up_to_date(&sink.path, &asset, &digest) {
                continue;
            }
            out.push(serde_json::json!({
                "path": sink.path,
                "asset": asset,
                "dims": sink.dims,
                "keys": ds.keys,
                "digest": digest,
            }));
        }
    }
    out
}

/// Delete the removed keys of every dataset in `datasets`. Returns the lines to show: a
/// deletion, or a warning when a dataset could not be pruned (the run's steps succeeded, so a
/// failed prune does not fail the run).
pub(crate) async fn prune(
    datasets: HashMap<String, DatasetKeys>,
    python: &std::path::Path,
    storage_options: Option<&str>,
) -> Vec<String> {
    let request = request(datasets);
    if request.is_empty() {
        return Vec::new();
    }
    let mut cmd = crate::helper_proc::python_module(python, "barca._sinks", storage_options);
    cmd.arg("prune").kill_on_drop(true);
    let answer = crate::helper_proc::run_json::<PruneAnswer>(
        cmd,
        &serde_json::json!({ "datasets": request }),
        None,
    )
    .await;
    let answer = match answer {
        Ok(answer) => answer,
        Err(e) => {
            let why = match e {
                crate::helper_proc::JsonFailed::Spawn(e)
                | crate::helper_proc::JsonFailed::Io(e) => e.to_string(),
                crate::helper_proc::JsonFailed::TimedOut(limit) => {
                    format!("it did not finish within {}s", limit.as_secs())
                }
                crate::helper_proc::JsonFailed::Exited(stderr) => {
                    crate::helper_proc::last_line(&stderr)
                }
                crate::helper_proc::JsonFailed::Invalid(e) => e.to_string(),
            };
            return vec![format!(
                "[barca] warning: dataset sinks: the keys the assets no longer have were not \
                 deleted: {why}"
            )];
        }
    };
    answer
        .datasets
        .into_iter()
        .filter_map(|ds| match ds.error {
            Some(error) => Some(format!(
                "[barca] warning: dataset sink {}: the keys its asset no longer has were not \
                 deleted: {error}",
                ds.path
            )),
            None if ds.removed > 0 => Some(format!(
                "[barca] dataset sink {}: deleted {} {} its asset no longer has",
                ds.path,
                ds.removed,
                if ds.removed == 1 { "key" } else { "keys" }
            )),
            None => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PartitionKey;
    use crate::planner::{StreamStep, WorkerStream};

    fn sink(path: &str, dims: &[&str]) -> SinkDecl {
        SinkDecl {
            path: path.to_string(),
            dims: dims.iter().map(|d| d.to_string()).collect(),
            ..Default::default()
        }
    }

    fn phase(base: &str, sinks: Vec<SinkDecl>, keys: &[&[(&str, &str)]]) -> Phase {
        let step = StreamStep {
            step_id: crate::StepId::unpartitioned(base),
            kind: crate::model::NodeKind::Asset,
            function_name: std::sync::Arc::from("f"),
            source_file: std::sync::Arc::from("p.py"),
            inputs: HashMap::new(),
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks,
            run_hashes: HashMap::new(),
            timeout_seconds: 0,
            retries: 1,
            weight: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: keys
                .iter()
                .map(|k| {
                    PartitionKey(
                        k.iter()
                            .map(|(d, v)| (d.to_string(), v.to_string()))
                            .collect(),
                    )
                })
                .collect(),
            param_types: HashMap::new(),
            return_type: None,
            keyed: Default::default(),
        };
        Phase {
            reason: crate::planner::PhaseReason::Initial,
            streams: vec![WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![step],
            }],
        }
    }

    #[test]
    fn keys_are_collected_in_dimension_order_for_dataset_sinks_only() {
        let mut datasets = HashMap::new();
        let keys: &[&[(&str, &str)]] = &[
            &[("region", "eu"), ("retailer", "r1")],
            &[("region", "us"), ("retailer", "r2")],
        ];
        collect(
            &mut datasets,
            &phase("p.py:a", vec![sink("out/", &["retailer", "region"])], keys),
            |_| false,
        );
        collect(
            &mut datasets,
            &phase("p.py:b", vec![sink("b.parquet", &[])], keys),
            |_| false,
        );
        collect(
            &mut datasets,
            &phase("p.py:c", vec![sink("c/", &["retailer", "region"])], keys),
            |base| base == "p.py:c",
        );
        assert_eq!(datasets.len(), 1, "{datasets:?}");
        let a = &datasets["p.py:a"];
        assert_eq!(
            a.keys.iter().cloned().collect::<Vec<_>>(),
            [vec!["r1", "eu"], vec!["r2", "us"]]
        );
    }

    #[test]
    fn the_digest_does_not_depend_on_order() {
        let a: BTreeSet<Vec<String>> = [vec!["r1".into()], vec!["r2".into()]].into();
        let b: BTreeSet<Vec<String>> = [vec!["r2".into()], vec!["r1".into()]].into();
        assert_eq!(keys_digest(&a), keys_digest(&b));
        let c: BTreeSet<Vec<String>> = [vec!["r1".into()]].into();
        assert_ne!(keys_digest(&a), keys_digest(&c));
    }

    #[test]
    fn a_local_dataset_pruned_to_the_same_keys_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = format!("{}/scored/", dir.path().display());
        let mut datasets = HashMap::new();
        collect(
            &mut datasets,
            &phase("p.py:a", vec![sink(&prefix, &["k"])], &[&[("k", "1")]]),
            |_| false,
        );
        let digest = keys_digest(&datasets["p.py:a"].keys);
        // No dataset yet: nothing to delete.
        assert!(up_to_date(&prefix, "p.py:a", &digest));
        std::fs::create_dir_all(dir.path().join("scored")).unwrap();
        std::fs::write(
            dir.path().join("scored").join(MARKER),
            serde_json::json!({"asset": "p.py:a"}).to_string(),
        )
        .unwrap();
        assert_eq!(request(std::mem::take(&mut datasets)).len(), 1);
        std::fs::write(
            dir.path().join("scored").join(MARKER),
            serde_json::json!({"asset": "p.py:a", "keys_digest": digest}).to_string(),
        )
        .unwrap();
        assert!(up_to_date(&prefix, "p.py:a", &digest));
        assert!(!up_to_date(&prefix, "p.py:other", &digest));
        assert!(!up_to_date("s3://b/scored/", "p.py:a", &digest));
    }
}
