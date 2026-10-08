//! Engine commands — get, plan, history, stats. Return typed results; callers handle display.
//!
//! Every command is an `async fn` that runs on the caller's runtime: the CLI
//! builds one runtime in `main()`, the server `.await`s these directly. No
//! runtime is ever constructed in this crate; genuinely blocking work (source
//! parsing, dynamic-partition subprocesses) runs via `spawn_blocking`.

use crate::BarcaError;
use crate::cache;
use crate::dag::Dag;
use crate::db;
use crate::dispatch;
use crate::dispatch::OutputRef;
use crate::planner::{self, ExecutionPlan, Phase, ResourceConfig};
use crate::recover::{self, Work};
use crate::state_sync;
use crate::transfer::{ArtifactLayout, TransferClient};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

// Moved out of this module; re-exported so existing `commands::` paths keep working until the
// callers move (tracked in the cleanup plan).
pub use crate::load::{build_dag, source_dir};
use crate::queries::filter_plan_to_subgraph;
pub use crate::queries::{history, list_assets, plan, stats};
pub use crate::results::*;

/// Format seconds as a fixed-width time string for progress display.
/// Always 8 chars wide: "   5s   ", " 2m 30s ", " 1h 05m ", "2d 03h  "
fn fmt_eta(secs: f64) -> String {
    let s = secs.round() as u64;
    if s < 60 {
        format!("{s:>4}s   ")
    } else if s < 3600 {
        format!("{:>2}m {:02}s ", s / 60, s % 60)
    } else if s < 86400 {
        format!("{:>2}h {:02}m ", s / 3600, (s % 3600) / 60)
    } else {
        let d = s / 86400;
        format!("{d:>2}d {:02}h  ", (s % 86400) / 3600)
    }
}

/// The progress total, grown to cover steps the plan did not count (the children a `parallel()`
/// call fans out to complete as extra steps). Keeps `completed <= total` for the counters and
/// the ETA subtraction.
fn reconcile_total(total_steps: usize, completed_steps: usize) -> usize {
    total_steps.max(completed_steps)
}

/// Resolve the target name to a node id (or `None` for the whole DAG), enforcing that `barca get`
/// Does a target `name` identify node `id`? A name is a function name (`deploy`), a full id
/// (`pipeline.py:deploy`), or a path-suffixed id (`p.py:deploy` for `sub/p.py:deploy`). It
/// matches only at a `:` or `/` boundary, never as the tail of a longer name, so `deploy` does
/// not select `prod_deploy`.
pub(crate) fn target_name_matches(id: &str, name: &str) -> bool {
    if id == name {
        return true;
    }
    match id.strip_suffix(name) {
        Some(prefix) => prefix.ends_with(':') || (name.contains(':') && prefix.ends_with('/')),
        None => false,
    }
}

/// The single node a target name identifies. No match is `AssetNotFound`; several matches (the
/// same function name in two files) is a usage error that lists the full ids to choose from.
pub(crate) fn find_target_id(dag: &Dag, name: &str) -> Result<String, BarcaError> {
    let matches: Vec<&str> = dag
        .topo_order()
        .into_iter()
        .filter(|id| target_name_matches(id, name))
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => {
            let available: Vec<&str> = dag.topo_order();
            Err(BarcaError::AssetNotFound(
                name.to_string(),
                available.join(", "),
            ))
        }
        many => Err(BarcaError::Usage(format!(
            "'{name}' matches more than one node: {}. Name one by its full id, e.g. `{}`",
            many.join(", "),
            many[0]
        ))),
    }
}

/// targets assets and `barca run` targets tasks.
fn resolve_target(
    dag: &Dag,
    target_name: Option<&str>,
    command_label: &str,
) -> Result<Option<String>, BarcaError> {
    match target_name {
        Some(name) => {
            let id = find_target_id(dag, name)?;
            // Enforce get/run semantics: `barca get` is for assets, `barca run` is for tasks.
            if let Some(node) = dag.get_node(&id) {
                let kind = node.kind();
                if command_label == "get" && kind == crate::NodeKind::Task {
                    return Err(BarcaError::Usage(format!(
                        "'{name}' is a task — use `barca run` instead"
                    )));
                }
                if command_label == "run" && kind == crate::NodeKind::Asset {
                    return Err(BarcaError::Usage(format!(
                        "'{name}' is an asset — use `barca get` instead"
                    )));
                }
            }
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

/// Resolve several target names (`barca run a,b`) to node ids, each checked like a single
/// target, before anything runs. Returns `(name as given, node id)` pairs; a name resolving to an
/// id already listed is dropped, so `a,a` is one target.
fn resolve_targets(
    dag: &Dag,
    names: &[String],
    command_label: &str,
) -> Result<Vec<(String, String)>, BarcaError> {
    let mut out: Vec<(String, String)> = Vec::new();
    for name in names {
        if let Some(id) = resolve_target(dag, Some(name), command_label)?
            && !out.iter().any(|(_, seen)| *seen == id)
        {
            out.push((name.clone(), id));
        }
    }
    Ok(out)
}

/// The plan for these targets: the union of their cones, planned once, so an upstream step
/// shared by several targets appears (and runs) once. No targets means everything the command
/// covers: for `get`, every asset and sensor (tasks are skipped: get is for assets, run is for
/// tasks); for anything else (`status`), the whole DAG.
pub(crate) fn plan_for_targets(
    dag: &Dag,
    target_ids: &[&str],
    config: &ResourceConfig,
    command_label: &str,
) -> ExecutionPlan {
    let full_plan = planner::plan_from_dag(dag, config);
    if !target_ids.is_empty() {
        filter_plan_to_subgraph(full_plan, &dag.subgraph_many(target_ids))
    } else if command_label == "get" {
        // A task is never upstream of an asset or sensor, so this is closed under upstream.
        let gettable: Vec<&str> = dag
            .topo_order()
            .into_iter()
            .filter(|id| !is_task(dag, id))
            .collect();
        filter_plan_to_subgraph(full_plan, &gettable)
    } else {
        full_plan
    }
}

fn is_task(dag: &Dag, id: &str) -> bool {
    dag.get_node(id)
        .is_some_and(|n| n.kind() == crate::NodeKind::Task)
}

/// The stderr note for `barca get <files>` with no target when the files define tasks: which
/// tasks were skipped and how to run one. `None` when there is no task to mention.
fn skipped_tasks_note(dag: &Dag, file_args: &[String]) -> Option<String> {
    let order = dag.topo_order();
    let tasks: Vec<&str> = order
        .iter()
        .filter(|id| is_task(dag, id))
        .map(|id| short_name(id))
        .collect();
    let first = tasks.first()?;
    let run_hint = format!("barca run {first} {}", file_args.join(" "));
    let listed = tasks.join(", ");
    Some(if tasks.len() == order.len() {
        format!(
            "[barca] nothing to get: no assets or sensors, only tasks ({listed}). \
             `barca get` without a target never runs tasks; run one with: {run_hint}"
        )
    } else {
        format!(
            "[barca] skipped {} task{} ({listed}): `barca get` without a target materializes \
             assets only. Run a task with: {run_hint}",
            tasks.len(),
            if tasks.len() == 1 { "" } else { "s" },
        )
    })
}

/// The failed node upstream of `base_id`, if any: it blocks `base_id` from running. Used when
/// several targets run together, so a failure stops only the targets that need it.
fn blocking_failure<'a>(
    dag: &'a Dag,
    base_id: &str,
    failed: &std::collections::HashSet<String>,
) -> Option<&'a str> {
    if failed.is_empty() {
        return None;
    }
    dag.subgraph(base_id)
        .into_iter()
        .find(|up| *up != base_id && failed.contains(*up))
}

/// The output of `target_id` in this run (the first partition, by key, for a partitioned one).
fn output_for(target_id: &str, all_outputs: &HashMap<String, OutputRef>) -> Option<OutputRef> {
    all_outputs.get(target_id).cloned().or_else(|| {
        let prefix = format!("{target_id}[");
        let mut matches: Vec<_> = all_outputs
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .collect();
        matches.sort_by_key(|(k, _)| (*k).clone());
        matches.first().map(|(_, v)| (*v).clone())
    })
}

/// How each target of a multi-target run ended: `success` with its output, or `failed` with
/// the step that failed (the target itself or something upstream of it) and its error.
fn target_outcomes(
    dag: &Dag,
    targets: &[(String, String)],
    all_outputs: &HashMap<String, OutputRef>,
    failures: &[dispatch::StepFailure],
) -> Vec<(String, TargetOutcome)> {
    let base = |f: &dispatch::StepFailure| crate::StepId::parse(&f.node_id).base_id().to_string();
    targets
        .iter()
        .map(|(name, tid)| {
            // Prefer the target's own failure, then the first failed step upstream of it.
            let failure = failures.iter().find(|f| base(f) == *tid).or_else(|| {
                dag.subgraph(tid)
                    .into_iter()
                    .find_map(|id| failures.iter().find(|f| base(f) == id))
            });
            let outcome = match (failure, output_for(tid, all_outputs)) {
                (Some(f), _) => TargetOutcome {
                    status: "failed".to_string(),
                    final_output: None,
                    error: Some(f.error.message.clone()),
                    failed_node: Some(f.node_id.clone()),
                },
                (None, Some(out)) => TargetOutcome {
                    status: "success".to_string(),
                    final_output: Some(out),
                    error: None,
                    failed_node: None,
                },
                (None, None) => TargetOutcome {
                    status: "failed".to_string(),
                    final_output: None,
                    error: Some("did not run".to_string()),
                    failed_node: None,
                },
            };
            (name.clone(), outcome)
        })
        .collect()
}

// ─── Cache decisions ─────────────────────────────────────────────────────────
//
// One function decides what happens to a step; a real run and `--dry-run` both call it, so the
// dry run cannot drift from what a run would do.

/// Why a step runs instead of being served from cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunReason {
    Task,
    Sensor,
    NoCache,
    Refresh,
    /// Downstream of an asset named in `--refresh` (the cascade); `root` is that asset.
    RefreshCascade {
        root: String,
    },
    RefreshAll,
    NotMaterialized,
    /// It has a cached result, but the artifact is gone and something needs to read it
    /// (see [`crate::recover`]).
    ArtifactMissing,
}

impl RunReason {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            RunReason::Task => "task",
            RunReason::Sensor => "sensor",
            RunReason::NoCache => "no_cache",
            RunReason::Refresh => "refresh",
            RunReason::RefreshCascade { .. } => "refresh_cascade",
            RunReason::RefreshAll => "refresh_all",
            RunReason::NotMaterialized => "not_materialized",
            RunReason::ArtifactMissing => "artifact_missing",
        }
    }

    pub(crate) fn detail(&self) -> String {
        match self {
            RunReason::Task => "tasks always re-run".to_string(),
            RunReason::Sensor => "sensors always re-run".to_string(),
            RunReason::NoCache => "--no-cache".to_string(),
            RunReason::Refresh => "named in --refresh".to_string(),
            RunReason::RefreshCascade { root } => {
                format!("downstream of refreshed '{root}'")
            }
            RunReason::RefreshAll => "--refresh-all".to_string(),
            RunReason::NotMaterialized => {
                "no cached result for this code and these inputs".to_string()
            }
            RunReason::ArtifactMissing => {
                "its cached result is still valid, but the artifact file is missing and \
                 something needs to read it"
                    .to_string()
            }
        }
    }
}

enum Decision {
    Run(RunReason),
    Cached {
        oref: OutputRef,
        /// The refreshed asset this cached step depends on, if any.
        stale_root: Option<String>,
    },
    Partitioned {
        cached: Vec<(String, OutputRef)>,
        missing: Vec<crate::model::PartitionKey>,
    },
}

/// Per-run state the decisions accumulate: run hashes (also used to persist results), assets
/// refreshed in this run, and cached assets downstream of a refreshed one.
#[derive(Default)]
struct DecideState {
    run_hashes: HashMap<String, String>,
    refreshed_ids: std::collections::HashSet<String>,
    /// Refreshed asset -> the `--refresh` name it was refreshed for (itself, or the named
    /// upstream it cascaded from).
    cascade_roots: HashMap<String, String>,
    stale_cached: HashMap<String, String>,
    /// Sensor step (display id) -> content hash of its output, folded into each consumer's run
    /// hash (#183). A real run fills it as sensors finish (they run in an earlier phase than
    /// their consumers); a dry run seeds it from each sensor's last recorded output.
    sensor_outputs: HashMap<String, String>,
}

/// The sensors `step` reads directly (base ids).
fn sensor_inputs<'a>(dag: &Dag, step: &'a crate::planner::StreamStep) -> Vec<&'a str> {
    let mut out: Vec<&str> = step
        .inputs
        .values()
        .map(|up| up.split('[').next().unwrap_or(up))
        .filter(|up| {
            dag.get_node(up)
                .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// True when `sensor` (a base id) has an output hash for this run (any partition of it).
fn has_sensor_output(state: &DecideState, sensor: &str) -> bool {
    let prefix = format!("{sensor}[");
    state
        .sensor_outputs
        .keys()
        .any(|k| k == sensor || k.starts_with(&prefix))
}

async fn lookup_in(
    cache: Option<&db::CacheReader>,
    node_id: &str,
    run_hash: &str,
) -> Option<OutputRef> {
    lookup_cached(cache?, node_id, run_hash).await
}

/// Decide what happens to `step`. Steps must be visited in plan order: in-phase upstream run
/// hashes are already in `state` when a consumer is hashed, so check-time and persist-time hashes
/// are identical. `cache` is `None` when there is no metadata DB yet (nothing is cached).
async fn decide_step(
    dag: &Dag,
    policy: &CachePolicy,
    no_cache: bool,
    cache: Option<&db::CacheReader>,
    state: &mut DecideState,
    step: &crate::planner::StreamStep,
) -> (crate::planner::StreamStep, Decision) {
    let base_id = step.step_id.base_id();
    let display_id = step.step_id.display();
    let base_node = dag.get_node(base_id);
    let def_hash = base_node.map(|n| n.definition_hash.as_str()).unwrap_or("");
    // Declared env (`@asset(env=[...])`) is read now, at plan time, and folded into the run hash.
    let env_input = base_node
        .map(|n| crate::envdeps::hash_input(&crate::envdeps::resolve(&n.extracted.env)))
        .unwrap_or(None);

    // Run hashes for EVERY step (sensors, tasks, refreshed and partitioned steps too): they
    // content-address artifacts and key persistence.
    let mut step = step.clone();
    if step.partition_keys.is_empty() {
        let partition_key = if step.step_id.partition.is_empty() {
            None
        } else {
            Some(step.step_id.partition.suffix())
        };
        let run_h = cache::compute_run_hash(
            def_hash,
            partition_key.as_deref(),
            step.inputs.values(),
            &state.run_hashes,
            &state.sensor_outputs,
            env_input.as_deref(),
        );
        state.run_hashes.insert(display_id.clone(), run_h.clone());
        step.run_hashes.insert(display_id.clone(), run_h);
    } else {
        for pk in &step.partition_keys {
            let pdisplay = pk.display_id(&step.step_id.base);
            let run_h = cache::compute_run_hash(
                def_hash,
                Some(&pk.suffix()),
                step.inputs.values(),
                &state.run_hashes,
                &state.sensor_outputs,
                env_input.as_deref(),
            );
            state.run_hashes.insert(pdisplay.clone(), run_h.clone());
            step.run_hashes.insert(pdisplay, run_h);
        }
    }

    let kind = base_node.map(|n| n.kind());
    // Sensors and tasks always re-run — never cached.
    match kind {
        Some(crate::NodeKind::Task) => return (step, Decision::Run(RunReason::Task)),
        Some(crate::NodeKind::Sensor) => return (step, Decision::Run(RunReason::Sensor)),
        _ => {}
    }
    if no_cache {
        return (step, Decision::Run(RunReason::NoCache));
    }

    // Refresh policy (`barca run`): force-rerun assets named in the refresh set and, when
    // cascading, every asset downstream of one (plan order puts upstream steps first, so a
    // refreshed upstream is already in `cascade_roots` when its consumers are decided).
    let is_asset = kind == Some(crate::NodeKind::Asset);
    let refresh = match policy {
        CachePolicy::CacheAware => None,
        CachePolicy::RefreshAll => is_asset.then_some(RunReason::RefreshAll),
        CachePolicy::RefreshSelective { names, cascade } => {
            if !is_asset {
                None
            } else if names.iter().any(|name| refresh_name_matches(base_id, name)) {
                Some(RunReason::Refresh)
            } else if *cascade {
                step.inputs.values().find_map(|up| {
                    let up_base = up.split('[').next().unwrap_or(up);
                    state
                        .cascade_roots
                        .get(up_base)
                        .map(|root| RunReason::RefreshCascade { root: root.clone() })
                })
            } else {
                None
            }
        }
    };
    if let Some(reason) = refresh {
        let root = match &reason {
            RunReason::RefreshCascade { root } => root.clone(),
            _ => short_name(base_id).to_string(),
        };
        state.refreshed_ids.insert(base_id.to_string());
        state.cascade_roots.insert(base_id.to_string(), root);
        return (step, Decision::Run(reason));
    }

    // A consumer of a sensor is only cache-checked against that sensor's output. The planner
    // runs sensors in an earlier phase, and a dry run reports such a step as unknown before it
    // gets here, so this is a safety net: without the output, never serve from cache.
    if sensor_inputs(dag, &step)
        .iter()
        .any(|s| !has_sensor_output(state, s))
    {
        return (step, Decision::Run(RunReason::NotMaterialized));
    }

    // Partitioned steps are checked per key: each partition has its own run hash, so keys
    // with a successful materialization are served from cache and only the rest execute.
    if !step.partition_keys.is_empty() {
        let mut cached = Vec::new();
        let mut missing = Vec::new();
        for pk in &step.partition_keys {
            let pdisplay = pk.display_id(&step.step_id.base);
            let run_h = step
                .run_hashes
                .get(&pdisplay)
                .cloned()
                .expect("partitioned step has a precomputed run hash per key");
            match lookup_in(cache, &pdisplay, &run_h).await {
                Some(oref) => cached.push((pdisplay, oref)),
                None => missing.push(pk.clone()),
            }
        }
        return (step, Decision::Partitioned { cached, missing });
    }

    let run_h = step
        .run_hashes
        .get(&display_id)
        .cloned()
        .expect("unpartitioned step has a precomputed run hash");
    match lookup_in(cache, &display_id, &run_h).await {
        None => (step, Decision::Run(RunReason::NotMaterialized)),
        Some(oref) => {
            // Cached, but does it depend on something refreshed in this run?
            let stale_root = step.inputs.values().find_map(|up| {
                let up_base = up.split('[').next().unwrap_or(up);
                if state.refreshed_ids.contains(up_base) {
                    Some(short_name(up_base).to_string())
                } else {
                    state.stale_cached.get(up_base).cloned()
                }
            });
            if let Some(root) = &stale_root {
                state.stale_cached.insert(base_id.to_string(), root.clone());
            }
            (step, Decision::Cached { oref, stale_root })
        }
    }
}

/// A partitioned asset plans one step per key; report it as one line with a partition summary.
fn merge_partition_reports(reports: Vec<StepReport>) -> Vec<StepReport> {
    let mut out: Vec<StepReport> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in reports {
        let Some(p) = r.partitions.clone() else {
            out.push(r);
            continue;
        };
        match index.get(&r.id).copied() {
            None => {
                index.insert(r.id.clone(), out.len());
                out.push(r);
            }
            Some(i) => {
                let m = &mut out[i];
                let mp = m.partitions.get_or_insert_with(PartitionSummary::default);
                mp.total += p.total;
                mp.cached += p.cached;
                mp.will_run += p.will_run;
                for k in p.will_run_keys {
                    if mp.will_run_keys.len() < 20 {
                        mp.will_run_keys.push(k);
                    }
                }
                if m.reason.is_none() {
                    m.reason = r.reason;
                    m.detail = r.detail;
                }
            }
        }
    }
    // Recompute the verdict of merged lines from their totals.
    for r in &mut out {
        let Some(p) = &r.partitions else { continue };
        let verdict = |dry: bool| match (p.cached, p.will_run) {
            (_, 0) => "cached",
            (0, _) => {
                if dry {
                    "run"
                } else {
                    "ran"
                }
            }
            _ => "partial",
        };
        if r.action.is_some() {
            r.action = Some(verdict(true).to_string());
        } else {
            r.status = Some(verdict(false).to_string());
        }
    }
    out
}

fn stale_warning(display_id: &str, root: &str, dry: bool) -> String {
    let id = short_name(display_id);
    let (served, reflect) = if dry {
        ("will be served", "will not reflect")
    } else {
        ("was served", "does not reflect")
    };
    format!(
        "'{id}' {served} from cache but depends on refreshed '{root}', so it {reflect} the \
         refresh. Drop --no-cascade, add it to --refresh (for example --refresh {root},{id}) or \
         use --refresh-all."
    )
}

/// The declared env values of `base_id` as reported in output (secrets redacted, unset = null),
/// or `None` when the node declares no env.
fn env_report(
    dag: &Dag,
    base_id: &str,
) -> Option<std::collections::BTreeMap<String, Option<String>>> {
    let node = dag.get_node(base_id)?;
    if node.extracted.env.is_empty() {
        return None;
    }
    Some(crate::envdeps::report(&crate::envdeps::resolve(
        &node.extracted.env,
    )))
}

/// The `--agent` step-line suffix for `node_id`'s declared env (empty when none is declared).
fn env_suffix(dag: &Dag, node_id: &str) -> String {
    dag.get_node(crate::StepId::parse(node_id).base_id())
        .map(|n| crate::envdeps::agent_suffix(&crate::envdeps::resolve(&n.extracted.env)))
        .unwrap_or_default()
}

fn kind_str(kind: Option<crate::NodeKind>) -> String {
    match kind {
        Some(crate::NodeKind::Asset) => "asset",
        Some(crate::NodeKind::Task) => "task",
        Some(crate::NodeKind::Sensor) => "sensor",
        None => "unknown",
    }
    .to_string()
}

/// Build the report line for one decided step. `dry` selects the vocabulary: a dry run says what
/// will happen (`action`), a real run says what happened (`status`).
fn report_for(
    dag: &Dag,
    step: &crate::planner::StreamStep,
    decision: &Decision,
    dry: bool,
) -> StepReport {
    let display_id = step.step_id.display();
    let base_id = step.step_id.base_id();
    let mut r = StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        env: env_report(dag, base_id),
        ..Default::default()
    };
    let (word_cached, word_run, word_partial) = if dry {
        ("cached", "run", "partial")
    } else {
        ("cached", "ran", "partial")
    };
    let verdict = match decision {
        Decision::Run(reason) => {
            r.reason = Some(reason.code().to_string());
            r.detail = Some(reason.detail());
            r.run_hash = step.run_hashes.get(&display_id).cloned();
            if step.partition_keys.is_empty() {
                word_run
            } else {
                // A forced (task/refresh/no-cache) partitioned step runs every key.
                r.partitions = Some(PartitionSummary {
                    total: step.partition_keys.len(),
                    cached: 0,
                    will_run: step.partition_keys.len(),
                    will_run_keys: step
                        .partition_keys
                        .iter()
                        .take(20)
                        .map(|k| k.suffix())
                        .collect(),
                });
                word_run
            }
        }
        Decision::Cached { oref, stale_root } => {
            r.run_hash = step.run_hashes.get(&display_id).cloned();
            r.artifact = Some(oref.path.clone());
            if let Some(root) = stale_root {
                r.warning = Some(stale_warning(&display_id, root, dry));
            }
            word_cached
        }
        Decision::Partitioned { cached, missing } => {
            let total = cached.len() + missing.len();
            r.partitions = Some(PartitionSummary {
                total,
                cached: cached.len(),
                will_run: missing.len(),
                will_run_keys: missing.iter().take(20).map(|k| k.suffix()).collect(),
            });
            if !missing.is_empty() {
                r.reason = Some(RunReason::NotMaterialized.code().to_string());
                r.detail = Some(RunReason::NotMaterialized.detail());
            }
            match (cached.is_empty(), missing.is_empty()) {
                (_, true) => word_cached,
                (true, false) => word_run,
                (false, false) => word_partial,
            }
        }
    };
    if dry {
        r.action = Some(verdict.to_string());
    } else {
        r.status = Some(verdict.to_string());
    }
    r
}

/// How a run ended, for its last stderr line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunOutcome {
    Done,
    Failed,
    Cancelled,
}

