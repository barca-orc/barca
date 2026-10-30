//! Inspection commands: `plan`, `history`, `stats` and the node listing behind `barca list`.
//!
//! None of these run a step. They read the project's source (statically) or the metadata DB.

use crate::BarcaError;
use crate::db;
use crate::load::build_dag;
use crate::planner::Phase;
use crate::planner::{self, ExecutionPlan, ResourceConfig};
use crate::results::{AssetSummary, PlanPhase, PlanPhaseReason, PlanResult, PlanStream};
use crate::targets::find_target_id;
use std::collections::HashMap;

// ─── plan ────────────────────────────────────────────────────────────────────

pub async fn plan(
    file_args: &[String],
    python: &std::path::Path,
    partition: Option<&crate::selection::PartitionSelection>,
) -> Result<PlanResult, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    // `barca plan` plans every step of the files it reads (#550).
    dag.check_imports(dag.graph.node_weights().map(|n| n.id.as_str()))?;
    let cwd = std::env::current_dir()
        .map_err(|e| BarcaError::Other(format!("cannot determine cwd: {e}")))?;
    let (capacity, pool) = crate::config::execution_in(&cwd)?;
    let sizing = crate::execution::sizing(capacity, pool, None);
    let result = plan_selected(&dag, sizing, partition)?;
    crate::warnings::print(&result.warnings);
    crate::execution::warn_over_capacity(&result.weights, result.capacity, &result.capacity_source);
    Ok(result)
}

/// Render a previously validated graph using the same planner as strict commands, for a pool
/// and capacity of `sizing` ([`crate::execution::sizing`]).
pub fn plan_from_dag(dag: &crate::dag::Dag, sizing: crate::execution::Sizing) -> PlanResult {
    plan_selected(dag, sizing, None).expect("a plan without a selection")
}

/// [`plan_from_dag`] narrowed to `partition` (`barca plan --partition`), as a `get` with no
/// target narrows its plan; a usage error as for `get`.
pub fn plan_selected(
    dag: &crate::dag::Dag,
    sizing: crate::execution::Sizing,
    partition: Option<&crate::selection::PartitionSelection>,
) -> Result<PlanResult, BarcaError> {
    let pool_size = sizing.pool_size;
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let mut plan = planner::plan_from_dag_narrowed(dag, &config, partition);
    let narrowing = match partition {
        Some(selection) => Some(selection.apply(dag, &mut plan, &[])?),
        None => None,
    };
    if let Some(note) = narrowing.as_ref().and_then(|n| n.note()) {
        crate::errln!("{note}");
    }

    let warnings = crate::warnings::for_plan(dag, &plan);
    Ok(PlanResult {
        plan_version: crate::results::PLAN_VERSION,
        warnings,
        pool_size,
        pool_source: sizing.pool_source.to_string(),
        capacity: sizing.capacity,
        capacity_source: sizing.capacity_source.to_string(),
        weights: crate::execution::planned_weights(&plan),
        total_steps: plan.total_steps,
        phases: plan
            .phases
            .iter()
            .map(|p| PlanPhase {
                reason: PlanPhaseReason::from(&p.reason),
                streams: p
                    .streams
                    .iter()
                    .map(|s| PlanStream {
                        stream_id: s.stream_id.clone(),
                        steps: s.steps.iter().map(|st| st.step_id.display()).collect(),
                    })
                    .collect(),
            })
            .collect(),
        partition_selection: narrowing.map(|n| n.report()),
    })
}

// ─── history ──────────────────────────────────────────────────────────────────

/// The most recent runs (newest first), at most `limit` of them (`None` = every run), plus the
/// total number of recorded runs so callers can report truncation.
pub async fn history(
    cfg: &crate::config::ResolvedConfig,
    limit: Option<usize>,
    filter: &crate::history_filter::HistoryFilter,
) -> Result<(Vec<db::RunRecord>, usize), BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    history_at(&cfg.db_path, limit, filter).await
}

