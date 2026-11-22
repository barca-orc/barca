//! Inspection commands: `plan`, `history`, `stats` and the node listing behind `barca list`.
//!
//! None of these run a step. They read the project's source (statically) or the metadata DB.

mod planned_nodes;
pub use planned_nodes::planned_run_nodes;

use crate::BarcaError;
use crate::db;
use crate::load::build_dag;
use crate::planner::{self, ResourceConfig};
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
    let sizing = crate::sizing(capacity, pool, None);
    let result = plan_selected(&dag, sizing, partition)?;
    crate::warnings::print(&result.warnings);
    crate::sizing::warn_over_capacity(&result.weights, result.capacity, &result.capacity_source);
    Ok(result)
}

/// Render a previously validated graph using the same planner as strict commands, for a pool
/// and capacity of `sizing` ([`crate::sizing()`]).
pub fn plan_from_dag(dag: &crate::dag::Dag, sizing: crate::sizing::Sizing) -> PlanResult {
    plan_selected(dag, sizing, None).expect("a plan without a selection")
}

/// [`plan_from_dag`] narrowed to `partition` (`barca plan --partition`), as a `get` with no
/// target narrows its plan; a usage error as for `get`.
pub(crate) fn plan_selected(
    dag: &crate::dag::Dag,
    sizing: crate::sizing::Sizing,
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
        weights: crate::sizing::planned_weights(&plan),
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

// ─── the metadata DB ──────────────────────────────────────────────────────────

pub use crate::db::open_metadata;

// ─── history ──────────────────────────────────────────────────────────────────

/// The most recent runs (newest first), at most `limit` of them (`None` = every run), plus the
/// total number of recorded runs so callers can report truncation.
pub async fn history(
    cfg: &crate::config::ResolvedConfig,
    limit: Option<usize>,
    filter: &crate::history_filter::HistoryFilter,
) -> Result<(Vec<db::RunRecord>, usize), BarcaError> {
    let read = crate::db::read_metadata(cfg, true).await?;
    history_at(read.path(), limit, filter).await
}

/// The newest `limit` runs of the database at `db_path` that `filter` keeps (every one when
/// `None`), and how many it keeps in all. The database keeps them, each filter from an index
/// ([`db::RunFilter`]): the cost is the runs kept, not the runs recorded. `--target` is the
/// exception: the database narrows the read to the runs whose target or job holds the node's
/// name, and each of those is then checked here.
pub async fn history_at(
    db_path: &str,
    limit: Option<usize>,
    filter: &crate::history_filter::HistoryFilter,
) -> Result<(Vec<db::RunRecord>, usize), BarcaError> {
    let kept = filter.run_filter();
    let total = db::count_runs_filtered(db_path, &kept).await?;
    if filter.target.is_none() {
        let runs = db::get_runs_filtered(db_path, limit.unwrap_or(total), &kept).await?;
        // Counted, then listed, each at its own moment beside a writer: a run recorded
        // between the two is listed, so it is counted (`total: 18` over 19 rows otherwise).
        let total = total.max(runs.len());
        return Ok((runs, total));
    }
    let mut runs = db::get_runs_filtered(db_path, total, &kept).await?;
    runs.retain(|run| filter.names(run));
    let total = runs.len();
    runs.truncate(limit.unwrap_or(total));
    Ok((runs, total))
}

/// The runs started in the last 24 hours that `filter` keeps, hour by hour, in this
/// environment ([`crate::run_list::RunActivity`]): `barca history --activity`.
pub async fn activity(
    cfg: &crate::config::ResolvedConfig,
    filter: &crate::history_filter::HistoryFilter,
) -> Result<crate::run_list::RunActivity, BarcaError> {
    let read = crate::db::read_metadata(cfg, true).await?;
    activity_at(read.path(), filter, chrono::Utc::now()).await
}

/// [`activity`] of the database at `db_path`, at `now`: the runs of the window, read once
/// ([`db::run_activity`]). The filter's own window is left out: the activity is always the
/// last 24 hours.
pub async fn activity_at(
    db_path: &str,
    filter: &crate::history_filter::HistoryFilter,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::run_list::RunActivity, BarcaError> {
    let since = crate::run_list::activity_since(now)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let facts = db::run_activity(db_path, &since, &filter.run_filter()).await?;
    Ok(crate::run_list::run_activity(
        &facts,
        &crate::run_list::ActivityFilter::of(filter),
        now,
    ))
}

/// What each run of `runs` that is in progress on this machine is working on now, by run id
/// ([`crate::run_list::RunNow`]): `now` of `barca history --status running` without a server.
///
/// A run in another process streams its steps to nobody, so this reads what it recorded: the
/// unfinished nodes of its job appear in the waiting count
/// ([`crate::run_list::unfollowed_run_now`]). A run whose owner cannot be
/// seen alive from here, one that recorded no job, and one whose files no longer parse have
/// no entry.
pub async fn history_now(
    cfg: &crate::config::ResolvedConfig,
    runs: &[db::RunRecord],
    python: &std::path::Path,
) -> std::collections::HashMap<String, crate::run_list::RunNow> {
    let mut out = std::collections::HashMap::new();
    let Ok(read) = crate::db::read_metadata(cfg, false).await else {
        return out;
    };
    let cfg = read.cfg();
    for alive in db::alive_running_runs(&cfg.db_path).await {
        let Some(run) = runs.iter().find(|r| r.run_id == alive.run_id) else {
            continue;
        };
        let targets: Vec<String> = match alive.job.as_str() {
            "" => continue,
            "all" => Vec::new(),
            ids => ids.split(',').map(str::to_string).collect(),
        };
        let Ok(dag) = crate::load::build_dag(&run.files, python).await else {
            continue;
        };
        let Ok(status) = crate::status::status_from_dag(cfg, &[], &dag, python, 0, false).await
        else {
            continue;
        };
        out.insert(
            alive.run_id,
            crate::run_list::unfollowed_run_now(
                &alive.command,
                &targets,
                &run.started_at,
                &status.nodes,
                &dag.groups,
            ),
        );
    }
    out
}

/// One run and the steps it recorded, for `barca history RUN_ID`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunDetail {
    pub run: db::RunRecord,
    pub steps: Vec<db::RunStep>,
    /// How many steps the run recorded, and whether `steps` lists fewer.
    #[serde(default)]
    pub steps_page: db::StepsPage,
    /// What the run's targets returned ([`crate::run_explanation::run_outputs`]): computed
    /// where the run is recorded, so a client prints the server's rows.
    #[serde(default)]
    pub outputs: Vec<crate::run_explanation::RunOutput>,
}

/// Record a running row as interrupted after the operator has verified it stopped.
/// This is the explicit write exception to ordinary, read-only history inspection.
pub async fn reconcile_run(
    cfg: &crate::config::ResolvedConfig,
    run_id: &str,
) -> Result<db::RunRecord, BarcaError> {
    open_metadata(cfg).await?;
    db::reconcile_run(&cfg.db_path, run_id).await
}

/// The most recent run (the newest row of `barca history`), for `barca logs` without a run id.
/// A usage error when no run is recorded in this environment.
pub(crate) async fn latest_run(
    cfg: &crate::config::ResolvedConfig,
) -> Result<db::RunRecord, BarcaError> {
    let read = crate::db::read_metadata(cfg, true).await?;
    db::get_recent_runs(read.path(), 1)
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
pub(crate) async fn run_log_lines(
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

/// The newest `n` lines a run in progress has spooled (#925): what `GET /runs/{run_id}` shows
/// of its logs until the run records them. Empty when it has spooled none.
pub fn live_log_tail(db_path: &str, run_id: &str, n: usize) -> Vec<db::LogEntry> {
    let spool = crate::run_logs::spool_path(db_path, run_id);
    let Ok((lines, _)) = crate::run_logs::read_spool(&spool, 0) else {
        return Vec::new();
    };
    let skip = lines.len().saturating_sub(n);
    lines
        .into_iter()
        .enumerate()
        .skip(skip)
        .map(|(seq, line)| db::LogEntry::at(seq, line))
        .collect()
}

pub use crate::history_filter::step_matches;

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
    let read = crate::db::read_metadata(cfg, true).await?;
    logs_at(&cfg.db_path, read.path(), run_id, step, stream).await
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
    let queued = crate::serve::run_queue::list(read_path)
        .await?
        .iter()
        .any(|r| r.run_id == run_id);
    if db::get_run(read_path, run_id).await?.is_none() && !queued {
        return Err(BarcaError::Usage(format!(
            "run '{run_id}' was not found; use barca history --json to find a run id"
        )));
    }
    let previous = db::runs::previous_attempts(read_path, run_id).await?;
    let mut logs = Vec::new();
    for attempt in &previous {
        logs.extend(run_log_lines(db_path, read_path, &attempt.run_id).await?);
        logs.push(db::LogEntry::at(
            0,
            crate::run_logs::RecordedLine::barca(format!(
                "attempt {} stopped: {} (logs: {})",
                attempt.attempt, attempt.stop_reason, attempt.run_id
            )),
        ));
    }
    if !previous.is_empty() {
        logs.push(db::LogEntry::at(
            0,
            crate::run_logs::RecordedLine::barca(format!("attempt {}", previous.len() + 1)),
        ));
    }
    // A queued request can still have the predecessor's spool; it is already archived.
    if !queued || db::get_run(read_path, run_id).await?.is_some() {
        logs.extend(run_log_lines(db_path, read_path, run_id).await?);
    }
    for (seq, line) in logs.iter_mut().enumerate() {
        line.seq = seq as i64;
    }
    if let Some(step) = step {
        let mut recorded = db::get_run_steps(read_path, run_id).await?;
        for attempt in &previous {
            recorded.extend(db::get_run_steps(read_path, &attempt.run_id).await?);
        }
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
    let read = crate::db::read_metadata(cfg, true).await?;
    let recent = db::get_recent_runs(read.path(), RUNNING_LOOKBACK).await?;
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
    let read = crate::db::read_metadata(cfg, true).await?;
    db::get_run(read.path(), run_id).await
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

    let read = crate::db::read_metadata(cfg, true).await?;
    db::get_asset_stats(read.path(), &target_id).await
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

pub use crate::results::list_assets_from_dag;

// ─── Helpers ─────────────────────────────────────────────────────────────────

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
    let result = crate::predict::explain_dag(
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
