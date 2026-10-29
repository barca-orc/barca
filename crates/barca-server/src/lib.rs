//! `barca-server` — the long-running HTTP API for the barca orchestrator.
//!
//! Layering: this crate depends on `barca-core` (pure planning/execution logic)
//! and exposes it over an axum JSON API. The CLI's `barca serve` subcommand is a
//! thin caller of [`serve`]. A future UI is a separate package that consumes this
//! same HTTP API as its contract.
//!
//! Runs are async: `POST /runs` answers with the run's database id at once, the run joins the
//! durable run queue (`queue`) and the work happens in a background task; clients stream `GET /runs/{run_id}/events`, read
//! `GET /runs/{run_id}`, and can cancel with `DELETE /runs/{run_id}`. Core commands are
//! async-native, so handlers `.await` them directly on the server's runtime — no
//! `spawn_blocking`, and every run future is genuinely cancellable.

// Print with `barca_core::outln!` / `errln!`, which cannot panic on a closed pipe (#286).
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod auth;
mod error;
mod handlers;
mod inspect;
mod ledger;
mod pause;
mod queue;
mod read_cache;
mod reset;
mod routes;
mod running;
mod runs;
mod scheduler;
mod state;
mod telemetry;
mod ui;
mod watch;

#[cfg(test)]
pub(crate) mod test_support {
    use std::time::Duration;

    /// How long a bounded test may take.
    pub const LIMIT: Duration = Duration::from_secs(30);

    /// Run the async test `body` on a runtime of its own thread, and fail when it has not
    /// finished (runtime shutdown included) within [`LIMIT`]. A deadlock then fails the test
    /// instead of hanging CI (#637): `tokio::time::timeout` cannot interrupt a thread blocked
    /// on a lock.
    pub fn bounded<F>(body: impl FnOnce() -> F + Send + 'static)
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let (done, finished) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a test runtime")
                .block_on(body());
            let _ = done.send(());
        });
        match finished.recv_timeout(LIMIT) {
            Ok(()) => thread.join().expect("the test thread finished"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if let Err(panic) = thread.join() {
                    std::panic::resume_unwind(panic);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                "the test did not finish within {LIMIT:?}: a deadlock or a wait without a \
                 deadline"
            ),
        }
    }
}

pub use handlers::node_states;
pub use running::{NodeRunning, PartitionProgress, RunningStep};
pub use state::{NodeState, ServeConfig};

use state::AppState;

/// Errors raised while starting or running the server (not per-request errors).
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// `ServeConfig::timezone` is not a zone the scheduler knows.
    #[error("{0}")]
    Timezone(String),
    #[error("source loading failed: {0}")]
    Load(#[from] barca_core::BarcaError),
    /// A writer server could not set up its metadata DB; nothing was bound. `barca serve`
    /// exits 3 with this error's envelope.
    #[error("{0}")]
    Metadata(barca_core::BarcaError),
}

/// Check a `--timezone` value: `local`, `utc`, or an IANA name such as `America/New_York`.
/// The error names the value and shows valid ones.
pub fn check_timezone(value: &str) -> Result<(), String> {
    scheduler::Zone::parse(value).map(|_| ())
}

/// Build the API router over a fresh [`AppState`] for the given config.
///
/// Exposed for tests and for embedding the API into another server; does not
/// start a watcher or bind a socket.
pub fn app(config: ServeConfig) -> axum::Router {
    routes::router(AppState::new(config))
}

/// [`app`] on a server that is draining (as after SIGTERM): runs are queued for the next start,
/// nothing starts, and `/health` says `draining`. For conformance tests; not part of the API.
#[doc(hidden)]
pub fn draining_app(config: ServeConfig) -> axum::Router {
    let state = AppState::new(config);
    state.draining.cancel();
    routes::router(state)
}

/// [`app`], plus a function that starts the cron scheduler on the same state. For tests that
/// must send requests before the first tick; not part of the API.
#[doc(hidden)]
pub fn app_with_scheduler(
    config: ServeConfig,
) -> (axum::Router, impl FnOnce() -> tokio::task::JoinHandle<()>) {
    let state = AppState::new(config);
    let router = routes::router(state.clone());
    (router, move || {
        tokio::spawn(scheduler::run_scheduler(state))
    })
}

