//! List command implementation.

use std::path::PathBuf;

pub(crate) async fn list_cmd(
    files: Vec<PathBuf>,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut assets = barca_core::queries::list_assets(&file_args, python).await?;
    // Bounded output: the first `limit` nodes in topological order (`--all` = no limit).
    let total = assets.len();
    assets.truncate(limit.unwrap_or(total));

    // Next fire times (local time) for scheduled definitions. Empty when nothing is
    // scheduled, so the table's NEXT FIRE column only appears when it carries information.
    let next_fires: std::collections::HashMap<String, String> =
        barca_core::schedule::describe_schedule(&file_args, python)
            .await
            .into_iter()
            .filter_map(|j| j.next_fire_local.map(|t| (j.id, t)))
            .collect();
    let root = super::project_root();
    super::emit(barca_core::report::render_list(
        &assets,
        total,
        &next_fires,
        json,
        fields,
        root.as_deref(),
    ));
    Ok(())
}
