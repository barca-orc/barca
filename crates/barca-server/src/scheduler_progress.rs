//! Process-local scheduler progress. No metadata or async locks on the health read path.
use barca_service::served::SchedulerLiveness;
use std::{sync::Mutex, time::Duration};

use tokio::time::Instant;

/// How long a live scheduler step waits for the metadata database before it gives up. It
/// lives here, below the scheduler, so the progress record can use it.
pub(crate) const METADATA_WAIT: Duration = Duration::from_millis(300);

const STALE: Duration = Duration::from_secs(3 * barca_service::liveness::HEARTBEAT_SECONDS as u64);

#[derive(Default)]
pub(crate) struct Progress(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    heartbeat: Option<Instant>,
    published: Option<Instant>,
    operation: Option<(&'static str, i64)>,
    starting: bool,
    exited: bool,
    timeout: Option<String>,
    admission_errors: std::collections::BTreeMap<String, String>,
    view: SchedulerLiveness,
}

impl Progress {
    pub fn start(&self) {
        self.heartbeat(true);
    }

    pub fn beat(&self) {
        self.heartbeat(false);
    }

    fn heartbeat(&self, starting: bool) {
        let mut p = self.0.lock().unwrap();
        let now = Instant::now();
        p.heartbeat = Some(now);
        // Keep conditional health reads stable between published heartbeats.
        if starting
            || p.starting
            || p.published.is_none_or(|last| {
                now - last >= Duration::from_secs(barca_service::liveness::HEARTBEAT_SECONDS as u64)
            })
        {
            p.view.heartbeat_at_ms = Some(chrono::Utc::now().timestamp_millis());
            p.published = Some(now);
        }
        p.starting = starting;
        p.exited = false;
    }

    pub fn exited(&self, reason: String, unexpected: bool) {
        let mut p = self.0.lock().unwrap();
        p.operation = None;
        p.heartbeat = None;
        p.exited = unexpected;
        p.view.last_exit_reason = Some(reason);
    }

    pub fn restarted(&self) {
        self.0.lock().unwrap().view.restart_count += 1;
    }

    pub async fn waiting<T>(&self, name: &'static str, work: impl Future<Output = T>) -> T {
        let previous = self
            .0
            .lock()
            .unwrap()
            .operation
            .replace((name, chrono::Utc::now().timestamp_millis()));
        let value = work.await;
        self.0.lock().unwrap().operation = previous;
        value
    }

    pub async fn bounded<T>(
        &self,
        name: &'static str,
        work: impl Future<Output = T>,
    ) -> Result<T, barca_service::BarcaError> {
        // Startup may initialize an older ledger. Live lock acquisition has a shorter budget.
        let budget = if name == "reload_jobs" {
            Duration::from_secs(30)
        } else if name == "init_db" {
            Duration::from_secs(5)
        } else {
            METADATA_WAIT
        };
        self.waiting(name, tokio::time::timeout(budget, work))
            .await
            .map_err(|_| {
                let reason = format!("scheduler timed out waiting for {name}");
                barca_service::errln!("[barca] {reason}");
                self.0.lock().unwrap().timeout = Some(reason.clone());
                std::io::Error::new(std::io::ErrorKind::TimedOut, reason).into()
            })
    }

    /// Keep startup's original epoch and completed work while a metadata lock is busy.
    pub async fn startup<T, F>(
        &self,
        name: &'static str,
        mut work: impl FnMut() -> F,
    ) -> Result<T, barca_service::BarcaError>
    where
        F: Future<Output = Result<T, barca_service::BarcaError>>,
    {
        let mut delay = Duration::from_millis(100);
        loop {
            self.beat();
            match self.bounded(name, work()).await.and_then(|result| result) {
                Err(error) if transient(&error) => {
                    self.0.lock().unwrap().timeout =
                        Some(format!("scheduler retrying {name}: {error}"));
                    self.waiting(name, tokio::time::sleep(delay)).await;
                    delay = (delay * 2).min(Duration::from_secs(1));
                }
                result => {
                    self.recovered();
                    return result;
                }
            }
        }
    }

    pub fn recovered(&self) {
        self.0.lock().unwrap().timeout = None;
    }

    pub fn admission_failure(&self, job: &str, reason: String) {
        self.0
            .lock()
            .unwrap()
            .admission_errors
            .insert(job.to_owned(), reason);
    }
    pub fn admission_recovered(&self, job: &str) {
        self.0.lock().unwrap().admission_errors.remove(job);
    }
    pub fn due(&self, epoch: i64) {
        self.0.lock().unwrap().view.last_due_slot = Some(epoch);
    }
    pub fn enqueued(&self) {
        self.0.lock().unwrap().view.last_enqueue = Some(chrono::Utc::now().timestamp());
    }
    pub fn snapshot(&self) -> Option<SchedulerLiveness> {
        let p = self.0.lock().unwrap();
        if p.heartbeat.is_none() && p.view.last_exit_reason.is_none() {
            return None;
        }
        let mut view = p.view.clone();
        let age = p.heartbeat.map(|at| at.elapsed());
        let grace = if p.starting { STALE * 2 } else { STALE };
        if let Some((name, at)) = p.operation {
            view.operation = Some(name.into());
            view.operation_started_at_ms = Some(at);
        }
        view.reason = if p.exited {
            view.last_exit_reason.clone()
        } else if age.is_some_and(|d| d > grace) {
            Some("scheduler heartbeat is stale".into())
        } else {
            p.timeout
                .clone()
                .or_else(|| p.admission_errors.values().next().cloned())
        };
        Some(view)
    }
}

pub(super) fn transient(error: &barca_service::BarcaError) -> bool {
    match error {
        barca_service::BarcaError::Io(e) => e.kind() == std::io::ErrorKind::TimedOut,
        // The metadata file-lock API reports its finite lock wait as a database error.
        barca_service::BarcaError::Db(reason) => {
            reason.contains("timed out after") && reason.contains(".lock")
        }
        _ => false,
    }
}
