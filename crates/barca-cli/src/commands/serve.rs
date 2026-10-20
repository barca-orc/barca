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
    if resolved.state == barca_core::config::StateMode::Optimistic && resolved.state_uri.is_some() {
        return Err(engine(barca_core::BarcaError::Usage(
            "barca serve does not support shared remote state yet — set state = \"off\" \
             in barca.toml (or BARCA_STATE=off) to serve with a local metadata DB"
                .to_string(),
        )));
    }
    if let Some(line) = resolved.startup_line() {
        barca_core::errln!("{line}");
    }
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
    match barca_server::serve(config)
        .await
        .map_err(|e| engine(barca_core::BarcaError::Other(e.to_string())))?
    {
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
