//! The execution request retained with history, independently of queue lifetime.

use serde::{Deserialize, Serialize};

/// Inputs that determine a run's scope. Replay supplies a new identity and acceptance time;
/// it never reuses the original principal, scheduled slot, or refresh cutoff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunScope {
    pub command: String,
    pub targets: Vec<String>,
    pub files: Vec<String>,
    pub refresh: Vec<String>,
    pub refresh_all: bool,
    pub cascade: bool,
    /// Canonical JSON accepted by the partition selector; absent means all keys.
    pub partition: Option<String>,
    pub batch: Vec<String>,
    pub pin_manual: bool,
    pub hold_paused: bool,
    pub allow_paused_targets: bool,
    pub tick: Option<String>,
    pub values: bool,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub timeout: Option<u64>,
    pub queue: String,
}