/// How [`serve`] ended. `queued` is how many runs wait in the run queue for the next start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Every run in flight finished during the drain (or there was none). `barca serve` exits 0.
    Drained { queued: usize },
    /// The drain timeout passed while runs were still in flight: that many were stopped and put
    /// back at the head of the queue, to start again from the beginning after the restart.
    /// Nothing accepted is lost; `barca serve` exits 0.
    Requeued { requeued: usize, queued: usize },
    /// A second SIGTERM/SIGINT came while runs were still in flight: that many runs were
    /// cancelled with reason `server shutdown`. `barca serve` exits 1.
    Forced { cancelled: usize, queued: usize },
}

/// Start the server on the caller's runtime: bind and serve until SIGTERM or SIGINT, then
/// drain (#190).
///
/// At startup the runs the last server left queued are resumed, in order, under their ids.
///
/// The first signal starts the drain: the queue stops starting runs (new ones are still
/// accepted and recorded, for the next server), the scheduler stops, `/health` reports
/// `draining`, and the runs in flight finish, uploads included, for at most `[server]
/// drain_timeout` seconds (`--drain-timeout`, 30 by default). When the deadline passes, the
/// runs still going are stopped and put back in the queue ([`Stopped::Requeued`]); when a
/// second signal comes first, they are cancelled (workers terminated, runs recorded
/// `cancelled` with reason `server shutdown`) and [`Stopped::Forced`] is returned.
pub async fn serve(config: ServeConfig) -> Result<Stopped, ServeError> {
    // Before anything starts: an unknown zone must not become local time (#289).
    check_timezone(&config.timezone).map_err(ServeError::Timezone)?;
    // Before the address is announced, so a signal sent as soon as the server is seen to be
    // up is already handled (a signal that arrives before it is awaited is kept).
    let mut signals = StopSignals::install()?;
    let addr = std::net::SocketAddr::new(config.host, config.port);
    let n_files = config.files.len();
    let watch = config.watch;
    // A read-only server never runs anything, so it never schedules either.
    let schedule = config.schedule && !config.read_only;
    let read_only = config.read_only;

    // `barca state restore` refuses to replace the database under a running server.
    let _serve_lock =
        barca_core::backup::ServeLock::acquire(&config.resolved, &format!("http://{addr}"));
    let backups = (!read_only && config.resolved.backup_uri.is_some())
        .then(|| (config.python.clone(), config.resolved.clone()));

    let state = AppState::new(config);
    // Before the sources load and before anything binds: a writer whose metadata DB cannot be
    // set up stops here (exit 3), instead of serving with its scheduler off (#513).
    metadata_startup(&state).await?;
    state.loaded_dag().await?;
    // The runs the last server accepted and did not start, in their order. Before the
    // scheduler's catch-up, which skips a job whose node has one.
    if !read_only {
        queue::resume_at_startup(&state)
            .await
            .map_err(ServeError::Metadata)?;
        queue::dispatch(&state);
    }

    // With a store, back the metadata DB up every minute it changed (RFC-0009). Runs back up
    // at their end too.
    // The last one is taken after the runs have stopped and recorded themselves.
    let backup_stop = barca_core::CancellationToken::new();
    let mut stop = StopBudget::of(
        state.config.resolved.drain_timeout_secs,
        backups.is_some(),
        false,
    );
    let backup_task = backups.map(|(python, resolved)| {
        tokio::spawn(backup_every_minute(
            python,
            resolved,
            backup_stop.clone(),
            stop.backup,
        ))
    });

    // Evict completed/failed runs older than 1 hour, checking every 5 minutes.
    tokio::spawn(runs::evict_finished_runs(
        state.clone(),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(3600),
    ));

    // The cache size limit (RFC-0012): every run checks it when it ends; this checks it once a
    // minute as well, so a server whose runs are rare still stays under its limit. A read-only
    // server deletes nothing.
    if !read_only {
        tokio::spawn(cache_checkpoint(state.clone()));
    }

    if read_only {
        barca_core::errln!(
            "[barca] read-only: runs are refused and the metadata DB is only read from snapshots"
        );
    }

    // Fire `Schedule(...)` assets on their cron ticks (local time). On by default.
    if schedule {
        tokio::spawn(scheduler::run_scheduler(state.clone()));
    }

    // Dev-mode hot reload. The watcher must be held for the server's lifetime.
    let _watcher = if watch {
        match watch::spawn(state.clone()) {
            Ok(w) => Some(w),
            Err(e) => {
                barca_core::errln!("[barca] watch disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    // `GET /state` computed ahead of the web UI's polls (`read_cache`).
    tokio::spawn(read_cache::keep_warm(state.clone()));

    let app = routes::router(state.clone());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Cancelled once the runs are over: the listener closes and the connections end.
    let stopping = barca_core::CancellationToken::new();
    let mut telemetry = telemetry::ServerTelemetry::start(state.clone(), stopping.clone());
    barca_core::errln!(
        "[barca] serving on http://{addr}  ({n_files} file{}{})",
        if n_files == 1 { "" } else { "s" },
        if watch { " · watch" } else { "" },
    );
    if telemetry.is_some() {
        stop.telemetry = Some(telemetry::FINISH_LIMIT);
    }
    if let Some(supervisor) = supervisor() {
        barca_core::errln!("{}", stop.grace_line(supervisor));
    }
    if !addr.ip().is_loopback() {
        barca_core::errln!(
            "[barca] warning: listening on {} with no authentication — anyone who can reach \
             port {} can trigger runs",
            addr.ip(),
            addr.port()
        );
    }

    // The listener keeps serving through the drain (status polls, event streams, /health,
    // the 503s). Once the runs are over the server stops accepting connections and waits
    // for the open ones, for at most [`CONNECTION_GRACE`].
    use futures::FutureExt;
    let server =
        axum::serve(listener, app).with_graceful_shutdown(stopping.clone().cancelled_owned());
    let lifecycle = {
        let state = state.clone();
        let stopping = stopping.clone();
        async move {
            let first = signals.recv().await;
            let stopped = drain_then_stop(&state, first, signals.recv(), stop).await;
            stopping.cancel();
            // Dropping the channels closes the event streams once each has delivered what
            // its run emitted, the final `run_finished` included. A `GET /events/{run_id}`
            // response stays open after its run finished (a browser tab on a run page holds
            // one), and the server waits for open connections: one such stream used to
            // keep a stopped server alive for ever.
            state.events.clear();
            stopped
        }
    }
    .boxed()
    .shared();
    tokio::select! {
        // Every connection closed (often there is none to wait for).
        served = server => served?,
        // Connections still open once the runs are over and the grace period has passed
        // are dropped with the server.
        _ = async {
            lifecycle.clone().await;
            tokio::time::sleep(stop.connections).await;
        } => {}
    }
    let stopped = lifecycle.await;
    backup_stop.cancel();
    if let Some(task) = backup_task {
        let _ = tokio::time::timeout(stop.backup + BACKUP_SLACK, task).await;
    }
    if let Some(telemetry) = telemetry.as_mut() {
        telemetry.finish().await;
    }
    Ok(stopped)
}

/// Set up (writer, when the file exists) or check (read-only) the metadata DB, and print the startup line naming it
/// and its format. A writer that cannot use its database fails with [`ServeError::Metadata`];
/// a read-only server keeps serving and reports the error on `/health` (`metadata_error`).
async fn metadata_startup(state: &AppState) -> Result<(), ServeError> {
    use barca_core::{BarcaError, db};
    let cfg = &state.config.resolved;
    let path = cfg.db_path.clone();
    let format = if state.config.read_only {
        db::probe_metadata(&path).await
    } else if !std::path::Path::new(&path).exists() {
        // Nothing to check: the first run (or the scheduler) creates it, as before.
        Ok(None)
    } else {
        let set_up = match db::ensure_env_dirs(&cfg.env) {
            Ok(_) => db::init_db_as(&path, &path).await.map(Some),
            Err(e) => Err(e),
        };
        // A lock or I/O failure names the database too.
        set_up.map_err(|e| match e {
            BarcaError::Db(msg) => {
                BarcaError::Db(format!("cannot set up the metadata DB {path}: {msg}"))
            }
            BarcaError::Io(io) => {
                BarcaError::Db(format!("cannot set up the metadata DB {path}: {io}"))
            }
            e => e,
        })
    };
    barca_core::errln!(
        "{}",
        cfg.serve_startup_line(match &format {
            Ok(Some(n)) => Some(Ok(*n)),
            Ok(None) => None,
            Err(_) => Some(Err(())),
        })
    );
    stray_root_db_warning(cfg);
    match format {
        Ok(_) => Ok(()),
        Err(e) if !state.config.read_only => Err(ServeError::Metadata(e)),
        Err(e) => {
            let fix = match &e {
                BarcaError::MetadataUnusable { path, .. } => {
                    format!(" {}", barca_core::envelope::metadata_remediation(path))
                }
                _ => String::new(),
            };
            barca_core::errln!(
                "[barca] warning: {e}; serving read-only without its history (GET /health \
                 metadata_error).{fix}"
            );
            *state.metadata_error.write().unwrap() = Some(e.to_string());
            Ok(())
        }
    }
}

/// Warn when this server uses a named environment's database and the default environment's
/// `.barca/metadata.db` is there but empty or not a database: a stray file, often left by a
/// container that started once without `--env`, that is easy to mistake for this one.
fn stray_root_db_warning(cfg: &barca_core::config::ResolvedConfig) {
    let root = barca_core::db::env_local_paths(barca_core::config::DEFAULT_ENV).db_path;
    if cfg.db_path == root {
        return;
    }
    if let Some(why) = barca_core::db::unusable_file(&root) {
        barca_core::errln!(
            "[barca] warning: {root} is not this server's metadata DB (env {} uses {}) and \
             cannot be used ({why}); remove it if nothing else needs it",
            cfg.env,
            cfg.db_path
        );
    }
}

/// Once a minute, evict if `.barca/` is over its size limit ([`barca_core::gc::checkpoint`]).
/// What runs in flight in this process hold is left alone.
async fn cache_checkpoint(state: AppState) {
    let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = every.tick() => {}
            _ = state.shutdown.cancelled() => return,
        }
        barca_core::gc::checkpoint(&state.config.resolved).await;
    }
}

/// How often `barca serve` backs its metadata DB up, when it changed (RFC-0009).
const BACKUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// How long one of those backups may take, upload included.
const BACKUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Back the database up every [`BACKUP_INTERVAL`] until `stopping`, then once more.
/// A copy identical to the newest one is not taken. Three failures in a row are reported once,
/// until a backup succeeds again.
async fn backup_every_minute(
    python: std::path::PathBuf,
    cfg: barca_core::config::ResolvedConfig,
    stopping: barca_core::CancellationToken,
    last_budget: std::time::Duration,
) {
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + BACKUP_INTERVAL,
        BACKUP_INTERVAL,
    );
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut failures = 0u32;
    loop {
        let last = tokio::select! {
            _ = tick.tick() => false,
            _ = stopping.cancelled() => true,
        };
        let budget = if last { last_budget } else { BACKUP_BUDGET };
        let until = barca_core::helper_proc::Until {
            cancel: None,
            deadline: Some(std::time::Instant::now() + budget),
        };
        let failed = match barca_core::backup::take(&python, &cfg, None, until).await {
            Ok(taken) => match taken.upload {
                Some(Err(why)) => Some(why),
                Some(Ok(at)) if taken.name.is_some() => {
                    barca_core::errln!("[barca] backed up the metadata DB to {at}");
                    None
                }
                _ => None,
            },
            Err(e) => Some(e.to_string()),
        };
        match failed {
            None => failures = 0,
            Some(why) => {
                failures += 1;
                if failures == 3 || last {
                    barca_core::errln!(
                        "[barca] warning: the metadata backup failed{} ({why}); the next one \
                         tries again",
                        if failures > 1 {
                            format!(" {failures} times in a row")
                        } else {
                            String::new()
                        }
                    );
                }
            }
        }
        if last {
            return;
        }
    }
}

/// How long the backup taken at shutdown is waited for beyond its own budget.
const BACKUP_SLACK: std::time::Duration = std::time::Duration::from_secs(1);

/// What a stopping server may take after its drain: the time it gives the runs it stops to
/// terminate their workers and record themselves, open connections to finish, the last
/// metadata backup (with a store) and the telemetry flush (with telemetry). The supervisor's
/// grace period must cover the drain timeout plus all of it (#628).
///
/// With a drain timeout of 10 s or more: 10 s for the runs, 2 s for connections, 30 s (+1) for
/// the backup. Below that each part shrinks with it, so a server asked to stop quickly fits a
/// short grace period: the runs get half the drain timeout (at least 2 s), connections 1 s,
/// the backup the drain timeout (at least 5 s). Stopping runs takes well under a second unless
/// a step ignores SIGTERM; a run that has not recorded itself when its time is up is found
/// `draining` by the next server and put back in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopBudget {
    pub drain: std::time::Duration,
    pub runs: std::time::Duration,
    pub connections: std::time::Duration,
    /// The last backup's budget; it counts only with a store.
    pub backup: std::time::Duration,
    pub with_backup: bool,
    /// The telemetry flush (3 s), when telemetry is on.
    pub telemetry: Option<std::time::Duration>,
}

