//! Why a DAG could not be built: the error `dag` returns and the crate's error wraps.

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("cycle detected in dependency graph")]
    CycleDetected,

    #[error("input '{param}' on node '{node}' references unknown upstream '{upstream}'")]
    UnresolvedInput {
        node: String,
        param: String,
        upstream: String,
    },

    #[error(
        "input '{param}' on node '{node}' is ambiguous: '{upstream}' is not defined in that \
         file, and more than one file defines it ({candidates}). Import it from the file you \
         mean (`from <module> import {upstream}`) or name it with asset_ref(\"<file>:{upstream}\")"
    )]
    AmbiguousInput {
        node: String,
        param: String,
        upstream: String,
        candidates: String,
    },

    #[error(
        "input '{param}' on node '{node}': '{name}' is imported from {module} ({file}), but no \
         @asset/@task/@sensor named '{name}' is defined there"
    )]
    UnresolvedImport {
        node: String,
        param: String,
        name: String,
        module: String,
        file: String,
    },

    #[error(
        "task '{task}' cannot be an input to {downstream_kind} '{downstream}' \
         (tasks are never cached, so this would poison caching)"
    )]
    TaskAsInput {
        task: String,
        downstream: String,
        downstream_kind: &'static str,
    },

    #[error("duplicate continuity key: '{key}' defined in both '{first}' and '{second}'")]
    DuplicateKey {
        key: String,
        first: String,
        second: String,
    },

    #[error("sensor '{sensor}' cannot have inputs")]
    SensorWithInputs { sensor: String },

    /// `inputs={"x": upstream}` where `upstream` is partitioned and the consumer is not (#189).
    /// This used to pass every partition as a list, a second spelling of `collect(upstream)`.
    #[error(
        "input '{param}' on '{node}' reads partitioned asset '{upstream}', but '{node}' is not \
         partitioned"
    )]
    PartitionedInputToUnpartitioned {
        node: String,
        param: String,
        upstream: String,
    },

    /// `partitions_from(upstream)` on a partitioned upstream that the consumer cannot mirror.
    #[error("'{node}': partitions_from({upstream}) {problem}")]
    PartitionsFrom {
        node: String,
        upstream: String,
        problem: String,
        fix: String,
    },
}

impl DagError {
    /// The fix, when this error has one more specific than "check each node's inputs".
    pub fn remediation(&self) -> Option<String> {
        match self {
            DagError::PartitionedInputToUnpartitioned {
                param, upstream, ..
            } => {
                let name = upstream.rsplit(':').next().unwrap_or(upstream);
                Some(format!(
                    "Use `inputs={{\"{param}\": collect({name})}}` to receive every partition of \
                     '{name}' as one list, or `partitions={{\"<key>\": partitions_from({name})}}` \
                     to run once per partition of '{name}' with that partition's output."
                ))
            }
            DagError::PartitionsFrom { fix, .. } => Some(fix.clone()),
            _ => None,
        }
    }
}
