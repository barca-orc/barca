//! Small helpers the server and the CLI share while barca-core grows its own: the clock in
//! the forms the server records and serves it in, a file digest, and the words a recorded
//! step's status is spelled with.
//!
//! They were `runs::timestamp` / `runs::durable_timestamp`, `state::now_ts`, an inline
//! `duration_since(UNIX_EPOCH)` in `telemetry`, and one `sha256_hex` each in the server's
//! `inspect` and the CLI's `client`. One definition each lives here until the core exposes
//! them.
// TODO(merge): use barca_core::time / hash (and barca_core::StepStatus) once the core refactor lands

#[cfg(test)]
mod tests;

/// Now, as unix epoch seconds (a run's `started_at` / `finished_at` in the live state).
pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Now, as unix epoch nanoseconds (telemetry spans), saturating at `u64::MAX`.
pub fn unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

/// A live time ([`now_ts`]) as RFC 3339 with milliseconds, in UTC: `2026-10-09T12:00:00.900Z`.
pub fn timestamp(at: f64) -> String {
    chrono::DateTime::from_timestamp_millis((at * 1000.0) as i64)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// A time the metadata DB recorded (`datetime('now')`: `YYYY-MM-DD HH:MM:SS`, UTC) in the
/// same form as [`timestamp`], so live and durable times sort together; anything else as it
/// is.
pub fn durable_timestamp(at: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S")
        .map(|t| {
            t.and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| at.to_string())
}

/// The SHA-256 of `bytes` as lowercase hex (a deployed file's digest in `GET /project`, and
/// what the CLI compares its copy against).
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The words a recorded step's status is spelled with, in the metadata DB, a run's outcome
/// and `step_finished` events.
pub(crate) mod step_status {
    pub(crate) const CACHED: &str = "cached";
    pub(crate) const SUCCESS: &str = "success";
    pub(crate) const FAILED: &str = "failed";

    /// The status a `step_finished` event reports.
    pub(crate) fn of_event(cached: bool, ok: bool) -> &'static str {
        match (cached, ok) {
            (true, _) => CACHED,
            (_, true) => SUCCESS,
            _ => FAILED,
        }
    }
}
