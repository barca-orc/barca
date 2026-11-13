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
