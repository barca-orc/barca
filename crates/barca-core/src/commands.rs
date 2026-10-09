//! Engine commands — get and run. Return typed results; callers handle display.
//!
//! Every command is an `async fn` that runs on the caller's runtime: the CLI
//! builds one runtime in `main()`, the server `.await`s these directly. No
//! runtime is ever constructed in this crate; genuinely blocking work (source
//! parsing, dynamic-partition subprocesses) runs via `spawn_blocking`.

use crate::execution::{ExecuteRequest, Executed, execute};
use crate::{
    BarcaError,
    cache::CachePolicy,
    results::{GetResult, MultiResult},
};
use std::{env, path::PathBuf};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

// ─── Shared setup ────────────────────────────────────────────────────────────

pub fn find_python() -> PathBuf {
    // Look for sibling python in the same bin/ directory as the barca binary.
    if let Ok(self_exe) = env::current_exe()
        && let Some(bin_dir) = self_exe.parent()
    {
        let candidate = bin_dir.join("python");
        if candidate.exists() {
            return candidate;
        }
        let candidate3 = bin_dir.join("python3");
        if candidate3.exists() {
            return candidate3;
        }
    }
    // Fall back to PATH.
    PathBuf::from("python3")
}

// ─── get / run ─────────────────────────────────────────────────────────────────

/// `barca get` — cache-aware execution of an asset (or all assets).
/// Cancelling `cancel` stops the run mid-flight: workers are terminated,
/// partial results are persisted, and the run row is marked `cancelled`.
pub async fn get(
    cfg: &crate::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<crate::interrupt::Interrupt>,
) -> Result<GetResult, BarcaError> {
    let names: Vec<String> = target_name.map(str::to_string).into_iter().collect();
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &names,
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx: None,
    })
    .await?
    .into_single()
}

/// `barca run` — execute a task (and its cone). The task always re-runs;
/// upstream assets follow `policy` (`CacheAware` by default, like `barca get`).
pub async fn run(
    cfg: &crate::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<crate::interrupt::Interrupt>,
) -> Result<GetResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &[target_name.to_string()],
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx: None,
    })
    .await?
    .into_single()
}

/// Like [`get`] but streams live [`crate::RunEvent`]s to `event_tx` as the run
/// progresses (logs, step completion). Logs are persisted to the DB regardless.
#[allow(clippy::too_many_arguments)]
pub async fn get_streaming(
    cfg: &crate::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: CancellationToken,
    event_tx: Option<UnboundedSender<crate::RunEvent>>,
) -> Result<GetResult, BarcaError> {
    let names: Vec<String> = target_name.map(str::to_string).into_iter().collect();
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &names,
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx,
    })
    .await?
    .into_single()
}

/// Like [`run`] but streams live [`crate::RunEvent`]s to `event_tx`.
#[allow(clippy::too_many_arguments)]
pub async fn run_streaming(
    cfg: &crate::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: CancellationToken,
    event_tx: Option<UnboundedSender<crate::RunEvent>>,
) -> Result<GetResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &[target_name.to_string()],
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx,
    })
    .await?
    .into_single()
}

/// `barca get a,b` — several assets in one run. The union of their cones is planned once, so a
/// shared upstream asset materializes once. Every target is attempted: a failure stops only the
/// targets downstream of it, and is reported in that target's outcome (not as an `Err`).
pub async fn get_many(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<crate::interrupt::Interrupt>,
) -> Result<MultiResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names,
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx: None,
    })
    .await
    .map(Executed::into_multi)
}

/// `barca run a,b` — several tasks in one run; see [`get_many`]. Every task always re-runs;
/// upstream assets follow `policy`, applied to the union of the cones.
pub async fn run_many(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<crate::interrupt::Interrupt>,
) -> Result<MultiResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names,
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx: None,
    })
    .await
    .map(Executed::into_multi)
}

/// Execute a served asset graph without reparsing excluded definitions.
#[allow(clippy::too_many_arguments)]
pub async fn get_streaming_from_dag(
    dag: crate::dag::Dag,
    cfg: &crate::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: CancellationToken,
    event_tx: Option<UnboundedSender<crate::RunEvent>>,
) -> Result<GetResult, BarcaError> {
    let names: Vec<String> = target_name.map(str::to_string).into_iter().collect();
    execute(ExecuteRequest {
        dag: Some(dag),
        cfg,
        target_names: &names,
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx,
    })
    .await?
    .into_single()
}

/// Execute a served task graph without reparsing excluded definitions.
#[allow(clippy::too_many_arguments)]
pub async fn run_streaming_from_dag(
    dag: crate::dag::Dag,
    cfg: &crate::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: CancellationToken,
    event_tx: Option<UnboundedSender<crate::RunEvent>>,
) -> Result<GetResult, BarcaError> {
    execute(ExecuteRequest {
        dag: Some(dag),
        cfg,
        target_names: &[target_name.to_string()],
        file_args,
        python,
        no_cache: false,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx,
    })
    .await?
    .into_single()
}
