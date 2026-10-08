//! Starting and stopping barca's child processes: the Python helpers (`barca._transfer`,
//! `barca._state`) and everything else it runs.
//!
//! Ctrl-C in a terminal is delivered to every process of the foreground job, helpers
//! included. What it means for a run is decided in one place, the coordinator: it cancels the
//! run, records it and stops its helpers. A helper that acted on the interrupt itself would
//! exit under a coordinator that is still waiting on it (the wait would then be reported as a
//! failed transfer, not as a cancellation) and would print a `KeyboardInterrupt` traceback.
//!
//! So helpers are started where the terminal's Ctrl-C does not reach them
//! ([`shield_from_ctrl_c`]) and are stopped by the coordinator with SIGTERM ([`stop`]), which
//! they answer by removing the temp files they were writing and exiting. A helper that the
//! terminal cannot interrupt must not outlive a coordinator that was killed, so each also gets
//! a lifeline ([`give_lifeline`]).
//!
//! Every child process of barca, helpers and workers alike, is started through [`spawn`] or
//! [`spawn_std`]: one at a time. That includes the children its tests start: a test that
//! started one on its own would bring back, inside the test process, the race the lock removes
//! (#293). `tests::every_child_process_is_started_through_this_module` fails on a `.spawn()`,
//! `.output()` or `.status()` anywhere else in `crates/`.

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
pub fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
    let _one_at_a_time = SPAWNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}

/// [`spawn`] for a `std::process::Command`.
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

/// How long a helper gets to clean up after SIGTERM before it is killed.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(2);

/// Start `cmd` in a process group of its own. The terminal sends Ctrl-C to the foreground
/// process group, which is the coordinator's, so the helper never sees it: not while the
/// interpreter starts either, since the group is set when the process is created.
///
/// Why not an ignored SIGINT set before `exec` (what this did at first): that needs a
/// `pre_exec` hook, which makes the standard library start the child with `fork` and wait on
/// a pipe of its own to learn that `exec` happened, and that pipe can be inherited by a child
/// started at the same instant just like the ones [`spawn`] describes. A process group is
/// set by `posix_spawn` itself, with no hook and no pipe.
pub(crate) fn shield_from_ctrl_c(cmd: &mut Command) {
    cmd.process_group(0);
}

/// Give `cmd` a lifeline: its stdin is a pipe whose other end only this process holds, and
/// `BARCA_LIFELINE=stdin` tells the helper to watch it (`barca._lifeline`). When this process
/// is gone, however it went (`kill -9` included), the pipe reaches end-of-file and the helper
/// removes its temp files and exits. A helper the terminal cannot interrupt needs this: nobody
/// else would stop it.
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

    /// A shell child that prints its process group, then waits; `term` on SIGTERM.
    fn reporter() -> Command {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "trap 'echo term; exit 0' TERM; ps -o pgid= -p $$; \
             while :; do sleep 0.05; done",
        ])
        .stdout(Stdio::piped())
        .kill_on_drop(true);
        cmd
    }

    fn own_group() -> i64 {
        // SAFETY: plain syscall.
        i64::from(unsafe { libc::getpgrp() })
    }

    #[tokio::test]
    async fn a_shielded_helper_is_outside_the_terminals_job_and_stops_on_sigterm() {
        let mut cmd = reporter();
        shield_from_ctrl_c(&mut cmd);
        let mut child = spawn(&mut cmd).unwrap();
        let pid = i64::from(child.id().unwrap());
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).await.unwrap();
        let group: i64 = line.trim().parse().unwrap();
        // A group of its own: a Ctrl-C sent to this process's group does not include it.
        assert_eq!(group, pid);
        assert_ne!(group, own_group());

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
    async fn an_unshielded_child_shares_the_coordinators_group() {
        // The control: without the shield the child is in the group Ctrl-C is sent to.
        let mut child = spawn(&mut reporter()).unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim().parse::<i64>().unwrap(), own_group());
        stop(&mut child).await;
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

    /// Calls that start a child process without [`spawn`]'s lock, in the Rust sources under
    /// `dir`: `(file, line number, line)`.
    ///
    /// `.spawn()` with no argument is `Command::spawn` (a thread or a task is spawned with
    /// one). `.output()` and `.status()` are looked for only in a file that builds a
    /// `Command`, since HTTP responses have a `.status()` too.
    fn unlocked_spawns(dir: &std::path::Path, found: &mut Vec<(String, usize, String)>) {
        let spawn = [".spawn", "()"].concat();
        let waits = [[".output", "()"].concat(), [".status", "()"].concat()];
        let builds_a_command = ["Command", "::new"].concat();
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                unlocked_spawns(&path, found);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let this_module = path.ends_with("barca-core/src/helper_proc.rs");
            let has_command = text.contains(&builds_a_command);
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let starts_one = code.contains(&spawn)
                    || (has_command && waits.iter().any(|w| code.contains(w.as_str())));
                // The two calls this module exists to make.
                let the_locked_call = this_module && code.trim() == format!("cmd{spawn}");
                if starts_one && !the_locked_call {
                    found.push((path.display().to_string(), i + 1, line.trim().to_string()));
                }
            }
        }
    }

    /// Every child process, in tests too, is started through [`spawn`], [`spawn_std`] or
    /// [`output_std`]. One started any other way can inherit a pipe made for a child that is
    /// being started on another thread at that instant and hold it open for as long as it
    /// lives: the hang these functions prevent, and what made
    /// `output_is_read_to_the_end_while_long_lived_siblings_start` time out once in a full
    /// test run (#293).
    #[test]
    fn every_child_process_is_started_through_this_module() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found = Vec::new();
        for krate in std::fs::read_dir(&crates).unwrap() {
            let krate = krate.unwrap().path();
            for sub in ["src", "tests"] {
                if krate.join(sub).is_dir() {
                    unlocked_spawns(&krate.join(sub), &mut found);
                }
            }
        }
        let listed: Vec<String> = found
            .iter()
            .map(|(file, line, text)| format!("{file}:{line}: {text}"))
            .collect();
        assert!(
            found.is_empty(),
            "child processes started without helper_proc's lock (use helper_proc::spawn, \
             spawn_std or output_std):\n{}",
            listed.join("\n")
        );
    }

    /// The scan finds what it is meant to find, and nothing in a file without a `Command`.
    #[test]
    fn the_scan_for_unlocked_spawns_finds_each_kind() {
        let dir = tempfile::tempdir().unwrap();
        let call = |name: &str| format!("    let _ = cmd{name}{};\n", "()");
        std::fs::write(
            dir.path().join("a.rs"),
            format!(
                "fn f() {{\n    let mut cmd = std::process::Command{}(\"true\");\n{}{}{}}}\n",
                "::new",
                call(".spawn"),
                call(".output"),
                call(".status"),
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.rs"),
            format!(
                "fn g(resp: Response) {{\n{}    tokio::spawn(async {{}});\n}}\n",
                call(".status").replace("cmd", "resp")
            ),
        )
        .unwrap();
        let mut found = Vec::new();
        unlocked_spawns(dir.path(), &mut found);
        let lines: Vec<usize> = found.iter().map(|(_, line, _)| *line).collect();
        assert_eq!(lines, [3, 4, 5], "{found:?}");
        assert!(found.iter().all(|(file, _, _)| file.ends_with("a.rs")));
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
        let mut child = spawn(&mut cmd).unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim(), "ready");
        stop(&mut child).await;
        assert!(!child.wait().await.unwrap().success());
    }
}
