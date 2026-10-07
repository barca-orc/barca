//! Starting and stopping barca's Python helper processes (`barca._transfer`, `barca._state`).
//!
//! Ctrl-C in a terminal is delivered to every process of the foreground job, helpers
//! included. What it means for a run is decided in one place, the coordinator: it cancels the
//! run, records it and stops its helpers. A helper that acted on the interrupt itself would
//! exit under a coordinator that is still waiting on it (the wait would then be reported as a
//! failed transfer, not as a cancellation) and would print a `KeyboardInterrupt` traceback.
//!
//! So helpers are started deaf to SIGINT ([`shield_from_ctrl_c`]) and are stopped by the
//! coordinator with SIGTERM ([`stop`]), which they answer by removing the temp files they
//! were writing and exiting. A helper that nobody can interrupt must not outlive a
//! coordinator that was killed, so each also gets a lifeline ([`give_lifeline`]).
//!
//! Every child process of barca, helpers and workers alike, is started through [`spawn`] or
//! [`spawn_std`]: one at a time.

use std::time::Duration;
use tokio::process::{Child, Command};

/// Held while a child process is being started.
static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Start a child process. Only one is started at a time, across the whole program.
///
/// Starting a child with a pipe (its stdout, its lifeline) creates the pipe first and marks
/// it close-on-exec second, where the system cannot do both in one step (macOS has no
/// `pipe2`). A second child started by another thread in between inherits the pipe's ends.
/// If that second child lives long, the pipe never reaches end-of-file: a wait for the first
/// child's output lasts as long as the second child does. Barca starts the state helper (its
/// output is read to the end) while it starts the transfer helper, which lives for the whole
/// run and which the run stops only at its own end: about one `barca get` in a thousand with
/// an artifact store hung for ever in its first pull. Started one at a time, no child can
/// inherit what was made for another.
pub(crate) fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
    let _one_at_a_time = SPAWNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}

/// [`spawn`] for a `std::process::Command`.
pub(crate) fn spawn_std(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    let _one_at_a_time = SPAWNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}

/// How long a helper gets to clean up after SIGTERM before it is killed.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(2);

/// Start `cmd` with SIGINT ignored. An ignored signal stays ignored across `exec`, and Python
/// leaves it that way, so there is no moment (not even while the interpreter starts) at which
/// a Ctrl-C raises `KeyboardInterrupt` in the helper.
pub(crate) fn shield_from_ctrl_c(cmd: &mut Command) {
    // SAFETY: `signal` is async-signal-safe and the closure touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            Ok(())
        });
    }
}

/// Give `cmd` a lifeline: its stdin is a pipe whose other end only this process holds, and
/// `BARCA_LIFELINE=stdin` tells the helper to watch it (`barca._lifeline`). When this process
/// is gone, however it went (`kill -9` included), the pipe reaches end-of-file and the helper
/// removes its temp files and exits. A helper deaf to Ctrl-C needs this: nobody else would
/// stop it.
///
/// The caller must keep the child's stdin handle open for as long as the helper should live.
pub(crate) fn give_lifeline(cmd: &mut Command) {
    cmd.env("BARCA_LIFELINE", "stdin")
        .stdin(std::process::Stdio::piped());
}

/// Ask the process `pid` to stop (SIGTERM). The caller keeps waiting on it.
pub(crate) fn terminate(pid: u32) {
    // SAFETY: plain syscall; a pid that is gone is an error we ignore.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// Kill the process `pid` (SIGKILL). The caller keeps waiting on it.
pub(crate) fn kill(pid: u32) {
    // SAFETY: plain syscall; a pid that is gone is an error we ignore.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Stop a helper: SIGTERM, up to [`STOP_GRACE`] for it to clean up and exit, then SIGKILL.
pub(crate) async fn stop(child: &mut Child) {
    if let Some(pid) = child.id() {
        terminate(pid);
        if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    /// A shell child that reports what it does with SIGINT and SIGTERM, then waits.
    fn reporter() -> Command {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            // `trap` with no arguments lists the traps in force; an inherited "ignore" is not
            // listed by every shell, so the child also sends itself the signal.
            "trap 'echo term; exit 0' TERM; kill -INT $$; echo survived; \
             while :; do sleep 0.05; done",
        ])
        .stdout(Stdio::piped())
        .kill_on_drop(true);
        cmd
    }

    #[tokio::test]
    async fn a_shielded_helper_ignores_sigint_and_stops_on_sigterm() {
        let mut cmd = reporter();
        shield_from_ctrl_c(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim(), "survived", "SIGINT must not end the helper");

        stop(&mut child).await;
        let mut rest = String::new();
        out.read_to_string(&mut rest).await.unwrap();
        assert_eq!(
            rest.trim(),
            "term",
            "stopped by SIGTERM, with its handler run"
        );
        assert!(child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn an_unshielded_child_is_ended_by_sigint() {
        // The control: without the shield the same child never gets to say `survived`. The
        // default action is set explicitly, because a test runner started in the background
        // hands its children an ignored SIGINT.
        let mut cmd = reporter();
        // SAFETY: as in `shield_from_ctrl_c`.
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        let mut said = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut said)
            .await
            .unwrap();
        assert_eq!(said, "");
        assert!(!child.wait().await.unwrap().success());
    }

    /// A child whose output is read to the end, started while long-lived children are being
    /// started on other threads, must not have to wait for them: none of them may inherit its
    /// pipe. (Without [`spawn`]'s lock this hangs now and then on macOS; it cannot fail
    /// spuriously.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn output_is_read_to_the_end_while_long_lived_siblings_start() {
        let mut waits = Vec::new();
        let mut siblings = Vec::new();
        for _ in 0..200 {
            siblings.push(tokio::spawn(async {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 60"]).kill_on_drop(true);
                shield_from_ctrl_c(&mut cmd);
                give_lifeline(&mut cmd);
                spawn(&mut cmd).unwrap()
            }));
            waits.push(tokio::spawn(async {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "echo done"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                let child = spawn(&mut cmd).unwrap();
                tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await
            }));
        }
        for wait in waits {
            let out = wait
                .await
                .unwrap()
                .expect("output never ended: a sibling holds the pipe");
            assert_eq!(out.unwrap().stdout, b"done\n");
        }
        for sibling in siblings {
            let _ = sibling.await.unwrap().kill().await;
        }
    }

    #[tokio::test]
    async fn a_helper_that_ignores_sigterm_is_killed_after_the_grace() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "trap '' TERM; echo ready; while :; do sleep 0.05; done",
        ])
        .stdout(Stdio::piped())
        .kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim(), "ready");
        stop(&mut child).await;
        assert!(!child.wait().await.unwrap().success());
    }
}
