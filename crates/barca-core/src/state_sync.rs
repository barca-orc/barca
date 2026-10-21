//! Cancellation and deadlines for the coordinator's helper processes and database locks, and
//! the write-ahead-log checkpoint every copy of the metadata DB starts from.
//!
//! Critical invariant: turso is WAL-only and barca's normal operation leaves most data in
//! `metadata.db-wal`. Before the database file is copied (a backup, RFC-0009) the WAL must be
//! checkpointed into the main file (`PRAGMA wal_checkpoint(TRUNCATE)`); a copy of the main
//! file alone would be an old or empty database.

use crate::BarcaError;
use tokio_util::sync::CancellationToken;

/// When a helper or a lock wait gives up: `cancel` fires, or `deadline` passes. Either way the
/// helper is stopped and the result is [`BarcaError::Cancelled`].
#[derive(Clone, Copy, Default)]
pub struct Until<'a> {
    pub cancel: Option<&'a CancellationToken>,
    pub deadline: Option<std::time::Instant>,
}

impl<'a> Until<'a> {
    pub(crate) fn check(self) -> Result<(), BarcaError> {
        if self.cancel.is_some_and(CancellationToken::is_cancelled)
            || self.remaining().is_some_and(|left| left.is_zero())
        {
            Err(BarcaError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Only wrap cancellable admission, never an admitted database mutation.
    pub(crate) async fn admit<T>(
        self,
        work: impl std::future::Future<Output = T>,
    ) -> Result<T, BarcaError> {
        self.check()?;
        let cancelled = async {
            match self.cancel {
                Some(cancel) => cancel.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let expired = async {
            match self.remaining() {
                Some(left) => tokio::time::sleep(left).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = cancelled => Err(BarcaError::Cancelled),
            _ = expired => Err(BarcaError::Cancelled),
            result = work => Ok(result),
        }
    }

    /// Never: the transfer runs to its end.
    pub fn done() -> Self {
        Self::default()
    }

    pub fn cancelled(cancel: &'a CancellationToken) -> Self {
        Self {
            cancel: Some(cancel),
            deadline: None,
        }
    }

    /// The time left, when there is a deadline (zero once it has passed).
    pub(crate) fn remaining(&self) -> Option<std::time::Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(std::time::Instant::now()))
    }
}

/// The reason a state helper failed: its last `error: ...` line (what `python -m barca._state`
/// prints), else its last non-empty line. Library warnings printed before it (google-auth's
/// quota-project notice, deprecation warnings) are dropped so they never reach the error.
pub(crate) fn helper_cause(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("error: "))
        .or_else(|| lines.last().copied())
        .unwrap_or("the state helper exited with an error")
        .to_string()
}

/// Checkpoint the WAL into the main database file and verify nothing is
/// left behind. Must be called with no other connections open on the file
/// (the caller drops all handles first).
pub async fn checkpoint_truncate(db_path: &str) -> Result<(), BarcaError> {
    crate::db::fold_log_locked(db_path).await
}

pub use crate::db::wal_is_clean;

#[cfg(test)]
mod tests {
    #[test]
    fn helper_cause_drops_library_warnings() {
        let stderr = b"/x/google/auth/_default.py:113: UserWarning: Your application has \
authenticated using end user credentials\n  warnings.warn(_CLOUD_SDK_CREDENTIALS_WARNING)\n\
error: RefreshError: Reauthentication is needed.\n";
        assert_eq!(
            super::helper_cause(stderr),
            "RefreshError: Reauthentication is needed."
        );
        assert_eq!(super::helper_cause(b"\n  boom  \n"), "boom");
        assert_eq!(
            super::helper_cause(b""),
            "the state helper exited with an error"
        );
    }

    use super::*;
    use crate::helper_proc::{OutputFailed as HelperFailed, run_to_output as run_helper};
    use std::path::Path;
    use tokio::process::Command;
    use turso::Builder;

    /// A helper that stalls is stopped at its limit; one that finishes in time is not
    /// disturbed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_helper_that_outlives_its_limit_is_killed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pid_file.display());
        let mut stalled = Command::new("sh");
        stalled.arg("-c").arg(&script);
        let started = std::time::Instant::now();
        let limit = std::time::Duration::from_millis(500);
        let failed = run_helper(stalled, None, Some(limit)).await;
        assert!(matches!(failed, Err(HelperFailed::TimedOut(l)) if l == limit));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // The process is gone, not left downloading.
        let pid: i64 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while crate::db::pid_alive(pid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !crate::db::pid_alive(pid),
            "the stalled helper is still running"
        );

        let mut quick = Command::new("sh");
        quick.arg("-c").arg("echo done");
        let out = run_helper(quick, None, Some(std::time::Duration::from_secs(30))).await;
        assert_eq!(out.ok().map(|o| o.stdout), Some(b"done\n".to_vec()));
    }

    /// A script that writes its pid to `pid_file` and then does `then`.
    fn script(pid_file: &Path, then: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("echo $$ > {}; {then}", pid_file.display()));
        cmd
    }

    async fn pid_in(pid_file: &Path) -> i64 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Ok(pid) = std::fs::read_to_string(pid_file)
                .unwrap_or_default()
                .trim()
                .parse()
            {
                return pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the helper never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A cancelled command must not return while a helper it started is alive: nothing may be
    /// left to finish a transfer, or to print, after the command has reported its outcome.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancelled_helper_is_gone_when_the_call_returns() {
        let dir = tempfile::tempdir().unwrap();
        for (name, then) in [
            ("obeys", "exec sleep 30"),
            // Ignores SIGTERM: killed once the grace period is over.
            ("deaf", "trap '' TERM; while :; do sleep 0.05; done"),
        ] {
            let pid_file = dir.path().join(name);
            let cancel = CancellationToken::new();
            let (seen, stop) = (pid_file.clone(), cancel.clone());
            let canceller = tokio::spawn(async move {
                let pid = pid_in(&seen).await;
                stop.cancel();
                pid
            });
            let failed = run_helper(script(&pid_file, then), Some(&cancel), None).await;
            assert!(matches!(failed, Err(HelperFailed::Cancelled)), "{name}");
            let pid = canceller.await.unwrap();
            assert!(
                !crate::db::pid_alive(pid),
                "{name}: alive after the call returned"
            );
        }
    }

    /// The terminal's Ctrl-C does not reach helpers, and a coordinator that is killed cannot
    /// stop them. Each is given a lifeline instead: its stdin is a pipe that closes with the
    /// coordinator.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_helper_is_started_in_its_own_group_and_with_a_lifeline() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // Checks the lifeline variable, then becomes `cat`, which ends only when its stdin
        // does: the pipe run_helper holds open.
        let helper = script(&pid_file, "test \"$BARCA_LIFELINE\" = stdin && exec cat");
        let cancel = CancellationToken::new();
        let run = run_helper(helper, Some(&cancel), None);
        tokio::pin!(run);
        let pid = tokio::select! {
            _ = &mut run => panic!("the helper ended although its lifeline is open"),
            pid = pid_in(&pid_file) => pid,
        };
        // Still running a moment later: its stdin is not at its end.
        let still = tokio::time::timeout(std::time::Duration::from_millis(300), &mut run).await;
        assert!(
            still.is_err(),
            "the helper ended although its lifeline is open"
        );
        assert!(crate::db::pid_alive(pid));
        // In a group of its own (its pid), not the one the terminal's Ctrl-C goes to.
        // SAFETY: plain syscalls.
        let (group, ours) = unsafe { (libc::getpgid(pid as i32), libc::getpgrp()) };
        assert_eq!(i64::from(group), pid);
        assert_ne!(group, ours);
        cancel.cancel();
        assert!(matches!(run.await, Err(HelperFailed::Cancelled)));
        assert!(!crate::db::pid_alive(pid));
    }

