//! Serve command implementation.

use std::path::PathBuf;

/// `barca serve`'s flags, as typed.
pub(crate) struct ServeArgs {
    pub(crate) env: Option<String>,
    pub(crate) auth: Option<barca_core::serve::AuthMode>,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) host: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) watch: bool,
    /// `!--no-schedule`.
    pub(crate) schedule: bool,
    pub(crate) timezone: String,
    pub(crate) read_only: bool,
    pub(crate) drain_timeout: Option<u64>,
    pub(crate) max_runs: Option<usize>,
    pub(crate) capacity: Option<usize>,
    pub(crate) run_timeout: Option<u64>,
}

pub(crate) async fn serve_cmd(
    args: ServeArgs,
    python: &std::path::Path,
    ctx: &crate::error::Context,
) -> Result<(), crate::error::CliError> {
    use crate::error::{CliError, ErrorKind};
    let ServeArgs {
        env,
        auth,
        files,
        host,
        port,
        watch,
        schedule,
        timezone,
        read_only,
        drain_timeout,
        max_runs,
        capacity,
        run_timeout,
    } = args;
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, ctx);
    let mut resolved = barca_core::config::resolve(env.as_deref()).map_err(engine)?;
    // A Datadog site that is not a host name would make every trace link wrong: a usage error.
    barca_core::config::datadog_app_url(&resolved.project_root).map_err(engine)?;
    if let Some(mode) = auth {
        resolved.server_auth = mode;
    }
    // `--drain-timeout` > `[server] drain_timeout` > 30 s.
    if let Some(secs) = drain_timeout {
        resolved.drain_timeout_secs = secs;
    }
    // `--max-runs` > `[server] max_runs` > one per core.
    if let Some(n) = max_runs {
        resolved.server_max_runs = Some(n);
    }
    // `--capacity` > `[server] capacity` > none (#628).
    if let Some(n) = capacity {
        resolved.server_capacity = Some((n, "--capacity"));
    }
    // `--run-timeout` > `[server] run_timeout_seconds` > none (#628).
    if let Some(secs) = run_timeout {
        resolved.server_run_timeout_secs = Some((secs, "--run-timeout"));
    }
    // `serve` prints its startup line (env, store and the metadata DB) once the database is
    // set up.
    let config = barca_server::ServeConfig {
        files: files.iter().map(|p| p.display().to_string()).collect(),
        host,
        port,
        watch,
        schedule,
        timezone,
        python: python.to_path_buf(),
        resolved,
        read_only,
    };
    let secs = config.resolved.drain_timeout_secs;
    match barca_server::serve(config).await.map_err(|e| match e {
        // Exit 3 with the database named and what to do (#513).
        barca_server::ServeError::Metadata(e) => engine(e),
        e => engine(barca_core::BarcaError::Other(e.to_string())),
    })? {
        // Nothing accepted was lost: the runs in flight finished, or were put back in the queue
        // for the next start.
        barca_server::Stopped::Drained { .. } | barca_server::Stopped::Requeued { .. } => Ok(()),
        // Exit 1, so deploy tooling can tell a forced stop from a clean drain.
        barca_server::Stopped::Forced { cancelled, .. } => {
            let mut error = CliError::from_prose(
                ErrorKind::StepFailed,
                format!(
                    "stopped before the runs in flight finished: {cancelled} run{} cancelled \
                     ({})",
                    if cancelled == 1 { "" } else { "s" },
                    barca_server::SHUTDOWN_REASON
                ),
            );
            error.remediation = Some(format!(
                "A second SIGTERM or SIGINT cancels the runs in flight; one signal lets them \
                 finish, and puts them back in the queue when the drain timeout (now {secs}s, \
                 --drain-timeout or [server] drain_timeout) passes. The cancelled runs are in \
                 barca history; run them again."
            ));
            Err(error)
        }
    }
}