/// The last progress line of a run that executed steps, with or without `--agent`:
/// `[barca] N/M steps | done in Xs` (`failed in`, `cancelled after`).
fn end_of_run_line(completed: usize, total: usize, secs: f64, outcome: RunOutcome) -> String {
    let how = match outcome {
        RunOutcome::Done => "done in",
        RunOutcome::Failed => "failed in",
        RunOutcome::Cancelled => "cancelled after",
    };
    format!("[barca] {completed}/{total} steps | {how} {secs:.1}s")
}

/// The `--agent` line for a step served from cache: `[barca] step:<id> cached`, with its declared
/// env.
fn cached_step_line(dag: &Dag, display_id: &str) -> String {
    format!(
        "[barca] step:{display_id} cached{}",
        env_suffix(dag, display_id)
    )
}

/// The `--agent` line for a step that raised: `[barca] step:<id> failed: <first line of the
/// error>`, beside the `completed` and `cached` lines.
fn failed_step_line(node_id: &str, error: &str) -> String {
    let first = error
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown error");
    format!("[barca] step:{node_id} failed: {first}")
}

/// Print a note above the progress bar, or to stderr when no bar is visible. A hidden bar
/// (stderr is not a terminal, as when an agent or CI drives barca) silently swallows
/// `ProgressBar::println`, which used to make warnings vanish.
fn note(pb: &Option<indicatif::ProgressBar>, msg: &str) {
    match pb {
        Some(bar) if !bar.is_hidden() => bar.println(msg),
        _ => eprintln!("{msg}"),
    }
}

/// Does a `--refresh` name (a function name, or a full `file.py:name` id) identify `node_id`?
pub(crate) fn refresh_name_matches(node_id: &str, name: &str) -> bool {
    node_id == name || node_id.ends_with(&format!(":{name}"))
}

/// The function name of a node id (`pipeline.py:src` -> `src`, `pipeline.py:p[k=v]` -> `p`).
pub(crate) fn short_name(node_id: &str) -> &str {
    let base = node_id.split('[').next().unwrap_or(node_id);
    base.rsplit(':').next().unwrap_or(base)
}

/// Fail before running anything when `--refresh` names something that is not an upstream
/// asset of the target: a typo must not be a silent no-op. `get` targets are assets, so with
/// `include_targets` a target may name itself; `run` targets are tasks and never can.
fn validate_refresh_names(
    dag: &Dag,
    target_ids: &[&str],
    names: &[String],
    include_targets: bool,
) -> Result<(), BarcaError> {
    let cone: Vec<&str> = if target_ids.is_empty() {
        dag.topo_order()
    } else {
        dag.subgraph_many(target_ids)
    };
    let assets: Vec<&str> = cone
        .into_iter()
        .filter(|id| include_targets || !target_ids.contains(id))
        .filter(|id| {
            dag.get_node(id)
                .is_some_and(|n| n.kind() == crate::NodeKind::Asset)
        })
        .collect();
    for name in names {
        if !assets.iter().any(|id| refresh_name_matches(id, name)) {
            let valid: Vec<&str> = assets.iter().map(|id| short_name(id)).collect();
            return Err(BarcaError::Usage(format!(
                "--refresh: no upstream asset named '{name}'{}.\n\
                 Upstream assets you can refresh: {}\n\
                 Pass several as a comma-separated list: --refresh {}",
                match target_ids {
                    [] => String::new(),
                    [t] => format!(" in the cone of '{}'", short_name(t)),
                    many => format!(
                        " in the cones of {}",
                        many.iter()
                            .map(|t| format!("'{}'", short_name(t)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
                if valid.is_empty() {
                    "(none)".to_string()
                } else {
                    valid.join(", ")
                },
                valid.iter().take(2).copied().collect::<Vec<_>>().join(","),
            )));
        }
    }
    Ok(())
}

/// The most recent successful materialization of `node_id` with this run hash, if any. The row
/// is the cache hit; whether its artifact can still be read is settled only when something
/// needs to read it (see [`crate::recover`]).
async fn lookup_cached(
    cache: &db::CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<dispatch::OutputRef> {
    const COLUMNS: &str = "artifact_path, artifact_format, artifact_size_bytes";
    let query = |columns: String| {
        cache.conn().query(
            format!(
                "SELECT {columns} FROM materializations WHERE node_id = ?1 AND run_hash = ?2 \
                 AND status = 'success' ORDER BY id DESC LIMIT 1"
            ),
            [node_id.to_string(), run_hash.to_string()],
        )
    };
    // A database from before barca recorded output hashes has no such column.
    let mut rows = match query(format!("{COLUMNS}, output_hash")).await {
        Ok(rows) => rows,
        Err(_) => query(COLUMNS.to_string()).await.unwrap(),
    };
    rows.next().await.unwrap().and_then(|row| {
        Some(dispatch::OutputRef {
            path: row.get::<String>(0).ok()?,
            format: row.get::<String>(1).ok()?,
            size_bytes: row.get::<i64>(2).ok()? as u64,
            elapsed_seconds: None,
            content_hash: row.get::<String>(3).ok().filter(|h| !h.is_empty()),
        })
    })
}

/// A spawned task that is aborted if dropped before it is joined, so an early
/// return never leaves background work running (e.g. a state pull that
/// would overwrite the local DB after the run gave up).
struct Background<T>(Option<tokio::task::JoinHandle<T>>);

impl<T: Send + 'static> Background<T> {
    fn spawn(fut: impl std::future::Future<Output = T> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(fut)))
    }

    async fn join(mut self) -> Result<T, BarcaError> {
        let handle = self.0.take().expect("joined once");
        handle
            .await
            .map_err(|e| BarcaError::Other(format!("background task failed: {e}")))
    }
}

impl<T> Drop for Background<T> {
    fn drop(&mut self) {
        if let Some(h) = &self.0 {
            h.abort();
        }
    }
}

/// How a cache row's artifact is reached on this machine.
enum CacheHit {
    /// Use the output as recorded.
    Local(dispatch::OutputRef),
    /// The row records a location in the artifact store; `local` points at
    /// its local mirror, which is fetched before anything reads it.
    Store {
        local: dispatch::OutputRef,
        store: String,
    },
}

/// Resolve a cache row against this run's artifact store (`layout` is Some
/// when the store is separate from the local artifact dir). This only says
/// where the artifact is read from; whether it is there is checked when
/// something needs to read it (see [`crate::recover`]).
fn resolve_cache_hit(oref: dispatch::OutputRef, layout: Option<&ArtifactLayout>) -> CacheHit {
    match layout.and_then(|l| l.local_for(&oref.path)) {
        Some(local) => {
            let store = oref.path.clone();
            CacheHit::Store {
                local: dispatch::OutputRef {
                    path: local.to_string_lossy().into_owned(),
                    ..oref
                },
                store,
            }
        }
        None => CacheHit::Local(oref),
    }
}

/// Apply this run's artifact store to a cache row: the output to read, with
/// a store-backed row registered for fetching.
fn accept_cache_hit(
    store: &mut Option<StoreSync>,
    node_id: &str,
    oref: dispatch::OutputRef,
) -> dispatch::OutputRef {
    match resolve_cache_hit(oref, store.as_ref().map(|s| &s.layout)) {
        CacheHit::Local(o) => o,
        CacheHit::Store { local, store: at } => {
            if let Some(s) = store.as_mut() {
                s.fetchable.insert(
                    local.path.clone(),
                    (node_id.to_string(), at, local.content_hash.clone()),
                );
            }
            local
        }
    }
}

/// Apply this run's artifact store to a cache decision: cached outputs point
/// at their local mirror.
fn localize_decision(
    decision: Decision,
    step: &crate::planner::StreamStep,
    store: &mut Option<StoreSync>,
) -> Decision {
    match decision {
        Decision::Cached { oref, stale_root } => Decision::Cached {
            oref: accept_cache_hit(store, &step.step_id.display(), oref),
            stale_root,
        },
        Decision::Partitioned { cached, missing } => Decision::Partitioned {
            cached: cached
                .into_iter()
                .map(|(pdisplay, oref)| {
                    let oref = accept_cache_hit(store, &pdisplay, oref);
                    (pdisplay, oref)
                })
                .collect(),
            missing,
        },
        run => run,
    }
}

/// The output a run returns as `final_output`: the target's when there is exactly one (for a
/// partitioned target, the first partition by key), none when there are several (each target
/// reports its own), and with no target the last planned asset's (a sensor's only when the plan
/// has no asset).
fn final_output_of(
    exec_plan: &ExecutionPlan,
    target_ids: &[&str],
    several_targets: bool,
    all_outputs: &HashMap<String, OutputRef>,
) -> Option<OutputRef> {
    if let [tid] = target_ids {
        return output_for(tid, all_outputs);
    }
    if several_targets {
        return None;
    }
    let last_planned_id = recover::returned_step(exec_plan)
        .map(|s| s.step_id.display())
        .unwrap_or_default();
    all_outputs.get(&last_planned_id).cloned().or_else(|| {
        let mut matches: Vec<_> = all_outputs
            .iter()
            .filter(|(k, _)| k.starts_with(&last_planned_id))
            .collect();
        matches.sort_by_key(|(k, _)| (*k).clone());
        matches.first().map(|(_, v)| (*v).clone())
    })
}

/// This run's link to a separate artifact store: the transfer helper plus
/// the cache hits whose artifacts still live only in the store.
pub(crate) struct StoreSync {
    client: TransferClient,
    layout: ArtifactLayout,
    /// Local mirror path → (node id, store location, recorded SHA-256), for
    /// store-backed cache hits. Fetched on first use, so fully-cached
    /// intermediates a run never reads are never downloaded. With a recorded
    /// hash, a copy already on disk is checked against it on first use too.
    fetchable: HashMap<String, (String, String, Option<String>)>,
    /// Base step id -> what was found, for each store copy fetched in this run that does not
    /// have its recorded hash. Put on the step reports when the run ends ([`crate::mismatch`]).
    mismatched: HashMap<String, String>,
    /// Local mirror paths of fetches the store answered with "no such object",
    /// until [`Self::take_missing`] collects them.
    missing: Vec<String>,
    /// Whether the store itself is there, once it has been asked (see
    /// [`Self::confirm_present`]).
    present: Option<Result<(), String>>,
    /// The run's cancellation: a wait on the store ends when it fires.
    cancel: CancellationToken,
}

/// What a wait on the artifact store reports when the run is cancelled during it. The run
/// then ends as cancelled; this text is never the error a user sees.
const STORE_WAIT_CANCELLED: &str = "run cancelled";

impl StoreSync {
    fn new(client: TransferClient, cancel: CancellationToken) -> Self {
        Self {
            layout: client.layout().clone(),
            client,
            fetchable: HashMap::new(),
            mismatched: HashMap::new(),
            missing: Vec::new(),
            present: None,
            cancel,
        }
    }

    /// Whether `path` is the local mirror of a cache hit that has not been
    /// fetched in this run: its artifact is read from the store if it is not
    /// here.
    pub(crate) fn holds(&self, path: &str) -> bool {
        self.fetchable.contains_key(path)
    }

    /// The local mirror paths of the artifacts [`Self::ensure_local`] found
    /// to be absent from the store since this was last called.
    pub(crate) fn take_missing(&mut self) -> Vec<String> {
        std::mem::take(&mut self.missing)
    }

    /// Whether a cache hit whose artifact is not on this disk is known to be
    /// absent, without asking a remote store: a local row, or a directory
    /// store, is a stat away. Unknown (false) for a result in a remote store.
    fn known_absent(store: Option<&Self>, path: &str) -> bool {
        match store.and_then(|s| s.fetchable.get(path)) {
            Some((_, at, _)) => {
                !std::path::Path::new(path).is_file()
                    && crate::transfer::local_path(at).is_some_and(|stored| !stored.exists())
            }
            None => !recover::on_disk(path, store.is_some()),
        }
    }

    /// Make sure the store itself is there before the cached results `lost`
    /// are computed again on the strength of "not in the store": its bucket,
    /// container or root directory must answer a listing. Asked once per run.
    ///
    /// An object that is absent from a store that is there is a missing
    /// artifact. A store that is gone, misnamed or unreachable answers the
    /// same way for every object, and recomputing then would turn an outage or
    /// a bad setting into a full recompute written to the wrong place.
    pub(crate) async fn confirm_present(&mut self, lost: &[String]) -> Result<(), String> {
        if self.present.is_none() {
            let answer = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(STORE_WAIT_CANCELLED.to_string()),
                answer = self.client.probe() => answer,
            };
            self.present = Some(answer);
        }
        let Some(Err(why)) = &self.present else {
            return Ok(());
        };
        let shown: Vec<String> = lost.iter().take(10).map(|id| format!("  {id}")).collect();
        let more = match lost.len().saturating_sub(shown.len()) {
            0 => String::new(),
            n => format!("\n  ... and {n} more"),
        };
        Err(format!(
            "could not fetch {} cached artifact(s) from the artifact store: the store at {} \
             is not there or cannot be listed ({why}).\n{}{more}\n\
             Nothing was recomputed. Check the store location and credentials, or re-run with \
             --refresh-all to recompute them.",
            lost.len(),
            self.layout.store_root(),
            shown.join("\n")
        ))
    }

    /// Point lazily read parquet inputs that are not on this disk at the store,
    /// so the step's reader fetches only the byte ranges its query uses. They
    /// stay fetchable: a later eager reader still downloads the whole artifact.
    fn read_in_place(
        &self,
        provided: &mut HashMap<String, dispatch::ProvidedInput>,
        lazy: &HashSet<String>,
    ) {
        for (key, input) in provided.iter_mut() {
            let base = key.split_once('[').map_or(key.as_str(), |(b, _)| b);
            if !lazy.contains(key) && !lazy.contains(base) {
                continue;
            }
            let dispatch::ProvidedInput::Single(oref) = input else {
                continue;
            };
            if oref.format != "parquet" || std::path::Path::new(&oref.path).is_file() {
                continue;
            }
            if let Some((_, at, _)) = self.fetchable.get(&oref.path) {
                oref.path = at.clone();
            }
        }
    }

    /// Make the store-backed artifacts among `paths` local, reporting any
    /// fetch on stderr (through the progress bar when one is live). A fetch
    /// the store answers with "no such object" is not an error: its local
    /// path is kept for [`Self::take_missing`], and the caller decides
    /// whether to compute that result again. Any other failure (permissions,
    /// a store that cannot be reached) is an error naming what could not be
    /// fetched.
    pub(crate) async fn ensure_local<'a>(
        &mut self,
        paths: impl IntoIterator<Item = &'a str>,
        pb: Option<&indicatif::ProgressBar>,
    ) -> Result<(), String> {
        let mut locals = Vec::new();
        for path in paths {
            if let Some((node, store, sha256)) = self.fetchable.remove(path)
                && let Some(local) = self.client.fetch(&node, &store, sha256.as_deref())
            {
                locals.push(local);
            }
        }
        if locals.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        // Ctrl-C ends the wait at once: the run is cancelled and the helper is stopped, with
        // whatever it was downloading discarded (`TransferClient::abort`).
        let report = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(STORE_WAIT_CANCELLED.to_string()),
            report = self.client.await_fetches(&locals) => report,
        };
        if report.transferred > 0 {
            let msg = format!(
                "[barca] fetched {} cached artifact{} ({}) in {:.1}s",
                report.transferred,
                if report.transferred == 1 { "" } else { "s" },
                fmt_bytes(report.bytes),
                started.elapsed().as_secs_f64()
            );
            match pb {
                Some(bar) if !bar.is_hidden() => bar.println(&msg),
                _ => eprintln!("{msg}"),
            }
        }
        // An artifact path is `{node}/{run_hash}`, so a refresh or another machine computing
        // the same step overwrites it; the store's copy is as valid a result as the recorded
        // one. Say so rather than fail.
        let mut differing: Vec<(String, &str, usize)> = Vec::new();
        for (node, at) in &report.mismatched {
            let base = crate::StepId::parse(node).base_id().to_string();
            match differing.iter_mut().find(|(b, _, _)| *b == base) {
                Some((_, _, count)) => *count += 1,
                None => differing.push((base, at, 1)),
            }
        }
        for (base, at, count) in differing {
            // The same words on stderr and, at the end of the run, in the JSON step entries.
            let finding = crate::mismatch::describe(&base, at, count - 1);
            let msg = format!("[barca] warning: {base}: {finding}");
            match pb {
                Some(bar) if !bar.is_hidden() => bar.println(&msg),
                _ => eprintln!("{msg}"),
            }
            self.mismatched.insert(base, finding);
        }
        let (missing, failed): (Vec<_>, Vec<_>) = report.failures.iter().partition(|f| f.missing);
        if failed.is_empty() {
            self.missing.extend(
                missing
                    .iter()
                    .filter_map(|f| self.layout.local_for(&f.store))
                    .map(|local| local.to_string_lossy().into_owned()),
            );
            return Ok(());
        }
        let detail: Vec<String> = failed
            .iter()
            .map(|f| format!("  {} ({}): {}", f.key, f.store, f.message))
            .collect();
        let messages: Vec<&str> = failed.iter().map(|f| f.message.as_str()).collect();
        Err(format!(
            "could not fetch {} cached artifact(s) from the artifact store:\n{}\n{}",
            failed.len(),
            detail.join("\n"),
            transfer_remedy(&messages, "Re-run with --refresh-all to recompute them.")
        ))
    }
}

