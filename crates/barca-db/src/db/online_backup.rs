//! SQLite's incremental backup interface, using the very same SQLite VFS as libsql.
use super::*;
use libsql::ffi;
use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;

struct Connection(*mut ffi::sqlite3);

impl Connection {
    fn open(path: &Path, flags: i32) -> Result<Self, BarcaError> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| BarcaError::Db("invalid backup path".into()))?;
        let mut handle = std::ptr::null_mut();
        // SQLite returns an owned handle (possibly null on OOM) even on failure.
        let rc = unsafe {
            ffi::sqlite3_open_v2(
                path.as_ptr(),
                &mut handle,
                flags | ffi::SQLITE_OPEN_FULLMUTEX,
                std::ptr::null(),
            )
        };
        let conn = Self(handle);
        if rc != ffi::SQLITE_OK {
            return Err(conn.error());
        }
        Ok(conn)
    }

    fn error(&self) -> BarcaError {
        // sqlite3_errmsg accepts a null handle (out of memory).
        let message = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.0)) };
        BarcaError::Db(format!(
            "online metadata backup: {}",
            message.to_string_lossy()
        ))
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_close(self.0);
        }
    }
}

pub(super) struct Backup {
    handle: *mut ffi::sqlite3_backup,
    dest: Connection,
    _source: Connection,
    copied_pages: i32,
    restarts: u32,
    activity: super::read_activity::BackupReader,
}
// Exclusive ownership, serialized connections, and no shared access across tasks.
unsafe impl Send for Backup {}

impl Backup {
    pub(super) async fn open(source: &Path, dest: &Path) -> Result<Self, BarcaError> {
        initialize_libsql().await?;
        let activity = super::read_activity::BackupReader::new(source);
        let source = Connection::open(source, ffi::SQLITE_OPEN_READONLY)?;
        let dest = Connection::open(dest, ffi::SQLITE_OPEN_READWRITE)?;
        let handle = unsafe {
            ffi::sqlite3_backup_init(dest.0, c"main".as_ptr(), source.0, c"main".as_ptr())
        };
        if handle.is_null() {
            return Err(dest.error());
        }
        Ok(Self {
            handle,
            dest,
            _source: source,
            copied_pages: 0,
            restarts: 0,
            activity,
        })
    }

    pub(super) fn step(&mut self) -> Result<i32, BarcaError> {
        // No enclosing read transaction: SQLite releases the source lock on return.
        let pages = if self.restarts >= 3 { -1 } else { 64 };
        let _active = self.activity.stepping();
        let rc = unsafe { ffi::sqlite3_backup_step(self.handle, pages) };
        if rc == ffi::SQLITE_OK {
            let copied = unsafe {
                ffi::sqlite3_backup_pagecount(self.handle)
                    - ffi::sqlite3_backup_remaining(self.handle)
            };
            // An external commit restarts SQLite at page one. Ordinary steps
            // always advance until DONE, even when the database grows.
            if copied <= self.copied_pages {
                self.restarts += 1;
                barca_model::errln!(
                    "[barca] metadata backup restarted {} time(s){}",
                    self.restarts,
                    if self.restarts == 3 {
                        "; finishing in a single step"
                    } else {
                        ""
                    }
                );
            }
            self.copied_pages = copied;
        }
        match rc {
            ffi::SQLITE_DONE | ffi::SQLITE_OK | ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED => Ok(rc),
            _ => Err(self.dest.error()),
        }
    }
}
impl Drop for Backup {
    fn drop(&mut self) {
        // Exactly once, before either of the owned connections closes.
        unsafe {
            ffi::sqlite3_backup_finish(self.handle);
        }
    }
}
