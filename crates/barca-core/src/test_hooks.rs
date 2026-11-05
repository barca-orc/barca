//! Hooks for barca's own test suites: every `BARCA_TEST_*` environment variable is read here and
//! nowhere else. They are not part of the contract (`barca docs contract`, "Environment
//! variables"); each only shortens a wait or relaxes a check so a test can reach a state quickly.

use std::time::Duration;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `BARCA_TEST_BEFORE_START_WORKERS_SECS`: how long a run sleeps after it is recorded and before
/// its workers start, so a test can cancel it in that window. `None` unless a whole number.
pub(crate) fn before_start_workers() -> Option<Duration> {
    var("BARCA_TEST_BEFORE_START_WORKERS_SECS")?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// `BARCA_TEST_CACHE_CHECKPOINT_SECS`: the period of `barca serve`'s cache-size checkpoint, so
/// the simulation need not wait a real minute per check. `None` unless a whole number.
pub fn cache_checkpoint_secs() -> Option<u64> {
    var("BARCA_TEST_CACHE_CHECKPOINT_SECS")?.parse().ok()
}

/// Slack's variables are read trimmed, an empty value as unset.
fn slack_var(name: &str) -> Option<String> {
    var(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `BARCA_TEST_SLACK_ALLOW_HTTP`: a fake Slack webhook may speak plain `http://`.
pub(crate) fn slack_allow_http() -> bool {
    slack_var("BARCA_TEST_SLACK_ALLOW_HTTP").is_some()
}

/// `BARCA_TEST_SLACK_REMIND_SECS`: when a still-failing job's Slack reminder is due, in place of
/// a day. `None` unless a whole number.
pub(crate) fn slack_remind_after() -> Option<Duration> {
    slack_var("BARCA_TEST_SLACK_REMIND_SECS")?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Set on the child process `helper_proc`'s storage-options test re-executes itself as.
#[cfg(test)]
pub const HELPER_OPTIONS_PROBE: &str = "BARCA_TEST_HELPER_OPTIONS_PROBE";

/// Set on the child process `db`'s lock test re-executes itself as: the path of the database
/// that child writes once and then closes.
#[cfg(test)]
pub const LAST_CONNECTION_WRITER: &str = "BARCA_TEST_LAST_CONNECTION_WRITER";

/// The database path from [`LAST_CONNECTION_WRITER`], when this process is that child.
#[cfg(test)]
pub fn last_connection_writer() -> Option<std::ffi::OsString> {
    std::env::var_os(LAST_CONNECTION_WRITER)
}
