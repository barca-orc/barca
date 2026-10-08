//! Whether the process that started a `running` run is gone, decided only from what this
//! process can observe.
//!
//! A run row records the coordinator's pid and host name (#220): a `running` row on this host
//! whose pid is gone is `interrupted`. In a container that never fires (#290): the
//! coordinator is pid 1, the next start is pid 1 again ("alive"), and the host name changes
//! with every start ("another machine's run").
//!
//! The rule here has three answers ([`Owner`]), and the third is the default. A run is
//! `interrupted` only on an observation that can only be made when its process is gone.
//! Whenever that observation cannot be made, by this process, on this kernel, through this
//! filesystem, the answer is [`Owner::Unknown`] and the run stays `running`. Who the reader
//! and the owner say they are (machine id, host name) never decides anything by itself.
//!
//! Two things can be observed:
//!
//! - **The process table**, when the reader is on the owner's kernel (same boot id) and in
//!   its pid namespace: no process with the owner's pid, or one with another start time,
//!   means the owner is gone. (A restarted container often gets the pid namespace number of
//!   the one it replaces; a number is only handed out again after its namespace, and every
//!   process in it, is gone, so the lookup is right then as well.)
//! - **The marker**: a FIFO the owner created in the project directory
//!   (`run-owners/<token>.fifo`) and holds open for reading for as long as it has a run in
//!   flight. The kernel closes it when the process ends, however it ended, and opening a
//!   FIFO for writing without blocking fails with `ENXIO` exactly when no process has it
//!   open for reading. That is one kernel's bookkeeping for one inode, so "no reader" means
//!   something only when the reader shows that it reaches the inode the owner held
//!   ([`judge`] says how for each case).
//!
//! The rules ([`judge`]):
//!
//! 1. **Same kernel, same pid namespace.** The process table decides. The marker is not
//!    asked to confirm it. Something other than the owner can hold a marker open (Docker
//!    Desktop's file sharing does, on the host, once a container has looked at the file),
//!    and that would keep a killed run `running` on the machine it ran on. Nor would
//!    asking help in the one case where a boot id does not name one kernel: two machines
//!    resumed from the same memory snapshot. Their process tables differ once a process
//!    dies on one of them, and a FIFO on a directory they share over the network has its
//!    readers counted by each kernel separately, so the marker agrees with the table on
//!    both. That case is not decidable from either machine and is the stated limit.
//! 2. **Same kernel, another pid namespace** (two containers, a container and its Linux
//!    host). The marker decides, when it is the same inode: same device and inode number,
//!    and the same file handle (Linux, `name_to_handle_at`) or a local disk (macOS, which
//!    has no handles). Docker Desktop's bind mounts give no file handle: `Unknown`.
//! 3. **Another kernel** (the boot ids differ). A process there cannot be observed, with one
//!    exception: the kernel it ran on has shut down. That is established from the marker
//!    alone: both the owner and the reader see it on a local disk filesystem (not a network
//!    or VM-shared mount; "cannot tell" counts as not local), it is the same filesystem
//!    and inode, and it has no reader. A local disk is mounted by one kernel at a time, so
//!    the reader's kernel is the one that has it now, the owner's had it before, and the
//!    owner's kernel, with every process on it, is gone. An owner still alive on another
//!    machine, a clone or the host of this container, reached the directory through a
//!    mount that is not local on one side or the other, and the rule does not apply.
//!
//! What rule 3 cannot tell apart is a block-for-block copy of the disk (a cloned or
//! restored virtual machine) taken while the run was going: the copy has the marker, same
//! filesystem and inode, with no reader. The copy's own history then says `interrupted`
//! for that run, which in that copy of the history it is.
//!
//! Nothing depends on a clock, so a suspended laptop, a stopped process (it keeps its pid
//! and its open files) or a clock step cannot make a live run look dead, and a killed run
//! reads `interrupted` at once.
//!
//! Readers never write: [`System`] opens and closes the marker, nothing else. The marker is
//! removed by its owner when its runs have ended, and otherwise by a starting run
//! ([`sweep`]) once no `running` row names it, never on a judgement about liveness.

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
    /// The filesystem it is on, by an id that outlasts a restart (the volume's UUID on
    /// macOS, `statfs`'s id on Linux). Empty when there is none.
    #[serde(default)]
    pub fs: String,
    /// Whether that is a local disk filesystem as far as the owner could tell.
    #[serde(default)]
    pub local: bool,
}

