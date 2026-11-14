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

/// Write text to stderr as it is (no newline added), without panicking. With structured
/// lines on (`barca serve` under `BARCA_LOG=json`), each line is a JSON object on stdout
/// ([`crate::jsonlog`]).
pub fn stderr_str(text: &str) {
    if crate::jsonlog::enabled() {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            stdout_line(format_args!("{}", crate::jsonlog::render(line, None, None)));
        }
        return;
    }
    clear_of_bar(|| {
        let _order = ChildOutput::copy_pending();
        let _one = locked(&STDERR_WRITER);
        write_to(
            &mut Stream(libc::STDERR_FILENO),
            &STDERR,
            format_args!("{text}"),
            false,
        );
    });
}

/// The progress bar a run is drawing on stderr, if any. A line written to stderr while it is
/// drawn would land on the bar's line and be overwritten or run into it, so [`stderr_line`] and
/// [`stderr_str`] clear the bar, write, and draw it again below. Held weakly: the bar's owner
/// decides when it ends.
static PROGRESS_BAR: Mutex<Option<indicatif::WeakProgressBar>> = Mutex::new(None);

/// Route every later stderr line above `bar` while it is drawn (until it is finished or
/// dropped).
pub fn above_progress_bar(bar: &indicatif::ProgressBar) {
    *PROGRESS_BAR.lock().unwrap_or_else(|e| e.into_inner()) = Some(bar.downgrade());
}

/// The progress bar currently drawn on stderr.
fn drawn_bar() -> Option<indicatif::ProgressBar> {
    let bar = PROGRESS_BAR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .upgrade()?;
    drawn(&bar).then_some(bar)
}

/// Whether `bar` is on the screen: a bar on a stream that is not a terminal is hidden, and a
/// finished one has been cleared or left behind for good.
fn drawn(bar: &indicatif::ProgressBar) -> bool {
    !bar.is_hidden() && !bar.is_finished()
}

/// Run `write` with the progress bar, if one is drawn, cleared from the screen.
fn clear_of_bar<R>(write: impl FnOnce() -> R) -> R {
    clear_of(drawn_bar(), write)
}

/// Run `write` with `bar` cleared from the screen, and draw it again after.
fn clear_of<R>(bar: Option<indicatif::ProgressBar>, write: impl FnOnce() -> R) -> R {
    match bar {
        Some(bar) => bar.suspend(write),
        None => write(),
    }
}

/// `eprintln!` that does not panic. Use [`errln!`](crate::errln). Child output
/// written before this call is copied first, so the line keeps its place. With structured
/// lines on (`barca serve` under `BARCA_LOG=json`), the line is a JSON object on stdout
/// instead ([`crate::jsonlog`]).
pub fn stderr_line(args: fmt::Arguments<'_>) {
    if crate::jsonlog::enabled() {
        let text = fmt::format(args);
        stdout_line(format_args!(
            "{}",
            crate::jsonlog::render(&text, None, None)
        ));
        return;
    }
    clear_of_bar(|| {
        let _order = ChildOutput::copy_pending();
        let _one = locked(&STDERR_WRITER);
        write_to(&mut Stream(libc::STDERR_FILENO), &STDERR, args, true);
    });
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
                // With structured lines on, barca reads its children's output even when
                // stderr is a terminal or a file: their `[barca] ` lines become JSON objects.
                if !can_lose_its_reader(libc::STDERR_FILENO) && !crate::jsonlog::enabled() {
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

/// A partial line of the children's output, held while structured lines are on.
static PARTIAL: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// A partial line longer than this is written as it is: a progress bar redrawn with `\r`
/// never ends its line.
const MAX_PARTIAL: usize = 64 * 1024;

/// Write the children's bytes to stderr as they are; drop them once stderr is closed. With
/// structured lines on, a whole line that starts with `[barca] ` (a notice from a worker or the
/// transfer helper) is a JSON object on stdout instead; every other line stays on stderr.
fn forward(bytes: &[u8]) {
    if crate::jsonlog::enabled() {
        let mut partial = PARTIAL.lock().unwrap_or_else(|e| e.into_inner());
        partial.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(end) = partial[start..].iter().position(|&b| b == b'\n') {
            forward_line(&partial[start..start + end + 1]);
            start += end + 1;
        }
        partial.drain(..start);
        if partial.len() > MAX_PARTIAL {
            let rest = std::mem::take(&mut *partial);
            write_stderr(&rest);
        }
        return;
    }
    write_stderr(bytes);
}

/// One whole line of child output (with its newline) under structured lines.
fn forward_line(line: &[u8]) {
    match line.strip_prefix(b"[barca] ") {
        Some(_) => {
            let text = String::from_utf8_lossy(line);
            let text = text.trim_end_matches(['\n', '\r']);
            stdout_line(format_args!("{}", crate::jsonlog::render(text, None, None)));
        }
        None => write_stderr(line),
    }
}

/// Write what is left of a partial child line, before the process exits.
fn flush_partial() {
    let rest = std::mem::take(&mut *PARTIAL.lock().unwrap_or_else(|e| e.into_inner()));
    if !rest.is_empty() {
        write_stderr(&rest);
    }
}

fn write_stderr(bytes: &[u8]) {
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
    flush_partial();
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

    /// A terminal that keeps what is on its current line.
    #[derive(Clone, Debug, Default)]
    struct Screen(std::sync::Arc<Mutex<String>>);
    impl Screen {
        fn line(&self) -> String {
            self.0.lock().unwrap().trim_end().to_string()
        }
    }
    impl indicatif::TermLike for Screen {
        fn width(&self) -> u16 {
            80
        }
        fn move_cursor_up(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_down(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_right(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_left(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn write_line(&self, s: &str) -> std::io::Result<()> {
            *self.0.lock().unwrap() = s.to_string();
            Ok(())
        }
        fn write_str(&self, s: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push_str(s);
            Ok(())
        }
        fn clear_line(&self) -> std::io::Result<()> {
            self.0.lock().unwrap().clear();
            Ok(())
        }
        fn flush(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_line_is_written_with_the_progress_bar_cleared_and_the_bar_comes_back() {
        // A line written while the bar was drawn landed on the bar's own line (`0/2 |[barca]
        // checking artifact store ...`) and left a stray bar frame on screen.
        let screen = Screen::default();
        let bar = indicatif::ProgressBar::with_draw_target(
            Some(2),
            indicatif::ProgressDrawTarget::term_like(Box::new(screen.clone())),
        );
        bar.set_style(indicatif::ProgressStyle::with_template("BAR {pos}/{len}").unwrap());
        bar.tick();
        assert_eq!(screen.line(), "BAR 0/2");
        assert!(drawn(&bar));
        let seen = clear_of(Some(bar.clone()), || screen.line());
        assert_eq!(
            seen, "",
            "the bar must be off its line while a line is written"
        );
        assert_eq!(screen.line(), "BAR 0/2", "and drawn again after");
        bar.finish_and_clear();
        assert!(!drawn(&bar), "a finished bar is not cleared or drawn again");
        assert!(!drawn(&indicatif::ProgressBar::hidden()));
        // No bar: the line is just written.
        assert_eq!(clear_of(None, || 7), 7);
    }

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
