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
//! were writing and exiting.

use std::time::Duration;
use tokio::process::{Child, Command};

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
