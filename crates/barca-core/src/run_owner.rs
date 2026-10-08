//! Whether the process that started a `running` run is gone, decided only from what this
//! process can observe for certain.
//!
//! A run row records the coordinator's pid and host name (#220): a `running` row on this host
//! whose pid is gone is `interrupted`. In a container that never fires (#290): the
//! coordinator is pid 1, the next start is pid 1 again ("alive"), and the host name changes
//! with every start ("another machine's run").
//!
//! The rule here has three answers ([`Owner`]), and the third is the default. A run is
//! `interrupted` only when its process is known to be gone. Whenever that cannot be
//! established, by this process, on this kernel, through this filesystem, the answer is
//! [`Owner::Unknown`] and the run stays `running`. A run that is going is never reported or
//! recorded as interrupted, whoever looks: a container at a run of its host, the host at a
//! run in a container, two containers at each other, another machine through the shared
//! history.
//!
//! A run records who started it ([`Identity`], in `runs.owner`), and a reader compares that
//! with itself ([`judge`]):
//!
//! 1. **Another kernel** (the boot ids differ). Nothing can be observed of a process on
//!    another kernel. One inference is made: when the machine id and the host name are both
//!    this machine's and the run's marker file is in this directory, the run was started
//!    here before a restart of the machine, and no process survives that.
//! 2. **This kernel, this pid namespace** (Linux; macOS has one). The process table is the
//!    one the run's process was in, so its pid can be looked up. No such process: gone. A
//!    process with that pid and another start time: gone, the pid was reused. The same start
//!    time: alive. A container that is restarted gets a new pid namespace, which often has
//!    the number of the one it replaces; a number is only handed out again after its
//!    namespace, and every process in it, is gone, so the lookup is right then as well.
//! 3. **This kernel, another pid namespace** (two containers, or a container and its Linux
//!    host). The run's process cannot be looked up. What both sides do share is the project
//!    directory: the owner holds the read end of a FIFO it created there
//!    (`run-owners/<token>.fifo`) for as long as it lives, and the kernel closes it when the
//!    process ends, however it ended. Opening a FIFO for writing without blocking fails with
//!    `ENXIO` exactly when no process has it open for reading. That is the kernel's own
//!    bookkeeping for one inode, so it is trusted only when the reader can show it reaches
//!    the inode the owner held: same device, same inode number, and on Linux the same file
//!    handle (`name_to_handle_at`), on macOS a local filesystem. Where that cannot be shown
//!    (Docker Desktop's bind mounts give no file handle) the FIFO is not consulted.
//!
//! Nothing here depends on a clock, so a suspended laptop, a stopped process (it keeps its
//! pid and its open files) or a clock step cannot make a live run look dead, and a killed
//! run reads `interrupted` at once.
//!
//! Readers never write: [`probe`] opens and closes, nothing else. The marker file is removed
//! by its owner when its runs have ended, and otherwise by a starting run ([`sweep`]) once no
//! `running` row names it, never on a judgement about liveness.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// The directory of owner markers, next to the metadata database.
const DIR: &str = "run-owners";

/// What this process can establish about the process that owns a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    /// It exists.
    Alive,
    /// It does not exist any more.
    Gone,
    /// Neither can be established from here. The run stays `running`.
    Unknown,
}

/// Who started a run: what the starting process knew about itself. Stored as JSON in
/// `runs.owner`. Every field may be empty or absent when it could not be read; a rule that
/// needs it then does not apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    /// The running kernel: random per boot, the same in every container on it.
    #[serde(default)]
    pub boot: String,
    /// The machine, across restarts. Empty in most containers.
    #[serde(default)]
    pub machine: String,
    /// The pid namespace (Linux), as `readlink /proc/self/ns/pid` gives it.
    #[serde(default)]
    pub pidns: String,
    /// The time namespace (Linux): process start times are shown relative to it.
    #[serde(default)]
    pub timens: String,
    /// The process's start time in clock ticks since boot (Linux, `/proc/<pid>/stat`).
    #[serde(default)]
    pub start: Option<u64>,
    /// The marker FIFO this process holds open in the project directory, if it has one.
    #[serde(default)]
    pub fifo: Option<FifoId>,
}

