//! What is known about a cached result's copies and bytes: this machine's copy against the
//! store's object and the recorded digest (`python -m barca._state stat`, the same answer the
//! server's ledger checks a post with), and the shape of an artifact read through
//! `python -m barca._inspect`. `barca status` and the ledger consume it.

use crate::cache_state::CacheState;
use crate::db;
use crate::status::NodeStatus;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

/// Where a cached result's copies are, against what was recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Copies {
    /// This machine's copy, the store's object and the recorded digest agree.
    InSync,
    /// Here, not in the store.
    LocalOnly,
    /// In the store, not here.
    StoreOnly,
    /// Both are there and differ (or this machine's copy is not the recorded one): why.
    Mismatch(String),
    /// Neither copy is anywhere.
    Missing,
}

/// What the store said about one object (`python -m barca._state stat`). The server's ledger
/// checks a post's objects with the same answer.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StoreObject {
    pub exists: bool,
    pub size_bytes: Option<u64>,
    /// Only for a store that is a plain directory: object stores give no content checksum to
    /// compare.
    pub sha256: Option<String>,
}

/// Classify a result's copies. `local` is this machine's copy (size, SHA-256) when there is
/// one; `recorded` the digest its row was recorded with. Without a checksum from the store,
/// copies of the same size are taken to agree.
pub(crate) fn classify(
    local: Option<(u64, &str)>,
    store: &StoreObject,
    recorded: Option<&str>,
) -> Copies {
    match (local, store.exists) {
        (None, false) => Copies::Missing,
        (None, true) => Copies::StoreOnly,
        (Some(_), false) => Copies::LocalOnly,
        (Some((size, sha)), true) => {
            if recorded.is_some_and(|r| r != sha) {
                Copies::Mismatch(
                    "this machine's copy is not the one recorded; get fetches the store's copy and records \
                     it"
                        .to_string(),
                )
            } else if store.size_bytes.is_some_and(|s| s != size) {
                Copies::Mismatch(format!(
                    "this machine's copy ({size} bytes) and the store's ({} bytes) differ; get \
                     keeps this machine's copy and uploads it again",
                    store.size_bytes.unwrap_or_default()
                ))
            } else if store.sha256.as_deref().is_some_and(|s| s != sha) {
                Copies::Mismatch(
                    "this machine's copy and the store's have different bytes; get keeps this \
                     machine's copy and uploads it again"
                        .to_string(),
                )
            } else {
                Copies::InSync
            }
        }
    }
}

