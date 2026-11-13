//! What one step produced, or how it failed: the artifact reference a result points at and the
//! failure a worker reports. Values only, shared by the engine, the metadata DB and the results.

use serde::{Deserialize, Serialize};

/// Reference to a materialized artifact on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct OutputRef {
    pub path: String,
    pub format: String,
    // serde_json emits u64/i64 as JSON numbers; map to `number`, not bigint.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub elapsed_seconds: Option<f64>,
    /// SHA-256 of the artifact's bytes, when one was recorded. Never sent to
    /// workers or printed: it is what a local copy is checked against.
    #[serde(default, skip_serializing)]
    pub content_hash: Option<String>,
}

/// Where a step failed, as its worker reported it (the `phase` of a step error). Only
/// [`ErrorPhase::UserCode`] is the step's own code; the others are barca's own work around it,
/// whose failure keeps barca's frames in the traceback (#664).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPhase {
    /// Reading the step's input artifacts (a `collect()` fan-in included).
    LoadInputs,
    /// Importing the step's module, calling its function and checking what it returned.
    UserCode,
    /// Dividing a batched call's result into its keys.
    SplitOutput,
    /// Writing the result to its artifact.
    WriteOutput,
    /// Writing the step's `@sink` files.
    Sink,
}

impl ErrorPhase {
    /// Every phase, in the order a step goes through them.
    pub const ALL: [ErrorPhase; 5] = [
        ErrorPhase::LoadInputs,
        ErrorPhase::UserCode,
        ErrorPhase::SplitOutput,
        ErrorPhase::WriteOutput,
        ErrorPhase::Sink,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorPhase::LoadInputs => "load_inputs",
            ErrorPhase::UserCode => "user_code",
            ErrorPhase::SplitOutput => "split_output",
            ErrorPhase::WriteOutput => "write_output",
            ErrorPhase::Sink => "sink",
        }
    }

    /// The phase named `name`; `None` for a name this barca does not know.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == name)
    }

    /// Whether the failure is in barca's own code rather than the step's.
    pub const fn in_barca(self) -> bool {
        !matches!(self, ErrorPhase::UserCode)
    }

    /// What barca was doing, for a failure in its own code: `loading its inputs`.
    pub const fn doing(self) -> &'static str {
        match self {
            ErrorPhase::LoadInputs => "loading its inputs",
            ErrorPhase::UserCode => "running it",
            ErrorPhase::SplitOutput => "splitting the batched call's result into its keys",
            ErrorPhase::WriteOutput => "writing its result",
            ErrorPhase::Sink => "writing its sinks",
        }
    }
}

/// A structured failure reported by a worker for a single step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepError {
    pub error_type: String,
    pub message: String,
    /// User-relevant traceback (barca-internal frames filtered out).
    pub traceback: String,
    /// Where the step failed, when a worker reported it (`None` for a worker that died, or a
    /// failure barca recorded without running the step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ErrorPhase>,
    /// Number of attempts made before this failure became permanent.
    pub attempts: u32,
    /// When the first attempt started, as Unix nanoseconds. `None` when the failure was not
    /// timed (an artifact upload that failed after the step itself succeeded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix_ns: Option<u64>,
    /// Wall seconds from the first attempt's start to the permanent failure, retries and
    /// backoff included. A step that timed out reports about its timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
}

/// A permanently-failed step (retry budget exhausted), for the metadata DB.
#[derive(Debug, Clone, PartialEq)]
pub struct StepFailure {
    pub node_id: String,
    pub error: StepError,
}
