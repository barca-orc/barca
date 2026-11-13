//! [`NodeEstimate`], one node's measured cost: what [`crate::cost`] updates on every task
//! completion and the metadata DB keeps between runs (`cost_estimates`).

/// One node's running estimate.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeEstimate {
    /// Current EWMA of per-task wall cost, seconds.
    pub estimate_seconds: f64,
    /// Last observed CPU seconds (informational; persisted for `barca stats`).
    pub cpu_seconds: f64,
    /// Last observed peak RSS in bytes (informational).
    pub max_rss_bytes: u64,
    /// Observations folded into this estimate (including seeded history).
    pub samples: u64,
}
