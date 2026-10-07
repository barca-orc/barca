//! Which shared-state blob the local database is based on (RFC-0006 §4.1).
//!
//! `<db>.base` is a small local file, never uploaded, written under the database's
//! cross-process lock when the local database is replaced by a pull or has been uploaded by
//! a push. It records the blob's token together with the **identity of the database file at
//! that instant** ([`FileId`]).
//!
//! The one rule: the record is [`trusted`](Base::trusted) only when the local database is
//! provably the very file it was written for, with nothing written since. Then, and only
//! then, the local database is exactly a blob that was in the shared state, which allows two
//! shortcuts: not downloading a shared state whose token is the recorded one, and letting a
//! download take the local database's place without comparing the two. In every other
//! situation (a database created fresh, replaced, restored from a copy, written to by a run
//! that did not push or by another tool, a record that is missing, unreadable, copied from
//! elsewhere, or written while a swap was in progress) a pull takes the full path: download,
//! carry over the local rows the download lacks, swap. That path is safe whatever the local
//! database is.
//!
//! The record has a second, independent use that needs no trust: a pull reads the raw bytes
//! before it downloads and again, under the lock, before it swaps. If they changed, another
//! process replaced or pushed the database meanwhile and the download is discarded
//! ([`Base::seq`] makes every write distinct).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::MetadataExt;

/// What identifies a database file as "this file, not modified since": where it is, how big,
/// its modification and status-change times, and its SQLite header (which holds the file
/// change counter and the schema cookie). Any write to the file changes its times; replacing
/// it changes its inode or its status-change time, which no tool can set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileId {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    /// SHA-256 of the first 100 bytes (the SQLite header), hex.
    header: String,
}

impl FileId {
    pub(crate) fn of(path: &str) -> Option<Self> {
        use std::io::Read;
        let mut file = fs::File::open(path).ok()?;
        let mut header = Vec::with_capacity(100);
        file.by_ref().take(100).read_to_end(&mut header).ok()?;
        // Stat the open file, after reading it: the same file the header came from.
        let meta = file.metadata().ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
            header: format!("{:x}", Sha256::digest(&header)),
        })
    }
}

