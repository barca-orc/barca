//! Whether the process that started a `running` run still exists.
//!
//! A run row records the coordinator's pid and host name (#220), and a `running` row on this
//! host whose pid is gone is reported as `interrupted`. That fails in a container: the
//! process is usually pid 1, the next start is pid 1 again and so "alive", and the host name
//! is a new one on every start, so the row is "another machine's". The run stayed `running`
//! for ever (#290).
//!
//! So a process that starts runs also holds an exclusive lock on a file of its own for as
//! long as it lives, `run-owners/<owner>.lock` next to the metadata database, and each run
//! row records that owner id. The kernel releases the lock when the process ends, however it
//! ended and whatever its pid was. Asking whether an owner is alive is asking whether its
//! lock is held ([`probe`]):
//!
//! - the file is there and locked: the owner is alive, in this container or in another one
//!   that mounts the same directory;
//! - the file is there and not locked: the owner is gone, and its `running` runs were
//!   interrupted;
//! - there is no such file: the run was not started in this directory. It came with the
//!   shared history from another machine, and nothing here can tell whether it is still
//!   going, so it is left as it is.
//!
//! The last case is what keeps one machine from marking another machine's live run: an owner
//! id is random, and its lock file only ever exists in the directory its process ran in.
//!
//! This asks of the filesystem what the database's own cross-process lock
//! (`metadata.db.lock`) already asks: that a lock taken with `flock` by one process is seen
//! by the others that use the directory.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// The directory of owner lock files, next to the metadata database.
const DIR: &str = "run-owners";

/// What is known about the process that owns a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    /// Its lock is held: the process exists.
    Alive,
    /// Its lock file is here and nobody holds it: the process is gone.
    Gone,
    /// No lock file for it here: the run was started somewhere else.
    NotHere,
}

fn dir_for(db_path: &str) -> PathBuf {
    Path::new(db_path)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(DIR)
}

/// This process's owner id: 32 hex digits, chosen at random once per process.
fn process_owner() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        // `RandomState` is keyed from the operating system's random source.
        let random = |salt: u64| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(salt);
            hasher.finish()
        };
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        format!(
            "{:016x}{:016x}",
            random(nanos),
            random(u64::from(std::process::id()))
        )
    })
}

