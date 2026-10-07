//! A counter of how many times the local database has been replaced or pushed (RFC-0006 §4.1).
//!
//! A pull downloads the shared state without holding the database's lock, so that other
//! barca commands are not kept waiting on the network. By the time it takes the lock to swap
//! the download in, another process may have pulled a newer blob, or pushed. Swapping the
//! older download in then would take rows out of the local database, and the next push could
//! drop them from the shared state.
//!
//! `<db>.base` is how a pull notices. It is a small local file, never uploaded, written only
//! under the database's lock: before every swap, after every swap, and after every push. A
//! pull reads its bytes before it downloads and again, under the lock, before it swaps. If
//! they differ the download is discarded and the pull starts again.
//!
//! Nothing is ever concluded from the file about what the database contains: only "did it
//! change between two reads by the same process". So a missing, unreadable or foreign file
//! (deleted by hand, copied from another project, left by a crash) cannot cause a loss: the
//! two reads are equal and the pull takes its one path, or they differ and it downloads
//! again. Whatever the file says, every pull downloads, carries the local rows the download
//! lacks, and swaps.
//!
//! Two fields:
//!
//! - `seq`, incremented on every write, so that two states of the file are never equal (it
//!   starts from the clock when there is no readable predecessor, so a file recreated after
//!   being deleted or damaged does not repeat an earlier state either);
//! - `kept`, what the last pull carried over ([`crate::state_carry::Carried::digest`]), so
//!   that the same rows are not announced again by every command until a run pushes them.
//!   It only decides whether a line is printed.

use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Base {
    pub seq: u64,
    pub kept: String,
}

pub(crate) fn path(db_path: &str) -> String {
    format!("{db_path}.base")
}

/// The file's bytes, or None when it is not there. Compared as bytes: any change counts.
pub(crate) fn read_raw(db_path: &str) -> Option<Vec<u8>> {
    fs::read(path(db_path)).ok()
}

/// What the file says, or None when it is missing or not understood (then it says nothing).
pub(crate) fn parse(raw: Option<&[u8]>) -> Option<Base> {
    serde_json::from_slice(raw?).ok()
}

/// Write the next state of the file, atomically. The caller holds the database lock.
/// `previous` is what [`read_raw`] returned under that lock.
pub(crate) fn write(db_path: &str, previous: Option<&[u8]>, kept: &str) -> std::io::Result<()> {
    let base = Base {
        seq: match parse(previous) {
            Some(b) => b.seq.saturating_add(1),
            // No readable predecessor: start from the clock, which no earlier state used.
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64),
        },
        kept: kept.to_string(),
    };
    let target = path(db_path);
    let tmp = format!("{target}.tmp-{}", std::process::id());
    fs::write(&tmp, serde_json::to_vec(&base).unwrap_or_default())?;
    fs::rename(&tmp, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_write_is_a_different_state_whatever_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db").to_string_lossy().to_string();
        assert_eq!(read_raw(&db), None);

        let mut seen = vec![read_raw(&db)];
        for kept in ["", "", "1:2:0:0:r1", ""] {
            write(&db, read_raw(&db).as_deref(), kept).unwrap();
            let now = read_raw(&db);
            assert!(!seen.contains(&now), "a state of the file repeated");
            assert_eq!(parse(now.as_deref()).unwrap().kept, kept);
            seen.push(now);
        }
        let seqs: Vec<u64> = seen[1..]
            .iter()
            .map(|raw| parse(raw.as_deref()).unwrap().seq)
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");

        // Garbage (a foreign or damaged file) says nothing, is still a state, and has a
        // successor that differs from it.
        fs::write(path(&db), b"not json").unwrap();
        let garbage = read_raw(&db);
        assert_eq!(parse(garbage.as_deref()), None);
        write(&db, garbage.as_deref(), "").unwrap();
        assert_ne!(read_raw(&db), garbage);
    }
}
