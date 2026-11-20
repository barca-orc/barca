//! Status command implementation.

use crate::request::{KeysView, StatusOpts};
use std::path::PathBuf;

pub(crate) async fn status_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    opts: StatusOpts<'_>,
    python: &std::path::Path,
) -> Result<(), barca_service::BarcaError> {
    let cfg = barca_service::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut result =
        barca_service::loaded::status(&cfg, &targets, &file_args, python, opts.sample, true)
            .await?;
    opts.filter.apply(&mut result);
    result.disk_usage = barca_service::gc::disk_usage(&cfg);
    let root = super::project_root();
    super::emit(barca_service::report::render_status(
        result,
        opts.json,
        opts.limit,
        opts.fields,
        root.as_deref(),
    ));
    Ok(())
}

/// `barca status --rollup`: the rollup of the targets' upstream cones (every node without one).
pub(crate) async fn rollup_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    json: bool,
    python: &std::path::Path,
) -> Result<(), barca_service::BarcaError> {
    let cfg = barca_service::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut rollup = barca_service::loaded::rollup(&cfg, &targets, &file_args, python).await?;
    if !targets.is_empty() {
        rollup.entries[0].label = targets.join(", ");
    }
    super::emit(barca_service::report::render_rollup(&rollup, json));
    Ok(())
}

/// `barca status NODE --keys | --overview | --grid | --key KEY` on this machine: the same
/// answers a server's routes serve, printed by the same renderers.
pub(crate) async fn keys_cmd(
    env: Option<&str>,
    target: &str,
    files: Vec<PathBuf>,
    view: KeysView,
    json: bool,
    limit: Option<usize>,
    python: &std::path::Path,
) -> Result<(), barca_service::BarcaError> {
    use barca_service::partition_grid as grid;
    let cfg = barca_service::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let rendered = match view {
        KeysView::Key { key, sample } => {
            let detail = barca_service::loaded::key_for_target(
                &cfg, target, &file_args, python, &key, sample,
            )
            .await?
            .ok_or_else(|| barca_service::BarcaError::Usage(grid::no_such_key(target, &key)))?;
            barca_service::report::render_partition_key(&detail, json)
        }
        KeysView::Grid => {
            let grid =
                barca_service::loaded::grid_for_target(&cfg, target, &file_args, python).await?;
            barca_service::report::render_partition_grid(&grid, limit, json)
        }
        KeysView::Overview => {
            let grid =
                barca_service::loaded::grid_for_target(&cfg, target, &file_args, python).await?;
            barca_service::report::render_partition_overview(&grid.overview(limit), json)
        }
        KeysView::Keys(mut query) => {
            let grid =
                barca_service::loaded::grid_for_target(&cfg, target, &file_args, python).await?;
            query.limit = limit;
            let page = grid
                .keys_page(&query)
                .map_err(barca_service::BarcaError::Usage)?;
            barca_service::report::render_partition_keys(&page, query.filters(), json)
        }
    };
    super::emit(rendered);
    Ok(())
}
