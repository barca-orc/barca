//! Run preparation, phase dispatch, and finalization.
use crate::BarcaError;
use crate::cache::{
    BLOCKED_ARTIFACT_PATH, CachePolicy, DecideState, Decision, RunReason, decide_step,
    has_sensor_output, in_dependency_order, localize_decision, sensor_inputs,
};
use crate::dag::Dag;
use crate::db;
use crate::db::materializations::NewMaterialization;
use crate::dispatch;
use crate::dispatch::OutputRef;
use crate::helper_proc;
use crate::planner::{ExecutionPlan, Phase, ResourceConfig};
use crate::recover::{self, Work};
use crate::report::{
    RunOutcome, batch_line, cached_step_line, end_of_run_line, env_suffix, failed_step_line,
    fmt_eta, kind_str, merge_partition_reports, reconcile_total, report_for, stale_warning,
};
use crate::step_status::StepStatus;
use crate::targets::{
    blocking_failure, final_output_of, plan_for_targets, resolve_targets, short_name,
    skipped_tasks_note, target_outcomes, validate_refresh_names,
};
use crate::transfer::{ArtifactLayout, TransferClient};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::load::build_dag;
use crate::results::{
    ExplainResult, ExplainSummary, GetResult, MultiResult, StepReport, TargetOutcome,
    TargetPrediction,
};

use crate::persist::{
    BackupCheckpoints, RunLedger, StepRecorder, cancel_recorded_run, canonical_job, persist_run,
    telemetry_report,
};
use crate::store_sync::{StoreSync, transfer_remedy};

macro_rules! trace_point {
    ($trace:expr, $($arg:tt)*) => {
        if $trace.enabled {
            crate::errln!("[trace] {:>8.1}ms  {}  [db opens {}, cpu {:.1}ms]", $trace.start.elapsed().as_secs_f64() * 1000.0, format_args!($($arg)*), db::opens(), trace_cpu_ms());
        }
    };
}

mod blocked;
mod bookkeeping;
mod finalize;
mod phase;
mod predict;
mod prepare;
mod republish;
mod select;
mod workers;
use bookkeeping::{LedgerContext, RunEvidence, RunResults};
use finalize::*;
pub(crate) use phase::note;
use phase::*;
pub(crate) use predict::{explain_dag, explain_dag_with_keys, explain_dag_with_membership};
use prepare::*;
use select::*;
pub(crate) use workers::upload_pending;
use workers::*;

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

/// The cores this process may use (`available_parallelism`), 4 when unknown.
pub(crate) fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// How many workers a run starts at most and the capacity their steps' `weight=` shares, each
/// with where it came from (#436, #628).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    pub pool_size: usize,
    /// `BARCA_POOL_SIZE`, `[execution] pool_size`, `cores`, or the capacity that bounded the
    /// cores (`[execution] capacity`, `BARCA_CAPACITY`, `[server] capacity`, `--capacity`).
    pub pool_source: &'static str,
    pub capacity: usize,
    /// `BARCA_CAPACITY`, `[execution] capacity`, or `pool size`.
    pub capacity_source: &'static str,
}

/// The pool size and capacity of a run.
///
/// Pool: `BARCA_POOL_SIZE` > `[execution] pool_size` > the cores, bounded by the capacity when
/// one is set (`server`, the capacity a `barca serve` shares between its runs, bounds it too).
/// Every step takes a weight of at least 1, so no more than `capacity` steps ever run at once:
/// a worker past that would start, import the project and wait. Weights above 1 lower how
/// many run at once further, but workers start only for steps the capacity admits, so the
/// pool is not sized from them.
///
/// Capacity: `BARCA_CAPACITY` > `[execution] capacity` > the pool size.
pub fn sizing(
    capacity: crate::config::CapacitySetting,
    pool: crate::config::PoolSetting,
    server: Option<(usize, &'static str)>,
) -> Sizing {
    sizing_with(capacity, pool, server, cores())
}

fn sizing_with(
    capacity: crate::config::CapacitySetting,
    pool: crate::config::PoolSetting,
    server: Option<(usize, &'static str)>,
    cores: usize,
) -> Sizing {
    let (pool_size, pool_source) = match pool {
        Some((n, source)) => (n.max(1), source),
        None => {
            let bound = [capacity, server]
                .into_iter()
                .flatten()
                .min_by_key(|(n, _)| *n);
            match bound {
                Some((n, source)) if n < cores => (n.max(1), source),
                _ => (cores.max(1), "cores"),
            }
        }
    };
    let (capacity, capacity_source) = capacity.unwrap_or((pool_size, "pool size"));
    Sizing {
        pool_size,
        pool_source,
        capacity: capacity.max(1),
        capacity_source,
    }
}

/// [`sizing`] for a run with this configuration, under the capacity its host shares between
/// its runs, when `barca serve` shares one.
pub(crate) fn sizing_for(
    cfg: &crate::config::ResolvedConfig,
    host: &dyn crate::run_host::RunHost,
) -> Sizing {
    sizing(
        cfg.capacity,
        cfg.pool_size,
        host.shared_capacity().map(|s| (s.total as usize, s.source)),
    )
}

/// The planned nodes whose `weight=` is above 1, by node id.
pub(crate) fn planned_weights(plan: &ExecutionPlan) -> std::collections::BTreeMap<String, u32> {
    plan.phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .filter(|st| st.weight > 1)
        .map(|st| (st.step_id.base_id().to_string(), st.weight))
        .collect()
}

/// Warn, once per node, about each weight above the capacity: such a step runs alone, with
/// nothing beside it, rather than never.
pub(crate) fn warn_over_capacity(
    weights: &std::collections::BTreeMap<String, u32>,
    capacity: usize,
    source: &str,
) {
    for (node, weight) in weights {
        if *weight as usize > capacity {
            crate::run_line!(
                Always,
                "[barca] warning: '{}' has weight={weight}, more than the capacity {capacity} \
                 ({source}); it runs alone",
                short_name(node)
            );
        }
    }
}

/// What `execute` produced: the run, each target's outcome, and the first step failure.
/// Single-target runs turn that failure into an `Err`; multi-target runs report it per target.
pub(crate) struct Executed {
    result: GetResult,
    targets: Vec<(String, TargetOutcome)>,
    step_failure: Option<crate::FailedStep>,
}

impl Executed {
    /// A step failure ends a single-target run: exit 1, with the node, its traceback, and what
    /// the run did before it stopped (the failed step's status is `failed`).
    pub(crate) fn into_single(self) -> Result<GetResult, BarcaError> {
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
                    partitions: r.partitions,
                    key_summary: r.key_summary,
                }));
                Err(BarcaError::WorkerFailed(Box::new(failed)))
            }
            None => Ok(self.result),
        }
    }

    pub(crate) fn into_multi(self) -> MultiResult {
        let r = self.result;
        MultiResult {
            run_id: r.run_id,
            elapsed_seconds: r.elapsed_seconds,
            steps_executed: r.steps_executed,
            phases: r.phases,
            steps: r.steps,
            warnings: r.warnings,
            targets: self.targets,
            cache: r.cache,
            uploads: r.uploads,
            metadata_seconds: r.metadata_seconds,
            partition_selection: r.partition_selection,
        }
    }
}

