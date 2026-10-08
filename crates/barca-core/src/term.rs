//! Writes to stdout and stderr that cannot panic (#286).
//!
//! `println!` and `eprintln!` panic when the write fails. Rust ignores SIGPIPE, so a reader
//! that goes away (`barca list | head -1`) turns the next write into an error and then into a
//! panic; on stderr, in the middle of a run, that panic abandoned the run. Everything barca
//! prints therefore goes through [`outln!`](crate::outln) and [`errln!`](crate::errln):
//!
//! - a failed write is remembered and never raised. Later writes to that stream are skipped;
//! - the command goes on: a run finishes and is recorded whether or not anyone is reading;
//! - a reader that leaves is not an error: the exit code is the one the command would have
//!   had with a reader (0 for `barca list | head -1`, as it always was for `barca docs`).
//!   Scripts that pipe barca into `head` or `grep -q` under `set -o pipefail` depend on that;
//! - a write that fails for another reason (a full disk behind `> file`) is an error: the CLI
//!   exits 3 ([`stdout_write_failed`]).
//!
//! Clippy denies `print_stdout` and `print_stderr` in the three crates, so a new `println!`
//! does not compile in CI.
//!
//! # What barca's child processes write to
//!
//! A worker's stdout and stderr are barca's stderr: a step's `print()` shows up there. If the
//! workers held barca's stderr itself and that were a pipe whose reader left, the damage would
//! be done where barca cannot catch it: Python raises `BrokenPipeError` inside the step, a
//! child process the step starts is killed by SIGPIPE, a C extension gets EPIPE. So when
//! barca's stderr is a pipe or a socket, the children get a pipe barca owns instead
//! ([`child_output`]), and barca copies what arrives on it to its own stderr, dropping it when
//! that is closed. The children's pipe never loses its reader while barca lives.
//!
//! - One pipe serves every child, and each child's stdout and stderr are both that pipe, as
//!   before both were the one stderr: the order in which the children's writes arrive is the
//!   order they were written in.
//! - barca's own lines keep their place: [`stderr_line`] first copies what the children had
//!   already written. A step's output therefore still precedes the `[barca] step:... completed`
//!   line that reports it.
//! - Nothing is held in memory: bytes are copied through a fixed buffer, and a child that
//!   writes faster than barca's stderr is read waits on a full pipe, as it did on the
//!   caller's.
//! - When barca's stderr is a terminal, a file or `/dev/null`, the children hold it directly,
//!   exactly as before: a terminal stays a terminal for a step that draws a progress bar, and
//!   none of those can lose its reader.