/// What to do about failed transfers, given the helper's error messages; `otherwise` when
/// nothing more specific is known.
///
/// A directory at an object's path in a store that is a shared directory is not fixed by
/// recomputing: the upload would meet the same directory. Barca changes nothing in a store
/// but its own objects, so the directory has to be removed there. A local directory that
/// could not be moved aside says what to do in its own message.
fn transfer_remedy(messages: &[&str], otherwise: &str) -> String {
    if messages.iter().any(|m| m.starts_with("IsADirectoryError")) {
        "A directory sits where the artifact's object belongs in the store. Remove or rename \
         it there (barca changes nothing in a store but its own objects), then run the \
         command again."
            .to_string()
    } else if messages
        .iter()
        .any(|m| m.starts_with(BLOCKED_ARTIFACT_PATH))
    {
        "Then run the command again.".to_string()
    } else {
        otherwise.to_string()
    }
}

/// How an error starts when a directory at an artifact path could not be moved aside
/// (`barca._storage.ArtifactPathError`): the state of barca's own artifact directory, not a
/// fault of the step that was writing there.
const BLOCKED_ARTIFACT_PATH: &str = "ArtifactPathError";

fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Total schedulable steps in a phase: 1 per unpartitioned step, `partition_keys.len()`
/// for late-expanded ones. Used to keep the live progress-bar total in sync with
/// `dispatch::expand_pending_partitions`, which turns a single planned
/// (`partitions_from`) step into its real per-key count only at dispatch time.
fn phase_step_count(phase: &Phase) -> usize {
    phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|st| {
            if st.partition_keys.is_empty() {
                1
            } else {
                st.partition_keys.len()
            }
        })
        .sum()
}

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

/// Worker pool size: `BARCA_POOL_SIZE` overrides auto-detection when set to a
/// positive integer. Lets benchmark harnesses (and anyone else) pin the pool
/// to a fixed core count instead of whatever `available_parallelism()` reports
/// on the current machine.
fn default_pool_size() -> usize {
    if let Some(n) = env::var("BARCA_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

// ─── get / run ─────────────────────────────────────────────────────────────────

/// How the engine treats cached asset materializations for this invocation.
#[derive(Debug, Clone)]
pub enum CachePolicy {
    /// Normal cache-aware behavior — reuse fresh asset artifacts.
    CacheAware,
    /// Force-rerun every asset in the target's cone (`barca get|run ... --refresh-all`).
    RefreshAll,
    /// Force-rerun the named assets (`barca get|run ... --refresh a,b`). A name matches when it
    /// equals the node's base id exactly, or matches the trailing `:name` segment. With
    /// `cascade` (the default) every asset downstream of a named one in the target's cone
    /// re-runs too; without it (`--no-cascade`) all other assets stay cache-aware.
    RefreshSelective { names: Vec<String>, cascade: bool },
}

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
    execute(
        cfg, &names, file_args, python, false, agent_mode, policy, "get", cancel, None,
    )
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
    execute(
        cfg,
        &[target_name.to_string()],
        file_args,
        python,
        false,
        agent_mode,
        policy,
        "run",
        cancel,
        None,
    )
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
    execute(
        cfg, &names, file_args, python, false, agent_mode, policy, "get", cancel, event_tx,
    )
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
    execute(
        cfg,
        &[target_name.to_string()],
        file_args,
        python,
        false,
        agent_mode,
        policy,
        "run",
        cancel,
        event_tx,
    )
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
    execute(
        cfg,
        target_names,
        file_args,
        python,
        false,
        agent_mode,
        policy,
        "get",
        cancel,
        None,
    )
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
    execute(
        cfg,
        target_names,
        file_args,
        python,
        false,
        agent_mode,
        policy,
        "run",
        cancel,
        None,
    )
    .await
    .map(Executed::into_multi)
}

/// What `execute` produced: the run, each target's outcome, and the first step failure.
/// Single-target runs turn that failure into an `Err`; multi-target runs report it per target.
struct Executed {
    result: GetResult,
    targets: Vec<(String, TargetOutcome)>,
    step_failure: Option<crate::FailedStep>,
}

impl Executed {
    /// A step failure ends a single-target run: exit 1, with the node, its traceback, and what
    /// the run did before it stopped (the failed step's status is `failed`).
    fn into_single(self) -> Result<GetResult, BarcaError> {
        match self.step_failure {
            Some(mut failed) => {
                let r = self.result;
                failed.run = Some(Box::new(crate::PartialRun {
                    run_id: r.run_id,
                    elapsed_seconds: r.elapsed_seconds,
                    steps_executed: r.steps_executed,
                    phases: r.phases,
                    steps: r.steps,
                    warnings: r.warnings,
                }));
                Err(BarcaError::WorkerFailed(Box::new(failed)))
            }
            None => Ok(self.result),
        }
    }

    fn into_multi(self) -> MultiResult {
        let r = self.result;
        MultiResult {
            run_id: r.run_id,
            elapsed_seconds: r.elapsed_seconds,
            steps_executed: r.steps_executed,
            phases: r.phases,
            steps: r.steps,
            warnings: r.warnings,
            targets: self.targets,
        }
    }
}

/// `barca get|run --dry-run` — report what the command would do, without doing it.
///
/// Plans exactly as a real run does and sends every step through [`decide_step`], so the
/// prediction is the real run's decision. Nothing executes, no worker starts, and nothing is
/// written: no `.barca` directory is created and no run is recorded. A dry run cannot know
/// the key set of a dynamic partition (`partitions_from`) whose source has to run first, nor
/// what a sensor will return: a step reading a sensor is predicted from the sensor's last
/// recorded output, and is `unknown` when there is none. Those steps (and anything depending on
/// them) are reported as `unknown`.
pub async fn explain(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    no_cache: bool,
    command_label: &str,
) -> Result<ExplainResult, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    if command_label == "get"
        && target_names.is_empty()
        && let Some(note) = skipped_tasks_note(&dag, file_args)
    {
        eprintln!("{note}");
    }
    let result = explain_dag(
        &dag,
        cfg,
        target_names,
        python,
        policy,
        no_cache,
        command_label,
    )
    .await?;
    crate::warnings::print(&result.warnings);
    Ok(result)
}