pub(crate) struct ExecuteRequest<'a> {
    pub(crate) dag: Option<std::sync::Arc<Dag>>,
    pub(crate) cfg: &'a crate::config::ResolvedConfig,
    pub(crate) target_names: &'a [String],
    pub(crate) file_args: &'a [String],
    pub(crate) python: &'a std::path::Path,
    pub(crate) agent_mode: bool,
    pub(crate) policy: CachePolicy,
    pub(crate) command_label: &'a str,
    pub(crate) interrupt: crate::interrupt::Interrupt,
    pub(crate) event_tx: Option<UnboundedSender<crate::RunEvent>>,
    /// `--partition`: the keys every narrowed node of the cone computes (all keys when `None`).
    pub(crate) partition: Option<crate::selection::PartitionSelection>,
    /// The run id to use and what started the run.
    pub(crate) options: crate::outcome::RunOptions,
    /// Freshness rules (RFC-0008): whether `Manual` upstreams are read as they are, and
    /// whether this is a scheduled tick.
    pub(crate) rules: crate::freshness::RunRules,
}

struct Trace {
    start: Instant,
    enabled: bool,
}
/// Flat PID-owned staging, shared with Python's dead-owner cleanup. One prefix
/// per run permits concurrent server runs; no live run's files are swept.
struct UploadSnapshots {
    prefix: std::path::PathBuf,
}
impl UploadSnapshots {
    fn new(run_id: &str) -> Result<Self, BarcaError> {
        let prefix = crate::transfer::upload_snapshot_prefix(run_id)?;
        std::fs::create_dir_all(prefix.parent().expect("snapshot prefix has a parent"))
            .map_err(|e| BarcaError::Other(format!("create upload staging: {e}")))?;
        Ok(Self { prefix })
    }
}
impl Drop for UploadSnapshots {
    fn drop(&mut self) {
        let Some(parent) = self.prefix.parent() else {
            return;
        };
        let Some(prefix) = self.prefix.file_name() else {
            return;
        };
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .trim_start_matches('.')
                    .starts_with(prefix.to_string_lossy().as_ref())
                {
                    std::fs::remove_file(entry.path()).ok();
                }
            }
        }
        // Keep the empty PID directory while this process lives. Removing it
        // races another server run that created its guard but has not written
        // a snapshot yet. Python's existing dead-PID cleanup removes it later.
    }
}

struct RunStart {
    trace: Trace,
    cancel: CancellationToken,
    run_id: String,
    run_started: std::time::SystemTime,
    telemetry: Vec<(String, Box<dyn crate::telemetry::Integration>)>,
    transfer_start: Option<crate::transfer::Launching>,
    /// The artifact store was checked less than five minutes ago and this run does not
    /// refresh: its transfer helper is started only if the run transfers something (#593).
    store_checked: bool,
    upload_snapshots: Option<UploadSnapshots>,
}
impl RunStart {
    fn begin(request: &ExecuteRequest<'_>) -> Result<Self, BarcaError> {
        let cancel = request.interrupt.cancel.clone();
        let trace = Trace {
            start: Instant::now(),
            enabled: std::env::var("BARCA_TRACE_TIMING").is_ok(),
        };
        let run_id = request
            .options
            .run_id
            .clone()
            .unwrap_or_else(db::generate_run_id);
        let run_started = std::time::SystemTime::now();
        let telemetry = crate::telemetry::configured();
        let cfg = request.cfg;
        let python = request.python;
        // Start remote I/O first so it overlaps parsing and planning: the artifact transfer
        // helper's startup (joined before workers start). Nothing is pulled: this machine's
        // database is the truth for this machine (RFC-0009).
        // Not a task on this stack frame: the helper connects on its own while this function
        // goes on, and the value stops it if this function returns before using it (see
        // `transfer::Launching`, which accepts the hello in a background task).
        let upload_snapshots = cfg
            .remote_artifacts()
            .then(|| UploadSnapshots::new(&run_id))
            .transpose()?;
        // A store checked in the last five minutes is not checked again by a run that does not
        // refresh (#593); `start_workers` still checks it when uploads are pending.
        let store_checked = cfg.remote_artifacts()
            && matches!(request.policy, CachePolicy::CacheAware)
            && !cfg.check_store
            && crate::gc::store_checked_recently(cfg);
        let transfer_start = match cfg.remote_artifacts() && !store_checked {
            true => Some(TransferClient::launch(python, cfg, &run_id)?),
            false => None,
        };

        Ok(Self {
            trace,
            cancel,
            run_id,
            run_started,
            telemetry,
            transfer_start,
            store_checked,
            upload_snapshots,
        })
    }
}
#[derive(Default)]
struct RunState {
    results: RunResults,
    incremental: Option<crate::incremental::Incremental>,
    cached_node_ids: std::collections::HashSet<String>,
    decide_state: DecideState,
    phase_error: Option<String>,
    usage_error: Option<String>,
    step_failure: Option<(String, String)>,
    /// Where `step_failure` failed, when its worker said so.
    step_failure_phase: Option<crate::dispatch::ErrorPhase>,
    failed_bases: std::collections::HashSet<String>,
    skipped_bases: std::collections::HashSet<String>,
    /// The keys whose step failed, by base (`PartitionKey::suffix`), and whether a step failed
    /// whole (a run with one target stops at that; a failed key stops only what reads it, #764).
    failed_keys: blocked::Keys,
    whole_failure: bool,
    /// The keys not computed because the same key of an input failed or was blocked, by base.
    blocked_keys: blocked::Keys,
    logs_buffer: crate::run_logs::Recorder,
    steps_executed: usize,
    total_steps: usize,
    total_estimated: f64,
    elapsed_so_far: f64,
    completed_steps: usize,
    store_paths: HashMap<String, String>,
    transfer_error: Option<String>,
    /// What this run left unpublished in the artifact store (#390), for the run JSON.
    uploads: Option<crate::results::UploadsReport>,
    /// Coordinator time recording results, when the run made a batched call, for the run JSON.
    metadata_seconds: Option<f64>,
    /// Targets whose copy here and copy in the store differed, re-uploaded from here without
    /// a step running (RFC-0009 `mismatch`); each gets the changed-bytes warning.
    repaired_mismatches: Vec<String>,
    cached_steps: recover::CachedSteps,
    held_cached_lines: Vec<String>,
    /// The results this run holds while it is in flight (the cache size limit leaves them).
    pin: Option<crate::gc::Pin>,
    /// Artifact paths (as recorded) handed to steps as inputs: `last_read_at` moves for them.
    read_paths: std::collections::HashSet<String>,
    /// `--partition`: the plan's narrowing, with the counts of keys known once a source ran.
    narrowing: Option<crate::selection::Narrowing>,
    /// Every key of each partitioned asset with a dataset sink, for deleting removed keys.
    dataset_keys: HashMap<String, crate::dataset_sinks::DatasetKeys>,
    /// The next run-local batched call number (RFC-0013).
    next_call: u64,
    /// A cache hit's sink could not be republished: the step failed (`republish`).
    sink_failed: bool,
}
// Fields drop in the same order as the original run locals: recorder, workers, store.
struct RunSession<'a, 'r> {
    request: &'a ExecuteRequest<'r>,
    prepared: &'a Prepared,
    state: RunState,
    recorder: Option<StepRecorder>,
    pool: Option<crate::io_loop::WorkerPool>,
    store: Option<StoreSync>,
    pb: Option<indicatif::ProgressBar>,
    cost_model: crate::cost::CostModel,
    started: RunStart,
}

