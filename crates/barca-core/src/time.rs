//! Wall-clock helpers: Unix seconds and nanoseconds, and the RFC 3339 spelling barca prints
//! (`2026-10-22T06:00:00Z`: UTC, to the second).

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix seconds as RFC 3339 UTC; `None` for a value chrono cannot represent.
pub fn rfc3339_secs(secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Unix seconds as RFC 3339 UTC; empty for a value chrono cannot represent.
pub fn rfc3339(secs: i64) -> String {
    rfc3339_secs(secs).unwrap_or_default()
}

/// Whole seconds since the Unix epoch at `t` (0 before the epoch).
pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Nanoseconds since the Unix epoch at `t` (0 before the epoch).
pub fn unix_nanos(t: SystemTime) -> u128 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Whole seconds since the Unix epoch, now.
pub fn now_secs() -> u64 {
    unix_secs(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_spells_utc_to_the_second() {
        assert_eq!(
            rfc3339_secs(1_792_648_800),
            Some("2026-10-22T06:00:00Z".into())
        );
        assert_eq!(rfc3339(1_792_648_800), "2026-10-22T06:00:00Z");
        assert_eq!(rfc3339_secs(i64::MAX), None);
        assert_eq!(rfc3339(i64::MAX), "");
    }

    #[test]
    fn times_before_the_epoch_read_as_zero() {
        let before = UNIX_EPOCH - std::time::Duration::from_secs(5);
        assert_eq!(unix_secs(before), 0);
        assert_eq!(unix_nanos(before), 0);
        assert_eq!(
            unix_secs(UNIX_EPOCH + std::time::Duration::from_nanos(1_500_000_000)),
            1
        );
        assert_eq!(
            unix_nanos(UNIX_EPOCH + std::time::Duration::from_nanos(1_500_000_000)),
            1_500_000_000
        );
    }
}