fn ns(secs: i64, nanos: i64) -> i64 {
    secs.saturating_mul(1_000_000_000).saturating_add(nanos)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Base {
    /// Incremented on every write, so two states of the file never compare equal.
    pub seq: u64,
    /// The token of the blob the local database was pulled from or pushed as. Empty while a
    /// swap is in progress (or was cut short), and whenever nothing can be said.
    pub token: String,
    /// True when the last pull carried local rows over: they are not in the shared state yet.
    pub unpushed: bool,
    /// What the last pull kept ([`crate::state_carry::Carried::digest`]), so the same rows
    /// are not announced again by every command until they are pushed.
    pub kept: String,
    /// The database's main file when this was written. None when it could not be read.
    id: Option<FileId>,
    /// True when, at the time of writing, the filesystem's clock had already moved past the
    /// file's timestamps: any later write to the file then gets different ones, however
    /// coarse the filesystem's timestamps are.
    settled: bool,
}

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

/// Forget what the local database is based on. Called when a database file is created where
/// there was none: whatever the record says was said about another file.
pub(crate) fn forget(db_path: &str) {
    let _ = fs::remove_file(path(db_path));
}

/// How long [`write`] waits for the filesystem's clock to move past the database file's
/// timestamps. Clocks tick at least every 10 ms on the filesystems barca is used on; where
/// they are coarser the record is written unsettled and is never trusted.
const SETTLE_TRIES: u32 = 15;

/// Write the next state of the record, atomically. The caller holds the database lock, and
/// nothing may write to the database between the event recorded and this call.
/// `previous` is what [`read_raw`] returned under that lock.
pub(crate) fn write(
    db_path: &str,
    previous: Option<&[u8]>,
    token: &str,
    unpushed: bool,
    kept: &str,
) -> std::io::Result<()> {
    let target = path(db_path);
    let tmp = format!("{target}.tmp-{}", std::process::id());
    let id = FileId::of(db_path);
    // A file created now, in the same directory, is stamped with the filesystem's idea of
    // "now". Only once that is later than the database's own stamps can a later write to the
    // database be told from no write at all.
    let mut settled = false;
    if let (false, Some(id)) = (token.is_empty(), &id) {
        for attempt in 0..SETTLE_TRIES {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            fs::write(&tmp, b"")?;
            let probe = fs::metadata(&tmp)?;
            if ns(probe.mtime(), probe.mtime_nsec()) > id.mtime_ns
                && ns(probe.ctime(), probe.ctime_nsec()) > id.ctime_ns
            {
                settled = true;
                break;
            }
        }
    }
    let base = Base {
        seq: parse(previous).map_or(0, |b| b.seq) + 1,
        token: token.to_string(),
        unpushed,
        kept: kept.to_string(),
        id,
        settled,
    };
    fs::write(&tmp, serde_json::to_vec(&base).unwrap_or_default())?;
    fs::rename(&tmp, &target)
}

impl Base {
    /// True when the local database is provably the file this record was written for and
    /// nothing has been written to it since: it is then exactly the blob `token` names.
    ///
    /// Every write to a barca database goes to its write-ahead log first, so a log with
    /// anything in it means "written to". A checkpoint, another tool writing the main file,
    /// or the file being replaced, restored or recreated changes the main file's identity.
    /// Rows carried over by the last pull are in the main file itself, hence `unpushed`.
    /// Anything that cannot be established is a "no", which only costs the full pull.
    pub(crate) fn trusted(&self, db_path: &str) -> bool {
        !self.token.is_empty()
            && !self.unpushed
            && self.settled
            && self.id.is_some()
            && crate::db::wal_is_clean(db_path)
            && FileId::of(db_path) == self.id
            // Looked at last: a log that appeared while the file was being identified.
            && crate::db::wal_is_clean(db_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_in(dir: &tempfile::TempDir, name: &str, content: &[u8]) -> String {
        let db = dir.path().join(name).to_string_lossy().to_string();
        fs::write(&db, content).unwrap();
        db
    }

    fn record(db: &str, token: &str, unpushed: bool) -> Base {
        write(db, read_raw(db).as_deref(), token, unpushed, "").unwrap();
        read(db).unwrap()
    }

    #[test]
    fn every_write_is_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir, "m.db", b"one");
        assert_eq!((read_raw(&db), read(&db)), (None, None));
        let first = record(&db, "t1", false);
        let first_raw = read_raw(&db).unwrap();
        // The same token again is still a different state of the record.
        let second = record(&db, "t1", false);
        assert_ne!(first_raw, read_raw(&db).unwrap());
        assert_eq!((first.seq, second.seq), (1, 2));
        // Garbage in the record is "unknown", not an error, and still a distinct state.
        fs::write(path(&db), b"{").unwrap();
        assert_eq!(read(&db), None);
        assert!(read_raw(&db).is_some());
        forget(&db);
        assert_eq!(read_raw(&db), None);
    }

    #[test]
    fn the_record_is_trusted_only_for_the_very_file_it_was_written_for() {
        let dir = tempfile::tempdir().unwrap();
        let content = vec![7u8; 4096];
        let db = db_in(&dir, "m.db", &content);
        let base = record(&db, "t1", false);
        assert!(
            base.settled,
            "the clock of this filesystem did not move in 15 ms"
        );
        assert!(base.trusted(&db));

        // A write-ahead log with something in it: written to since.
        fs::write(format!("{db}-wal"), b"frame").unwrap();
        assert!(!base.trusted(&db));
        fs::write(format!("{db}-wal"), b"").unwrap();
        assert!(base.trusted(&db), "an empty log is no write");
        fs::remove_file(format!("{db}-wal")).unwrap();

        // The same bytes written again, in place: same inode, same size, new times.
        fs::write(&db, &content).unwrap();
        assert!(!base.trusted(&db));

        // Changed in place with the modification time put back (`cp -p`, `touch -r`): the
        // status-change time cannot be put back.
        let base = record(&db, "t1", false);
        assert!(base.trusted(&db));
        let before = fs::metadata(&db).unwrap();
        let mut changed = content.clone();
        changed[200] = 8;
        fs::write(&db, &changed).unwrap();
        fs::File::options()
            .write(true)
            .open(&db)
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        assert_eq!(
            fs::metadata(&db).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert!(!base.trusted(&db));

        // Deleted and created again, even with the same content.
        let base = record(&db, "t1", false);
        fs::remove_file(&db).unwrap();
        assert!(!base.trusted(&db));
        fs::write(&db, &changed).unwrap();
        assert!(!base.trusted(&db));

        // Another file moved into its place (a restored copy).
        let base = record(&db, "t1", false);
        let copy = db_in(&dir, "copy.db", &changed);
        fs::rename(&copy, &db).unwrap();
        assert!(!base.trusted(&db));

        // A record that belongs to another database (copied from another project or env).
        let other = db_in(&dir, "other.db", &changed);
        record(&other, "t-other", false);
        record(&db, "t1", false);
        fs::copy(path(&other), path(&db)).unwrap();
        assert!(!read(&db).unwrap().trusted(&db));

        // Unpushed rows, a swap in progress, and a record whose file could not be read.
        assert!(!record(&db, "t1", true).trusted(&db));
        assert!(!record(&db, "", false).trusted(&db));
        let missing = dir.path().join("missing.db").to_string_lossy().to_string();
        assert!(!record(&missing, "t1", false).trusted(&missing));
    }

    #[test]
    fn a_record_written_before_the_clock_moved_is_never_trusted() {
        // What a filesystem with coarse timestamps produces: the record cannot tell a later
        // write in the same tick from no write.
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir, "m.db", &[7u8; 4096]);
        let mut base = record(&db, "t1", false);
        assert!(base.trusted(&db));
        base.settled = false;
        assert!(!base.trusted(&db));
    }
}
