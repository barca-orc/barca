//! Plan command implementation.

use std::path::PathBuf;

pub(crate) async fn plan_cmd(
    files: Vec<PathBuf>,
    python: &std::path::Path,
    partition: Option<barca_service::selection::PartitionSelection>,
) -> Result<(), barca_service::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let result = barca_service::queries::plan(&file_args, python, partition.as_ref()).await?;
    super::emit(barca_service::report::render_plan(&result));
    Ok(())
}
