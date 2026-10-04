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
use crate::parse::extract_nodes;
use crate::planner::{self, ExecutionPlan, Phase, ResourceConfig};
use crate::state_sync;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

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
fn find_target_id(dag: &Dag, name: &str) -> Result<String, BarcaError> {
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
fn plan_for_targets(
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
enum RunReason {
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
}

impl RunReason {
    fn code(&self) -> &'static str {
        match self {
            RunReason::Task => "task",
            RunReason::Sensor => "sensor",
            RunReason::NoCache => "no_cache",
            RunReason::Refresh => "refresh",
            RunReason::RefreshCascade { .. } => "refresh_cascade",
            RunReason::RefreshAll => "refresh_all",
            RunReason::NotMaterialized => "not_materialized",
        }
    }

    fn detail(&self) -> String {
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
fn short_name(node_id: &str) -> &str {
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

/// The most recent successful materialization of `node_id` with this run hash, if any.
async fn lookup_cached(
    cache: &db::CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<dispatch::OutputRef> {
    let mut rows = cache
        .conn()
        .query(
            "SELECT artifact_path, artifact_format, artifact_size_bytes FROM materializations WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
            [node_id.to_string(), run_hash.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().and_then(|row| {
        Some(dispatch::OutputRef {
            path: row.get::<String>(0).ok()?,
            format: row.get::<String>(1).ok()?,
            size_bytes: row.get::<i64>(2).ok()? as u64,
            elapsed_seconds: None,
        })
    })
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

// ─── Result types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetResult {
    pub run_id: String,
    pub elapsed_seconds: f64,
    pub steps_executed: usize,
    pub phases: usize,
    pub final_output: Option<OutputRef>,
    /// What happened to each planned step in this run (ran / cached / partial, and why).
    #[serde(default)]
    pub steps: Vec<StepReport>,
}

/// The result of `barca get|run a,b` (several targets): one run over the union of the targets'
/// cones, with each target's outcome. A failed target does not stop the others.
#[derive(Debug, Clone, Serialize)]
pub struct MultiResult {
    pub run_id: String,
    pub elapsed_seconds: f64,
    pub steps_executed: usize,
    pub phases: usize,
    /// What happened to each planned step (shared upstream steps appear once).
    pub steps: Vec<StepReport>,
    /// Each target by the name it was given, in the order given (serialized as a map).
    #[serde(serialize_with = "serialize_targets")]
    pub targets: Vec<(String, TargetOutcome)>,
}

fn serialize_targets<S: serde::Serializer>(
    targets: &[(String, TargetOutcome)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(targets.iter().map(|(k, v)| (k, v)))
}

impl MultiResult {
    /// True when any target failed.
    pub fn any_failed(&self) -> bool {
        self.targets.iter().any(|(_, t)| t.status != "success")
    }
}

/// How one target of a multi-target run ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    /// `success` or `failed`.
    pub status: String,
    /// The target's output, when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_output: Option<OutputRef>,
    /// The error of the step that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The step that failed: the target itself, or a step upstream of it (`failed_node`, the
    /// same key a failed single-target run uses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_node: Option<String>,
}

/// How a step was (or, in a dry run, will be) treated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepReport {
    pub id: String,
    /// `asset`, `task` or `sensor`.
    pub kind: String,
    /// Dry run only: `cached`, `run`, `partial` (some partition keys cached) or `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Real run only: `ran`, `cached`, `partial`, or `failed` (in a failed run's result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why the step runs: `task`, `sensor`, `refresh`, `refresh_cascade`, `refresh_all`,
    /// `not_materialized`, `partitions_unknown` or `sensor_output_unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The reason in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_hash: Option<String>,
    /// The cached artifact, when the step is served from cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// Set when a cached step depends on an asset refreshed in the same run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<PartitionSummary>,
    /// Declared env values the step used (`@asset(env=[...])`): name -> value, `null` when unset,
    /// `"<redacted>"` for secret-looking names. Absent when the node declares no env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartitionSummary {
    pub total: usize,
    pub cached: usize,
    pub will_run: usize,
    /// The keys that will (or did) run, capped at 20.
    pub will_run_keys: Vec<String>,
}

/// What `--dry-run` reports: the same decisions a real run would make, without making them.
///
/// Serialized with `target` (one target, or null for the whole file) or, when several targets
/// were given, `targets` in its place: an object keyed by target name in the order given, like a
/// real multi-target run, each `{"summary": {...}}` counted over that target's cone.
#[derive(Debug, Clone)]
pub struct ExplainResult {
    pub dry_run: bool,
    pub command: String,
    pub target: Option<String>,
    /// Every target with its predicted summary when more than one was given; empty otherwise.
    pub targets: Vec<(String, TargetPrediction)>,
    pub steps: Vec<StepReport>,
    pub summary: ExplainSummary,
}

/// One target of a multi-target dry run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetPrediction {
    /// The dry-run summary counted over this target's cone only (shared upstream steps count
    /// for every target that needs them).
    pub summary: ExplainSummary,
}

impl ExplainResult {
    /// The target names in scope: the several given, the one given, or none (whole file).
    pub fn target_names(&self) -> Vec<String> {
        if self.targets.is_empty() {
            self.target.iter().cloned().collect()
        } else {
            self.targets.iter().map(|(n, _)| n.clone()).collect()
        }
    }
}

impl Serialize for ExplainResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        struct Targets<'a>(&'a [(String, TargetPrediction)]);
        impl Serialize for Targets<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_map(self.0.iter().map(|(k, v)| (k, v)))
            }
        }
        let mut s = serializer.serialize_struct("ExplainResult", 5)?;
        s.serialize_field("dry_run", &self.dry_run)?;
        s.serialize_field("command", &self.command)?;
        if self.targets.len() > 1 {
            s.serialize_field("targets", &Targets(&self.targets))?;
        } else {
            s.serialize_field("target", &self.target)?;
        }
        s.serialize_field("steps", &self.steps)?;
        s.serialize_field("summary", &self.summary)?;
        s.end()
    }
}

