//! `[cache] max_size`: parsing a size, resolving it against the filesystem, printing bytes.

use std::path::Path;

/// A configured cache limit, before it is resolved to bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MaxSize {
    /// `off`: no limit (barca's behaviour before 0.25).
    Off,
    /// A number of bytes.
    Bytes(u64),
    /// A percentage of the filesystem `.barca/` is on, read at each check.
    Percent(f64),
}

/// The limit when nothing is configured: 10% of the filesystem (decided 2026-10-20, RFC-0012).
pub const DEFAULT: MaxSize = MaxSize::Percent(10.0);

/// Parse `500MB`, `20GB`, `20GiB`, `1.5 TB`, a bare number of bytes, `10%` or `off`.
/// Units are case-insensitive; `KB`/`MB`/`GB`/`TB` are decimal, `KiB`/`MiB`/`GiB`/`TiB` binary.
pub fn parse(raw: &str) -> Result<MaxSize, String> {
    let s = raw.trim();
    let bad = |why: &str| {
        format!(
            "invalid cache size '{raw}': {why} (expected e.g. \"20GB\", \"500MB\", \"20GiB\", a \
             number of bytes, \"10%\" or \"off\")"
        )
    };
    if s.eq_ignore_ascii_case("off") {
        return Ok(MaxSize::Off);
    }
    if let Some(pct) = s.strip_suffix('%') {
        let value: f64 = pct.trim().parse().map_err(|_| bad("not a number"))?;
        if !(value > 0.0 && value <= 100.0) {
            return Err(bad("a percentage must be above 0 and at most 100"));
        }
        return Ok(MaxSize::Percent(value));
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let value: f64 = number.parse().map_err(|_| bad("not a number"))?;
    let scale: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => return Err(bad("unknown unit")),
    };
    let bytes = value * scale as f64;
    if !bytes.is_finite() || bytes < 1.0 || bytes > u64::MAX as f64 {
        return Err(bad("must be at least 1 byte"));
    }
    Ok(MaxSize::Bytes(bytes as u64))
}

impl MaxSize {
    /// The limit in bytes for a `.barca/` at `dir` (`None` when off). A percentage is of the
    /// filesystem `dir` is on, or the nearest existing parent's.
    pub fn resolve(self, dir: &Path) -> Option<u64> {
        match self {
            MaxSize::Off => None,
            MaxSize::Bytes(n) => Some(n),
            MaxSize::Percent(p) => {
                let total = filesystem(dir)?.total;
                Some(((total as f64) * p / 100.0) as u64)
            }
        }
    }

    /// How it reads in a message: `20 GB`, `10% of the filesystem`, `off`.
    pub fn describe(self) -> String {
        match self {
            MaxSize::Off => "off".to_string(),
            MaxSize::Bytes(n) => human(n),
            MaxSize::Percent(p) => format!("{p}% of the filesystem"),
        }
    }
}

/// Size and free space of a filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filesystem {
    pub total: u64,
    /// Space an unprivileged process can still write.
    pub available: u64,
}

/// The filesystem `dir` is on (or its nearest existing parent's). `None` when it cannot be read.
#[cfg(unix)]
pub fn filesystem(dir: &Path) -> Option<Filesystem> {
    use std::os::unix::ffi::OsStrExt;
    let mut at = dir;
    while !at.exists() {
        at = at.parent().filter(|p| !p.as_os_str().is_empty())?;
    }
    let c = std::ffi::CString::new(at.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid NUL-terminated path; `stat` is written by the call only.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    Some(Filesystem {
        total: (stat.f_blocks as u64).saturating_mul(block),
        available: (stat.f_bavail as u64).saturating_mul(block),
    })
}

#[cfg(not(unix))]
pub fn filesystem(_dir: &Path) -> Option<Filesystem> {
    None
}

/// Bytes in decimal units, as limits are written: `512 B`, `1.2 MB`, `21.4 GB`. barca's one byte
/// formatter: every size it prints (cache, transfers, backups, `barca sql`) goes through it.
pub fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if v.fract() < 0.05 || v >= 100.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_in_decimal_and_binary_units() {
        assert_eq!(parse("20GB"), Ok(MaxSize::Bytes(20_000_000_000)));
        assert_eq!(parse("20 gb"), Ok(MaxSize::Bytes(20_000_000_000)));
        assert_eq!(parse("500MB"), Ok(MaxSize::Bytes(500_000_000)));
        assert_eq!(parse("1.5TB"), Ok(MaxSize::Bytes(1_500_000_000_000)));
        assert_eq!(parse("20GiB"), Ok(MaxSize::Bytes(20 << 30)));
        assert_eq!(parse("4096"), Ok(MaxSize::Bytes(4096)));
        assert_eq!(parse("10%"), Ok(MaxSize::Percent(10.0)));
        assert_eq!(parse(" 2.5 % "), Ok(MaxSize::Percent(2.5)));
        assert_eq!(parse("OFF"), Ok(MaxSize::Off));
    }

    #[test]
    fn nonsense_is_refused_with_the_accepted_forms() {
        for bad in ["", "GB", "20XB", "-5GB", "0", "0%", "101%", "abc%", "1e3"] {
            let err = parse(bad).unwrap_err();
            assert!(err.contains("expected e.g."), "{bad}: {err}");
        }
    }

    #[test]
    fn a_percentage_is_of_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let fs = filesystem(dir.path()).unwrap();
        assert!(fs.total > 0);
        let ten = MaxSize::Percent(10.0).resolve(dir.path()).unwrap();
        assert_eq!(ten, fs.total / 10);
        // A `.barca` that does not exist yet is on its parent's filesystem.
        assert_eq!(
            MaxSize::Percent(10.0).resolve(&dir.path().join(".barca/x")),
            Some(ten)
        );
        assert_eq!(MaxSize::Off.resolve(dir.path()), None);
        assert_eq!(MaxSize::Bytes(7).resolve(dir.path()), Some(7));
    }

    #[test]
    fn bytes_print_in_decimal_units() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(20_000_000_000), "20 GB");
        assert_eq!(human(21_400_000_000), "21.4 GB");
        assert_eq!(human(1_200_000), "1.2 MB");
    }
}