/// [`explain`] on an already-built DAG (`barca status` reuses its DAG for the node listing).
pub(crate) async fn explain_dag(
    dag: &Dag,
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    python: &std::path::Path,
    policy: CachePolicy,
    no_cache: bool,
    command_label: &str,
) -> Result<ExplainResult, BarcaError> {
    let targets = resolve_targets(dag, target_names, command_label)?;
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let pool_size = default_pool_size();
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let exec_plan = plan_for_targets(dag, &target_ids, &config, command_label);
    if let CachePolicy::RefreshSelective { names, .. } = &policy {
        validate_refresh_names(dag, &target_ids, names, command_label == "get")?;
    }
    let warnings = crate::warnings::for_plan(dag, &exec_plan);

    // Shared remote state: pull it like a real run, so the cache check sees every machine's
    // materializations. A pull keeps the local rows that were never pushed, so this is safe
    // while a run is going in the same project: what that run has recorded so far is still
    // there afterwards, next to what other machines pushed.
    if cfg.state == crate::config::StateMode::Optimistic && cfg.state_uri.is_some() {
        let pulled = state_sync::pull_state(python, cfg, state_sync::Until::done()).await?;
        if let Some(note) = pulled.carried.note() {
            eprintln!("{note}");
        }
    }

    // No metadata DB yet means nothing is cached. Do not create one just to look.
    let cache = if std::path::Path::new(&cfg.db_path).exists() {
        Some(db::CacheReader::open(&cfg.db_path).await?)
    } else {
        None
    };

    let mut state = DecideState::default();
    // A dry run executes nothing, so it cannot know what a sensor will return. It predicts with
    // each sensor's last recorded output (#183); a consumer of a sensor with none is unknown.
    if let Some(cache) = &cache {
        let sensors: Vec<&str> = exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .filter(|st| st.kind == crate::NodeKind::Sensor)
            .map(|st| st.step_id.base_id())
            .collect();
        state.sensor_outputs = db::last_output_hashes(cache, &sensors).await?;
    }
    let mut all_outputs: HashMap<String, OutputRef> = HashMap::new();
    // What a run would compute again because an artifact it needs is missing (#252): the same
    // rule as `execute`, predicted from what is on this disk.
    let layout = cfg
        .remote_artifacts()
        .then(|| ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root));
    let mut cached_steps = recover::CachedSteps::default();
    let requested = recover::requested(&exec_plan, &target_ids);
    // Steps reported as unknown, by base id, with the reason code their dependents inherit.
    let mut unknown_ids: HashMap<String, &'static str> = HashMap::new();
    // Steps whose run hash cannot be predicted because a sensor upstream of them has no
    // recorded output (a forced step, e.g. under --refresh, is still reported as `run`).
    let mut hash_unknown: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut steps: Vec<StepReport> = Vec::new();
    let mut summary = ExplainSummary::default();

    let unknown_report = |dag: &Dag, base_id: &str, reason: &str, detail: String| StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        action: Some("unknown".to_string()),
        reason: Some(reason.to_string()),
        detail: Some(detail),
        ..Default::default()
    };

    for phase in &exec_plan.phases {
        // A `partitions_from` source is read to expand its consumers: if its artifact is
        // missing it runs again first, and its keys are not known until it has.
        let sources = dispatch::partition_sources(phase, &all_outputs)
            .into_iter()
            .map(|o| o.path.clone())
            .collect();
        recover::predict_recomputes(
            sources,
            &mut cached_steps,
            &mut all_outputs,
            layout.as_ref(),
            &mut steps,
            &mut summary,
        );

        // Dynamic partitions need their source's output to know their keys. If the source is
        // not available (it would have to run first) the step cannot be expanded.
        let mut ready = phase.clone();
        for stream in &mut ready.streams {
            stream.steps.retain(|st| {
                let missing_source = st.pending_partitions.values().find(|src| {
                    !all_outputs
                        .keys()
                        .any(|k| k.ends_with(&format!(":{src}")) || k.as_str() == src.as_str())
                });
                match missing_source {
                    Some(src) => {
                        let base = st.step_id.base_id();
                        steps.push(unknown_report(
                            dag,
                            base,
                            "partitions_unknown",
                            format!(
                                "partition keys come from the output of '{src}', which is not \
                                 available until it runs"
                            ),
                        ));
                        unknown_ids.insert(base.to_string(), "partitions_unknown");
                        summary.unknown += 1;
                        false
                    }
                    None => true,
                }
            });
        }

        let expanded = dispatch::expand_pending_partitions(&ready, &all_outputs, pool_size);
        let phase_ref = expanded.as_ref().unwrap_or(&ready);
        // The steps of this phase that would execute: what they read has to be there.
        let mut to_run: Vec<crate::planner::StreamStep> = Vec::new();

        for stream in &phase_ref.streams {
            for step in &stream.steps {
                let base = step.step_id.base_id();
                let up_base = |up: &str| up.split('[').next().unwrap_or(up).to_string();
                let unknown_dep = step
                    .inputs
                    .values()
                    .find(|up| unknown_ids.get(&up_base(up)) == Some(&"partitions_unknown"));
                if let Some(up) = unknown_dep {
                    steps.push(unknown_report(
                        dag,
                        base,
                        "partitions_unknown",
                        format!(
                            "depends on '{}', whose partitions are not known until it runs",
                            short_name(up)
                        ),
                    ));
                    unknown_ids.insert(base.to_string(), "partitions_unknown");
                    summary.unknown += 1;
                    continue;
                }

                // Sensors this step reads, and whether the dry run knows their output.
                let read_sensors = sensor_inputs(dag, step);
                let missing_sensor = read_sensors
                    .iter()
                    .find(|s| !has_sensor_output(&state, s))
                    .map(|s| s.to_string());
                let hash_unknown_dep = step
                    .inputs
                    .values()
                    .find(|up| hash_unknown.contains(&up_base(up)))
                    .cloned();

                let (step, decision) =
                    decide_step(dag, &policy, no_cache, cache.as_ref(), &mut state, step).await;
                // Forced to run whatever the cache holds (task, sensor, refresh, --no-cache).
                let forced = matches!(
                    &decision,
                    Decision::Run(reason) if *reason != RunReason::NotMaterialized
                );
                if missing_sensor.is_some() || hash_unknown_dep.is_some() {
                    hash_unknown.insert(base.to_string());
                    if !forced {
                        let detail = match (&missing_sensor, &hash_unknown_dep) {
                            (Some(s), _) => format!(
                                "reads sensor '{}', which has no recorded output: its value is \
                                 not known until it runs",
                                short_name(s)
                            ),
                            (None, Some(up)) => format!(
                                "depends on '{}', whose inputs include a sensor with no \
                                 recorded output",
                                short_name(up)
                            ),
                            (None, None) => unreachable!(),
                        };
                        // A partitioned step split across streams is one line.
                        if !steps.iter().any(|r| r.id == base) {
                            steps.push(unknown_report(dag, base, "sensor_output_unknown", detail));
                        }
                        unknown_ids.insert(base.to_string(), "sensor_output_unknown");
                        summary.unknown += step.partition_keys.len().max(1);
                        continue;
                    }
                }

                let mut report = report_for(dag, &step, &decision, true);
                if !forced && !read_sensors.is_empty() {
                    let note = read_sensors
                        .iter()
                        .map(|s| {
                            format!(
                                "assumes sensor '{}' returns the same value as its last run",
                                short_name(s)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    report.detail = Some(match report.detail.take() {
                        Some(d) => format!("{d}; {note}"),
                        None => note,
                    });
                }
                steps.push(report);
                match decision {
                    Decision::Run(_) => {
                        summary.will_run += step.partition_keys.len().max(1);
                        to_run.push(step);
                    }
                    Decision::Cached { oref, .. } => {
                        summary.cached += 1;
                        all_outputs.insert(step.step_id.display(), oref);
                        cached_steps.remember(step);
                    }
                    Decision::Partitioned { cached, missing } => {
                        summary.cached += cached.len();
                        summary.will_run += missing.len();
                        let any_cached = !cached.is_empty();
                        for (pdisplay, oref) in cached {
                            all_outputs.insert(pdisplay, oref);
                        }
                        if !missing.is_empty() {
                            let mut partial = step.clone();
                            partial.partition_keys = missing;
                            to_run.push(partial);
                        }
                        if any_cached {
                            cached_steps.remember(step);
                        }
                    }
                }
            }
        }

        let running = Phase {
            reason: phase_ref.reason.clone(),
            streams: vec![crate::planner::WorkerStream {
                stream_id: "dry-run".to_string(),
                steps: to_run,
            }],
        };
        let inputs = recover::input_paths(&dispatch::build_provided_inputs(&running, &all_outputs));
        recover::predict_recomputes(
            inputs,
            &mut cached_steps,
            &mut all_outputs,
            layout.as_ref(),
            &mut steps,
            &mut summary,
        );
    }
    drop(cache);

    // The outputs the command was asked for are read by whoever ran it.
    let returned = all_outputs
        .iter()
        .filter(|(id, _)| requested.contains(recover::base_of(id)))
        .map(|(_, o)| o.path.clone())
        .collect();
    recover::predict_recomputes(
        returned,
        &mut cached_steps,
        &mut all_outputs,
        layout.as_ref(),
        &mut steps,
        &mut summary,
    );

    let steps = merge_partition_reports(steps);
    // Per-target predictions: the same step lines, counted over each target's cone.
    let per_target = if targets.len() > 1 {
        targets
            .iter()
            .map(|(name, id)| {
                let cone: std::collections::HashSet<&str> = dag.subgraph(id).into_iter().collect();
                let mut s = ExplainSummary::default();
                for r in steps.iter().filter(|r| cone.contains(r.id.as_str())) {
                    s.add(r);
                }
                (name.clone(), TargetPrediction { summary: s })
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(ExplainResult {
        dry_run: true,
        command: command_label.to_string(),
        target: match target_ids.as_slice() {
            [one] => Some(short_name(one).to_string()),
            _ => None,
        },
        targets: per_target,
        steps,
        summary,
        warnings,
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    no_cache: bool,
    agent_mode: bool,
    policy: CachePolicy,
    command_label: &str,
    interrupt: impl Into<crate::interrupt::Interrupt>,
    event_tx: Option<UnboundedSender<crate::RunEvent>>,
) -> Result<Executed, BarcaError> {
    let interrupt = interrupt.into();
    let cancel = interrupt.cancel.clone();
    let t0 = Instant::now();
    // BARCA_TRACE_TIMING=1: emit a millisecond-resolution waterfall of every
    // major checkpoint in this run to stderr (DAG parse, planning, DB setup,
    // per-phase cache-check/dispatch/shutdown, persist_run — plus per-worker
    // spawn and per-step dispatch/completion timestamps from io_loop.rs).
    // One `env::var` check per call site; free when unset. Written to track
    // down where wall-clock time actually goes on a slow run — e.g. it's how
    // a real ~1.3s of JSON serialization on a couple of heavy assets was
    // found to be invisible to barca's own per-step timing (see the fix to
    // `_materialize`'s timer placement in python/barca/_worker.py).
    let trace_on = std::env::var("BARCA_TRACE_TIMING").is_ok();
    macro_rules! trace_point {
        ($($arg:tt)*) => {
            if trace_on {
                eprintln!("[trace] {:>8.1}ms  {}", t0.elapsed().as_secs_f64() * 1000.0, format!($($arg)*));
            }
        };
    }
    let run_id = db::generate_run_id();
    let run_started = std::time::SystemTime::now();
    let telemetry = crate::telemetry::configured();

    // Start remote I/O first so it overlaps parsing and planning: the shared
    // state pull (joined just before the metadata DB is opened) and the
    // artifact transfer helper's startup (joined before workers start).
    let state_sync_on =
        cfg.state == crate::config::StateMode::Optimistic && cfg.state_uri.is_some();
    let pull = state_sync_on.then(|| {
        let (python, cfg, cancel) = (python.to_path_buf(), cfg.clone(), cancel.clone());
        Background::spawn(async move {
            let started = Instant::now();
            // Ctrl-C while the shared state is still being pulled ends the command here:
            // nothing has run and no run has been created yet.
            let until = state_sync::Until::cancelled(&cancel);
            let pulled = state_sync::pull_state(&python, &cfg, until).await?;
            Ok::<_, BarcaError>((pulled, started.elapsed()))
        })
    });
    // Not a task: the helper connects on its own while this function goes on, and the value
    // stops it if this function returns before using it (see `transfer::Launching`).
    let mut transfer_start = match cfg.remote_artifacts() {
        true => Some(TransferClient::launch(python, cfg, &run_id)?),
        false => None,
    };

    let dag = build_dag(file_args, python).await?;
    trace_point!("dag_built");

    let targets = resolve_targets(&dag, target_names, command_label)?;
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let job_name = canonical_job(&target_ids);
    if command_label == "get"
        && target_ids.is_empty()
        && let Some(note) = skipped_tasks_note(&dag, file_args)
    {
        eprintln!("{note}");
    }
    // Several targets: a step failure stops only what depends on it, so every target that can
    // still run does (one run reports every failure). One target keeps the stop-at-first-failure
    // behavior: nothing else in its cone could produce its value.
    let keep_going = targets.len() > 1;
    // For the run record: the targets as given, comma-separated.
    let target_label: Option<String> = (!targets.is_empty()).then(|| {
        targets
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    });

    let pool_size = default_pool_size();
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let exec_plan = plan_for_targets(&dag, &target_ids, &config, command_label);
    trace_point!("planned");

    if let CachePolicy::RefreshSelective { names, .. } = &policy {
        validate_refresh_names(&dag, &target_ids, names, command_label == "get")?;
    }
    // Plan-time warnings for the steps this command planned, before anything runs.
    let plan_warnings = crate::warnings::for_plan(&dag, &exec_plan);
    crate::warnings::print(&plan_warnings);

    db::ensure_env_dirs(&cfg.env)?;
    let db_path = cfg.db_path.clone();

    // Shared remote state: the pull must land before the DB is opened, so
    // cache checks below see every machine's materializations. Pull failure
    // is a hard error — silently diverging local runs are worse than stopping.
    let mut state_token = match pull {
        Some(pull) => {
            let pulled = match pull.join().await.and_then(|pulled| pulled) {
                Ok(pulled) => pulled,
                Err(e) => {
                    // The command ends here (the pull failed, or Ctrl-C cancelled it). It
                    // does not return before the transfer helper it started is gone.
                    if let Some(start) = transfer_start.take() {
                        start.stop().await;
                    }
                    return Err(e);
                }
            };
            let (state_sync::Pulled { token, carried }, took) = pulled;
            if let Some(note) = carried.note() {
                eprintln!("{note}");
            }
            match token.0 {
                Some(_) => eprintln!(
                    "[barca] pulled state ({}) in {:.2}s",
                    fmt_bytes(std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)),
                    took.as_secs_f64()
                ),
                None => eprintln!("[barca] no shared state yet — this run will create it"),
            }
            Some(token)
        }
        None => None,
    };
    trace_point!("state_sync_pull_joined (enabled={state_sync_on})");

    db::init_db(&db_path).await?;
    trace_point!("db_init");

    db::create_run(
        &db_path,
        &run_id,
        command_label,
        &db::encode_files(file_args),
        target_label.as_deref(),
        Some(exec_plan.total_steps),
    )
    .await?;
    trace_point!("db_create_run");

    // Measured-cost model: seed from persisted estimates so batch sizing is
    // pre-warmed — the cold-start probe is paid once ever per stable node,
    // not once per run.
    let mut cost_model = crate::cost::CostModel::new();
    cost_model.seed(db::load_cost_estimates(&db_path).await?);
    trace_point!("cost_model_seeded");

    let mut cached_node_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Run hashes, assets refreshed in this run, and cached assets downstream of a refreshed one
    // (run hashes cover definitions and upstream *hashes*, not outputs, so a refresh does not
    // invalidate downstream caches; we say so when that happens). See `decide_step`.
    let mut decide_state = DecideState::default();
    // What happened to each planned step, for the result.
    let mut step_reports: Vec<StepReport> = Vec::new();
    let mut phase_error: Option<String> = None;
    // The first step failure (a step raised, timed out or its worker died): (node, message).
    // With one target it ends the run; with several (`keep_going`) the run continues around it.
    let mut step_failure: Option<(String, String)> = None;
    // Base ids of failed steps, and of steps not run because something upstream failed.
    let mut failed_bases: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut skipped_bases: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_outputs: HashMap<String, dispatch::OutputRef> = HashMap::new();
    // Captured user output (node_id, line), persisted to the DB after the run.
    let mut logs_buffer: Vec<(String, String)> = Vec::new();
    // Sink outcomes (JSON) per node, accumulated across phases for the DB.
    let mut all_sinks: HashMap<String, String> = HashMap::new();
    // Per-node self-timing (cpu_seconds, max_rss_bytes) reported by workers.
    let mut all_timings: HashMap<String, (Option<f64>, Option<u64>)> = HashMap::new();
    // Per-node wall clock from the worker: (finished_at as Unix seconds, wall seconds).
    let mut step_clocks: HashMap<String, (f64, f64)> = HashMap::new();
    // Permanently-failed steps + attempt counts, accumulated across phases for the DB.
    let mut all_failures: Vec<dispatch::StepFailure> = Vec::new();
    let mut all_attempts: HashMap<String, u32> = HashMap::new();
    let mut steps_executed = 0;

    // Progress bar setup. `total_steps` starts as the plan-time estimate and
    // grows as dynamic (`partitions_from`) phases expand at dispatch time —
    // see the `phase_step_count` reconciliation below.
    let mut total_steps = exec_plan.total_steps;
    // Collect unpartitioned node_ids for exact ETA lookup.
    let unpartitioned_node_ids: Vec<String> = exec_plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .filter(|st| st.partition_keys.is_empty())
        .map(|st| st.step_id.display())
        .collect();
    // Collect partitioned base node_ids for LIKE-based ETA lookup.
    let partitioned_base_ids: Vec<String> = exec_plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .filter(|st| !st.partition_keys.is_empty())
        .map(|st| st.step_id.base_id().to_string())
        .collect();
    let avg_times = db::get_avg_elapsed(&db_path, &unpartitioned_node_ids).await?;
    let partitioned_avg_times =
        db::get_avg_elapsed_for_partitioned(&db_path, &partitioned_base_ids).await?;
    trace_point!(
        "eta_queries ({} unpartitioned, {} partitioned base ids)",
        unpartitioned_node_ids.len(),
        partitioned_base_ids.len()
    );
    let total_estimated: f64 = unpartitioned_node_ids
        .iter()
        .filter_map(|nid| avg_times.get(nid))
        .sum::<f64>()
        + exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .filter(|st| !st.partition_keys.is_empty())
            .filter_map(|st| {
                let base = st.step_id.base_id().to_string();
                partitioned_avg_times
                    .get(&base)
                    .map(|avg| avg * st.partition_keys.len() as f64)
            })
            .sum::<f64>();
    let mut elapsed_so_far: f64 = 0.0;
    let mut completed_steps: usize = 0;

    // Create indicatif progress bar for human mode, plain text for agent mode.
    let pb = if !agent_mode && total_steps > 0 {
        use indicatif::{ProgressBar, ProgressStyle};
        let bar = ProgressBar::new(total_steps as u64);
        bar.set_style(
            ProgressStyle::with_template(
                "[barca] {prefix} {bar:20.cyan/dim} {pos}/{len} | {wide_msg}",
            )
            .unwrap()
            .progress_chars("█▓░"),
        );
        if total_estimated > 0.0 {
            bar.set_prefix(format!("{}left", fmt_eta(total_estimated)));
        } else {
            bar.set_prefix("        ");
        }
        bar.set_message("");
        Some(bar)
    } else {
        None
    };

    // Separate artifact store: workers still read and write only the local
    // artifact dir; the transfer helper uploads finished artifacts in the
    // background and fetches cache hits recorded by other machines.
    let mut store: Option<StoreSync> = if let Some(start) = transfer_start {
        Some(StoreSync::new(start.connect().await?, cancel.clone()))
    } else {
        None
    };
    trace_point!("store_sync_started (enabled={})", store.is_some());
    // Store location of every output uploaded this run, by node id.
    let mut store_paths: HashMap<String, String> = HashMap::new();
    // Set when an artifact cannot be fetched or uploaded: the run fails.
    let mut transfer_error: Option<String> = None;
    let worker_artifact_root = match &store {
        Some(s) => s.layout.local_root().to_string_lossy().into_owned(),
        None => cfg.artifact_root.clone(),
    };

    // Persistent worker pool: one pool for the whole run, shared across
    // phases so workers keep their interpreter (and imported user modules)
    // warm between phases.
    let io_config = crate::io_loop::IoConfig {
        python: python.to_path_buf(),
        pool_size,
        run_id: run_id.clone(),
        datadog_job: telemetry
            .iter()
            .any(|(name, _)| name == "datadog")
            .then(|| job_name.clone()),
        artifact_root: worker_artifact_root,
        storage_options_json: cfg.storage_options_json.clone(),
    };
    let mut pool = crate::io_loop::WorkerPool::start(io_config).map_err(BarcaError::Other)?;
    // Finished steps are written to the local metadata DB while the run goes on (#214).
    let recorder = StepRecorder::start(db_path.clone(), run_id.clone());
    {
        // A step that runs for a while must not look hung: report it periodically.
        let bar = pb.clone();
        pool.on_running(Box::new(move |running| match &bar {
            Some(bar) if !bar.is_hidden() => {
                if let Some((id, secs)) = running.first() {
                    bar.set_message(format!("{} running {}s", short_name(id), *secs as u64));
                }
            }
            _ => {
                for (id, secs) in running {
                    eprintln!("[barca] still running ({}s): {id}", *secs as u64);
                }
            }
        }));
    }
    trace_point!("pool_started");

    // Steps served from cache, kept so that one can be run again if its artifact turns out to
    // be missing when something needs to read it (#252, see `recover`).
    let mut cached_steps = recover::CachedSteps::default();
    let requested = recover::requested(&exec_plan, &target_ids);
    // The plan's phases in order, then the outputs this command returns. When an artifact that
    // is needed is missing, the steps to compute again go in front of whatever needs them.
    let mut queue: VecDeque<Work<'_>> = exec_plan.phases.iter().map(Work::Planned).collect();
    queue.push_back(Work::Returned);
    let mut next_idx = 0usize;
    // `--agent`: cached steps whose `cached` line is held back because their artifact is known
    // to be absent. Each is announced once, with what became of it: `completed` if something
    // needed it and it was computed again, `cached` at the end of the run if nothing did.
    let mut held_cached_lines: Vec<String> = Vec::new();

    loop {
        // Stop scheduling new phases once cancelled; partial results from
        // completed phases are persisted below.
        if cancel.is_cancelled() {
            if phase_error.is_none() {
                phase_error = Some("run cancelled".to_string());
            }
            break;
        }
        let Some(work) = queue.pop_front() else {
            break;
        };
        let phase_idx = next_idx;
        next_idx += 1;
        trace_point!("phase{phase_idx}_start");

        // The steps to dispatch next, and whether they are cached steps being computed again.
        let (mut filtered_phase, recompute) = match work {
            Work::Ready(phase) => (phase, false),
            Work::Recompute(mut ids) => {
                // One of them may have been computed again since this was queued.
                ids.retain(|id| cached_node_ids.contains(id));
                match cached_steps.phase_for(&ids) {
                    Some(phase) => (phase, true),
                    None => continue,
                }
            }
            Work::Returned => {
                // The output this command hands back is read by whoever ran it: what it
                // prints is made local, and every part of it must be on disk or in the store.
                let returned: Vec<String> =
                    final_output_of(&exec_plan, &target_ids, keep_going, &all_outputs)
                        .iter()
                        .chain(
                            target_outcomes(&dag, &targets, &all_outputs, &all_failures)
                                .iter()
                                .filter_map(|(_, o)| o.final_output.as_ref()),
                        )
                        .map(|o| o.path.clone())
                        .collect();
                let check: Vec<String> = cached_node_ids
                    .iter()
                    .filter(|id| requested.contains(recover::base_of(id)))
                    .filter_map(|id| all_outputs.get(id))
                    .map(|o| o.path.clone())
                    .chain(returned.iter().cloned())
                    .collect();
                let lost = recover::lost(
                    &mut store,
                    &check,
                    &returned,
                    pb.as_ref(),
                    &all_outputs,
                    &cached_node_ids,
                    &mut cached_steps,
                )
                .await;
                match lost {
                    Ok(lost) if lost.is_empty() => {}
                    Ok(lost) => {
                        queue.push_front(Work::Returned);
                        queue.push_front(Work::Recompute(cached_steps.first_layer(&lost)));
                    }
                    Err(e) => {
                        transfer_error = Some(e);
                        break;
                    }
                }
                continue;
            }
            Work::Planned(phase) => {
                // `partitions_from` sources are read from disk during expansion.
                let sources: Vec<String> = dispatch::partition_sources(phase, &all_outputs)
                    .into_iter()
                    .map(|o| o.path.clone())
                    .collect();
                let lost = recover::lost(
                    &mut store,
                    &sources,
                    &sources,
                    pb.as_ref(),
                    &all_outputs,
                    &cached_node_ids,
                    &mut cached_steps,
                )
                .await;
                match lost {
                    Ok(lost) if lost.is_empty() => {}
                    Ok(lost) => {
                        queue.push_front(Work::Planned(phase));
                        queue.push_front(Work::Recompute(cached_steps.first_layer(&lost)));
                        continue;
                    }
                    Err(e) => {
                        transfer_error = Some(e);
                        break;
                    }
                }

                // Multi-target run after a failure: drop the steps that depend on a failed
                // step before they are decided; the rest of the phase still runs.
                let unblocked_phase;
                let phase = if keep_going && !failed_bases.is_empty() {
                    let mut p = phase.clone();
                    recover::drop_blocked(
                        &mut p,
                        |base| blocking_failure(&dag, base, &failed_bases),
                        |st, up| {
                            let base = st.step_id.base_id();
                            step_reports.push(StepReport {
                                id: base.to_string(),
                                kind: kind_str(dag.get_node(base).map(|n| n.kind())),
                                status: Some("skipped".to_string()),
                                reason: Some("upstream_failed".to_string()),
                                detail: Some(format!(
                                    "depends on '{}', which failed",
                                    short_name(up)
                                )),
                                ..Default::default()
                            });
                            skipped_bases.insert(base.to_string());
                        },
                    );
                    unblocked_phase = p;
                    &unblocked_phase
                } else {
                    phase
                };

                let expanded_phase =
                    dispatch::expand_pending_partitions(phase, &all_outputs, pool_size);
                let phase_ref = expanded_phase.as_ref().unwrap_or(phase);

                // Dynamic partitions (`partitions_from`) are a single placeholder step
                // in the plan-time count but expand to their real per-key count here —
                // reconcile `total_steps` so the ETA math below can't underflow and
                // the printed summary reflects what actually ran.
                if expanded_phase.is_some() {
                    let expanded_count = phase_step_count(phase_ref);
                    let planned_count = phase_step_count(phase);
                    if expanded_count > planned_count {
                        total_steps += expanded_count - planned_count;
                        if let Some(ref bar) = pb {
                            bar.set_length(total_steps as u64);
                        }
                    }
                }

                let mut uncached_streams: Vec<crate::planner::WorkerStream> = Vec::new();

                // Open the DB only for this phase's cache lookups and release it before any
                // step runs, so other barca processes can use the metadata DB while Python
                // executes.
                let cache = db::CacheReader::open(&db_path).await?;

                for stream in &phase_ref.streams {
                    let mut uncached_steps: Vec<crate::planner::StreamStep> = Vec::new();

                    for step in &stream.steps {
                        let (step, decision) = decide_step(
                            &dag,
                            &policy,
                            no_cache,
                            Some(&cache),
                            &mut decide_state,
                            step,
                        )
                        .await;
                        let decision = localize_decision(decision, &step, &mut store);
                        step_reports.push(report_for(&dag, &step, &decision, false));
                        let display_id = step.step_id.display();
                        match decision {
                            Decision::Run(_) => uncached_steps.push(step),
                            Decision::Cached { oref, stale_root } => {
                                if let Some(root) = stale_root {
                                    note(
                                        &pb,
                                        &format!(
                                            "[barca] warning: {}",
                                            stale_warning(&display_id, &root, false)
                                        ),
                                    );
                                }
                                if agent_mode {
                                    // Announce a step once, with its true outcome: when its
                                    // artifact is known to be gone it may yet be computed
                                    // again, so its line waits until that is settled.
                                    if StoreSync::known_absent(store.as_ref(), &oref.path) {
                                        held_cached_lines.push(display_id.clone());
                                    } else {
                                        eprintln!("{}", cached_step_line(&dag, &display_id));
                                    }
                                }
                                all_outputs.insert(display_id.clone(), oref);
                                cached_node_ids.insert(display_id);
                                cached_steps.remember(step);
                            }
                            Decision::Partitioned { cached, missing } => {
                                let any_cached = !cached.is_empty();
                                for (pdisplay, oref) in cached {
                                    all_outputs.insert(pdisplay.clone(), oref);
                                    cached_node_ids.insert(pdisplay);
                                }
                                if !missing.is_empty() {
                                    let mut partial = step.clone();
                                    partial.partition_keys = missing;
                                    uncached_steps.push(partial);
                                }
                                if any_cached {
                                    cached_steps.remember(step);
                                }
                            }
                        }
                    }

                    if !uncached_steps.is_empty() {
                        uncached_streams.push(crate::planner::WorkerStream {
                            stream_id: stream.stream_id.clone(),
                            steps: uncached_steps,
                        });
                    }
                }

                drop(cache);
                trace_point!("phase{phase_idx}_cache_check_done");

                if uncached_streams.is_empty() {
                    continue;
                }
                let decided = Phase {
                    reason: phase_ref.reason.clone(),
                    streams: uncached_streams,
                };
                (decided, false)
            }
        };

        // A phase that waited for a recompute may since have lost an upstream to a failure
        // (several targets keep going around one): its blocked steps do not run.
        if keep_going && !failed_bases.is_empty() {
            recover::drop_blocked(
                &mut filtered_phase,
                |base| blocking_failure(&dag, base, &failed_bases),
                |st, _| {
                    skipped_bases.insert(st.step_id.base_id().to_string());
                    if recompute {
                        // Its artifact is still missing and it cannot be computed: it is no
                        // longer an output of this run.
                        for id in recover::output_ids(st) {
                            recover::mark_recomputed(&mut step_reports, &id, false);
                            cached_node_ids.remove(&id);
                            all_outputs.remove(&id);
                        }
                    }
                },
            );
            if filtered_phase.streams.is_empty() {
                continue;
            }
        }

        // Make this phase's inputs available: exactly the artifacts its steps were provided.
        // Cache hits recorded by other machines are fetched (less the parquet inputs every
        // reader in the phase scans lazily, which are read in place). An input that is
        // neither on disk nor in the store has its step computed again first.
        let mut provided = dispatch::build_provided_inputs(&filtered_phase, &all_outputs);
        let check = recover::input_paths(&provided);
        if let Some(s) = store.as_ref() {
            s.read_in_place(
                &mut provided,
                &dispatch::lazily_read_inputs(&filtered_phase),
            );
        }
        let fetch = recover::input_paths(&provided);
        let lost = recover::lost(
            &mut store,
            &check,
            &fetch,
            pb.as_ref(),
            &all_outputs,
            &cached_node_ids,
            &mut cached_steps,
        )
        .await;
        trace_point!("phase{phase_idx}_inputs_local");
        let lost = match lost {
            Ok(lost) => lost,
            Err(e) => {
                if !recompute {
                    // Decided to run, but never dispatched: say so (see below the loop).
                    queue.push_front(Work::Ready(filtered_phase));
                }
                transfer_error = Some(e);
                break;
            }
        };
        if !lost.is_empty() {
            queue.push_front(if recompute {
                let steps = filtered_phase.streams.iter().flat_map(|s| &s.steps);
                Work::Recompute(steps.flat_map(recover::output_ids).collect())
            } else {
                Work::Ready(filtered_phase)
            });
            queue.push_front(Work::Recompute(cached_steps.first_layer(&lost)));
            continue;
        }

        if recompute {
            // These are no longer served from cache: they run, and are recorded, like any
            // other step of this run.
            let mut lost: Vec<(String, String)> = Vec::new();
            for step in filtered_phase.streams.iter().flat_map(|s| &s.steps) {
                for id in recover::output_ids(step) {
                    recover::mark_recomputed(&mut step_reports, &id, false);
                    cached_node_ids.remove(&id);
                    if let Some(oref) = all_outputs.remove(&id) {
                        lost.push((id, oref.path));
                    }
                }
            }
            for line in recover::recompute_warnings(&lost) {
                note(&pb, &line);
            }
        }

        steps_executed += phase_step_count(&filtered_phase);

        let mut coord = crate::coordinator::Coordinator::new();
        let loaded = coord.load_phase(&filtered_phase, &provided);
        let expected: usize = filtered_phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .map(|st| {
                if st.partition_keys.is_empty() {
                    1
                } else {
                    st.partition_keys.len()
                }
            })
            .sum();
        assert_eq!(
            loaded, expected,
            "plan/coordinator step count mismatch: loaded {loaded}, expected {expected}"
        );

        // Progress callback — update bar as each step completes.
        let run_hashes = &decide_state.run_hashes;
        let on_step_cb: crate::io_loop::StepCallback<'_> = Box::new(
            |node_id: &str, artifact: &serde_json::Value, attempts: u32| {
                // Hand the finished step to the recorder. The worker reports a step only after
                // its artifact is in place (an atomic rename), so the row never points at a
                // missing file. Steps without a run hash are parallel() children, which are
                // never recorded. With a remote store nothing is recorded early: a row is
                // written only once its upload is confirmed, which the end-of-run ledger does.
                if store.is_none()
                    && let Some(run_hash) = run_hashes.get(node_id)
                {
                    recorder.record(StepRow::from_artifact(
                        node_id, run_hash, artifact, attempts,
                    ));
                }
                // Sink failures never fail the asset — surface them prominently.
                if let Some(sinks) = artifact.get("sinks").and_then(|v| v.as_array()) {
                    for s in sinks {
                        if s.get("status").and_then(|v| v.as_str()) == Some("error") {
                            let msg = format!(
                                "[barca] SINK FAILED: {} -> {}: {}",
                                node_id,
                                s.get("path").and_then(|v| v.as_str()).unwrap_or("?"),
                                s.get("error")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown error"),
                            );
                            note(&pb, &msg);
                        }
                    }
                }
                // Upload plan-step artifacts in the background while the run
                // continues. parallel() children (no run hash) are never
                // recorded, so they stay local.
                if let Some(s) = store.as_mut()
                    && decide_state.run_hashes.contains_key(node_id)
                    && let Some(path) = artifact.get("path").and_then(|v| v.as_str())
                    && let Some(at) = s.client.upload(node_id, path)
                {
                    store_paths.insert(node_id.to_string(), at);
                }
                let elapsed_s = artifact.get("elapsed_seconds").and_then(|v| v.as_f64());
                if let Some(e) = elapsed_s {
                    elapsed_so_far += e;
                }
                completed_steps += 1;
                // parallel() children complete as extra steps the plan didn't count: grow the
                // total so the counters and the ETA never run past it.
                let grown = reconcile_total(total_steps, completed_steps);
                if grown != total_steps {
                    total_steps = grown;
                    if let Some(ref bar) = pb {
                        bar.set_length(total_steps as u64);
                    }
                }
                if let Some(ref bar) = pb {
                    bar.set_position(completed_steps as u64);
                    let remaining = if total_estimated > 0.0 {
                        (total_estimated - elapsed_so_far).max(0.0)
                    } else if completed_steps > 0 {
                        let avg = elapsed_so_far / completed_steps as f64;
                        avg * total_steps.saturating_sub(completed_steps) as f64
                    } else {
                        0.0
                    };
                    let short_name = node_id.rsplit(':').next().unwrap_or(node_id);
                    if remaining > 0.5 {
                        bar.set_prefix(format!("{}left", fmt_eta(remaining)));
                    } else {
                        bar.set_prefix("   done ");
                    }
                    bar.set_message(format!("{short_name} done"));
                } else if agent_mode {
                    eprintln!(
                        "[barca] step:{} completed {:.1}s ({}/{}){}",
                        node_id,
                        elapsed_s.unwrap_or(0.0),
                        completed_steps,
                        total_steps,
                        env_suffix(&dag, node_id)
                    );
                }
            },
        );

        // Event sink — buffer log lines for DB persistence, and forward every
        // event live to the caller's channel (the HTTP server) if present.
        let event_tx_phase = event_tx.clone();
        let logs_sink = &mut logs_buffer;
        let on_event_cb: crate::io_loop::EventCallback<'_> =
            Box::new(move |ev: crate::RunEvent| {
                if let crate::RunEvent::Log {
                    ref node_id,
                    ref line,
                } = ev
                {
                    logs_sink.push((node_id.clone(), line.clone()));
                }
                if let Some(ref tx) = event_tx_phase {
                    let _ = tx.send(ev);
                }
            });

        // Drive this phase against the persistent pool. The cost model both
        // sizes the batch pulls and absorbs the timings coming back.
        let phase_err = pool
            .run_phase(
                &mut coord,
                &mut cost_model,
                Some(on_step_cb),
                Some(on_event_cb),
                &cancel,
            )
            .await;
        trace_point!("phase{phase_idx}_run_phase_done");
        if let Err(e) = phase_err
            && phase_error.is_none()
        {
            phase_error = Some(e);
        }

        // Collect results from coordinator
        let mut phase_outputs: HashMap<String, dispatch::OutputRef> = HashMap::new();
        for (&item_id, artifact_val) in coord.outputs() {
            let item = coord.item(item_id);
            let node_id = item.step_id.display();
            // Attempts made for this item (dispatch count), keyed by base node
            // id — how the success-row INSERT looks it up.
            all_attempts.insert(
                crate::StepId::parse(&node_id).base_id().to_string(),
                item.attempts,
            );
            let oref = dispatch::OutputRef {
                path: artifact_val
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                format: artifact_val
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("json")
                    .to_string(),
                size_bytes: artifact_val
                    .get("size_bytes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                elapsed_seconds: artifact_val.get("elapsed_seconds").and_then(|v| v.as_f64()),
                content_hash: artifact_val
                    .get("content_hash")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            };
            if let Some(sinks) = artifact_val.get("sinks").and_then(|v| v.as_array())
                && !sinks.is_empty()
            {
                all_sinks.insert(
                    node_id.clone(),
                    serde_json::Value::from(sinks.clone()).to_string(),
                );
            }
            let cpu = artifact_val.get("cpu_seconds").and_then(|v| v.as_f64());
            let rss = artifact_val.get("max_rss_bytes").and_then(|v| v.as_u64());
            if cpu.is_some() || rss.is_some() {
                all_timings.insert(node_id.clone(), (cpu, rss));
            }
            if let (Some(finished), Some(wall)) = (
                artifact_val.get("finished_at").and_then(|v| v.as_f64()),
                artifact_val.get("wall_seconds").and_then(|v| v.as_f64()),
            ) {
                step_clocks.insert(node_id.clone(), (finished, wall));
            }
            // A sensor's output hash: its consumers, decided in a later phase, fold it into
            // their run hashes (#183).
            if dag
                .get_node(item.step_id.base_id())
                .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
                && decide_state.run_hashes.contains_key(&node_id)
                && let Some(h) = artifact_val.get("content_hash").and_then(|v| v.as_str())
            {
                decide_state
                    .sensor_outputs
                    .insert(node_id.clone(), h.to_string());
            }
            phase_outputs.insert(node_id, oref);
        }

        // Collect failures — parallel branch failures (group members) are
        // contained within the group and surfaced as ParallelError to the parent,
        // so they should NOT abort the entire phase.
        //
        // A Ctrl-C in a terminal reaches the workers as well as this process, and a worker
        // may report its step's KeyboardInterrupt before the cancellation is seen here. That
        // step was interrupted, not failed: it gets no `failed` line and no failed row. When
        // such a report came first, give this process's own signal a moment to arrive.
        let interrupted = |message: &str| message.starts_with("KeyboardInterrupt");
        if !cancel.is_cancelled() && coord.failed_items().iter().any(|(_, m)| interrupted(m)) {
            let grace = std::time::Duration::from_millis(250);
            let _ = tokio::time::timeout(grace, cancel.cancelled()).await;
        }
        let cancelled = cancel.is_cancelled();
        let mut first_non_group_failure: Option<(String, String)> = None;
        for (item_id, error_msg) in coord.failed_items() {
            let item = coord.item(item_id);
            if item.group.is_some() {
                // Parallel branch failure — handled by the group/parent, not a phase error.
                continue;
            }
            if cancelled && interrupted(error_msg) {
                continue;
            }
            let node_id = item.step_id.display();
            if error_msg.starts_with(BLOCKED_ARTIFACT_PATH) {
                // The step ran; its result could not be written because barca's artifact
                // directory is in a state barca cannot repair. Infrastructure (exit 3), not a
                // failed step: there is nothing in the step to fix.
                let why = error_msg
                    .strip_prefix(BLOCKED_ARTIFACT_PATH)
                    .map_or(error_msg, |m| m.trim_start_matches([':', ' ']));
                transfer_error.get_or_insert(format!(
                    "the result of {node_id} could not be written: {why}"
                ));
                failed_bases.insert(item.step_id.base_id().to_string());
                continue;
            }
            if agent_mode {
                eprintln!("{}", failed_step_line(&node_id, error_msg));
            }
            if first_non_group_failure.is_none() {
                first_non_group_failure = Some((node_id.clone(), error_msg.to_string()));
            }
            failed_bases.insert(item.step_id.base_id().to_string());
            all_failures.push(dispatch::StepFailure {
                node_id,
                error: dispatch::StepError {
                    error_type: "WorkerError".to_string(),
                    message: error_msg.to_string(),
                    traceback: String::new(),
                    attempts: item.attempts,
                },
            });
        }

        // Steps the coordinator skipped because an in-phase dependency failed.
        // They never executed, so they do not count in `steps_executed`.
        for item_id in coord.skipped_items() {
            let item = coord.item(item_id);
            if item.group.is_none() {
                skipped_bases.insert(item.step_id.base_id().to_string());
                steps_executed = steps_executed.saturating_sub(1);
            }
        }

        // Remember the first step failure (it does not overwrite an infrastructure error).
        if let Some(msg) = first_non_group_failure
            && step_failure.is_none()
        {
            step_failure = Some(msg);
        }

        // Run hashes were computed pre-dispatch for every plan step (including
        // per-partition ids), so collection is a filter: coordinator outputs
        // with a known hash are plan steps; the rest are parallel() children,
        // which are never persisted.
        for (node_id, oref) in &phase_outputs {
            if decide_state.run_hashes.contains_key(node_id) {
                all_outputs.insert(node_id.clone(), oref.clone());
            }
        }

        // Stop after collecting partial results if the pool itself failed, or if a step failed
        // and there is only one target (several targets keep going around the failure).
        if phase_error.is_some()
            || transfer_error.is_some()
            || (step_failure.is_some() && !keep_going)
        {
            break;
        }
    }

    // Whatever is still queued when the loop ends was never dispatched, whatever ended it (a
    // failed step, a failed recompute, cancellation, an infrastructure error). A step that had
    // been decided to run and was waiting for an input must not be reported as having run: it
    // is skipped, like a step the coordinator never started.
    for work in queue.drain(..) {
        if let Work::Ready(phase) = work {
            for step in phase.streams.iter().flat_map(|s| &s.steps) {
                skipped_bases.insert(step.step_id.base_id().to_string());
            }
        }
    }
    // A cached step whose `cached` line was held back and that was not computed again after
    // all is announced now.
    for id in held_cached_lines {
        if cached_node_ids.contains(&id) {
            eprintln!("{}", cached_step_line(&dag, &id));
        }
    }

    // All phases done (or aborted/cancelled) — release the worker pool before
    // persisting.
    let repeated_warnings = pool.take_repeated_warnings();
    pool.shutdown().await;
    trace_point!("pool_shutdown");

    // Finish progress bar. The end-of-run line is the same with and without --agent.
    if let Some(ref bar) = pb {
        bar.finish_and_clear();
    }
    // Library warnings the workers printed once and then suppressed (every mode).
    // The ten most repeated get a line each, so the summary cannot become the noise it removes.
    const SUMMARY_LINES: usize = 10;
    for (text, n) in repeated_warnings.iter().take(SUMMARY_LINES) {
        eprintln!("[barca] {n} more: {text}");
    }
    if repeated_warnings.len() > SUMMARY_LINES {
        let rest = &repeated_warnings[SUMMARY_LINES..];
        eprintln!(
            "[barca] {} more: {} other repeated warnings",
            rest.iter().map(|(_, n)| n).sum::<u64>(),
            rest.len()
        );
    }
    if (pb.is_some() || agent_mode) && steps_executed > 0 {
        let outcome = if cancel.is_cancelled() {
            RunOutcome::Cancelled
        } else if phase_error.is_some() || step_failure.is_some() {
            RunOutcome::Failed
        } else {
            RunOutcome::Done
        };
        eprintln!(
            "{}",
            end_of_run_line(completed_steps, total_steps, elapsed_so_far, outcome)
        );
    }

    let mut was_cancelled = cancel.is_cancelled();

    // Artifact store, before anything is recorded: wait for every upload. Rows are recorded
    // only for artifacts confirmed in the store, so the metadata never points at a missing
    // object. The transfer client stays up to fetch the final outputs below.
    //
    // Ctrl-C during the wait cancels the run like one during a step: the uploads still in
    // flight are abandoned and their steps are not recorded.
    let drained = match store.as_mut() {
        Some(s) if !was_cancelled => {
            let queued = s.client.pending_uploads();
            let t_drain = Instant::now();
            let report = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                report = s.client.drain() => Some(report),
            };
            trace_point!("store_sync_drained ({queued} uploads)");
            was_cancelled = report.is_none();
            report.map(|report| (report, t_drain))
        }
        _ => None,
    };
    if was_cancelled {
        if let Some(s) = store.take() {
            let (unconfirmed, hashes) = s.client.abort().await;
            for node in unconfirmed {
                all_outputs.remove(&node);
                store_paths.remove(&node);
            }
            for (node, sha256) in hashes {
                if let Some(oref) = all_outputs.get_mut(&node) {
                    oref.content_hash.get_or_insert(sha256);
                }
            }
        }
    } else if let Some((report, t_drain)) = drained {
        // The hash of the bytes that reached the store is recorded with the row, so any
        // machine can check its copy of the artifact against it.
        for (node, sha256) in &report.hashes {
            if let Some(oref) = all_outputs.get_mut(node) {
                oref.content_hash.get_or_insert_with(|| sha256.clone());
            }
        }
        if report.transferred > 0 {
            eprintln!(
                "[barca] uploaded {} artifact{} ({}); waited {:.1}s at end of run",
                report.transferred,
                if report.transferred == 1 { "" } else { "s" },
                fmt_bytes(report.bytes),
                t_drain.elapsed().as_secs_f64()
            );
        }
        if !report.failures.is_empty() {
            let mut detail = Vec::new();
            for f in &report.failures {
                all_outputs.remove(&f.key);
                store_paths.remove(&f.key);
                failed_bases.insert(crate::StepId::parse(&f.key).base_id().to_string());
                all_failures.push(dispatch::StepFailure {
                    node_id: f.key.clone(),
                    error: dispatch::StepError {
                        error_type: "UploadError".to_string(),
                        message: format!("upload to {} failed: {}", f.store, f.message),
                        traceback: String::new(),
                        attempts: f.attempts,
                    },
                });
                detail.push(format!("  {} ({}): {}", f.key, f.store, f.message));
            }
            let messages: Vec<&str> = report.failures.iter().map(|f| f.message.as_str()).collect();
            transfer_error.get_or_insert(format!(
                "{} artifact upload(s) failed — those steps were not recorded and \
                 will recompute next run:\n{}{}",
                report.failures.len(),
                detail.join("\n"),
                match transfer_remedy(&messages, "") {
                    remedy if remedy.is_empty() => remedy,
                    remedy => format!("\n{remedy}"),
                }
            ));
        }
    }

    let steps_cached = cached_node_ids.len();
    let elapsed = t0.elapsed().as_secs_f64();

    // Stop the step recorder before persistence: the ledger below writes whatever it had not
    // written yet, and the state push checkpoints the WAL, which requires no other open handle
    // on the file.
    recorder.finish().await;
    trace_point!("recorder_stopped");

    // Persist all executed outputs (including partial results on failure) —
    // held in a ledger so a state-push conflict can replay this run's rows
    // onto a freshly pulled database.
    let cost_snapshot: Vec<(String, crate::cost::NodeEstimate)> = cost_model
        .snapshot()
        .map(|(node_id, est)| (node_id.clone(), *est))
        .collect();
    let mut ledger = RunLedger {
        run_id: &run_id,
        status: if was_cancelled {
            "cancelled"
        } else if phase_error.is_some() || step_failure.is_some() || transfer_error.is_some() {
            "failed"
        } else {
            "success"
        },
        command: command_label,
        files: db::encode_files(file_args),
        target: target_label.as_deref(),
        steps_total: exec_plan.total_steps,
        steps_executed,
        steps_cached,
        elapsed,
        all_outputs: &all_outputs,
        all_failures: &all_failures,
        all_sinks: &all_sinks,
        all_attempts: &all_attempts,
        all_timings: &all_timings,
        cached_node_ids: &cached_node_ids,
        run_hashes: &decide_state.run_hashes,
        output_hashes: &decide_state.sensor_outputs,
        store_paths: &store_paths,
        cost_snapshot: &cost_snapshot,
    };
    persist_run(&db_path, &ledger).await?;
    // Persist captured output. Rust owns persistence — logs land in the DB
    // regardless of how the run was triggered (CLI or server).
    db::insert_logs(&db_path, &run_id, &logs_buffer).await?;
    trace_point!("persist_run_done");

    if !telemetry.is_empty() {
        let report = telemetry_report(&ledger, &dag, run_started, &step_clocks, &job_name);
        crate::telemetry::export(&telemetry, &report).await;
        trace_point!("telemetry_exported");
    }

    // Shared remote state: upload the local history (`push_state` folds the WAL in and copies
    // it under the database lock). See `SharedPush::run` for conflicts.
    if state_sync_on {
        let mut push = SharedPush {
            python,
            cfg,
            db_path: &db_path,
            run_id: &run_id,
            logs: &logs_buffer,
            token: state_token.take().expect("pulled when state sync is on"),
        };
        let t_push = Instant::now();
        let mut pushed: Option<u32> = None;
        if !was_cancelled {
            // Ctrl-C stops the push. The run's work is done and recorded, but the command is
            // cancelled before its record was shared, so the record says `cancelled`; the
            // wrap-up below then tries to share that.
            match push
                .run(&ledger, state_sync::Until::cancelled(&cancel))
                .await
            {
                Ok(retries) => pushed = Some(retries),
                Err(BarcaError::Cancelled) => {
                    // The ledger too, so that a replay after a conflict keeps the mark.
                    ledger.status = "cancelled";
                    cancel_recorded_run(&db_path, &ledger).await?;
                    was_cancelled = true;
                }
                Err(e) => return Err(e),
            }
        }
        if was_cancelled && pushed.is_none() {
            // Wrap-up of a cancelled run: what it finished is worth sharing, so that other
            // machines do not compute it again, but nobody who pressed Ctrl-C should wait on
            // a slow store. The push gets `WRAP_UP_LIMIT`, and a second Ctrl-C ends it at
            // once. Nothing is lost when it does not finish: the record is in the local
            // history, a pull keeps what was recorded only here, and the next run on this
            // machine uploads it.
            let limit = crate::interrupt::WRAP_UP_LIMIT;
            let until = state_sync::Until {
                cancel: Some(&interrupt.abandon),
                deadline: Some(Instant::now() + limit),
            };
            let why = match push.run(&ledger, until).await {
                Ok(retries) => {
                    pushed = Some(retries);
                    None
                }
                Err(BarcaError::Cancelled) if interrupt.abandon.is_cancelled() => {
                    Some("stopped by a second Ctrl-C".to_string())
                }
                Err(BarcaError::Cancelled) => Some(format!(
                    "the upload did not finish within {}s",
                    limit.as_secs()
                )),
                // The run is cancelled whatever became of the push: say why, do not fail.
                Err(e) => Some(e.to_string().lines().next().unwrap_or_default().to_string()),
            };
            if let Some(why) = why {
                eprintln!(
                    "[barca] the shared history was not updated ({why}). This run is recorded \
                     on this machine; the next barca get or barca run here uploads it."
                );
            }
        }
        if let Some(retries) = pushed {
            eprintln!(
                "[barca] pushed state ({}) in {:.2}s{}",
                fmt_bytes(std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)),
                t_push.elapsed().as_secs_f64(),
                match retries {
                    0 => String::new(),
                    1 => " after 1 conflict retry".to_string(),
                    n => format!(" after {n} conflict retries"),
                }
            );
            trace_point!("state_sync_pushed (attempts={})", retries + 1);
        }
    }

    // Propagate cancellation/worker error after persisting partial results.
    if was_cancelled {
        return Err(BarcaError::Cancelled);
    }
    if let Some(error) = phase_error {
        // Only the pool itself (e.g. no worker could be spawned) sets `phase_error`; user step
        // failures are in `step_failure`. Infra, not user code: exit 3.
        return Err(BarcaError::Other(format!("Worker failed: {error}")));
    }
    if let Some(error) = transfer_error.take() {
        return Err(BarcaError::Other(error));
    }

    // Steps reported as `ran` before dispatch that failed, or were skipped because something
    // upstream failed, say so.
    let mut step_reports = merge_partition_reports(step_reports);
    for report in &mut step_reports {
        if !matches!(report.status.as_deref(), Some("ran" | "partial")) {
            continue;
        }
        let base = report.id.split('[').next().unwrap_or(&report.id);
        if failed_bases.contains(base) {
            report.status = Some("failed".to_string());
        } else if skipped_bases.contains(base) {
            report.status = Some("skipped".to_string());
            report.reason = Some("upstream_failed".to_string());
            report.detail = Some("a step it depends on failed".to_string());
        }
    }

    let outcomes = target_outcomes(&dag, &targets, &all_outputs, &all_failures);

    let final_output = final_output_of(&exec_plan, &target_ids, keep_going, &all_outputs);

    // The outputs this command returns were made readable here when the last phase finished
    // (`Work::Returned`). A run that stopped at a failed step did not get that far and may still
    // return an earlier output, so make sure of it. Then stop the transfer helper.
    if let Some(mut s) = store.take() {
        let paths: Vec<String> = final_output
            .iter()
            .chain(outcomes.iter().filter_map(|(_, o)| o.final_output.as_ref()))
            .map(|o| o.path.clone())
            .collect();
        let fetched = s.ensure_local(paths.iter().map(String::as_str), None).await;
        if fetched.is_err() && cancel.is_cancelled() {
            // Ctrl-C while the returned output was being fetched. The run itself is over:
            // its record is written and, with shared history, uploaded, and it stays as it
            // is, the same on every machine. Only the command is cancelled.
            s.client.abort().await;
            return Err(BarcaError::Cancelled);
        }
        s.client.shutdown().await;
        if let Err(e) = fetched {
            return Err(BarcaError::Other(e));
        }
        crate::mismatch::mark(&dag, &mut step_reports, &s.mismatched);
    }

    Ok(Executed {
        result: GetResult {
            run_id,
            elapsed_seconds: elapsed,
            steps_executed,
            phases: exec_plan.phases.len(),
            final_output,
            steps: step_reports,
            warnings: plan_warnings,
        },
        targets: outcomes,
        step_failure: step_failure.map(|(node, message)| crate::FailedStep {
            artifact_dir: Some(format!(
                "{}/{}",
                cfg.artifact_root.trim_end_matches('/'),
                crate::safe_node_id(&node)
            )),
            node,
            message,
            run: None,
        }),
    })
}

// ─── run persistence ──────────────────────────────────────────────────────────

/// The upload of one run's record to the shared history.
struct SharedPush<'a> {
    python: &'a std::path::Path,
    cfg: &'a crate::config::ResolvedConfig,
    db_path: &'a str,
    run_id: &'a str,
    logs: &'a [(String, String)],
    /// The token of the shared history the local one was last brought up to.
    token: state_sync::StateToken,
}

impl SharedPush<'_> {
    /// Upload the local history. On conflict (another machine pushed first): pull the fresh
    /// history, replay this run's ledger onto it, and upload again, up to `push_retries`
    /// times. Returns the number of retries. `Err(BarcaError::Cancelled)` when `until` stopped
    /// it; the shared history is then the old one or the new one, never part of one.
    async fn run(
        &mut self,
        ledger: &RunLedger<'_>,
        until: state_sync::Until<'_>,
    ) -> Result<u32, BarcaError> {
        let (python, cfg) = (self.python, self.cfg);
        let mut attempt = 0u32;
        let mut pushed_again = false;
        loop {
            let again = match state_sync::push_state(python, cfg, &self.token, until).await? {
                state_sync::PushOutcome::Pushed {
                    local_unchanged: true,
                    ..
                } => false,
                // Uploaded, but another process wrote to the local database (or replaced
                // it) while the upload was on its way. Treated like a conflict, once: pull
                // what was just uploaded, which keeps those rows, and push again. Only once,
                // because a run going in the same project writes during every upload, and
                // chasing it would cost a pull and an upload each time for rows that run
                // pushes itself when it ends. The upload stands either way; rows written
                // after it go with the next push from this machine.
                state_sync::PushOutcome::Pushed { .. } => {
                    !std::mem::replace(&mut pushed_again, true) && attempt < cfg.push_retries
                }
                state_sync::PushOutcome::Conflict => {
                    if attempt >= cfg.push_retries {
                        return Err(BarcaError::Other(format!(
                            "shared state push conflicted {attempt} times — results were \
                             computed but the shared state was not updated; re-run to retry"
                        )));
                    }
                    true
                }
            };
            if !again {
                return Ok(attempt);
            }
            attempt += 1;
            // The pull carries this run's rows over with the rest of the local database; the
            // ledger then adds whatever is still missing (both are idempotent), so the run
            // is whole however much of it made the trip.
            self.token = state_sync::pull_state(python, cfg, until).await?.token;
            db::init_db(self.db_path).await?;
            persist_run(self.db_path, ledger).await?;
            db::insert_logs(self.db_path, self.run_id, self.logs).await?;
        }
    }
}

/// Record as `cancelled` a run that was already recorded with its outcome, because Ctrl-C
/// arrived while its record was being shared. The steps it recorded stay: they finished.
async fn cancel_recorded_run(db_path: &str, l: &RunLedger<'_>) -> Result<(), BarcaError> {
    db::finish_run(
        db_path,
        l.run_id,
        "cancelled",
        l.steps_executed,
        l.steps_cached,
        l.elapsed,
    )
    .await
}

/// Everything one run wants written to the metadata DB, held in memory so a
/// state-push conflict can replay it onto a freshly pulled database.
struct RunLedger<'a> {
    run_id: &'a str,
    status: &'a str,
    command: &'a str,
    files: String,
    target: Option<&'a str>,
    steps_total: usize,
    steps_executed: usize,
    steps_cached: usize,
    elapsed: f64,
    all_outputs: &'a HashMap<String, dispatch::OutputRef>,
    all_failures: &'a [dispatch::StepFailure],
    all_sinks: &'a HashMap<String, String>,
    all_attempts: &'a HashMap<String, u32>,
    /// Per-node worker self-timing: (cpu_seconds, max_rss_bytes).
    all_timings: &'a HashMap<String, (Option<f64>, Option<u64>)>,
    cached_node_ids: &'a std::collections::HashSet<String>,
    run_hashes: &'a HashMap<String, String>,
    /// Sensor step -> content hash of the output it returned in this run. Other steps
    /// record the hash on their `OutputRef`, when the artifact went through a store.
    output_hashes: &'a HashMap<String, String>,
    /// Artifact-store location of each uploaded output, recorded instead of
    /// its local path so cache hits resolve on every machine.
    store_paths: &'a HashMap<String, String>,
    /// Run-end snapshot of the measured-cost EWMA, seeding the next run.
    cost_snapshot: &'a [(String, crate::cost::NodeEstimate)],
}

/// One successful step as a `materializations` row. Built when the step finishes (for the
/// [`StepRecorder`]) and again from the ledger at the end of the run.
#[derive(Debug, Clone)]
struct StepRow {
    node_id: String,
    run_hash: String,
    path: String,
    format: String,
    size_bytes: u64,
    elapsed_seconds: Option<f64>,
    attempts: u32,
    sinks_json: Option<String>,
    cpu_seconds: Option<f64>,
    max_rss_bytes: Option<u64>,
    /// A sensor's output content hash; None for everything else.
    output_hash: Option<String>,
}

impl StepRow {
    /// From the artifact a worker reported for a finished step. Reads the same fields the
    /// end-of-run ledger is built from, so both writers produce the same row.
    fn from_artifact(
        node_id: &str,
        run_hash: &str,
        artifact: &serde_json::Value,
        attempts: u32,
    ) -> Self {
        let str_of = |key: &str| artifact.get(key).and_then(|v| v.as_str());
        Self {
            node_id: node_id.to_string(),
            run_hash: run_hash.to_string(),
            path: str_of("path").unwrap_or("").to_string(),
            format: str_of("format").unwrap_or("json").to_string(),
            size_bytes: artifact
                .get("size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            elapsed_seconds: artifact.get("elapsed_seconds").and_then(|v| v.as_f64()),
            attempts,
            sinks_json: artifact
                .get("sinks")
                .and_then(|v| v.as_array())
                .filter(|sinks| !sinks.is_empty())
                .map(|sinks| serde_json::Value::from(sinks.clone()).to_string()),
            cpu_seconds: artifact.get("cpu_seconds").and_then(|v| v.as_f64()),
            max_rss_bytes: artifact.get("max_rss_bytes").and_then(|v| v.as_u64()),
            output_hash: str_of("content_hash").map(str::to_string),
        }
    }

    async fn insert(&self, conn: &turso::Connection, run_id: &str) -> Result<(), turso::Error> {
        let opt = |v: Option<String>| v.unwrap_or_default();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes, output_hash, run_id) VALUES (?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), ?12)",
            [
                self.node_id.clone(),
                self.run_hash.clone(),
                self.path.clone(),
                self.format.clone(),
                self.size_bytes.to_string(),
                opt(self.elapsed_seconds.map(|e| e.to_string())),
                self.attempts.to_string(),
                opt(self.sinks_json.clone()),
                opt(self.cpu_seconds.map(|c| c.to_string())),
                opt(self.max_rss_bytes.map(|r| r.to_string())),
                opt(self.output_hash.clone()),
                run_id.to_string(),
            ],
        )
        .await
        .map(|_| ())
    }
}