pub(crate) async fn execute(request: ExecuteRequest<'_>) -> Result<Executed, BarcaError> {
    let lines = crate::lines::Lines::new(request.event_tx.clone());
    // Boxed: the run's future is large, and a debug build would copy it through the stack.
    lines.scope(Box::pin(execute_run(request))).await
}
async fn execute_run(request: ExecuteRequest<'_>) -> Result<Executed, BarcaError> {
    let started = RunStart::begin(&request)?;
    // Lines written from here on belong to this run (`BARCA_LOG=json` under serve). Boxed: the
    // run's state machine is large, and wrapping it inline in the task-local scope doubled the
    // future a serve run task carries, which overflowed a worker thread's stack in debug builds.
    let run_id = started.run_id.clone();
    crate::jsonlog::in_run(run_id, Box::pin(execute_started(request, started))).await
}

async fn execute_started(
    mut request: ExecuteRequest<'_>,
    started: RunStart,
) -> Result<Executed, BarcaError> {
    // Held from before anything is decided: until the plan is known, eviction in this process
    // keeps to leftovers and superseded results.
    let pin = crate::gc::pin(&started.run_id);
    let prepared = prepare_run(request.dag.take(), &request, |point| {
        trace_point!(started.trace, "{point}")
    })
    .await?;
    // A tick whose every target reads a `Manual` node that has never been computed: nothing
    // to run, and not an error of the tick.
    if prepared.targets.is_empty() && !prepared.blocked.is_empty() {
        return Ok(Executed {
            result: GetResult {
                run_id: started.run_id.clone(),
                elapsed_seconds: 0.0,
                steps_executed: 0,
                phases: 0,
                final_output: None,
                steps: blocked_steps(&prepared.dag, &prepared.blocked, &prepared.paused),
                warnings: Vec::new(),
                partitions: None,
                key_summary: None,
                cache: None,
                uploads: None,
                metadata_seconds: None,
                partition_selection: None,
            },
            targets: prepared.blocked,
            step_failure: None,
        });
    }
    pin.nodes(
        prepared
            .exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|st| st.step_id.base_id().to_string()),
    );
    // With a server, its ledger answers the cache lookups (RFC-0007 section 4.1): check the
    // server, post what this machine could not post before, then ask once for every run hash
    // known before anything runs.
    if let Some(ledger) = &request.cfg.ledger {
        ledger.prepare(request.cfg, true).await?;
        ledger
            .prefetch(crate::cache::cone_keys(
                &prepared.dag,
                &prepared.exec_plan.phases,
            ))
            .await?;
        trace_point!(started.trace, "ledger_lookup");
    }
    // `--batch NODE=V` names nodes that declare batch= (RFC-0013), checked before the run starts.
    request
        .options
        .batch
        .check(&prepared.dag)
        .map_err(BarcaError::Usage)?;
    let mut session = open_run(&request, &prepared, started).await?;
    session.state.pin = Some(pin);
    drive_phases(&mut session).await?;
    trace_point!(
        session.started.trace,
        "incremental identities {}",
        session.state.decide_state.run_hashes().len()
    );
    prune_dataset_sinks(&mut session).await;
    note_stale_pins(&mut session).await?;
    // What the run read (its inputs, and the outputs it returns) gets its `last_read_at` before
    // the run is recorded, so that the run's backup carries it and nothing is written locally
    // after it (RFC-0012).
    let returned: Vec<String> = {
        let ids: Vec<&str> = prepared.targets.iter().map(|(_, id)| id.as_str()).collect();
        let requested = recover::requested(&prepared.exec_plan, &ids);
        session
            .state
            .results
            .outputs
            .iter()
            .filter(|(id, _)| requested.contains(recover::base_of(id)))
            .map(|(_, o)| o.path.clone())
            .collect()
    };
    session.state.read_paths.extend(returned);
    // A result read within the last hour keeps its stamp (`cache::READ_STAMP_REFRESH`).
    let recent = &session.state.decide_state.recently_read;
    let mut read: Vec<String> = session
        .state
        .read_paths
        .iter()
        .filter(|p| !recent.contains(*p))
        .cloned()
        .collect();
    read.sort();
    // Their `last_read_at` is written with the run's ledger, on its connection.
    let elapsed = record_and_publish(&mut session, &read).await?;
    if let Some(error) = session.state.usage_error.take() {
        return Err(BarcaError::Usage(error));
    }
    let mut executed = return_outputs(&mut session, elapsed).await?;
    if session.state.phase_error.is_none()
        && session.state.step_failure.is_none()
        && session.state.transfer_error.is_none()
        && !session.started.cancel.is_cancelled()
        && executed.step_failure.is_none()
        && let Some(incremental) = &session.state.incremental
    {
        incremental
            .save(
                request.cfg,
                &executed.result.run_id,
                executed.result.steps_executed > 0,
                session.state.decide_state.run_hashes(),
                &session.state.cached_node_ids,
            )
            .await?;
    }
    let own = cache_check_inputs(&session, &executed);
    let pin = session.state.pin.take();
    let cfg = request.cfg;
    drop(session);
    // The cache size limit after a run: evict if `.barca/` is over its limit. What the run
    // produced, read or returns is never evicted.
    executed.result.cache = crate::gc::after_command(cfg, &own, pin.as_ref()).await;
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        crate::errln!("[trace] metadata db opens {}", db::opens());
        trace_resources();
    }
    Ok(executed)
}

#[cfg(unix)]
fn trace_cpu_ms() -> f64 {
    // SAFETY: getrusage writes only this initialized output struct.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0.;
    }
    (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.
}
#[cfg(not(unix))]
fn trace_cpu_ms() -> f64 {
    0.
}

