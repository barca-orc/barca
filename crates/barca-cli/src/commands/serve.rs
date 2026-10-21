//! Serve command implementation.

use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
pub(crate) async fn serve_cmd(
    env: Option<&str>,
    files: Vec<PathBuf>,
    host: std::net::IpAddr,
    port: u16,
    watch: bool,
    schedule: bool,
    timezone: String,
    read_only: bool,
    drain_timeout: Option<u64>,
    python: &std::path::Path,
    ctx: &crate::error::Context,
) -> Result<(), crate::error::CliError> {
    use crate::error::{CliError, ErrorKind};
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, ctx);
    let mut resolved = barca_core::config::resolve(env).map_err(engine)?;
    // `--drain-timeout` > `[server] drain_timeout` > 30 s.
    if let Some(secs) = drain_timeout {
        resolved.drain_timeout_secs = secs;
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
        barca_server::Stopped::Drained => Ok(()),
        // Exit 1, so deploy tooling can tell a forced stop from a clean drain.
        barca_server::Stopped::Forced { cancelled } => {
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
                "Give runs longer to finish with --drain-timeout (now {secs}s) or [server] \
                 drain_timeout in barca.toml, and the supervisor a longer stop grace period \
                 than that. The cancelled runs are in barca history; run them again."
            ));
            Err(error)
        }
    }
}