/// How often, at most, the recorder writes finished steps to the metadata DB. A finished step
/// is recorded within about this long of finishing; a run shorter than this records everything
/// in the one end-of-run write, exactly as before, so short runs pay nothing.
const RECORD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Writes finished steps to the local metadata DB while a run is still going (#214), so
/// `barca status` in another process sees them and a killed run keeps them.
///
/// The run loop only sends rows down a channel; a background task batches whatever has
/// arrived and writes it with a short-lived connection, at most once per [`RECORD_INTERVAL`].
/// The loop therefore never waits on the database (another barca process may be holding it),
/// and a phase of thousands of quick steps costs a few writes, not thousands.
///
/// It is an optimisation of *when* rows land, never the only writer: the end-of-run
/// [`persist_run`] writes every row of the run that is not already there (rows carry the run
/// id). So a write that fails here and rows still queued at [`StepRecorder::finish`] are made
/// good at the end. (A pull of the shared state by another process mid-run keeps the rows
/// written here: see `state_carry`.)
struct StepRecorder {
    tx: tokio::sync::mpsc::UnboundedSender<StepRow>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl StepRecorder {
    fn start(db_path: String, run_id: String) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StepRow>();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let mut next_write = tokio::time::Instant::now() + RECORD_INTERVAL;
            loop {
                let first = tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    row = rx.recv() => match row {
                        Some(row) => row,
                        None => break,
                    },
                };
                tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    _ = tokio::time::sleep_until(next_write) => {}
                }
                let mut batch = vec![first];
                while let Ok(row) = rx.try_recv() {
                    batch.push(row);
                }
                // Best effort: see the type's doc comment for why a failure is not an error.
                record_steps(&db_path, &run_id, &batch).await.ok();
                next_write = tokio::time::Instant::now() + RECORD_INTERVAL;
            }
        });
        Self { tx, stop, task }
    }

    /// Queue a finished step. Never blocks.
    fn record(&self, row: StepRow) {
        self.tx.send(row).ok();
    }

    /// Stop the background task and wait for it, so no connection is left open. Rows it had
    /// not written yet are left to [`persist_run`].
    async fn finish(self) {
        self.stop.cancel();
        self.task.await.ok();
    }
}

