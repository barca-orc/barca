//! Which shared-state blob the local database is based on (RFC-0006 §4.1).
//!
//! `<db>.base` is a small local file, never uploaded, written under the database's
//! cross-process lock every time the local database is replaced by a pull or uploaded by a
//! push. It answers three questions without opening the database:
//!
//! - **Has the local database been replaced or pushed since this download began?** A pull
//!   reads the file before it downloads and again, under the lock, before it swaps. If it
//!   changed, the download may be older than what is there now and is discarded
//!   ([`Base::seq`] makes every write distinct).
//! - **Is the shared state still the blob the local database is based on?** Then there is
//!   nothing to pull ([`Base::token`]).
//! - **Has anything been written locally since?** If not, the local database is exactly a
//!   blob that was in the shared state, there is nothing to carry over a pull, and the pulled
//!   file can simply take its place ([`Base::untouched`]).
//!
//! Invariant, kept by writing the file only under the lock: when `token` is not empty, the
//! local database holds every row of the blob with that token. A swap first writes an empty
//! token (so downloads in flight are discarded even if the process dies before finishing),
//! and the real one once the new file is in place.
//!
//! The file is an optimisation and a guard, never the record of what is unpushed: without it
//! (first pull after an upgrade, or deleted by hand) every pull downloads and compares.

use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Base {
    /// Incremented on every write, so two states of the file never compare equal.
    pub seq: u64,
    /// The token of the blob the local database was last pulled from or pushed as. Empty
    /// while a swap is in progress (or was cut short).
    pub token: String,
    /// True when the last pull carried local rows over: they are not in the shared state yet.
    pub unpushed: bool,
    /// What the last pull kept ([`crate::state_carry::Carried::digest`]), so the same rows
    /// are not announced again by every command until they are pushed.
    pub kept: String,
    /// Size and modification time of the main database file when this was written.
    pub len: u64,
    pub mtime_ns: i64,
    /// When this was written, by the same clock as `mtime_ns`.
    pub written_ns: i64,
}

/// A file modified this close to the time its fingerprint was taken could be modified again
/// without its timestamp changing, on a filesystem with coarse timestamps.
const RACY_NS: i64 = 2_000_000_000;

pub(crate) fn path(db_path: &str) -> String {
    format!("{db_path}.base")
}

/// The file's bytes, or None when it is not there. Compared as bytes: any change counts.
pub(crate) fn read_raw(db_path: &str) -> Option<Vec<u8>> {
    fs::read(path(db_path)).ok()
}

pub(crate) fn parse(raw: Option<&[u8]>) -> Option<Base> {
    serde_json::from_slice(raw?).ok()
}

#[cfg(test)]
pub(crate) fn read(db_path: &str) -> Option<Base> {
    parse(read_raw(db_path).as_deref())
}

fn fingerprint(db_path: &str) -> Option<(u64, i64)> {
    let meta = fs::metadata(db_path).ok()?;
    Some((
        meta.len(),
        meta.mtime()
            .checked_mul(1_000_000_000)?
            .checked_add(meta.mtime_nsec())?,
    ))
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Write the next state of the file, atomically. The caller holds the database lock.
/// `previous` is what [`read_raw`] returned under that lock.
pub(crate) fn write(
    db_path: &str,
    previous: Option<&[u8]>,
    token: &str,
    unpushed: bool,
    kept: &str,
) -> std::io::Result<()> {
    let (len, mtime_ns) = fingerprint(db_path).unwrap_or((0, 0));
    let base = Base {
        seq: parse(previous).map_or(0, |b| b.seq) + 1,
        token: token.to_string(),
        unpushed,
        kept: kept.to_string(),
        len,
        mtime_ns,
        written_ns: now_ns(),
    };
    let target = path(db_path);
    let tmp = format!("{target}.tmp-{}", std::process::id());
    fs::write(&tmp, serde_json::to_vec(&base).unwrap_or_default())?;
    fs::rename(&tmp, &target)
}

impl Base {
    /// True when nothing has written to the local database since this was recorded, so it is
    /// still exactly the blob `token` names: no unpushed rows, an empty write-ahead log (every
    /// write goes there first), and a main file that has not changed. A false "no" only costs
    /// the comparison this is meant to save.
    pub(crate) fn untouched(&self, db_path: &str) -> bool {
        if self.token.is_empty() || self.unpushed || !crate::db::wal_is_clean(db_path) {
            return false;
        }
        let Some((len, mtime_ns)) = fingerprint(db_path) else {
            return false;
        };
        // A timestamp taken within the filesystem's resolution of the write proves nothing.
        let settled = mtime_ns % 1_000_000_000 != 0 || now_ns() - mtime_ns > RACY_NS;
        (len, mtime_ns) == (self.len, self.mtime_ns) && settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_write_is_distinct_and_a_touched_database_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db").to_string_lossy().to_string();
        assert_eq!(read_raw(&db), None);
        assert_eq!(read(&db), None);

        fs::write(&db, b"one").unwrap();
        write(&db, None, "t1", false, "").unwrap();
        let first = read_raw(&db).unwrap();
        // The same token again is still a different state of the file.
        write(&db, Some(&first), "t1", false, "").unwrap();
        let second = read_raw(&db).unwrap();
        assert_ne!(first, second);
        let base = parse(Some(&second)).unwrap();
        assert_eq!((base.seq, base.token.as_str()), (2, "t1"));

        // Untouched: same file, no log. (Sub-second timestamps, or the file is treated as
        // possibly modified for two seconds: either answer is safe.)
        let settled = base.mtime_ns % 1_000_000_000 != 0;
        assert_eq!(base.untouched(&db), settled);
        // A write-ahead log with something in it.
        fs::write(format!("{db}-wal"), b"frame").unwrap();
        assert!(!base.untouched(&db));
        fs::remove_file(format!("{db}-wal")).unwrap();
        // A main file that changed.
        fs::write(&db, b"other").unwrap();
        assert!(!base.untouched(&db));
        // Unpushed rows, and a swap in progress.
        fs::write(&db, b"one").unwrap();
        for (token, unpushed) in [("t1", true), ("", false)] {
            write(&db, read_raw(&db).as_deref(), token, unpushed, "").unwrap();
            assert!(!read(&db).unwrap().untouched(&db));
        }
        // Garbage in the file is "unknown", not an error.
        fs::write(path(&db), b"{").unwrap();
        assert_eq!(read(&db), None);
        assert!(read_raw(&db).is_some());
    }
}
