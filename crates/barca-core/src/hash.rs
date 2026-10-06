//! Hashing — content-addressed identity for nodes and materializations.
//!
//! Hash protocol:
//! - definition_hash: identity of the code (function source + deps + metadata)
//! - run_hash: identity of a specific execution (definition + inputs + partition)

use sha2::{Digest, Sha256};

/// Protocol version — bump when hash computation changes.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// Compute the definition hash for a node.
///
/// Inputs:
/// - function source text
/// - dependency cone source (helpers, constants)
/// - decorator metadata (freshness, partitions, inputs declarations)
/// - protocol version
pub fn definition_hash(function_source: &str, cone_source: &str, metadata_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"definition:");
    hasher.update(PROTOCOL_VERSION.as_bytes());
    hasher.update(b"\n");
    hasher.update(function_source.as_bytes());
    hasher.update(b"\n");
    hasher.update(cone_source.as_bytes());
    hasher.update(b"\n");
    hasher.update(metadata_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Compute the run hash for a materialization.
///
/// Inputs:
/// - definition_hash of this node
/// - partition key (if any)
/// - upstream materialization IDs (sorted for determinism)
/// - ad-hoc params (if any)
/// - declared env values (if any; see [`crate::envdeps::hash_input`])
///
/// `None` sections contribute nothing, so a node without params or declared env hashes exactly
/// as it did before those sections existed.
pub fn run_hash(
    definition_hash: &str,
    partition_key: Option<&str>,
    upstream_ids: &[&str],
    params: Option<&str>,
    env: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"run:");
    hasher.update(PROTOCOL_VERSION.as_bytes());
    hasher.update(b"\n");
    hasher.update(definition_hash.as_bytes());
    hasher.update(b"\n");

    if let Some(pk) = partition_key {
        hasher.update(b"partition:");
        hasher.update(pk.as_bytes());
        hasher.update(b"\n");
    }

    // Sort upstream IDs for deterministic hashing.
    let mut sorted_upstream: Vec<&str> = upstream_ids.to_vec();
    sorted_upstream.sort();
    for id in sorted_upstream {
        hasher.update(b"upstream:");
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }

    if let Some(p) = params {
        hasher.update(b"params:");
        hasher.update(p.as_bytes());
        hasher.update(b"\n");
    }

    if let Some(e) = env {
        hasher.update(b"env:");
        hasher.update(e.as_bytes());
        hasher.update(b"\n");
    }

    format!("{:x}", hasher.finalize())
}

/// Compute a hash of partition source identity.
///
/// For static partitions: hash of the sorted partition values.
/// For derived partitions: the materialization ID of the source asset.
pub fn partition_source_hash(values_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"partition_source:");
    hasher.update(PROTOCOL_VERSION.as_bytes());
    hasher.update(b"\n");
    hasher.update(values_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_definition_hash_deterministic() {
        let h1 = definition_hash("def foo(): pass", "# no deps", "{}");
        let h2 = definition_hash("def foo(): pass", "# no deps", "{}");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_definition_hash_changes_with_source() {
        let h1 = definition_hash("def foo(): return 1", "", "{}");
        let h2 = definition_hash("def foo(): return 2", "", "{}");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_run_hash_with_partition() {
        let def_h = definition_hash("def foo(): pass", "", "{}");
        let h1 = run_hash(&def_h, Some("AAPL"), &[], None, None);
        let h2 = run_hash(&def_h, Some("MSFT"), &[], None, None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_run_hash_upstream_order_independent() {
        let def_h = definition_hash("def foo(): pass", "", "{}");
        let h1 = run_hash(&def_h, None, &["a", "b", "c"], None, None);
        let h2 = run_hash(&def_h, None, &["c", "a", "b"], None, None);
        assert_eq!(h1, h2); // sorted internally
    }
}
