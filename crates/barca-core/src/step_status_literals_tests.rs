//! No bare step-status literal outside the vocabulary (`barca_model::step_status`): a source
//! scan over `barca-model` and `barca-core`, which lives here beside `source_scan`.

use crate::source_scan;
use std::path::Path;

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
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut offenders = Vec::new();
    let sources = ["barca-model", "barca-core"]
        .iter()
        .flat_map(|krate| source_scan::non_test_sources(&crates.join(krate).join("src")));
    for (rel, src) in sources {
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