#[cfg(unix)]
fn trace_resources() {
    // SAFETY: rusage is a plain C output struct and the pointer stays valid for the call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return;
    }
    let cpu_ms = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.;
    #[cfg(target_os = "macos")]
    let peak_kib = usage.ru_maxrss / 1024;
    #[cfg(not(target_os = "macos"))]
    let peak_kib = usage.ru_maxrss;
    // Linux rusage can retain a spawning interpreter's pre-exec high-water mark.
    // VmHWM belongs to this executable's address space alone.
    #[cfg(target_os = "linux")]
    let peak_kib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse::<i64>().ok())
        })
        .unwrap_or(peak_kib);
    crate::errln!("[trace] coordinator resources peak_kib={peak_kib} cpu_ms={cpu_ms:.3}");
}
#[cfg(not(unix))]
fn trace_resources() {}

/// Delete the keys that assets with a dataset sink no longer have (`crate::dataset_sinks`).
/// Not after a cancelled or broken run: its key sets may be incomplete.
async fn prune_dataset_sinks(session: &mut RunSession<'_, '_>) {
    let datasets = std::mem::take(&mut session.state.dataset_keys);
    if datasets.is_empty()
        || session.started.cancel.is_cancelled()
        || session.state.phase_error.is_some()
        || session.state.usage_error.is_some()
        || session.state.transfer_error.is_some()
    {
        return;
    }
    let lines = crate::dataset_sinks::prune(
        datasets,
        session.request.python,
        session.request.cfg.storage_options_json.as_deref(),
    )
    .await;
    for line in lines {
        note(&session.pb, &line);
    }
}

/// What a run protects from the cache size limit: everything it produced, read or returns (as
/// recorded paths), its run hashes, and the `Manual` nodes of its DAG.
fn cache_check_inputs(session: &RunSession<'_, '_>, executed: &Executed) -> crate::gc::Own {
    // Every path `get` prints or returns, including each partition key of a partitioned target.
    let returned = executed
        .result
        .final_output
        .iter()
        .chain(executed.result.partitions.iter().flatten().map(|(_, o)| o))
        .chain(executed.targets.iter().flat_map(|(_, t)| {
            t.final_output
                .iter()
                .chain(t.partitions.iter().flatten().map(|(_, o)| o))
        }))
        .map(|o| o.path.clone());
    let mut read: Vec<String> = session
        .state
        .read_paths
        .iter()
        .cloned()
        .chain(returned)
        .collect();
    read.sort();
    read.dedup();
    crate::gc::Own {
        nodes: session
            .state
            .incremental
            .as_ref()
            .map(|i| i.cached.keys().cloned().collect())
            .unwrap_or_default(),
        recorded: read
            .into_iter()
            .chain(
                session
                    .state
                    .results
                    .outputs
                    .values()
                    .map(|o| o.path.clone()),
            )
            .chain(session.state.store_paths.values().cloned())
            .collect(),
        run_hashes: session
            .state
            .decide_state
            .run_hashes()
            .values()
            .cloned()
            .collect(),
        manual: session
            .prepared
            .dag
            .graph
            .node_weights()
            .filter(|n| n.extracted.freshness == crate::model::Freshness::Manual)
            .map(|n| n.id.clone())
            .collect(),
        wrote_nothing: session.state.steps_executed == 0 && session.store.is_none(),
    }
}
/// After the steps ran: say which `Manual` nodes this run read as they are although they are
/// stale, now that the sensors it polled are known (not under a tick, which reads them
/// silently). The step report of each gets the stale wording too.
async fn note_stale_pins(session: &mut RunSession<'_, '_>) -> Result<(), BarcaError> {
    let request = session.request;
    if session.prepared.pinned.is_empty() || request.rules.tick.is_some() {
        return Ok(());
    }
    let cache = crate::cache::CacheReader::for_config(request.cfg).await?;
    let dag = &session.prepared.dag;
    let state = &session.state.decide_state;
    let mut found: Vec<(&str, bool)> = Vec::new();
    for id in dag.topo_order() {
        let Some(pin) = session.prepared.pinned.get(id) else {
            continue;
        };
        if pin.user.is_some() {
            continue;
        }
        let stale = crate::freshness::is_stale(
            &cache,
            dag,
            id,
            &pin.run_hash,
            state.run_hashes(),
            &state.sensor_outputs,
        )
        .await;
        found.push((id, stale));
    }
    drop(cache);
    for (id, stale) in found {
        if stale {
            note(
                &session.pb,
                &crate::freshness::stale_note(dag, id, request.file_args),
            );
        }
        if let Some(report) = session
            .state
            .results
            .reports
            .iter_mut()
            .find(|r| r.id == id)
        {
            report.detail = Some(crate::cache::Held::Manual { stale }.detail().to_string());
        }
    }
    Ok(())
}

/// Average step times by node id, for the ETA.
type AvgTimes = HashMap<String, f64>;

async fn open_state(
    request: &ExecuteRequest<'_>,
    prepared: &Prepared,
    started: &mut RunStart,
) -> Result<(crate::cost::CostModel, AvgTimes, AvgTimes), BarcaError> {
    let cfg = request.cfg;
    let run_id = &started.run_id;
    let command_label = request.command_label;
    let file_args = request.file_args;
    let target_label = &prepared.target_label;
    let exec_plan = &prepared.exec_plan;
    crate::layout::ensure_env_dirs(&cfg.env)?;
    let db_path = cfg.db_path.clone();

    // One visit to the database: initialize it, record the run, and read the cost estimates
    // and the average step times the progress bar's ETA is made of.
    let (node_ids, base_ids) = eta_ids(exec_plan);
    let definitions = crate::changes::definition_rows(
        &prepared.dag,
        exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|st| st.step_id.base_id()),
    );
    let db::StartedRun {
        cost_estimates,
        avg_elapsed,
        partitioned_avg_elapsed,
    } = db::start_run(
        &db_path,
        db::NewRun {
            run_id,
            command: command_label,
            files: &db::encode_files(file_args),
            target: target_label.as_deref(),
            steps_total: Some(exec_plan.total_steps),
            job: Some(&prepared.job_name),
            trigger: &request.options.trigger,
            // The run's steps are traced with the integration `execution::workers` starts for
            // the same `BARCA_TELEMETRY`.
            traced: crate::telemetry::is_on("datadog"),
        },
        &node_ids,
        &base_ids,
        &definitions,
    )
    .await?;
    if let Some(tx) = &request.event_tx {
        let _ = tx.send(crate::RunEvent::RunStarted {
            run_id: run_id.clone(),
        });
    }
    trace_point!(
        started.trace,
        "db_run_started ({} unpartitioned, {} partitioned base ids)",
        node_ids.len(),
        base_ids.len()
    );

    // Measured-cost model: seed from persisted estimates so batch sizing is
    // pre-warmed — the cold-start probe is paid once ever per stable node,
    // not once per run.
    let mut cost_model = crate::cost::CostModel::new();
    cost_model.seed(cost_estimates);

    Ok((cost_model, avg_elapsed, partitioned_avg_elapsed))
}

