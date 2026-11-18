//! What a run records about itself so that it explains itself (#925): why it executed the steps
//! it did instead of serving them from cache ([`PlanReasons`]), and why it was cancelled
//! ([`Cancellation`]). Both are recorded on the run's `runs` row as JSON and shown by
//! `GET /runs/{run_id}` and `barca history RUN_ID --json`.

use serde::{Deserialize, Serialize};

/// Why a step was executed instead of being served from cache, as the engine decided it. A
/// category names what the engine can tell apart; nothing here is inferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ReasonCategory {
    /// No successful result of the node was ever recorded (for a partitioned node: of any of
    /// its keys).
    NeverRun,
    /// Keys of a partitioned node that never had a successful result, while other keys of the
    /// node did: keys added since.
    NewKeys,
    /// The last attempt at exactly this code and these inputs failed: this is the retry.
    Failed,
    /// The code changed since the last success: the subject is `code`, `decorator argument
    /// NAME`, `decorator NAME`, `helper FILE:NAME` (or `helper FILE`) or `kind`.
    CodeChanged,
    /// An input changed since the last success: the subject is `input ID` (an upstream's run
    /// hash), `sensor ID` (a sensor's output) or `value ID` (a dict `partitions_from` source's
    /// value for the key).
    InputsChanged,
    /// A declared environment variable changed since the last success: `env NAME`.
    EnvChanged,
    /// The run hash differs from the last success, which recorded no components (a result
    /// recorded before 0.29.0, or a row trimmed by the metadata size limit): what changed is not
    /// known.
    Changed,
    /// Named in `--refresh` (`refresh` of `POST /runs`): the subject is the node.
    Refresh,
    /// Downstream of a node named in `--refresh`: the subject is that node.
    RefreshCascade,
    /// `--refresh-all`.
    RefreshAll,
    /// Its cached result is valid, but the artifact file is missing and something reads it.
    ArtifactMissing,
    /// The same, because the cache size limit deleted the file.
    Evicted,
    /// A task: tasks always run (unless a tick finds them up to date).
    Task,
    /// A sensor: sensors always run.
    Sensor,
    /// No cache hit, and barca did not look up why: a server's ledger answered the lookup, or
    /// the node had more keys to run in one phase than barca examines
    /// ([`EXAMINED_KEYS_PER_NODE`]).
    NotExamined,
}

impl ReasonCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonCategory::NeverRun => "never_run",
            ReasonCategory::NewKeys => "new_keys",
            ReasonCategory::Failed => "failed",
            ReasonCategory::CodeChanged => "code_changed",
            ReasonCategory::InputsChanged => "inputs_changed",
            ReasonCategory::EnvChanged => "env_changed",
            ReasonCategory::Changed => "changed",
            ReasonCategory::Refresh => "refresh",
            ReasonCategory::RefreshCascade => "refresh_cascade",
            ReasonCategory::RefreshAll => "refresh_all",
            ReasonCategory::ArtifactMissing => "artifact_missing",
            ReasonCategory::Evicted => "evicted",
            ReasonCategory::Task => "task",
            ReasonCategory::Sensor => "sensor",
            ReasonCategory::NotExamined => "not_examined",
        }
    }

    /// The category of a changed run-hash component (`changes::diff`'s words).
    pub fn of_component(component: &str) -> Self {
        match component.split(' ').next().unwrap_or(component) {
            "input" | "sensor" | "value" => ReasonCategory::InputsChanged,
            "env" => ReasonCategory::EnvChanged,
            _ => ReasonCategory::CodeChanged,
        }
    }
}

/// The most keys of one node, per phase, whose reason barca looks up; the rest are
/// [`ReasonCategory::NotExamined`]. Looking a key up reads its history, and a node of 200k keys
/// that all run would read every row of it.
pub const EXAMINED_KEYS_PER_NODE: usize = 10_000;

/// The most `(category, subject)` entries [`PlanReasons::reasons`] lists; the others are counted
/// in [`PlanReasons::omitted`].
pub const MAX_REASONS: usize = 64;