use std::fmt;
use std::io::{ErrorKind, PipeReader, PipeWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const OPEN: u8 = 0;
/// The reader went away (EPIPE).
const CLOSED: u8 = 1;
/// Any other write error (a full disk behind a redirect, a revoked terminal).
const FAILED: u8 = 2;

static STDOUT: AtomicU8 = AtomicU8::new(OPEN);
static STDERR: AtomicU8 = AtomicU8::new(OPEN);

fn state_of(e: &std::io::Error) -> u8 {
    if e.kind() == ErrorKind::BrokenPipe {
        CLOSED
    } else {
        FAILED
    }
}

fn write_to(stream: &mut dyn Write, state: &AtomicU8, args: fmt::Arguments<'_>, newline: bool) {
    if state.load(Ordering::Relaxed) != OPEN {
        return;
    }
    // One buffer, one `write_all`: a line is not split between two writers of the stream.
    let mut text = fmt::format(args);
    if newline {
        text.push('\n');
    }
    if let Err(e) = stream.write_all(text.as_bytes()) {
        state.store(state_of(&e), Ordering::Relaxed);
    }
}

/// A standard stream written to directly, for a write that waits instead of failing.
///
/// The caller may hand barca a descriptor in non-blocking mode (a pipe shared with an event
/// loop does this). A write to it fails with EAGAIN while the reader is behind. That is not a
/// closed stream: the write waits until the descriptor is writable (`poll`, no busy loop) and
/// goes on, so a slow reader receives everything and the wait reaches whoever produces the
/// output, as it does on a blocking pipe. EINTR is retried. Only a real error (EPIPE when the
/// reader is gone, EIO, ENOSPC) comes back to the caller.
///
/// It writes to the descriptor itself and not through `std::io::stdout()`, whose line buffer
/// cannot be retried after a partial write without repeating bytes.
struct Stream(i32);

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            // SAFETY: the pointer and length are those of `buf`. The length is capped
            // because some systems refuse a single write above INT_MAX.
            let n = unsafe { libc::write(self.0, buf.as_ptr().cast(), buf.len().min(1 << 30)) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = std::io::Error::last_os_error();
            match e.kind() {
                ErrorKind::Interrupted => {}
                ErrorKind::WouldBlock => {
                    let mut poll = libc::pollfd {
                        fd: self.0,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: one valid pollfd. It returns when the descriptor is writable
                    // or its reader is gone; the next write then succeeds or says why not.
                    unsafe { libc::poll(&mut poll, 1, -1) };
                }
                // A stream that was closed before barca started (`>&-`): nothing to write
                // to, and not an error, as with `println!`.
                _ if e.raw_os_error() == Some(libc::EBADF) => return Ok(buf.len()),
                _ => return Err(e),
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One writer at a time per stream, so lines from different threads do not interleave.
static STDOUT_WRITER: Mutex<()> = Mutex::new(());
static STDERR_WRITER: Mutex<()> = Mutex::new(());

fn locked(lock: &'static Mutex<()>) -> MutexGuard<'static, ()> {
    lock.lock().unwrap_or_else(|e| e.into_inner())
}

/// `println!` that does not panic. Use [`outln!`](crate::outln).
pub fn stdout_line(args: fmt::Arguments<'_>) {
    let _one = locked(&STDOUT_WRITER);
    write_to(&mut Stream(libc::STDOUT_FILENO), &STDOUT, args, true);
}

/// Write text to stdout as it is (no newline added), without panicking.
pub fn stdout_str(text: &str) {
    let _one = locked(&STDOUT_WRITER);
    write_to(
        &mut Stream(libc::STDOUT_FILENO),
        &STDOUT,
        format_args!("{text}"),
        false,
    );
}

/// `eprintln!` that does not panic. Use [`errln!`](crate::errln). What the child processes
/// wrote before this call is copied to stderr first, so the line keeps its place.
pub fn stderr_line(args: fmt::Arguments<'_>) {
    let _order = ChildOutput::copy_pending();
    let _one = locked(&STDERR_WRITER);
    write_to(&mut Stream(libc::STDERR_FILENO), &STDERR, args, true);
}

/// The pipe barca's children write to when barca's own stderr could lose its reader.
struct ChildOutput {
    /// Non-blocking, so a copy never waits for a child to write.
    reader: PipeReader,
    /// Kept open for the life of the process: the children's pipe always has a reader and a
    /// writer, so a read never reports end-of-file while a child could still be started.
    writer: PipeWriter,
    /// Held while bytes are copied and while barca writes a line of its own: the order on
    /// stderr is the order of those critical sections.
    order: Mutex<()>,
}

static CHILD_OUTPUT: OnceLock<Option<ChildOutput>> = OnceLock::new();

/// Whether descriptor `fd` is a pipe or a socket: the kinds whose reader can go away.
fn can_lose_its_reader(fd: i32) -> bool {
    // SAFETY: `fstat` fills the struct it is given; an error leaves the answer "no".
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return false;
    }
    matches!(st.st_mode & libc::S_IFMT, libc::S_IFIFO | libc::S_IFSOCK)
}

impl ChildOutput {
    fn get() -> Option<&'static ChildOutput> {
        CHILD_OUTPUT
            .get_or_init(|| {
                if !can_lose_its_reader(libc::STDERR_FILENO) {
                    return None;
                }
                let (reader, writer) = std::io::pipe().ok()?;
                // SAFETY: plain syscalls on a descriptor this function owns. The flag is on
                // the read end only: the children's writes stay blocking.
                let nonblocking = unsafe {
                    let flags = libc::fcntl(reader.as_raw_fd(), libc::F_GETFL);
                    flags >= 0
                        && libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                            == 0
                };
                if !nonblocking {
                    return None;
                }
                let fd = reader.as_raw_fd();
                std::thread::Builder::new()
                    .name("barca-child-output".into())
                    .spawn(move || {
                        loop {
                            let mut poll = libc::pollfd {
                                fd,
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            // SAFETY: one valid pollfd; the descriptor lives as long as the
                            // process (it is in a static).
                            let ready = unsafe { libc::poll(&mut poll, 1, -1) };
                            if ready < 0
                                && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted
                            {
                                return;
                            }
                            ChildOutput::copy_pending();
                        }
                    })
                    .ok()?;
                Some(ChildOutput {
                    reader,
                    writer,
                    order: Mutex::new(()),
                })
            })
            .as_ref()
    }

    /// Copy to stderr what the children have written so far, and return the guard that keeps
    /// the order until the caller has written its own line. Copies only what was in the pipe
    /// when it was called, so a child that never stops writing cannot hold the caller.
    fn copy_pending() -> Option<MutexGuard<'static, ()>> {
        // Not `get()`: before the first child is started there is nothing to copy, and a
        // command that starts none never creates the pipe.
        let out = CHILD_OUTPUT.get()?.as_ref()?;
        let guard = out.order.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending: libc::c_int = 0;
        // SAFETY: FIONREAD writes one int.
        if unsafe { libc::ioctl(out.reader.as_raw_fd(), libc::FIONREAD, &mut pending) } != 0 {
            return Some(guard);
        }
        let mut left = pending.max(0) as usize;
        let mut buf = [0u8; 16 * 1024];
        while left > 0 {
            let want = left.min(buf.len());
            match (&out.reader).read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => {
                    left -= n;
                    forward(&buf[..n]);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break, // WouldBlock: another copy took it
            }
        }
        Some(guard)
    }
}

/// Write the children's bytes to stderr as they are; drop them once stderr is closed.
fn forward(bytes: &[u8]) {
    if STDERR.load(Ordering::Relaxed) != OPEN {
        return;
    }
    let _one = locked(&STDERR_WRITER);
    if let Err(e) = Stream(libc::STDERR_FILENO).write_all(bytes) {
        STDERR.store(state_of(&e), Ordering::Relaxed);
    }
}

/// What a child process's stdout and stderr should be, so that its output reaches barca's
/// stderr and the child never writes to a pipe that has lost its reader (see the module
/// documentation). Use it for both streams of every child whose output is not captured.
pub fn child_output() -> Stdio {
    match ChildOutput::get().and_then(|out| out.writer.try_clone().ok()) {
        Some(writer) => Stdio::from(writer),
        None => Stdio::from(std::io::stderr()),
    }
}

/// Exit the process, after copying to stderr what its children wrote. Every exit of the CLI
/// goes through here: `std::process::exit` alone would leave their last lines in the pipe.
pub fn exit(code: i32) -> ! {
    drop(ChildOutput::copy_pending());
    std::process::exit(code)
}

/// Whether everything written to stdout got out. Nothing is buffered: each write went to the
/// descriptor when it was made.
fn stdout_state() -> u8 {
    STDOUT.load(Ordering::Relaxed)
}

/// Whether a write to stdout failed for a reason other than a closed pipe.
/// The caller reports that as an error; a reader that went away is not one.
pub fn stdout_write_failed() -> bool {
    stdout_state() == FAILED
}

/// Print a line on stdout. A closed stdout is remembered, not raised: see [`crate::term`].
#[macro_export]
macro_rules! outln {
    () => { $crate::term::stdout_line(format_args!("")) };
    ($($arg:tt)*) => { $crate::term::stdout_line(format_args!($($arg)*)) };
}

/// Print a line on stderr. A closed stderr is remembered, not raised: see [`crate::term`].
#[macro_export]
macro_rules! errln {
    () => { $crate::term::stderr_line(format_args!("")) };
    ($($arg:tt)*) => { $crate::term::stderr_line(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fails(ErrorKind);
    impl Write for Fails {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_write_is_remembered_and_later_writes_are_skipped() {
        let state = AtomicU8::new(OPEN);
        write_to(
            &mut Fails(ErrorKind::BrokenPipe),
            &state,
            format_args!("a"),
            true,
        );
        assert_eq!(state.load(Ordering::Relaxed), CLOSED);

        // Skipped: a writer that would record FAILED is not called.
        write_to(
            &mut Fails(ErrorKind::Other),
            &state,
            format_args!("b"),
            true,
        );
        assert_eq!(state.load(Ordering::Relaxed), CLOSED);
    }

    #[test]
    fn an_error_other_than_a_closed_pipe_is_told_apart() {
        let state = AtomicU8::new(OPEN);
        write_to(
            &mut Fails(ErrorKind::StorageFull),
            &state,
            format_args!("a"),
            false,
        );
        assert_eq!(state.load(Ordering::Relaxed), FAILED);
    }

    /// A non-blocking pipe that is full is not a closed one: the write waits for the reader
    /// and every byte arrives, in order.
    #[test]
    fn a_write_to_a_full_non_blocking_pipe_waits_for_the_reader() {
        let (mut reader, writer) = std::io::pipe().unwrap();
        // SAFETY: plain syscalls on a descriptor this test owns.
        unsafe {
            let flags = libc::fcntl(writer.as_raw_fd(), libc::F_GETFL);
            assert_eq!(
                libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
                0
            );
        }
        // Fill the pipe until it refuses: from here on a plain write fails with EAGAIN.
        let mut filled = 0usize;
        loop {
            let n = unsafe { libc::write(writer.as_raw_fd(), [b'.'; 4096].as_ptr().cast(), 4096) };
            if n < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    ErrorKind::WouldBlock
                );
                break;
            }
            filled += n as usize;
        }
        let payload: Vec<u8> = (0..300_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let expected = payload.clone();
        let fd = writer.as_raw_fd();
        let state = std::sync::Arc::new(AtomicU8::new(OPEN));
        let seen = state.clone();
        let writing = std::thread::spawn(move || {
            let text = String::from_utf8(payload).unwrap();
            write_to(&mut Stream(fd), &seen, format_args!("{text}"), false);
            drop(writer); // end-of-file for the reader
        });
        // The reader starts only now, with the pipe full and the writer waiting.
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        writing.join().unwrap();
        assert_eq!(state.load(Ordering::Relaxed), OPEN);
        assert_eq!(received.len(), filled + expected.len());
        assert_eq!(&received[filled..], &expected[..]);
    }

    #[test]
    fn a_write_to_a_pipe_without_a_reader_is_a_closed_stream() {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let state = AtomicU8::new(OPEN);
        write_to(
            &mut Stream(writer.as_raw_fd()),
            &state,
            format_args!("x"),
            true,
        );
        assert_eq!(state.load(Ordering::Relaxed), CLOSED);
        // A descriptor that is not open at all is not an error, as with `println!`.
        let state = AtomicU8::new(OPEN);
        write_to(&mut Stream(-1), &state, format_args!("x"), true);
        assert_eq!(state.load(Ordering::Relaxed), OPEN);
    }

    #[test]
    fn only_a_pipe_or_a_socket_can_lose_its_reader() {
        let (reader, writer) = std::io::pipe().unwrap();
        assert!(can_lose_its_reader(writer.as_raw_fd()));
        assert!(can_lose_its_reader(reader.as_raw_fd()));
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(can_lose_its_reader(a.as_raw_fd()));
        // A file and the null device are held by the children directly, as before.
        let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        assert!(!can_lose_its_reader(file.as_raw_fd()));
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(!can_lose_its_reader(null.as_raw_fd()));
        assert!(!can_lose_its_reader(-1));
    }

    #[test]
    fn a_successful_write_adds_the_newline() {
        let state = AtomicU8::new(OPEN);
        let mut buf = Vec::new();
        write_to(&mut buf, &state, format_args!("a{}", 1), true);
        write_to(&mut buf, &state, format_args!("b"), false);
        assert_eq!(buf, b"a1\nb");
        assert_eq!(state.load(Ordering::Relaxed), OPEN);
    }
}
