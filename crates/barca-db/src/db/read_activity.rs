//! The local driver's public Connection does not expose a progress handler. SQLite's
//! connection-init hook gives us that handle during the synchronous ReadView open only.
use super::*;
use libsql::ffi;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};

thread_local! { static OPENING: std::cell::Cell<Option<FileId>> = const { std::cell::Cell::new(None) }; }
static INIT: OnceLock<i32> = OnceLock::new();
static READERS: LazyLock<Mutex<Vec<Weak<Reader>>>> = LazyLock::new(Default::default);
const READ_BUDGET: Duration = Duration::from_secs(30);

struct Reader {
    id: FileId,
    active: Mutex<Option<(Instant, &'static str)>>,
}

pub(super) fn connect(db: &libsql::Database, path: &str) -> Result<libsql::Connection, BarcaError> {
    if *INIT.get_or_init(|| unsafe { ffi::sqlite3_auto_extension(Some(opened)) }) != ffi::SQLITE_OK
    {
        return Err(BarcaError::Db(
            "could not register SQLite reader diagnostics".into(),
        ));
    }
    OPENING.set(file_id(path));
    let conn = db.connect();
    OPENING.set(None);
    conn.map_err(db_open_error)
}

pub(super) fn active(path: &str) -> Option<String> {
    let id = file_id(path)?;
    let readers = READERS.lock().unwrap();
    readers
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|r| r.id == id)
        .filter_map(|r| *r.active.lock().unwrap())
        .min_by_key(|(start, _)| *start)
        .map(|(_, name)| format!("active reader in this process: {name}"))
}

unsafe extern "C" fn opened(
    db: *mut ffi::sqlite3,
    _: *mut *const c_char,
    _: *const ffi::sqlite3_api_routines,
) -> c_int {
    let Some(id) = OPENING.get() else {
        return ffi::SQLITE_OK;
    };
    let reader = Arc::new(Reader {
        id,
        active: Mutex::new(None),
    });
    let mut readers = READERS.lock().unwrap();
    readers.retain(|r| r.strong_count() > 0);
    readers.push(Arc::downgrade(&reader));
    drop(readers);
    // SQLite owns this reference until TRACE_CLOSE. Both callbacks run under
    // its serialized connection mutex and use it only for this connection.
    let context = Arc::into_raw(reader).cast_mut().cast();
    unsafe {
        ffi::sqlite3_trace_v2(
            db,
            (ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_CLOSE) as u32,
            Some(trace),
            context,
        );
        ffi::sqlite3_progress_handler(db, 4096, Some(expired), context);
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn expired(context: *mut c_void) -> c_int {
    let reader = unsafe { &*context.cast::<Reader>() };
    i32::from(
        reader
            .active
            .lock()
            .unwrap()
            .is_some_and(|(t, _)| t.elapsed() >= READ_BUDGET),
    )
}

unsafe extern "C" fn trace(
    event: u32,
    context: *mut c_void,
    subject: *mut c_void,
    _: *mut c_void,
) -> c_int {
    if event == ffi::SQLITE_TRACE_CLOSE as u32 {
        unsafe {
            let db = subject.cast::<ffi::sqlite3>();
            ffi::sqlite3_progress_handler(db, 0, None, std::ptr::null_mut());
            ffi::sqlite3_trace_v2(db, 0, None, std::ptr::null_mut());
            drop(Arc::from_raw(context.cast::<Reader>()));
        }
        return 0;
    }
    let reader = unsafe { &*context.cast::<Reader>() };
    let stmt = subject.cast::<ffi::sqlite3_stmt>();
    let mut active = reader.active.lock().unwrap();
    if event == ffi::SQLITE_TRACE_STMT as u32 {
        // Fixed labels: health must never contain query arguments or user data.
        let sql = unsafe { ffi::sqlite3_sql(stmt) };
        let sql = if sql.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(sql) }.to_str().unwrap_or("")
        };
        let name = if sql.contains("materializations") {
            "ReadView materializations"
        } else if sql.contains("runs") {
            "ReadView runs"
        } else {
            "ReadView metadata"
        };
        let entry = active.get_or_insert_with(|| (Instant::now(), name));
        entry.1 = name;
        // A transaction can also consist entirely of statements too short to
        // invoke the VM progress handler. Always let transaction cleanup run.
        if entry.0.elapsed() >= READ_BUDGET && !matches!(sql, "COMMIT" | "ROLLBACK") {
            unsafe {
                ffi::sqlite3_interrupt(ffi::sqlite3_db_handle(stmt));
            }
        }
    } else if unsafe { ffi::sqlite3_get_autocommit(ffi::sqlite3_db_handle(stmt)) } != 0 {
        // Another cursor on this connection can still be between rows.
        let db = unsafe { ffi::sqlite3_db_handle(stmt) };
        let mut other = std::ptr::null_mut();
        loop {
            other = unsafe { ffi::sqlite3_next_stmt(db, other) };
            if other.is_null() {
                *active = None;
                break;
            }
            if other != stmt && unsafe { ffi::sqlite3_stmt_busy(other) } != 0 {
                break;
            }
        }
    }
    0
}

/// The local driver returns ready futures for SQLite calls. Keep a consistent
/// multi-query snapshot, but reject any future added to its body that would
/// suspend on a scheduler, filesystem operation, or response stream.
pub(super) fn local_read<T>(
    work: impl std::future::Future<Output = Result<T, BarcaError>>,
) -> Result<T, BarcaError> {
    use std::task::{Context, Poll, Waker};
    match std::pin::pin!(work).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result,
        Poll::Pending => Err(BarcaError::Db(
            "metadata snapshot attempted to suspend".into(),
        )),
    }
}
