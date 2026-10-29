//! Per-database admission, separate from the database's short write lock.
use crate::{BarcaError, config::ResolvedConfig, helper_proc::Until};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Routine,
    Final,
}

/// Held through copy, hashing and upload. Dropping it also handles cancellation/crashes.
pub struct Admission(std::fs::File);

impl Drop for Admission {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Copy)]
struct Evidence {
    stamp: u128,
    interval: Duration,
    observed: Instant,
    remaining: Duration,
}

type Observations = Mutex<HashMap<PathBuf, Evidence>>;
static OBSERVATIONS: OnceLock<Observations> = OnceLock::new();

/// Cache a wall-clock observation once; subsequent checks use monotonic elapsed time.
/// A future timestamp waits at most one interval, including after clock rollback.
fn remaining(
    old: Option<Evidence>,
    stamp: u128,
    wall: u128,
    now: Instant,
    interval: Duration,
) -> (Evidence, Duration) {
    let evidence = match old {
        Some(old) if old.stamp == stamp && old.interval == interval => old,
        _ => Evidence {
            stamp,
            interval,
            observed: now,
            remaining: {
                let age = wall.saturating_sub(stamp).min(interval.as_nanos());
                interval.saturating_sub(Duration::new(
                    (age / 1_000_000_000) as u64,
                    (age % 1_000_000_000) as u32,
                ))
            },
        },
    };
    (
        evidence,
        evidence
            .remaining
            .saturating_sub(now.saturating_duration_since(evidence.observed)),
    )
}

pub async fn admit(
    cfg: &ResolvedConfig,
    request: Request,
    until: Until<'_>,
) -> Result<Option<Admission>, BarcaError> {
    until.check()?;
    if request == Request::Routine && cfg.backup_interval.0.is_none() {
        return Ok(None);
    }
    let dir = cfg.local_backup_dir();
    let failed = |e: std::io::Error| {
        BarcaError::Other(format!(
            "metadata backup admission in {}: {e}",
            dir.display()
        ))
    };
    std::fs::create_dir_all(&dir).map_err(failed)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".admission.lock"))
        .map_err(failed)?;
    let wait_started = Instant::now();
    loop {
        until.check()?;
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if request == Request::Routine => {
                return Ok(None);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if wait_started.elapsed() >= Duration::from_secs(60) {
                    return Err(BarcaError::Other(
                        "timed out waiting for the metadata backup operation".into(),
                    ));
                }
                until
                    .admit(tokio::time::sleep(Duration::from_millis(20)))
                    .await?;
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(failed(e)),
        }
    }
    let guard = Admission(file);
    let path = dir.join(".last-attempt");
    let key = std::fs::canonicalize(&path).unwrap_or_else(|_| {
        std::fs::canonicalize(&dir)
            .unwrap_or(dir.clone())
            .join(".last-attempt")
    });
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let now = Instant::now();
    let mut observations = OBSERVATIONS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let (Request::Routine, Some(interval), Some(stamp)) = (
        request,
        cfg.backup_interval.0,
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.parse::<u128>().ok()),
    ) {
        let (evidence, left) =
            remaining(observations.get(&key).copied(), stamp, wall, now, interval);
        observations.insert(key.clone(), evidence);
        if !left.is_zero() {
            return Ok(None);
        }
    }
    // Atomic evidence replacement under the operation lock; record attempts, including
    // failures and final attempts, rather than mistaking admission for backup coverage.
    let tmp = dir.join(".last-attempt.tmp");
    std::fs::write(&tmp, wall.to_string()).map_err(failed)?;
    std::fs::rename(&tmp, &path).map_err(failed)?;
    observations.insert(
        key,
        Evidence {
            stamp: wall,
            interval: cfg.backup_interval.0.unwrap_or_default(),
            observed: now,
            remaining: cfg.backup_interval.0.unwrap_or_default(),
        },
    );
    Ok(Some(guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_busy_restart_and_final_admission() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::resolve_in(None, dir.path()).unwrap();
        cfg.db_path = dir
            .path()
            .join("metadata.db")
            .to_string_lossy()
            .into_owned();
        cfg.backup_interval = crate::config::BackupInterval(None);
        assert!(
            admit(&cfg, Request::Routine, Until::done())
                .await
                .unwrap()
                .is_none()
        );
        assert!(!cfg.local_backup_dir().exists());
        let final_guard = admit(&cfg, Request::Final, Until::done())
            .await
            .unwrap()
            .unwrap();
        cfg.backup_interval = crate::config::BackupInterval(Some(Duration::from_secs(3600)));
        assert!(
            admit(&cfg, Request::Routine, Until::done())
                .await
                .unwrap()
                .is_none()
        );
        let cancel = crate::CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            admit(&cfg, Request::Final, Until::cancelled(&cancel)).await,
            Err(BarcaError::Cancelled)
        ));
        let until = Until {
            cancel: None,
            deadline: Some(Instant::now() + Duration::from_millis(30)),
        };
        assert!(matches!(
            admit(&cfg, Request::Final, until).await,
            Err(BarcaError::Cancelled)
        ));
        drop(final_guard);
        // Another process has no in-memory observation: persisted admission still wins.
        let key = std::fs::canonicalize(cfg.local_backup_dir().join(".last-attempt")).unwrap();
        OBSERVATIONS.get().unwrap().lock().unwrap().remove(&key);
        assert!(
            admit(&cfg, Request::Routine, Until::done())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            admit(&cfg, Request::Final, Until::done())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn clock_rollback_and_repeated_checks_do_not_extend_the_interval() {
        let now = Instant::now();
        let interval = Duration::from_secs(60);
        let (e, left) = remaining(None, 200, 100, now, interval);
        assert_eq!(left, interval);
        assert_eq!(
            remaining(Some(e), 200, 0, now + interval, interval).1,
            Duration::ZERO
        );
        assert_eq!(
            remaining(None, 100, 100 + interval.as_nanos(), now, interval).1,
            Duration::ZERO
        );
    }
}
