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
