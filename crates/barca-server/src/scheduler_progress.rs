//! Process-local scheduler progress. No metadata or async locks on the health read path.
use barca_service::served::SchedulerLiveness;
use std::{sync::Mutex, time::Duration};

use tokio::time::Instant;

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
            crate::scheduler::METADATA_WAIT
        };
        self.waiting(name, tokio::time::timeout(budget, work))
            .await
            .map_err(|_| {
                let reason = format!("scheduler timed out waiting for {name}");
                barca_service::errln!("[barca] {reason}");
                self.0.lock().unwrap().timeout = Some(reason.clone());
                barca_service::BarcaError::Db(reason)
            })
    }

    pub fn recovered(&self) {
        self.0.lock().unwrap().timeout = None;
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
            p.timeout.clone()
        };
        Some(view)
    }
}
