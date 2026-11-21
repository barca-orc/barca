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
}
// Exclusive ownership, serialized connections, and no shared access across tasks.
unsafe impl Send for Backup {}

impl Backup {
    pub(super) async fn open(source: &Path, dest: &Path) -> Result<Self, BarcaError> {
        initialize_libsql().await?;
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
        })
    }

    pub(super) fn step(&mut self) -> Result<i32, BarcaError> {
        // No enclosing read transaction: SQLite releases the source lock on return.
        let rc = unsafe { ffi::sqlite3_backup_step(self.handle, 64) };
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