/// A marker FIFO as its owner saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FifoId {
    /// `run-owners/<token>.fifo`.
    pub token: String,
    pub dev: u64,
    pub ino: u64,
    /// The kernel's handle for the inode (Linux). `None` where the filesystem gives none.
    #[serde(default)]
    pub handle: Option<String>,
}

/// What the reading process is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Here {
    pub boot: String,
    pub machine: String,
    pub host: String,
    pub pidns: String,
    pub timens: String,
}

/// What a lookup of a pid in the reader's process table shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Process {
    /// No process has this pid.
    Absent,
    /// One has, started at this time (`None` when the start time cannot be read).
    Present(Option<u64>),
}

/// What the reader sees of a marker FIFO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Marker {
    /// No such file here.
    Missing,
    /// The file is here.
    Present {
        dev: u64,
        ino: u64,
        handle: Option<String>,
        /// Whether a process has it open for reading; `None` when that could not be asked.
        reader: Option<bool>,
        /// Whether device and inode number identify one inode on this filesystem, when no
        /// file handle is available (macOS: a local filesystem).
        local: bool,
    },
}

/// What [`judge`] asks of the machine. The real one is [`System`]; tests supply their own.
pub(crate) trait Observer {
    fn here(&self) -> Here;
    fn process(&self, pid: i64) -> Process;
    fn marker(&self, token: &str) -> Marker;
}

/// The rule (see the module docs). `pid` and `host` are the run row's.
pub(crate) fn judge(pid: Option<i64>, host: &str, id: &Identity, on: &dyn Observer) -> Owner {
    let here = on.here();
    if id.boot.is_empty() || here.boot.is_empty() {
        return Owner::Unknown;
    }
    let marker = || match &id.fifo {
        Some(fifo) => on.marker(&fifo.token),
        None => Marker::Missing,
    };

    // 1. Another kernel.
    if id.boot != here.boot {
        let this_machine = !id.machine.is_empty()
            && id.machine == here.machine
            && !host.is_empty()
            && host == here.host;
        return if this_machine && marker() != Marker::Missing {
            Owner::Gone
        } else {
            Owner::Unknown
        };
    }

    // 2. This kernel and this pid namespace: the process table answers. A namespace that
    // could not be read is not known to be the same one.
    if let Some(pid) = pid
        && !id.pidns.is_empty()
        && id.pidns == here.pidns
        && id.timens == here.timens
    {
        match (on.process(pid), id.start) {
            (Process::Absent, _) => return Owner::Gone,
            (Process::Present(Some(now)), Some(then)) => {
                return if now == then {
                    Owner::Alive
                } else {
                    Owner::Gone
                };
            }
            // A process has the pid and it cannot be told whether it is the same one.
            (Process::Present(_), _) => {}
        }
    }

    // 3. This kernel: the marker FIFO, when the reader reaches the inode the owner held.
    let Some(fifo) = &id.fifo else {
        return Owner::Unknown;
    };
    match marker() {
        Marker::Present {
            dev,
            ino,
            handle,
            reader: Some(reader),
            local,
        } if dev == fifo.dev && ino == fifo.ino => {
            let same_inode = match (&fifo.handle, &handle) {
                (Some(theirs), Some(ours)) => theirs == ours,
                (None, None) => local,
                _ => false,
            };
            match (same_inode, reader) {
                (true, true) => Owner::Alive,
                (true, false) => Owner::Gone,
                (false, _) => Owner::Unknown,
            }
        }
        _ => Owner::Unknown,
    }
}

/// Judge the owner recorded in `owner_json` (the `runs.owner` column). Anything that does
/// not parse is [`Owner::Unknown`].
pub(crate) fn judge_json(
    pid: Option<i64>,
    host: &str,
    owner_json: &str,
    on: &dyn Observer,
) -> Owner {
    match serde_json::from_str::<Identity>(owner_json) {
        Ok(id) => judge(pid, host, &id, on),
        Err(_) => Owner::Unknown,
    }
}

/// The marker token named by a `runs.owner` value, if any.
pub(crate) fn token_of(owner_json: &str) -> Option<String> {
    serde_json::from_str::<Identity>(owner_json)
        .ok()
        .and_then(|id| id.fifo)
        .map(|fifo| fifo.token)
        .filter(|token| is_token(token))
}

/// True for a string that can be a marker token. Tokens are read back from the database,
/// which may have come from anywhere, and become part of a file name.
fn is_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

fn dir_for(db_path: &str) -> PathBuf {
    Path::new(db_path)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(DIR)
}

