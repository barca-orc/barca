//! Stats command implementation.

use std::path::PathBuf;

pub(crate) async fn stats_cmd(
    env: Option<&str>,
    target: String,
    files: Vec<PathBuf>,
    json: bool,
    fields: Option<&[String]>,
    python: &std::path::Path,
) -> Result<(), barca_service::BarcaError> {
    let cfg = barca_service::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let stats = barca_service::queries::stats(&cfg, &target, &file_args, python).await?;
    super::emit(barca_service::report::render_stats(&stats, json, fields));
    Ok(())
}
