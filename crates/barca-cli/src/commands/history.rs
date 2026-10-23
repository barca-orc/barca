//! History command implementation: the run table (filtered as asked), one run in detail, or
//! `--reconcile`.

use crate::error::{CliError, ErrorKind};
use barca_core::history_filter::HistoryFilter;

/// The filter flags of `barca history`, checked here (also before a server sees them).
#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
pub(crate) fn history_filter(
    status: Option<&str>,
    target: Option<&str>,
    triggered_by: Option<&str>,
    since: Option<&str>,
) -> Result<HistoryFilter, CliError> {
    HistoryFilter::parse(status, target, triggered_by, since, chrono::Utc::now())
        .map_err(|msg| CliError::from_prose(ErrorKind::Usage, format!("error: {msg}")))
}

/// What a hint's command adds so it reads the same history as this one: `--server URL`,
/// `--server`, `--local` and `--env NAME`, as given.
pub(crate) fn same_history(server: &crate::args::ServerFlags, env: Option<&str>) -> String {
    let mut out = String::new();
    match server.server.as_deref() {
        Some("") => out.push_str(" --server"),
        Some(url) => out.push_str(&format!(" --server {url}")),
        None if server.local => out.push_str(" --local"),
        None => {}
    }
    if let Some(env) = env {
        out.push_str(&format!(" --env {env}"));
    }
    out
}

pub(crate) struct HistoryArgs<'a> {
    pub(crate) limit: Option<usize>,
    pub(crate) json: bool,
    pub(crate) fields: Option<&'a [String]>,
    pub(crate) reconcile: Option<&'a str>,
    pub(crate) filter: HistoryFilter,
}

pub(crate) async fn history_cmd(
    env: Option<&str>,
    args: HistoryArgs<'_>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let (runs, total) = if let Some(run_id) = args.reconcile {
        (
            vec![barca_core::queries::reconcile_run(&cfg, run_id).await?],
            1,
        )
    } else {
        barca_core::queries::history(&cfg, args.limit, &args.filter).await?
    };
    super::emit(barca_core::report::render_history(
        &runs,
        total,
        args.json,
        args.fields,
    ));
    Ok(())
}

/// `barca history RUN_ID` on this machine.
pub(crate) async fn run_detail_cmd(
    env: Option<&str>,
    run_id: &str,
    limit: Option<usize>,
    json: bool,
    select: &str,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let detail = barca_core::queries::run_detail(&cfg, run_id, limit).await?;
    super::emit(barca_core::report::render_run_detail(&detail, json, select));
    Ok(())
}
