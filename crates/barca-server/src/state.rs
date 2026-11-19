//! The server's state: the parts each route family uses, composed into one [`AppState`].
//!
//! Each part is a struct of shared handles that knows nothing of [`AppState`]: [`Reads`] (the
//! config, the loaded graph, what the read routes keep), [`Runs`] (the run registry, the queue,
//! the slots), [`Scheduling`] (the scheduler's published jobs, the pauses), [`Lifecycle`]
//! (start, drain, shutdown), the auth store and the SQL helper. A handler takes the parts it
//! uses (`State<Reads>`, `State<Lifecycle>`: axum's [`FromRef`]), so a route that only reads
//! the graph cannot reach the run registry; one that needs several takes `State<AppState>`.
//! What needs several parts at once is a method here or a function in a module above this one
//! (`docs/engineering/server-layering.md`).

use crate::config::ServeConfig;
use crate::read_cache::Reads;
use crate::replay::{ReplayHub, RunChannel};
use crate::run_model::{RunState, RunStatus};
use crate::views::JobStatus;
use axum::extract::FromRef;
use barca_core::CancellationToken;
use barca_core::db;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::Semaphore;

/// How many runs may execute concurrently by default (one per available core).
fn default_run_concurrency() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// The runs this server holds and what bounds them.
#[derive(Clone)]
pub struct Runs {
    /// Every run accepted and not yet evicted, by run id.
    pub registry: Arc<DashMap<String, RunState>>,
    /// Live event channels per run handle (logs + step/run lifecycle).
    pub(crate) events: Arc<DashMap<String, RunChannel>>,
    pub(crate) replay: ReplayHub,
    /// Bounds how many runs execute concurrently (`--max-runs`). Runs execute Python in
    /// parallel; the shared metadata.db is kept race-free by a process-wide DB
    /// lock in `barca-core`, not by serializing whole runs.
    pub slots: Arc<Semaphore>,
    /// One more slot that only a run someone asked for (not a cron tick) may take while a
    /// scheduled run holds one of `slots` and they are full: an API `get` never waits behind
    /// a schedule.
    pub interactive_slot: Arc<Semaphore>,
    /// The capacity every run shares (`--capacity`, `[server] capacity`, #628): a step of any
    /// run is admitted against it as well as against its run's own. `None`: not set.
    pub capacity: Option<Arc<barca_core::serve::capacity::SharedCapacity>>,
    /// The asset steps the runs are executing (#654): a run waits for a step another run is
    /// computing instead of computing it again.
    pub shared_steps: Arc<barca_core::serve::claims::SharedSteps>,
    /// The runs accepted and not started, in order, and whether the queue is paused.
    pub queue: Arc<crate::queue::Queue>,
    /// Every run task, so a drain can wait for them to end.
    pub tasks: tokio_util::task::TaskTracker,
    /// Set once the writer's settings are published ([`AppState::record_writer`]).
    pub(crate) writer_recorded: Arc<tokio::sync::OnceCell<()>>,
}

impl Runs {
    /// The shared capacity as `barca_core::sizing` takes it: its total and the setting that
    /// gave it.
    pub(crate) fn server_capacity(&self) -> Option<(usize, &'static str)> {
        self.capacity
            .as_ref()
            .map(|c| (c.total() as usize, c.source()))
    }
}

/// What the scheduler publishes and what holds a schedule back.
#[derive(Clone)]
pub struct Scheduling {
    /// Live scheduler view, published by the scheduler and read by `GET /schedule`.
    pub jobs: Arc<RwLock<Vec<JobStatus>>>,
    /// Paused schedules by node id, read from `schedule_state` on first use and kept in step
    /// with it by the pause and resume routes.
    pub pauses: Arc<tokio::sync::OnceCell<RwLock<HashMap<String, db::SchedulePause>>>>,
    /// Set when the scheduler stopped on an error; `GET /health` `scheduler_enabled`. The
    /// read side holds the same flag ([`Reads::note_metadata`]).
    pub stopped: Arc<AtomicBool>,
}

/// When the server started, and how it stops.
#[derive(Clone)]
pub struct Lifecycle {
    /// Cancelled when a drain ends without the runs finishing (its deadline, or a second
    /// SIGTERM/SIGINT); every run token is a child of this, so in-flight runs stop (workers
    /// terminated, runs marked cancelled with [`Self::cancel_reason`]).
    pub shutdown: CancellationToken,
    /// Cancelled by the first SIGTERM/SIGINT: the server drains. New runs are refused with
    /// `503`, the scheduler stops and `/health` says `draining`; runs in flight go on.
    pub draining: CancellationToken,
    /// Why the server cancelled its runs (`server shutdown`), set before [`Self::shutdown`]
    /// is cancelled. Every run executes inside its scope, so the run records the reason.
    pub cancel_reason: barca_core::interrupt::CancelReason,
    /// When this server started (unix seconds), for `GET /project`'s `deployed_at`.
    pub started_at: i64,
    /// The same, RFC 3339 with milliseconds (`GET /health` `started_at`): a client that
    /// reconnects tells a restarted server from the one it was following.
    pub started: String,
}

impl Lifecycle {
    /// Whether the server is draining (or past it): no queued run starts.
    pub fn is_draining(&self) -> bool {
        self.draining.is_cancelled()
    }