/// The ids whose average step time the ETA needs: the unpartitioned nodes (exact lookup), and
/// the base ids of partitioned ones (a `LIKE` lookup).
fn eta_ids(exec_plan: &ExecutionPlan) -> (Vec<String>, Vec<String>) {
    let steps = || {
        exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
    };
    let unpartitioned = steps()
        .filter(|st| st.partition_keys.is_empty())
        .map(|st| st.step_id.display())
        .collect();
    let partitioned = steps()
        .filter(|st| !st.partition_keys.is_empty())
        .map(|st| st.step_id.base_id().to_string())
        .collect();
    (unpartitioned, partitioned)
}
fn prepare_progress(
    exec_plan: &ExecutionPlan,
    avg_times: &AvgTimes,
    partitioned_avg_times: &AvgTimes,
    agent_mode: bool,
) -> (Option<indicatif::ProgressBar>, f64) {
    // Progress bar setup. `total_steps` starts as the plan-time estimate and
    // grows as dynamic (`partitions_from`) phases expand at dispatch time —
    // see the `phase_step_count` reconciliation below.
    let total_steps = exec_plan.total_steps;
    let (unpartitioned_node_ids, _) = eta_ids(exec_plan);
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
        // Every stderr line from here on (the store check, a fetch, a warning) is written above
        // the bar, not into it.
        crate::term::above_progress_bar(&bar);
        Some(bar)
    } else {
        None
    };

    (pb, total_estimated)
}
async fn open_run<'a, 'r>(
    request: &'a ExecuteRequest<'r>,
    prepared: &'a Prepared,
    mut started: RunStart,
) -> Result<RunSession<'a, 'r>, BarcaError> {
    if let Some(wait) = crate::test_hooks::before_start_workers() {
        tokio::time::sleep(wait).await;
    }
    let (cost_model, avg_times, partitioned_avg_times) =
        open_state(request, prepared, &mut started).await?;
    let (pb, total_estimated) = prepare_progress(
        &prepared.exec_plan,
        &avg_times,
        &partitioned_avg_times,
        request.agent_mode,
    );
    let (store, pool, recorder) = match start_workers(request, prepared, &mut started, &pb).await {
        Ok(workers) => workers,
        Err(error) => {
            if let Some(bar) = &pb {
                bar.finish_and_clear();
            }
            let status = if matches!(error, BarcaError::Cancelled) {
                "cancelled"
            } else {
                "failed"
            };
            let line = match crate::interrupt::cancel_reason() {
                Some(reason) if matches!(error, BarcaError::Cancelled) => {
                    format!("[barca] run cancelled: {reason}")
                }
                _ => format!("[barca] {error}"),
            };
            db::insert_logs(
                &request.cfg.db_path,
                &started.run_id,
                &[crate::run_logs::RecordedLine::barca(line)],
            )
            .await?;
            db::finish_run(
                &request.cfg.db_path,
                &started.run_id,
                status,
                0,
                0,
                started.trace.start.elapsed().as_secs_f64(),
            )
            .await?;
            return Err(error);
        }
    };
    let mut state = RunState {
        total_steps: prepared.exec_plan.total_steps,
        total_estimated,
        narrowing: prepared.narrowing.clone(),
        ..Default::default()
    };
    // Lines are spooled as they arrive, so `barca logs -f` can read a run still in progress.
    state.logs_buffer.spool_to(crate::run_logs::spool_path(
        &request.cfg.db_path,
        &started.run_id,
    ));
    state.incremental = crate::incremental::Incremental::open(
        &prepared.dag,
        &prepared.exec_plan,
        request.cfg,
        &request.policy,
        request.partition.is_some(),
    )
    .await?;
    state.decide_state.pinned = prepared.pinned.clone();
    state.decide_state.tick = request.rules.tick.clone();
    Ok(RunSession {
        request,
        prepared,
        started,
        state,
        cost_model,
        pb,
        store,
        pool: Some(pool),
        recorder: Some(recorder),
    })
}
async fn dispatch_ready(
    session: &mut RunSession<'_, '_>,
    filtered_phase: &Phase,
    provided: &HashMap<String, dispatch::ProvidedInput>,
    phase_idx: usize,
    calls: Vec<(String, Vec<String>)>,
) -> crate::coordinator::Coordinator {
    session.state.steps_executed += phase_step_count(filtered_phase);

    let (coord, phase_err, phase_elapsed, phase_completed, phase_total) =
        dispatch_phase(DispatchPhase {
            filtered_phase,
            provided,
            calls,
            pool: session.pool.as_mut().expect("workers started"),
            cost_model: &mut session.cost_model,
            decide_state: &session.state.decide_state,
            store: &mut session.store,
            recorder: session.recorder.as_ref().expect("recorder started"),
            run_id: &session.started.run_id,
            store_paths: &mut session.state.store_paths,
            dag: &session.prepared.dag,
            pb: &session.pb,
            agent_mode: session.request.agent_mode,
            total_estimated: session.state.total_estimated,
            elapsed_so_far: session.state.elapsed_so_far,
            completed_steps: session.state.completed_steps,
            total_steps: session.state.total_steps,
            event_tx: &session.request.event_tx,
            logs_buffer: &mut session.state.logs_buffer,
            step_durations: &mut session.state.results.durations,
            cancel: &session.started.cancel,
        })
        .await;
    session.state.elapsed_so_far = phase_elapsed;
    session.state.completed_steps = phase_completed;
    session.state.total_steps = phase_total;
    trace_point!(session.started.trace, "phase{phase_idx}_run_phase_done");
    if let Err(e) = phase_err
        && session.state.phase_error.is_none()
    {
        session.state.phase_error = Some(e);
    }

    coord
}
/// Steps another run of the server finished while this run waited for them (#654): cache
/// hits here. The owner recorded them; this run records no row and uploads nothing.
fn computed_by_other_runs(
    session: &mut RunSession<'_, '_>,
    phase_outputs: &HashMap<String, OutputRef>,
) {
    let Some(pool) = session.pool.as_mut() else {
        return;
    };
    for node_id in pool.take_from_other_runs() {
        session.state.steps_executed = session.state.steps_executed.saturating_sub(1);
        // Not executed here: no attempts of this run's.
        session.state.results.attempts.remove(&node_id);
        crate::report::computed_by_another_run(
            &mut session.state.results.reports,
            &node_id,
            phase_outputs.get(&node_id).map(|o| o.path.as_str()),
        );
        if session.request.agent_mode {
            crate::run_line!(
                Agent,
                "{}",
                cached_step_line(&session.prepared.dag, &node_id)
            );
        }
        session.state.cached_node_ids.insert(node_id);
    }
}
fn collect_outputs(
    session: &mut RunSession<'_, '_>,
    coord: &crate::coordinator::Coordinator,
) -> HashMap<String, OutputRef> {
    // Collect results from coordinator
    let mut phase_outputs: HashMap<String, dispatch::OutputRef> = HashMap::new();
    for (&item_id, artifact_val) in coord.outputs() {
        let item = coord.item(item_id);
        let node_id = item.step_id.display();
        let oref = session
            .state
            .results
            .record_output(&node_id, artifact_val, item.attempts);
        // A sensor's or `Manual` asset's output hash: its consumers, decided in a later
        // phase, fold it into their run hashes (#183, #548).
        if crate::cache::folds_output(&session.prepared.dag, item.step_id.base_id())
            && session
                .state
                .decide_state
                .run_hashes()
                .contains_key(&node_id)
            && let Some(h) = artifact_val.get("content_hash").and_then(|v| v.as_str())
        {
            let revision = crate::observation::from_artifact(
                artifact_val,
                session.state.decide_state.sensor_outputs.get(&node_id),
                &session.started.run_id,
                &node_id,
            )
            .unwrap_or_else(|| h.to_owned());
            session
                .state
                .decide_state
                .sensor_outputs
                .insert(node_id.clone(), revision);
        }
        phase_outputs.insert(node_id, oref);
    }

    phase_outputs
}
async fn collect_failures(
    session: &mut RunSession<'_, '_>,
    coord: &crate::coordinator::Coordinator,
) {
    // Collect failures — parallel branch failures (group members) are
    // contained within the group and surfaced as ParallelError to the parent,
    // so they should NOT abort the entire phase.
    //
    // A Ctrl-C in a terminal reaches the workers as well as this process, and a worker
    // may report its step's KeyboardInterrupt before the cancellation is seen here. That
    // step was interrupted, not failed: it gets no `failed` line and no failed row. When
    // such a report came first, give this process's own signal a moment to arrive.
    let interrupted = |message: &str| message.starts_with("KeyboardInterrupt");
    if !session.started.cancel.is_cancelled()
        && coord.failed_items().iter().any(|(_, m)| interrupted(m))
    {
        let grace = std::time::Duration::from_millis(250);
        let _ = tokio::time::timeout(grace, session.started.cancel.cancelled()).await;
    }
    let cancelled = session.started.cancel.is_cancelled();
    let mut first_non_group_failure: Option<(String, String)> = None;
    let mut first_failure_phase = None;
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
        session
            .state
            .results
            .attempts
            .insert(node_id.clone(), item.attempts);
        if error_msg.starts_with(BLOCKED_ARTIFACT_PATH) {
            // The step ran; its result could not be written because barca's artifact
            // directory is in a state barca cannot repair. Infrastructure (exit 3), not a
            // failed step: there is nothing in the step to fix.
            let why = error_msg
                .strip_prefix(BLOCKED_ARTIFACT_PATH)
                .map_or(error_msg, |m| m.trim_start_matches([':', ' ']));
            session.state.transfer_error.get_or_insert(format!(
                "the result of {node_id} could not be written: {why}"
            ));
            session
                .state
                .failed_bases
                .insert(item.step_id.base_id().to_string());
            session.state.whole_failure = true;
            continue;
        }
        if session.request.agent_mode {
            crate::run_line!(
                Agent,
                "{}{}",
                failed_step_line(&node_id, error_msg),
                if item.attempts > 1 {
                    format!(" attempts={}", item.attempts)
                } else {
                    String::new()
                }
            );
        }
        let phase = coord.failure_phase(item_id);
        if first_non_group_failure.is_none() {
            first_non_group_failure = Some((node_id.clone(), error_msg.to_string()));
            first_failure_phase = phase;
        }
        session
            .state
            .failed_bases
            .insert(item.step_id.base_id().to_string());
        if item.step_id.partition.is_empty() {
            session.state.whole_failure = true;
        } else {
            session
                .state
                .failed_keys
                .entry(item.step_id.base_id().to_string())
                .or_default()
                .insert(item.step_id.partition.suffix());
        }
        let failure = session.state.results.record_failure(
            node_id,
            "WorkerError",
            error_msg.to_string(),
            item.attempts,
            coord.failure_clock(item_id),
            phase,
        );
        // The failed step's trace goes out now, not at the end of the run.
        if let Some(datadog) = session.pool.as_ref().and_then(|p| p.datadog()) {
            datadog.step_failed(failure);
        }
    }

    // Steps the coordinator skipped because an in-phase dependency failed.
    // They never executed, so they do not count in `steps_executed`. A skipped key is
    // reported on its own (#764); its node is skipped only when none of its keys ran.
    let ran: std::collections::HashSet<&str> = coord
        .outputs()
        .keys()
        .map(|id| coord.item(*id).step_id.base_id())
        .collect();
    let mut keyed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item_id in coord.skipped_items() {
        let item = coord.item(item_id);
        if item.group.is_some() {
            continue;
        }
        session.state.steps_executed = session.state.steps_executed.saturating_sub(1);
        let base = item.step_id.base_id().to_string();
        if item.step_id.partition.is_empty() {
            session.state.skipped_bases.insert(base);
            continue;
        }
        let key = item.step_id.partition.suffix();
        let cause = blocked::cause(&failures(session), &base, &key);
        let report = blocked::report(&session.prepared.dag, &base, &cause);
        session.state.results.reports.push(report);
        session
            .state
            .blocked_keys
            .entry(base.clone())
            .or_default()
            .insert(key);
        keyed.insert(base);
    }
    for base in keyed {
        if !ran.contains(base.as_str()) && !session.state.failed_bases.contains(&base) {
            session.state.skipped_bases.insert(base);
        }
    }

    // Remember the first step failure (it does not overwrite an infrastructure error).
    if let Some(msg) = first_non_group_failure
        && session.state.step_failure.is_none()
    {
        session.state.step_failure = Some(msg);
        session.state.step_failure_phase = first_failure_phase;
    }
}
fn finish_queue(session: &mut RunSession<'_, '_>, queue: &mut VecDeque<Work<'_>>) {
    // Whatever is still queued when the loop ends was never dispatched, whatever ended it (a
    // failed step, a failed recompute, cancellation, an infrastructure error). A step that had
    // been decided to run and was waiting for an input must not be reported as having run: it
    // is skipped, like a step the coordinator never started.
    for work in queue.drain(..) {
        if let Work::Ready(phase) = work {
            for step in phase.streams.iter().flat_map(|s| &s.steps) {
                session
                    .state
                    .skipped_bases
                    .insert(step.step_id.base_id().to_string());
            }
        }
    }
    // A cached step whose `cached` line was held back and that was not computed again after
    // all is announced now.
    for id in std::mem::take(&mut session.state.held_cached_lines) {
        if session.state.cached_node_ids.contains(&id)
            || (session
                .state
                .incremental
                .as_ref()
                .is_some_and(|i| i.unseeded_fresh(&id))
                && !session.state.results.attempts.contains_key(&id))
        {
            crate::run_line!(Agent, "{}", cached_step_line(&session.prepared.dag, &id));
            crate::lines::Lines::current().cached_step(&id);
        }
    }
}
struct PlannedCalls {
    calls: Vec<(String, Vec<String>)>,
    errors: Vec<(String, String)>,
}