impl StopBudget {
    pub fn of(drain_secs: u64, with_backup: bool, telemetry: bool) -> Self {
        use std::time::Duration;
        let drain = Duration::from_secs(drain_secs);
        let runs = (drain / 2).clamp(Duration::from_secs(2), Duration::from_secs(10));
        let connections = if drain_secs >= 10 {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(1)
        };
        let backup = drain.clamp(Duration::from_secs(5), Duration::from_secs(30));
        Self {
            drain,
            runs,
            connections,
            backup,
            with_backup,
            telemetry: telemetry.then_some(telemetry::FINISH_LIMIT),
        }
    }

    /// Everything after the drain, in whole seconds rounded up.
    pub fn after_drain_secs(&self) -> u64 {
        let mut total = self.runs + self.connections;
        if self.with_backup {
            total += self.backup + BACKUP_SLACK;
        }
        if let Some(t) = self.telemetry {
            total += t;
        }
        total.as_millis().div_ceil(1000) as u64
    }

    /// The least grace period a supervisor must give between SIGTERM and SIGKILL, in seconds.
    pub fn grace_secs(&self) -> u64 {
        self.drain.as_secs() + self.after_drain_secs()
    }

    /// The startup line under a supervisor.
    pub fn grace_line(&self, supervisor: Supervisor) -> String {
        let mut parts = vec!["stopping runs and closing connections"];
        if self.with_backup {
            parts.push("the last metadata backup");
        }
        if self.telemetry.is_some() {
            parts.push("the telemetry flush");
        }
        format!(
            "[barca] stop grace: {} must allow at least {}s between SIGTERM and SIGKILL (drain \
             timeout {}s + {}s for {}); {}",
            supervisor.name(),
            self.grace_secs(),
            self.drain.as_secs(),
            self.after_drain_secs(),
            parts.join(", "),
            supervisor.setting(),
        )
    }
}

