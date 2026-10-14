//! Execute command implementation.

use crate::args::OutputMode;
use barca_core::report::artifact_metadata;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn explain_cmd(
    cfg: &barca_core::config::ResolvedConfig,
    targets: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: barca_core::cache::CachePolicy,
    label: &str,
    mode: OutputMode,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let result =
        barca_core::queries::explain(cfg, targets, file_args, python, policy, false, label).await?;
    super::emit(barca_core::report::render_explain(
        &result,
        label,
        mode.into(),
        fields,
    ));
    Ok(())
}

/// Print a multi-target run (`barca get|run a,b`) and exit 1 if any target failed.
///
/// JSON: the run fields of a single-target run without `final_output`, plus `targets`, keyed by
/// target name in the order given: `{"status": "success", "final_output": ...}` or
/// `{"status": "failed", "failed_node": ..., "error": ...}`.
/// Print a multi-target result. When any target failed this returns the first failure as a
/// step failure (exit 1, and the error envelope on stderr), after the full result was printed.
pub(crate) fn print_multi(
    result: &barca_core::results::MultiResult,
    mode: OutputMode,
    verb: &str,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let values: Vec<_> = result
        .targets
        .iter()
        .map(|(_, target)| target.final_output.as_ref().map(read_final_output))
        .collect();
    super::emit(barca_core::report::render_multi(
        result,
        &values,
        mode.into(),
        verb,
        fields,
    ));
    match barca_core::report::first_multi_failure(result) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// On a failed run in JSON mode, still print the one-line result on stdout so agents need not
/// parse stderr: `status: "failed"`, the failing step and its error, and what ran before it.
/// The error itself still goes to stderr as the envelope.
pub(crate) fn print_failed_run(err: &barca_core::BarcaError, mode: OutputMode) {
    super::emit(barca_core::report::render_failed_run(err, mode.into()));
}

/// The run's stop signals, driven by Ctrl-C (SIGINT) and by SIGTERM, which is what a
/// supervisor, a CI runner's timeout or `docker stop` sends. Both mean the same. The first
/// one cancels the run: its workers and transfers are stopped and it is recorded as
/// `cancelled` instead of lingering as `running`, then it wraps up (it shares its record, for
/// a bounded time). A second one, of either kind, abandons the wrap-up. Every signal counts,
/// whether the terminal sent it to the whole job or something sent it to barca alone.
///
/// The handlers are installed here, before the run starts. SIGTERM used to keep its default
/// action: the process died at once, its workers were left to notice, and the run stayed
/// `running` until a later command reported it `interrupted`. As process 1 of a container
/// (`docker run image barca get`) the kernel discarded the signal instead, because process 1
/// only receives signals it has a handler for (#289).
pub(crate) fn cancel_on_ctrl_c() -> barca_core::interrupt::Interrupt {
    use tokio::signal::unix::{SignalKind, signal};
    let interrupt = barca_core::interrupt::Interrupt::new();
    for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
        let Ok(mut stream) = signal(kind) else {
            continue;
        };
        let seen = interrupt.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                seen.interrupt();
            }
        });
    }
    interrupt
}

/// Read an artifact for display: inline JSON values, show metadata for binary formats.
pub(crate) fn read_final_output(oref: &barca_core::dispatch::OutputRef) -> serde_json::Value {
    if oref.format == "json" {
        std::fs::read_to_string(&oref.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| artifact_metadata(oref))
    } else {
        artifact_metadata(oref)
    }
}
