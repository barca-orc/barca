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
use crate::transfer::{ArtifactLayout, TransferClient};
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
/// targets assets and `barca run` targets tasks.
fn resolve_target(
    dag: &Dag,
    target_name: Option<&str>,
    command_label: &str,
) -> Result<Option<String>, BarcaError> {
    match target_name {
        Some(name) => {
            let id = dag
                .topo_order()
                .into_iter()
                .find(|id| id.ends_with(&format!(":{name}")) || *id == name || id.ends_with(name))
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    let available: Vec<&str> = dag.topo_order();
                    BarcaError::AssetNotFound(name.to_string(), available.join(", "))
                })?;
            // Enforce get/run semantics: `barca get` is for assets, `barca run` is for tasks.
            if let Some(node) = dag.get_node(&id) {
                let kind = node.kind();
                if command_label == "get" && kind == crate::NodeKind::Task {
                    return Err(BarcaError::Other(format!(
                        "'{name}' is a task — use `barca run` instead"
                    )));
                }
                if command_label == "run" && kind == crate::NodeKind::Asset {
                    return Err(BarcaError::Other(format!(
                        "'{name}' is an asset — use `barca get` instead"
                    )));
                }
            }
            Ok(Some(id))
        }
        None => Ok(None),
    }
}

// ─── Cache decisions ─────────────────────────────────────────────────────────
//
// One function decides what happens to a step; a real run and `--dry-run` both call it, so the
// dry run cannot drift from what a run would do.

/// Why a step runs instead of being served from cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunReason {
    Task,
    Sensor,
    NoCache,
    Refresh,
    RefreshAll,
    NotMaterialized,
}