/// True for a string that can be an owner id. Ids are read back from the database, which may
/// have come from anywhere, and become part of a file name.
fn is_owner_id(owner: &str) -> bool {
    owner.len() == 32 && owner.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Make sure this process holds its owner lock in the directory of `db_path`, and return its
/// owner id to record on the runs it starts there. The lock is held until the process ends.
///
/// `None` when the lock cannot be taken or does not exclude anybody (a read-only directory,
/// a filesystem without working locks): the run then records no owner and is judged by its
/// pid and host, as before.
pub(crate) fn claim(db_path: &str) -> Option<String> {
    /// The lock files this process holds, by directory. Never emptied: closing a file would
    /// release its lock.
    static HELD: Mutex<Option<HashMap<PathBuf, File>>> = Mutex::new(None);

    let owner = process_owner();
    let dir = dir_for(db_path);
    let path = dir.join(format!("{owner}.lock"));
    let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let held = held.get_or_insert_with(HashMap::new);
    if held.contains_key(&dir) && path.exists() {
        return Some(owner.to_string());
    }
    let file = lock_new_file(&dir, &path, owner)?;
    held.insert(dir, file);
    Some(owner.to_string())
}

/// Create `path` already locked: the file is made under another name, locked, and only then
/// given its name, so nobody ever finds an owner's file without its lock.
fn lock_new_file(dir: &Path, path: &Path, owner: &str) -> Option<File> {
    fs::create_dir_all(dir).ok()?;
    let staged = dir.join(format!(".{owner}.tmp"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&staged)
        .ok()?;
    // The lock has to be one that others can see. On a filesystem where taking a lock always
    // succeeds, every owner would look gone: there the file is not used at all.
    let visible = file.try_lock().is_ok() && lock_state(&staged) == Owner::Alive;
    if !visible || fs::rename(&staged, path).is_err() {
        fs::remove_file(&staged).ok();
        return None;
    }
    Some(file)
}

/// Whether the process with this owner id exists, as far as the directory of `db_path` shows.
/// Reads only: it creates and changes nothing.
pub(crate) fn probe(db_path: &str, owner: &str) -> Owner {
    if !is_owner_id(owner) {
        return Owner::NotHere;
    }
    lock_state(&dir_for(db_path).join(format!("{owner}.lock")))
}

fn lock_state(path: &Path) -> Owner {
    let Ok(file) = File::open(path) else {
        return Owner::NotHere;
    };
    // A shared lock, so two readers asking at once do not take each other for the owner.
    // It is released when `file` is dropped.
    match file.try_lock_shared() {
        Ok(()) => Owner::Gone,
        Err(TryLockError::WouldBlock) => Owner::Alive,
        // Locks do not work here: nothing can be concluded.
        Err(TryLockError::Error(_)) => Owner::NotHere,
    }
}

/// How old a staged lock file must be before it is taken for one its process left behind.
const STAGED_GRACE: Duration = Duration::from_secs(60);

/// Remove the lock files of processes that are gone, except those in `still_named`: the
/// owners of rows that are still `running`, whose file is the evidence that they are not.
/// Called by a run, after it has recorded what it found.
pub(crate) fn sweep(db_path: &str, still_named: &HashSet<String>) {
    let Ok(entries) = fs::read_dir(dir_for(db_path)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let stale = if let Some(owner) = name.strip_suffix(".lock") {
            is_owner_id(owner) && !still_named.contains(owner)
        } else if name.starts_with('.') && name.ends_with(".tmp") {
            // Left by a process that died between creating the file and naming it.
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age > STAGED_GRACE)
        } else {
            false
        };
        if stale && lock_state(&path) == Owner::Gone {
            fs::remove_file(&path).ok();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Take the owner lock of some other process in the directory of `db_path`: the returned
    /// file is that process, alive until it is dropped.
    pub(crate) fn other_process(db_path: &str, owner: &str) -> File {
        assert!(is_owner_id(owner), "{owner}");
        let dir = dir_for(db_path);
        lock_new_file(&dir, &dir.join(format!("{owner}.lock")), owner).expect("an owner lock")
    }

    pub(crate) const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    pub(crate) const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn db_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("metadata.db").display().to_string()
    }

    #[test]
    fn an_owner_is_alive_while_its_lock_is_held_and_gone_once_released() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        // Never seen here: nothing can be said about it.
        assert_eq!(probe(&db, A), Owner::NotHere);

        let process = other_process(&db, A);
        assert_eq!(probe(&db, A), Owner::Alive);
        // Asking does not disturb the lock, and another reader asking at once agrees.
        assert_eq!(probe(&db, A), Owner::Alive);
        assert_eq!(probe(&db, B), Owner::NotHere);

        // The process ends: the kernel closes its files, which is all that happens here.
        drop(process);
        assert_eq!(probe(&db, A), Owner::Gone);
        assert_eq!(
            probe(&db, A),
            Owner::Gone,
            "asking leaves the file in place"
        );
    }

    #[test]
    fn this_process_is_alive_wherever_it_has_claimed() {
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let owner = claim(&db_in(&one)).unwrap();
        assert!(is_owner_id(&owner), "{owner}");
        assert_eq!(claim(&db_in(&one)).unwrap(), owner, "one id per process");
        assert_eq!(probe(&db_in(&one), &owner), Owner::Alive);
        // A directory it started no run in holds no lock of it.
        assert_eq!(probe(&db_in(&two), &owner), Owner::NotHere);
        assert_eq!(claim(&db_in(&two)).unwrap(), owner);
        assert_eq!(probe(&db_in(&two), &owner), Owner::Alive);
        // No staged file is left behind.
        let names: Vec<String> = fs::read_dir(dir_for(&db_in(&one)))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [format!("{owner}.lock")]);
    }

    #[test]
    fn an_owner_id_from_the_database_cannot_name_a_file_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secret.lock"), "").unwrap();
        fs::create_dir_all(dir_for(&db_in(&dir))).unwrap();
        for owner in ["../secret", "", "zz", &"g".repeat(32), &format!("{A}/../x")] {
            assert_eq!(probe(&db_in(&dir), owner), Owner::NotHere, "{owner:?}");
        }
    }

    #[test]
    fn sweep_removes_the_files_of_gone_owners_that_no_running_row_names() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let (gone, named, alive) = (A, B, "cccccccccccccccccccccccccccccccc");
        drop(other_process(&db, gone));
        drop(other_process(&db, named));
        let _process = other_process(&db, alive);
        let mine = claim(&db).unwrap();
        fs::write(dir_for(&db).join("notes.txt"), "not ours").unwrap();

        sweep(&db, &HashSet::from([named.to_string()]));

        let mut left: Vec<String> = fs::read_dir(dir_for(&db))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut want = vec![
            format!("{named}.lock"),
            format!("{alive}.lock"),
            format!("{mine}.lock"),
            "notes.txt".to_string(),
        ];
        want.sort();
        assert_eq!(left, want);
        // The one a `running` row still names is still evidence that its process is gone.
        assert_eq!(probe(&db, named), Owner::Gone);
    }
}
