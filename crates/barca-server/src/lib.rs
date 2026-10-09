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

/// Start the server on the caller's runtime: bind and serve until SIGINT (Ctrl-C) or
/// SIGTERM. On shutdown, in-flight runs are cancelled (workers terminated, runs marked
/// cancelled) before returning.
pub async fn serve(config: ServeConfig) -> Result<(), ServeError> {
    // Before anything starts: an unknown zone must not become local time (#289).
    check_timezone(&config.timezone).map_err(ServeError::Timezone)?;
    // Before the address is announced, so a signal sent as soon as the server is seen to be
    // up is already handled.
    let mut stop = StopSignals::install()?;
    let stopping = barca_core::CancellationToken::new();
    tokio::spawn({
        let stopping = stopping.clone();
        async move {
            let name = stop.recv().await;
            barca_core::errln!("[barca] {name} received: stopping runs and shutting down");
            stopping.cancel();
        }
    });
    let addr = std::net::SocketAddr::new(config.host, config.port);
    let n_files = config.files.len();
    let watch = config.watch;
    // A read-only server never runs anything, so it never schedules either.
    let schedule = config.schedule && !config.read_only;
    let read_only = config.read_only;

    let state = AppState::new(config);
    state.loaded_dag().await?;

    // Evict completed/failed runs older than 1 hour, checking every 5 minutes.
    tokio::spawn(handlers::evict_finished_runs(
        state.clone(),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(3600),
    ));

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

    // On a stop signal the server stops accepting connections and waits for the open ones,
    // while `stop_runs` stops the runs and then ends what would keep a connection open.
    use futures::FutureExt;
    let server =
        axum::serve(listener, app).with_graceful_shutdown(stopping.clone().cancelled_owned());
    let runs_stopped = stop_runs(state.clone(), stopping).boxed().shared();
    tokio::select! {
        // Every connection closed (often there is none to wait for).
        served = server => served?,
        // Connections still open once the runs are over and the grace period has passed
        // are dropped with the server.
        _ = async {
            runs_stopped.clone().await;
            tokio::time::sleep(CONNECTION_GRACE).await;
        } => {}
    }
    runs_stopped.await;
    Ok(())
}

/// How long shutdown waits for cancelled runs to stop their workers and record themselves.
const RUN_STOP_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long open connections get to finish once the runs are over.
const CONNECTION_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// What shutdown does besides closing the listener. Once `stopping` is cancelled: cancel
/// every run (their tokens are children of the shutdown token), wait, for at most
/// [`RUN_STOP_LIMIT`], until the run tasks have terminated their workers, recorded the runs
/// as `cancelled` and released their slots, then end the live event streams. The caller
/// gives the connections [`CONNECTION_GRACE`] after that.
///
/// The event streams have to be ended here. A `GET /events/{run_id}` response stays open
/// after its run finished (a browser tab on a run page holds one), and the server waits for
/// open connections: one such stream used to keep a stopped server alive for ever.
async fn stop_runs(state: AppState, stopping: barca_core::CancellationToken) {
    stopping.cancelled().await;
    state.shutdown.cancel();
    let _ = tokio::time::timeout(
        RUN_STOP_LIMIT,
        state.run_slots.acquire_many(state.run_slot_count as u32),
    )
    .await;
    // Dropping the channels closes the streams once each has delivered what its run
    // emitted, the final `run_finished` included.
    state.events.clear();
}

/// The signals that stop the server: SIGINT (Ctrl-C) and SIGTERM (`docker stop`, `kill`,
/// systemd). Both do the same.
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
