//! Serve command implementation.

use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_cmd(
    env: Option<&str>,
    files: Vec<PathBuf>,
    host: std::net::IpAddr,
    port: u16,
    watch: bool,
    schedule: bool,
    timezone: String,
    read_only: bool,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let resolved = barca_core::config::resolve(env)?;
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
    barca_server::serve(config)
        .await
        .map_err(|e| barca_core::BarcaError::Other(e.to_string()))
}
