//! History command implementation: the run table (filtered as asked), one run in detail, or
//! `--reconcile`.

use crate::error::{CliError, ErrorKind};
use barca_core::history_filter::HistoryFilter;

/// The filter flags of `barca history`, checked here (also before a server sees them).
pub(crate) fn history_filter(
    flags: barca_core::history_filter::HistoryFlags<'_>,
) -> Result<HistoryFilter, CliError> {
    HistoryFilter::parse(flags, chrono::Utc::now())
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
    /// The interpreter a run in progress is inspected with (`--status running`).
    pub(crate) python: &'a std::path::Path,
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
    // Asked for the runs in progress: each with what it is working on now (#991).
    let now = if args.filter.statuses.iter().any(|s| s == "running")
        && runs.iter().any(|r| r.status == "running")
    {
        barca_core::queries::history_now(&cfg, &runs, args.python).await
    } else {
        Default::default()
    };
    super::emit(barca_core::report::render_history(
        &runs,
        total,
        args.json,
        args.fields,
        &barca_core::report::HistoryExtras::now(now),
    ));
    Ok(())
}

/// `barca history --activity` on this machine: the runs of the last 24 hours, hour by hour.
pub(crate) async fn activity_cmd(
    env: Option<&str>,
    filter: &HistoryFilter,
    json: bool,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let activity = barca_core::queries::activity(&cfg, filter).await?;
    super::emit(barca_core::report::render_activity(&activity, json));
    Ok(())
}

/// `barca history RUN_ID` on this machine.
pub(crate) async fn run_detail_cmd(
    env: Option<&str>,
    run_id: &str,
    limit: Option<usize>,
    json: bool,
    select: &str,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let detail = barca_core::served::run_detail(&cfg, run_id, limit, python).await?;
    super::emit(barca_core::report::render_run_detail(&detail, json, select));
    Ok(())
}
