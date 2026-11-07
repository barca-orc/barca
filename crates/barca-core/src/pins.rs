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
    /// Whether it can be pinned: an asset without partitions.
    pub pinnable: bool,
    pub pin: Option<AssetPin>,
    pub versions: Vec<Version>,
}

/// The node `name` names, checked to be one that can be pinned.
fn pinnable(dag: &Dag, name: &str) -> Result<(String, bool), BarcaError> {
    let id = find_target_id(dag, name)?;
    let ok = dag
        .get_node(&id)
        .is_some_and(|n| n.kind() == crate::NodeKind::Asset && n.extracted.partitions.is_empty());
    Ok((id, ok))
}

fn refuse(id: &str) -> BarcaError {
    BarcaError::Usage(format!(
        "'{}' cannot be pinned: only an asset without partitions can be pinned\nTasks, sensors \
         and partitioned assets are not supported.",
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

/// Remove `name`'s pin. Unpinning an asset that is not pinned changes nothing.
pub async fn unpin(db_path: &str, dag: &Dag, name: &str) -> Result<Unpinned, BarcaError> {
    // A pin of a node that is no longer defined is removed by its node id.
    let id = match find_target_id(dag, name) {
        Ok(id) => id,
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