/// What the reading process is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Here {
    pub boot: String,
    pub machine: String,
    pub host: String,
    pub pidns: String,
    pub timens: String,
    /// Whether this system names inodes by file handles (Linux). Where it does, a marker
    /// without one cannot be verified; where it does not (macOS), a local disk's device
    /// and inode number have to do.
    pub handles: bool,
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
        /// Whether a process has it open for reading; `None` when that could not be asked
        /// (it is not a FIFO, or it changed while it was asked).
        reader: Option<bool>,
        /// The filesystem's lasting id; empty when there is none.
        fs: String,
        /// Whether it is on a local disk filesystem: one that one kernel at a time has
        /// mounted. False for network and VM-shared mounts and whenever it cannot be told.
        local: bool,
    },
}

/// What [`judge`] asks of the machine. The real one is [`System`]; tests supply their own.
pub(crate) trait Observer {
    fn here(&self) -> Here;
    fn process(&self, pid: i64) -> Process;
    fn marker(&self, token: &str) -> Marker;
}

/// What the reader's view of the marker establishes about the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Witness {
    /// It is the owner's inode on this kernel and a process has it open for reading.
    Held,
    /// It is the owner's inode on this kernel and no process has it open for reading.
    Free,
    /// It is not there, not verifiably the owner's, or could not be asked.
    Silent,
}

/// The marker as a witness on the owner's own kernel (rules 1 and 2): the same device and
/// inode number, and the same file handle, or, where neither side has handles (macOS), a
/// local disk.
fn witness(fifo: Option<&FifoId>, seen: &Marker, handles: bool) -> Witness {
    let (
        Some(fifo),
        Marker::Present {
            dev,
            ino,
            handle,
            reader,
            local,
            ..
        },
    ) = (fifo, seen)
    else {
        return Witness::Silent;
    };
    let same_inode = *dev == fifo.dev
        && *ino == fifo.ino
        && match (&fifo.handle, handle) {
            (Some(theirs), Some(ours)) => theirs == ours,
            (None, None) => !handles && *local && fifo.local,
            _ => false,
        };
    match (same_inode, reader) {
        (true, Some(true)) => Witness::Held,
        (true, Some(false)) => Witness::Free,
        _ => Witness::Silent,
    }
}

/// Rule 3's observation: the kernel the owner ran on has shut down. Both sides see the
/// marker on a local disk filesystem, it is the same filesystem and the same inode, and
/// nobody on this kernel has it open. (The device number is not compared: it can change
/// from one boot to the next.)
fn owners_kernel_is_gone(fifo: Option<&FifoId>, seen: &Marker) -> bool {
    let (
        Some(fifo),
        Marker::Present {
            ino,
            handle,
            reader,
            fs,
            local,
            ..
        },
    ) = (fifo, seen)
    else {
        return false;
    };
    fifo.local
        && *local
        && !fifo.fs.is_empty()
        && fifo.fs == *fs
        && fifo.ino == *ino
        && fifo.handle == *handle
        && *reader == Some(false)
}

