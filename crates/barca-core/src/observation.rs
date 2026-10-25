//! A sensor's freshness revision is independent of its optional payload digest.
//! Explicit change signals advance the revision even when no data is returned.
use sha2::{Digest, Sha256};

pub(crate) fn revision(
    changed: Option<bool>,
    content_hash: &str,
    previous: Option<&str>,
    run_id: &str,
    node_id: &str,
) -> String {
    match changed {
        Some(false) => previous.unwrap_or(content_hash).to_owned(),
        Some(true) => {
            let bytes = serde_json::to_vec(&("sensor-observation-v1", run_id, node_id)).unwrap();
            format!("{:x}", Sha256::digest(bytes))
        }
        None => content_hash.to_owned(),
    }
}

pub(crate) fn from_artifact(
    artifact: &serde_json::Value,
    previous: Option<&String>,
    run_id: &str,
    node_id: &str,
) -> Option<String> {
    let changed = artifact.get("sensor_changed")?.as_bool()?;
    Some(revision(
        Some(changed),
        artifact.get("content_hash")?.as_str()?,
        previous.map(String::as_str),
        run_id,
        node_id,
    ))
}

pub(crate) async fn previous(
    cache: &crate::db::CacheReader,
    node_id: &str,
) -> Result<Option<(String, crate::dispatch::OutputRef)>, crate::BarcaError> {
    let fail = |e| crate::BarcaError::Db(format!("failed to read sensor observation: {e}"));
    let mut rows = cache.conn().query(
        "SELECT COALESCE(observation_hash, output_hash), artifact_path, artifact_format, artifact_size_bytes, output_hash FROM materializations INDEXED BY idx_mat_node_run WHERE node_id = ?1 AND status = 'success' AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1",
        [node_id.to_owned()],
    ).await.map_err(fail)?;
    let Some(row) = rows.next().await.map_err(fail)? else {
        return Ok(None);
    };
    let Ok(revision) = row.get::<String>(0) else {
        return Ok(None);
    };
    Ok(Some((
        revision,
        crate::dispatch::OutputRef {
            path: row.get::<String>(1).map_err(fail)?,
            format: row.get::<String>(2).map_err(fail)?,
            size_bytes: row.get::<i64>(3).unwrap_or(0).max(0) as u64,
            elapsed_seconds: None,
            content_hash: row.get::<String>(4).ok(),
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::revision;

    #[test]
    fn change_signals_are_independent_of_payloads_and_idempotent_within_a_run() {
        let first = revision(Some(true), "null", None, "run1", "source[a]");
        assert_eq!(
            revision(Some(false), "different", Some(&first), "run2", "source[a]"),
            first
        );
        let next = revision(Some(true), "null", Some(&first), "run2", "source[a]");
        assert_ne!(next, first);
        assert_eq!(
            next,
            revision(Some(true), "null", Some(&next), "run2", "source[a]")
        );
        assert_ne!(
            next,
            revision(Some(true), "null", Some(&first), "run2", "source[b]")
        );
        assert_eq!(
            revision(None, "payload", Some(&first), "run2", "source[a]"),
            "payload"
        );
    }
}
