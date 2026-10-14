//! Sql command implementation.

use std::path::PathBuf;

pub(crate) async fn sql_cmd(
    env: Option<&str>,
    query: &str,
    files: Vec<PathBuf>,
    limit: Option<usize>,
    json: bool,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let result = barca_core::sql::sql(&cfg, query, &file_args, python, limit).await?;
    super::emit(barca_core::report::render_sql(result, json));
    Ok(())
}
