//! `barca status` against the artifact store: which cached results to ask the store about, and
//! what the answer does to a status row. The questions themselves (`python -m barca._state
//! stat`, `python -m barca._inspect`) and their answers are `store_inspect`'s, which knows
//! nothing of a status row.

use super::NodeStatus;
use barca_decide::cache::{CacheReader, lookup_cached};
use barca_model::cache_state::CacheState;
use barca_store::store_inspect::{self, Copies};
use std::collections::HashSet;
use std::path::Path;

/// For every `cached` unpartitioned node: compare this machine's copy, the store's object (one
/// `python -m barca._state stat` for all of them) and the recorded digest, and turn the node
/// `unsynced` (`local_only`, `store_only`, `mismatch`) or `stale` (`artifact_missing`) when they
/// do not agree. A store that cannot be asked leaves the states as they are, with a note.
pub(crate) async fn check_store_copies(
    python: &Path,
    cfg: &barca_model::config::ResolvedConfig,
    nodes: &mut [NodeStatus],
) -> HashSet<usize> {
    let mut verified_local = HashSet::new();
    let layout =
        barca_store::transfer::ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root);
    let Ok(cache) = CacheReader::for_config(cfg).await else {
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
        let Some(row) = lookup_cached(&cache, &n.id, hash).await else {
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
    let evidence = match store_inspect::inspect_copies(python, cfg, &paths, &local_paths).await {
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
        let copies = store_inspect::classify(
            here.as_ref()
                .map(|local| (local.size_bytes, local.sha256.as_str())),
            &object,
            recorded.as_deref(),
        );
        let at = barca_store::transfer::diagnostic_uri(&store);
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

/// Fill `shape` for every node whose last materialization produced an artifact, with one call
/// to `python -m barca._inspect`. A remote artifact is read through the same fsspec filesystem
/// the workers use, so the reader gets the same storage options. A failure to run the reader is
/// reported in each shape's `note` rather than failing the command.
/// `fields`: also JSON object field and list element types (`read_schemas`).
pub(crate) async fn read_shapes(
    python: &Path,
    cfg: &barca_model::config::ResolvedConfig,
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
        CacheReader::for_config(cfg).await.ok()
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
            let Some(selected) = lookup_cached(cache, &n.id, hash).await else {
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
            let layout = barca_store::transfer::ArtifactLayout::new(
                &cfg.local_artifact_dir,
                &cfg.artifact_root,
            );
            let local = layout.local_for(path).filter(|path| path.is_file());
            wanted.push((
                i,
                local.map_or_else(|| path.clone(), |path| path.to_string_lossy().into_owned()),
                m.format.clone().unwrap_or_default(),
            ));
        } else if n.cache.state == CacheState::Unsynced && n.cache.reason != "store_only" {
            // Not in the store, or not the same there (#390, RFC-0009): read this machine's
            // copy. A `store_only` result is read from the store like a cached one.
            let layout = barca_store::transfer::ArtifactLayout::new(
                &cfg.local_artifact_dir,
                &cfg.artifact_root,
            );
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
    let shapes = store_inspect::artifact_shapes(python, cfg, &artifacts, sample, fields).await;
    for ((i, _, _), shape) in wanted.into_iter().zip(shapes) {
        nodes[i].shape = Some(shape);
    }
}