/// Append `rows` for a run that is still in progress, and advance its `steps_executed` so
/// `barca history` shows how far it has got.
async fn record_steps(db_path: &str, run_id: &str, rows: &[StepRow]) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    // One transaction: one commit for the batch, and a reader sees all of it or none.
    conn.execute("BEGIN", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin: {e}")))?;
    let mut written = 0usize;
    for row in rows {
        if row.insert(&conn, run_id).await.is_ok() {
            written += 1;
        }
    }
    conn.execute(
        "UPDATE runs SET steps_executed = steps_executed + ?1 WHERE run_id = ?2 AND status = 'running'",
        [written.to_string(), run_id.to_string()],
    )
    .await
    .ok();
    conn.execute("COMMIT", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to commit: {e}")))?;
    Ok(())
}

/// The exception a step failure carries, as (type, message, traceback). A worker reports a
/// Python exception as the generic `WorkerError` whose text is `Type: message` followed by the
/// traceback frames; the type is what groups errors in a telemetry backend.
fn exception_of(error: &dispatch::StepError) -> (String, String, Option<String>) {
    // The frames are the trailing run of `  File "..."` lines and their indented source lines.
    // Taking only that run keeps a message that itself quotes a traceback in one piece.
    let lines: Vec<&str> = error.message.split('\n').collect();
    let mut first_frame = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        if line.starts_with("  File \"") {
            first_frame = i;
        } else if !line.starts_with("    ") {
            break;
        }
    }
    let joined;
    let (text, frames) = if first_frame > 0 && first_frame < lines.len() {
        joined = lines[..first_frame].join("\n");
        (joined.as_str(), Some(lines[first_frame..].join("\n")))
    } else {
        (error.message.as_str(), None)
    };
    let stack = Some(error.traceback.clone())
        .filter(|t| !t.is_empty())
        .or(frames);
    if error.error_type == "WorkerError"
        && let Some((head, rest)) = text.split_once(": ")
        && !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return (head.to_string(), rest.to_string(), stack);
    }
    (error.error_type.clone(), text.to_string(), stack)
}

/// Group resolved ids consistently in APM. The run ledger retains the
/// original target spelling for CLI/history compatibility.
fn canonical_job(target_ids: &[&str]) -> String {
    let mut ids = target_ids.to_vec();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        "all".to_string()
    } else {
        ids.join(",")
    }
}