/// Why a run executed what it did, by reason: recorded as each phase of the run is decided, so
/// a run in progress shows the phases decided so far. Experimental.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanReasons {
    /// The nodes the run decided to execute, so far.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub nodes: u64,
    /// The steps the run decided to execute, so far: a partitioned node's keys each count.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps: u64,
    /// By `(category, subject)`, most steps first, at most [`MAX_REASONS`]. A step with
    /// several changed components counts under each of them, so the entries can add up to more
    /// than `steps`.
    pub reasons: Vec<PlanReason>,
    /// How many `(category, subject)` entries were left out of `reasons`.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub omitted: u64,
}

/// One reason of [`PlanReasons`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanReason {
    pub category: ReasonCategory,
    /// What changed (`helper pipelines/common.py:bucket_ltv`, `input pipeline.py:orders`), the
    /// refreshed node, or `null` when the category says it all.
    pub subject: Option<String>,
    /// The nodes executed for this reason.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub nodes: u64,
    /// Their steps (a partitioned node's keys each count).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps: u64,
    /// The first node executed for this reason (a full node id), to link to.
    pub example: String,
}

/// Who or what cancelled a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum CancelledBy {
    /// `DELETE /runs/{run_id}`: the web UI's Cancel, a `--server` client's first stop signal
    /// (Ctrl-C, SIGTERM), or any other client.
    Request,
    /// The server stopped (a second stop signal during its drain).
    Shutdown,
}

/// Why a queued run has not started beyond the runs ahead of it (`waiting_for` of `POST /runs`,
/// `GET /queue` and `GET /runs/{run_id}`; `null` when nothing else holds it). Experimental.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum WaitingFor {
    /// The server is draining before it stops (SIGTERM, SIGINT): it starts no queued run, and
    /// the run starts on the next server started on the same metadata DB, after the drain
    /// (at most `[server] drain_timeout`) and the restart.
    Restart,
}

/// Why a run was cancelled, recorded with it. Experimental.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Cancellation {
    pub by: CancelledBy,
    /// The token the cancel request was authenticated with (`token:<name>`); `null` without
    /// authentication.
    pub principal: Option<String>,
    /// Whether the run had left the queue: `false` for a run cancelled while it waited, which
    /// executed nothing.
    pub started: bool,
    /// For a run cancelled before it started: the runs that were executing then, which it
    /// waited behind (at most 10).
    #[serde(default)]
    pub waiting_behind: Vec<String>,
    /// Of those, one with the same command, targets and files: the same work was already
    /// running as this run.
    #[serde(default)]
    pub same_as: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_spell_as_they_serialize() {
        for c in [
            ReasonCategory::NeverRun,
            ReasonCategory::NewKeys,
            ReasonCategory::Failed,
            ReasonCategory::CodeChanged,
            ReasonCategory::InputsChanged,
            ReasonCategory::EnvChanged,
            ReasonCategory::Changed,
            ReasonCategory::Refresh,
            ReasonCategory::RefreshCascade,
            ReasonCategory::RefreshAll,
            ReasonCategory::ArtifactMissing,
            ReasonCategory::Evicted,
            ReasonCategory::Task,
            ReasonCategory::Sensor,
            ReasonCategory::NotExamined,
        ] {
            assert_eq!(
                serde_json::to_value(c).unwrap(),
                serde_json::Value::String(c.as_str().into())
            );
        }
    }

    #[test]
    fn a_component_names_its_category() {
        assert_eq!(
            ReasonCategory::of_component("helper a.py:f"),
            ReasonCategory::CodeChanged
        );
        assert_eq!(
            ReasonCategory::of_component("input a.py:x"),
            ReasonCategory::InputsChanged
        );
        assert_eq!(
            ReasonCategory::of_component("value a.py:v"),
            ReasonCategory::InputsChanged
        );
        assert_eq!(
            ReasonCategory::of_component("env TOKEN"),
            ReasonCategory::EnvChanged
        );
        assert_eq!(
            ReasonCategory::of_component("kind"),
            ReasonCategory::CodeChanged
        );
    }
}
