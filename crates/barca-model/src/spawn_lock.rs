//! Starting a child process one at a time, across the whole program (#293): the lock that
//! `helper_proc::spawn` and [`spawn_std`] hold while a child is started.
//!
//! A process primitive below every layer, so that loading a pipeline (the partition-key
//! resolver, `batches(...)`) starts its subprocesses under the same lock as the engine's
//! workers and helpers. See `crate::helper_proc` for why the lock exists.

/// Held while a child process is being started.
pub static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `helper_proc::spawn` for a `std::process::Command`.
pub fn spawn_std(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    let _one_at_a_time = SPAWNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}

/// Run `cmd` to completion and collect its output, started through [`spawn_std`]: what
/// `Command::output` does, one at a time.
pub fn output_std(cmd: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    spawn_std(cmd)?.wait_with_output()
}

/// Run an owned resolver process, polling cancellation while its pipes are drained.
/// A cancellable child has its own group: cancellation also stops its subprocesses.
/// Without a cancellation owner, keep terminal signals reaching the resolver directly.
/// Blocking callers must run this on the blocking pool, not an async runtime thread.
pub fn output_until(
    cmd: &mut std::process::Command,
    input: Vec<u8>,
    until: crate::process::Until<'_>,
) -> Result<std::process::Output, crate::BarcaError> {
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};
    until.check()?;
    let owned = until.cancel.is_some() || until.deadline.is_some();
    if owned {
        cmd.process_group(0);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = spawn_std(cmd)?;
    let pid = child.id() as libc::pid_t;
    let signal_target = if owned { -pid } else { pid };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    std::thread::scope(|scope| {
        let write = scope.spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let read = |mut pipe: Box<dyn Read + Send>| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        };
        let out = scope.spawn(move || read(Box::new(stdout)));
        let err = scope.spawn(move || read(Box::new(stderr)));
        let mut status = None;
        let mut stopped = None;
        let mut failure = None;
        loop {
            if stopped.is_none()
                && let Err(error) = until.check()
            {
                failure = Some(error);
                stopped = Some(Instant::now());
                // SAFETY: the child leads the private process group created above.
                unsafe {
                    libc::kill(signal_target, libc::SIGCONT);
                    libc::kill(signal_target, libc::SIGTERM);
                }
            }
            if stopped.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) {
                // SIGTERM may be ignored by an expression or one of its subprocesses.
                unsafe {
                    libc::kill(signal_target, libc::SIGKILL);
                }
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(found) => status = found,
                    Err(error) => {
                        unsafe {
                            libc::kill(signal_target, libc::SIGKILL);
                        }
                        let _ = child.wait();
                        failure = Some(error.into());
                        break;
                    }
                }
            }
            if status.is_some() && out.is_finished() && err.is_finished() && write.is_finished() {
                // A descendant can close its pipes and still be alive. Cancellation owns it too.
                if stopped.is_some() {
                    unsafe {
                        libc::kill(signal_target, libc::SIGKILL);
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let stdout = out.join().expect("resolver stdout reader");
        let stderr = err.join().expect("resolver stderr reader");
        let _ = write.join();
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(std::process::Output {
            status: status.expect("reaped resolver"),
            stdout: stdout?,
            stderr: stderr?,
        })
    })
}

#[cfg(test)]
mod resolver_tests {
    use super::output_until;
    use crate::process::Until;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn a_resolver_without_a_cancel_owner_stays_in_the_terminals_group() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "ps -o pgid= -p $$"]);
        let output = output_until(&mut cmd, Vec::new(), Until::done()).unwrap();
        let group: libc::pid_t = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: getpgrp reads this process's group.
        assert_eq!(group, unsafe { libc::getpgrp() });
    }

    #[test]
    fn resolver_pipes_are_drained_while_the_request_is_written() {
        let body = vec![b'x'; 1024 * 1024];
        let mut cmd = Command::new("cat");
        let output = output_until(
            &mut cmd,
            body.clone(),
            Until {
                cancel: None,
                deadline: Some(Instant::now() + Duration::from_secs(10)),
            },
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, body);
    }

    #[test]
    fn resolver_ignoring_term_is_reaped_even_when_stdin_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let cancel = crate::CancellationToken::new();
        let token = cancel.clone();
        let marker = pid_file.clone();
        let canceller = std::thread::spawn(move || {
            let start = Instant::now();
            while !marker.exists() {
                assert!(start.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(10));
            }
            token.cancel();
        });
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' TERM; echo $$ > \"$1\"; exec sleep 60")
            .arg("resolver")
            .arg(&pid_file);
        let start = Instant::now();
        let result = output_until(&mut cmd, vec![b'x'; 1024 * 1024], Until::cancelled(&cancel));
        canceller.join().unwrap();
        assert!(matches!(result, Err(crate::BarcaError::Cancelled)));
        assert!(start.elapsed() < Duration::from_secs(5));
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!crate::process::pid_alive(pid));
    }
}
