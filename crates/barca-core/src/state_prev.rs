//! `<db>.prev`: the local database as it was before the last pull that changed it (#243).
//!
//! A pull puts a download of the shared state in the place of the local database. The rows the
//! download lacks are carried onto it first ([`crate::state_carry`]), but not everything is a
//! row that is carried (timing estimates, scheduler state, step rows from before 0.17), and a
//! shared state can be valid and still not be what anyone wanted (rolled back, reset, written
//! by a run pointed at the wrong location). So the file the swap replaces is kept, one
//! generation of it, where it can be put back by hand (`barca docs remote`).
//!
//! It is kept by the swap itself, in three steps around the rename in
//! [`crate::db::replace_db`], all under the database's lock:
//!
//! 1. [`Kept::stage`], before the swap: `<db>.prev.tmp` becomes a second name for the local
//!    database file (a hard link; where the filesystem has none, a copy that is fsynced). The
//!    log is already folded into that file, so it is the whole database.
//! 2. The swap renames the download over `<db>`. The old file now has one name, `.prev.tmp`.
//! 3. [`Kept::publish`], after the swap: `.prev.tmp` is renamed to `<db>.prev`.
//!
//! A hard link costs the same whatever the size of the history, and nothing is read or
//! written. `<db>.prev` only ever changes by that last rename, so at every instant it is
//! either the previous generation, whole, or the new one, whole; a process that dies at any
//! point leaves at worst a `.prev.tmp`, which the next pull removes ([`remove_leftover`]). A pull that dies between
//! steps 2 and 3 has replaced the database without updating `<db>.prev`, which then still
//! holds the generation before.
//!
//! The name is published only after the swap for a reason: until the swap, `.prev.tmp` and
//! the live database are one file under two names. Published earlier, a pull that then failed
//! would leave `<db>.prev` following every later write to the live database.
//!
//! Not kept: a local file that held no barca history (absent, empty, not a database), and a
//! pull that brings nothing new, which would otherwise overwrite the one generation with a
//! copy of what is already there. That is a download that is byte-for-byte the local database,
//! and a download of the same version of the shared state object as the last swap put in
//! place (the local database is then that version plus rows recorded only here, which are
//! carried again). So `<db>.prev` is the local database as it was before the last pull that
//! brought in a different version of the shared history.

use std::fs;
use std::io;
use std::path::Path;

/// Where the previous local database is kept.
pub(crate) fn path(db_path: &str) -> String {
    format!("{db_path}.prev")
}

fn tmp_path(db_path: &str) -> String {
    format!("{db_path}.prev.tmp")
}

/// The local database file under its second name, between [`Kept::stage`] and
/// [`Kept::publish`]. Dropped without being published (the swap did not happen), the second
/// name is removed and `<db>.prev` is as it was.
pub(crate) struct Kept {
    tmp: String,
    target: String,
    published: bool,
}

impl Kept {
    /// Step 1. The caller holds the database's lock, and the database's log is folded in.
    pub(crate) fn stage(db_path: &str) -> io::Result<Kept> {
        let kept = Kept {
            tmp: tmp_path(db_path),
            target: path(db_path),
            published: false,
        };
        // Left by a pull that died; only ever a second name or a copy, never the database.
        remove_if_there(&kept.tmp)?;
        if fs::hard_link(db_path, &kept.tmp).is_err() {
            // No hard links here (some network and FAT filesystems): a copy, on disk before
            // the file it copies is unlinked.
            remove_if_there(&kept.tmp)?;
            fs::copy(db_path, &kept.tmp)?;
            fs::File::open(&kept.tmp)?.sync_all()?;
        }
        Ok(kept)
    }

    /// Step 3, after the swap: the replaced file becomes `<db>.prev`.
    pub(crate) fn publish(mut self) -> io::Result<()> {
        fs::rename(&self.tmp, &self.target)?;
        self.published = true;
        if let Some(dir) = Path::new(&self.target).parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            // Best effort: the rename is visible; this makes it survive a power cut.
            fs::File::open(dir).and_then(|d| d.sync_all()).ok();
        }
        Ok(())
    }
}