fn marker_path(dir: &Path, token: &str) -> PathBuf {
    dir.join(format!("{token}.fifo"))
}

// ─── The real machine ─────────────────────────────────────────────────────────

/// What this process observes, for markers in the directory of one database.
pub(crate) struct System {
    dir: PathBuf,
}

impl System {
    pub(crate) fn for_db(db_path: &str) -> Self {
        Self {
            dir: dir_for(db_path),
        }
    }
}

impl Observer for System {
    fn here(&self) -> Here {
        Here {
            boot: sys::boot_id(),
            machine: sys::machine_id(),
            host: crate::db::local_host(),
            pidns: sys::namespace("pid"),
            timens: sys::namespace("time"),
        }
    }

    fn process(&self, pid: i64) -> Process {
        sys::process(pid)
    }

    fn marker(&self, token: &str) -> Marker {
        if !is_token(token) {
            return Marker::Missing;
        }
        sys::marker(&marker_path(&self.dir, token))
    }
}

/// Whether a process has the FIFO at `path` open for reading: `Some(true)`, `Some(false)`
/// (`ENXIO`), or `None` when the open failed for another reason. Opens for writing without
/// blocking and closes again; nothing is written.
fn has_reader(path: &Path) -> Option<bool> {
    use std::os::unix::fs::OpenOptionsExt;
    match fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(_) => Some(true),
        Err(e) if e.raw_os_error() == Some(libc::ENXIO) => Some(false),
        Err(_) => None,
    }
}

mod sys {
    use super::{Marker, Process, has_reader};
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::path::Path;

    #[cfg(not(target_os = "macos"))]
    fn read_trimmed(path: &str) -> String {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    #[cfg(target_os = "macos")]
    fn sysctl_string(name: &str) -> String {
        let Ok(name) = std::ffi::CString::new(name) else {
            return String::new();
        };
        let mut buf = [0u8; 128];
        let mut len = buf.len();
        // SAFETY: `buf` is `len` bytes long and the kernel writes at most `len` bytes.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return String::new();
        }
        let end = buf[..len].iter().position(|&b| b == 0).unwrap_or(len);
        String::from_utf8_lossy(&buf[..end]).trim().to_string()
    }

    /// Identifies the running kernel: random per boot.
    pub(super) fn boot_id() -> String {
        #[cfg(target_os = "macos")]
        {
            sysctl_string("kern.bootsessionuuid")
        }
        #[cfg(not(target_os = "macos"))]
        {
            read_trimmed("/proc/sys/kernel/random/boot_id")
        }
    }

    /// Identifies the machine across restarts, where it has such an id.
    pub(super) fn machine_id() -> String {
        #[cfg(target_os = "macos")]
        {
            sysctl_string("kern.uuid")
        }
        #[cfg(not(target_os = "macos"))]
        {
            read_trimmed("/etc/machine-id")
        }
    }