impl RunReason {
    fn code(self) -> &'static str {
        match self {
            RunReason::Task => "task",
            RunReason::Sensor => "sensor",
            RunReason::NoCache => "no_cache",
            RunReason::Refresh => "refresh",
            RunReason::RefreshAll => "refresh_all",
            RunReason::NotMaterialized => "not_materialized",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            RunReason::Task => "tasks always re-run",
            RunReason::Sensor => "sensors always re-run",
            RunReason::NoCache => "--no-cache",
            RunReason::Refresh => "named in --refresh",
            RunReason::RefreshAll => "--refresh-all",
            RunReason::NotMaterialized => "no cached result for this code and these inputs",
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
    stale_cached: HashMap<String, String>,
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

    // Refresh policy (`barca run`): force-rerun assets in/named by the refresh set.
    let is_asset = kind == Some(crate::NodeKind::Asset);
    let refresh = match policy {
        CachePolicy::CacheAware => None,
        CachePolicy::RefreshAll => is_asset.then_some(RunReason::RefreshAll),
        CachePolicy::RefreshSelective(names) => (is_asset
            && names.iter().any(|name| refresh_name_matches(base_id, name)))
        .then_some(RunReason::Refresh),
    };
    if let Some(reason) = refresh {
        state.refreshed_ids.insert(base_id.to_string());
        return (step, Decision::Run(reason));
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
         refresh. Add it to --refresh (for example --refresh {root},{id}) or use --refresh-all."
    )
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
            r.detail = Some(reason.detail().to_string());
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
                r.detail = Some(RunReason::NotMaterialized.detail().to_string());
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
/// asset of the target: a typo must not be a silent no-op.
fn validate_refresh_names(
    dag: &Dag,
    target_id: Option<&str>,
    names: &[String],
) -> Result<(), BarcaError> {
    let cone: Vec<&str> = match target_id {
        Some(tid) => dag.subgraph(tid),
        None => dag.topo_order(),
    };
    let assets: Vec<&str> = cone
        .into_iter()
        .filter(|id| Some(*id) != target_id)
        .filter(|id| {
            dag.get_node(id)
                .is_some_and(|n| n.kind() == crate::NodeKind::Asset)
        })
        .collect();
    for name in names {
        if !assets.iter().any(|id| refresh_name_matches(id, name)) {
            let valid: Vec<&str> = assets.iter().map(|id| short_name(id)).collect();
            return Err(BarcaError::Other(format!(
                "--refresh: no upstream asset named '{name}'{}.\n\
                 Upstream assets you can refresh: {}\n\
                 Pass several as a comma-separated list: --refresh {}",
                target_id
                    .map(|t| format!(" in the cone of '{}'", short_name(t)))
                    .unwrap_or_default(),
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
    /// Not reachable here (recorded against a different store, or a local
    /// path that is not on this disk) — treat as a miss and recompute.
    Miss,
}

/// Resolve a cache row against this run's artifact store (`layout` is Some
/// when the store is separate from the local artifact dir).
fn resolve_cache_hit(oref: dispatch::OutputRef, layout: Option<&ArtifactLayout>) -> CacheHit {
    let Some(layout) = layout else {
        return CacheHit::Local(oref);
    };
    if let Some(local) = layout.local_for(&oref.path) {
        let store = oref.path.clone();
        return CacheHit::Store {
            local: dispatch::OutputRef {
                path: local.to_string_lossy().into_owned(),
                ..oref
            },
            store,
        };
    }
    if std::path::Path::new(&oref.path).exists() {
        CacheHit::Local(oref)
    } else {
        CacheHit::Miss
    }
}

/// Apply this run's artifact store to a cache row: the output to use, or None
/// to treat the row as a miss.
fn accept_cache_hit(
    store: &mut Option<StoreSync>,
    node_id: &str,
    oref: dispatch::OutputRef,
) -> Option<dispatch::OutputRef> {
    match resolve_cache_hit(oref, store.as_ref().map(|s| &s.layout)) {
        CacheHit::Local(o) => Some(o),
        CacheHit::Store { local, store: at } => {
            if let Some(s) = store.as_mut() {
                s.fetchable
                    .insert(local.path.clone(), (node_id.to_string(), at));
            }
            Some(local)
        }
        CacheHit::Miss => None,
    }
}

/// Apply this run's artifact store to a cache decision: cached outputs point
/// at their local mirror, and rows not reachable here become runs.
fn localize_decision(
    decision: Decision,
    step: &crate::planner::StreamStep,
    store: &mut Option<StoreSync>,
) -> Decision {
    match decision {
        Decision::Cached { oref, stale_root } => {
            let id = step.step_id.display();
            match accept_cache_hit(store, &id, oref) {
                Some(oref) => Decision::Cached { oref, stale_root },
                None => Decision::Run(RunReason::NotMaterialized),
            }
        }
        Decision::Partitioned {
            cached,
            mut missing,
        } => {
            let mut kept = Vec::with_capacity(cached.len());
            for (pdisplay, oref) in cached {
                match accept_cache_hit(store, &pdisplay, oref) {
                    Some(oref) => kept.push((pdisplay, oref)),
                    None => missing.extend(
                        step.partition_keys
                            .iter()
                            .find(|pk| pk.display_id(&step.step_id.base) == pdisplay)
                            .cloned(),
                    ),
                }
            }
            Decision::Partitioned {
                cached: kept,
                missing,
            }
        }
        run => run,
    }
}

/// This run's link to a separate artifact store: the transfer helper plus
/// the cache hits whose artifacts still live only in the store.
struct StoreSync {
    client: TransferClient,
    layout: ArtifactLayout,
    /// Local mirror path → (node id, store location), for store-backed cache
    /// hits. Fetched on first use, so fully-cached intermediates a run never
    /// reads are never downloaded.
    fetchable: HashMap<String, (String, String)>,
}

impl StoreSync {
    /// Make the store-backed artifacts among `paths` local, reporting any
    /// fetch on stderr (through the progress bar when one is live). Errors
    /// name what could not be fetched.
    async fn ensure_local<'a>(
        &mut self,
        paths: impl IntoIterator<Item = &'a str>,
        pb: Option<&indicatif::ProgressBar>,
    ) -> Result<(), String> {
        let mut locals = Vec::new();
        for path in paths {
            if let Some((node, store)) = self.fetchable.remove(path)
                && let Some(local) = self.client.fetch(&node, &store)
            {
                locals.push(local);
            }
        }
        if locals.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let report = self.client.await_fetches(&locals).await;
        if report.transferred > 0 {
            let msg = format!(
                "[barca] fetched {} cached artifact{} ({}) in {:.1}s",
                report.transferred,
                if report.transferred == 1 { "" } else { "s" },
                fmt_bytes(report.bytes),
                started.elapsed().as_secs_f64()
            );
            match pb {
                Some(bar) => bar.println(&msg),
                None => eprintln!("{msg}"),
            }
        }
        if report.failures.is_empty() {
            return Ok(());
        }
        let detail: Vec<String> = report
            .failures
            .iter()
            .map(|f| format!("  {} ({}): {}", f.key, f.store, f.message))
            .collect();
        Err(format!(
            "could not fetch {} cached artifact(s) from the artifact store:\n{}\n\
             Re-run with --no-cache to recompute them.",
            report.failures.len(),
            detail.join("\n")
        ))
    }
}

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

/// How a step was (or, in a dry run, will be) treated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepReport {
    pub id: String,
    /// `asset`, `task` or `sensor`.
    pub kind: String,
    /// Dry run only: `cached`, `run`, `partial` (some partition keys cached) or `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Real run only: `ran`, `cached` or `partial`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why the step runs: `task`, `sensor`, `no_cache`, `refresh`, `refresh_all`,
    /// `not_materialized`, or `partitions_unknown`.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainResult {
    pub dry_run: bool,
    pub command: String,
    pub target: Option<String>,
    pub steps: Vec<StepReport>,
    pub summary: ExplainSummary,
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
    pub reason: String,
    pub streams: Vec<PlanStream>,
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
    /// Normal cache-aware behavior — reuse fresh asset artifacts (`barca get`).
    CacheAware,
    /// Force-rerun every asset in the target's cone
    /// (`barca run <task> --refresh-all` / `--no-cache`).
    RefreshAll,
    /// Force-rerun only the named assets; all others stay cache-aware
    /// (`barca run <task> --refresh a,b`). A name matches when it equals the
    /// node's base id exactly, or matches the trailing `:name` segment.
    RefreshSelective(Vec<String>),
}

/// `barca get` — cache-aware execution of an asset (or all assets).
/// Cancelling `cancel` stops the run mid-flight: workers are terminated,
/// partial results are persisted, and the run row is marked `cancelled`.
pub async fn get(
    cfg: &crate::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &PathBuf,
    no_cache: bool,
    agent_mode: bool,
    cancel: CancellationToken,
) -> Result<GetResult, BarcaError> {
    execute(
        cfg,
        target_name,
        file_args,
        python,
        no_cache,
        agent_mode,
        CachePolicy::CacheAware,
        "get",
        cancel,
    )
    .await
}

/// `barca run` — execute a task (and its cone). The task always re-runs;
/// upstream assets follow `policy` (`CacheAware` by default, like `barca get`).
pub async fn run(
    cfg: &crate::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &PathBuf,
    policy: CachePolicy,
    agent_mode: bool,
    cancel: CancellationToken,
) -> Result<GetResult, BarcaError> {
    execute(
        cfg,
        Some(target_name),
        file_args,
        python,
        false,
        agent_mode,
        policy,
        "run",
        cancel,
    )
    .await
}

/// `barca get|run --dry-run` — report what the command would do, without doing it.
///
/// Plans exactly as a real run does and sends every step through [`decide_step`], so the
/// prediction is the real run's decision. Nothing executes, no worker starts, and nothing is
/// written: no `.barca` directory is created and no run is recorded. The one thing a dry run
/// cannot know is the key set of a dynamic partition (`partitions_from`) whose source has to
/// run first; those steps (and anything depending on them) are reported as `unknown`.
pub async fn explain(
    cfg: &crate::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &PathBuf,
    policy: CachePolicy,
    no_cache: bool,
    command_label: &str,
) -> Result<ExplainResult, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    let target_id = resolve_target(&dag, target_name, command_label)?;
    let pool_size = default_pool_size();
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let full_plan = planner::plan_from_dag(&dag, &config);
    let exec_plan = if let Some(ref tid) = target_id {
        let subgraph_ids = dag.subgraph(tid);
        filter_plan_to_subgraph(full_plan, &subgraph_ids)
    } else {
        full_plan
    };
    if let CachePolicy::RefreshSelective(names) = &policy {
        validate_refresh_names(&dag, target_id.as_deref(), names)?;
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
    let mut all_outputs: HashMap<String, OutputRef> = HashMap::new();
    let mut unknown_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut steps: Vec<StepReport> = Vec::new();
    let mut summary = ExplainSummary::default();

    let unknown_report = |dag: &Dag, base_id: &str, detail: String| StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        action: Some("unknown".to_string()),
        reason: Some("partitions_unknown".to_string()),
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
                            &dag,
                            base,
                            format!(
                                "partition keys come from the output of '{src}', which is not \
                                 available until it runs"
                            ),
                        ));
                        unknown_ids.insert(base.to_string());
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
                let unknown_dep = step
                    .inputs
                    .values()
                    .find(|up| unknown_ids.contains(up.split('[').next().unwrap_or(up.as_str())));
                if let Some(up) = unknown_dep {
                    steps.push(unknown_report(
                        &dag,
                        base,
                        format!(
                            "depends on '{}', whose partitions are not known until it runs",
                            short_name(up)
                        ),
                    ));
                    unknown_ids.insert(base.to_string());
                    summary.unknown += 1;
                    continue;
                }

                let (step, decision) =
                    decide_step(&dag, &policy, no_cache, cache.as_ref(), &mut state, step).await;
                steps.push(report_for(&dag, &step, &decision, true));
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

    Ok(ExplainResult {
        dry_run: true,
        command: command_label.to_string(),
        target: target_id.as_deref().map(|t| short_name(t).to_string()),
        steps: merge_partition_reports(steps),
        summary,
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    cfg: &crate::config::ResolvedConfig,
    target_name: Option<&str>,
    file_args: &[String],
    python: &PathBuf,
    no_cache: bool,
    agent_mode: bool,
    policy: CachePolicy,
    command_label: &str,
    cancel: CancellationToken,
) -> Result<GetResult, BarcaError> {
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

    // Start remote I/O first so it overlaps parsing and planning: the shared
    // state pull (joined just before the metadata DB is opened) and the
    // artifact transfer helper's startup (joined before workers start).
    let state_sync_on =
        cfg.state == crate::config::StateMode::Optimistic && cfg.state_uri.is_some();
    let pull = state_sync_on.then(|| {
        let (python, cfg) = (python.clone(), cfg.clone());
        Background::spawn(async move {
            let started = Instant::now();
            let token = state_sync::pull_state(&python, &cfg).await?;
            Ok::<_, BarcaError>((token, started.elapsed()))
        })
    });
    let transfer_start = cfg.remote_artifacts().then(|| {
        let (python, cfg, run_id) = (python.clone(), cfg.clone(), run_id.clone());
        Background::spawn(async move { TransferClient::start(&python, &cfg, &run_id).await })
    });

    let dag = build_dag(file_args, python).await?;
    trace_point!("dag_built");

    let target_id = resolve_target(&dag, target_name, command_label)?;

    let pool_size = default_pool_size();
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let full_plan = planner::plan_from_dag(&dag, &config);
    let exec_plan = if let Some(ref tid) = target_id {
        let subgraph_ids = dag.subgraph(tid);
        filter_plan_to_subgraph(full_plan, &subgraph_ids)
    } else {
        full_plan
    };
    trace_point!("planned");

    if let CachePolicy::RefreshSelective(names) = &policy {
        validate_refresh_names(&dag, target_id.as_deref(), names)?;
    }

    db::ensure_env_dirs(&cfg.env)?;
    let db_path = cfg.db_path.clone();

    // Shared remote state: the pull must land before the DB is opened, so
    // cache checks below see every machine's materializations. Pull failure
    // is a hard error — silently diverging local runs are worse than stopping.
    let mut state_token = match pull {
        Some(pull) => {
            let (token, took) = pull.join().await??;
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
        &file_args.join(" "),
        target_name,
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

    // Separate artifact store: workers still read and write only the local
    // artifact dir; the transfer helper uploads finished artifacts in the
    // background and fetches cache hits recorded by other machines.
    let mut store: Option<StoreSync> = if let Some(start) = transfer_start {
        let client = start.join().await??;
        let layout = client.layout().clone();
        Some(StoreSync {
            client,
            layout,
            fetchable: HashMap::new(),
        })
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
        python: python.clone(),
        pool_size,
        run_id: run_id.clone(),
        artifact_root: worker_artifact_root,
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

        // `partitions_from` sources are read from disk during expansion.
        if let Some(s) = store.as_mut() {
            let sources: Vec<String> = dispatch::partition_sources(phase, &all_outputs)
                .into_iter()
                .map(|o| o.path.clone())
                .collect();
            if let Err(e) = s
                .ensure_local(sources.iter().map(String::as_str), pb.as_ref())
                .await
            {
                transfer_error = Some(e);
                break;
            }
        }

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
                            eprintln!("[barca] step:{display_id} cached");
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

        // Cache hits recorded by other machines are fetched before the steps
        // that read them run — exactly the inputs this phase was provided.
        if let Some(s) = store.as_mut() {
            let paths = provided.values().flat_map(|p| match p {
                dispatch::ProvidedInput::Single(o) => std::slice::from_ref(o),
                dispatch::ProvidedInput::Collected(v) => v.as_slice(),
            });
            if let Err(e) = s
                .ensure_local(paths.map(|o| o.path.as_str()), pb.as_ref())
                .await
            {
                transfer_error = Some(e);
                break;
            }
            trace_point!("phase{phase_idx}_inputs_local");
        }

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
                        "[barca] step:{} completed {:.1}s ({}/{})",
                        node_id,
                        elapsed_s.unwrap_or(0.0),
                        completed_steps,
                        total_steps
                    );
                }
            });

        // Drive this phase against the persistent pool. The cost model both
        // sizes the batch pulls and absorbs the timings coming back.
        let phase_err = pool
            .run_phase(&mut coord, &mut cost_model, Some(on_step_cb), &cancel)
            .await;
        trace_point!("phase{phase_idx}_run_phase_done");
        if let Err(e) = phase_err {
            if phase_error.is_none() {
                phase_error = Some(e);
            }
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
            phase_outputs.insert(node_id, oref);
        }

        // Collect failures — parallel branch failures (group members) are
        // contained within the group and surfaced as ParallelError to the parent,
        // so they should NOT abort the entire phase.
        let mut first_non_group_failure: Option<String> = None;
        for (item_id, error_msg) in coord.failed_items() {
            let item = coord.item(item_id);
            if item.group.is_some() {
                // Parallel branch failure — handled by the group/parent, not a phase error.
                continue;
            }
            let node_id = item.step_id.display();
            if first_non_group_failure.is_none() {
                first_non_group_failure = Some(error_msg.to_string());
            }
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

        // Propagate non-group failures into phase_error if not already set
        if let Some(msg) = first_non_group_failure {
            if phase_error.is_none() {
                phase_error = Some(msg);
            }
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

        // If this phase had a worker failure, stop after collecting partial results.
        if phase_error.is_some() {
            break;
        }
    }

    // All phases done (or aborted/cancelled) — release the worker pool before
    // persisting.
    pool.shutdown().await;
    trace_point!("pool_shutdown");

    // Finish progress bar.
    if let Some(ref bar) = pb {
        if steps_executed > 0 {
            bar.finish_and_clear();
            eprintln!(
                "[barca] {}/{} steps done in {:.1}s",
                completed_steps, total_steps, elapsed_so_far
            );
        } else {
            bar.finish_and_clear();
        }
    } else if agent_mode && steps_executed > 0 {
        eprintln!(
            "[barca] {}/{} steps | done in {:.1}s",
            completed_steps, total_steps, elapsed_so_far
        );
    }

    let was_cancelled = cancel.is_cancelled();

    // Determine final_output: use target if specified, otherwise last planned step.
    let final_output = if let Some(ref tid) = target_id {
        all_outputs.get(tid).cloned().or_else(|| {
            // For partitioned targets, sort by key for deterministic output.
            let prefix = format!("{tid}[");
            let mut matches: Vec<_> = all_outputs
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .collect();
            matches.sort_by_key(|(k, _)| (*k).clone());
            matches.first().map(|(_, v)| (*v).clone())
        })
    } else {
        // No target: return the last planned step's output.
        let last_planned_id = exec_plan
            .phases
            .last()
            .and_then(|p| p.streams.last())
            .and_then(|s| s.steps.last())
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

    // Artifact store: make the final output readable here, then wait for
    // every upload — rows are recorded only for artifacts confirmed in the
    // store, so the metadata never points at a missing object.
    if let Some(mut s) = store.take() {
        if was_cancelled {
            for node in s.client.abort().await {
                all_outputs.remove(&node);
                store_paths.remove(&node);
            }
        } else {
            if transfer_error.is_none()
                && phase_error.is_none()
                && let Some(ref out) = final_output
                && let Err(e) = s.ensure_local([out.path.as_str()], None).await
            {
                transfer_error = Some(e);
            }
            let queued = s.client.pending_uploads();
            let t_drain = Instant::now();
            let report = s.client.drain().await;
            s.client.shutdown().await;
            trace_point!("store_sync_drained ({queued} uploads)");
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
                transfer_error.get_or_insert(format!(
                    "{} artifact upload(s) failed — those steps were not recorded and \
                     will recompute next run:\n{}",
                    report.failures.len(),
                    detail.join("\n")
                ));
            }
        }
    }

    let steps_cached = cached_node_ids.len();
    let elapsed = t0.elapsed().as_secs_f64();

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
        } else if phase_error.is_some() || transfer_error.is_some() {
            "failed"
        } else {
            "success"
        },
        command: command_label,
        files: file_args.join(" "),
        target: target_name,
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
        store_paths: &store_paths,
        cost_snapshot: &cost_snapshot,
    };
    persist_run(&db_path, &ledger).await?;
    trace_point!("persist_run_done");

    // Shared remote state: fold the WAL into the main file and conditionally
    // upload it. On conflict (another machine pushed first): pull the fresh
    // database, replay this run's ledger onto it, retry.
    if state_sync_on {
        let mut attempt = 0u32;
        let t_push = Instant::now();
        loop {
            state_sync::checkpoint_truncate(&db_path).await?;
            match state_sync::push_state(python, cfg, state_token.as_ref().unwrap()).await? {
                state_sync::PushOutcome::Pushed(_) => {
                    eprintln!(
                        "[barca] pushed state ({}) in {:.2}s{}",
                        fmt_bytes(std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)),
                        t_push.elapsed().as_secs_f64(),
                        match attempt {
                            0 => String::new(),
                            1 => " after 1 conflict retry".to_string(),
                            n => format!(" after {n} conflict retries"),
                        }
                    );
                    trace_point!("state_sync_pushed (attempts={})", attempt + 1);
                    break;
                }
                state_sync::PushOutcome::Conflict => {
                    if attempt >= cfg.push_retries {
                        return Err(BarcaError::Other(format!(
                            "shared state push conflicted {attempt} times — results were \
                             computed but the shared state was not updated; re-run to retry"
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
        return Err(BarcaError::WorkerFailed(error));
    }
    if let Some(error) = transfer_error {
        return Err(BarcaError::Other(error));
    }

    Ok(GetResult {
        run_id,
        elapsed_seconds: elapsed,
        steps_executed,
        phases: exec_plan.phases.len(),
        final_output,
        steps: merge_partition_reports(step_reports),
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
    /// Artifact-store location of each uploaded output, recorded instead of
    /// its local path so cache hits resolve on every machine.
    store_paths: &'a HashMap<String, String>,
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
                "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes) VALUES (?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''))",
                [
                    node_id.clone(),
                    run_h.clone(),
                    l.store_paths
                        .get(node_id)
                        .unwrap_or(&oref.path)
                        .clone(),
                    oref.format.clone(),
                    oref.size_bytes.to_string(),
                    elapsed_str,
                    attempts.to_string(),
                    l.all_sinks.get(node_id).cloned().unwrap_or_default(),
                    cpu.map(|c| c.to_string()).unwrap_or_default(),
                    rss.map(|r| r.to_string()).unwrap_or_default(),
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
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        // Each failure carries its own attempt count: dispatches for a worker
        // failure, transfer attempts for an upload failure.
        conn.execute(
                "INSERT INTO materializations (node_id, run_hash, status, error_type, error_message, error_traceback, attempts) VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6)",
                [
                    node_id.clone(),
                    run_h,
                    failure.error.error_type.clone(),
                    failure.error.message.clone(),
                    failure.error.traceback.clone(),
                    failure.error.attempts.to_string(),
                ],
            )
            .await
            .ok();
    }
    Ok(())
}

// ─── plan ────────────────────────────────────────────────────────────────────

pub async fn plan(file_args: &[String], python: &PathBuf) -> Result<PlanResult, BarcaError> {
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
                reason: format!("{:?}", p.reason),
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

pub async fn history(
    cfg: &crate::config::ResolvedConfig,
    limit: usize,
) -> Result<Vec<db::RunRecord>, BarcaError> {
    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::get_recent_runs(&cfg.db_path, limit).await
}

// ─── stats ────────────────────────────────────────────────────────────────────

pub async fn stats(
    cfg: &crate::config::ResolvedConfig,
    target_name: &str,
    file_args: &[String],
    python: &PathBuf,
) -> Result<db::AssetStats, BarcaError> {
    let dag = build_dag(file_args, python).await?;

    let target_id = dag
        .topo_order()
        .into_iter()
        .find(|id| {
            id.ends_with(&format!(":{target_name}"))
                || *id == target_name
                || id.ends_with(target_name)
        })
        .map(|s| s.to_string())
        .ok_or_else(|| {
            let available: Vec<&str> = dag.topo_order();
            BarcaError::AssetNotFound(target_name.to_string(), available.join(", "))
        })?;

    db::ensure_env_dirs(&cfg.env)?;
    db::init_db(&cfg.db_path).await?;
    db::get_asset_stats(&cfg.db_path, &target_id).await
}

// ─── list_assets ──────────────────────────────────────────────────────────────

/// Build the DAG and return a summary of every node (id, kind, freshness, inputs).
/// Pure static analysis — no execution, no DB. Used by the server's `/assets` route.
pub async fn list_assets(
    file_args: &[String],
    python: &PathBuf,
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
pub async fn build_dag(file_args: &[String], python: &PathBuf) -> Result<Dag, BarcaError> {
    let files = file_args.to_vec();
    let py = python.clone();
    tokio::task::spawn_blocking(move || build_dag_blocking(&files, &py))
        .await
        .map_err(|e| BarcaError::Other(format!("DAG analysis task failed: {e}")))?
}

fn build_dag_blocking(file_args: &[String], python: &PathBuf) -> Result<Dag, BarcaError> {
    let paths: Vec<PathBuf> = file_args.iter().map(PathBuf::from).collect();
    let mut all_nodes = Vec::new();
    let mut file_sources: HashMap<String, String> = HashMap::new();
    // Dotted module names in `file_sources` that are `__init__.py` packages,
    // as opposed to regular submodules — needed to resolve relative imports.
    let mut packages: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in &paths {
        let source = fs::read_to_string(path)
            .map_err(|e| BarcaError::Other(format!("{}: {e}", path.display())))?;
        let file_str = path.to_string_lossy().to_string();
        let nodes =
            extract_nodes(&source, &file_str).map_err(|e| BarcaError::Parse(e.to_string()))?;
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        file_sources.insert(stem, source.clone());
        if let Some(parent) = path.parent() {
            // Scan subdirectories FIRST — packages (__init__.py) take precedence
            // over same-named sibling .py files, matching Python's import semantics.
            scan_subdirectories(parent, parent, &mut file_sources, &mut packages);
            // Then scan sibling .py files (flat) — or_insert_with is a no-op if
            // a package with the same name was already registered above.
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let ep = entry.path();
                    if ep.extension().map(|e| e == "py").unwrap_or(false) && ep != *path {
                        let estem = ep
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            file_sources.entry(estem)
                            && let Ok(content) = fs::read_to_string(&ep)
                        {
                            e.insert(content);
                        }
                    }
                }
            }
        }
        all_nodes.extend(nodes);
    }

    // Parse module definitions once per source file, then compute cone hashes.
    // This avoids re-parsing the same file for every node (O(n) parses instead of O(n²)).
    let mut cached_defs: HashMap<String, HashMap<String, crate::cone::ModuleDef>> = HashMap::new();
    for (key, src) in &file_sources {
        cached_defs.insert(key.clone(), crate::cone::collect_module_definitions(src));
    }

    for node in &mut all_nodes {
        let stem_from_path = node
            .source_file
            .rsplit('/')
            .next()
            .unwrap_or("")
            .replace(".py", "");
        let defs = cached_defs.get(&stem_from_path).or_else(|| {
            let stem = PathBuf::from(&node.source_file);
            let s = stem.file_stem()?.to_str()?;
            cached_defs.get(s)
        });
        if let Some(defs) = defs {
            node.cone_hash = crate::cone::cone_hash_from_defs(
                defs,
                &node.function_name,
                &file_sources,
                &packages,
            );
        }
    }

    // Free source text memory before execution starts.
    drop(file_sources);
    drop(packages);

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

fn resolve_dynamic_partitions(nodes: &mut [crate::model::ExtractedNode], python: &PathBuf) {
    for node in nodes.iter_mut() {
        let mut resolved: Vec<(String, Vec<crate::model::PartitionValue>)> = Vec::new();

        for (dim, spec) in &node.partitions {
            if let crate::model::PartitionSpec::Dynamic { source_text } = spec {
                let module_path = std::path::Path::new(&node.source_file)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(&node.source_file));
                let script = "import json, importlib.util, sys\n\
                     _spec = importlib.util.spec_from_file_location('_m', sys.argv[1])\n\
                     _mod = importlib.util.module_from_spec(_spec)\n\
                     _spec.loader.exec_module(_mod)\n\
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
                        "Warning: failed to evaluate partition expression '{}' for {}: {}",
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
mod store_tests {
    use super::*;
    use crate::transfer::ArtifactLayout;

    fn oref(path: &str) -> dispatch::OutputRef {
        dispatch::OutputRef {
            path: path.to_string(),
            format: "json".to_string(),
            size_bytes: 3,
            elapsed_seconds: None,
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
    fn row_outside_the_store_and_not_on_disk_is_a_miss() {
        let layout = ArtifactLayout::new("/w/a", "s3://b/p");
        // e.g. recorded against a different store, or another machine's local path
        assert!(matches!(
            resolve_cache_hit(oref("s3://other/p/n/h.json"), Some(&layout)),
            CacheHit::Miss
        ));
        assert!(matches!(
            resolve_cache_hit(oref("/elsewhere/n/h.json"), Some(&layout)),
            CacheHit::Miss
        ));
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
