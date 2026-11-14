//! The body of a ledger post (`POST /ledger/runs`) and the answer to it: a finished run and its
//! step rows as the posting machine recorded them. Plain data: `barca_core::ledger` sends and
//! accepts them, and the metadata DB records them (`ledger_posts`, barca-db's `recording`).

use serde::{Deserialize, Serialize};

/// The run row of a ledger post: a finished run on the machine that posts it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerRun {
    /// The run id on the posting machine. The server records it as `client_run_id` and
    /// answers a repeated post of the same run with the run it already recorded.
    pub run_id: String,
    pub command: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub target: Option<String>,
    pub status: String,
    #[serde(default)]
    pub steps_total: Option<i64>,
    #[serde(default)]
    pub steps_executed: i64,
    #[serde(default)]
    pub steps_cached: i64,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
    /// The posting machine's host name.
    #[serde(default)]
    pub host: Option<String>,
}

/// One step row of a ledger post: a materialization the run recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerStep {
    pub node_id: String,
    pub run_hash: String,
    /// `success` or `failed`.
    pub status: String,
    /// The artifact's location in the store; required for a successful step.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// SHA-256 of the artifact's bytes; required for a successful step.
    #[serde(default)]
    pub digest: Option<String>,
    /// Sensor freshness revision; independent of the artifact digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_hash: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
    #[serde(default)]
    pub attempts: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The body of `POST /ledger/runs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerPost {
    pub run: LedgerRun,
    pub steps: Vec<LedgerStep>,
    /// The base ids of the steps whose new bytes under an unchanged run hash are expected, not
    /// a sign of a nondeterministic function (`barca_core::cache::checks_bytes`): a `Manual`
    /// asset. Taken from the graph when the post is built and kept with it in `ledger_posts`,
    /// so a post sent later, when no graph is at hand (#570), warns for the same steps. Never
    /// sent to the server.
    #[serde(skip)]
    pub bytes_unchecked: Vec<String>,
}

impl LedgerPost {
    /// The steps of `bytes_changed` (the server's answer to this post) that get the
    /// non-determinism warning: those whose node promises the same bytes for the same code and
    /// inputs.
    pub fn warns_for(&self, bytes_changed: Vec<String>) -> Vec<String> {
        bytes_changed
            .into_iter()
            .filter(|node| {
                let base = crate::StepId::parse(node).base_id().to_string();
                !self.bytes_unchecked.contains(&base)
            })
            .collect()
    }
}

/// The answer to an accepted post (`201`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PostAccepted {
    /// The run id the server recorded the run under.
    pub run_id: String,
    /// Steps whose run hash the ledger already held with a different digest. The ledger keeps
    /// its row; the posting machine prints the non-determinism warning for each.
    #[serde(default)]
    pub bytes_changed: Vec<String>,
}