/// The newest `limit` runs of the database at `db_path` that `filter` keeps (every one when
/// `None`), and how many it keeps in all.
pub async fn history_at(
    db_path: &str,
    limit: Option<usize>,
    filter: &crate::history_filter::HistoryFilter,
) -> Result<(Vec<db::RunRecord>, usize), BarcaError> {
    let total = db::count_runs(db_path).await?;
    if filter.is_empty() {
        let runs = db::get_recent_runs(db_path, limit.unwrap_or(total)).await?;
        return Ok((runs, total));
    }
    let mut runs = db::get_recent_runs(db_path, total).await?;
    runs.retain(|run| filter.matches(run));
    let total = runs.len();
    runs.truncate(limit.unwrap_or(total));
    Ok((runs, total))
}

/// One run and the steps it recorded, for `barca history RUN_ID`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunDetail {
    pub run: db::RunRecord,
    pub steps: Vec<db::RunStep>,
    /// How many steps the run recorded, and whether `steps` lists fewer.
    #[serde(default)]
    pub steps_page: db::StepsPage,
}

/// [`RunDetail`] of `run_id` in this environment; a usage error when it is not recorded.
/// At most `limit` of its recorded steps (every one with `None`).
pub async fn run_detail(
    cfg: &crate::config::ResolvedConfig,
    run_id: &str,
    limit: Option<usize>,
) -> Result<RunDetail, BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    let Some(run) = db::get_run(&cfg.db_path, run_id).await? else {
        return Err(BarcaError::Usage(format!(
            "run '{run_id}' was not found; use barca history --json to find a run id"
        )));
    };
    let (steps, steps_page) = db::get_run_steps_page(&cfg.db_path, run_id, limit).await?;
    Ok(RunDetail {
        run,
        steps,
        steps_page,
    })
}

/// Record a running row as interrupted after the operator has verified it stopped.
/// This is the explicit write exception to ordinary, read-only history inspection.
pub async fn reconcile_run(
    cfg: &crate::config::ResolvedConfig,
    run_id: &str,
) -> Result<db::RunRecord, BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::reconcile_run(&cfg.db_path, run_id).await
}

/// The most recent run (the newest row of `barca history`), for `barca logs` without a run id.
/// A usage error when no run is recorded in this environment.
pub async fn latest_run(cfg: &crate::config::ResolvedConfig) -> Result<db::RunRecord, BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::get_recent_runs(&cfg.db_path, 1)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            BarcaError::Usage(
                "no run is recorded in this environment yet\nRun `barca get` or `barca run` \
                 first, then `barca logs`."
                    .to_string(),
            )
        })
}

/// A run's recorded lines: the `logs` rows of the database at `read_path` (the metadata DB at
/// `db_path`, or a snapshot of it), else its spool beside `db_path`: a run still in progress,
/// or one whose process died before recording them.
pub async fn run_log_lines(
    db_path: &str,
    read_path: &str,
    run_id: &str,
) -> Result<Vec<db::LogEntry>, BarcaError> {
    let logs = db::get_logs(read_path, run_id).await?;
    if !logs.is_empty() {
        return Ok(logs);
    }
    let spool = crate::run_logs::spool_path(db_path, run_id);
    let (lines, _) = crate::run_logs::read_spool(&spool, 0)?;
    Ok(lines
        .into_iter()
        .enumerate()
        .map(|(seq, line)| db::LogEntry::at(seq, line))
        .collect())
}

/// Whether `--step <step>` selects the line of node `id`: its exact id, its base id (every
/// partition key), or its function name.
pub fn step_matches(id: &str, step: &str) -> bool {
    let parsed = crate::StepId::parse(id);
    if id == step || parsed.base_id() == step || parsed.base_id().rsplit(':').next() == Some(step) {
        return true;
    }
    // One key by function name: `scored[retailer=r7]`.
    let wanted = crate::StepId::parse(step);
    !wanted.partition.is_empty()
        && wanted.partition == parsed.partition
        && parsed.base_id().rsplit(':').next() == Some(wanted.base_id())
}

