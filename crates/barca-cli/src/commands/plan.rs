//! Plan command implementation.

use std::path::PathBuf;

pub(crate) async fn plan_cmd(
    files: Vec<PathBuf>,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let result = barca_core::queries::plan(&file_args, python).await?;
    super::emit(barca_core::report::render_plan(&result));
    Ok(())
}