/// For every `cached` unpartitioned node: compare this machine's copy, the store's object (one
/// `python -m barca._state stat` for all of them) and the recorded digest, and turn the node
/// `unsynced` (`local_only`, `store_only`, `mismatch`) or `stale` (`artifact_missing`) when they
/// do not agree. A store that cannot be asked leaves the states as they are, with a note.
pub(crate) async fn check_store_copies(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
) -> HashSet<usize> {
    let mut verified_local = HashSet::new();
    let layout = crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
    let Ok(cache) = db::CacheReader::for_config(cfg).await else {
        return verified_local;
    };
    // (node index, store path, local path, recorded digest)
    let mut wanted: Vec<(usize, String, std::path::PathBuf, Option<String>)> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.cache.state != CacheState::Cached || n.partitioned {
            continue;
        }
        let Some(hash) = n.cache.run_hash.as_deref() else {
            continue;
        };
        let Some(row) = crate::cache::lookup_cached(&cache, &n.id, hash).await else {
            continue;
        };
        let Some(local) = layout.local_for(&row.path) else {
            continue;
        };
        wanted.push((i, row.path, local, row.content_hash));
    }
    drop(cache);
    if wanted.is_empty() {
        return verified_local;
    }
    let paths: Vec<&str> = wanted.iter().map(|(_, p, _, _)| p.as_str()).collect();
    let local_paths: Vec<_> = wanted
        .iter()
        .map(|(_, _, local, _)| local.to_string_lossy().into_owned())
        .collect();
    let evidence = match inspect_copies(python, cfg, &paths, &local_paths).await {
        Ok(evidence)
            if evidence.objects.len() == wanted.len() && evidence.locals.len() == wanted.len() =>
        {
            evidence
        }
        Ok(_) => return verified_local,
        Err(why) => {
            for (i, ..) in &wanted {
                let c = &mut nodes[*i].cache;
                c.detail = format!(
                    "{} (the artifact store could not be checked: {why})",
                    c.detail
                );
            }
            return verified_local;
        }
    };
    for (((i, store, _, recorded), object), here) in wanted
        .into_iter()
        .zip(evidence.objects)
        .zip(evidence.locals)
    {
        let copies = classify(
            here.as_ref()
                .map(|local| (local.size_bytes, local.sha256.as_str())),
            &object,
            recorded.as_deref(),
        );
        let at = crate::transfer::diagnostic_uri(&store);
        let c = &mut nodes[i].cache;
        let (state, reason, detail) = match copies {
            Copies::InSync => {
                verified_local.insert(i);
                continue;
            }
            Copies::LocalOnly => (
                CacheState::Unsynced,
                "local_only",
                format!(
                    "computed here and not in the artifact store ({at}); the next get uploads it"
                ),
            ),
            Copies::StoreOnly => (
                CacheState::Unsynced,
                "store_only",
                format!("in the artifact store ({at}), not on this machine; get fetches it"),
            ),
            Copies::Mismatch(why) => (CacheState::Unsynced, "mismatch", format!("{why} ({at})")),
            Copies::Missing => (
                CacheState::Stale,
                "artifact_missing",
                format!(
                    "the artifact is neither on this machine nor in the store ({at}); a run computes it again"
                ),
            ),
        };
        c.state = state;
        c.reason = reason.to_string();
        c.detail = detail;
        if state != CacheState::Cached && state != CacheState::Unsynced {
            c.artifact = None;
        }
    }
    verified_local
}

/// Disposable status evidence; publication and transfer checks remain strict.
#[derive(Deserialize)]
struct CopyEvidence {
    objects: Vec<StoreObject>,
    locals: Vec<Option<LocalEvidence>>,
}
#[derive(Deserialize)]
struct LocalEvidence {
    size_bytes: u64,
    sha256: String,
}
async fn inspect_copies(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    paths: &[&str],
    locals: &[String],
) -> Result<CopyEvidence, String> {
    let mut command = crate::backup::state_helper(python, cfg);
    command.arg("stat").kill_on_drop(true);
    let request = serde_json::json!({"paths": paths, "locals": locals, "inspection_cache": cfg.inspection_cache_path()});
    let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(60));
    use crate::helper_proc::JsonFailed;
    match crate::helper_proc::run_json(command, &request, Some(limit)).await {
        Ok(reply) => Ok(reply),
        Err(JsonFailed::Spawn(error) | JsonFailed::Io(error)) => Err(error.to_string()),
        Err(JsonFailed::TimedOut(limit)) => Err(format!("no answer within {}s", limit.as_secs())),
        Err(JsonFailed::Exited(stderr)) => Err(crate::helper_proc::helper_cause(&stderr)),
        Err(JsonFailed::Invalid(error)) => Err(error.to_string()),
    }
}

