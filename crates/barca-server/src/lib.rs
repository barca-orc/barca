//! `barca-server` — the long-running HTTP API for the barca orchestrator.
//!
//! Layering: this crate depends on `barca-core` (pure planning/execution logic)
//! and exposes it over an axum JSON API. The CLI's `barca serve` subcommand is a
//! thin caller of [`serve`]. A future UI is a separate package that consumes this
//! same HTTP API as its contract.
//!
//! Runs are async: `POST /run` returns a handle immediately and the work happens
//! in a background task; clients poll `GET /status/{run_id}` and can cancel via
//! `DELETE /run/{run_id}`. Core commands are async-native, so handlers `.await`
//! them directly on the server's runtime — no `spawn_blocking`, and every run
//! future is genuinely cancellable.

// Print with `barca_core::outln!` / `errln!`, which cannot panic on a closed pipe (#286).
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

mod error;
mod handlers;
mod routes;
mod runs;
mod scheduler;
mod state;
mod telemetry;
mod ui;
mod watch;

pub use handlers::node_states;
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

/// [`app`] on a server that is draining (as after SIGTERM): runs are refused with `503`
/// and `/health` says `draining`. For conformance tests; not part of the API.
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

/// How [`serve`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Every run in flight finished during the drain (or there was none).
    Drained,
    /// The drain timeout passed, or a second SIGTERM/SIGINT came, while runs were still in
    /// flight: that many runs were cancelled with reason `server shutdown`. `barca serve`
    /// exits 1.
    Forced { cancelled: usize },
}

/// Start the server on the caller's runtime: bind and serve until SIGTERM or SIGINT, then
/// drain (#190).
///
/// The first signal starts the drain: new runs are refused with `503` and `Retry-After`, the
/// scheduler stops, `/health` reports `draining`, and the runs in flight finish, uploads
/// included, for at most `[server] drain_timeout` seconds (`--drain-timeout`, 30 by default).
/// When the deadline passes or a second signal comes first, the runs still going are
/// cancelled (workers terminated, runs recorded `cancelled` with reason `server shutdown`)
/// and [`Stopped::Forced`] is returned.
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
    state.loaded_dag().await?;

    // With a store, back the metadata DB up every minute it changed (RFC-0009). Runs back up
    // at their end too.
    // The last one is taken after the runs have stopped and recorded themselves.
    let backup_stop = barca_core::CancellationToken::new();
    let backup_task = backups.map(|(python, resolved)| {
        tokio::spawn(backup_every_minute(python, resolved, backup_stop.clone()))
    });

    // Evict completed/failed runs older than 1 hour, checking every 5 minutes.
    tokio::spawn(handlers::evict_finished_runs(
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
            let stopped = drain_then_stop(&state, first, signals.recv()).await;
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
            tokio::time::sleep(CONNECTION_GRACE).await;
        } => {}
    }
    let stopped = lifecycle.await;
    backup_stop.cancel();
    if let Some(task) = backup_task {
        let _ = tokio::time::timeout(BACKUP_BUDGET + CONNECTION_GRACE, task).await;
    }
    if let Some(telemetry) = telemetry.as_mut() {
        telemetry.finish().await;
    }
    Ok(stopped)
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
        let until = barca_core::helper_proc::Until {
            cancel: None,
            deadline: Some(std::time::Instant::now() + BACKUP_BUDGET),
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

/// How long a forced stop waits for cancelled runs to stop their workers and record themselves.
const RUN_STOP_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long open connections get to finish once the runs are over.
const CONNECTION_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// The reason a run cancelled by a stopping server records, in its logs and on its
/// `barca.run` span.
pub const SHUTDOWN_REASON: &str = "server shutdown";

/// Runs that have not finished: queued or executing.
fn runs_in_flight(state: &AppState) -> usize {
    state
        .runs
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                state::RunStatus::Pending | state::RunStatus::Running
            )
        })
        .count()
}

/// The drain, after the first stop signal (`first`). Stop admitting runs and stop the
/// scheduler ([`AppState::draining`]), then wait until every run task has ended, for at most
/// the drain timeout. If the deadline passes or `again` (the next stop signal) comes first,
/// cancel the runs still in flight with reason [`SHUTDOWN_REASON`] and wait, for at most
/// [`RUN_STOP_LIMIT`], until they have terminated their workers and recorded themselves.
async fn drain_then_stop(
    state: &AppState,
    first: &str,
    again: impl std::future::Future<Output = &'static str>,
) -> Stopped {
    let secs = state.config.resolved.drain_timeout_secs;
    state.draining.cancel();
    state.run_tasks.close();
    let in_flight = runs_in_flight(state);
    if in_flight > 0 {
        barca_core::errln!(
            "[barca] {first} received: draining — new runs are refused; waiting up to {secs}s \
             for {in_flight} run{} in flight (send {first} again to stop now)",
            if in_flight == 1 { "" } else { "s" }
        );
    } else {
        barca_core::errln!("[barca] {first} received: no run in flight; shutting down");
    }
    let forced_by = tokio::select! {
        _ = state.run_tasks.wait() => None,
        _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {
            Some(format!("the drain timeout ({secs}s) passed"))
        }
        name = again => Some(format!("{name} received again")),
    };
    let cancelled = match forced_by {
        None => 0,
        Some(why) => {
            let cancelled = runs_in_flight(state);
            if cancelled > 0 {
                barca_core::errln!(
                    "[barca] {why}: cancelling {cancelled} run{} ({SHUTDOWN_REASON})",
                    if cancelled == 1 { "" } else { "s" }
                );
            }
            state.cancel_reason.set(SHUTDOWN_REASON);
            state.shutdown.cancel();
            let _ = tokio::time::timeout(RUN_STOP_LIMIT, state.run_tasks.wait()).await;
            cancelled
        }
    };
    if cancelled > 0 {
        Stopped::Forced { cancelled }
    } else {
        if in_flight > 0 {
            barca_core::errln!("[barca] drained: every run finished; shutting down");
        }
        Stopped::Drained
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