/// The rule (see the module docs). `pid` is the run row's.
///
/// `Gone` is returned in three places, each on an observation: the process table of the
/// owner's kernel and namespace shows no such process; the owner's marker has no reader on the owner's kernel; the owner's kernel
/// has shut down.
pub(crate) fn judge(pid: Option<i64>, id: &Identity, on: &dyn Observer) -> Owner {
    let here = on.here();
    if id.boot.is_empty() || here.boot.is_empty() {
        return Owner::Unknown;
    }
    let fifo = id.fifo.as_ref();
    let seen = match fifo {
        Some(fifo) => on.marker(&fifo.token),
        None => Marker::Missing,
    };

    // 3. Another kernel.
    if id.boot != here.boot {
        // Two different machines never have the same local disk; when both name
        // themselves and the names differ, something is not as it seems.
        let other_machine =
            !id.machine.is_empty() && !here.machine.is_empty() && id.machine != here.machine;
        return if !other_machine && owners_kernel_is_gone(fifo, &seen) {
            Owner::Gone
        } else {
            Owner::Unknown
        };
    }

    let witness = witness(fifo, &seen, here.handles);

    // 1. This kernel and this pid namespace: the process table answers. A namespace that
    // could not be read is not known to be the same one.
    if let Some(pid) = pid
        && !id.pidns.is_empty()
        && id.pidns == here.pidns
        && id.timens == here.timens
    {
        let gone = match (on.process(pid), id.start) {
            (Process::Absent, _) => Some(true),
            (Process::Present(Some(now)), Some(then)) => Some(now != then),
            // A process has the pid and it cannot be told whether it is the same one.
            (Process::Present(_), _) => None,
        };
        match gone {
            Some(true) => return Owner::Gone,
            Some(false) => return Owner::Alive,
            None => {}
        }
    }

    // 2. This kernel: the marker.
    match witness {
        Witness::Held => Owner::Alive,
        Witness::Free => Owner::Gone,
        Witness::Silent => Owner::Unknown,
    }
}

