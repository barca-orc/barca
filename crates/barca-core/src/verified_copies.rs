//! Local copies of stored artifacts that this process has already found to hold their recorded
//! digest, so a later run does not hash them again (#1010).
//!
//! With an artifact store, a cache hit's local copy is checked against the digest recorded with
//! the result before a step reads it (`barca docs remote`, "Checking a local copy against the
//! store"). That check hashes the whole file: about 150 µs per small input through the
//! transfer helper, and per byte for a large one. A long-lived server reads the same cached
//! inputs run after run, so this remembers, per local path, the file it hashed (device, inode,
//! size, mtime and ctime, to the nanosecond) and the digest it found, and takes that file as
//! checked while all five are unchanged. Anything else is hashed as before.
//!
//! What it protects against, and what it does not:
//!
//! - Any write through the file system changes the file's ctime (the kernel sets it; a program
//!   cannot set it back) and replacing the file changes its inode, so an edit, a truncation, a
//!   replacement by rename, `touch -r` and a chmod are all seen, whatever the size.
//! - A file is remembered only when its mtime and ctime are older than [`SETTLED`] before the
//!   hash began. A write in the same timestamp tick as the hash, or between the hash and the
//!   `stat` that records the file, therefore cannot leave a stale entry: such a file is hashed
//!   again next time, and remembered then. So a file the run itself just fetched (or wrote) is
//!   checked once more on its next use, not trusted on the spot.
//! - Not seen: bytes changed without going through the file system (a disk or a bug below the
//!   kernel), a system clock set back by more than [`SETTLED`], or a network file system whose
//!   server stamps times from a clock that far behind. The read itself has the same exposure,
//!   and more: a worker reads the path after the check without hashing what it reads, and
//!   without a store nothing is ever hashed on read.
//!
//! The memo lives in this process: a `barca serve` keeps it for its lifetime and starts empty
//! after a restart; a `barca get` keeps it for the one command, which checks each input once
//! anyway. It holds at most [`CAPACITY`] files; when full, files whose copy has changed or gone
//! are dropped, and if it is still full, new ones are hashed every time rather than remembered.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How old a file's mtime and ctime must be, before its hash began, for it to be remembered.
/// Far longer than any local file system's timestamp granularity.
pub(crate) const SETTLED: Duration = Duration::from_secs(2);

/// Most files remembered (about 300 bytes each).
pub(crate) const CAPACITY: usize = 131_072;

/// What identifies the file that was hashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}

fn nanos(secs: i64, nsec: i64) -> i128 {
    i128::from(secs) * 1_000_000_000 + i128::from(nsec)
}

fn identity(path: &Path) -> Option<Identity> {
    let m = std::fs::metadata(path).ok()?;
    m.is_file().then(|| Identity {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        mtime_ns: nanos(m.mtime(), m.mtime_nsec()),
        ctime_ns: nanos(m.ctime(), m.ctime_nsec()),
    })
}

/// Local path -> the file that was hashed and its SHA-256.
#[derive(Default)]
struct Memo {
    files: HashMap<PathBuf, (Identity, String)>,
}

impl Memo {
    fn holds(&self, path: &Path, sha256: &str) -> bool {
        self.files
            .get(path)
            .is_some_and(|(id, known)| known == sha256 && identity(path) == Some(*id))
    }

    fn remember<'a>(
        &mut self,
        files: impl IntoIterator<Item = (&'a Path, &'a str)>,
        hashed_from: SystemTime,
        capacity: usize,
    ) {
        let settled_ns = hashed_from
            .checked_sub(SETTLED)
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(i128::MIN, |d| d.as_nanos() as i128);
        let found: Vec<(PathBuf, Option<Identity>, String)> = files
            .into_iter()
            .map(|(path, sha256)| {
                let id = identity(path)
                    .filter(|id| id.mtime_ns < settled_ns && id.ctime_ns < settled_ns);
                (path.to_path_buf(), id, sha256.to_string())
            })
            .collect();
        let new = found
            .iter()
            .filter(|(path, id, _)| id.is_some() && !self.files.contains_key(path))
            .count();
        if self.files.len() + new > capacity {
            // Copies that changed or are gone (evicted, recomputed) make room first.
            self.files
                .retain(|path, (id, _)| identity(path) == Some(*id));
        }
        for (path, id, sha256) in found {
            match id {
                Some(id) if self.files.len() < capacity || self.files.contains_key(&path) => {
                    self.files.insert(path, (id, sha256));
                }
                Some(_) => {}
                None => {
                    self.files.remove(&path);
                }
            }
        }
    }
}

