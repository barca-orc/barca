//! Status command implementation.

use std::path::PathBuf;

pub(crate) struct StatusOpts<'a> {
    pub(crate) json: bool,
    pub(crate) limit: Option<usize>,
    pub(crate) fields: Option<&'a [String]>,
    pub(crate) sample: usize,
    /// `--state` / `--problems`.
    pub(crate) filter: barca_core::status::StatusFilter,
}

pub(crate) async fn status_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    opts: StatusOpts<'_>,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut result =
        barca_core::status::status(&cfg, &targets, &file_args, python, opts.sample, true).await?;
    opts.filter.apply(&mut result);
    result.disk_usage = barca_core::gc::disk_usage(&cfg);
    let root = super::project_root();
    super::emit(barca_core::report::render_status(
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
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut rollup = barca_core::rollup::rollup(&cfg, &targets, &file_args, python).await?;
    if !targets.is_empty() {
        rollup.entries[0].label = targets.join(", ");
    }
    super::emit(barca_core::report::render_rollup(&rollup, json));
    Ok(())
}

/// `barca status NODE --keys`, `--overview`, `--grid` or `--key KEY`: which view of one
/// partitioned node's keys is printed.
pub(crate) enum KeysView {
    /// `--keys [--state a,b] [--needs-run] [--value D=V] [--range D=A..B] [--error N]
    /// [--match TEXT] [--sort ORDER] [--offset N]`.
    Keys(barca_core::key_query::KeyQuery),
    /// `--overview`.
    Overview,
    /// `--grid`.
    Grid,
    /// `--key KEY [--sample N]`.
    Key { key: String, sample: usize },
}

/// The flags of `barca status` that pick a keys view and narrow `--keys`.
pub(crate) struct KeysFlags {
    pub(crate) keys: bool,
    pub(crate) overview: bool,
    pub(crate) grid: bool,
    pub(crate) key: Option<String>,
    pub(crate) states: Vec<String>,
    pub(crate) needs_run: bool,
    pub(crate) values: Vec<String>,
    pub(crate) range: Option<String>,
    pub(crate) error: Option<usize>,
    pub(crate) text: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) sample: Option<usize>,
}

impl KeysView {
    /// The view the flags ask for, if any. `--state` names key states with `--keys`; a state,
    /// an order, a value or a range barca does not read is a usage error saying what it does.
    pub(crate) fn of(flags: KeysFlags) -> Result<Option<KeysView>, String> {
        if let Some(key) = flags.key {
            let sample = flags
                .sample
                .unwrap_or(0)
                .min(barca_core::partition_grid::KEY_SAMPLE_MAX);
            return Ok(Some(KeysView::Key { key, sample }));
        }
        if flags.grid {
            return Ok(Some(KeysView::Grid));
        }
        if flags.overview {
            return Ok(Some(KeysView::Overview));
        }
        if flags.keys {
            let args = barca_core::key_query::KeyQueryArgs {
                states: &flags.states,
                needs_run: flags.needs_run,
                values: &flags.values,
                range: flags.range.as_deref(),
                error: flags.error,
                text: flags.text.as_deref(),
                sort: flags.sort.as_deref(),
                offset: flags.offset.unwrap_or(0),
                limit: None,
            };
            return barca_core::key_query::KeyQuery::parse(&args)
                .map(|query| Some(KeysView::Keys(query)))
                .map_err(|e| format!("error: --keys: {e}"));
        }
        Ok(None)
    }

    /// The one node a keys view is of: exactly one target, or a usage error.
    pub(crate) fn target(&self, targets: &[String]) -> Result<String, String> {
        match targets {
            [one] => Ok(one.clone()),
            _ => Err(format!(
                "error: {} shows the keys of one partitioned node: name exactly one target\n\
                 For example: barca status NODE {}",
                self.flag(),
                self.flag()
            )),
        }
    }

    fn flag(&self) -> &'static str {
        match self {
            KeysView::Keys(_) => "--keys",
            KeysView::Overview => "--overview",
            KeysView::Grid => "--grid",
            KeysView::Key { .. } => "--key KEY",
        }
    }
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
) -> Result<(), barca_core::BarcaError> {
    use barca_core::partition_grid as grid;
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let rendered = match view {
        KeysView::Key { key, sample } => {
            let detail = grid::key_for_target(&cfg, target, &file_args, python, &key, sample)
                .await?
                .ok_or_else(|| barca_core::BarcaError::Usage(grid::no_such_key(target, &key)))?;
            barca_core::report::render_partition_key(&detail, json)
        }
        KeysView::Grid => {
            let grid = grid::grid_for_target(&cfg, target, &file_args, python).await?;
            barca_core::report::render_partition_grid(&grid, limit, json)
        }
        KeysView::Overview => {
            let grid = grid::grid_for_target(&cfg, target, &file_args, python).await?;
            barca_core::report::render_partition_overview(&grid.overview(limit), json)
        }
        KeysView::Keys(mut query) => {
            let grid = grid::grid_for_target(&cfg, target, &file_args, python).await?;
            query.limit = limit;
            let page = grid
                .keys_page(&query)
                .map_err(barca_core::BarcaError::Usage)?;
            barca_core::report::render_partition_keys(&page, query.filters(), json)
        }
    };
    super::emit(rendered);
    Ok(())
}
