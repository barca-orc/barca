//! Read-only commands: `plan`, `history`, `stats` and the node listing behind `barca list`.
//!
//! None of these run a step. They read the project's source (statically) or the metadata DB.

use crate::BarcaError;
use crate::commands::find_target_id;
use crate::db;
use crate::load::build_dag;
use crate::planner::Phase;
use crate::planner::{self, ExecutionPlan, ResourceConfig};
use crate::results::{AssetSummary, PlanPhase, PlanPhaseReason, PlanResult, PlanStream};
use std::collections::HashMap;

// ─── plan ────────────────────────────────────────────────────────────────────

pub async fn plan(
    file_args: &[String],
    python: &std::path::Path,
) -> Result<PlanResult, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    let config = ResourceConfig {
        pool_size: 10,
        concurrency_groups: HashMap::new(),
    };
    let plan = planner::plan_from_dag(&dag, &config);

    let warnings = crate::warnings::for_plan(&dag, &plan);
    crate::warnings::print(&warnings);
    Ok(PlanResult {
        warnings,
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
    })
}

// ─── history ──────────────────────────────────────────────────────────────────

/// The most recent runs (newest first), at most `limit` of them (`None` = every run), plus the
/// total number of recorded runs so callers can report truncation.
pub async fn history(
    cfg: &crate::config::ResolvedConfig,
    limit: Option<usize>,
) -> Result<(Vec<db::RunRecord>, usize), BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    let total = db::count_runs(&cfg.db_path).await?;
    let runs = db::get_recent_runs(&cfg.db_path, limit.unwrap_or(total)).await?;
    Ok((runs, total))
}

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
    let summaries = dag
        .topo_order()
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
            }
        })
        .collect();
    Ok(summaries)
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