static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(Mutex::default);

fn memo() -> std::sync::MutexGuard<'static, Memo> {
    MEMO.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether `path` is a file this process found to have `sha256` and that has not changed since.
pub(crate) fn holds(path: &Path, sha256: &str) -> bool {
    memo().holds(path, sha256)
}

/// Remember that each `(path, sha256)` held that digest when hashed by a check that began at
/// `hashed_from`. A file written since `hashed_from - SETTLED` is not remembered (and any
/// earlier entry for it is dropped): it is hashed again next time.
pub(crate) fn remember<'a>(
    files: impl IntoIterator<Item = (&'a Path, &'a str)>,
    hashed_from: SystemTime,
) {
    memo().remember(files, hashed_from, CAPACITY);
}

/// Forget `path`: new bytes are being written there, or a check found it not to be the
/// recorded result.
pub(crate) fn forget(path: &Path) {
    memo().files.remove(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;
    use std::io::Write;

    /// A check that began well after the file was written: it is settled.
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(60)
    }

    fn file(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn an_unchanged_file_is_known_and_any_change_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), "a.json", b"[1, 2, 3]");
        assert!(!holds(&path, "d1"), "unknown before it is remembered");
        remember([(path.as_path(), "d1")], later());
        assert!(holds(&path, "d1"));
        assert!(
            !holds(&path, "d2"),
            "another recorded digest is checked again"
        );

        // A different size.
        std::fs::write(&path, b"[9, 9]").unwrap();
        assert!(!holds(&path, "d1"));

        // The same size, written in place with its old mtime put back: ctime still moves.
        remember([(path.as_path(), "d1")], later());
        assert!(holds(&path, "d1"));
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.write_all(b"[8, 8]").unwrap();
        f.set_times(FileTimes::new().set_modified(mtime)).unwrap();
        drop(f);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert!(!holds(&path, "d1"));

        // Replaced by rename (a new inode), and removed.
        remember([(path.as_path(), "d1")], later());
        let other = file(dir.path(), "b.json", b"[7, 7]");
        std::fs::rename(&other, &path).unwrap();
        assert!(!holds(&path, "d1"));
        remember([(path.as_path(), "d1")], later());
        std::fs::remove_file(&path).unwrap();
        assert!(!holds(&path, "d1"));
    }

    #[test]
    fn a_file_written_around_the_hash_is_not_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), "a.json", b"x");
        remember([(path.as_path(), "d1")], SystemTime::now());
        assert!(!holds(&path, "d1"));
        // A settled entry is dropped when a later check finds the file unsettled.
        remember([(path.as_path(), "d1")], later());
        assert!(holds(&path, "d1"));
        remember([(path.as_path(), "d1")], SystemTime::now());
        assert!(!holds(&path, "d1"));
    }

    #[test]
    fn forget_drops_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), "a.json", b"x");
        remember([(path.as_path(), "d1")], later());
        forget(&path);
        assert!(!holds(&path, "d1"));
        assert!(path.is_file());
    }

    #[test]
    fn a_full_memo_drops_changed_copies_first_and_then_stops_growing() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..4)
            .map(|i| file(dir.path(), &format!("{i}.json"), b"x"))
            .collect();
        let mut memo = Memo::default();
        memo.remember(paths[..2].iter().map(|p| (p.as_path(), "d")), later(), 2);
        assert!(memo.holds(&paths[0], "d") && memo.holds(&paths[1], "d"));
        // Full of live entries: a new file is not remembered, the old ones stay.
        memo.remember([(paths[2].as_path(), "d")], later(), 2);
        assert!(!memo.holds(&paths[2], "d"));
        assert!(memo.holds(&paths[0], "d") && memo.holds(&paths[1], "d"));
        // A copy that is gone makes room.
        std::fs::remove_file(&paths[0]).unwrap();
        memo.remember([(paths[3].as_path(), "d")], later(), 2);
        assert!(memo.holds(&paths[3], "d") && memo.holds(&paths[1], "d"));
        assert_eq!(memo.files.len(), 2);
    }
}