/// What stops this server, when it runs under something that does (#628).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervisor {
    /// Process 1: a container's entrypoint.
    Container,
    Kubernetes,
    Systemd,
}

impl Supervisor {
    fn name(self) -> &'static str {
        match self {
            Supervisor::Container => "the container runtime",
            Supervisor::Kubernetes => "Kubernetes",
            Supervisor::Systemd => "systemd",
        }
    }

    fn setting(self) -> &'static str {
        match self {
            Supervisor::Container => {
                "set docker stop -t or stop_grace_period (Docker's default is 10s)"
            }
            Supervisor::Kubernetes => "set terminationGracePeriodSeconds (default 30)",
            Supervisor::Systemd => "set TimeoutStopSec= (default 90s)",
        }
    }
}

/// The supervisor this process runs under: Kubernetes (`KUBERNETES_SERVICE_HOST`), systemd
/// (`INVOCATION_ID`, set for every unit it starts), or a container when this is process 1.
fn supervisor() -> Option<Supervisor> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("KUBERNETES_SERVICE_HOST") {
        Some(Supervisor::Kubernetes)
    } else if set("INVOCATION_ID") {
        Some(Supervisor::Systemd)
    } else if std::process::id() == 1 {
        Some(Supervisor::Container)
    } else {
        None
    }
}

