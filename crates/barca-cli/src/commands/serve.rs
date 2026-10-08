//! Serve command implementation.

use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_cmd(
    env: Option<&str>,
    files: Vec<PathBuf>,
    port: u16,
    watch: bool,
    schedule: bool,
    timezone: String,
    read_only: bool,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let resolved = barca_core::config::resolve(env)?;
    if resolved.state == barca_core::config::StateMode::Optimistic && resolved.state_uri.is_some() {
        return Err(barca_core::BarcaError::Usage(
            "barca serve does not support shared remote state yet — set state = \"off\" \
             in barca.toml (or BARCA_STATE=off) to serve with a local metadata DB"
                .to_string(),
        ));
    }
    let config = barca_server::ServeConfig {
        files: files.iter().map(|p| p.display().to_string()).collect(),
        host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
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