/// Remove a `<db>.prev.tmp` left by a pull that was killed, for a pull that keeps nothing
/// itself. The caller holds the database's lock, so no other pull is between its steps.
pub(crate) fn remove_leftover(db_path: &str) {
    let _ = remove_if_there(&tmp_path(db_path));
}

impl Drop for Kept {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

fn remove_if_there(path: &str) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(dir: &tempfile::TempDir) -> String {
        dir.path().join("m.db").to_string_lossy().to_string()
    }

    #[test]
    fn the_replaced_file_becomes_prev_only_once_published() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        fs::write(&db, b"generation 1").unwrap();
        fs::write(path(&db), b"generation 0").unwrap();

        let kept = Kept::stage(&db).unwrap();
        // Staged: `.prev` is still the generation before.
        assert_eq!(fs::read(path(&db)).unwrap(), b"generation 0");
        // The swap: a new file takes the database's name.
        let staged = format!("{db}.pull");
        fs::write(&staged, b"generation 2").unwrap();
        fs::rename(&staged, &db).unwrap();
        assert_eq!(fs::read(path(&db)).unwrap(), b"generation 0");

        kept.publish().unwrap();
        assert_eq!(fs::read(path(&db)).unwrap(), b"generation 1");
        assert_eq!(fs::read(&db).unwrap(), b"generation 2");
        assert!(!Path::new(&tmp_path(&db)).exists());
    }

    #[test]
    fn a_swap_that_does_not_happen_leaves_prev_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        fs::write(&db, b"generation 1").unwrap();
        fs::write(path(&db), b"generation 0").unwrap();
        drop(Kept::stage(&db).unwrap());
        assert_eq!(fs::read(path(&db)).unwrap(), b"generation 0");
        assert!(!Path::new(&tmp_path(&db)).exists());
        assert_eq!(fs::read(&db).unwrap(), b"generation 1");
    }

    /// What a pull killed after staging leaves is a second name for the live database. The
    /// next pull must drop that name, not publish it and not write through it.
    #[test]
    fn a_leftover_second_name_is_dropped_and_the_live_database_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        fs::write(&db, b"generation 1").unwrap();
        std::mem::forget(Kept::stage(&db).unwrap());
        assert!(Path::new(&tmp_path(&db)).exists());
        assert!(!Path::new(&path(&db)).exists());

        // The database moves on (the next pull replaces it), then the one after stages again.
        let staged = format!("{db}.pull");
        fs::write(&staged, b"generation 2").unwrap();
        let kept = Kept::stage(&db).unwrap();
        fs::rename(&staged, &db).unwrap();
        kept.publish().unwrap();
        assert_eq!(fs::read(path(&db)).unwrap(), b"generation 1");
        assert_eq!(fs::read(&db).unwrap(), b"generation 2");
    }

    #[test]
    fn a_leftover_is_removed_without_touching_the_database_or_prev() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        fs::write(&db, b"generation 1").unwrap();
        fs::write(path(&db), b"generation 0").unwrap();
        std::mem::forget(Kept::stage(&db).unwrap());
        remove_leftover(&db);
        assert!(!Path::new(&tmp_path(&db)).exists());
        assert_eq!(fs::read(&db).unwrap(), b"generation 1");
        assert_eq!(fs::read(path(&db)).unwrap(), b"generation 0");
        remove_leftover(&db); // nothing there: nothing happens
    }

    #[cfg(unix)]
    #[test]
    fn staging_reads_and_writes_nothing_where_hard_links_exist() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        fs::write(&db, b"generation 1").unwrap();
        let _kept = Kept::stage(&db).unwrap();
        let (a, b) = (
            fs::metadata(&db).unwrap(),
            fs::metadata(tmp_path(&db)).unwrap(),
        );
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
    }

    #[test]
    fn there_is_nothing_to_stage_without_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        assert!(Kept::stage(&db).is_err());
        assert!(!Path::new(&tmp_path(&db)).exists());
        assert!(!Path::new(&path(&db)).exists());
    }
}