/// Ask the store about each of `paths` with one `python -m barca._state stat`.
pub(crate) async fn stat_objects(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    paths: &[&str],
) -> Result<Vec<StoreObject>, String> {
    use crate::helper_proc::JsonFailed;
    #[derive(Deserialize)]
    struct Reply {
        objects: Vec<StoreObject>,
    }
    let mut cmd = crate::backup::state_helper(python, cfg);
    cmd.arg("stat").kill_on_drop(true);
    let request = serde_json::json!({ "paths": paths });
    let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(60));
    match crate::helper_proc::run_json::<Reply>(cmd, &request, Some(limit)).await {
        Ok(reply) => Ok(reply.objects),
        Err(JsonFailed::Spawn(e)) => Err(format!("{}: {e}", python.display())),
        Err(JsonFailed::Io(e)) => Err(e.to_string()),
        Err(JsonFailed::TimedOut(limit)) => Err(format!("no answer within {}s", limit.as_secs())),
        Err(JsonFailed::Exited(stderr)) => Err(crate::helper_proc::helper_cause(&stderr)),
        Err(JsonFailed::Invalid(e)) => Err(e.to_string()),
    }
}

/// Fill `shape` for every node whose last materialization produced an artifact, with one call
/// to `python -m barca._inspect`. A remote artifact is read through the same fsspec filesystem
/// the workers use, so the reader gets the same storage options. A failure to run the reader is
/// reported in each shape's `note` rather than failing the command.
/// `fields`: also JSON object field and list element types (`read_schemas`).
pub(crate) async fn read_shapes(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
    sample: usize,
    fields: bool,
    verified_local: &HashSet<usize>,
) {
    // A remote-off cache hit can select an older local row than the historical latest
    // materialization. Keep the selected artifact's metadata together rather than read
    // its bytes with the latest row's (possibly different) format.
    let cache = if cfg.remote_off
        && nodes.iter().any(|n| {
            n.cache.artifact.as_ref().is_some_and(|path| {
                n.last_materialization
                    .as_ref()
                    .and_then(|m| m.artifact.as_ref())
                    != Some(path)
            })
        }) {
        db::CacheReader::for_config(cfg).await.ok()
    } else {
        None
    };
    let mut wanted: Vec<(usize, String, String)> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let Some(m) = n.last_materialization.as_ref() else {
            continue;
        };
        if m.status != "success" {
            continue;
        }
        let path = if cfg.remote_off {
            n.cache.artifact.as_ref().or(m.artifact.as_ref())
        } else {
            m.artifact.as_ref()
        };
        let Some(path) = path.filter(|path| cfg.allows_artifact(path)) else {
            continue;
        };
        if cfg.remote_off && m.artifact.as_ref() != Some(path) {
            let (Some(cache), Some(hash)) = (&cache, &n.cache.run_hash) else {
                continue;
            };
            let Some(selected) = crate::cache::lookup_cached(cache, &n.id, hash).await else {
                continue;
            };
            // A concurrent write may have changed which result would now be selected.
            // Do not attach unrelated metadata to the previously reported artifact.
            if selected.path == *path {
                wanted.push((i, selected.path, selected.format));
            }
        } else if verified_local.contains(&i)
            && n.cache.state == CacheState::Cached
            && n.cache.artifact.as_deref() == Some(path.as_str())
        {
            // Use the selected, verified local copy instead of reopening remote payloads.
            let layout =
                crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
            let local = layout.local_for(path).filter(|path| path.is_file());
            wanted.push((
                i,
                local.map_or_else(|| path.clone(), |path| path.to_string_lossy().into_owned()),
                m.format.clone().unwrap_or_default(),
            ));
        } else if n.cache.state == CacheState::Unsynced && n.cache.reason != "store_only" {
            // Not in the store, or not the same there (#390, RFC-0009): read this machine's
            // copy. A `store_only` result is read from the store like a cached one.
            let layout =
                crate::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
            let local = layout
                .local_for(path)
                .map_or_else(|| path.clone(), |p| p.to_string_lossy().into_owned());
            wanted.push((i, local, m.format.clone().unwrap_or_default()));
        } else {
            wanted.push((i, path.clone(), m.format.clone().unwrap_or_default()));
        }
    }
    // The inspector only reads artifact files; release the database before launching it.
    drop(cache);
    if wanted.is_empty() {
        return;
    }
    let artifacts: Vec<(String, String)> = wanted
        .iter()
        .map(|(_, path, format)| (path.clone(), format.clone()))
        .collect();
    let shapes = artifact_shapes(python, cfg, &artifacts, sample, fields).await;
    for ((i, _, _), shape) in wanted.into_iter().zip(shapes) {
        nodes[i].shape = Some(shape);
    }
}