    /// `waiting_for` of a run in `status`: `restart` for a queued run while the server drains
    /// (it starts on the next server, #1011); `None` otherwise.
    pub fn waiting_for(&self, status: RunStatus) -> Option<barca_core::run_explain::WaitingFor> {
        (status == RunStatus::Pending && self.is_draining())
            .then_some(barca_core::run_explain::WaitingFor::Restart)
    }

    /// The error a cancelled run reports: `run cancelled`, plus the reason when the server
    /// cancelled it (`run cancelled: server shutdown`).
    pub fn cancelled_message(&self) -> String {
        match self.cancel_reason.get() {
            Some(reason) => format!("run cancelled: {reason}"),
            None => "run cancelled".to_string(),
        }
    }
}

/// Cloneable application state shared across all axum handlers: the parts, each a struct of
/// shared handles.
#[derive(Clone)]
pub struct AppState {
    /// What the server was started with. Every part that needs it holds the same one.
    pub config: Arc<ServeConfig>,
    /// The loaded graph and what the read routes keep between requests
    /// ([`crate::read_cache`]).
    pub reads: Reads,
    pub runs: Runs,
    pub scheduling: Scheduling,
    pub lifecycle: Lifecycle,
    pub(crate) auth: Arc<crate::auth_http::Access>,
    /// The query helper of `POST /sql/query` ([`crate::sql_helper`]): started by the first
    /// query, killed when the server's state is dropped.
    pub sql: Arc<crate::sql_helper::QueryHelper>,
}

impl FromRef<AppState> for Reads {
    fn from_ref(state: &AppState) -> Self {
        state.reads.clone()
    }
}

impl FromRef<AppState> for Runs {
    fn from_ref(state: &AppState) -> Self {
        state.runs.clone()
    }
}

impl FromRef<AppState> for Lifecycle {
    fn from_ref(state: &AppState) -> Self {
        state.lifecycle.clone()
    }
}

impl FromRef<AppState> for Arc<crate::auth_http::Access> {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

impl AppState {
    pub fn new(config: ServeConfig) -> Self {
        // Unit tests never write `.barca` into the crate's source directory.
        #[cfg(test)]
        crate::test_support::leave_source_tree();
        let run_slot_count = config
            .resolved
            .server_max_runs
            .unwrap_or_else(default_run_concurrency)
            .max(1);
        let now = chrono::Utc::now();
        let admission =
            barca_core::serve::capacity::Admission::from(config.resolved.server_admission);
        let capacity = config.resolved.server_capacity.map(|(total, source)| {
            Arc::new(barca_core::serve::capacity::SharedCapacity::with_admission(
                total, source, admission,
            ))
        });
        let auth = Arc::new(crate::auth_http::Access::new(&config.resolved));
        let config = Arc::new(config);
        let scheduler_stopped = Arc::new(AtomicBool::new(false));
        Self {
            auth,
            reads: Reads::new(config.clone(), scheduler_stopped.clone()),
            runs: Runs {
                registry: Arc::new(DashMap::new()),
                events: Arc::new(DashMap::new()),
                replay: Default::default(),
                slots: Arc::new(Semaphore::new(run_slot_count)),
                interactive_slot: Arc::new(Semaphore::new(1)),
                capacity,
                shared_steps: Arc::new(barca_core::serve::claims::SharedSteps::new()),
                queue: Arc::new(crate::queue::Queue::new(run_slot_count)),
                tasks: tokio_util::task::TaskTracker::new(),
                writer_recorded: Default::default(),
            },
            scheduling: Scheduling {
                jobs: Arc::new(RwLock::new(Vec::new())),
                pauses: Arc::new(tokio::sync::OnceCell::new()),
                stopped: scheduler_stopped,
            },
            lifecycle: Lifecycle {
                shutdown: CancellationToken::new(),
                draining: CancellationToken::new(),
                cancel_reason: barca_core::interrupt::CancelReason::new(),
                started_at: now.timestamp(),
                started: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            },
            config,
            sql: Arc::default(),
        }
    }

    /// Whether this server fires `Schedule(...)` nodes: configured to (not `--no-schedule`,
    /// not `--read-only`) and the scheduler has not stopped on an error.
    pub fn scheduler_enabled(&self) -> bool {
        self.config.schedule
            && !self.config.read_only
            && !self.scheduling.stopped.load(Ordering::Relaxed)
    }

    /// The scheduler stopped for `why`, a metadata DB error: `/health` says so.
    pub fn scheduler_failed(&self, why: String) {
        self.scheduling.stopped.store(true, Ordering::Relaxed);
        *self.reads.metadata_error.write().unwrap() = Some(why);
    }

    /// Publish the writer's settings on its first metadata write. An idle server can
    /// inspect a project with no database without creating one.
    pub(crate) async fn record_writer(&self) -> Result<(), barca_core::BarcaError> {
        self.runs
            .writer_recorded
            .get_or_try_init(|| self.publish_writer(vec![]))
            .await?;
        Ok(())
    }

    pub(crate) async fn publish_writer(
        &self,
        schedules: Vec<String>,
    ) -> Result<(), barca_core::BarcaError> {
        let cfg = &self.config.resolved;
        let workers =
            barca_core::sizing(cfg.capacity, cfg.pool_size, self.runs.server_capacity()).pool_size;
        let now = chrono::Utc::now().timestamp();
        barca_core::liveness::record_serving(&cfg.db_path, now, self.runs.queue.max_runs, workers)
            .await?;
        barca_core::liveness::record_scheduler(&cfg.db_path, now, schedules, &self.config.timezone)
            .await
    }
}