struct BatchKeys {
    base: String,
    ordered: Vec<(usize, String)>,
}

/// Group the keys of each batched node in `phase` into calls (RFC-0013): `(call id, the
/// display ids of its keys in key order)`. Only the keys that run are in the phase, so only
/// those are grouped. A node whose grouping cannot be made (a `batches_by` function that
/// raises, `strict=True` with a key in no group) is returned with why, to be failed.
fn plan_calls(
    dag: &Dag,
    over: &crate::batch::BatchOverride,
    phase: &Phase,
    python: &std::path::Path,
    next_call: &mut u64,
) -> PlannedCalls {
    let mut keys_of: Vec<BatchKeys> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for step in phase.streams.iter().flat_map(|s| &s.steps) {
        if step.partition_keys.is_empty() {
            continue;
        }
        let base = step.step_id.base_id();
        let Some(node) = dag.get_node(base) else {
            continue;
        };
        if node.extracted.batch.is_none() {
            continue;
        }
        let i = *index.entry(base.to_string()).or_insert_with(|| {
            keys_of.push(BatchKeys {
                base: base.to_string(),
                ordered: Vec::new(),
            });
            keys_of.len() - 1
        });
        keys_of[i]
            .ordered
            .extend(step.partition_keys.iter().map(|pk| {
                let key = pk.suffix();
                let position = step
                    .keyed
                    .order
                    .as_ref()
                    .and_then(|order| order.get(&key))
                    .copied()
                    .unwrap_or(usize::MAX);
                (position, key)
            }));
    }
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    for BatchKeys { base, mut ordered } in keys_of {
        ordered.sort_by_key(|(position, _)| *position);
        let keys: Vec<String> = ordered.into_iter().map(|(_, key)| key).collect();
        let node = dag.get_node(&base).expect("found above");
        let Some(grouping) = crate::batch::effective(node, over) else {
            continue;
        };
        let labels = match &grouping {
            crate::batch::Grouping::By { source_text, .. } => {
                match crate::batch::evaluate_labels(
                    python,
                    &node.extracted.source_file,
                    source_text,
                    &keys,
                ) {
                    Ok(labels) => Some(labels),
                    Err(e) => {
                        errors.push((base.clone(), format!("could not group the keys: {e}")));
                        continue;
                    }
                }
            }
            _ => None,
        };
        let groups = match crate::batch::group_keys(&keys, &grouping, labels.as_deref()) {
            Ok(groups) => groups,
            Err(e) => {
                errors.push((base.clone(), format!("could not group the keys: {e}")));
                continue;
            }
        };
        for group in groups {
            *next_call += 1;
            calls.push((
                format!("b{next_call}"),
                group
                    .into_iter()
                    .map(|i| format!("{base}[{}]", keys[i]))
                    .collect(),
            ));
        }
    }
    PlannedCalls { calls, errors }
}