/// The run as telemetry integrations see it: every step that ran, was served from cache, or
/// failed. A step that ran is placed by the worker's own clock; a cached step is a zero-length
/// mark at the start of the run and a failed one at its end, since neither reports a time.
fn telemetry_report(
    l: &RunLedger<'_>,
    dag: &Dag,
    started: std::time::SystemTime,
    clocks: &HashMap<String, (f64, f64)>,
    job: &str,
) -> crate::telemetry::RunReport {
    use crate::telemetry::{RunReport, StepOutcome, StepReport};

    let ns = |seconds: f64| (seconds.max(0.0) * 1e9) as u64;
    let start_unix_ns = started
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let duration_ns = ns(l.elapsed);
    let kind = |node_id: &str| match dag
        .get_node(crate::StepId::parse(node_id).base_id())
        .map(|n| n.kind())
    {
        Some(crate::NodeKind::Task) => "task",
        Some(crate::NodeKind::Sensor) => "sensor",
        Some(crate::NodeKind::Asset) | None => "asset",
    };
    // Attempts are counted per step, not per partition key, so they are only attributed to
    // an unpartitioned step.
    let attempts = |node_id: &str| {
        let id = crate::StepId::parse(node_id);
        (id.display() == id.base_id())
            .then(|| l.all_attempts.get(id.base_id()).copied().unwrap_or(1))
    };

    let mut steps: Vec<StepReport> = Vec::new();
    for (node_id, oref) in l.all_outputs {
        let cached = l.cached_node_ids.contains(node_id);
        let (step_start, step_duration) = match clocks.get(node_id) {
            _ if cached => (start_unix_ns, 0),
            Some((finished, wall)) => (ns(finished - wall), ns(*wall)),
            None => (start_unix_ns, ns(oref.elapsed_seconds.unwrap_or(0.0))),
        };
        let (cpu_seconds, max_rss_bytes) =
            l.all_timings.get(node_id).copied().unwrap_or((None, None));
        steps.push(StepReport {
            node_id: node_id.clone(),
            kind: kind(node_id),
            outcome: if cached {
                StepOutcome::Cached
            } else {
                StepOutcome::Ran
            },
            start_unix_ns: step_start,
            duration_ns: step_duration,
            attempts: if cached { None } else { attempts(node_id) },
            run_hash: l.run_hashes.get(node_id).cloned(),
            size_bytes: Some(oref.size_bytes),
            cpu_seconds,
            max_rss_bytes,
            error_type: None,
            error_message: None,
            error_traceback: None,
        });
    }
    for failure in l.all_failures {
        let (error_type, error_message, error_traceback) = exception_of(&failure.error);
        steps.push(StepReport {
            node_id: failure.node_id.clone(),
            kind: kind(&failure.node_id),
            outcome: StepOutcome::Failed,
            start_unix_ns: start_unix_ns + duration_ns,
            duration_ns: 0,
            attempts: Some(failure.error.attempts),
            run_hash: l.run_hashes.get(&failure.node_id).cloned(),
            size_bytes: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            error_type: Some(error_type),
            error_message: Some(error_message),
            error_traceback,
        });
    }
    steps.sort_by(|a, b| (a.start_unix_ns, &a.node_id).cmp(&(b.start_unix_ns, &b.node_id)));

    RunReport {
        run_id: l.run_id.to_string(),
        command: l.command.to_string(),
        target: l.target.map(str::to_string),
        job: job.to_string(),
        status: l.status.to_string(),
        start_unix_ns,
        duration_ns,
        steps_total: l.steps_total,
        steps_executed: l.steps_executed,
        steps_cached: l.steps_cached,
        steps,
    }
}

/// Write a run's ledger with a short-lived connection. Idempotent, so it is safe after the
/// [`StepRecorder`] has written some of the steps and on a replay: the run row is INSERT OR
/// IGNORE + terminal UPDATE, and a step is appended only if this run has no row for it yet
/// (a step has one outcome per run).
async fn persist_run(db_path: &str, l: &RunLedger<'_>) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;

    conn.execute(
            "INSERT OR IGNORE INTO runs (run_id, command, files, target, status, steps_total, pid, host) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
            [
                l.run_id.to_string(),
                l.command.to_string(),
                l.files.clone(),
                l.target.unwrap_or("").to_string(),
                l.steps_total.to_string(),
                std::process::id().to_string(),
                db::local_host(),
            ],
        )
        .await
        .ok();
    conn.execute(
            "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, elapsed_seconds = ?4, finished_at = datetime('now') WHERE run_id = ?5",
            [
                l.status.to_string(),
                l.steps_executed.to_string(),
                l.steps_cached.to_string(),
                l.elapsed.to_string(),
                l.run_id.to_string(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to finish run: {e}")))?;

    // What the [`StepRecorder`] wrote during the run, or, on a replay after a shared-state
    // conflict, what the pulled database holds of this run.
    let already = crate::state_carry::steps_of_run(&conn, l.run_id).await;

    for (node_id, oref) in l.all_outputs {
        if l.cached_node_ids.contains(node_id) || already.contains(node_id) {
            continue;
        }
        let Some(run_h) = l.run_hashes.get(node_id) else {
            continue;
        };
        let base = crate::StepId::parse(node_id).base_id().to_string();
        let (cpu, rss) = l.all_timings.get(node_id).copied().unwrap_or((None, None));
        let mut row = StepRow::from_artifact(node_id, run_h, &serde_json::Value::Null, 1);
        row.path = l.store_paths.get(node_id).unwrap_or(&oref.path).clone();
        row.format = oref.format.clone();
        row.size_bytes = oref.size_bytes;
        row.elapsed_seconds = oref.elapsed_seconds;
        row.attempts = l.all_attempts.get(&base).copied().unwrap_or(1);
        row.sinks_json = l.all_sinks.get(node_id).cloned();
        row.cpu_seconds = cpu;
        row.max_rss_bytes = rss;
        row.output_hash = l
            .output_hashes
            .get(node_id)
            .or(oref.content_hash.as_ref())
            .cloned();
        row.insert(&conn, l.run_id).await.ok();
    }

    // Persist the measured-cost EWMA so the next run starts pre-warmed and
    // skips the cold-start probe entirely. (Inline — this fn already holds
    // the process-wide DB guard.)
    for (node_id, est) in l.cost_snapshot {
        let base = crate::StepId::parse(node_id).base_id().to_string();
        conn.execute(
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(node_id) DO UPDATE SET
                 estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
                 samples = ?6, updated_at = datetime('now')",
            [
                node_id.clone(),
                base,
                est.estimate_seconds.to_string(),
                est.cpu_seconds.to_string(),
                est.max_rss_bytes.to_string(),
                est.samples.to_string(),
            ],
        )
        .await
        .ok();
    }

    // Persist permanently-failed steps as `status='failed'` rows (artifact
    // columns NULL). Failed rows are never served as cache hits.
    for failure in l.all_failures {
        let node_id = &failure.node_id;
        if already.contains(node_id) {
            continue;
        }
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        // Each failure carries its own attempt count: dispatches for a worker
        // failure, transfer attempts for an upload failure.
        conn.execute(
                "INSERT INTO materializations (node_id, run_hash, status, error_type, error_message, error_traceback, attempts, run_id) VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6, ?7)",
                [
                    node_id.clone(),
                    run_h,
                    failure.error.error_type.clone(),
                    failure.error.message.clone(),
                    failure.error.traceback.clone(),
                    failure.error.attempts.to_string(),
                    l.run_id.to_string(),
                ],
            )
            .await
            .ok();
    }
    Ok(())
}

#[cfg(test)]
mod persist_tests {
    use super::*;
    use std::collections::HashSet;

    /// A run that executed `a`, `part[k=1]` and `part[k=2]`, found `cached` in the cache, and
    /// saw `bad` fail.
    struct Fixture {
        outputs: HashMap<String, OutputRef>,
        failures: Vec<dispatch::StepFailure>,
        cached: HashSet<String>,
        run_hashes: HashMap<String, String>,
        empty: HashMap<String, String>,
        attempts: HashMap<String, u32>,
        timings: HashMap<String, (Option<f64>, Option<u64>)>,
    }

    const EXECUTED: [&str; 3] = ["f.py:a", "f.py:part[k=1]", "f.py:part[k=2]"];

    fn oref(node: &str) -> OutputRef {
        OutputRef {
            path: format!(".barca/artifacts/{node}/h.json"),
            format: "json".to_string(),
            size_bytes: 2,
            elapsed_seconds: Some(0.5),
            content_hash: None,
        }
    }

    impl Fixture {
        fn new() -> Self {
            let mut outputs = HashMap::new();
            let mut run_hashes = HashMap::new();
            for node in EXECUTED.iter().chain(&["f.py:cached", "f.py:bad"]) {
                run_hashes.insert(node.to_string(), format!("hash-{node}"));
                if *node != "f.py:bad" {
                    outputs.insert(node.to_string(), oref(node));
                }
            }
            Self {
                outputs,
                failures: vec![dispatch::StepFailure {
                    node_id: "f.py:bad".to_string(),
                    error: dispatch::StepError {
                        error_type: "WorkerError".to_string(),
                        message: "boom".to_string(),
                        traceback: String::new(),
                        attempts: 1,
                    },
                }],
                cached: HashSet::from(["f.py:cached".to_string()]),
                run_hashes,
                empty: HashMap::new(),
                attempts: HashMap::new(),
                timings: HashMap::new(),
            }
        }

        fn ledger<'a>(&'a self, run_id: &'a str) -> RunLedger<'a> {
            RunLedger {
                run_id,
                status: "failed",
                command: "get",
                files: db::encode_files(&["f.py".to_string()]),
                target: None,
                steps_total: 5,
                steps_executed: 4,
                steps_cached: 1,
                elapsed: 1.5,
                all_outputs: &self.outputs,
                all_failures: &self.failures,
                all_sinks: &self.empty,
                all_attempts: &self.attempts,
                all_timings: &self.timings,
                cached_node_ids: &self.cached,
                run_hashes: &self.run_hashes,
                output_hashes: &self.empty,
                store_paths: &self.empty,
                cost_snapshot: &[],
            }
        }

        /// The row the recorder would write for `node` when it finishes.
        fn row(&self, node: &str) -> StepRow {
            let artifact = serde_json::json!({
                "path": self.outputs[node].path, "format": "json", "size_bytes": 2,
                "elapsed_seconds": 0.5,
            });
            StepRow::from_artifact(node, &self.run_hashes[node], &artifact, 1)
        }
    }

    async fn fresh_db(dir: &tempfile::TempDir, name: &str) -> String {
        let db_path = dir.path().join(name).to_string_lossy().to_string();
        db::init_db(&db_path).await.unwrap();
        db_path
    }

    /// Every materialization row as `(run_id, node_id, status)`, sorted.
    async fn rows(db_path: &str) -> Vec<(String, String, String)> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await.unwrap();
        let mut found = conn
            .query(
                "SELECT COALESCE(run_id, ''), node_id, status FROM materializations",
                (),
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(row) = found.next().await.unwrap() {
            out.push((
                row.get::<String>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<String>(2).unwrap(),
            ));
        }
        out.sort();
        out
    }

    /// What one complete write of the fixture's run looks like: each executed step once, the
    /// failure once, and nothing for the cache hit.
    fn complete(run_id: &str) -> Vec<(String, String, String)> {
        let mut want: Vec<_> = EXECUTED
            .iter()
            .map(|n| (run_id.to_string(), n.to_string(), "success".to_string()))
            .collect();
        want.push((
            run_id.to_string(),
            "f.py:bad".to_string(),
            "failed".to_string(),
        ));
        want.sort();
        want
    }

    async fn run_record(db_path: &str, run_id: &str) -> db::RunRecord {
        db::get_recent_runs(db_path, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.run_id == run_id)
            .expect("run row")
    }

    #[tokio::test]
    async fn the_ledger_adds_only_what_the_recorder_has_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[\"f.py\"]", None, Some(5))
            .await
            .unwrap();

        // Mid-run: two steps recorded; the run is `running` and counts them.
        record_steps(
            &db_path,
            "r1",
            &[fx.row("f.py:a"), fx.row("f.py:part[k=1]")],
        )
        .await
        .unwrap();
        assert_eq!(rows(&db_path).await.len(), 2);
        let mid = run_record(&db_path, "r1").await;
        assert_eq!((mid.status.as_str(), mid.steps_executed), ("running", 2));
        assert_eq!(mid.finished_at, None);

        // End of run: the rest is added, nothing twice, and the counts are the final ones.
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
        let end = run_record(&db_path, "r1").await;
        assert_eq!(
            (end.status.as_str(), end.steps_executed, end.steps_cached),
            ("failed", 4, 1)
        );

        // Writing the ledger again (a replay onto a database that already has it) is a no-op.
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
    }

    #[tokio::test]
    async fn a_replay_onto_a_freshly_pulled_db_carries_the_whole_run() {
        let dir = tempfile::tempdir().unwrap();
        let fx = Fixture::new();

        // The local DB, with steps recorded mid-run, is replaced by a pulled one that has
        // never heard of this run: the replay must not assume the recorder's rows survived.
        let local = fresh_db(&dir, "local.db").await;
        db::create_run(&local, "r1", "get", "[\"f.py\"]", None, Some(5))
            .await
            .unwrap();
        record_steps(&local, "r1", &[fx.row("f.py:a")])
            .await
            .unwrap();
        let pulled = fresh_db(&dir, "pulled.db").await;
        persist_run(&pulled, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&pulled).await, complete("r1"));
        let run = run_record(&pulled, "r1").await;
        assert_eq!((run.status.as_str(), run.steps_executed), ("failed", 4));

        // A pulled DB that already holds part of this run (another process on this machine
        // pushed the shared local DB mid-run) gets the rest, and keeps another run's rows.
        let partial = fresh_db(&dir, "partial.db").await;
        record_steps(&partial, "other", &[fx.row("f.py:a")])
            .await
            .unwrap();
        record_steps(
            &partial,
            "r1",
            &[fx.row("f.py:a"), fx.row("f.py:part[k=2]")],
        )
        .await
        .unwrap();
        persist_run(&partial, &fx.ledger("r1")).await.unwrap();
        let mut want = complete("r1");
        want.push((
            "other".to_string(),
            "f.py:a".to_string(),
            "success".to_string(),
        ));
        want.sort();
        assert_eq!(rows(&partial).await, want);
    }

    /// A pull, as `state_sync::pull_state` does it once the blob is downloaded: a fresh copy
    /// of `shared` is swapped in for `local`, which keeps its unpushed rows.
    async fn pull(dir: &tempfile::TempDir, shared: &str, local: &str) {
        state_sync::checkpoint_truncate(shared).await.unwrap();
        let staged = dir.path().join("staged.db");
        std::fs::copy(shared, &staged).unwrap();
        db::pull_for_tests(local, &staged).await;
    }

    #[tokio::test]
    async fn a_pull_in_the_middle_of_a_run_leaves_the_run_whole_and_nothing_twice() {
        // Another process in the same project (a second `barca get`, a `barca status`) pulls
        // while this run is going; later the run's own push conflicts and it pulls again.
        let dir = tempfile::tempdir().unwrap();
        let mut fx = Fixture::new();
        // The pull only carries steps whose artifact is there.
        for (node, oref) in fx.outputs.iter_mut() {
            let file = dir.path().join(crate::safe_node_id(node));
            std::fs::write(&file, b"1").unwrap();
            oref.path = file.to_string_lossy().to_string();
        }
        let shared = fresh_db(&dir, "shared.db").await;
        db::create_run(&shared, "theirs", "get", "[\"g.py\"]", None, Some(1))
            .await
            .unwrap();
        let local = fresh_db(&dir, "local.db").await;
        db::create_run(&local, "r1", "get", "[\"f.py\"]", None, Some(5))
            .await
            .unwrap();
        record_steps(&local, "r1", &[fx.row("f.py:a")])
            .await
            .unwrap();

        // Mid-run pull: the run row and the recorded step are still there afterwards, so
        // `barca status` goes on showing the step and `barca history` the run.
        pull(&dir, &shared, &local).await;
        let mid = run_record(&local, "r1").await;
        assert_eq!((mid.status.as_str(), mid.steps_executed), ("running", 1));
        assert_eq!(rows(&local).await.len(), 1);
        run_record(&local, "theirs").await;

        // The run goes on recording, ends, and writes its ledger and its log.
        record_steps(&local, "r1", &[fx.row("f.py:part[k=1]")])
            .await
            .unwrap();
        persist_run(&local, &fx.ledger("r1")).await.unwrap();
        let log = [("f.py:a".to_string(), "hello".to_string())];
        db::insert_logs(&local, "r1", &log).await.unwrap();
        assert_eq!(rows(&local).await, complete("r1"));

        // Its push conflicts: pull again, replay the ledger and the log.
        db::create_run(&shared, "theirs-2", "get", "[\"g.py\"]", None, Some(1))
            .await
            .unwrap();
        pull(&dir, &shared, &local).await;
        db::init_db(&local).await.unwrap();
        persist_run(&local, &fx.ledger("r1")).await.unwrap();
        db::insert_logs(&local, "r1", &log).await.unwrap();

        assert_eq!(rows(&local).await, complete("r1"));
        assert_eq!(db::get_logs(&local, "r1").await.unwrap().len(), 1);
        let end = run_record(&local, "r1").await;
        assert_eq!((end.status.as_str(), end.steps_executed), ("failed", 4));
        assert_eq!(db::count_runs(&local).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn the_recorder_and_the_ledger_write_the_same_row() {
        let dir = tempfile::tempdir().unwrap();
        let fx = Fixture::new();
        let columns = "node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
                       elapsed_seconds, status, attempts, run_id";
        let mut seen = Vec::new();
        for (name, by_recorder) in [("recorder.db", true), ("ledger.db", false)] {
            let db_path = fresh_db(&dir, name).await;
            if by_recorder {
                record_steps(&db_path, "r1", &[fx.row("f.py:a")])
                    .await
                    .unwrap();
            } else {
                persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
            }
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            let mut found = conn
                .query(
                    &format!("SELECT {columns} FROM materializations WHERE node_id = 'f.py:a'"),
                    (),
                )
                .await
                .unwrap();
            let row = found.next().await.unwrap().expect("a row for f.py:a");
            seen.push(format!(
                "{:?}",
                (0..9)
                    .map(|i| row.get_value(i).unwrap())
                    .collect::<Vec<_>>()
            ));
        }
        assert_eq!(seen[0], seen[1]);
    }

    #[tokio::test]
    async fn the_recorder_writes_during_the_run_and_leaves_the_rest_to_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();

        let recorder = StepRecorder::start(db_path.clone(), "r1".to_string());
        recorder.record(fx.row("f.py:a"));
        recorder.record(fx.row("f.py:part[k=1]"));
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while rows(&db_path).await.len() < 2 {
            assert!(Instant::now() < deadline, "the recorder never wrote");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // Queued just before the run ends: finish() does not wait a full interval to write it.
        recorder.record(fx.row("f.py:part[k=2]"));
        let stopping = Instant::now();
        recorder.finish().await;
        assert!(stopping.elapsed() < RECORD_INTERVAL);

        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
    }
}

#[cfg(test)]
mod progress_tests {
    use super::reconcile_total;

    #[test]
    fn total_grows_to_cover_steps_the_plan_did_not_count() {
        // parallel() children complete as extra steps beyond the plan.
        assert_eq!(reconcile_total(2, 4), 4);
        assert_eq!(reconcile_total(4, 2), 4);
        assert_eq!(reconcile_total(3, 3), 3);
        assert_eq!(reconcile_total(0, 0), 0);
    }

    #[test]
    fn remaining_never_underflows() {
        // The ETA math subtracts usizes; this used to panic in debug and wrap in release.
        assert_eq!(2usize.saturating_sub(4), 0);
    }
}

#[cfg(test)]
mod refresh_name_tests {
    use super::refresh_name_matches;

    #[test]
    fn matches_the_full_id_or_the_function_name() {
        assert!(refresh_name_matches("pipeline.py:src", "src"));
        assert!(refresh_name_matches("pipeline.py:src", "pipeline.py:src"));
        assert!(!refresh_name_matches("pipeline.py:src", "rc"));
        assert!(!refresh_name_matches("pipeline.py:source", "src"));
        assert!(!refresh_name_matches("pipeline.py:src", "nope"));
    }
}

#[cfg(test)]
mod telemetry_report_tests {
    use super::*;

    fn worker_error(message: &str) -> dispatch::StepError {
        dispatch::StepError {
            error_type: "WorkerError".to_string(),
            message: message.to_string(),
            traceback: String::new(),
            attempts: 1,
        }
    }

    #[test]
    fn the_exception_type_message_and_frames_are_separated() {
        let (ty, msg, stack) = exception_of(&worker_error(
            "ValueError: cannot publish\n  File \"p.py\", line 3, in publish\n    raise ValueError(\"cannot publish\")",
        ));
        assert_eq!(
            (ty.as_str(), msg.as_str()),
            ("ValueError", "cannot publish")
        );
        assert_eq!(
            stack.as_deref(),
            Some("  File \"p.py\", line 3, in publish\n    raise ValueError(\"cannot publish\")")
        );
    }

    #[test]
    fn a_message_that_quotes_a_traceback_stays_whole() {
        let (ty, msg, stack) = exception_of(&worker_error(
            "RuntimeError: bad config:\n  File \"/etc/x.conf\" is missing\nplease fix\n  File \"p.py\", line 9, in load\n    raise RuntimeError(m)",
        ));
        assert_eq!(ty, "RuntimeError");
        assert_eq!(
            msg,
            "bad config:\n  File \"/etc/x.conf\" is missing\nplease fix"
        );
        assert_eq!(
            stack.as_deref(),
            Some("  File \"p.py\", line 9, in load\n    raise RuntimeError(m)")
        );
    }

    #[test]
    fn an_error_without_frames_or_a_python_type_is_passed_through() {
        let mut upload = worker_error("upload to s3://b/x failed: ConnectionError: reset");
        upload.error_type = "UploadError".to_string();
        let (ty, msg, stack) = exception_of(&upload);
        assert_eq!(ty, "UploadError");
        assert_eq!(msg, "upload to s3://b/x failed: ConnectionError: reset");
        assert_eq!(stack, None);
        assert_eq!(
            exception_of(&worker_error("worker disconnected")).0,
            "WorkerError"
        );
    }
}

#[cfg(test)]
mod multi_target_tests {
    use super::*;

    const SRC: &str = r#"
from barca import asset, task


@asset()
def src() -> int:
    return 1


@asset(inputs={"s": src})
def left(s: int) -> int:
    return s


@asset(inputs={"l": left})
def deeper(l: int) -> int:
    return l


@task(inputs={"s": src})
def check_a(s: int) -> None:
    pass


@task(inputs={"d": deeper})
def check_b(d: int) -> None:
    pass


@asset()
def lone() -> int:
    return 2
"#;

    fn dag() -> Dag {
        let nodes = crate::parse::extract_nodes(SRC, "p.py").unwrap();
        Dag::build(&nodes).unwrap()
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn failure(node_id: &str, message: &str) -> dispatch::StepFailure {
        dispatch::StepFailure {
            node_id: node_id.to_string(),
            error: dispatch::StepError {
                error_type: "WorkerError".to_string(),
                message: message.to_string(),
                traceback: String::new(),
                attempts: 1,
            },
        }
    }

    fn oref(path: &str) -> OutputRef {
        OutputRef {
            path: path.to_string(),
            format: "json".to_string(),
            size_bytes: 1,
            elapsed_seconds: None,
            content_hash: None,
        }
    }

    fn outcome(status: &str) -> TargetOutcome {
        TargetOutcome {
            status: status.to_string(),
            final_output: None,
            error: None,
            failed_node: None,
        }
    }

    #[test]
    fn every_name_resolves_and_repeats_collapse() {
        let dag = dag();
        let got = resolve_targets(&dag, &names(&["check_a", "check_b", "check_a"]), "run").unwrap();
        assert_eq!(
            got,
            vec![
                ("check_a".to_string(), "p.py:check_a".to_string()),
                ("check_b".to_string(), "p.py:check_b".to_string()),
            ]
        );
        assert!(resolve_targets(&dag, &[], "get").unwrap().is_empty());
    }

    #[test]
    fn one_bad_name_fails_the_whole_list() {
        let dag = dag();
        let err = resolve_targets(&dag, &names(&["check_a", "nope"]), "run").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        let err = resolve_targets(&dag, &names(&["left", "check_a"]), "get").unwrap_err();
        assert!(err.to_string().contains("barca run"), "{err}");
    }

    #[test]
    fn the_plan_is_the_union_of_cones_with_shared_upstream_once() {
        let dag = dag();
        let config = ResourceConfig {
            pool_size: 4,
            concurrency_groups: HashMap::new(),
        };
        let plan = plan_for_targets(&dag, &["p.py:check_a", "p.py:check_b"], &config, "run");
        let ids: Vec<String> = plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|s| s.step_id.display())
            .collect();
        assert_eq!(ids.iter().filter(|i| *i == "p.py:src").count(), 1);
        for want in ["p.py:left", "p.py:deeper", "p.py:check_a", "p.py:check_b"] {
            assert!(ids.iter().any(|i| i == want), "{want} missing from {ids:?}");
        }
        assert!(!ids.iter().any(|i| i == "p.py:lone"));
    }

    fn planned_ids(plan: &ExecutionPlan) -> Vec<String> {
        plan.phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|s| s.step_id.display())
            .collect()
    }

    #[test]
    fn bare_get_plans_every_asset_and_no_task() {
        let dag = dag();
        let config = ResourceConfig {
            pool_size: 4,
            concurrency_groups: HashMap::new(),
        };
        let mut got = planned_ids(&plan_for_targets(&dag, &[], &config, "get"));
        got.sort();
        assert_eq!(got, ["p.py:deeper", "p.py:left", "p.py:lone", "p.py:src"]);
        // status (inspection) still covers the whole file, tasks included.
        let all = planned_ids(&plan_for_targets(&dag, &[], &config, "status"));
        assert!(all.iter().any(|i| i == "p.py:check_a"), "{all:?}");
    }

    #[test]
    fn skipped_tasks_note_names_the_tasks_and_the_run_command() {
        let files = names(&["p.py"]);
        let note = skipped_tasks_note(&dag(), &files).unwrap();
        assert!(
            note.contains("skipped 2 tasks (check_a, check_b)"),
            "{note}"
        );
        assert!(note.contains("barca run check_a p.py"), "{note}");

        let only = "from barca import task\n\n@task()\ndef deploy() -> None:\n    pass\n";
        let only = Dag::build(&crate::parse::extract_nodes(only, "t.py").unwrap()).unwrap();
        let note = skipped_tasks_note(&only, &names(&["t.py"])).unwrap();
        assert!(note.contains("nothing to get"), "{note}");
        assert!(note.contains("barca run deploy t.py"), "{note}");

        let assets = "from barca import asset\n\n@asset()\ndef a() -> int:\n    return 1\n";
        let assets = Dag::build(&crate::parse::extract_nodes(assets, "a.py").unwrap()).unwrap();
        assert!(skipped_tasks_note(&assets, &names(&["a.py"])).is_none());
    }

    #[test]
    fn refresh_names_may_come_from_any_targets_cone() {
        let dag = dag();
        let both = ["p.py:check_a", "p.py:check_b"];
        validate_refresh_names(&dag, &both, &names(&["src", "deeper"]), false).unwrap();
        let msg = validate_refresh_names(&dag, &both, &names(&["lone"]), false)
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("in the cones of 'check_a', 'check_b'"),
            "{msg}"
        );
        assert!(
            validate_refresh_names(&dag, &["p.py:check_a"], &names(&["deeper"]), false).is_err()
        );
    }

    #[test]
    fn a_failure_blocks_only_what_depends_on_it() {
        let dag = dag();
        let failed: std::collections::HashSet<String> = ["p.py:left".to_string()].into();
        assert_eq!(
            blocking_failure(&dag, "p.py:check_b", &failed),
            Some("p.py:left")
        );
        assert_eq!(blocking_failure(&dag, "p.py:check_a", &failed), None);
        assert_eq!(blocking_failure(&dag, "p.py:left", &failed), None);
    }

    #[test]
    fn each_target_reports_its_own_outcome() {
        let dag = dag();
        let targets = vec![
            ("check_a".to_string(), "p.py:check_a".to_string()),
            ("check_b".to_string(), "p.py:check_b".to_string()),
            ("lone".to_string(), "p.py:lone".to_string()),
        ];
        let outputs: HashMap<String, OutputRef> =
            [("p.py:check_a".to_string(), oref("a.json"))].into();
        let failures = vec![failure("p.py:left", "boom")];
        let out = target_outcomes(&dag, &targets, &outputs, &failures);
        assert_eq!(out[0].0, "check_a");
        assert_eq!(out[0].1.status, "success");
        assert_eq!(out[0].1.final_output.as_ref().unwrap().path, "a.json");
        assert_eq!(out[1].1.status, "failed");
        assert_eq!(out[1].1.failed_node.as_deref(), Some("p.py:left"));
        assert_eq!(out[1].1.error.as_deref(), Some("boom"));
        assert_eq!(out[2].1.status, "failed");
        assert_eq!(out[2].1.error.as_deref(), Some("did not run"));
    }

    #[test]
    fn end_of_run_line_is_one_format_and_never_says_done_on_failure() {
        assert_eq!(
            end_of_run_line(3, 3, 1.26, RunOutcome::Done),
            "[barca] 3/3 steps | done in 1.3s"
        );
        let failed = end_of_run_line(0, 3, 0.0, RunOutcome::Failed);
        assert_eq!(failed, "[barca] 0/3 steps | failed in 0.0s");
        assert!(!failed.contains("done"));
        assert_eq!(
            end_of_run_line(1, 3, 2.0, RunOutcome::Cancelled),
            "[barca] 1/3 steps | cancelled after 2.0s"
        );
    }

    #[test]
    fn failed_step_line_names_the_step_and_the_first_error_line() {
        assert_eq!(
            failed_step_line(
                "p.py:broken",
                "\nValueError: boom\n  File \"p.py\", line 3, in broken"
            ),
            "[barca] step:p.py:broken failed: ValueError: boom"
        );
        assert_eq!(
            failed_step_line("p.py:x", ""),
            "[barca] step:p.py:x failed: unknown error"
        );
    }

    #[test]
    fn summary_add_counts_like_the_dry_run() {
        let line = |action: &str, p: Option<(usize, usize)>| StepReport {
            action: Some(action.to_string()),
            partitions: p.map(|(cached, will_run)| PartitionSummary {
                total: cached + will_run,
                cached,
                will_run,
                will_run_keys: Vec::new(),
            }),
            ..Default::default()
        };
        let mut s = ExplainSummary::default();
        for r in [
            line("cached", None),
            line("run", None),
            line("unknown", None),
            line("partial", Some((2, 3))),
            line("run", Some((0, 4))),
        ] {
            s.add(&r);
        }
        assert_eq!(
            s,
            ExplainSummary {
                will_run: 8,
                cached: 3,
                unknown: 1
            }
        );
    }

    #[test]
    fn dry_run_json_names_one_target_or_lists_several() {
        let mut r = ExplainResult {
            dry_run: true,
            command: "run".to_string(),
            target: Some("a".to_string()),
            targets: Vec::new(),
            steps: Vec::new(),
            summary: ExplainSummary::default(),
            warnings: Vec::new(),
        };
        let one = serde_json::to_value(&r).unwrap();
        assert_eq!(one["target"], "a");
        assert!(one.get("targets").is_none());
        r.target = None;
        let predicted = |will_run| TargetPrediction {
            summary: ExplainSummary {
                will_run,
                cached: 1,
                unknown: 0,
            },
        };
        r.targets = vec![
            ("b".to_string(), predicted(2)),
            ("a".to_string(), predicted(0)),
        ];
        assert_eq!(r.target_names(), names(&["b", "a"]));
        let many = serde_json::to_value(&r).unwrap();
        assert_eq!(
            many["targets"],
            serde_json::json!({
                "b": {"summary": {"will_run": 2, "cached": 1, "unknown": 0}},
                "a": {"summary": {"will_run": 0, "cached": 1, "unknown": 0}},
            })
        );
        assert!(many.get("target").is_none());
        // Keyed in the order given, like a real multi-target run.
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.find("\"b\"").unwrap() < s.find("\"a\"").unwrap(), "{s}");
    }

    #[test]
    fn multi_result_targets_serialize_as_a_map_in_the_order_given() {
        let r = MultiResult {
            run_id: "r".to_string(),
            elapsed_seconds: 0.0,
            steps_executed: 0,
            phases: 0,
            steps: Vec::new(),
            warnings: Vec::new(),
            targets: vec![
                ("zeta".to_string(), outcome("success")),
                ("alpha".to_string(), outcome("failed")),
            ],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            s.find("\"zeta\"").unwrap() < s.find("\"alpha\"").unwrap(),
            "{s}"
        );
        assert!(r.any_failed());
    }
}

