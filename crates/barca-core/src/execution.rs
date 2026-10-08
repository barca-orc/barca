//! Run preparation, phase dispatch, and finalization.
use crate::BarcaError;
use crate::cache::{
    BLOCKED_ARTIFACT_PATH, CachePolicy, DecideState, Decision, RunReason, decide_step,
    has_sensor_output, in_dependency_order, localize_decision, sensor_inputs,
};
use crate::dag::Dag;
use crate::db;
use crate::dispatch;
use crate::dispatch::OutputRef;
use crate::planner::{ExecutionPlan, Phase, ResourceConfig};
use crate::recover::{self, Work};
use crate::report::{
    RunOutcome, cached_step_line, end_of_run_line, env_suffix, failed_step_line, fmt_bytes,
    fmt_eta, kind_str, merge_partition_reports, reconcile_total, report_for, stale_warning,
};
use crate::state_sync;
use crate::targets::{
    blocking_failure, final_output_of, plan_for_targets, resolve_targets, short_name,
    skipped_tasks_note, target_outcomes, validate_refresh_names,
};
use crate::transfer::{ArtifactLayout, TransferClient};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::load::build_dag;
use crate::results::{
    ExplainResult, ExplainSummary, GetResult, MultiResult, StepReport, TargetOutcome,
    TargetPrediction,
};

use crate::persist::{
    RunLedger, SharedPush, StepRecorder, StepRow, cancel_recorded_run, canonical_job, persist_run,
    telemetry_report,
};
use crate::store_sync::{StoreSync, transfer_remedy};

/// A spawned task that is aborted if dropped before it is joined, so an early
/// return never leaves background work running (e.g. a state pull that
/// would overwrite the local DB after the run gave up).
pub(crate) struct Background<T>(Option<tokio::task::JoinHandle<T>>);

impl<T: Send + 'static> Background<T> {
    pub(crate) fn spawn(fut: impl std::future::Future<Output = T> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(fut)))
    }

    pub(crate) async fn join(mut self) -> Result<T, BarcaError> {
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

/// Worker pool size: `BARCA_POOL_SIZE` overrides auto-detection when set to a
/// positive integer. Lets benchmark harnesses (and anyone else) pin the pool
/// to a fixed core count instead of whatever `available_parallelism()` reports
/// on the current machine.
pub(crate) fn default_pool_size() -> usize {
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
        }
    }
}

pub(crate) struct ExecuteRequest<'a> {
    pub(crate) cfg: &'a crate::config::ResolvedConfig,
    pub(crate) target_names: &'a [String],
    pub(crate) file_args: &'a [String],
    pub(crate) python: &'a std::path::Path,
    pub(crate) no_cache: bool,
    pub(crate) agent_mode: bool,
    pub(crate) policy: CachePolicy,
    pub(crate) command_label: &'a str,
    pub(crate) interrupt: crate::interrupt::Interrupt,
    pub(crate) event_tx: Option<UnboundedSender<crate::RunEvent>>,
}

struct Trace {
    start: Instant,
    enabled: bool,
}
macro_rules! trace_point {
    ($trace:expr, $($arg:tt)*) => {
        if $trace.enabled {
            eprintln!("[trace] {:>8.1}ms  {}", $trace.start.elapsed().as_secs_f64() * 1000.0, format_args!($($arg)*));
        }
    };
}
struct RunStart {
    trace: Trace,
    cancel: CancellationToken,
    run_id: String,
    run_started: std::time::SystemTime,
    telemetry: Vec<(String, Box<dyn crate::telemetry::Integration>)>,
    state_sync_on: bool,
    transfer_start: Option<crate::transfer::Launching>,
    pull: Option<Background<Result<(state_sync::Pulled, std::time::Duration), BarcaError>>>,
}
impl RunStart {
    fn begin(request: &ExecuteRequest<'_>) -> Result<Self, BarcaError> {
        let cancel = request.interrupt.cancel.clone();
        let trace = Trace {
            start: Instant::now(),
            enabled: std::env::var("BARCA_TRACE_TIMING").is_ok(),
        };
        let run_id = db::generate_run_id();
        let run_started = std::time::SystemTime::now();
        let telemetry = crate::telemetry::configured();
        let cfg = request.cfg;
        let python = request.python;
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
        let transfer_start = match cfg.remote_artifacts() {
            true => Some(TransferClient::launch(python, cfg, &run_id)?),
            false => None,
        };

        Ok(Self {
            trace,
            cancel,
            run_id,
            run_started,
            telemetry,
            state_sync_on,
            pull,
            transfer_start,
        })
    }
}
#[derive(Default)]
struct RunState {
    cached_node_ids: std::collections::HashSet<String>,
    decide_state: DecideState,
    step_reports: Vec<StepReport>,
    phase_error: Option<String>,
    step_failure: Option<(String, String)>,
    failed_bases: std::collections::HashSet<String>,
    skipped_bases: std::collections::HashSet<String>,
    all_outputs: HashMap<String, OutputRef>,
    logs_buffer: Vec<(String, String)>,
    all_sinks: HashMap<String, String>,
    all_timings: HashMap<String, (Option<f64>, Option<u64>)>,
    step_clocks: HashMap<String, (f64, f64)>,
    all_failures: Vec<dispatch::StepFailure>,
    all_attempts: HashMap<String, u32>,
    steps_executed: usize,
    total_steps: usize,
    total_estimated: f64,
    elapsed_so_far: f64,
    completed_steps: usize,
    store_paths: HashMap<String, String>,
    transfer_error: Option<String>,
    cached_steps: recover::CachedSteps,
    held_cached_lines: Vec<String>,
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
    state_token: Option<state_sync::StateToken>,
    started: RunStart,
}