    async fn open_and_count(db_path: &str, table: &str) -> u64 {
        let db = Builder::new_local(db_path).build().await.unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get_value(0).unwrap().as_integer().copied().unwrap() as u64
    }

    /// The load-bearing spike for metadata backups: after
    /// wal_checkpoint(TRUNCATE), the -wal sidecar must be empty/absent and
    /// all rows must be readable from the main file alone.
    #[tokio::test]
    async fn checkpoint_truncate_collapses_wal_into_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();

        // Create a table and write enough rows that data definitely lives in the WAL.
        {
            let db = Builder::new_local(&db_path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)",
                (),
            )
            .await
            .unwrap();
            for i in 0..200 {
                conn.execute("INSERT INTO t (v) VALUES (?1)", [format!("value-{i}")])
                    .await
                    .unwrap();
            }
        }

        // Sanity: without a checkpoint the WAL holds the data.
        let wal = format!("{db_path}-wal");
        let wal_size_before = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert!(
            wal_size_before > 0,
            "expected data in the WAL before checkpoint (got {wal_size_before} bytes) — \
             if this fails, turso started auto-checkpointing and the sync design should be revisited"
        );

        checkpoint_truncate(&db_path).await.unwrap();
        assert!(wal_is_clean(&db_path), "WAL must be empty after checkpoint");

        // The main file alone (as a backup copies it: without the WAL)
        // must contain every row.
        let copy = dir.path().join("uploaded.db").to_string_lossy().to_string();
        std::fs::copy(&db_path, &copy).unwrap();
        assert_eq!(open_and_count(&copy, "t").await, 200);
    }
}