    /// A namespace of this process (`pid`, `time`), as the kernel names it; empty when it
    /// cannot be read. macOS has one process table for everything: its pid namespace is
    /// `host`, and it has no other.
    pub(super) fn namespace(kind: &str) -> String {
        if cfg!(target_os = "macos") {
            return if kind == "pid" { "host" } else { "" }.to_string();
        }
        std::fs::read_link(format!("/proc/self/ns/{kind}"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// True when `/proc` shows this process's own pid namespace: the pid `/proc/self` gives
    /// is the one `getpid` gives, and the process has no other (`NSpid` lists one number
    /// per nested namespace, outermost first as `/proc` sees it). A `/proc` mounted for
    /// another namespace numbers processes differently, and looking a pid up in it would
    /// find some other process.
    fn proc_is_ours() -> bool {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return false;
        };
        let me = std::process::id().to_string();
        status
            .lines()
            .find_map(|line| line.strip_prefix("NSpid:"))
            .is_some_and(|pids| pids.split_whitespace().eq([me.as_str()]))
    }

    /// The start time of a process, in clock ticks since boot (Linux only), when `/proc`
    /// can be trusted to mean the same pids as this process does.
    pub(super) fn start_time(pid: i64) -> Option<u64> {
        if !proc_is_ours() {
            return None;
        }
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The second field is the command in parentheses and may itself contain spaces and
        // parentheses; the start time is the 20th field after it.
        let after = &stat[stat.rfind(')')? + 1..];
        after.split_whitespace().nth(19)?.parse().ok()
    }

    pub(super) fn process(pid: i64) -> Process {
        let Ok(raw) = libc::pid_t::try_from(pid) else {
            return Process::Present(None);
        };
        if raw <= 0 {
            return Process::Present(None);
        }
        // SAFETY: signal 0 sends nothing; it only checks that the pid exists.
        let rc = unsafe { libc::kill(raw, 0) };
        let absent = rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if absent {
            Process::Absent
        } else {
            Process::Present(start_time(pid))
        }
    }

    /// The kernel's handle for the inode at `path` (Linux): on every filesystem that has
    /// one it names the inode itself, where an inode number may not.
    #[cfg(target_os = "linux")]
    pub(super) fn file_handle(path: &Path) -> Option<String> {
        use std::os::unix::ffi::OsStrExt;
        const MAX: usize = 128;
        // struct file_handle { unsigned int handle_bytes; int handle_type; char f_handle[]; }
        #[repr(C)]
        struct Handle {
            bytes: libc::c_uint,
            kind: libc::c_int,
            data: [u8; MAX],
        }
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut handle = Handle {
            bytes: MAX as libc::c_uint,
            kind: 0,
            data: [0; MAX],
        };
        let mut mount_id: libc::c_int = 0;
        // SAFETY: `handle` has room for the `MAX` bytes it announces, and the path is a
        // NUL-terminated string.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_name_to_handle_at,
                libc::AT_FDCWD,
                c_path.as_ptr(),
                &mut handle as *mut Handle,
                &mut mount_id as *mut libc::c_int,
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let len = (handle.bytes as usize).min(MAX);
        let hex: String = handle.data[..len]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Some(format!("{}:{hex}", handle.kind))
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn file_handle(_path: &Path) -> Option<String> {
        None
    }

    /// Whether `path` is on a filesystem of this machine's own disks (macOS), where a device
    /// and inode number name one inode. Linux relies on the file handle instead.
    #[cfg(target_os = "macos")]
    pub(super) fn is_local(path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `statfs` fills the struct it is given; the path is NUL-terminated.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut st) };
        rc == 0 && (st.f_flags & libc::MNT_LOCAL as u32) != 0
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn is_local(_path: &Path) -> bool {
        false
    }

    pub(super) fn marker(path: &Path) -> Marker {
        let Ok(before) = std::fs::symlink_metadata(path) else {
            return Marker::Missing;
        };
        if !before.file_type().is_fifo() {
            // Something else has the name: nothing can be asked of it.
            return Marker::Present {
                dev: before.dev(),
                ino: before.ino(),
                handle: None,
                reader: None,
                local: false,
            };
        }
        let handle = file_handle(path);
        let reader = has_reader(path);
        // The same file before and after the question, or the answer is about another one.
        let unchanged = std::fs::symlink_metadata(path)
            .is_ok_and(|after| after.dev() == before.dev() && after.ino() == before.ino());
        Marker::Present {
            dev: before.dev(),
            ino: before.ino(),
            handle,
            reader: if unchanged { reader } else { None },
            local: is_local(path),
        }
    }
}

// ─── The owner's side ─────────────────────────────────────────────────────────

/// What this process holds for the runs it has in flight in one directory.
struct Session {
    /// The `runs.owner` value of its runs.
    owner_json: String,
    token: Option<String>,
    /// The read end of the marker FIFO. Closed (by the kernel, at the latest) when the
    /// process ends.
    _reader: Option<File>,
    runs: HashSet<String>,
}

static SESSIONS: Mutex<Option<HashMap<PathBuf, Session>>> = Mutex::new(None);

/// A random 32-digit hex token.
fn new_token() -> String {
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
}

/// Create the marker FIFO and hold its read end. `None` when the directory cannot hold one
/// or a FIFO does not behave as one here: the run is then judged without it.
fn hold_marker(dir: &Path) -> Option<(FifoId, File)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    fs::create_dir_all(dir).ok()?;
    let token = new_token();
    let path = marker_path(dir, &token);
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: the path is a NUL-terminated string.
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) } != 0 {
        return None;
    }
    let held = (|| {
        // Before anybody reads it, a writer must be refused; once this process does, a
        // writer must get in. Otherwise the file says nothing about its owner.
        if has_reader(&path) != Some(false) {
            return None;
        }
        // `O_NONBLOCK`: opening the read end of a FIFO otherwise waits for a writer. The
        // standard library opens with `O_CLOEXEC`, so no child process inherits it.
        let reader = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .ok()?;
        if has_reader(&path) != Some(true) {
            return None;
        }
        let meta = fs::symlink_metadata(&path).ok()?;
        let id = FifoId {
            token,
            dev: meta.dev(),
            ino: meta.ino(),
            handle: sys::file_handle(&path),
        };
        Some((id, reader))
    })();
    if held.is_none() {
        fs::remove_file(&path).ok();
    }
    held
}