async fn drive_phases(session: &mut RunSession<'_, '_>) -> Result<(), BarcaError> {
    let prepared = session.prepared;
    let target_ids: Vec<&str> = prepared.targets.iter().map(|(_, id)| id.as_str()).collect();
    let requested = recover::requested(&prepared.exec_plan, &target_ids);
    let mut queue: VecDeque<Work<'_>> = prepared
        .exec_plan
        .phases
        .iter()
        .map(Work::Planned)
        .collect();
    queue.push_back(Work::Returned);
    let mut next_idx = 0;
    loop {
        if session.started.cancel.is_cancelled() {
            session
                .state
                .phase_error
                .get_or_insert_with(|| "run cancelled".to_string());
            break;
        }
        let Some(work) = queue.pop_front() else {
            break;
        };
        let phase_idx = next_idx;
        next_idx += 1;
        trace_point!(session.started.trace, "phase{phase_idx}_start");
        let selected = select_work(session, work, &mut queue, &requested, phase_idx).await?;
        if session.state.transfer_error.is_some()
            || (session.state.sink_failed && !prepared.keep_going)
        {
            break;
        }
        let Some((phase, recompute)) = selected else {
            continue;
        };
        let recovered =
            recover_phase_inputs(session, phase, recompute, &mut queue, phase_idx).await?;
        if session.state.transfer_error.is_some() {
            break;
        }
        let Some((phase, provided)) = recovered else {
            continue;
        };
        let PlannedCalls {
            calls,
            errors: ungrouped,
        } = plan_calls(
            &prepared.dag,
            &session.request.options.batch,
            &phase,
            session.request.python,
            &mut session.state.next_call,
        );
        if let Some((base, message)) = ungrouped.first() {
            session.state.usage_error = Some(format!("{base}: {message}"));
            for (base, message) in ungrouped {
                fail_step(session, base, message, "BatchError");
            }
            break;
        }
        session.state.results.batches.extend(
            calls
                .iter()
                .flat_map(|(id, keys)| keys.iter().map(move |k| (k.clone(), id.clone()))),
        );
        let coord = dispatch_ready(session, &phase, &provided, phase_idx, calls).await;
        let phase_outputs = collect_outputs(session, &coord);
        computed_by_other_runs(session, &phase_outputs);
        collect_failures(session, &coord).await;
        session
            .state
            .results
            .merge_plan_outputs(&phase_outputs, session.state.decide_state.run_hashes());
        if let Some(incremental) = &mut session.state.incremental {
            incremental.observe(
                &prepared.dag,
                &phase,
                &session.state.results.outputs,
                session.state.decide_state.run_hashes(),
                &session.state.decide_state.sensor_outputs,
                &session.state.results.observations,
            );
        }
        // A failed key blocks only the keys that read it: the run goes on and the next phases
        // drop those keys (`blocked`). A step that failed whole stops a single-target run.
        if session.state.phase_error.is_some()
            || session.state.transfer_error.is_some()
            || (session.state.whole_failure && !prepared.keep_going)
        {
            break;
        }
    }
    finish_queue(session, &mut queue);
    Ok(())
}
async fn record_and_publish(
    session: &mut RunSession<'_, '_>,
    touched: &[String],
) -> Result<f64, BarcaError> {
    let elapsed = finalize_run(
        FinalizeRun {
            results: &mut session.state.results,
            touched,
            pool: session.pool.take().expect("workers started"),
            recorder: session.recorder.take().expect("recorder started"),
            pb: &session.pb,
            agent_mode: session.request.agent_mode,
            incremental: session.state.incremental.as_ref(),
            proven_cached: session
                .state
                .incremental
                .as_ref()
                .map_or(0, |i| i.unseeded_cached()),
            steps_executed: session.state.steps_executed,
            completed_steps: session.state.completed_steps,
            total_steps: session.state.total_steps,
            cancel: &session.started.cancel,
            phase_error: &session.state.phase_error,
            step_failure: &session.state.step_failure,
            store: &mut session.store,
            store_paths: &mut session.state.store_paths,
            transfer_error: &mut session.state.transfer_error,
            uploads: &mut session.state.uploads,
            metadata_seconds: &mut session.state.metadata_seconds,
            cached_node_ids: &session.state.cached_node_ids,
            t0: session.started.trace.start,
            cost_model: &session.cost_model,
            run_id: &session.started.run_id,
            command_label: session.request.command_label,
            file_args: session.request.file_args,
            target_label: &session.prepared.target_label,
            decide_state: &session.state.decide_state,
            db_path: &session.request.cfg.db_path,
            logs_buffer: &mut session.state.logs_buffer,
            telemetry: &session.started.telemetry,
            dag: &session.prepared.dag,
            run_started: session.started.run_started,
            skipped_bases: &session.state.skipped_bases,
            job_name: &session.prepared.job_name,
            targets: &session.prepared.targets,
            repaired_mismatches: &mut session.state.repaired_mismatches,
            python: session.request.python,
            cfg: session.request.cfg,
            interrupt: &session.request.interrupt,
            trigger: &session.request.options.trigger,
            server_owned: session.request.options.host.server_owned(),
        },
        |point| trace_point!(session.started.trace, "{point}"),
    )
    .await?;

    Ok(elapsed)
}
async fn return_outputs(
    session: &mut RunSession<'_, '_>,
    elapsed: f64,
) -> Result<Executed, BarcaError> {
    let (mut step_reports, mut outcomes, final_output, mut partitions, mut key_summary) =
        finalize_outputs(
            &session.prepared.dag,
            &session.prepared.targets,
            &session.prepared.exec_plan,
            session.prepared.keep_going,
            session.request.partition.as_ref(),
            &session.state.results.outputs,
            &session.state.results.failures,
            std::mem::take(&mut session.state.results.reports),
            &session.state.failed_bases,
            &session.state.skipped_bases,
            &session.state.cached_node_ids,
            &mut session.store,
            &session.started.cancel,
            // With a server, its ledger holds the record (#576).
            session
                .request
                .cfg
                .ledger
                .is_none()
                .then_some(session.request.cfg.db_path.as_str()),
        )
        .await?;

    if let Some(incremental) = &session.state.incremental {
        let ids: Vec<&str> = session
            .prepared
            .targets
            .iter()
            .map(|(_, id)| id.as_str())
            .collect();
        if let Some(node) = returned_node(
            &session.prepared.exec_plan,
            &ids,
            session.prepared.keep_going,
        ) && let Some(summary) =
            incremental.summary(&node, partitions.as_deref(), &session.state.cached_node_ids)
        {
            key_summary = Some(summary);
            partitions.get_or_insert_with(Vec::new);
        }
        for ((_, id), (_, outcome)) in session.prepared.targets.iter().zip(outcomes.iter_mut()) {
            if let Some(summary) = incremental.summary(
                id,
                outcome.partitions.as_deref(),
                &session.state.cached_node_ids,
            ) {
                if outcome.error.as_deref() == Some("did not run") {
                    outcome.status = StepStatus::Success.to_string();
                    outcome.error = None;
                }
                outcome.key_summary = Some(summary);
                outcome.partitions.get_or_insert_with(Vec::new);
            }
        }
    }

    session.state.results.annotate_reports(&mut step_reports);

    // Only a step that ran can record changed bytes: a run that executed none (every step
    // cached) is not looked up.
    let mut changed = if session.state.steps_executed > 0 {
        db::changed_bytes_in_run(&session.request.cfg.db_path, &session.started.run_id).await?
    } else {
        Vec::new()
    };
    // With a server, the run is posted to its ledger now that its uploads are confirmed. A
    // step the ledger already held with other bytes gets the same warning.
    if let Some(ledger) = &session.request.cfg.ledger {
        for node in ledger
            .record_run(
                session.request.cfg,
                &session.started.run_id,
                &session.prepared.dag,
            )
            .await
        {
            if !changed.contains(&node) {
                changed.push(node);
            }
        }
    }
    // Only an asset promises the same bytes for the same code and inputs. A sensor returns a
    // new value under the same run hash whenever the data it watches changes, a task runs
    // every time, and a `Manual` asset is refreshed by hand to pick up new data (#548): none
    // of them is a nondeterministic function.
    changed.retain(|node| {
        let base = crate::StepId::parse(node).base_id().to_string();
        session
            .prepared
            .dag
            .get_node(&base)
            .is_none_or(crate::cache::checks_bytes)
    });
    for node in changed.iter().chain(&session.state.repaired_mismatches) {
        crate::run_line!(Always, "{}", crate::ledger::bytes_changed_warning(node));
        let base = crate::StepId::parse(node).base_id().to_string();
        if let Some(report) = step_reports.iter_mut().find(|report| report.id == base) {
            report.bytes_changed = Some(true);
        }
    }

    // Under a tick, a target blocked on a never-computed `Manual` node is a skipped step.
    step_reports.extend(blocked_steps(
        &session.prepared.dag,
        &session.prepared.blocked,
        &session.prepared.paused,
    ));
    // Under --partition, a fan-in over a narrowed node (and what is below it) is not run.
    if let Some(n) = &session.state.narrowing {
        step_reports.extend(n.skipped_reports(&session.prepared.dag, false));
    }
    Ok(Executed {
        result: GetResult {
            run_id: std::mem::take(&mut session.started.run_id),
            elapsed_seconds: elapsed,
            steps_executed: session.state.steps_executed,
            phases: session.prepared.exec_plan.phases.len(),
            final_output,
            steps: step_reports,
            warnings: session.prepared.plan_warnings.clone(),
            partitions,
            key_summary,
            cache: None,
            uploads: session.state.uploads.take(),
            metadata_seconds: session.state.metadata_seconds,
            partition_selection: session.state.narrowing.as_ref().map(|n| n.report()),
        },
        targets: outcomes
            .into_iter()
            .chain(session.prepared.blocked.iter().cloned())
            .collect(),
        step_failure: session
            .state
            .step_failure
            .take()
            .map(|(node, message)| crate::FailedStep {
                phase: session.state.step_failure_phase,
                artifact_dir: Some(format!(
                    "{}/{}",
                    session.request.cfg.artifact_root.trim_end_matches('/'),
                    crate::safe_node_id(&node)
                )),
                node,
                message,
                run: None,
            }),
    })
}

#[cfg(test)]
mod store_tests;
