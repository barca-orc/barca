//! The read commands of this machine that start from the project's files: load the graph (the
//! Python frontend, `load::build_dag`), then ask a read model or a decision, which take a graph
//! and never load one (#1056, C6). `barca-analysis` and `barca-decide` do not depend on the
//! frontend; a server, which holds the graph it loaded, calls their `*_from_dag` functions
//! itself.

use crate::BarcaError;
use crate::partition_grid::{
    PartitionGrid, PartitionKeyDetail, partition_grid, partition_key_detail, partitioned_target,
};
use crate::schedule::{ScheduleInfo, Zone, describe_schedule_from_dag};
use crate::status::{StatusResult, status_from_dag_with_membership};
use crate::text::trace_since;

/// Gather status for every node in scope (the targets' upstream cones, or the whole DAG when
/// `target_names` is empty). `sample` > 0 adds up to that many sample rows to json/parquet
/// shapes; `shape` = false skips the artifact reader entirely.
pub async fn status(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<StatusResult, BarcaError> {
    let dag = crate::load::build_dag(file_args, python).await?;
    let mut result =
        status_from_dag_with_membership(cfg, target_names, &dag, python, sample, shape)
            .await?
            .0;
    // No scheduler runs here: a schedule's state is what the metadata DB holds.
    crate::health::attach_from_db_with_schedules(
        &mut result,
        &dag,
        cfg,
        &crate::schedule::Zone::Local,
        chrono::Utc::now(),
    )
    .await?;
    Ok(result)
}

/// SQL reuses this prediction instead of loading the DAG or interpreting keys again.
pub async fn status_with_membership(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    sample: usize,
    shape: bool,
) -> Result<
    (
        StatusResult,
        crate::cache::ExpandedMembership,
        crate::dag::Dag,
    ),
    BarcaError,
> {
    let t0 = std::time::Instant::now();
    let dag = crate::load::build_dag(file_args, python).await?;
    trace_since("status: load", t0);
    let (mut result, membership) =
        status_from_dag_with_membership(cfg, target_names, &dag, python, sample, shape).await?;
    crate::health::attach_from_db(
        &mut result,
        &dag,
        cfg,
        &crate::schedule::Zone::Local,
        chrono::Utc::now(),
    )
    .await?;
    Ok((result, membership, dag))
}

/// `barca status --rollup` on this machine: the status of the targets' upstream cones (the
/// whole graph without targets), classified with the evidence the metadata DB holds, summed.
/// No run is tracked here, so `running` is 0 everywhere.
pub async fn rollup(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
) -> Result<crate::rollup::Rollup, BarcaError> {
    let dag = crate::load::build_dag(file_args, python).await?;
    let mut result =
        crate::status::status_from_dag(cfg, target_names, &dag, python, 0, false).await?;
    crate::health::rollup_from_db(
        &mut result,
        &dag,
        cfg,
        &crate::schedule::Zone::Local,
        chrono::Utc::now(),
    )
    .await
}

/// `barca status NODE --keys` / `--grid` on this machine: every key of the node `name` names
/// in the project's files.
pub async fn grid_for_target(
    cfg: &crate::config::ResolvedConfig,
    name: &str,
    file_args: &[String],
    python: &std::path::Path,
) -> Result<PartitionGrid, BarcaError> {
    let dag = crate::load::build_dag(file_args, python).await?;
    let id = partitioned_target(&dag, name)?;
    partition_grid(cfg, &dag, &id).await
}

/// `barca status NODE --key KEY` on this machine; `None` when the node has no such key.
pub async fn key_for_target(
    cfg: &crate::config::ResolvedConfig,
    name: &str,
    file_args: &[String],
    python: &std::path::Path,
    key: &str,
    sample: usize,
) -> Result<Option<PartitionKeyDetail>, BarcaError> {
    let dag = crate::load::build_dag(file_args, python).await?;
    let id = partitioned_target(&dag, name)?;
    partition_key_detail(cfg, &dag, python, &id, key, sample).await
}

/// Enumerate scheduled jobs from source and compute each one's next fire time.
/// Pure static analysis — used by the `barca list` CLI, no running server.
pub async fn describe_schedule(files: &[String], python: &std::path::Path) -> Vec<ScheduleInfo> {
    describe_schedule_in(files, python, &Zone::Local).await
}

/// Describe jobs using the configured server timezone.
async fn describe_schedule_in(
    files: &[String],
    python: &std::path::Path,
    zone: &Zone,
) -> Vec<ScheduleInfo> {
    match crate::load::build_dag(files, python).await {
        Ok(dag) => describe_schedule_from_dag(Some(&dag), zone),
        Err(error) => {
            crate::errln!("[barca] scheduler disabled: failed to analyze DAG: {error}");
            Vec::new()
        }
    }
}
