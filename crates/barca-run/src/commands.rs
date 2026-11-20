//! Engine commands — get and run. Return typed results; callers handle display.
//!
//! Every command is an `async fn` that runs on the caller's runtime: the CLI
//! builds one runtime in `main()`, the server `.await`s these directly. No
//! runtime is ever constructed in this crate; genuinely blocking work (source
//! parsing, dynamic-partition subprocesses) runs via `spawn_blocking`.

use crate::execution::{ExecuteRequest, Executed, RunCommand, execute};
use barca_decide::cache::CachePolicy;
use barca_model::BarcaError;
use barca_store::results::{GetResult, MultiResult};
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
    cfg: &barca_model::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<barca_engine::interrupt::Interrupt>,
) -> Result<GetResult, BarcaError> {
    let names: Vec<String> = target_name.map(str::to_string).into_iter().collect();
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &names,
        file_args,
        python,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx: None,
        partition: None,
        options: Default::default(),
        rules: barca_decide::freshness::RunRules::request(),
    })
    .await?
    .into_single()
}

/// `barca run` — execute a task (and its cone). The task always re-runs;
/// upstream assets follow `policy` (`CacheAware` by default, like `barca get`).
pub async fn run(
    cfg: &barca_model::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<barca_engine::interrupt::Interrupt>,
) -> Result<GetResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &[target_name.to_string()],
        file_args,
        python,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx: None,
        partition: None,
        options: Default::default(),
        rules: barca_decide::freshness::RunRules::request(),
    })
    .await?
    .into_single()
}

/// Like [`get`] but streams live [`barca_model::RunEvent`]s to `event_tx` as the run
/// progresses (logs, step completion). Logs are persisted to the DB regardless.
#[allow(clippy::too_many_arguments)]
pub async fn get_streaming(
    cfg: &barca_model::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<barca_engine::interrupt::Interrupt>,
    event_tx: Option<UnboundedSender<barca_model::RunEvent>>,
    partition: Option<barca_compile::selection::PartitionSelection>,
    batch: barca_engine::batch::BatchOverride,
) -> Result<GetResult, BarcaError> {
    let names: Vec<String> = target_name.map(str::to_string).into_iter().collect();
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &names,
        file_args,
        python,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx,
        partition,
        options: crate::execution::RunOptions {
            batch,
            ..Default::default()
        },
        rules: barca_decide::freshness::RunRules::request(),
    })
    .await?
    .into_single()
}

/// Like [`run`] but streams live [`barca_model::RunEvent`]s to `event_tx`.
#[allow(clippy::too_many_arguments)]
pub async fn run_streaming(
    cfg: &barca_model::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<barca_engine::interrupt::Interrupt>,
    event_tx: Option<UnboundedSender<barca_model::RunEvent>>,
    partition: Option<barca_compile::selection::PartitionSelection>,
    batch: barca_engine::batch::BatchOverride,
) -> Result<GetResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names: &[target_name.to_string()],
        file_args,
        python,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx,
        partition,
        options: crate::execution::RunOptions {
            batch,
            ..Default::default()
        },
        rules: barca_decide::freshness::RunRules::request(),
    })
    .await?
    .into_single()
}

/// Streaming counterpart that preserves the multi-target execution policy.
#[allow(clippy::too_many_arguments)]
pub async fn get_many_streaming(
    cfg: &barca_model::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<barca_engine::interrupt::Interrupt>,
    event_tx: Option<UnboundedSender<barca_model::RunEvent>>,
    partition: Option<barca_compile::selection::PartitionSelection>,
    batch: barca_engine::batch::BatchOverride,
) -> Result<MultiResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names,
        file_args,
        python,
        agent_mode,
        policy,
        command_label: "get",
        interrupt: cancel.into(),
        event_tx,
        partition,
        options: crate::execution::RunOptions {
            batch,
            ..Default::default()
        },
        rules: barca_decide::freshness::RunRules::request(),
    })
    .await
    .map(Executed::into_multi)
}

