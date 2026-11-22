//! List command implementation.

use std::path::PathBuf;

pub(crate) async fn list_cmd(
    files: Vec<PathBuf>,
    groups: bool,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
    python: &std::path::Path,
) -> Result<(), barca_service::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    if groups {
        let dag = barca_service::load::build_dag(&file_args, python).await?;
        crate::print::print_groups(&dag.groups, json);
        return Ok(());
    }
    let mut assets = barca_service::queries::list_assets(&file_args, python).await?;
    // Bounded output: the first `limit` nodes in topological order (`--all` = no limit).
    let total = assets.len();
    assets.truncate(limit.unwrap_or(total));

    // Next fire times (RFC 3339) for scheduled definitions. Empty when nothing is
    // scheduled, so the table's NEXT FIRE column only appears when it carries information.
    let next_fires: std::collections::HashMap<String, String> =
        barca_service::loaded::describe_schedule(&file_args, python)
            .await
            .into_iter()
            .filter_map(|j| j.next_fire.map(|t| (j.id, barca_service::time::rfc3339(t))))
            .collect();
    let root = super::project_root();
    super::emit(barca_service::report::render_list(
        &assets,
        total,
        &next_fires,
        json,
        fields,
        root.as_deref(),
    ));
    Ok(())
}
