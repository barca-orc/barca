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

use std::fmt;
use std::io::{ErrorKind, Write};
use std::sync::atomic::{AtomicU8, Ordering};

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
    let mut result = stream.write_fmt(args);
    if result.is_ok() && newline {
        result = stream.write_all(b"\n");
    }
    if let Err(e) = result {
        state.store(state_of(&e), Ordering::Relaxed);
    }
}

/// `println!` that does not panic. Use [`outln!`](crate::outln).
pub fn stdout_line(args: fmt::Arguments<'_>) {
    write_to(&mut std::io::stdout().lock(), &STDOUT, args, true);
}

/// Write text to stdout as it is (no newline added), without panicking.
pub fn stdout_str(text: &str) {
    write_to(
        &mut std::io::stdout().lock(),
        &STDOUT,
        format_args!("{text}"),
        false,
    );
}

/// `eprintln!` that does not panic. Use [`errln!`](crate::errln).
pub fn stderr_line(args: fmt::Arguments<'_>) {
    write_to(&mut std::io::stderr().lock(), &STDERR, args, true);
}

/// Flush stdout and say whether everything written to it got out.
fn stdout_state() -> u8 {
    if STDOUT.load(Ordering::Relaxed) == OPEN
        && let Err(e) = std::io::stdout().lock().flush()
    {
        STDOUT.store(state_of(&e), Ordering::Relaxed);
    }
    STDOUT.load(Ordering::Relaxed)
}

/// Whether a write to stdout failed for a reason other than a closed pipe (flushes first).
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
