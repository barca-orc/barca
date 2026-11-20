//! What a `barca serve` instance was started with. Built by the CLI and handed to
//! [`crate::serve`]; every other module reads it and none changes it.

use std::net::IpAddr;
use std::path::PathBuf;

/// Configuration for a `barca serve` instance. Built by the CLI and handed to
/// [`crate::serve`].
#[derive(Clone, Debug)]
pub struct ServeConfig {
    /// Python source files that define the DAG this server operates on.
    pub files: Vec<String>,
    /// Bind address (`--host`, default 127.0.0.1 — local only). There is no auth.
    pub host: IpAddr,
    /// Bind port (default 8274).
    pub port: u16,
    /// Dev-mode hot reload: re-parse the DAG when source files change.
    /// Off by default; has no effect on the production serving path.
    pub watch: bool,
    /// Whether the cron scheduler fires `Schedule(...)` assets. On by default;
    /// disabled with `barca serve --no-schedule`.
    pub schedule: bool,
    /// Timezone cron expressions are evaluated in: `local` (default), `utc`, or
    /// an IANA name like `America/New_York`. Set via `--timezone`.
    pub timezone: String,
    /// Python interpreter used for execution (and dynamic-partition resolution).
    pub python: PathBuf,
    /// Resolved barca configuration (environment, DB path, artifact root, state).
    pub resolved: barca_service::config::ResolvedConfig,
    /// Inspect-only mode (`--read-only`): endpoints that run or cancel work
    /// return 403, the scheduler never starts, and every DB read goes through a
    /// private snapshot — the metadata DB is never opened, locked, created, or
    /// written. Safe to point at a project another process is running.
    pub read_only: bool,
}

/// The zone cron expressions are evaluated in (`--timezone`); local time when the value does
/// not parse, which [`crate::check_timezone`] refuses before a server starts.
pub(crate) fn zone_of(config: &ServeConfig) -> barca_service::schedule::Zone {
    barca_service::schedule::Zone::parse(&config.timezone)
        .unwrap_or(barca_service::schedule::Zone::Local)
}
