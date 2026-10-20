//! Get command implementation.

use super::execute::{
    cancel_on_ctrl_c, explain_cmd, print_failed_run, print_multi, read_final_output,
};
use crate::args::OutputMode;
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn get_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    python: &std::path::Path,
    mode: OutputMode,
    policy: barca_core::cache::CachePolicy,
    dry_run: bool,
    agent: bool,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    if let Some(line) = cfg.startup_line() {
        barca_core::errln!("{line}");
    }
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    if dry_run {
        return explain_cmd(
            &cfg, &targets, &file_args, python, policy, "get", mode, fields,
        )
        .await;
    }
    let (events, log_task) = super::execute::user_logs(mode);
    if targets.len() > 1 {
        let result = barca_core::commands::get_many_streaming(
            &cfg,
            &targets,
            &file_args,
            python,
            policy,
            agent,
            cancel_on_ctrl_c(),
            Some(events),
        )
        .await;
        let _ = log_task.await;
        let result = result?;
        return print_multi(&result, mode, "got", fields);
    }
    let target = targets.into_iter().next();
    let result = barca_core::commands::get_streaming(
        &cfg,
        target.as_deref(),
        &file_args,
        python,
        policy,
        agent,
        cancel_on_ctrl_c(),
        Some(events),
    )
    .await;
    let _ = log_task.await;
    let result = result.inspect_err(|e| print_failed_run(e, mode))?;
    super::emit(barca_core::report::render_get(
        &result,
        target.as_deref(),
        result.final_output.as_ref().map(read_final_output),
        mode.into(),
        fields,
    ));
    Ok(())
}