/// Judge the owner recorded in `owner_json` (the `runs.owner` column). Anything that does
/// not parse is [`Owner::Unknown`].
pub(crate) fn judge_json(pid: Option<i64>, owner_json: &str, on: &dyn Observer) -> Owner {
    match serde_json::from_str::<Identity>(owner_json) {
        Ok(id) => judge(pid, &id, on),
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
            handles: cfg!(target_os = "linux"),
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

    fn c_path(path: &Path) -> Option<std::ffi::CString> {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(path.as_os_str().as_bytes()).ok()
    }

    /// Whether `path` is on a local disk filesystem, and that filesystem's lasting id.
    /// "Local" is a filesystem that one kernel at a time has mounted from a disk: not a
    /// network mount, not a directory a virtual machine shares with its host. Anything
    /// that is not known to be one is not local.
    #[cfg(target_os = "macos")]
    pub(super) fn filesystem(path: &Path) -> (bool, String) {
        let Some(c_path) = c_path(path) else {
            return (false, String::new());
        };
        // SAFETY: `statfs` fills the struct it is given; the path is NUL-terminated.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
            return (false, String::new());
        }
        let local = (st.f_flags & libc::MNT_LOCAL as u32) != 0;

        // The volume's UUID: the device number in `statfs` can change between boots.
        #[repr(C)]
        struct VolumeUuid {
            length: u32,
            uuid: [u8; 16],
        }
        // SAFETY: an all-zero `attrlist` is a valid one to fill in.
        let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
        attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attrs.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
        let mut out = VolumeUuid {
            length: 0,
            uuid: [0; 16],
        };
        // SAFETY: the buffer is as large as the size passed, and the attributes asked for
        // (a length and one 16-byte UUID) fit it.
        let rc = unsafe {
            libc::getattrlist(
                c_path.as_ptr(),
                (&mut attrs as *mut libc::attrlist).cast(),
                (&mut out as *mut VolumeUuid).cast(),
                std::mem::size_of::<VolumeUuid>(),
                0,
            )
        };
        let complete = rc == 0 && out.length as usize >= std::mem::size_of::<VolumeUuid>();
        if !complete || out.uuid == [0; 16] {
            return (local, String::new());
        }
        let hex: String = out.uuid.iter().map(|b| format!("{b:02x}")).collect();
        (local, hex)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn filesystem(path: &Path) -> (bool, String) {
        // The disk filesystems: each is mounted from a block device by one kernel.
        const LOCAL: [i64; 6] = [
            0xEF53,      // ext2, ext3, ext4
            0x5846_5342, // xfs
            0x9123_683E, // btrfs
            0xF2F5_2010, // f2fs
            0x2FC1_2FC1, // zfs
            0xCA45_1A4E, // bcachefs
        ];
        let Some(c_path) = c_path(path) else {
            return (false, String::new());
        };
        // SAFETY: `statfs` fills the struct it is given; the path is NUL-terminated.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
            return (false, String::new());
        }
        let local = LOCAL.contains(&(st.f_type as i64 & 0xFFFF_FFFF));
        // SAFETY: `fsid_t` is two C ints on Linux.
        let id: [libc::c_int; 2] = unsafe { std::mem::transmute(st.f_fsid) };
        let fs = if id == [0, 0] {
            String::new()
        } else {
            format!("{:08x}{:08x}", id[0] as u32, id[1] as u32)
        };
        (local, fs)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn filesystem(_path: &Path) -> (bool, String) {
        (false, String::new())
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
                fs: String::new(),
                local: false,
            };
        }
        let handle = file_handle(path);
        let reader = has_reader(path);
        // The same file before and after the question, or the answer is about another one.
        let unchanged = std::fs::symlink_metadata(path)
            .is_ok_and(|after| after.dev() == before.dev() && after.ino() == before.ino());
        let (local, fs) = filesystem(path);
        Marker::Present {
            dev: before.dev(),
            ino: before.ino(),
            handle,
            reader: if unchanged { reader } else { None },
            fs,
            local,
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
        let (local, fs) = sys::filesystem(&path);
        let id = FifoId {
            token,
            dev: meta.dev(),
            ino: meta.ino(),
            handle: sys::file_handle(&path),
            fs,
            local,
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

    /// A Linux reader in the owner's kernel and pid namespace.
    fn here() -> Here {
        Here {
            boot: "boot-1".into(),
            machine: "machine-1".into(),
            host: "host-1".into(),
            pidns: "pid:[1]".into(),
            timens: "time:[1]".into(),
            handles: true,
        }
    }

    /// A run started by pid 7, started at tick 500, in `here()`, holding marker `T` on a
    /// local disk.
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
                fs: "fs-1".into(),
                local: true,
            }),
        }
    }

    /// The owner's marker as a reader on the owner's kernel sees it.
    pub(crate) fn marker(reader: Option<bool>) -> Marker {
        Marker::Present {
            dev: 10,
            ino: 20,
            handle: Some("1:abcd".into()),
            reader,
            fs: "fs-1".into(),
            local: true,
        }
    }

    /// `marker(reader)` with one thing about it changed.
    fn marker_but(reader: Option<bool>, change: impl Fn(&mut MarkerParts)) -> Marker {
        let mut parts = MarkerParts {
            dev: 10,
            ino: 20,
            handle: Some("1:abcd".into()),
            fs: "fs-1".into(),
            local: true,
        };
        change(&mut parts);
        Marker::Present {
            dev: parts.dev,
            ino: parts.ino,
            handle: parts.handle,
            reader,
            fs: parts.fs,
            local: parts.local,
        }
    }

    struct MarkerParts {
        dev: u64,
        ino: u64,
        handle: Option<String>,
        fs: String,
        local: bool,
    }

    fn fake(process: Process, marker: Marker) -> Fake {
        Fake {
            here: here(),
            process,
            marker,
        }
    }

    fn verdict(id: &Identity, on: &Fake) -> Owner {
        judge(Some(7), id, on)
    }

    const ABSENT: Process = Process::Absent;
    const SAME: Process = Process::Present(Some(500));
    const REUSED: Process = Process::Present(Some(900));

    // ─── rule 1: the owner's kernel and pid namespace ─────────────────────────

    #[test]
    fn in_the_same_pid_namespace_the_process_table_decides() {
        let id = identity();
        // Whatever the marker says. A marker that is held although the process is gone has
        // another holder: on a macOS host, Docker Desktop's file sharing keeps a marker
        // open once a container has looked at it.
        for m in [
            marker(Some(false)),
            marker(Some(true)),
            marker(None),
            Marker::Missing,
        ] {
            assert_eq!(verdict(&id, &fake(ABSENT, m.clone())), Owner::Gone);
            // The pid belongs to a later process: in a container, the next pid 1.
            assert_eq!(verdict(&id, &fake(REUSED, m.clone())), Owner::Gone);
            assert_eq!(verdict(&id, &fake(SAME, m)), Owner::Alive);
        }
    }

    #[test]
    fn machines_resumed_from_one_memory_snapshot_are_one_machine_to_this_rule() {
        // (e) The limit, pinned down so that it is not mistaken for a guarantee. Two
        // machines resumed from the same snapshot have the same boot id, pid namespace
        // numbers and processes. If the run's process is killed on one and they share the
        // project directory over a network mount, that one sees: no such process, and no
        // reader on a marker whose readers its own kernel counts. Every observation it can
        // make says gone, while the process lives on the other machine.
        let id = identity();
        let seen = marker_but(Some(false), |m| m.local = false);
        assert_eq!(verdict(&id, &fake(ABSENT, seen)), Owner::Gone);
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
        assert_eq!(verdict(&id, &fake(REUSED, Marker::Missing)), Owner::Unknown);
        assert_eq!(verdict(&id, &fake(ABSENT, Marker::Missing)), Owner::Gone);
    }

    // ─── rule 2: the owner's kernel, another pid namespace ────────────────────

    #[test]
    fn in_another_pid_namespace_only_the_marker_can_decide() {
        // (d) Two containers on one kernel and one volume, or a container and its Linux
        // host. The pid means nothing here, whatever the lookup would say.
        for other in ["pid:[2]", ""] {
            for process in [ABSENT, SAME, REUSED] {
                let mut on = fake(process, marker(Some(true)));
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
        }
        // A reader and a run that both failed to read their namespace are not thereby in
        // the same one.
        let mut unread = identity();
        unread.pidns = String::new();
        let mut on = fake(ABSENT, Marker::Missing);
        on.here.pidns = String::new();
        assert_eq!(verdict(&unread, &on), Owner::Unknown);
        // Another time namespace shows other start times: the pid is not compared either.
        let mut on = fake(REUSED, Marker::Missing);
        on.here.timens = "time:[2]".into();
        assert_eq!(verdict(&identity(), &on), Owner::Unknown);
    }

    #[test]
    fn the_marker_counts_only_when_it_is_the_inode_the_owner_held() {
        let id = identity();
        let next_door = |marker: Marker| {
            let mut on = fake(ABSENT, marker);
            on.here.pidns = "pid:[2]".into();
            on
        };
        assert_eq!(verdict(&id, &next_door(marker(Some(false)))), Owner::Gone);
        // Another device, another inode number, another handle: another file.
        for other in [
            marker_but(Some(false), |m| m.dev = 11),
            marker_but(Some(false), |m| m.ino = 21),
            marker_but(Some(false), |m| m.handle = Some("1:ffff".into())),
            // No handle on the reader's side, even on a disk it calls local.
            marker_but(Some(false), |m| m.handle = None),
        ] {
            assert_eq!(verdict(&id, &next_door(other)), Owner::Unknown);
        }

        // An owner that had no handle either (Docker Desktop's bind mounts): on a system
        // with handles the same numbers are not proof...
        let mut bare = identity();
        bare.fifo.as_mut().unwrap().handle = None;
        let no_handle = marker_but(Some(false), |m| m.handle = None);
        assert_eq!(
            verdict(&bare, &next_door(no_handle.clone())),
            Owner::Unknown
        );
        // ...and on a system without them (macOS) they are, on a local disk only.
        let mut mac = next_door(no_handle);
        mac.here.handles = false;
        assert_eq!(verdict(&bare, &mac), Owner::Gone);
        mac.marker = marker_but(Some(false), |m| (m.handle, m.local) = (None, false));
        assert_eq!(verdict(&bare, &mac), Owner::Unknown);
        let mut network_owner = bare.clone();
        network_owner.fifo.as_mut().unwrap().local = false;
        mac.marker = marker_but(Some(false), |m| m.handle = None);
        assert_eq!(verdict(&network_owner, &mac), Owner::Unknown);
    }

    // ─── rule 3: another kernel ───────────────────────────────────────────────

    /// A reader on another kernel: a later boot of the same machine unless changed.
    fn after_reboot(process: Process, marker: Marker) -> Fake {
        let mut on = fake(process, marker);
        on.here.boot = "boot-2".into();
        on
    }

    #[test]
    fn a_restarted_machine_sees_that_the_owners_kernel_is_gone() {
        // (a) The same machine, restarted, the project on its local disk. The device number
        // may have changed; filesystem, inode and handle have not, and nobody holds the
        // marker.
        let id = identity();
        let seen = marker_but(Some(false), |m| m.dev = 99);
        for process in [ABSENT, SAME, REUSED] {
            assert_eq!(
                verdict(&id, &after_reboot(process, seen.clone())),
                Owner::Gone
            );
        }
        // A container restarted with the machine has no machine id and a new host name:
        // neither is needed, the observation is the marker's.
        let mut container = after_reboot(ABSENT, seen.clone());
        container.here.machine = String::new();
        container.here.host = "5142bad76cd8".into();
        assert_eq!(verdict(&id, &container), Owner::Gone);
        // Each part of the observation is needed.
        for (what, other) in [
            ("somebody holds it", marker_but(Some(true), |_| {})),
            ("it could not be asked", marker_but(None, |_| {})),
            (
                "the reader's mount is not local",
                marker_but(Some(false), |m| m.local = false),
            ),
            (
                "another filesystem",
                marker_but(Some(false), |m| m.fs = "fs-2".into()),
            ),
            (
                "a filesystem with no id",
                marker_but(Some(false), |m| m.fs = String::new()),
            ),
            ("another inode", marker_but(Some(false), |m| m.ino = 21)),
            (
                "another handle",
                marker_but(Some(false), |m| m.handle = Some("1:ffff".into())),
            ),
            ("no handle", marker_but(Some(false), |m| m.handle = None)),
            ("no marker", Marker::Missing),
        ] {
            assert_eq!(
                verdict(&id, &after_reboot(ABSENT, other)),
                Owner::Unknown,
                "{what}"
            );
        }
        // The owner's own mount was not local, or it recorded no filesystem id.
        let mut shared = id.clone();
        shared.fifo.as_mut().unwrap().local = false;
        assert_eq!(
            verdict(&shared, &after_reboot(ABSENT, seen.clone())),
            Owner::Unknown
        );
        let mut unnamed = id.clone();
        unnamed.fifo.as_mut().unwrap().fs = String::new();
        let blank = marker_but(Some(false), |m| m.fs = String::new());
        assert_eq!(
            verdict(&unnamed, &after_reboot(ABSENT, blank)),
            Owner::Unknown
        );
        // Two machines that name themselves differently do not share a local disk.
        let mut other = after_reboot(ABSENT, seen);
        other.here.machine = "machine-2".into();
        assert_eq!(verdict(&id, &other), Owner::Unknown);
    }

    #[test]
    fn who_the_reader_says_it_is_never_makes_a_run_gone() {
        // The review's attack. (b) Two virtual machines cloned from one image, with the
        // same machine id and host name, share the project over NFS or SMB; the owner is
        // alive on the other one. (c) A container started with its host's machine id and
        // host name on a bind mount of the project; either side has the live run. In each
        // the reader is "the same machine" by every name, its process table has no such
        // pid, and the marker has no reader on its kernel. None of that is an observation
        // of the owner.
        let id = identity();
        // (b), and (c) read from the container: the reader's mount is not a local disk.
        for seen in [
            marker_but(Some(false), |m| m.local = false),
            marker_but(Some(false), |m| (m.local, m.handle) = (false, None)),
            marker_but(Some(false), |m| {
                (m.local, m.dev, m.ino) = (false, 50, 33454)
            }),
            marker_but(Some(false), |m| (m.local, m.fs) = (false, String::new())),
        ] {
            for process in [ABSENT, REUSED, Process::Present(None)] {
                assert_eq!(
                    verdict(&id, &after_reboot(process, seen.clone())),
                    Owner::Unknown
                );
            }
        }
        // (b) seen from the owner's side of the clone pair, and (c) read from the host
        // while the run is in the container: the owner's mount was not local, whatever
        // the reader's is.
        let mut over_the_network = id.clone();
        over_the_network.fifo.as_mut().unwrap().local = false;
        for reader in [Some(false), Some(true), None] {
            let seen = marker_but(reader, |_| {});
            let on = after_reboot(ABSENT, seen);
            assert_eq!(verdict(&over_the_network, &on), Owner::Unknown);
        }
    }

    #[test]
    fn a_directory_copied_to_another_machine_leaves_its_runs_running() {
        // (f) A project copied or restored from a backup, `running` row and all. The
        // marker is missing, or a regular file, or a new FIFO: another inode on another
        // filesystem, with nobody holding it.
        let id = identity();
        let not_a_fifo = Marker::Present {
            dev: 10,
            ino: 20,
            handle: None,
            reader: None,
            fs: String::new(),
            local: false,
        };
        for seen in [
            Marker::Missing,
            not_a_fifo,
            marker_but(Some(false), |m| (m.fs, m.ino) = ("fs-9".into(), 4711)),
            marker_but(Some(false), |m| m.fs = "fs-9".into()),
            marker_but(Some(false), |m| {
                (m.ino, m.handle) = (4711, Some("1:0001".into()))
            }),
        ] {
            let mut on = after_reboot(ABSENT, seen);
            assert_eq!(verdict(&id, &on), Owner::Unknown);
            on.here.machine = "machine-2".into();
            on.here.host = "host-2".into();
            assert_eq!(verdict(&id, &on), Owner::Unknown);
        }
    }

    #[test]
    fn a_reader_or_a_run_that_cannot_name_its_kernel_knows_nothing() {
        let mut blind = fake(ABSENT, marker(Some(false)));
        blind.here.boot = String::new();
        assert_eq!(verdict(&identity(), &blind), Owner::Unknown);
        let mut anonymous = identity();
        anonymous.boot = String::new();
        assert_eq!(
            verdict(&anonymous, &fake(ABSENT, marker(Some(false)))),
            Owner::Unknown
        );
    }

    #[test]
    fn an_owner_value_that_is_not_an_identity_is_unknown() {
        let on = fake(ABSENT, marker(Some(false)));
        for junk in ["", "{", "[]", "\"x\"", "{}", "{\"boot\":\"\"}"] {
            assert_eq!(judge_json(Some(7), junk, &on), Owner::Unknown, "{junk}");
            assert_eq!(token_of(junk), None);
        }
        let json = serde_json::to_string(&identity()).unwrap();
        assert_eq!(judge_json(Some(7), &json, &on), Owner::Gone);
        assert_eq!(token_of(&json).as_deref(), Some(T));
        // A token that is not one names no file.
        let mut odd = identity();
        odd.fifo.as_mut().unwrap().token = "../../etc/passwd".into();
        assert_eq!(token_of(&serde_json::to_string(&odd).unwrap()), None);
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("metadata.db").display().to_string();
        assert_eq!(System::for_db(&db).marker("../x"), Marker::Missing);
    }

    // ─── the real marker ──────────────────────────────────────────────────────

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
                Marker::Present { dev, ino, reader: Some(r), ref fs, local, .. }
                    if dev == id.dev && ino == id.ino && r == want && *fs == id.fs && local == id.local
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
        // Something else under the marker's name answers nothing.
        let path = marker_path(&dir_for(&db), T);
        fs::write(&path, "a copy").unwrap();
        assert!(matches!(
            system.marker(T),
            Marker::Present {
                reader: None,
                local: false,
                ..
            }
        ));
    }

    #[test]
    fn this_process_is_alive_to_itself_and_removes_its_marker_when_its_runs_end() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let system = System::for_db(&db);
        let owner = claim(&db, "r1");
        assert_eq!(claim(&db, "r2"), owner, "one marker for the runs in flight");
        let pid = Some(i64::from(std::process::id()));
        assert_eq!(judge_json(pid, &owner, &system), Owner::Alive);
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