/// The node ids `--step <step>` selects among a run's recorded lines and steps. A usage error
/// when it selects none, or two different definitions.
fn select_step(
    logs: &[db::LogEntry],
    recorded: &[db::RunStep],
    run_id: &str,
    step: &str,
) -> Result<std::collections::HashSet<String>, BarcaError> {
    let all: std::collections::BTreeSet<&str> = logs
        .iter()
        .map(|line| line.node_id.as_str())
        .chain(recorded.iter().map(|row| row.node_id.as_str()))
        .collect();
    let matches: Vec<_> = all
        .into_iter()
        .filter(|id| step_matches(id, step))
        .collect();
    if matches.is_empty() {
        return Err(BarcaError::Usage(format!(
            "step '{step}' has no recorded execution or output in run '{run_id}'"
        )));
    }
    // A base name may select all of its partitions, but never two different definitions.
    let bases: std::collections::BTreeSet<_> = matches
        .iter()
        .map(|id| crate::StepId::parse(id).base_id().to_string())
        .collect();
    if bases.len() > 1 {
        return Err(BarcaError::Usage(format!(
            "step '{step}' is ambiguous; use one of: {}",
            bases.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(matches.into_iter().map(String::from).collect())
}

/// Read recorded output without loading user source or contacting remote state.
pub async fn logs(
    cfg: &crate::config::ResolvedConfig,
    run_id: &str,
    step: Option<&str>,
    stream: Option<crate::LogStream>,
) -> Result<Vec<db::LogEntry>, BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    logs_at(&cfg.db_path, &cfg.db_path, run_id, step, stream).await
}

/// [`logs`] of the metadata DB at `db_path`, read from `read_path` (it or a snapshot of it).
/// An unknown run is a usage error.
pub async fn logs_at(
    db_path: &str,
    read_path: &str,
    run_id: &str,
    step: Option<&str>,
    stream: Option<crate::LogStream>,
) -> Result<Vec<db::LogEntry>, BarcaError> {
    if db::get_run(read_path, run_id).await?.is_none() {
        return Err(BarcaError::Usage(format!(
            "run '{run_id}' was not found; use barca history --json to find a run id"
        )));
    }
    let mut logs = run_log_lines(db_path, read_path, run_id).await?;
    if let Some(step) = step {
        let recorded = db::get_run_steps(read_path, run_id).await?;
        let mut matches = select_step(&logs, &recorded, run_id, step)?;
        // A key of a batched call (RFC-0013): its call's output, recorded once for the call.
        for row in &recorded {
            if let Some(b) = &row.batch
                && matches.contains(&row.node_id)
            {
                matches.insert(format!(
                    "{}[batch={b}]",
                    crate::StepId::parse(&row.node_id).base_id()
                ));
            }
        }
        logs.retain(|line| matches.contains(&line.node_id));
    }
    if let Some(stream) = stream {
        logs.retain(|line| line.stream == stream);
    }
    Ok(logs)
}

/// The run `barca logs` reads without a run id: the most recent; with `running`, the newest
/// run still in progress when there is one.
pub fn pick_run(runs: Vec<db::RunRecord>, running: bool) -> Option<db::RunRecord> {
    if running && let Some(run) = runs.iter().find(|r| r.status == "running") {
        return Some(run.clone());
    }
    runs.into_iter().next()
}

/// [`latest_run`], or with `running` the newest run in progress (else the most recent).
pub async fn run_to_read(
    cfg: &crate::config::ResolvedConfig,
    running: bool,
) -> Result<db::RunRecord, BarcaError> {
    if !running {
        return latest_run(cfg).await;
    }
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    let recent = db::get_recent_runs(&cfg.db_path, RUNNING_LOOKBACK).await?;
    match pick_run(recent, true) {
        Some(run) => Ok(run),
        None => latest_run(cfg).await,
    }
}

/// The run's record, or `None` for an unknown run.
pub async fn run_record(
    cfg: &crate::config::ResolvedConfig,
    run_id: &str,
) -> Result<Option<db::RunRecord>, BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::get_run(&cfg.db_path, run_id).await
}

/// How many recent runs `barca logs -f` looks through for one in progress.
pub const RUNNING_LOOKBACK: usize = 50;

// ─── stats ────────────────────────────────────────────────────────────────────

pub async fn stats(
    cfg: &crate::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &std::path::Path,
) -> Result<db::AssetStats, BarcaError> {
    let dag = build_dag(file_args, python).await?;

    let target_id = find_target_id(&dag, target_name)?;

    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::get_asset_stats(&cfg.db_path, &target_id).await
}

// ─── list_assets ──────────────────────────────────────────────────────────────

/// Build the DAG and return a summary of every node (id, kind, freshness, inputs).
/// Pure static analysis — no execution, no DB. Used by the server's `/assets` route.
pub async fn list_assets(
    file_args: &[String],
    python: &std::path::Path,
) -> Result<Vec<AssetSummary>, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    Ok(list_assets_from_dag(&dag))
}

/// Describe a previously validated graph without reading/importing source again.
pub fn list_assets_from_dag(dag: &crate::dag::Dag) -> Vec<AssetSummary> {
    dag.topo_order()
        .into_iter()
        .filter_map(|id| dag.get_node(id))
        .map(|node| {
            let mut inputs: Vec<String> = node
                .resolved_inputs
                .values()
                .chain(node.resolved_collected.values())
                .cloned()
                .collect();
            inputs.sort();
            inputs.dedup();
            AssetSummary {
                id: node.id.clone(),
                kind: node.kind(),
                freshness: node.extracted.freshness.clone(),
                inputs,
                env: node.extracted.env.clone(),
                file: node.extracted.source_file.clone(),
                line: node.extracted.line,
            }
        })
        .collect()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

pub(crate) fn filter_plan_to_subgraph(plan: ExecutionPlan, subgraph_ids: &[&str]) -> ExecutionPlan {
    let subgraph_set: std::collections::HashSet<&str> = subgraph_ids.iter().copied().collect();
    let exec_plan = ExecutionPlan {
        phases: plan
            .phases
            .into_iter()
            .map(|phase| {
                let filtered_streams: Vec<crate::planner::WorkerStream> = phase
                    .streams
                    .into_iter()
                    .map(|s| crate::planner::WorkerStream {
                        stream_id: s.stream_id,
                        steps: s
                            .steps
                            .into_iter()
                            .filter(|st| {
                                let base_id = st.step_id.base_id();
                                subgraph_set.contains(base_id)
                            })
                            .collect(),
                    })
                    .filter(|s| !s.steps.is_empty())
                    .collect();
                Phase {
                    reason: phase.reason,
                    streams: filtered_streams,
                }
            })
            .filter(|p| !p.streams.is_empty())
            .collect(),
        total_steps: 0,
    };
    ExecutionPlan {
        total_steps: exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|st| {
                if st.partition_keys.is_empty() {
                    1
                } else {
                    st.partition_keys.len()
                }
            })
            .sum(),
        ..exec_plan
    }
}

use crate::targets::skipped_tasks_note;
use crate::{cache::CachePolicy, results::ExplainResult};

/// `barca get|run --dry-run` — report what the command would do, without doing it.
///
/// Plans exactly as a real run does and sends every step through [`crate::cache::decide_step`], so the
/// prediction is the real run's decision. Nothing executes, no worker starts, and nothing is
/// written: no `.barca` directory is created and no run is recorded. A dry run cannot know
/// the key set of a dynamic partition (`partitions_from`) whose source has to run first, nor
/// what a sensor will return: a step reading a sensor is predicted from the sensor's last
/// recorded output, and is `unknown` when there is none. Those steps (and anything depending on
/// them) are reported as `unknown`.
#[allow(clippy::too_many_arguments)] // Mirrors the execution request, including per-run overrides.
pub async fn explain(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    command_label: &str,
    partition: Option<&crate::selection::PartitionSelection>,
    batch: &crate::batch::BatchOverride,
) -> Result<ExplainResult, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    batch.check(&dag).map_err(BarcaError::Usage)?;
    if command_label == "get"
        && target_names.is_empty()
        && let Some(note) = skipped_tasks_note(&dag, file_args)
    {
        crate::errln!("{note}");
    }
    let result = crate::execution::explain_dag(
        &dag,
        cfg,
        target_names,
        policy,
        command_label,
        partition,
        file_args,
    )
    .await?;
    let mut result = result;
    crate::batch::annotate(&dag, &mut result.steps, batch, python)?;
    if let Some(note) = crate::selection::explain_note(&result) {
        crate::errln!("{note}");
    }
    crate::warnings::print(&result.warnings);
    Ok(result)
}
