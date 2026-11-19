//! What is known about a cached result's copies and bytes: this machine's copy against the
//! store's object and the recorded digest (`python -m barca._state stat`, the same answer the
//! server's ledger checks a post with), and the shape of an artifact read through
//! `python -m barca._inspect`. `barca status` and the ledger consume it.
//!
//! This module answers with facts about paths ([`CopyEvidence`], [`Copies`], shapes). Which
//! results to ask about, and what a finding does to a status row, is `barca status`'s
//! (`status::store_check`), so nothing here names a status row or the cache reader.

use serde::Deserialize;
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

/// Disposable status evidence; publication and transfer checks remain strict.
#[derive(Deserialize)]
pub(crate) struct CopyEvidence {
    pub(crate) objects: Vec<StoreObject>,
    pub(crate) locals: Vec<Option<LocalEvidence>>,
}
#[derive(Deserialize)]
pub(crate) struct LocalEvidence {
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}
/// What the store holds at each of `paths` and this machine at each of `locals`, with one
/// `python -m barca._state stat`.
pub(crate) async fn inspect_copies(
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
