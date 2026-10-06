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

mod error;
mod handlers;
mod routes;
mod scheduler;
mod state;
mod ui;
mod watch;

pub use handlers::node_states;
pub use scheduler::{ScheduleInfo, describe_schedule};
pub use state::{NodeState, ServeConfig};

use state::AppState;

/// Errors raised while starting or running the server (not per-request errors).
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Build the API router over a fresh [`AppState`] for the given config.
///
/// Exposed for tests and for embedding the API into another server; does not
/// start a watcher or bind a socket.
pub fn app(config: ServeConfig) -> axum::Router {
    routes::router(AppState::new(config))
}

/// Start the server on the caller's runtime: bind and serve until Ctrl-C. On
/// shutdown, in-flight runs are cancelled (workers terminated, runs marked
/// cancelled) before returning.
pub async fn serve(config: ServeConfig) -> Result<(), ServeError> {
    let addr = std::net::SocketAddr::new(config.host, config.port);
    let n_files = config.files.len();
    let watch = config.watch;
    // A read-only server never runs anything, so it never schedules either.
    let schedule = config.schedule && !config.read_only;
    let read_only = config.read_only;

    let state = AppState::new(config);

    // Evict completed/failed runs older than 1 hour, checking every 5 minutes.
    tokio::spawn(handlers::evict_finished_runs(
        state.clone(),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(3600),
    ));

    if read_only {
        eprintln!(
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
                eprintln!("[barca] watch disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    let app = routes::router(state.clone());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "[barca] serving on http://{addr}  ({n_files} file{}{})",
        if n_files == 1 { "" } else { "s" },
        if watch { " · watch" } else { "" },
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Stop in-flight runs: cancel every run token (they are children of the
    // shutdown token), then wait — bounded — for the run tasks to terminate
    // their workers and release their slots.
    state.shutdown.cancel();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        state.run_slots.acquire_many(state.run_slot_count as u32),
    )
    .await;
    Ok(())
}

/// Resolve on Ctrl-C for graceful shutdown.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