/// This process as the owner of a run it is about to start in the directory of `db_path`:
/// the value for `runs.owner`. Empty when not even the kernel can be identified; such a run
/// is judged by pid and host name, as before.
///
/// The first run of a process in a directory creates the marker; further runs share it.
pub(crate) fn claim(db_path: &str, run_id: &str) -> String {
    let dir = dir_for(db_path);
    let mut sessions = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
    let sessions = sessions.get_or_insert_with(HashMap::new);
    if let Some(session) = sessions.get_mut(&dir) {
        let marker_there = session
            .token
            .as_ref()
            .is_none_or(|token| marker_path(&dir, token).exists());
        if marker_there {
            session.runs.insert(run_id.to_string());
            return session.owner_json.clone();
        }
        // Somebody removed the marker: start over with a new one.
        sessions.remove(&dir);
    }
    let boot = sys::boot_id();
    if boot.is_empty() {
        return String::new();
    }
    let held = hold_marker(&dir);
    let identity = Identity {
        boot,
        machine: sys::machine_id(),
        pidns: sys::namespace("pid"),
        timens: sys::namespace("time"),
        start: sys::start_time(i64::from(std::process::id())),
        fifo: held.as_ref().map(|(id, _)| id.clone()),
    };
    let owner_json = serde_json::to_string(&identity).unwrap_or_default();
    sessions.insert(
        dir,
        Session {
            owner_json: owner_json.clone(),
            token: held.as_ref().map(|(id, _)| id.token.clone()),
            _reader: held.map(|(_, reader)| reader),
            runs: HashSet::from([run_id.to_string()]),
        },
    );
    owner_json
}

/// A run of this process has recorded its outcome. When it was the last one in flight in
/// its directory, the marker is removed: its owner needs no witness any more.
pub(crate) fn release(db_path: &str, run_id: &str) {
    let dir = dir_for(db_path);
    let mut sessions = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
    let Some(sessions) = sessions.as_mut() else {
        return;
    };
    let Some(session) = sessions.get_mut(&dir) else {
        return;
    };
    session.runs.remove(run_id);
    if !session.runs.is_empty() {
        return;
    }
    if let Some(session) = sessions.remove(&dir)
        && let Some(token) = &session.token
    {
        fs::remove_file(marker_path(&dir, token)).ok();
    }
}

/// How old a marker must be before it is removed for having no `running` row. A marker is
/// created an instant before its first row is written; this is that instant, with room.
const UNNAMED_GRACE: Duration = Duration::from_secs(3600);