/// `barca run a,b` — several tasks in one run; see [`get_many`]. Every task always re-runs;
/// upstream assets follow `policy`, applied to the union of the cones. Events go to `event_tx`.
#[allow(clippy::too_many_arguments)]
pub async fn run_many_streaming(
    cfg: &barca_model::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: impl Into<barca_engine::interrupt::Interrupt>,
    event_tx: Option<UnboundedSender<barca_model::RunEvent>>,
    partition: Option<barca_compile::selection::PartitionSelection>,
    batch: barca_engine::batch::BatchOverride,
) -> Result<MultiResult, BarcaError> {
    execute(ExecuteRequest {
        dag: None,
        cfg,
        target_names,
        file_args,
        python,
        agent_mode,
        policy,
        command_label: "run",
        interrupt: cancel.into(),
        event_tx,
        partition,
        options: crate::execution::RunOptions {
            batch,
            ..Default::default()
        },
        rules: barca_decide::freshness::RunRules::request(),
    })
    .await
    .map(Executed::into_multi)
}

/// What a run produced: one target's result, or the result of several targets.
#[derive(Debug)]
pub enum RunResult {
    Single(GetResult),
    Multi(MultiResult),
}

/// A run `barca serve` starts over its loaded graph (no source is parsed again). Progress
/// lines are printed as for `--agent` and streamed as `line` events to `event_tx`.
pub struct ServedRun<'a> {
    pub dag: std::sync::Arc<barca_model::dag::Dag>,
    pub cfg: &'a barca_model::config::ResolvedConfig,
    pub command: RunCommand,
    /// As given; one target is a single-target run, several a multi-target run, none every
    /// asset and sensor.
    pub targets: &'a [String],
    pub files: &'a [String],
    pub python: &'a std::path::Path,
    pub policy: CachePolicy,
    pub cancel: CancellationToken,
    pub event_tx: Option<UnboundedSender<barca_model::RunEvent>>,
    pub options: crate::execution::RunOptions,
    /// `--partition`: the keys every narrowed node of the cone computes (all keys when `None`;
    /// see [`barca_compile::selection`]).
    pub partition: Option<barca_compile::selection::PartitionSelection>,
    /// The freshness rules (RFC-0008): [`barca_decide::freshness::RunRules::request`] for a run
    /// asked for with the CLI's flags (`POST /runs`), [`barca_decide::freshness::RunRules::tick`] for
    /// a scheduled tick, the default (nothing pinned) for the older trigger routes.
    pub rules: barca_decide::freshness::RunRules,
}

/// Execute a [`ServedRun`]. A single-target run that stops at a failed step is an `Err`, as
/// for [`get`]; a multi-target run reports failures per target.
pub async fn serve_run(run: ServedRun<'_>) -> Result<RunResult, BarcaError> {
    let executed = execute(ExecuteRequest {
        dag: Some(run.dag),
        cfg: run.cfg,
        target_names: run.targets,
        file_args: run.files,
        python: run.python,
        agent_mode: true,
        policy: run.policy,
        command_label: run.command.as_str(),
        interrupt: run.cancel.into(),
        event_tx: run.event_tx,
        partition: run.partition,
        options: run.options,
        rules: run.rules,
    })
    .await?;
    if run.targets.len() > 1 {
        Ok(RunResult::Multi(executed.into_multi()))
    } else {
        executed.into_single().map(RunResult::Single)
    }
}

/// Upload this host's pending results ([`barca_db::pending_uploads`]) among `store_paths` now (see
/// [`crate::execution::upload_pending`]): `barca get -o url` calls it before signing, so a
/// `local only` result is uploaded and then signed rather than reported missing.
pub async fn upload_pending_now(
    python: &std::path::Path,
    cfg: &barca_model::config::ResolvedConfig,
    store_paths: &[String],
    cancel: &CancellationToken,
) -> Result<(), BarcaError> {
    crate::execution::upload_pending(python, cfg, store_paths, cancel).await
}