#[cfg(test)]
mod target_name_tests {
    use super::target_name_matches;

    #[test]
    fn exact_names_and_ids_match() {
        assert!(target_name_matches("p.py:deploy", "deploy"));
        assert!(target_name_matches("p.py:deploy", "p.py:deploy"));
        assert!(target_name_matches("sub/p.py:deploy", "p.py:deploy"));
        assert!(target_name_matches("sub/p.py:deploy", "sub/p.py:deploy"));
    }

    #[test]
    fn a_suffix_of_another_name_does_not_match() {
        assert!(!target_name_matches("p.py:prod_deploy", "deploy"));
        assert!(!target_name_matches("p.py:dyn_margin_all", "margin_all"));
        assert!(!target_name_matches("subp.py:deploy", "p.py:deploy"));
        assert!(!target_name_matches("sub/p.py:deploy", "/p.py:deploy_x"));
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::transfer::ArtifactLayout;

    fn oref(path: &str) -> dispatch::OutputRef {
        dispatch::OutputRef {
            path: path.to_string(),
            format: "json".to_string(),
            size_bytes: 3,
            elapsed_seconds: None,
            content_hash: None,
        }
    }

    #[tokio::test]
    async fn background_task_is_aborted_when_dropped_unjoined() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = Background::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let _ = tx.send(());
        });
        drop(task);
        // Aborting drops the future, and with it the sender.
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
        assert!(matches!(r, Ok(Err(_))), "task kept running after drop");
    }

    #[tokio::test]
    async fn background_task_join_returns_its_output() {
        let task = Background::spawn(async { 7 });
        assert_eq!(task.join().await.unwrap(), 7);
    }

    #[test]
    fn cache_hit_without_store_is_used_as_recorded() {
        match resolve_cache_hit(oref("/w/.barca/artifacts/n/h.json"), None) {
            CacheHit::Local(o) => assert_eq!(o.path, "/w/.barca/artifacts/n/h.json"),
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn cache_hit_in_store_points_at_local_mirror() {
        let layout = ArtifactLayout::new("/w/a", "s3://b/p/default/artifacts");
        match resolve_cache_hit(oref("s3://b/p/default/artifacts/n/h.json"), Some(&layout)) {
            CacheHit::Store { local, store } => {
                assert_eq!(local.path, "/w/a/n/h.json");
                assert_eq!(local.format, "json");
                assert_eq!(store, "s3://b/p/default/artifacts/n/h.json");
            }
            _ => panic!("expected Store"),
        }
    }

    #[test]
    fn legacy_local_row_is_used_when_the_file_is_here() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("h.json");
        std::fs::write(&f, "[1]").unwrap();
        let layout = ArtifactLayout::new("/w/a", "s3://b/p");
        match resolve_cache_hit(oref(f.to_str().unwrap()), Some(&layout)) {
            CacheHit::Local(o) => assert_eq!(o.path, f.to_str().unwrap()),
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn a_row_outside_the_store_is_a_hit_whatever_is_on_disk() {
        let layout = ArtifactLayout::new("/w/a", "s3://b/p");
        // e.g. recorded against a different store, or another machine's local path. It is still
        // the cached result: it is computed again only if something has to read it (#252).
        for path in ["s3://other/p/n/h.json", "/elsewhere/n/h.json"] {
            match resolve_cache_hit(oref(path), Some(&layout)) {
                CacheHit::Local(o) => assert_eq!(o.path, path),
                _ => panic!("expected Local"),
            }
        }
    }

    #[tokio::test]
    async fn persist_run_records_failure_type_and_its_own_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db").to_string_lossy().into_owned();
        db::init_db(&db_path).await.unwrap();
        let failures = vec![dispatch::StepFailure {
            node_id: "f:a".to_string(),
            error: dispatch::StepError {
                error_type: "UploadError".to_string(),
                message: "upload to s3://b/f__a/h1.json failed: ConnectionError: reset".to_string(),
                traceback: String::new(),
                attempts: 4,
            },
        }];
        let run_hashes = HashMap::from([("f:a".to_string(), "h1".to_string())]);
        // The step itself ran once; the upload made 4 attempts.
        let all_attempts = HashMap::from([("f:a".to_string(), 1u32)]);
        let ledger = RunLedger {
            run_id: "r1",
            status: "failed",
            command: "get",
            files: "f.py".to_string(),
            target: None,
            steps_total: 1,
            steps_executed: 1,
            steps_cached: 0,
            elapsed: 0.1,
            all_outputs: &HashMap::new(),
            all_failures: &failures,
            all_sinks: &HashMap::new(),
            all_attempts: &all_attempts,
            all_timings: &HashMap::new(),
            cached_node_ids: &std::collections::HashSet::new(),
            run_hashes: &run_hashes,
            output_hashes: &HashMap::new(),
            store_paths: &HashMap::new(),
            cost_snapshot: &[],
        };
        persist_run(&db_path, &ledger).await.unwrap();

        let (_db, conn) = db::open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT status, error_type, attempts, artifact_path IS NULL, run_hash FROM materializations",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "failed");
        assert_eq!(row.get::<String>(1).unwrap(), "UploadError");
        assert_eq!(row.get::<i64>(2).unwrap(), 4);
        assert_eq!(row.get::<i64>(3).unwrap(), 1);
        assert_eq!(row.get::<String>(4).unwrap(), "h1");
    }

    #[tokio::test]
    async fn persist_run_records_store_locations() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db").to_string_lossy().into_owned();
        db::init_db(&db_path).await.unwrap();

        let all_outputs = HashMap::from([
            ("f:a".to_string(), oref("/w/a/f__a/h1.json")),
            ("f:b".to_string(), oref("/w/a/f__b/h2.json")),
        ]);
        let run_hashes = HashMap::from([
            ("f:a".to_string(), "h1".to_string()),
            ("f:b".to_string(), "h2".to_string()),
        ]);
        // Only a was uploaded through a store; b keeps its recorded path.
        let store_paths = HashMap::from([("f:a".to_string(), "s3://b/p/f__a/h1.json".to_string())]);
        let ledger = RunLedger {
            run_id: "r1",
            status: "success",
            command: "get",
            files: "f.py".to_string(),
            target: None,
            steps_total: 2,
            steps_executed: 2,
            steps_cached: 0,
            elapsed: 0.1,
            all_outputs: &all_outputs,
            all_failures: &[],
            all_sinks: &HashMap::new(),
            all_attempts: &HashMap::new(),
            all_timings: &HashMap::new(),
            cached_node_ids: &std::collections::HashSet::new(),
            run_hashes: &run_hashes,
            output_hashes: &HashMap::new(),
            store_paths: &store_paths,
            cost_snapshot: &[],
        };
        persist_run(&db_path, &ledger).await.unwrap();

        let (_db, conn) = db::open_conn(&db_path).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT node_id, artifact_path FROM materializations ORDER BY node_id",
                (),
            )
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            got.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
        }
        assert_eq!(
            got,
            vec![
                ("f:a".to_string(), "s3://b/p/f__a/h1.json".to_string()),
                ("f:b".to_string(), "/w/a/f__b/h2.json".to_string()),
            ]
        );
    }
}