/// Remove the markers no `running` row names (`named`) and that are older than
/// [`UNNAMED_GRACE`] as of `now`. Called by a starting run.
///
/// No judgement about anybody's liveness is involved, so it cannot be wrong about a live
/// run: a run in flight has a `running` row that names its marker, and that marker stays
/// for as long as the row says `running`, whoever sweeps.
pub(crate) fn sweep(db_path: &str, named: &HashSet<String>, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir_for(db_path)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(token) = name.to_str().and_then(|n| n.strip_suffix(".fifo")) else {
            continue;
        };
        if !is_token(token) || named.contains(token) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > UNNAMED_GRACE);
        if old {
            fs::remove_file(entry.path()).ok();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const T: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// A machine whose answers are given.
    #[derive(Clone)]
    pub(crate) struct Fake {
        pub here: Here,
        pub process: Process,
        pub marker: Marker,
    }

    impl Observer for Fake {
        fn here(&self) -> Here {
            self.here.clone()
        }
        fn process(&self, _pid: i64) -> Process {
            self.process
        }
        fn marker(&self, _token: &str) -> Marker {
            self.marker.clone()
        }
    }

    fn here() -> Here {
        Here {
            boot: "boot-1".into(),
            machine: "machine-1".into(),
            host: "host-1".into(),
            pidns: "pid:[1]".into(),
            timens: "time:[1]".into(),
        }
    }

    /// A run started by pid 7, started at tick 500, in `here()`, holding marker `T`.
    pub(crate) fn identity() -> Identity {
        Identity {
            boot: "boot-1".into(),
            machine: "machine-1".into(),
            pidns: "pid:[1]".into(),
            timens: "time:[1]".into(),
            start: Some(500),
            fifo: Some(FifoId {
                token: T.into(),
                dev: 10,
                ino: 20,
                handle: Some("1:abcd".into()),
            }),
        }
    }

    fn marker(reader: Option<bool>) -> Marker {
        Marker::Present {
            dev: 10,
            ino: 20,
            handle: Some("1:abcd".into()),
            reader,
            local: false,
        }
    }

    fn fake(process: Process, marker: Marker) -> Fake {
        Fake {
            here: here(),
            process,
            marker,
        }
    }

    fn verdict(id: &Identity, on: &Fake) -> Owner {
        judge(Some(7), "host-1", id, on)
    }

    #[test]
    fn in_the_same_pid_namespace_the_process_table_decides() {
        let id = identity();
        // Whatever the marker says: the lookup is the stronger evidence.
        for m in [marker(Some(true)), marker(Some(false)), Marker::Missing] {
            assert_eq!(verdict(&id, &fake(Process::Absent, m.clone())), Owner::Gone);
            let same = Process::Present(Some(500));
            assert_eq!(verdict(&id, &fake(same, m.clone())), Owner::Alive);
            // The pid belongs to a later process: in a container, the next pid 1.
            let reused = Process::Present(Some(900));
            assert_eq!(verdict(&id, &fake(reused, m)), Owner::Gone);
        }
    }

    #[test]
    fn a_pid_whose_start_time_cannot_be_compared_does_not_decide() {
        let mut id = identity();
        let present = Process::Present(None);
        assert_eq!(
            verdict(&id, &fake(present, Marker::Missing)),
            Owner::Unknown
        );
        assert_eq!(
            verdict(&id, &fake(present, marker(Some(true)))),
            Owner::Alive
        );
        assert_eq!(
            verdict(&id, &fake(present, marker(Some(false)))),
            Owner::Gone
        );
        // A run that recorded no start time (macOS) is alive as far as its pid goes.
        id.start = None;
        let some = Process::Present(Some(900));
        assert_eq!(verdict(&id, &fake(some, Marker::Missing)), Owner::Unknown);
        assert_eq!(
            verdict(&id, &fake(Process::Absent, Marker::Missing)),
            Owner::Gone
        );
    }

    #[test]
    fn in_another_pid_namespace_only_the_marker_can_decide() {
        // Two containers on one kernel, or a container and its Linux host. The pid means
        // nothing here, whatever the lookup would say.
        for other in ["pid:[2]", ""] {
            let mut on = fake(Process::Absent, marker(Some(true)));
            on.here.pidns = other.into();
            let id = identity();
            assert_eq!(verdict(&id, &on), Owner::Alive);
            on.marker = marker(Some(false));
            assert_eq!(verdict(&id, &on), Owner::Gone);
            on.marker = marker(None);
            assert_eq!(verdict(&id, &on), Owner::Unknown);
            on.marker = Marker::Missing;
            assert_eq!(verdict(&id, &on), Owner::Unknown);
        }
        // A reader and a run that both failed to read their namespace are not thereby in
        // the same one.
        let mut unread = identity();
        unread.pidns = String::new();
        let mut on = fake(Process::Absent, Marker::Missing);
        on.here.pidns = String::new();
        assert_eq!(verdict(&unread, &on), Owner::Unknown);
        // Another time namespace shows other start times: the pid is not compared either.
        let mut on = fake(Process::Present(Some(900)), Marker::Missing);
        on.here.timens = "time:[2]".into();
        assert_eq!(verdict(&identity(), &on), Owner::Unknown);
    }

    #[test]
    fn the_marker_counts_only_when_it_is_the_inode_the_owner_held() {
        let mut on = fake(Process::Absent, marker(Some(false)));
        on.here.pidns = "pid:[2]".into();
        let id = identity();
        let with = |change: &dyn Fn(&mut Marker)| {
            let mut on = on.clone();
            change(&mut on.marker);
            verdict(&id, &on)
        };
        assert_eq!(with(&|_| {}), Owner::Gone);
        let set = |m: &mut Marker, d: u64, i: u64, h: Option<&str>, l: bool| {
            *m = Marker::Present {
                dev: d,
                ino: i,
                handle: h.map(String::from),
                reader: Some(false),
                local: l,
            }
        };
        // Another device, another inode number, another handle: another file.
        assert_eq!(
            with(&|m| set(m, 11, 20, Some("1:abcd"), false)),
            Owner::Unknown
        );
        assert_eq!(
            with(&|m| set(m, 10, 21, Some("1:abcd"), false)),
            Owner::Unknown
        );
        assert_eq!(
            with(&|m| set(m, 10, 20, Some("1:ffff"), false)),
            Owner::Unknown
        );
        // No handle to compare on the reader's side (a filesystem that gives none), even
        // if it calls itself local.
        assert_eq!(with(&|m| set(m, 10, 20, None, true)), Owner::Unknown);

        // An owner that had no handle either (Docker Desktop's bind mounts): the same
        // numbers are not proof there...
        let mut bare = identity();
        bare.fifo.as_mut().unwrap().handle = None;
        let mut on = on.clone();
        set(&mut on.marker, 10, 20, None, false);
        assert_eq!(verdict(&bare, &on), Owner::Unknown);
        // ...and they are on a local filesystem of a system without handles (macOS).
        set(&mut on.marker, 10, 20, None, true);
        assert_eq!(verdict(&bare, &on), Owner::Gone);
    }

    #[test]
    fn two_observers_that_cannot_see_each_other_leave_the_run_running() {
        // The review's failure: a run going on a macOS host, read from a container on a
        // bind mount of the project. Another kernel: the container's process table has no
        // such pid, and the marker FIFO has no reader as far as its kernel knows. Neither
        // is evidence.
        let mut container = fake(Process::Absent, marker(Some(false)));
        container.here = Here {
            boot: "the-vm".into(),
            machine: String::new(),
            host: "5142bad76cd8".into(),
            pidns: "pid:[4026532980]".into(),
            timens: "time:[4026531834]".into(),
        };
        let mut host_run = identity();
        host_run.pidns = String::new();
        host_run.timens = String::new();
        host_run.start = None;
        assert_eq!(
            judge(Some(7), "host-1", &host_run, &container),
            Owner::Unknown
        );
        // The other way round: the host looking at a run in the container.
        let mut host = fake(Process::Absent, marker(Some(false)));
        host.here.pidns = String::new();
        let mut container_run = identity();
        container_run.boot = "the-vm".into();
        container_run.machine = String::new();
        assert_eq!(
            judge(Some(1), "5142bad76cd8", &container_run, &host),
            Owner::Unknown
        );
        // And a reader that cannot identify its own kernel knows nothing.
        let mut blind = fake(Process::Absent, marker(Some(false)));
        blind.here.boot = String::new();
        assert_eq!(verdict(&identity(), &blind), Owner::Unknown);
    }

    #[test]
    fn after_a_restart_of_this_machine_a_run_started_here_is_gone() {
        let mut on = fake(Process::Present(Some(500)), marker(Some(false)));
        on.here.boot = "boot-2".into();
        let id = identity();
        assert_eq!(verdict(&id, &on), Owner::Gone);
        // Not without the marker in this directory: the row came with the shared history.
        let mut elsewhere = on.clone();
        elsewhere.marker = Marker::Missing;
        assert_eq!(verdict(&id, &elsewhere), Owner::Unknown);
        // Not for another machine, another host name, or a run with no machine id.
        let mut other = on.clone();
        other.here.machine = "machine-2".into();
        assert_eq!(verdict(&id, &other), Owner::Unknown);
        assert_eq!(judge(Some(7), "host-2", &id, &on), Owner::Unknown);
        assert_eq!(judge(Some(7), "", &id, &on), Owner::Unknown);
        let mut anonymous = id.clone();
        anonymous.machine = String::new();
        let mut nameless = on.clone();
        nameless.here.machine = String::new();
        assert_eq!(verdict(&anonymous, &nameless), Owner::Unknown);
    }

    #[test]
    fn an_owner_value_that_is_not_an_identity_is_unknown() {
        let on = fake(Process::Absent, marker(Some(false)));
        for junk in ["", "{", "[]", "\"x\"", "{}", "{\"boot\":\"\"}"] {
            assert_eq!(
                judge_json(Some(7), "host-1", junk, &on),
                Owner::Unknown,
                "{junk}"
            );
            assert_eq!(token_of(junk), None);
        }
        let json = serde_json::to_string(&identity()).unwrap();
        assert_eq!(judge_json(Some(7), "host-1", &json, &on), Owner::Gone);
        assert_eq!(token_of(&json).as_deref(), Some(T));
        // A token that is not one names no file.
        let mut odd = identity();
        odd.fifo.as_mut().unwrap().token = "../../etc/passwd".into();
        assert_eq!(token_of(&serde_json::to_string(&odd).unwrap()), None);
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("metadata.db").display().to_string();
        assert_eq!(System::for_db(&db).marker("../x"), Marker::Missing);
    }

    fn db_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("metadata.db").display().to_string()
    }

    /// Wait until `seen` holds. A child that another test is starting at this instant holds
    /// a copy of every open file for the moment before it becomes its own program, so "the
    /// last holder closed it" can lag by that moment.
    pub(crate) fn eventually(what: &str, seen: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !seen() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Another process's marker in the directory of `db_path`: the returned file is that
    /// process, alive until it is dropped.
    pub(crate) fn other_process(db_path: &str) -> (FifoId, File) {
        hold_marker(&dir_for(db_path)).expect("a marker FIFO")
    }

    #[test]
    fn a_marker_has_a_reader_while_its_owner_lives_and_none_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let system = System::for_db(&db);
        assert_eq!(system.marker(T), Marker::Missing);

        let (id, process) = other_process(&db);
        let seen = |want: bool| {
            matches!(
                system.marker(&id.token),
                Marker::Present { dev, ino, reader: Some(r), .. }
                    if dev == id.dev && ino == id.ino && r == want
            )
        };
        assert!(seen(true));
        assert!(seen(true), "asking changes nothing");
        // The process ends: the kernel closes its files, which is all that happens here.
        drop(process);
        eventually("the marker has no reader", || seen(false));
        assert!(
            marker_path(&dir_for(&db), &id.token).exists(),
            "asking removes nothing"
        );
    }

    #[test]
    fn this_process_is_alive_to_itself_and_removes_its_marker_when_its_runs_end() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let system = System::for_db(&db);
        let owner = claim(&db, "r1");
        assert_eq!(claim(&db, "r2"), owner, "one marker for the runs in flight");
        let pid = Some(i64::from(std::process::id()));
        let host = crate::db::local_host();
        assert_eq!(judge_json(pid, &host, &owner, &system), Owner::Alive);
        let token = token_of(&owner).expect("a marker");
        let path = marker_path(&dir_for(&db), &token);
        assert!(path.exists());

        release(&db, "r1");
        assert!(path.exists(), "r2 is still going");
        release(&db, "r1");
        assert!(path.exists(), "releasing twice counts once");
        release(&db, "r2");
        assert!(!path.exists());
        assert_eq!(fs::read_dir(dir_for(&db)).unwrap().count(), 0);
        // The next run starts over.
        let again = claim(&db, "r3");
        assert_ne!(token_of(&again), Some(token));
        release(&db, "r3");
    }

    #[test]
    fn sweep_removes_only_old_markers_that_no_running_row_names() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let (named, _a) = other_process(&db);
        let (unnamed, b) = other_process(&db);
        let (live_unnamed, _c) = other_process(&db);
        drop(b);
        fs::write(dir_for(&db).join("notes.txt"), "not ours").unwrap();
        let names = || {
            let mut names: Vec<String> = fs::read_dir(dir_for(&db))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let all = names();
        assert_eq!(all.len(), 4);
        let keep = HashSet::from([named.token.clone()]);

        // Just created (a marker exists an instant before its row): nothing goes.
        sweep(&db, &keep, SystemTime::now());
        assert_eq!(names(), all);
        // Two hours on, the ones no running row names go, whether or not somebody holds
        // them (a holder with no running row has no run to protect).
        sweep(&db, &keep, SystemTime::now() + Duration::from_secs(7200));
        let mut want = vec![format!("{}.fifo", named.token), "notes.txt".to_string()];
        want.sort();
        assert_eq!(names(), want);
        let _ = (unnamed, live_unnamed);
    }
}
