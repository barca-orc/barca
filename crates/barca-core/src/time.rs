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
pub(crate) fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Nanoseconds since the Unix epoch at `t` (0 before the epoch).
pub(crate) fn unix_nanos(t: SystemTime) -> u128 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Whole seconds since the Unix epoch, now.
pub(crate) fn now_secs() -> u64 {
    unix_secs(SystemTime::now())
}

/// Seconds since the Unix epoch, now, with a fractional part (a served run's `started_at` /
/// `finished_at` in the live state).
pub fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Nanoseconds since the Unix epoch, now, saturating at `u64::MAX` (telemetry spans).
pub fn now_nanos_u64() -> u64 {
    unix_nanos(SystemTime::now()).min(u64::MAX as u128) as u64
}

/// Fractional Unix seconds as RFC 3339 UTC with milliseconds: `2026-10-09T12:00:00.900Z`
/// (how the server serves a live run's times); empty for a value chrono cannot represent.
pub fn rfc3339_millis(secs: f64) -> String {
    chrono::DateTime::from_timestamp_millis((secs * 1000.0) as i64)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// A time the metadata DB recorded (`datetime('now')`: `YYYY-MM-DD HH:MM:SS`, UTC) in the same
/// form as [`rfc3339_millis`], so live and durable times sort together; anything else as it is.
pub fn sql_datetime_rfc3339(at: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S")
        .map(|t| {
            t.and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| at.to_string())
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
    fn durable_and_live_timestamps_sort_in_the_same_utc_format() {
        let old = sql_datetime_rfc3339("2026-10-09 12:00:00");
        let newer = rfc3339_millis(1791547200.0 + 0.9);
        assert!(newer > old);
        assert_eq!(old, "2026-10-09T12:00:00.000Z");
        assert_eq!(newer, "2026-10-09T12:00:00.900Z");
        // Not a database time: kept as it is.
        assert_eq!(
            sql_datetime_rfc3339("2026-10-09T12:00:00Z"),
            "2026-10-09T12:00:00Z"
        );
        let secs = now_secs_f64();
        let nanos = now_nanos_u64();
        assert!((nanos as f64 / 1e9 - secs).abs() < 5.0);
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
