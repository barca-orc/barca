//! Get command implementation.

use super::execute::{cancel_on_ctrl_c, explain_cmd};
use crate::input::GetMode;
use barca_core::dispatch::OutputRef;
use barca_core::report::{GetShape, Returned, Shown, artifact_metadata};
use barca_core::selection::PartitionSelection;
use serde_json::Value;
use std::path::PathBuf;

/// A json value is previewed under its path in a terminal when its file is at most this big...
const PREVIEW_MAX_BYTES: u64 = 2048;
/// ...and it pretty-prints to at most this many lines.
const PREVIEW_MAX_LINES: usize = 10;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn get_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    python: &std::path::Path,
    mode: GetMode,
    policy: barca_core::cache::CachePolicy,
    partition: Option<PartitionSelection>,
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
            &cfg,
            &targets,
            &file_args,
            python,
            policy,
            "get",
            mode.explain_mode(),
            fields,
            partition.as_ref(),
        )
        .await;
    }
    let (events, log_task) = super::execute::user_logs(mode.shape != GetShape::Json);
    let reader = Reader {
        python,
        cfg: &cfg,
        mode,
    };
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
            partition,
        )
        .await;
        let _ = log_task.await;
        let result = result?;
        let mut returned = Vec::with_capacity(result.targets.len());
        for (_, t) in &result.targets {
            returned.push(
                reader
                    .returned(t.final_output.as_ref(), t.partitions.as_deref())
                    .await,
            );
        }
        super::emit(barca_core::report::render_get_multi(
            &result, &returned, mode.shape, fields,
        ));
        return match barca_core::report::first_multi_failure(&result) {
            Some(error) => Err(error),
            None => Ok(()),
        };
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
        partition,
    )
    .await;
    let _ = log_task.await;
    let result = match result {
        Ok(result) => result,
        Err(e) => {
            // A partitioned target that stopped at a failed key still returns the keys that
            // succeeded; the failed ones are absent.
            let partial = match &e {
                barca_core::BarcaError::WorkerFailed(f) => {
                    match f.run.as_ref().and_then(|r| r.partitions.as_deref()) {
                        Some(keys) => Some(reader.returned(None, Some(keys)).await),
                        None => None,
                    }
                }
                _ => None,
            };
            super::emit(barca_core::report::render_failed_get(
                &e,
                partial.as_ref(),
                mode.shape,
            ));
            return Err(e);
        }
    };
    let returned = reader
        .returned(result.final_output.as_ref(), result.partitions.as_deref())
        .await;
    super::emit(barca_core::report::render_get(
        &result,
        target.as_deref(),
        &returned,
        mode.shape,
        fields,
    ));
    Ok(())
}

/// Turns what a run returned into what the command prints.
struct Reader<'a> {
    python: &'a std::path::Path,
    cfg: &'a barca_core::config::ResolvedConfig,
    mode: GetMode,
}

impl Reader<'_> {
    /// One target's output: its artifact, or each of a partitioned target's keys.
    async fn returned(
        &self,
        single: Option<&OutputRef>,
        keys: Option<&[(String, OutputRef)]>,
    ) -> Returned {
        let items: Vec<(Option<&str>, &OutputRef)> = match keys {
            Some(keys) => keys.iter().map(|(k, o)| (Some(k.as_str()), o)).collect(),
            None => single.into_iter().map(|o| (None, o)).collect(),
        };
        let refs: Vec<&OutputRef> = items.iter().map(|(_, o)| *o).collect();
        let shown_values: Vec<Value> = if self.mode.values {
            barca_core::values::read_values(self.python, self.cfg, &refs)
                .await
                .into_iter()
                .zip(&refs)
                .map(|(value, oref)| {
                    value.unwrap_or_else(|e| {
                        barca_core::errln!(
                            "[barca] warning: could not read the value of {}: {e}; its path is \
                             returned instead",
                            oref.path
                        );
                        artifact_metadata(oref)
                    })
                })
                .collect()
        } else {
            refs.iter().map(|o| artifact_metadata(o)).collect()
        };
        let preview = self.mode.shape == GetShape::Pretty
            && !self.mode.values
            && std::io::IsTerminal::is_terminal(&std::io::stdout());
        let shown = items
            .iter()
            .map(|(key, oref)| Shown {
                key: key.map(str::to_string),
                path: oref.path.clone(),
                preview: preview.then(|| preview_of(oref)).flatten(),
            })
            .collect();
        let final_output = match keys {
            Some(_) => Some(Value::Object(
                items
                    .iter()
                    .zip(shown_values)
                    .map(|((key, _), v)| (key.unwrap_or_default().to_string(), v))
                    .collect(),
            )),
            None => shown_values.into_iter().next(),
        };
        Returned {
            final_output,
            values: self.mode.values,
            shown,
        }
    }
}

/// A small json value, pretty-printed, to show under its path.
fn preview_of(oref: &OutputRef) -> Option<String> {
    if oref.format != "json" || oref.size_bytes > PREVIEW_MAX_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&oref.path).ok()?).ok()?;
    let text = serde_json::to_string_pretty(&value).ok()?;
    (text.lines().count() <= PREVIEW_MAX_LINES).then_some(text)
}