pub(crate) async fn execute(request: ExecuteRequest<'_>) -> Result<Executed, BarcaError> {
    let started = RunStart::begin(&request)?;
    let prepared = prepare_run(
        request.target_names,
        request.file_args,
        request.python,
        &request.policy,
        request.command_label,
        |point| trace_point!(started.trace, "{point}"),
    )
    .await?;
    let mut session = open_run(&request, &prepared, started).await?;
    drive_phases(&mut session).await?;
    let elapsed = record_and_publish(&mut session).await?;
    return_outputs(&mut session, elapsed).await
}
async fn open_state(
    request: &ExecuteRequest<'_>,
    prepared: &Prepared,
    started: &mut RunStart,
) -> Result<(Option<state_sync::StateToken>, crate::cost::CostModel), BarcaError> {
    let cfg = request.cfg;
    let run_id = &started.run_id;
    let command_label = request.command_label;
    let file_args = request.file_args;
    let target_label = &prepared.target_label;
    let exec_plan = &prepared.exec_plan;
    let state_sync_on = started.state_sync_on;
    db::ensure_env_dirs(&cfg.env)?;
    let db_path = cfg.db_path.clone();

    // Shared remote state: the pull must land before the DB is opened, so
    // cache checks below see every machine's materializations. Pull failure
    // is a hard error — silently diverging local runs are worse than stopping.
    let state_token = match started.pull.take() {
        Some(pull) => {
            let pulled = match pull.join().await.and_then(|pulled| pulled) {
                Ok(pulled) => pulled,
                Err(e) => {
                    // The command ends here (the pull failed, or Ctrl-C cancelled it). It
                    // does not return before the transfer helper it started is gone.
                    if let Some(start) = started.transfer_start.take() {
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
    trace_point!(
        started.trace,
        "state_sync_pull_joined (enabled={state_sync_on})"
    );

    db::init_db(&db_path).await?;
    trace_point!(started.trace, "db_init");

    db::create_run(
        &db_path,
        run_id,
        command_label,
        &db::encode_files(file_args),
        target_label.as_deref(),
        Some(exec_plan.total_steps),
    )
    .await?;
    trace_point!(started.trace, "db_create_run");

    // Measured-cost model: seed from persisted estimates so batch sizing is
    // pre-warmed — the cold-start probe is paid once ever per stable node,
    // not once per run.
    let mut cost_model = crate::cost::CostModel::new();
    cost_model.seed(db::load_cost_estimates(&db_path).await?);
    trace_point!(started.trace, "cost_model_seeded");

    Ok((state_token, cost_model))
}
async fn prepare_progress(
    exec_plan: &ExecutionPlan,
    db_path: &str,
    agent_mode: bool,
    trace: &Trace,
) -> Result<(Option<indicatif::ProgressBar>, f64), BarcaError> {
    // Progress bar setup. `total_steps` starts as the plan-time estimate and
    // grows as dynamic (`partitions_from`) phases expand at dispatch time —
    // see the `phase_step_count` reconciliation below.
    let total_steps = exec_plan.total_steps;
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
    let avg_times = db::get_avg_elapsed(db_path, &unpartitioned_node_ids).await?;
    let partitioned_avg_times =
        db::get_avg_elapsed_for_partitioned(db_path, &partitioned_base_ids).await?;
    trace_point!(
        trace,
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

    Ok((pb, total_estimated))
}
async fn start_workers(
    request: &ExecuteRequest<'_>,
    prepared: &Prepared,
    started: &mut RunStart,
    pb: &Option<indicatif::ProgressBar>,
) -> Result<(Option<StoreSync>, crate::io_loop::WorkerPool, StepRecorder), BarcaError> {
    let cfg = request.cfg;
    let python = request.python;
    let cancel = &started.cancel;
    let pool_size = prepared.pool_size;
    let run_id = &started.run_id;
    let telemetry = &started.telemetry;
    let job_name = &prepared.job_name;
    let db_path = &cfg.db_path;
    // Separate artifact store: workers still read and write only the local
    // artifact dir; the transfer helper uploads finished artifacts in the
    // background and fetches cache hits recorded by other machines.
    let store: Option<StoreSync> = if let Some(start) = started.transfer_start.take() {
        Some(StoreSync::new(start.connect().await?, cancel.clone()))
    } else {
        None
    };
    trace_point!(
        started.trace,
        "store_sync_started (enabled={})",
        store.is_some()
    );
    // Store location of every output uploaded this run, by node id.
    // Set when an artifact cannot be fetched or uploaded: the run fails.
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
    trace_point!(started.trace, "pool_started");

    Ok((store, pool, recorder))
}
async fn open_run<'a, 'r>(
    request: &'a ExecuteRequest<'r>,
    prepared: &'a Prepared,
    mut started: RunStart,
) -> Result<RunSession<'a, 'r>, BarcaError> {
    let (state_token, cost_model) = open_state(request, prepared, &mut started).await?;
    let (pb, total_estimated) = prepare_progress(
        &prepared.exec_plan,
        &request.cfg.db_path,
        request.agent_mode,
        &started.trace,
    )
    .await?;
    let (store, pool, recorder) = start_workers(request, prepared, &mut started, &pb).await?;
    let state = RunState {
        total_steps: prepared.exec_plan.total_steps,
        total_estimated,
        ..Default::default()
    };
    Ok(RunSession {
        request,
        prepared,
        started,
        state,
        state_token,
        cost_model,
        pb,
        store,
        pool: Some(pool),
        recorder: Some(recorder),
    })
}
async fn select_work<'p>(
    session: &mut RunSession<'_, '_>,
    work: Work<'p>,
    queue: &mut VecDeque<Work<'p>>,
    requested: &std::collections::HashSet<String>,
    phase_idx: usize,
) -> Result<Option<(Phase, bool)>, BarcaError> {
    let target_ids: Vec<&str> = session
        .prepared
        .targets
        .iter()
        .map(|(_, id)| id.as_str())
        .collect();
    // The steps to dispatch next, and whether they are cached steps being computed again.
    let selected = match work {
        Work::Ready(phase) => (phase, false),
        Work::Recompute(mut ids) => {
            // One of them may have been computed again since this was queued.
            ids.retain(|id| session.state.cached_node_ids.contains(id));
            match session.state.cached_steps.phase_for(&ids) {
                Some(phase) => (phase, true),
                None => return Ok(None),
            }
        }
        Work::Returned => {
            // The output this command hands back is read by whoever ran it: what it
            // prints is made local, and every part of it must be on disk or in the store.
            let returned: Vec<String> = final_output_of(
                &session.prepared.exec_plan,
                &target_ids,
                session.prepared.keep_going,
                &session.state.all_outputs,
            )
            .iter()
            .chain(
                target_outcomes(
                    &session.prepared.dag,
                    &session.prepared.targets,
                    &session.state.all_outputs,
                    &session.state.all_failures,
                )
                .iter()
                .filter_map(|(_, o)| o.final_output.as_ref()),
            )
            .map(|o| o.path.clone())
            .collect();
            let check: Vec<String> = session
                .state
                .cached_node_ids
                .iter()
                .filter(|id| requested.contains(recover::base_of(id)))
                .filter_map(|id| session.state.all_outputs.get(id))
                .map(|o| o.path.clone())
                .chain(returned.iter().cloned())
                .collect();
            let lost = recover::lost(
                &mut session.store,
                &check,
                &returned,
                session.pb.as_ref(),
                &session.state.all_outputs,
                &session.state.cached_node_ids,
                &mut session.state.cached_steps,
            )
            .await;
            match lost {
                Ok(lost) if lost.is_empty() => {}
                Ok(lost) => {
                    queue.push_front(Work::Returned);
                    queue.push_front(Work::Recompute(
                        session.state.cached_steps.first_layer(&lost),
                    ));
                }
                Err(e) => {
                    session.state.transfer_error = Some(e);
                    return Ok(None);
                }
            }
            return Ok(None);
        }
        Work::Planned(phase) => {
            // `partitions_from` sources are read from disk during expansion.
            let sources: Vec<String> =
                dispatch::partition_sources(phase, &session.state.all_outputs)
                    .into_iter()
                    .map(|o| o.path.clone())
                    .collect();
            let lost = recover::lost(
                &mut session.store,
                &sources,
                &sources,
                session.pb.as_ref(),
                &session.state.all_outputs,
                &session.state.cached_node_ids,
                &mut session.state.cached_steps,
            )
            .await;
            match lost {
                Ok(lost) if lost.is_empty() => {}
                Ok(lost) => {
                    queue.push_front(Work::Planned(phase));
                    queue.push_front(Work::Recompute(
                        session.state.cached_steps.first_layer(&lost),
                    ));
                    return Ok(None);
                }
                Err(e) => {
                    session.state.transfer_error = Some(e);
                    return Ok(None);
                }
            }

            return expand_and_decide(session, phase, phase_idx).await;
        }
    };

    Ok(Some(selected))
}
async fn expand_and_decide(
    session: &mut RunSession<'_, '_>,
    phase: &Phase,
    phase_idx: usize,
) -> Result<Option<(Phase, bool)>, BarcaError> {
    // Multi-target run after a failure: drop the steps that depend on a failed
    // step before they are decided; the rest of the phase still runs.
    let unblocked_phase;
    let phase = if session.prepared.keep_going && !session.state.failed_bases.is_empty() {
        let mut p = phase.clone();
        recover::drop_blocked(
            &mut p,
            |base| blocking_failure(&session.prepared.dag, base, &session.state.failed_bases),
            |st, up| {
                let base = st.step_id.base_id();
                session.state.step_reports.push(StepReport {
                    id: base.to_string(),
                    kind: kind_str(session.prepared.dag.get_node(base).map(|n| n.kind())),
                    status: Some("skipped".to_string()),
                    reason: Some("upstream_failed".to_string()),
                    detail: Some(format!("depends on '{}', which failed", short_name(up))),
                    ..Default::default()
                });
                session.state.skipped_bases.insert(base.to_string());
            },
        );
        unblocked_phase = p;
        &unblocked_phase
    } else {
        phase
    };

    let expanded_phase = dispatch::expand_pending_partitions(
        phase,
        &session.state.all_outputs,
        session.prepared.pool_size,
    );
    let phase_ref = expanded_phase.as_ref().unwrap_or(phase);

    // Dynamic partitions (`partitions_from`) are a single placeholder step
    // in the plan-time count but expand to their real per-key count here —
    // reconcile `total_steps` so the ETA math below can't underflow and
    // the printed summary reflects what actually ran.
    if expanded_phase.is_some() {
        let expanded_count = phase_step_count(phase_ref);
        let planned_count = phase_step_count(phase);
        if expanded_count > planned_count {
            session.state.total_steps += expanded_count - planned_count;
            if let Some(bar) = session.pb.as_ref() {
                bar.set_length(session.state.total_steps as u64);
            }
        }
    }

    let decided = decide_phase(DecidePhase {
        dag: &session.prepared.dag,
        policy: &session.request.policy,
        no_cache: session.request.no_cache,
        db_path: &session.request.cfg.db_path,
        phase_ref,
        decide_state: &mut session.state.decide_state,
        store: &mut session.store,
        step_reports: &mut session.state.step_reports,
        all_outputs: &mut session.state.all_outputs,
        cached_node_ids: &mut session.state.cached_node_ids,
        cached_steps: &mut session.state.cached_steps,
        held_cached_lines: &mut session.state.held_cached_lines,
        pb: &session.pb,
        agent_mode: session.request.agent_mode,
    })
    .await?;
    trace_point!(session.started.trace, "phase{phase_idx}_cache_check_done");
    if decided.streams.is_empty() {
        return Ok(None);
    }
    Ok(Some((decided, false)))
}
async fn recover_phase_inputs<'p>(
    session: &mut RunSession<'_, '_>,
    mut filtered_phase: Phase,
    recompute: bool,
    queue: &mut VecDeque<Work<'p>>,
    phase_idx: usize,
) -> Result<Option<(Phase, HashMap<String, dispatch::ProvidedInput>)>, BarcaError> {
    // A phase that waited for a recompute may since have lost an upstream to a failure
    // (several targets keep going around one): its blocked steps do not run.
    if session.prepared.keep_going && !session.state.failed_bases.is_empty() {
        recover::drop_blocked(
            &mut filtered_phase,
            |base| blocking_failure(&session.prepared.dag, base, &session.state.failed_bases),
            |st, _| {
                session
                    .state
                    .skipped_bases
                    .insert(st.step_id.base_id().to_string());
                if recompute {
                    // Its artifact is still missing and it cannot be computed: it is no
                    // longer an output of this run.
                    for id in recover::output_ids(st) {
                        recover::mark_recomputed(&mut session.state.step_reports, &id, false);
                        session.state.cached_node_ids.remove(&id);
                        session.state.all_outputs.remove(&id);
                    }
                }
            },
        );
        if filtered_phase.streams.is_empty() {
            return Ok(None);
        }
    }

    // Make this phase's inputs available: exactly the artifacts its steps were provided.
    // Cache hits recorded by other machines are fetched (less the parquet inputs every
    // reader in the phase scans lazily, which are read in place). An input that is
    // neither on disk nor in the store has its step computed again first.
    let mut provided = dispatch::build_provided_inputs(&filtered_phase, &session.state.all_outputs);
    let check = recover::input_paths(&provided);
    if let Some(s) = session.store.as_ref() {
        s.read_in_place(
            &mut provided,
            &dispatch::lazily_read_inputs(&filtered_phase),
        );
    }
    let fetch = recover::input_paths(&provided);
    let lost = recover::lost(
        &mut session.store,
        &check,
        &fetch,
        session.pb.as_ref(),
        &session.state.all_outputs,
        &session.state.cached_node_ids,
        &mut session.state.cached_steps,
    )
    .await;
    trace_point!(session.started.trace, "phase{phase_idx}_inputs_local");
    let lost = match lost {
        Ok(lost) => lost,
        Err(e) => {
            if !recompute {
                // Decided to run, but never dispatched: say so (see below the loop).
                queue.push_front(Work::Ready(filtered_phase));
            }
            session.state.transfer_error = Some(e);
            return Ok(None);
        }
    };
    if !lost.is_empty() {
        queue.push_front(if recompute {
            let steps = filtered_phase.streams.iter().flat_map(|s| &s.steps);
            Work::Recompute(steps.flat_map(recover::output_ids).collect())
        } else {
            Work::Ready(filtered_phase)
        });
        queue.push_front(Work::Recompute(
            session.state.cached_steps.first_layer(&lost),
        ));
        return Ok(None);
    }

    if recompute {
        // These are no longer served from cache: they run, and are recorded, like any
        // other step of this run.
        let mut lost: Vec<(String, String)> = Vec::new();
        for step in filtered_phase.streams.iter().flat_map(|s| &s.steps) {
            for id in recover::output_ids(step) {
                recover::mark_recomputed(&mut session.state.step_reports, &id, false);
                session.state.cached_node_ids.remove(&id);
                if let Some(oref) = session.state.all_outputs.remove(&id) {
                    lost.push((id, oref.path));
                }
            }
        }
        for line in recover::recompute_warnings(&lost) {
            note(&session.pb, &line);
        }
    }

    Ok(Some((filtered_phase, provided)))
}
async fn dispatch_ready(
    session: &mut RunSession<'_, '_>,
    filtered_phase: &Phase,
    provided: &HashMap<String, dispatch::ProvidedInput>,
    phase_idx: usize,
) -> crate::coordinator::Coordinator {
    session.state.steps_executed += phase_step_count(filtered_phase);

    let (coord, phase_err, phase_elapsed, phase_completed, phase_total) =
        dispatch_phase(DispatchPhase {
            filtered_phase,
            provided,
            pool: session.pool.as_mut().expect("workers started"),
            cost_model: &mut session.cost_model,
            decide_state: &session.state.decide_state,
            store: &mut session.store,
            recorder: session.recorder.as_ref().expect("recorder started"),
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
fn collect_outputs(
    session: &mut RunSession<'_, '_>,
    coord: &crate::coordinator::Coordinator,
) -> HashMap<String, OutputRef> {
    // Collect results from coordinator
    let mut phase_outputs: HashMap<String, dispatch::OutputRef> = HashMap::new();
    for (&item_id, artifact_val) in coord.outputs() {
        let item = coord.item(item_id);
        let node_id = item.step_id.display();
        // Attempts made for this item (dispatch count), keyed by base node
        // id — how the success-row INSERT looks it up.
        session.state.all_attempts.insert(
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
            session.state.all_sinks.insert(
                node_id.clone(),
                serde_json::Value::from(sinks.clone()).to_string(),
            );
        }
        let cpu = artifact_val.get("cpu_seconds").and_then(|v| v.as_f64());
        let rss = artifact_val.get("max_rss_bytes").and_then(|v| v.as_u64());
        if cpu.is_some() || rss.is_some() {
            session
                .state
                .all_timings
                .insert(node_id.clone(), (cpu, rss));
        }
        if let (Some(finished), Some(wall)) = (
            artifact_val.get("finished_at").and_then(|v| v.as_f64()),
            artifact_val.get("wall_seconds").and_then(|v| v.as_f64()),
        ) {
            session
                .state
                .step_clocks
                .insert(node_id.clone(), (finished, wall));
        }
        // A sensor's output hash: its consumers, decided in a later phase, fold it into
        // their run hashes (#183).
        if session
            .prepared
            .dag
            .get_node(item.step_id.base_id())
            .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
            && session.state.decide_state.run_hashes.contains_key(&node_id)
            && let Some(h) = artifact_val.get("content_hash").and_then(|v| v.as_str())
        {
            session
                .state
                .decide_state
                .sensor_outputs
                .insert(node_id.clone(), h.to_string());
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
            session.state.transfer_error.get_or_insert(format!(
                "the result of {node_id} could not be written: {why}"
            ));
            session
                .state
                .failed_bases
                .insert(item.step_id.base_id().to_string());
            continue;
        }
        if session.request.agent_mode {
            eprintln!("{}", failed_step_line(&node_id, error_msg));
        }
        if first_non_group_failure.is_none() {
            first_non_group_failure = Some((node_id.clone(), error_msg.to_string()));
        }
        session
            .state
            .failed_bases
            .insert(item.step_id.base_id().to_string());
        session.state.all_failures.push(dispatch::StepFailure {
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
            session
                .state
                .skipped_bases
                .insert(item.step_id.base_id().to_string());
            session.state.steps_executed = session.state.steps_executed.saturating_sub(1);
        }
    }

    // Remember the first step failure (it does not overwrite an infrastructure error).
    if let Some(msg) = first_non_group_failure
        && session.state.step_failure.is_none()
    {
        session.state.step_failure = Some(msg);
    }
}
fn merge_outputs(session: &mut RunSession<'_, '_>, phase_outputs: &HashMap<String, OutputRef>) {
    // Run hashes were computed pre-dispatch for every plan step (including
    // per-partition ids), so collection is a filter: coordinator outputs
    // with a known hash are plan steps; the rest are parallel() children,
    // which are never persisted.
    for (node_id, oref) in phase_outputs {
        if session.state.decide_state.run_hashes.contains_key(node_id) {
            session
                .state
                .all_outputs
                .insert(node_id.clone(), oref.clone());
        }
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
        if session.state.cached_node_ids.contains(&id) {
            eprintln!("{}", cached_step_line(&session.prepared.dag, &id));
        }
    }
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
        if session.state.transfer_error.is_some() {
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
        let coord = dispatch_ready(session, &phase, &provided, phase_idx).await;
        let phase_outputs = collect_outputs(session, &coord);
        collect_failures(session, &coord).await;
        merge_outputs(session, &phase_outputs);
        if session.state.phase_error.is_some()
            || session.state.transfer_error.is_some()
            || (session.state.step_failure.is_some() && !prepared.keep_going)
        {
            break;
        }
    }
    finish_queue(session, &mut queue);
    Ok(())
}
async fn record_and_publish(session: &mut RunSession<'_, '_>) -> Result<f64, BarcaError> {
    let elapsed = finalize_run(
        FinalizeRun {
            pool: session.pool.take().expect("workers started"),
            recorder: session.recorder.take().expect("recorder started"),
            pb: &session.pb,
            agent_mode: session.request.agent_mode,
            steps_executed: session.state.steps_executed,
            completed_steps: session.state.completed_steps,
            total_steps: session.state.total_steps,
            elapsed_so_far: session.state.elapsed_so_far,
            cancel: &session.started.cancel,
            phase_error: &session.state.phase_error,
            step_failure: &session.state.step_failure,
            store: &mut session.store,
            all_outputs: &mut session.state.all_outputs,
            store_paths: &mut session.state.store_paths,
            failed_bases: &mut session.state.failed_bases,
            all_failures: &mut session.state.all_failures,
            transfer_error: &mut session.state.transfer_error,
            cached_node_ids: &session.state.cached_node_ids,
            t0: session.started.trace.start,
            cost_model: &session.cost_model,
            run_id: &session.started.run_id,
            command_label: session.request.command_label,
            file_args: session.request.file_args,
            target_label: &session.prepared.target_label,
            exec_plan: &session.prepared.exec_plan,
            all_sinks: &session.state.all_sinks,
            all_attempts: &session.state.all_attempts,
            all_timings: &session.state.all_timings,
            decide_state: &session.state.decide_state,
            db_path: &session.request.cfg.db_path,
            logs_buffer: &session.state.logs_buffer,
            telemetry: &session.started.telemetry,
            dag: &session.prepared.dag,
            run_started: session.started.run_started,
            step_clocks: &session.state.step_clocks,
            job_name: &session.prepared.job_name,
            state_sync_on: session.started.state_sync_on,
            python: session.request.python,
            cfg: session.request.cfg,
            state_token: &mut session.state_token,
            interrupt: &session.request.interrupt,
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
    let (step_reports, outcomes, final_output) = finalize_outputs(
        &session.prepared.dag,
        &session.prepared.targets,
        &session.prepared.exec_plan,
        session.prepared.keep_going,
        &session.state.all_outputs,
        &session.state.all_failures,
        std::mem::take(&mut session.state.step_reports),
        &session.state.failed_bases,
        &session.state.skipped_bases,
        &mut session.store,
        &session.started.cancel,
    )
    .await?;

    Ok(Executed {
        result: GetResult {
            run_id: std::mem::take(&mut session.started.run_id),
            elapsed_seconds: elapsed,
            steps_executed: session.state.steps_executed,
            phases: session.prepared.exec_plan.phases.len(),
            final_output,
            steps: step_reports,
            warnings: session.prepared.plan_warnings.clone(),
        },
        targets: outcomes,
        step_failure: session
            .state
            .step_failure
            .take()
            .map(|(node, message)| crate::FailedStep {
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

struct Prepared {
    dag: Dag,
    targets: Vec<(String, String)>,
    job_name: String,
    keep_going: bool,
    target_label: Option<String>,
    pool_size: usize,
    exec_plan: ExecutionPlan,
    plan_warnings: Vec<crate::warnings::PlanWarning>,
}

async fn prepare_run(
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
    policy: &CachePolicy,
    command_label: &str,
    trace: impl Fn(&str),
) -> Result<Prepared, BarcaError> {
    let dag = build_dag(file_args, python).await?;
    trace("dag_built");

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
    trace("planned");

    if let CachePolicy::RefreshSelective { names, .. } = policy {
        validate_refresh_names(&dag, &target_ids, names, command_label == "get")?;
    }
    // Plan-time warnings for the steps this command planned, before anything runs.
    let plan_warnings = crate::warnings::for_plan(&dag, &exec_plan);
    crate::warnings::print(&plan_warnings);

    Ok(Prepared {
        dag,
        targets,
        job_name,
        keep_going,
        target_label,
        pool_size,
        exec_plan,
        plan_warnings,
    })
}

#[allow(clippy::too_many_arguments)]
async fn finalize_outputs(
    dag: &Dag,
    targets: &[(String, String)],
    exec_plan: &ExecutionPlan,
    keep_going: bool,
    all_outputs: &HashMap<String, dispatch::OutputRef>,
    all_failures: &[dispatch::StepFailure],
    step_reports: Vec<StepReport>,
    failed_bases: &std::collections::HashSet<String>,
    skipped_bases: &std::collections::HashSet<String>,
    store: &mut Option<StoreSync>,
    cancel: &CancellationToken,
) -> Result<
    (
        Vec<StepReport>,
        Vec<(String, TargetOutcome)>,
        Option<OutputRef>,
    ),
    BarcaError,
> {
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
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

    let outcomes = target_outcomes(dag, targets, all_outputs, all_failures);

    let final_output = final_output_of(exec_plan, &target_ids, keep_going, all_outputs);

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
        crate::mismatch::mark(dag, &mut step_reports, &s.mismatched);
    }

    Ok((step_reports, outcomes, final_output))
}

/// [`crate::queries::explain`] on an already-built DAG (`barca status` reuses its DAG for the node listing).
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

    let mut prediction = Prediction {
        dag,
        policy: &policy,
        no_cache,
        cache: cache.as_ref(),
        layout: layout.as_ref(),
        pool_size,
        state: &mut state,
        all_outputs: &mut all_outputs,
        cached_steps: &mut cached_steps,
        unknown_ids: &mut unknown_ids,
        hash_unknown: &mut hash_unknown,
        steps: &mut steps,
        summary: &mut summary,
    };
    for phase in &exec_plan.phases {
        predict_phase(&mut prediction, phase).await;
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

struct Prediction<'a> {
    dag: &'a Dag,
    policy: &'a CachePolicy,
    no_cache: bool,
    cache: Option<&'a db::CacheReader>,
    layout: Option<&'a ArtifactLayout>,
    pool_size: usize,
    state: &'a mut DecideState,
    all_outputs: &'a mut HashMap<String, OutputRef>,
    cached_steps: &'a mut recover::CachedSteps,
    unknown_ids: &'a mut HashMap<String, &'static str>,
    hash_unknown: &'a mut std::collections::HashSet<String>,
    steps: &'a mut Vec<StepReport>,
    summary: &'a mut ExplainSummary,
}
fn unknown_report(dag: &Dag, base_id: &str, reason: &str, detail: String) -> StepReport {
    StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        action: Some("unknown".to_string()),
        reason: Some(reason.to_string()),
        detail: Some(detail),
        ..Default::default()
    }
}
fn expand_prediction(ctx: &mut Prediction<'_>, phase: &Phase) -> Phase {
    // A `partitions_from` source is read to expand its consumers: if its artifact is
    // missing it runs again first, and its keys are not known until it has.
    let sources = dispatch::partition_sources(phase, ctx.all_outputs)
        .into_iter()
        .map(|o| o.path.clone())
        .collect();
    recover::predict_recomputes(
        sources,
        ctx.cached_steps,
        ctx.all_outputs,
        ctx.layout,
        ctx.steps,
        ctx.summary,
    );

    // Dynamic partitions need their source's output to know their keys. If the source is
    // not available (it would have to run first) the step cannot be expanded.
    let mut ready = phase.clone();
    for stream in &mut ready.streams {
        stream.steps.retain(|st| {
            let missing_source = st.pending_partitions.values().find(|src| {
                !ctx.all_outputs
                    .keys()
                    .any(|k| k.ends_with(&format!(":{src}")) || k.as_str() == src.as_str())
            });
            match missing_source {
                Some(src) => {
                    let base = st.step_id.base_id();
                    ctx.steps.push(unknown_report(
                        ctx.dag,
                        base,
                        "partitions_unknown",
                        format!(
                            "partition keys come from the output of '{src}', which is not \
                                 available until it runs"
                        ),
                    ));
                    ctx.unknown_ids
                        .insert(base.to_string(), "partitions_unknown");
                    ctx.summary.unknown += 1;
                    false
                }
                None => true,
            }
        });
    }

    ready
}
async fn predict_steps(
    ctx: &mut Prediction<'_>,
    phase_ref: &Phase,
) -> Vec<crate::planner::StreamStep> {
    let mut to_run = Vec::new();
    for (_, step) in in_dependency_order(ctx.dag, phase_ref) {
        let base = step.step_id.base_id();
        let up_base = |up: &str| up.split('[').next().unwrap_or(up).to_string();
        let unknown_dep = step
            .inputs
            .values()
            .find(|up| ctx.unknown_ids.get(&up_base(up)) == Some(&"partitions_unknown"));
        if let Some(up) = unknown_dep {
            ctx.steps.push(unknown_report(
                ctx.dag,
                base,
                "partitions_unknown",
                format!(
                    "depends on '{}', whose partitions are not known until it runs",
                    short_name(up)
                ),
            ));
            ctx.unknown_ids
                .insert(base.to_string(), "partitions_unknown");
            ctx.summary.unknown += 1;
            continue;
        }

        // Sensors this step reads, and whether the dry run knows their output.
        let read_sensors = sensor_inputs(ctx.dag, step);
        let missing_sensor = read_sensors
            .iter()
            .find(|s| !has_sensor_output(ctx.state, s))
            .map(|s| s.to_string());
        let hash_unknown_dep = step
            .inputs
            .values()
            .find(|up| ctx.hash_unknown.contains(&up_base(up)))
            .cloned();

        let (step, decision) = decide_step(
            ctx.dag,
            ctx.policy,
            ctx.no_cache,
            ctx.cache,
            ctx.state,
            step,
        )
        .await;
        // Forced to run whatever the cache holds (task, sensor, refresh, --no-cache).
        let forced = matches!(
            &decision,
            Decision::Run(reason) if *reason != RunReason::NotMaterialized
        );
        if missing_sensor.is_some() || hash_unknown_dep.is_some() {
            ctx.hash_unknown.insert(base.to_string());
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
                if !ctx.steps.iter().any(|r| r.id == base) {
                    ctx.steps.push(unknown_report(
                        ctx.dag,
                        base,
                        "sensor_output_unknown",
                        detail,
                    ));
                }
                ctx.unknown_ids
                    .insert(base.to_string(), "sensor_output_unknown");
                ctx.summary.unknown += step.partition_keys.len().max(1);
                continue;
            }
        }

        let mut report = report_for(ctx.dag, &step, &decision, true);
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
        ctx.steps.push(report);
        match decision {
            Decision::Run(_) => {
                ctx.summary.will_run += step.partition_keys.len().max(1);
                to_run.push(step);
            }
            Decision::Cached { oref, .. } => {
                ctx.summary.cached += 1;
                ctx.all_outputs.insert(step.step_id.display(), oref);
                ctx.cached_steps.remember(step);
            }
            Decision::Partitioned { cached, missing } => {
                ctx.summary.cached += cached.len();
                ctx.summary.will_run += missing.len();
                let any_cached = !cached.is_empty();
                for (pdisplay, oref) in cached {
                    ctx.all_outputs.insert(pdisplay, oref);
                }
                if !missing.is_empty() {
                    let mut partial = step.clone();
                    partial.partition_keys = missing;
                    to_run.push(partial);
                }
                if any_cached {
                    ctx.cached_steps.remember(step);
                }
            }
        }
    }

    to_run
}
async fn predict_phase(ctx: &mut Prediction<'_>, phase: &Phase) {
    let ready = expand_prediction(ctx, phase);
    let expanded = dispatch::expand_pending_partitions(&ready, ctx.all_outputs, ctx.pool_size);
    let phase_ref = expanded.as_ref().unwrap_or(&ready);
    // The steps of this phase that would execute: what they read has to be there.

    let to_run = predict_steps(ctx, phase_ref).await;
    let running = Phase {
        reason: phase_ref.reason.clone(),
        streams: vec![crate::planner::WorkerStream {
            stream_id: "dry-run".to_string(),
            steps: to_run,
        }],
    };
    let inputs = recover::input_paths(&dispatch::build_provided_inputs(&running, ctx.all_outputs));
    recover::predict_recomputes(
        inputs,
        ctx.cached_steps,
        ctx.all_outputs,
        ctx.layout,
        ctx.steps,
        ctx.summary,
    );
}
struct DecidePhase<'a> {
    dag: &'a Dag,
    policy: &'a CachePolicy,
    no_cache: bool,
    db_path: &'a str,
    phase_ref: &'a Phase,
    decide_state: &'a mut DecideState,
    store: &'a mut Option<StoreSync>,
    step_reports: &'a mut Vec<StepReport>,
    all_outputs: &'a mut HashMap<String, OutputRef>,
    cached_node_ids: &'a mut std::collections::HashSet<String>,
    cached_steps: &'a mut recover::CachedSteps,
    held_cached_lines: &'a mut Vec<String>,
    pb: &'a Option<indicatif::ProgressBar>,
    agent_mode: bool,
}

async fn decide_phase(ctx: DecidePhase<'_>) -> Result<Phase, BarcaError> {
    let DecidePhase {
        dag,
        policy,
        no_cache,
        db_path,
        phase_ref,
        decide_state,
        store,
        step_reports,
        all_outputs,
        cached_node_ids,
        cached_steps,
        held_cached_lines,
        pb,
        agent_mode,
    } = ctx;
    let mut uncached_streams: Vec<crate::planner::WorkerStream> = Vec::new();

    // Open the DB only for this phase's cache lookups and release it before any
    // step runs, so other barca processes can use the metadata DB while Python
    // executes.
    let cache = db::CacheReader::open(db_path).await?;

    // Decide all upstream chunks before any consumer, independently of the worker split.
    let mut uncached: Vec<Vec<crate::planner::StreamStep>> =
        vec![Vec::new(); phase_ref.streams.len()];
    for (stream_idx, step) in in_dependency_order(dag, phase_ref) {
        let uncached_steps = &mut uncached[stream_idx];
        let (step, decision) =
            decide_step(dag, policy, no_cache, Some(&cache), decide_state, step).await;
        let decision = localize_decision(decision, &step, store);
        step_reports.push(report_for(dag, &step, &decision, false));
        let display_id = step.step_id.display();
        match decision {
            Decision::Run(_) => uncached_steps.push(step),
            Decision::Cached { oref, stale_root } => {
                if let Some(root) = stale_root {
                    note(
                        pb,
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
                        eprintln!("{}", cached_step_line(dag, &display_id));
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
    for (stream, uncached_steps) in phase_ref.streams.iter().zip(uncached) {
        if !uncached_steps.is_empty() {
            uncached_streams.push(crate::planner::WorkerStream {
                stream_id: stream.stream_id.clone(),
                steps: uncached_steps,
            });
        }
    }

    drop(cache);

    Ok(Phase {
        reason: phase_ref.reason.clone(),
        streams: uncached_streams,
    })
}

/// Print a note above the progress bar, or to stderr when no bar is visible. A hidden bar
/// (stderr is not a terminal, as when an agent or CI drives barca) silently swallows
/// `ProgressBar::println`, which used to make warnings vanish.
pub(crate) fn note(pb: &Option<indicatif::ProgressBar>, msg: &str) {
    match pb {
        Some(bar) if !bar.is_hidden() => bar.println(msg),
        _ => eprintln!("{msg}"),
    }
}

struct DispatchPhase<'a> {
    filtered_phase: &'a Phase,
    provided: &'a HashMap<String, dispatch::ProvidedInput>,
    pool: &'a mut crate::io_loop::WorkerPool,
    cost_model: &'a mut crate::cost::CostModel,
    decide_state: &'a DecideState,
    store: &'a mut Option<StoreSync>,
    recorder: &'a StepRecorder,
    store_paths: &'a mut HashMap<String, String>,
    dag: &'a Dag,
    pb: &'a Option<indicatif::ProgressBar>,
    agent_mode: bool,
    total_estimated: f64,
    elapsed_so_far: f64,
    completed_steps: usize,
    total_steps: usize,
    event_tx: &'a Option<UnboundedSender<crate::RunEvent>>,
    logs_buffer: &'a mut Vec<(String, String)>,
    cancel: &'a CancellationToken,
}

async fn dispatch_phase(
    ctx: DispatchPhase<'_>,
) -> (
    crate::coordinator::Coordinator,
    Result<(), String>,
    f64,
    usize,
    usize,
) {
    let DispatchPhase {
        filtered_phase,
        provided,
        pool,
        cost_model,
        decide_state,
        store,
        recorder,
        store_paths,
        dag,
        pb,
        agent_mode,
        total_estimated,
        mut elapsed_so_far,
        mut completed_steps,
        mut total_steps,
        event_tx,
        logs_buffer,
        cancel,
    } = ctx;
    let mut coord = crate::coordinator::Coordinator::new();
    let loaded = coord.load_phase(filtered_phase, provided);
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
                        note(pb, &msg);
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
                if let Some(bar) = pb.as_ref() {
                    bar.set_length(total_steps as u64);
                }
            }
            if let Some(bar) = pb.as_ref() {
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
                    env_suffix(dag, node_id)
                );
            }
        },
    );

    // Event sink — buffer log lines for DB persistence, and forward every
    // event live to the caller's channel (the HTTP server) if present.
    let event_tx_phase = event_tx.clone();
    let logs_sink = logs_buffer;
    let on_event_cb: crate::io_loop::EventCallback<'_> = Box::new(move |ev: crate::RunEvent| {
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
            cost_model,
            Some(on_step_cb),
            Some(on_event_cb),
            cancel,
        )
        .await;
    (
        coord,
        phase_err,
        elapsed_so_far,
        completed_steps,
        total_steps,
    )
}

struct FinalizeRun<'a> {
    pool: crate::io_loop::WorkerPool,
    recorder: StepRecorder,
    pb: &'a Option<indicatif::ProgressBar>,
    agent_mode: bool,
    steps_executed: usize,
    completed_steps: usize,
    total_steps: usize,
    elapsed_so_far: f64,
    cancel: &'a CancellationToken,
    phase_error: &'a Option<String>,
    step_failure: &'a Option<(String, String)>,
    store: &'a mut Option<StoreSync>,
    all_outputs: &'a mut HashMap<String, OutputRef>,
    store_paths: &'a mut HashMap<String, String>,
    failed_bases: &'a mut std::collections::HashSet<String>,
    all_failures: &'a mut Vec<dispatch::StepFailure>,
    transfer_error: &'a mut Option<String>,
    cached_node_ids: &'a std::collections::HashSet<String>,
    t0: Instant,
    cost_model: &'a crate::cost::CostModel,
    run_id: &'a str,
    command_label: &'a str,
    file_args: &'a [String],
    target_label: &'a Option<String>,
    exec_plan: &'a ExecutionPlan,
    all_sinks: &'a HashMap<String, String>,
    all_attempts: &'a HashMap<String, u32>,
    all_timings: &'a HashMap<String, (Option<f64>, Option<u64>)>,
    decide_state: &'a DecideState,
    db_path: &'a str,
    logs_buffer: &'a [(String, String)],
    telemetry: &'a [(String, Box<dyn crate::telemetry::Integration>)],
    dag: &'a Dag,
    run_started: std::time::SystemTime,
    step_clocks: &'a HashMap<String, (f64, f64)>,
    job_name: &'a str,
    state_sync_on: bool,
    python: &'a std::path::Path,
    cfg: &'a crate::config::ResolvedConfig,
    state_token: &'a mut Option<state_sync::StateToken>,
    interrupt: &'a crate::interrupt::Interrupt,
}

async fn finalize_run(ctx: FinalizeRun<'_>, trace: impl Fn(&str)) -> Result<f64, BarcaError> {
    let FinalizeRun {
        pool,
        recorder,
        pb,
        agent_mode,
        steps_executed,
        completed_steps,
        total_steps,
        elapsed_so_far,
        cancel,
        phase_error,
        step_failure,
        store,
        all_outputs,
        store_paths,
        failed_bases,
        all_failures,
        transfer_error,
        cached_node_ids,
        t0,
        cost_model,
        run_id,
        command_label,
        file_args,
        target_label,
        exec_plan,
        all_sinks,
        all_attempts,
        all_timings,
        decide_state,
        db_path,
        logs_buffer,
        telemetry,
        dag,
        run_started,
        step_clocks,
        job_name,
        state_sync_on,
        python,
        cfg,
        state_token,
        interrupt,
    } = ctx;
    macro_rules! trace_point {
        ($($arg:tt)*) => { trace(&format!($($arg)*)); };
    }
    stop_workers(
        StopProgress {
            pool,
            pb,
            agent_mode,
            steps_executed,
            completed_steps,
            total_steps,
            elapsed_so_far,
            cancel,
            phase_error,
            step_failure,
        },
        &trace,
    )
    .await;
    let mut was_cancelled = drain_store(
        DrainStore {
            store,
            cancel,
            all_outputs,
            store_paths,
            failed_bases,
            all_failures,
            transfer_error,
        },
        &trace,
    )
    .await;

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
        run_id,
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
        all_outputs,
        all_failures,
        all_sinks,
        all_attempts,
        all_timings,
        cached_node_ids,
        run_hashes: &decide_state.run_hashes,
        output_hashes: &decide_state.sensor_outputs,
        store_paths,
        cost_snapshot: &cost_snapshot,
    };
    persist_run(db_path, &ledger).await?;
    // Persist captured output. Rust owns persistence — logs land in the DB
    // regardless of how the run was triggered (CLI or server).
    db::insert_logs(db_path, run_id, logs_buffer).await?;
    trace_point!("persist_run_done");

    if !telemetry.is_empty() {
        let report = telemetry_report(&ledger, dag, run_started, step_clocks, job_name);
        crate::telemetry::export(telemetry, &report).await;
        trace_point!("telemetry_exported");
    }

    // Shared remote state: upload the local history (`push_state` folds the WAL in and copies
    // it under the database lock). See `SharedPush::run` for conflicts.
    if state_sync_on {
        let push = SharedPush {
            python,
            cfg,
            db_path,
            run_id,
            logs: logs_buffer,
            token: state_token.take().expect("pulled when state sync is on"),
        };
        publish_history(push, &mut ledger, interrupt, &mut was_cancelled, &trace).await?;
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

    Ok(elapsed)
}

struct StopProgress<'a> {
    pool: crate::io_loop::WorkerPool,
    pb: &'a Option<indicatif::ProgressBar>,
    agent_mode: bool,
    steps_executed: usize,
    completed_steps: usize,
    total_steps: usize,
    elapsed_so_far: f64,
    cancel: &'a CancellationToken,
    phase_error: &'a Option<String>,
    step_failure: &'a Option<(String, String)>,
}
async fn stop_workers(ctx: StopProgress<'_>, trace: &impl Fn(&str)) {
    let StopProgress {
        mut pool,
        pb,
        agent_mode,
        steps_executed,
        completed_steps,
        total_steps,
        elapsed_so_far,
        cancel,
        phase_error,
        step_failure,
    } = ctx;
    macro_rules! trace_point { ($($arg:tt)*) => { trace(&format!($($arg)*)); }; }
    // All phases done (or aborted/cancelled) — release the worker pool before
    // persisting.
    let repeated_warnings = pool.take_repeated_warnings();
    pool.shutdown().await;
    trace_point!("pool_shutdown");

    // Finish progress bar. The end-of-run line is the same with and without --agent.
    if let Some(bar) = pb.as_ref() {
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
}
struct DrainStore<'a> {
    store: &'a mut Option<StoreSync>,
    cancel: &'a CancellationToken,
    all_outputs: &'a mut HashMap<String, OutputRef>,
    store_paths: &'a mut HashMap<String, String>,
    failed_bases: &'a mut std::collections::HashSet<String>,
    all_failures: &'a mut Vec<dispatch::StepFailure>,
    transfer_error: &'a mut Option<String>,
}
async fn drain_store(ctx: DrainStore<'_>, trace: &impl Fn(&str)) -> bool {
    let DrainStore {
        store,
        cancel,
        all_outputs,
        store_paths,
        failed_bases,
        all_failures,
        transfer_error,
    } = ctx;
    macro_rules! trace_point { ($($arg:tt)*) => { trace(&format!($($arg)*)); }; }
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

    was_cancelled
}
async fn publish_history(
    mut push: SharedPush<'_>,
    ledger: &mut RunLedger<'_>,
    interrupt: &crate::interrupt::Interrupt,
    cancelled: &mut bool,
    trace: &impl Fn(&str),
) -> Result<(), BarcaError> {
    let cancel = &interrupt.cancel;
    let db_path = push.db_path;
    let mut was_cancelled = *cancelled;
    macro_rules! trace_point { ($($arg:tt)*) => { trace(&format!($($arg)*)); }; }
    let t_push = Instant::now();
    let mut pushed: Option<u32> = None;
    if !was_cancelled {
        // Ctrl-C stops the push. The run's work is done and recorded, but the command is
        // cancelled before its record was shared, so the record says `cancelled`; the
        // wrap-up below then tries to share that.
        match push.run(ledger, state_sync::Until::cancelled(cancel)).await {
            Ok(retries) => pushed = Some(retries),
            Err(BarcaError::Cancelled) => {
                // The ledger too, so that a replay after a conflict keeps the mark.
                ledger.status = "cancelled";
                cancel_recorded_run(db_path, ledger).await?;
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
        let why = match push.run(ledger, until).await {
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
            fmt_bytes(std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0)),
            t_push.elapsed().as_secs_f64(),
            match retries {
                0 => String::new(),
                1 => " after 1 conflict retry".to_string(),
                n => format!(" after {n} conflict retries"),
            }
        );
        trace_point!("state_sync_pushed (attempts={})", retries + 1);
    }
    *cancelled = was_cancelled;
    Ok(())
}
#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::cache::{CacheHit, resolve_cache_hit};
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
