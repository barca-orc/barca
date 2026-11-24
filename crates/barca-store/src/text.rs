//! Text every layer formats the same way: sizes, lengths of time, stored timestamps, shell
//! words and the `BARCA_TRACE_TIMING` line. A leaf: it names no other module of this crate, so a computation module that puts a
//! size or a span into a sentence does not depend on `report` (rendering) or `gc` for it
//! (#1056).

pub use barca_model::cache_size::human;

/// A stored timestamp (always UTC: `datetime('now')`, or RFC 3339 with `Z`) for a human table,
/// marked as UTC so it is not read as local time (#496). Empty stays empty.
pub fn utc_cell(ts: &str) -> String {
    let ts = ts.trim();
    if ts.is_empty() {
        return String::new();
    }
    let canonical = barca_model::time::sql_datetime_rfc3339(ts);
    let utc = canonical.strip_suffix('Z').unwrap_or(&canonical);
    // Whole-second database rows gain .000 on the wire. Show the same timestamp locally
    // and remotely, preserving nonzero millisecond precision.
    let utc = utc.strip_suffix(".000").unwrap_or(utc);
    format!("{} UTC", utc.replacen('T', " ", 1))
}

/// How long ago, in the largest whole unit: `45s ago`, `3m ago`, `2h ago`, `6d ago`. A time
/// not in the past (another machine's clock) is `just now`.
pub fn ago(seconds: i64) -> String {
    match seconds {
        ..=0 => "just now".to_string(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// A length of time in the largest whole unit: `45s`, `3m`, `16h`, `6d`.
pub fn span(seconds: i64) -> String {
    match seconds.max(0) {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// Quote a word for display in a copy-pasteable shell command.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:,=@+%".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// With `BARCA_TRACE_TIMING` set: one stderr line with how long `what` took since `t0`, so the
/// cost of each part of `barca status` (and of a dry run) can be read without a profiler.
pub fn trace_since(what: &str, t0: std::time::Instant) {
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        barca_model::errln!(
            "[timing] {what}: {:.1} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
}
