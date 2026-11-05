//! The status of a step or target in a run's report: `steps[].status` of `barca get --json`
//! (`cached`, `ran`, `partial`, `skipped`, `failed`), `targets.<name>.status` (`success`,
//! `failed`, `blocked`) and a run's recorded steps (`cached`, `success`, `failed`). One
//! vocabulary, spelled in JSON exactly as [`StepStatus::as_str`]; the dry-run `action` shares
//! `cached` and `partial` with it (`run` and `unknown` are its own).

use serde::{Deserialize, Serialize};

/// How a step or target ended (`barca docs contract`, the result bodies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Served from cache; the function did not run.
    Cached,
    /// The function ran (every partition key, for a partitioned step).
    Ran,
    /// A partitioned step with some keys cached and the others run.
    Partial,
    /// Planned but not run: a step it depends on failed, or the run stopped first.
    Skipped,
    /// A target blocked on a Manual node that has never been computed.
    Blocked,
    /// A target that returned, or a recorded step attempt that succeeded.
    Success,
    /// The step (or the target's failing step) failed permanently.
    Failed,
}

impl StepStatus {
    /// Every status, in the order the manual lists them.
    pub const ALL: [StepStatus; 7] = [
        StepStatus::Cached,
        StepStatus::Ran,
        StepStatus::Partial,
        StepStatus::Skipped,
        StepStatus::Blocked,
        StepStatus::Success,
        StepStatus::Failed,
    ];

    /// The JSON spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            StepStatus::Cached => "cached",
            StepStatus::Ran => "ran",
            StepStatus::Partial => "partial",
            StepStatus::Skipped => "skipped",
            StepStatus::Blocked => "blocked",
            StepStatus::Success => "success",
            StepStatus::Failed => "failed",
        }
    }

    /// The status spelled `s`, if it is one.
    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    /// Whether `status` (a report's `status` or `action` text) is this status.
    pub fn is(self, status: Option<&str>) -> bool {
        status == Some(self.as_str())
    }
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The text is not one of the statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStepStatus(pub String);

impl std::fmt::Display for UnknownStepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown step status '{}'", self.0)
    }
}

impl std::error::Error for UnknownStepStatus {}

impl std::str::FromStr for StepStatus {
    type Err = UnknownStepStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        StepStatus::ALL
            .into_iter()
            .find(|status| status.as_str() == s)
            .ok_or_else(|| UnknownStepStatus(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_scan;
    use std::path::Path;

    #[test]
    fn every_status_round_trips_through_str_and_serde() {
        for status in StepStatus::ALL {
            assert_eq!(status.as_str().parse::<StepStatus>(), Ok(status));
            assert_eq!(StepStatus::parse(status.as_str()), Some(status));
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(serde_json::from_str::<StepStatus>(&json).unwrap(), status);
            assert_eq!(status.to_string(), status.as_str());
            assert!(status.is(Some(status.as_str())));
        }
        assert_eq!(StepStatus::parse("run"), None);
        assert!("Cached".parse::<StepStatus>().is_err());
        assert!(serde_json::from_str::<StepStatus>("\"will_run\"").is_err());
        assert!(!StepStatus::Cached.is(None));
    }

    /// The words only a step status spells. `success` and `failed` are left out: run and
    /// materialization statuses spell theirs the same way.
    const WORDS: [&str; 5] = ["cached", "ran", "partial", "skipped", "blocked"];

    /// Files that may spell one of [`WORDS`] as a bare literal, each with why.
    const ALLOWED: &[(&str, &str, &str)] = &[
        ("step_status.rs", "*", "the vocabulary"),
        (
            "cache_state.rs",
            "*",
            "the cache-state vocabulary shares three spellings",
        ),
        ("parse.rs", "partial", "Python's functools.partial"),
        (
            "io_loop.rs",
            "blocked",
            "the worker protocol's `blocked` frame type",
        ),
        (
            "wire.rs",
            "blocked",
            "the worker protocol's `blocked` frame type",
        ),
        (
            "jsonlog.rs",
            "skipped",
            "a word a log line's severity is read from",
        ),
        (
            "report.rs",
            "cached",
            "also the `cached` count key of a partitioned target's summary",
        ),
        (
            "outcome.rs",
            "ran",
            "the verb of the multi-target summary line",
        ),
        (
            "telemetry/datadog.rs",
            "skipped",
            "a telemetry run status: a serve tick that started no run",
        ),
    ];

    #[test]
    fn no_bare_step_status_literal_outside_the_vocabulary() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for (rel, src) in source_scan::non_test_sources(&root) {
            for (line, text) in source_scan::string_literals(&src) {
                let Some(word) = WORDS.iter().find(|w| **w == text) else {
                    continue;
                };
                let allowed = ALLOWED
                    .iter()
                    .any(|(f, w, _)| *f == rel && (*w == "*" || w == word));
                if !allowed {
                    offenders.push(format!("  {rel}:{line}: \"{word}\""));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "bare step-status literals (use StepStatus::<Variant>.as_str()):\n{}",
            offenders.join("\n")
        );
    }
}