/// The reason a run cancelled by a stopping server records, in its logs and on its
/// `barca.run` span.
pub const SHUTDOWN_REASON: &str = "server shutdown";

/// The reason a run the stopping server put back in the queue records on its stopped attempt.
pub const REQUEUE_REASON: &str = "server restart (requeued)";

/// Runs executing now.
fn runs_in_flight(state: &AppState) -> usize {
    state
        .runs
        .iter()
        .filter(|r| r.status == state::RunStatus::Running)
        .count()
}

/// Runs waiting in the queue.
fn runs_queued(state: &AppState) -> usize {
    // Waiting in the queue, or put back in it by this drain.
    state
        .runs
        .iter()
        .filter(|r| r.status == state::RunStatus::Pending)
        .count()
}

fn runs(n: usize) -> String {
    format!("{n} run{}", if n == 1 { "" } else { "s" })
}

/// The drain, after the first stop signal (`first`). Stop starting runs and stop the scheduler
/// ([`AppState::draining`]); runs that are accepted meanwhile join the queue. Then wait until
/// every run task has ended, for at most the drain timeout. If the deadline passes first, stop
/// the runs still in flight and put them back in the queue ([`REQUEUE_REASON`]); if `again`
/// (the next stop signal) comes first, cancel them ([`SHUTDOWN_REASON`]). Either way wait, for
/// at most [`RUN_STOP_LIMIT`], until they have terminated their workers and recorded
/// themselves.
async fn drain_then_stop(
    state: &AppState,
    first: &str,
    again: impl std::future::Future<Output = &'static str>,
    budget: StopBudget,
) -> Stopped {
    let secs = state.config.resolved.drain_timeout_secs;
    state.draining.cancel();
    state.run_tasks.close();
    // Recorded before anything else: if whatever sent the signal kills the server before the
    // drain ends, the next server puts these runs back in the queue instead of recording them
    // interrupted (#628).
    let flying: Vec<String> = state
        .runs
        .iter()
        .filter(|r| r.status == state::RunStatus::Running)
        .map(|r| r.handle.clone())
        .collect();
    if !state.config.read_only
        && let Err(e) =
            barca_core::run_queue::mark_draining(&state.config.resolved.db_path, &flying).await
    {
        barca_core::errln!("[barca] warning: could not mark the runs in flight draining: {e}");
    }
    let in_flight = runs_in_flight(state);
    let waiting = runs_queued(state);
    let kept = if waiting > 0 {
        format!("; {} queued for the next start", runs(waiting))
    } else {
        String::new()
    };
    if in_flight > 0 {
        barca_core::errln!(
            "[barca] {first} received: draining — the queue is paused and new runs are queued \
             for the next start; waiting up to {secs}s for {} in flight (then {} back in the \
             queue; send {first} again to cancel {} now){kept}",
            runs(in_flight),
            if in_flight == 1 { "it goes" } else { "they go" },
            if in_flight == 1 { "it" } else { "them" },
        );
    } else {
        barca_core::errln!("[barca] {first} received: no run in flight{kept}; shutting down");
    }
    enum Stop {
        Requeue,
        Cancel(String),
    }
    let stop = tokio::select! {
        _ = state.run_tasks.wait() => None,
        _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => Some(Stop::Requeue),
        name = again => Some(Stop::Cancel(format!("{name} received again"))),
    };
    let stopped = match stop {
        None => {
            if in_flight > 0 {
                barca_core::errln!("[barca] drained: every run finished; shutting down");
            }
            let queued = runs_queued(state);
            if queued > 0 {
                barca_core::errln!(
                    "[barca] {} wait in the queue for the next start",
                    runs(queued)
                );
            }
            return Stopped::Drained { queued };
        }
        Some(Stop::Requeue) => {
            let n = runs_in_flight(state);
            barca_core::errln!(
                "[barca] the drain timeout ({secs}s) passed: stopping {} and putting {} back in \
                 the queue; {} again from the beginning when the server restarts (steps \
                 that finished are cached)",
                runs(n),
                if n == 1 { "it" } else { "them" },
                if n == 1 { "it starts" } else { "they start" },
            );
            state
                .queue
                .requeue
                .store(true, std::sync::atomic::Ordering::SeqCst);
            state.cancel_reason.set(REQUEUE_REASON);
            n
        }
        Some(Stop::Cancel(why)) => {
            let n = runs_in_flight(state);
            if n > 0 {
                barca_core::errln!("[barca] {why}: cancelling {} ({SHUTDOWN_REASON})", runs(n));
            }
            state.cancel_reason.set(SHUTDOWN_REASON);
            n
        }
    };
    state.shutdown.cancel();
    let _ = tokio::time::timeout(budget.runs, state.run_tasks.wait()).await;
    let queued = runs_queued(state);
    if queued > 0 {
        barca_core::errln!(
            "[barca] {} wait in the queue for the next start",
            runs(queued)
        );
    }
    if state
        .queue
        .requeue
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        Stopped::Requeued {
            requeued: stopped,
            queued,
        }
    } else if stopped > 0 {
        Stopped::Forced {
            cancelled: stopped,
            queued,
        }
    } else {
        Stopped::Drained { queued }
    }
}

