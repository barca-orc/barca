//! List command implementation.

use std::path::PathBuf;

pub(crate) async fn list_cmd(
    files: Vec<PathBuf>,
    groups: bool,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    if groups {
        let dag = barca_core::load::build_dag(&file_args, python).await?;
        if json {
            barca_core::outln!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"groups": dag.groups})).unwrap()
            );
        } else {
            let by_id: std::collections::HashMap<_, _> =
                dag.groups.iter().map(|g| (g.id.as_str(), g)).collect();
            let nested: std::collections::HashSet<_> = dag
                .groups
                .iter()
                .flat_map(|g| g.members.iter().map(String::as_str))
                .collect();
            fn show(
                id: &str,
                depth: usize,
                groups: &std::collections::HashMap<&str, &barca_core::groups::NodeGroup>,
            ) {
                if let Some(g) = groups.get(id) {
                    barca_core::outln!(
                        "{}{} [{}] → {}",
                        "  ".repeat(depth),
                        g.name,
                        g.id,
                        g.output
                    );
                    for m in &g.members {
                        show(m, depth + 1, groups);
                    }
                } else {
                    barca_core::outln!("{}{}", "  ".repeat(depth), id);
                }
            }
            if dag.groups.is_empty() {
                barca_core::outln!("No groups declared.");
            }
            for g in &dag.groups {
                if !nested.contains(g.id.as_str()) {
                    show(&g.id, 0, &by_id);
                }
            }
        }
        return Ok(());
    }
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