/// The shape of each artifact (`(path, format)`), as `barca status` reads it: one per artifact,
/// a `{note}` where it could not be read.
pub(crate) async fn artifact_shapes(
    python: &Path,
    cfg: &crate::config::ResolvedConfig,
    artifacts: &[(String, String)],
    sample: usize,
    fields: bool,
) -> Vec<serde_json::Value> {
    if artifacts.is_empty() {
        return Vec::new();
    }
    let request = serde_json::json!({
        "sample": sample,
        "fields": fields,
        "inspection_cache": cfg.inspection_cache_path(),
        "artifacts": artifacts
            .iter()
            .map(|(path, format)| serde_json::json!({"path": path, "format": format}))
            .collect::<Vec<_>>(),
    });
    let inspector = crate::helper_proc::python_module(
        python,
        "barca._inspect",
        cfg.storage_options_json.as_deref(),
    );
    match run_json_helper(inspector, python, &request).await {
        Ok(shapes) if shapes.len() == artifacts.len() => shapes,
        Ok(_) => vec![note("the shape reader returned an unexpected result"); artifacts.len()],
        Err(e) => vec![note(&format!("could not run the shape reader: {e}")); artifacts.len()],
    }
}

fn note(msg: &str) -> serde_json::Value {
    serde_json::json!({ "note": msg })
}

/// Run a Python helper that reads one JSON document on stdin and answers with one JSON array
/// on stdout (`barca._inspect`, `barca._value`). Its last stderr line is the error. `cmd` is
/// built by `helper_proc::python_module`, so it carries the resolved storage options.
pub(crate) async fn run_json_helper(
    cmd: tokio::process::Command,
    python: &Path,
    request: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    use crate::helper_proc::JsonFailed;
    crate::helper_proc::run_json(cmd, request, None)
        .await
        .map_err(|failed| match failed {
            JsonFailed::Spawn(e) => format!("{}: {e}", python.display()),
            JsonFailed::Io(e) => e.to_string(),
            JsonFailed::TimedOut(limit) => format!("no answer within {}s", limit.as_secs()),
            JsonFailed::Exited(stderr) => crate::helper_proc::last_line(&stderr),
            JsonFailed::Invalid(e) => e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_here_in_the_store_and_on_record() {
        let there = |size: u64, sha: Option<&str>| StoreObject {
            exists: true,
            size_bytes: Some(size),
            sha256: sha.map(str::to_string),
        };
        let gone = StoreObject::default();
        assert_eq!(classify(None, &gone, Some("d")), Copies::Missing);
        assert_eq!(
            classify(None, &there(3, None), Some("d")),
            Copies::StoreOnly
        );
        assert_eq!(
            classify(Some((3, "d")), &gone, Some("d")),
            Copies::LocalOnly
        );
        // Agree: same size (an object store has no checksum to compare) or same digest.
        assert_eq!(
            classify(Some((3, "d")), &there(3, None), Some("d")),
            Copies::InSync
        );
        assert_eq!(
            classify(Some((3, "d")), &there(3, Some("d")), None),
            Copies::InSync
        );
        // Differ: by size, by digest where the store gives one, or this copy is not the
        // recorded one.
        for (local, store, recorded) in [
            ((3, "d"), there(4, None), Some("d")),
            ((3, "d"), there(3, Some("e")), Some("d")),
            ((3, "e"), there(3, None), Some("d")),
        ] {
            assert!(
                matches!(classify(Some(local), &store, recorded), Copies::Mismatch(_)),
                "{local:?} {store:?} {recorded:?}"
            );
        }
    }
}