/// The signals that stop the server: SIGINT (Ctrl-C) and SIGTERM (`docker stop`, `kill`,
/// systemd). Both do the same: the first starts the drain, the next one ends it.
///
/// A handler is installed for each. That matters for a server that is process 1 of a
/// container: the kernel delivers to process 1 only the signals it has a handler for, so
/// without one SIGTERM was discarded and `docker stop` waited out its timeout and killed
/// the server (#289).
///
/// SIGHUP and SIGQUIT keep their default action: they end the process at once (as process 1
/// they are discarded), and `nohup barca serve` keeps ignoring SIGHUP.
struct StopSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    /// Wait for the first of them and return its name.
    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "SIGINT",
            _ = self.terminate.recv() => "SIGTERM",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stop_budget_shrinks_with_a_short_drain_timeout() {
        // The default drain: 30 s + 10 s for runs + 2 s for connections.
        let default = StopBudget::of(30, false, false);
        assert_eq!(default.after_drain_secs(), 12);
        assert_eq!(default.grace_secs(), 42);
        // With a store, the last backup gets up to 30 s (+1).
        assert_eq!(StopBudget::of(30, true, false).grace_secs(), 73);
        // drain_timeout = 5 fits Docker's default 10 s: 5 + 2.5 + 1, rounded up.
        let short = StopBudget::of(5, false, false);
        assert_eq!(short.after_drain_secs(), 4);
        assert_eq!(short.grace_secs(), 9);
        assert_eq!(StopBudget::of(5, true, false).grace_secs(), 15);
        // 0 stops at once: 2 s for runs and 1 for connections.
        assert_eq!(StopBudget::of(0, false, false).grace_secs(), 3);
        // Telemetry adds its 3 s flush.
        assert_eq!(StopBudget::of(0, false, true).grace_secs(), 6);
        // Long drains keep the full budgets.
        assert_eq!(StopBudget::of(300, false, false).after_drain_secs(), 12);
    }

    #[test]
    fn the_grace_line_names_the_supervisor_setting() {
        let line = StopBudget::of(5, false, false).grace_line(Supervisor::Container);
        assert_eq!(
            line,
            "[barca] stop grace: the container runtime must allow at least 9s between SIGTERM \
             and SIGKILL (drain timeout 5s + 4s for stopping runs and closing connections); set \
             docker stop -t or stop_grace_period (Docker's default is 10s)"
        );
        let line = StopBudget::of(30, true, true).grace_line(Supervisor::Systemd);
        assert!(line.contains("at least 76s"), "{line}");
        assert!(line.contains("TimeoutStopSec="), "{line}");
    }
}