impl ExplainSummary {
    /// Add one (merged) dry-run step line: a partitioned line counts its keys.
    fn add(&mut self, r: &StepReport) {
        match (r.action.as_deref(), &r.partitions) {
            (Some("unknown"), _) => self.unknown += 1,
            (_, Some(p)) => {
                self.cached += p.cached;
                self.will_run += p.will_run;
            }
            (Some("cached"), None) => self.cached += 1,
            _ => self.will_run += 1,
        }
    }
}

/// Counted in steps: each partition key is one step.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplainSummary {
    pub will_run: usize,
    pub cached: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    pub total_steps: usize,
    pub phases: Vec<PlanPhase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPhase {
    /// Why this phase starts: `{"type": "initial"}`, or `{"type": "fan_in", "node_id": ...}`
    /// when it waits for a node that gathers several upstream results.
    pub reason: PlanPhaseReason,
    pub streams: Vec<PlanStream>,
}

/// [`crate::planner::PhaseReason`] as `barca plan` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanPhaseReason {
    Initial,
    FanIn { node_id: String },
}

impl From<&crate::planner::PhaseReason> for PlanPhaseReason {
    fn from(r: &crate::planner::PhaseReason) -> Self {
        match r {
            crate::planner::PhaseReason::Initial => PlanPhaseReason::Initial,
            crate::planner::PhaseReason::FanIn { node_id } => PlanPhaseReason::FanIn {
                node_id: node_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStream {
    pub stream_id: String,
    pub steps: Vec<String>,
}

/// Lightweight summary of a single DAG node, for the server's `/assets` listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSummary {
    /// Stable node id (continuity key), e.g. `pipeline.py:fetch`.
    pub id: String,
    /// Node kind: asset, sensor, or task.
    pub kind: crate::NodeKind,
    /// Freshness policy (always / manual / schedule).
    pub freshness: crate::Freshness,
    /// Upstream node ids this node depends on (direct + collected), sorted.
    pub inputs: Vec<String>,
    /// Declared environment variable names (`@asset(env=[...])`), in declaration order.
    #[serde(default)]
    pub env: Vec<String>,
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
    cancel: CancellationToken,
) -> Result<GetResult, BarcaError> {
    let names: Vec<String> = target_name.map(str::to_string).into_iter().collect();
    execute(
        cfg, &names, file_args, python, false, agent_mode, policy, "get", cancel,
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
    cancel: CancellationToken,
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
    cancel: CancellationToken,
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
    cancel: CancellationToken,
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
    explain_dag(
        &dag,
        cfg,
        target_names,
        python,
        policy,
        no_cache,
        command_label,
    )
    .await
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

    // Shared remote state: pull it like a real run, so the cache check sees every machine's
    // materializations.
    if cfg.state == crate::config::StateMode::Optimistic && cfg.state_uri.is_some() {
        state_sync::pull_state(python, cfg).await?;
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
                    Decision::Run(_) => summary.will_run += step.partition_keys.len().max(1),
                    Decision::Cached { oref, .. } => {
                        summary.cached += 1;
                        all_outputs.insert(step.step_id.display(), oref);
                    }
                    Decision::Partitioned { cached, missing } => {
                        summary.cached += cached.len();
                        summary.will_run += missing.len();
                        for (pdisplay, oref) in cached {
                            all_outputs.insert(pdisplay, oref);
                        }
                    }
                }
            }
        }
    }
    drop(cache);

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
    cancel: CancellationToken,
) -> Result<Executed, BarcaError> {
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

    let dag = build_dag(file_args, python).await?;
    trace_point!("dag_built");

    let targets = resolve_targets(&dag, target_names, command_label)?;
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
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

    db::ensure_env_dirs(&cfg.env)?;
    let db_path = cfg.db_path.clone();

    // Shared remote state: pull the metadata DB before opening it, so cache
    // checks below see every machine's materializations. Pull failure is a
    // hard error — silently diverging local runs are worse than stopping.
    let state_sync_on =
        cfg.state == crate::config::StateMode::Optimistic && cfg.state_uri.is_some();
    let mut state_token = if state_sync_on {
        Some(state_sync::pull_state(python, cfg).await?)
    } else {
        None
    };
    trace_point!("state_sync_pull (enabled={state_sync_on})");

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
    // Sink outcomes (JSON) per node, accumulated across phases for the DB.
    let mut all_sinks: HashMap<String, String> = HashMap::new();
    // Per-node self-timing (cpu_seconds, max_rss_bytes) reported by workers.
    let mut all_timings: HashMap<String, (Option<f64>, Option<u64>)> = HashMap::new();
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

    // Persistent worker pool: one pool for the whole run, shared across
    // phases so workers keep their interpreter (and imported user modules)
    // warm between phases.
    let io_config = crate::io_loop::IoConfig {
        python: python.to_path_buf(),
        pool_size,
        run_id: run_id.clone(),
        artifact_root: cfg.artifact_root.clone(),
        storage_options_json: cfg.storage_options_json.clone(),
    };
    let mut pool = crate::io_loop::WorkerPool::start(io_config).map_err(BarcaError::Other)?;
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

    for (phase_idx, phase) in exec_plan.phases.iter().enumerate() {
        // Stop scheduling new phases once cancelled; partial results from
        // completed phases are persisted below.
        if cancel.is_cancelled() {
            if phase_error.is_none() {
                phase_error = Some("run cancelled".to_string());
            }
            break;
        }
        trace_point!("phase{phase_idx}_start");

        // Multi-target run after a failure: drop the steps that depend on a failed step; the
        // rest of the phase still runs.
        let unblocked_phase;
        let phase = if keep_going && !failed_bases.is_empty() {
            let mut p = phase.clone();
            for stream in &mut p.streams {
                stream.steps.retain(|st| {
                    let base = st.step_id.base_id();
                    let Some(up) = blocking_failure(&dag, base, &failed_bases) else {
                        return true;
                    };
                    step_reports.push(StepReport {
                        id: base.to_string(),
                        kind: kind_str(dag.get_node(base).map(|n| n.kind())),
                        status: Some("skipped".to_string()),
                        reason: Some("upstream_failed".to_string()),
                        detail: Some(format!("depends on '{}', which failed", short_name(up))),
                        ..Default::default()
                    });
                    skipped_bases.insert(base.to_string());
                    false
                });
            }
            p.streams.retain(|s| !s.steps.is_empty());
            unblocked_phase = p;
            &unblocked_phase
        } else {
            phase
        };

        let expanded_phase = dispatch::expand_pending_partitions(phase, &all_outputs, pool_size);
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

        // Open the DB only for this phase's cache lookups and release it before any step
        // runs, so other barca processes can use the metadata DB while Python executes.
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
                            eprintln!(
                                "[barca] step:{display_id} cached{}",
                                env_suffix(&dag, &display_id)
                            );
                        }
                        all_outputs.insert(display_id.clone(), oref);
                        cached_node_ids.insert(display_id);
                    }
                    Decision::Partitioned { cached, missing } => {
                        for (pdisplay, oref) in cached {
                            all_outputs.insert(pdisplay.clone(), oref);
                            cached_node_ids.insert(pdisplay);
                        }
                        if !missing.is_empty() {
                            let mut partial = step.clone();
                            partial.partition_keys = missing;
                            uncached_steps.push(partial);
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

        let filtered_phase = Phase {
            reason: phase_ref.reason.clone(),
            streams: uncached_streams,
        };

        steps_executed += filtered_phase
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
            .sum::<usize>();

        let provided = dispatch::build_provided_inputs(&filtered_phase, &all_outputs);
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
        let on_step_cb: crate::io_loop::StepCallback<'_> =
            Box::new(|node_id: &str, artifact: &serde_json::Value| {
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
            });

        // Drive this phase against the persistent pool. The cost model both
        // sizes the batch pulls and absorbs the timings coming back.
        let phase_err = pool
            .run_phase(&mut coord, &mut cost_model, Some(on_step_cb), &cancel)
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
        let mut first_non_group_failure: Option<(String, String)> = None;
        for (item_id, error_msg) in coord.failed_items() {
            let item = coord.item(item_id);
            if item.group.is_some() {
                // Parallel branch failure — handled by the group/parent, not a phase error.
                continue;
            }
            let node_id = item.step_id.display();
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
        if phase_error.is_some() || (step_failure.is_some() && !keep_going) {
            break;
        }
    }

    // All phases done (or aborted/cancelled) — release the worker pool before
    // persisting.
    pool.shutdown().await;
    trace_point!("pool_shutdown");

    // Finish progress bar. The end-of-run line is the same with and without --agent.
    if let Some(ref bar) = pb {
        bar.finish_and_clear();
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

    let steps_cached = cached_node_ids.len();
    let elapsed = t0.elapsed().as_secs_f64();

    // Drop the run-long cache connection before persistence: the state push
    // checkpoints the WAL, which requires no other open handles on the file.

    let was_cancelled = cancel.is_cancelled();

    // Persist all executed outputs (including partial results on failure) —
    // held in a ledger so a state-push conflict can replay this run's rows
    // onto a freshly pulled database.
    let cost_snapshot: Vec<(String, crate::cost::NodeEstimate)> = cost_model
        .snapshot()
        .map(|(node_id, est)| (node_id.clone(), *est))
        .collect();
    let ledger = RunLedger {
        run_id: &run_id,
        status: if was_cancelled {
            "cancelled"
        } else if phase_error.is_some() || step_failure.is_some() {
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
        cost_snapshot: &cost_snapshot,
    };
    persist_run(&db_path, &ledger).await?;
    trace_point!("persist_run_done");

    // Shared remote state: fold the WAL into the main file and conditionally
    // upload it. On conflict (another machine pushed first): pull the fresh
    // database, replay this run's ledger onto it, retry.
    if state_sync_on {
        let mut attempt = 0u32;
        loop {
            state_sync::checkpoint_truncate(&db_path).await?;
            match state_sync::push_state(python, cfg, state_token.as_ref().unwrap()).await? {
                state_sync::PushOutcome::Pushed(_) => break,
                state_sync::PushOutcome::Conflict => {
                    if attempt >= cfg.push_retries {
                        return Err(BarcaError::Other(format!(
                            "shared state push conflicted {attempt} times — results were                              computed but the shared state was not updated; re-run to retry"
                        )));
                    }
                    attempt += 1;
                    state_token = Some(state_sync::pull_state(python, cfg).await?);
                    db::init_db(&db_path).await?;
                    persist_run(&db_path, &ledger).await?;
                }
            }
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

    // Determine final_output: the target's output when there is exactly one, otherwise the last
    // planned step's (no target). Several targets report theirs in `outcomes`.
    let final_output = if let [tid] = target_ids.as_slice() {
        // For partitioned targets, the first partition by key (deterministic).
        output_for(tid, &all_outputs)
    } else if keep_going {
        None
    } else {
        // No target: return the last planned asset's output (a sensor's only when the plan
        // has no asset).
        let planned: Vec<&planner::StreamStep> = exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .collect();
        let last_planned_id = planned
            .iter()
            .rev()
            .find(|st| {
                dag.get_node(st.step_id.base_id())
                    .is_some_and(|n| n.kind() == crate::NodeKind::Asset)
            })
            .or(planned.last())
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
    };

    Ok(Executed {
        result: GetResult {
            run_id,
            elapsed_seconds: elapsed,
            steps_executed,
            phases: exec_plan.phases.len(),
            final_output,
            steps: step_reports,
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
    /// Sensor step -> content hash of the output it returned in this run.
    output_hashes: &'a HashMap<String, String>,
    /// Run-end snapshot of the measured-cost EWMA, seeding the next run.
    cost_snapshot: &'a [(String, crate::cost::NodeEstimate)],
}

/// Write a run's ledger with a short-lived connection. Idempotent for the run
/// row (INSERT OR IGNORE + terminal UPDATE) so replays don't duplicate it;
/// materialization rows are append-only history and re-appended on replay
/// only against a database that doesn't already contain them.
async fn persist_run(db_path: &str, l: &RunLedger<'_>) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;

    conn.execute(
            "INSERT OR IGNORE INTO runs (run_id, command, files, target, status, steps_total) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
            [
                l.run_id.to_string(),
                l.command.to_string(),
                l.files.clone(),
                l.target.unwrap_or("").to_string(),
                l.steps_total.to_string(),
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

    for (node_id, oref) in l.all_outputs {
        if l.cached_node_ids.contains(node_id) {
            continue;
        }
        let Some(run_h) = l.run_hashes.get(node_id) else {
            continue;
        };
        let elapsed_str = oref
            .elapsed_seconds
            .map(|e| e.to_string())
            .unwrap_or_default();
        let base = crate::StepId::parse(node_id).base_id().to_string();
        let attempts = l.all_attempts.get(&base).copied().unwrap_or(1);
        let (cpu, rss) = l.all_timings.get(node_id).copied().unwrap_or((None, None));
        conn.execute(
                "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes, output_hash) VALUES (?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''))",
                [
                    node_id.clone(),
                    run_h.clone(),
                    oref.path.clone(),
                    oref.format.clone(),
                    oref.size_bytes.to_string(),
                    elapsed_str,
                    attempts.to_string(),
                    l.all_sinks.get(node_id).cloned().unwrap_or_default(),
                    cpu.map(|c| c.to_string()).unwrap_or_default(),
                    rss.map(|r| r.to_string()).unwrap_or_default(),
                    l.output_hashes.get(node_id).cloned().unwrap_or_default(),
                ],
            )
            .await
            .ok();
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
        let base = crate::StepId::parse(node_id).base_id().to_string();
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        let attempts = l
            .all_attempts
            .get(&base)
            .copied()
            .unwrap_or(failure.error.attempts);
        conn.execute(
                "INSERT INTO materializations (node_id, run_hash, status, error_message, error_traceback, attempts) VALUES (?1, ?2, 'failed', ?3, ?4, ?5)",
                [
                    node_id.clone(),
                    run_h,
                    failure.error.message.clone(),
                    failure.error.traceback.clone(),
                    attempts.to_string(),
                ],
            )
            .await
            .ok();
    }
    Ok(())
}

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

    Ok(PlanResult {
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

fn filter_plan_to_subgraph(plan: ExecutionPlan, subgraph_ids: &[&str]) -> ExecutionPlan {
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

// ─── DAG construction ────────────────────────────────────────────────────────

/// Build the DAG from source files. The work is genuinely blocking (file I/O,
/// parsing, and a Python subprocess for dynamic partitions), so it runs on the
/// blocking pool rather than an async worker thread.
pub async fn build_dag(file_args: &[String], python: &std::path::Path) -> Result<Dag, BarcaError> {
    let files = file_args.to_vec();
    let py = python.to_path_buf();
    tokio::task::spawn_blocking(move || build_dag_blocking(&files, &py))
        .await
        .map_err(|e| BarcaError::Other(format!("DAG analysis task failed: {e}")))?
}

/// The directory a pipeline file lives in, as a path that can be read: the directory its helper
/// modules are scanned from (static analysis), and the one `barca serve --watch` watches.
///
/// `Path::new("p.py").parent()` is `Some("")`, an empty path that `read_dir` cannot open, so a
/// bare filename used to scan no helpers at all (#178). Every spelling of the same file must
/// scan the same directory: `p.py` and `./p.py` give `.`, `sub/p.py` gives `sub`, and an
/// absolute path gives its parent. (The CLI already normalizes file arguments to root-relative
/// paths, so node ids do not depend on the spelling either.)
pub fn source_dir(file: &std::path::Path) -> PathBuf {
    match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

pub(crate) fn build_dag_blocking(
    file_args: &[String],
    python: &std::path::Path,
) -> Result<Dag, BarcaError> {
    let paths: Vec<PathBuf> = file_args.iter().map(PathBuf::from).collect();

    // Parse every file once. Files are grouped by directory because that directory is what the
    // worker puts on sys.path, so it is what a bare `import helpers` in the file refers to: two
    // `helpers.py` (or two `assets.py`) in different directories must never share hashing state.
    struct ParsedFile {
        index: usize,
        stem: String,
        source: String,
        nodes: Vec<crate::model::ExtractedNode>,
    }
    let mut by_dir: std::collections::BTreeMap<PathBuf, Vec<ParsedFile>> =
        std::collections::BTreeMap::new();
    for (index, path) in paths.iter().enumerate() {
        let source = fs::read_to_string(path)
            .map_err(|e| BarcaError::Usage(format!("{}: {e}", path.display())))?;
        let file_str = path.to_string_lossy().to_string();
        let nodes =
            extract_nodes(&source, &file_str).map_err(|e| BarcaError::Parse(e.to_string()))?;
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        by_dir
            .entry(source_dir(path))
            .or_default()
            .push(ParsedFile {
                index,
                stem,
                source,
                nodes,
            });
    }

    // Nodes per file, in command-line order (the last asset is `get file.py`'s final value).
    let mut per_file: Vec<Vec<crate::model::ExtractedNode>> = vec![Vec::new(); paths.len()];
    for (dir, files) in &by_dir {
        // Module name -> source for everything an import in this directory can reach:
        // the directory's own files first, then packages and modules below it, then siblings,
        // then (as before 0.13) the other files on the command line.
        let mut file_sources: HashMap<String, String> = HashMap::new();
        // Dotted module names in `file_sources` that are `__init__.py` packages,
        // as opposed to regular submodules — needed to resolve relative imports.
        let mut packages: std::collections::HashSet<String> = std::collections::HashSet::new();
        for f in files {
            file_sources.insert(f.stem.clone(), f.source.clone());
        }
        // Subdirectories first — packages (__init__.py) take precedence over same-named
        // sibling .py files, matching Python's import semantics.
        scan_subdirectories(dir, dir, &mut file_sources, &mut packages);
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let ep = entry.path();
                if ep.extension().map(|e| e == "py").unwrap_or(false) {
                    let estem = ep
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if let std::collections::hash_map::Entry::Vacant(e) = file_sources.entry(estem)
                        && let Ok(content) = fs::read_to_string(&ep)
                    {
                        e.insert(content);
                    }
                }
            }
        }
        for (other_dir, others) in &by_dir {
            if other_dir != dir {
                for f in others {
                    file_sources
                        .entry(f.stem.clone())
                        .or_insert_with(|| f.source.clone());
                }
            }
        }

        // Each file's own definitions are parsed once, then shared by its nodes.
        for f in files {
            let defs = crate::cone::collect_module_definitions(&f.source);
            for node in &f.nodes {
                let mut node = node.clone();
                node.cone_hash = crate::cone::cone_hash_from_defs(
                    &defs,
                    &node.function_name,
                    &file_sources,
                    &packages,
                );
                per_file[f.index].push(node);
            }
        }
    }
    drop(by_dir);
    let mut all_nodes: Vec<crate::model::ExtractedNode> = per_file.into_iter().flatten().collect();

    resolve_dynamic_partitions(&mut all_nodes, python);

    Ok(Dag::build(&all_nodes)?)
}

/// Recursively scan subdirectories for Python modules.
/// Stores dotted module paths as keys: `utils/math.py` → `"utils.math"`.
/// Handles `__init__.py`: `mylib/__init__.py` → `"mylib"`.
fn scan_subdirectories(
    dir: &std::path::Path,
    root: &std::path::Path,
    file_sources: &mut HashMap<String, String>,
    packages: &mut std::collections::HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let ep = entry.path();
        if ep.is_dir() {
            let dir_name = ep
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            // Skip hidden dirs, __pycache__, .venv, etc.
            if dir_name.starts_with('.')
                || dir_name == "__pycache__"
                || dir_name == ".venv"
                || dir_name == "node_modules"
            {
                continue;
            }
            // Check if this is a Python package (has __init__.py).
            let init_path = ep.join("__init__.py");
            if init_path.exists()
                && let Ok(content) = fs::read_to_string(&init_path)
            {
                let module_path = ep
                    .strip_prefix(root)
                    .unwrap_or(&ep)
                    .to_string_lossy()
                    .replace(['/', '\\'], ".");
                packages.insert(module_path.clone());
                file_sources.entry(module_path).or_insert_with(|| content);
            }
            // Scan .py files in the subdirectory.
            if let Ok(sub_entries) = std::fs::read_dir(&ep) {
                for sub_entry in sub_entries.flatten() {
                    let sp = sub_entry.path();
                    if sp.extension().map(|e| e == "py").unwrap_or(false)
                        && sp.file_name().map(|n| n != "__init__.py").unwrap_or(true)
                        && let Ok(content) = fs::read_to_string(&sp)
                    {
                        // Build dotted module path relative to root.
                        let rel = sp.strip_prefix(root).unwrap_or(&sp);
                        let module_path = rel
                            .to_string_lossy()
                            .replace(['/', '\\'], ".")
                            .trim_end_matches(".py")
                            .to_string();
                        file_sources.entry(module_path).or_insert_with(|| content);
                    }
                }
            }
            // Recurse into deeper subdirectories.
            scan_subdirectories(&ep, root, file_sources, packages);
        }
    }
}

fn resolve_dynamic_partitions(nodes: &mut [crate::model::ExtractedNode], python: &std::path::Path) {
    for node in nodes.iter_mut() {
        let mut resolved: Vec<(String, Vec<crate::model::PartitionValue>)> = Vec::new();

        for (dim, spec) in &node.partitions {
            if let crate::model::PartitionSpec::Dynamic { source_text } = spec {
                let module_path = std::path::Path::new(&node.source_file)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(&node.source_file));
                // Compile from source, never a cached .pyc, like the worker (#176).
                let script = "import json, sys\n\
                     from barca._source_import import load_source_module\n\
                     _mod = load_source_module(sys.argv[1], '_m')\n\
                     _ns = vars(_mod); _ns['__builtins__'] = __builtins__\n\
                     print(json.dumps(eval(sys.argv[2], _ns)))\n"
                    .to_string();
                let mut script_file =
                    tempfile::NamedTempFile::new().expect("failed to create temp file");
                use std::io::Write;
                script_file
                    .write_all(script.as_bytes())
                    .expect("failed to write script");
                let script_path = script_file.path().to_path_buf();
                let output = Command::new(python)
                    .arg(&script_path)
                    .arg(module_path.to_string_lossy().as_ref())
                    .arg(source_text)
                    .output()
                    .unwrap_or_else(|e| {
                        panic!(
                            "Failed to evaluate partition expression for {}: {e}",
                            node.function_name
                        )
                    });

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!(
                        "[barca] warning: failed to evaluate partition expression '{}' for {}: {}",
                        source_text,
                        node.function_name,
                        stderr.trim()
                    );
                    continue;
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                let values: Vec<serde_json::Value> =
                    serde_json::from_str(stdout.trim()).unwrap_or_default();
                let partition_values: Vec<crate::model::PartitionValue> = values
                    .into_iter()
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => Some(crate::model::PartitionValue::Str(s)),
                        serde_json::Value::Number(n) => {
                            n.as_i64().map(crate::model::PartitionValue::Int)
                        }
                        _ => None,
                    })
                    .collect();

                resolved.push((dim.clone(), partition_values));
            }
        }

        for (dim, values) in resolved {
            node.partitions
                .insert(dim, crate::model::PartitionSpec::Static { values });
        }
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
mod source_dir_tests {
    use super::source_dir;
    use std::path::{Path, PathBuf};

    /// Every spelling of a pipeline file names a directory that can be read (#178): a bare
    /// filename used to give an empty path, so no helper module was scanned or hashed.
    #[test]
    fn every_spelling_of_a_file_names_a_readable_directory() {
        assert_eq!(source_dir(Path::new("p.py")), PathBuf::from("."));
        assert_eq!(source_dir(Path::new("./p.py")), PathBuf::from("."));
        assert_eq!(source_dir(Path::new("sub/p.py")), PathBuf::from("sub"));
        assert_eq!(
            source_dir(Path::new("/abs/sub/p.py")),
            PathBuf::from("/abs/sub")
        );
        assert!(std::fs::read_dir(source_dir(Path::new("p.py"))).is_ok());
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
