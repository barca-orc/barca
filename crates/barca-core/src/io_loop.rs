//! Async I/O layer — a persistent pool of stateless workers pulling leased
//! batches from the coordinator's global ready queue.
//!
//! Workers are long-lived across phases of a run (amortizing interpreter
//! startup and user-module imports) and stateless between tasks. Each pull
//! leases `K` tasks, where `K` comes from the measured-cost model
//! ([`crate::cost::CostModel`]): heavy tasks pull one-at-a-time (fully
//! parallel), light tasks batch enough to amortize the per-pull coordination
//! cost. The completion message closes the lease, carries the output ref, and
//! feeds the cost estimator.
//!
//! Lease state machine (at-least-once):
//! `queued → leased → done`, with `failed / worker-died → requeued`. When a
//! worker dies mid-batch only its in-flight task consumes retry budget — the
//! unstarted remainder returns to the queue front untouched.
//!
//! On parallel(), the requesting worker is SIGSTOP'd, a replacement is
//! spawned, and children enter the ready queue. When all children complete,
//! the original is SIGCONT'd; the pool is then one worker over strength, and
//! the next worker that has nothing leased is stopped.
//!
//! Everything runs on the caller's runtime — no runtime is constructed here.
//! Cancellation is cooperative: `run_phase` returns early when the token
//! fires, and `shutdown` terminates every worker.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::coordinator::{Coordinator, FailureAction, GroupId, ItemId, ItemSpec};
use crate::cost::CostModel;
use crate::events::RunEvent;
use crate::protocol::{CoordinatorMessage, ParallelResult, WorkerMessage, read_frame, write_frame};

// ─── Configuration ───────────────────────────────────────────────────────────

pub struct IoConfig {
    pub python: PathBuf,
    pub pool_size: usize,
    pub run_id: String,
    /// Present only when the Datadog integration successfully configured.
    pub datadog_job: Option<String>,
    /// Local artifact directory workers write to and read from. Always local:
    /// a separate artifact store is synced by `transfer::TransferClient`.
    /// Set explicitly on every worker so env-separated layouts work
    /// regardless of the coordinator's own environment.
    pub artifact_root: String,
    /// Merged fsspec storage options (JSON), forwarded to workers (for remote
    /// `@sink` destinations).
    pub storage_options_json: Option<String>,
}

/// Callback invoked on each step completion with (node_id, artifact_json, attempts made).
/// `Send` so the whole run future can be spawned onto a multi-thread runtime.
pub type StepCallback<'a> = Box<dyn FnMut(&str, &serde_json::Value, u32) + Send + 'a>;

/// Callback invoked with each live [`RunEvent`] as a run progresses.
pub type EventCallback<'a> = Box<dyn FnMut(RunEvent) + Send + 'a>;

/// Called periodically while steps are running, with `(node_id, seconds running)` for each
/// step that has been in flight longer than the progress interval.
pub type RunningHook = Box<dyn FnMut(&[(String, f64)]) + Send + 'static>;

// ─── Worker handle ───────────────────────────────────────────────────────────

struct WorkerHandle {
    child: Child,
    cmd_tx: mpsc::Sender<serde_json::Value>,
    _task: JoinHandle<()>,
    /// Items leased to this worker, in execution order (front = in-flight).
    leases: VecDeque<ItemId>,
    /// When the current front lease became the in-flight item (assignment or the previous
    /// completion). Drives the "still running" progress report.
    front_since: std::time::Instant,
}

// ─── Frozen worker (SIGSTOP'd, waiting for parallel group) ──────────────────

struct FrozenWorker {
    child: Child,
    cmd_tx: mpsc::Sender<serde_json::Value>,
    _task: JoinHandle<()>,
    parent_item: ItemId,
    group_id: GroupId,
    /// The worker_id used when this worker was originally spawned (matches
    /// the worker_io_task's worker_id, so events arrive with this key).
    original_worker_id: usize,
    /// What the worker had leased behind the step that called `parallel()`. It has those
    /// steps in hand (a batch is sent whole) and runs them when it is resumed, so they stay
    /// leased to it: returned to the queue they would run a second time on another worker.
    rest: VecDeque<ItemId>,
    /// Whether the worker reads branch results from their artifacts (`Submit::artifact_results`).
    artifact_results: bool,
}

// ─── Events ──────────────────────────────────────────────────────────────────

enum IoEvent {
    Message {
        worker_id: usize,
        msg: Box<WorkerMessage>,
    },
    Disconnected {
        worker_id: usize,
    },
    /// A retry-backoff timer elapsed — the item may be re-queued.
    RetryReady {
        item_id: ItemId,
    },
}

/// Re-deliver an item to the ready queue after its retry backoff elapses.
/// The sleep runs off the event loop so independent work keeps flowing.
fn schedule_retry(event_tx: &mpsc::Sender<IoEvent>, item_id: ItemId, delay: Duration) {
    let tx = event_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tx.send(IoEvent::RetryReady { item_id }).await;
    });
}

// ─── Worker I/O task ─────────────────────────────────────────────────────────

async fn worker_io_task(
    worker_id: usize,
    mut stream: UnixStream,
    mut cmd_rx: mpsc::Receiver<serde_json::Value>,
    event_tx: mpsc::Sender<IoEvent>,
) {
    loop {
        tokio::select! {
            result = read_frame::<_, WorkerMessage>(&mut stream) => {
                match result {
                    Ok(Some(msg)) => {
                        if event_tx.send(IoEvent::Message { worker_id, msg: Box::new(msg) }).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) | Err(_) => {
                        let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await;
                        break;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(msg) => {
                        if write_frame(&mut stream, &msg).await.is_err() {
                            let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await;
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
}

// ─── Worker pool ─────────────────────────────────────────────────────────────

/// A pool of long-lived Python workers, persistent across the phases of a run.
///
/// Spawned lazily up to `pool_size`, supervised, and replaced on death.
/// Workers keep their interpreter (and imported user modules) warm between
/// phases — the dominant per-spawn cost is `importlib` + heavy imports, not
/// the fork itself.
pub struct WorkerPool {
    config: IoConfig,
    socket_path: PathBuf,
    listener: UnixListener,
    event_tx: mpsc::Sender<IoEvent>,
    event_rx: mpsc::Receiver<IoEvent>,
    workers: HashMap<usize, WorkerHandle>,
    frozen: Vec<FrozenWorker>,
    next_worker_id: usize,
    trace_start: std::time::Instant,
    trace_on: bool,
    /// How long a step must run before it is reported as "still running" (0 = never).
    /// `BARCA_PROGRESS_SECS`, default 15.
    progress_interval: Duration,
    running_hook: Option<RunningHook>,
    /// Library warnings the workers suppressed as repeats: (first line, times suppressed).
    repeated_warnings: Vec<(String, u64)>,
    /// Where this run's `parallel()` branches write their results: see [`branch_results_root`].
    branch_root: PathBuf,
    /// Numbers the run's parallel groups, across phases: each gets a directory of its own.
    next_branch_group: u64,
    /// The result directories of the groups each step started, removed when the step ends.
    branch_dirs: HashMap<ItemId, Vec<PathBuf>>,
}

/// The directory that holds branch results, under the project's `.barca`.
fn branch_results_parent() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".barca")
        .join("branches")
}

/// Where one run's `parallel()` branches write what they return:
/// `.barca/branches/<run id>-<coordinator pid>/<group>/<branch>.<ext>`.
///
/// Branch results used to be written into the artifact directory under a name made of the
/// branch's file, its function and its number within the phase. Two runs of one pipeline at
/// the same time (two runs under one `barca serve`, two `barca` processes in one project)
/// wrote and read the same files, and a caller received results another run's branches had
/// written (#332). The run id keeps runs apart, the group number keeps a run's groups apart
/// (nested ones, and the same call made twice), and the branch number its branches. Nothing
/// else is ever written there, so the whole directory can be removed when the run ends.
///
/// The pid is for the sweep ([`sweep_branch_results`]): it tells whether the run that owns a
/// directory can still be alive.
fn branch_results_root(run_id: &str) -> PathBuf {
    branch_results_parent().join(format!("{run_id}-{}", std::process::id()))
}

/// Remove the branch results of runs whose coordinator is gone (killed, out of memory): a run
/// that ends in any other way removes its own. Like the staging directories of dead workers,
/// they are found by the pid in their name.
fn sweep_branch_results(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let owner = name
            .to_str()
            .and_then(|n| n.rsplit_once('-'))
            .and_then(|(_, pid)| pid.parse::<i64>().ok());
        // A name this code did not make is left alone.
        if owner.is_some_and(|pid| !crate::db::pid_alive(pid)) {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
    // The directory itself stays, empty: another run may be creating its own inside it now.
}

/// Where workers claim "first in this run to print this warning": a directory next to the
/// coordination socket. `python/barca/_dedupe.py` derives the same path from `BARCA_SOCKET`.
fn warning_claims_dir(socket_path: &Path) -> PathBuf {
    let mut dir = socket_path.as_os_str().to_owned();
    dir.push(".warnings");
    PathBuf::from(dir)
}

impl WorkerPool {
    /// Bind the coordination socket. Workers spawn on demand during phases.
    /// Must be called from within a tokio runtime context.
    pub fn start(config: IoConfig) -> Result<Self, String> {
        let socket_path = crate::protocol::socket_path(&config.run_id, "main");
        std::fs::remove_file(&socket_path).ok();
        let listener = UnixListener::bind(&socket_path).map_err(|e| format!("socket bind: {e}"))?;
        // Best effort: without the directory each worker prints a repeated warning once
        // instead of the run printing it once.
        let claims = warning_claims_dir(&socket_path);
        std::fs::remove_dir_all(&claims).ok();
        std::fs::create_dir_all(&claims).ok();
        let (event_tx, event_rx) = mpsc::channel::<IoEvent>(config.pool_size.max(1) * 8);
        sweep_branch_results(&branch_results_parent());
        let branch_root = branch_results_root(&config.run_id);
        Ok(Self {
            config,
            socket_path,
            listener,
            event_tx,
            event_rx,
            workers: HashMap::new(),
            frozen: Vec::new(),
            next_worker_id: 0,
            trace_start: std::time::Instant::now(),
            trace_on: std::env::var("BARCA_TRACE_TIMING").is_ok(),
            progress_interval: Duration::from_secs(
                std::env::var("BARCA_PROGRESS_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(15),
            ),
            running_hook: None,
            repeated_warnings: Vec::new(),
            branch_root,
            next_branch_group: 0,
            branch_dirs: HashMap::new(),
        })
    }

    /// Library warnings the workers printed once and then suppressed, with how many times
    /// each was suppressed, most repeated first. Drains the tally.
    pub fn take_repeated_warnings(&mut self) -> Vec<(String, u64)> {
        let mut out = std::mem::take(&mut self.repeated_warnings);
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Report steps that stay in flight longer than the progress interval, so a long
    /// step does not look like a hang.
    pub fn on_running(&mut self, hook: RunningHook) {
        self.running_hook = Some(hook);
    }

    /// Add the remote parent context without requiring a Python SDK.
    fn traced_step(
        &self,
        item: &crate::coordinator::Item,
        coord: &Coordinator,
    ) -> serde_json::Value {
        let mut step = build_step_json(item, coord);
        if let Some(job) = &self.config.datadog_job {
            use crate::telemetry::datadog::span_id;
            let run = &self.config.run_id;
            // Dynamic parallel items have no synthetic step span in the run report.
            // Attach their Python execution to the nearest reported ancestor.
            let mut parent = item;
            while let Some(group) = parent.group {
                parent = coord.item(coord.group(group).parent);
            }
            step["datadog"] = serde_json::json!({
                "trace_id": span_id(&["trace", run]),
                "parent_id": span_id(&["step", run, &parent.step_id.display()]),
                "run_id": run,
                "job": job,
                "attempt": item.attempts,
            });
        }
        step
    }

    /// `(node_id, seconds)` for every worker's in-flight step older than the interval.
    fn running_steps(&self, coord: &Coordinator) -> Vec<(String, f64)> {
        let mut out: Vec<(String, f64)> = self
            .workers
            .values()
            .filter_map(|w| {
                let &iid = w.leases.front()?;
                let secs = w.front_since.elapsed();
                (secs >= self.progress_interval)
                    .then(|| (coord.item(iid).step_id.display(), secs.as_secs_f64()))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Drive one phase's coordinator to completion against the (persistent)
    /// pool. `cost` supplies batch sizes and absorbs the timings coming back.
    ///
    /// Cancelling `cancel` returns `Err("run cancelled")` promptly; workers
    /// stay alive until [`WorkerPool::shutdown`], which the caller runs on
    /// every exit path.
    pub async fn run_phase(
        &mut self,
        coord: &mut Coordinator,
        cost: &mut CostModel,
        mut on_step: Option<StepCallback<'_>>,
        mut on_event: Option<EventCallback<'_>>,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("run cancelled".to_string());
        }
        self.assign_ready(coord, cost, cancel).await;

        let mut ticker = if self.running_hook.is_some() && !self.progress_interval.is_zero() {
            let mut t = tokio::time::interval(self.progress_interval);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            t.tick().await; // the first tick fires immediately
            Some(t)
        } else {
            None
        };

        loop {
            if coord.is_finished() {
                break;
            }
            // Before the check below: a worker whose start was cancelled is not a pool that
            // failed to start.
            if cancel.is_cancelled() {
                return Err("run cancelled".to_string());
            }
            if self.workers.is_empty() && self.frozen.is_empty() && coord.ready_count() > 0 {
                // Ready work exists but spawning failed — nothing will ever
                // produce a completion event.
                return Err("no workers available and work remains".to_string());
            }

            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    // Cooperative cancellation: the caller's shutdown()
                    // terminates every worker (frozen ones included).
                    return Err("run cancelled".to_string());
                }
                ev = self.event_rx.recv() => match ev {
                    Some(e) => e,
                    None => break,
                },
                _ = async {
                    match ticker.as_mut() {
                        Some(t) => { t.tick().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let running = self.running_steps(coord);
                    if !running.is_empty()
                        && let Some(hook) = self.running_hook.as_mut() {
                            hook(&running);
                        }
                    continue;
                }
            };

            match event {
                IoEvent::Message { worker_id, msg } => match *msg {
                    WorkerMessage::StepCompleted {
                        ref node_id,
                        ref artifact,
                    } => {
                        if self.trace_on {
                            eprintln!(
                                "[trace]  {:>8.1}ms  StepCompleted <- worker {worker_id}: {node_id} (self-reported elapsed={:.1}ms cpu={:.1}ms)",
                                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                                artifact.elapsed_seconds.unwrap_or(0.0) * 1000.0,
                                artifact.cpu_seconds.unwrap_or(0.0) * 1000.0,
                            );
                        }
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            eprintln!(
                                "[barca] StepCompleted for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        // Feed the estimator: within-run adaptation means the
                        // very next pull sizes K from this observation.
                        if let Some(wall) = artifact.elapsed_seconds {
                            cost.observe(
                                node_id,
                                wall,
                                artifact.cpu_seconds.unwrap_or(wall),
                                artifact.max_rss_bytes.unwrap_or(0),
                            );
                        }

                        // Record output and fire progress callback
                        let artifact_val = serde_json::to_value(artifact).unwrap_or_default();
                        coord.record_output(item_id, artifact_val.clone());
                        if let Some(ref mut cb) = on_step {
                            cb(node_id, &artifact_val, coord.item(item_id).attempts);
                        }
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::StepFinished {
                                node_id: node_id.clone(),
                                ok: true,
                                elapsed_seconds: artifact.elapsed_seconds,
                                error: None,
                            });
                        }
                        coord.on_item_completed(item_id);
                        self.drop_branch_results(item_id);

                        // Check if any frozen worker's group is now complete
                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::StepError {
                        ref node_id,
                        error_type,
                        message,
                        traceback,
                        ..
                    } => {
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            eprintln!(
                                "[barca] StepError for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        // Surface the full picture: exception type, message, and
                        // the (barca-frame-filtered) traceback from the worker.
                        let mut error = format!("{error_type}: {message}");
                        if !traceback.trim().is_empty() {
                            error.push('\n');
                            error.push_str(&traceback);
                        }
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::StepFinished {
                                node_id: node_id.clone(),
                                ok: false,
                                elapsed_seconds: None,
                                error: Some(error.clone()),
                            });
                        }
                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(item_id, error)
                        {
                            schedule_retry(&self.event_tx, item_id, delay);
                        }
                        self.drop_branch_results(item_id);

                        // User code failed (or hung past its timeout) in this
                        // process — kill it so retries and remaining work get a
                        // fresh interpreter. Unstarted leases go back to the
                        // queue front; the 200ms SIGTERM grace runs off the
                        // event loop.
                        if let Some(mut handle) = self.workers.remove(&worker_id) {
                            Self::return_leases(&mut handle, coord);
                            tokio::task::spawn_blocking(move || {
                                graceful_kill(&mut handle.child);
                            });
                        }

                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Blocked {
                        ref node_id,
                        reason,
                    } => {
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            eprintln!(
                                "[barca] Blocked for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(item_id, format!("blocked: {reason}"))
                        {
                            schedule_retry(&self.event_tx, item_id, delay);
                        }

                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Submit {
                        items,
                        artifact_results,
                    } => {
                        let Some(handle) = self.workers.get(&worker_id) else {
                            eprintln!("[barca] Submit from unknown worker {worker_id}, ignoring");
                            continue;
                        };
                        let Some(&item_id) = handle.leases.front() else {
                            eprintln!(
                                "[barca] Submit from worker {worker_id} with no lease, ignoring"
                            );
                            continue;
                        };

                        let specs: Vec<ItemSpec> = items
                            .into_iter()
                            .map(|si| {
                                let (source_file, function_name) = si
                                    .fn_ref
                                    .rsplit_once(':')
                                    .map(|(f, n)| (f.to_string(), n.to_string()))
                                    .unwrap_or_else(|| (String::new(), si.fn_ref.clone()));
                                ItemSpec {
                                    fn_ref: si.fn_ref,
                                    function_name,
                                    source_file,
                                    direct_args: si.args,
                                    direct_kwargs: si.kwargs,
                                    dag_inputs: HashMap::new(),
                                    timeout_seconds: 300,
                                    retries: 1,
                                    retry_backoff_seconds: 0.0,
                                    serializer: None,
                                    sinks: Vec::new(),
                                    run_hash: None,
                                    upstream_inputs: HashMap::new(),
                                    collected_inputs: HashMap::new(),
                                    param_types: HashMap::new(),
                                    return_type: None,
                                    kind: "task".to_string(),
                                    is_dynamic: false,
                                }
                            })
                            .collect();

                        let (group_id, _child_ids) = coord.on_parallel_requested(item_id, specs);
                        // The group's own directory for what its branches return, removed
                        // when the step that called parallel() ends (`drop_branch_results`).
                        let result_dir = self.branch_root.join(self.next_branch_group.to_string());
                        self.next_branch_group += 1;
                        if let Err(e) = std::fs::create_dir_all(&result_dir) {
                            eprintln!("[barca] could not create {}: {e}", result_dir.display());
                        }
                        coord.set_group_result_dir(group_id, result_dir.clone());
                        self.branch_dirs
                            .entry(item_id)
                            .or_default()
                            .push(result_dir);

                        // The parent blocks frozen on its group. Whatever else this
                        // worker had leased stays leased to it (see `FrozenWorker::rest`).
                        let mut handle = self
                            .workers
                            .remove(&worker_id)
                            .expect("worker existence checked above");
                        handle.leases.pop_front();
                        let rest = std::mem::take(&mut handle.leases);

                        // SIGSTOP the requesting worker, move it to frozen list
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(handle.child.id() as i32, libc::SIGSTOP);
                        }

                        // Spawn a replacement worker in the same slot
                        let replacement_id = self.next_worker_id;
                        self.next_worker_id += 1;
                        match spawn_worker(
                            &self.config,
                            &self.socket_path,
                            replacement_id,
                            &self.listener,
                            &self.event_tx,
                            cancel,
                        )
                        .await
                        {
                            Ok(replacement) => {
                                self.workers.insert(replacement_id, replacement);
                            }
                            Err(SpawnError::Cancelled) => {}
                            Err(SpawnError::Failed(e)) => {
                                eprintln!("[barca] failed to spawn replacement worker: {e}")
                            }
                        }

                        self.frozen.push(FrozenWorker {
                            child: handle.child,
                            cmd_tx: handle.cmd_tx,
                            _task: handle._task,
                            parent_item: item_id,
                            group_id,
                            original_worker_id: worker_id,
                            rest,
                            artifact_results,
                        });

                        // Assign ready items (children are now in the ready queue)
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Heartbeat => {}
                    WorkerMessage::Log { node_id, line } => {
                        // A line of user stdout — forward live; the caller persists
                        // it to the DB. Does not change worker/coordinator state.
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::Log { node_id, line });
                        }
                    }
                    WorkerMessage::RepeatedWarnings { counts } => {
                        for (text, n) in counts {
                            match self.repeated_warnings.iter_mut().find(|(t, _)| *t == text) {
                                Some((_, total)) => *total += n,
                                None => self.repeated_warnings.push((text, n)),
                            }
                        }
                    }
                },
                IoEvent::Disconnected { worker_id } => {
                    // Worker crashed — its in-flight item failed; unstarted
                    // leases return to the queue for another worker.
                    if let Some(mut handle) = self.workers.remove(&worker_id) {
                        if let Some(in_flight) = handle.leases.pop_front() {
                            if let FailureAction::RetryAfter(delay) =
                                coord.on_item_failed(in_flight, "worker disconnected".to_string())
                            {
                                schedule_retry(&self.event_tx, in_flight, delay);
                            }
                            self.drop_branch_results(in_flight);
                        }
                        Self::return_leases(&mut handle, coord);
                        tokio::task::spawn_blocking(move || {
                            let _ = handle.child.kill();
                            let _ = handle.child.wait();
                        });
                    } else if let Some(at) = self
                        .frozen
                        .iter()
                        .position(|fw| fw.original_worker_id == worker_id)
                    {
                        // A worker that died while frozen in parallel() (killed, out of
                        // memory). It is a dead worker like any other: its step failed. This
                        // used to go unnoticed, since only running workers were looked up,
                        // and when its branches finished there was nobody to resume: the run
                        // never ended (#333).
                        let mut fw = self.frozen.swap_remove(at);
                        coord.abandon_group(fw.group_id);
                        if let FailureAction::RetryAfter(delay) = coord.on_item_failed(
                            fw.parent_item,
                            "worker disconnected while it waited for its parallel() branches"
                                .to_string(),
                        ) {
                            schedule_retry(&self.event_tx, fw.parent_item, delay);
                        }
                        self.drop_branch_results(fw.parent_item);
                        // The rest of its batch never started and nobody has it in hand now.
                        while let Some(item_id) = fw.rest.pop_back() {
                            coord.return_leased(item_id);
                        }
                        tokio::task::spawn_blocking(move || {
                            let _ = fw.child.kill();
                            let _ = fw.child.wait();
                        });
                    }
                    self.resume_frozen(coord).await;
                    self.assign_ready(coord, cost, cancel).await;
                }
                IoEvent::RetryReady { item_id } => {
                    coord.requeue(item_id);
                    self.assign_ready(coord, cost, cancel).await;
                }
            }
        }

        Ok(())
    }

    /// Gracefully terminate every worker and remove the socket. Runs on the
    /// blocking pool because this polls for exit between SIGTERM and SIGKILL.
    ///
    /// Signals every worker up front and then polls them all together for one
    /// shared 200ms grace window, rather than the previous sequential
    /// SIGTERM-then-sleep(200ms)-then-check per worker — that cost
    /// `pool_size * 200ms` in the common case (every worker still mid-cleanup
    /// at the first `try_wait()`, right after its own SIGTERM), which for a
    /// default `pool_size` of 4 meant shutdown alone could take ~800ms
    /// regardless of how little work the run actually did (confirmed via
    /// `BARCA_TRACE_TIMING=1` — see benchmarks/RESULTS.md's docker-harness
    /// re-run notes).
    pub async fn shutdown(self) {
        let workers = self.workers;
        let frozen = self.frozen;
        let kill_task = tokio::task::spawn_blocking(move || {
            let mut children: Vec<Child> = Vec::with_capacity(workers.len() + frozen.len());

            #[cfg(unix)]
            for (_, w) in workers {
                unsafe {
                    libc::kill(w.child.id() as i32, libc::SIGTERM);
                }
                children.push(w.child);
            }
            #[cfg(not(unix))]
            for (_, w) in workers {
                children.push(w.child);
            }

            #[cfg(unix)]
            for fw in frozen {
                unsafe {
                    libc::kill(fw.child.id() as i32, libc::SIGCONT);
                    libc::kill(fw.child.id() as i32, libc::SIGTERM);
                }
                children.push(fw.child);
            }
            #[cfg(not(unix))]
            for fw in frozen {
                children.push(fw.child);
            }

            #[cfg(unix)]
            {
                let deadline = std::time::Instant::now() + Duration::from_millis(200);
                while std::time::Instant::now() < deadline
                    && children
                        .iter_mut()
                        .any(|c| !matches!(c.try_wait(), Ok(Some(_))))
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            for mut c in children {
                if !matches!(c.try_wait(), Ok(Some(_))) {
                    let _ = c.kill();
                }
                let _ = c.wait();
            }
        });
        let _ = kill_task.await;
        // Every worker is gone, so nothing writes or reads a branch result any more: the
        // run's directory goes, whatever way the run ended.
        let branch_root = self.branch_root;
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::remove_dir_all(&branch_root).ok();
        })
        .await;
        std::fs::remove_file(&self.socket_path).ok();
        std::fs::remove_dir_all(warning_claims_dir(&self.socket_path)).ok();
    }

    /// Close a worker's lease for `node_id`. Workers execute their batch in
    /// order, so this is normally the front of the deque; matching by node id
    /// keeps us honest if a worker ever reports out of order.
    fn take_lease(
        &mut self,
        worker_id: usize,
        node_id: &str,
        coord: &Coordinator,
    ) -> Option<ItemId> {
        let handle = self.workers.get_mut(&worker_id)?;
        let pos = handle
            .leases
            .iter()
            .position(|&iid| coord.item(iid).step_id.display() == node_id)?;
        let taken = handle.leases.remove(pos);
        handle.front_since = std::time::Instant::now();
        taken
    }

    /// Return every unstarted lease on a dead/frozen worker to the queue front.
    /// Popping from the back preserves the original order across push_fronts.
    fn return_leases(handle: &mut WorkerHandle, coord: &mut Coordinator) {
        while let Some(item_id) = handle.leases.pop_back() {
            coord.return_leased(item_id);
        }
    }

    /// Assign ready items to idle workers in cost-sized batches, spawning
    /// workers on demand up to `pool_size`.
    ///
    /// Workers are spawned one at a time, not concurrently: `spawn_worker`
    /// pairs a spawned child process with whichever connection its own
    /// `listener.accept()` call happens to receive, and workers never send an
    /// identifying handshake after connecting (see `_runtime.connect()` in
    /// python/barca/_runtime.py — a bare `sock.connect()`, nothing else). Two
    /// concurrent `spawn_worker` calls racing on the same listener could each
    /// accept the *other's* connection, pairing a `WorkerHandle`'s `Child`
    /// (kill target) with a socket that's actually talking to a different
    /// process — a real process-lifecycle bug, not just a cosmetic ID swap.
    /// Fixing this properly needs a handshake protocol change; not worth it
    /// for the ~200ms/run this would save.
    async fn assign_ready(
        &mut self,
        coord: &mut Coordinator,
        cost: &CostModel,
        cancel: &CancellationToken,
    ) {
        self.retire_surplus();
        loop {
            // A cancelled run starts nothing more: no worker, no step.
            if coord.ready_count() == 0 || cancel.is_cancelled() {
                return;
            }

            // Find an idle worker, or spawn one if the pool is under strength.
            let wid = match self
                .workers
                .iter()
                .find(|(_, w)| w.leases.is_empty())
                .map(|(&id, _)| id)
            {
                Some(id) => id,
                None => {
                    if self.workers.len() >= self.config.pool_size.max(1) {
                        return; // pool saturated — completions will re-enter here
                    }
                    let wid = self.next_worker_id;
                    self.next_worker_id += 1;
                    match spawn_worker(
                        &self.config,
                        &self.socket_path,
                        wid,
                        &self.listener,
                        &self.event_tx,
                        cancel,
                    )
                    .await
                    {
                        Ok(handle) => {
                            self.workers.insert(wid, handle);
                            wid
                        }
                        Err(SpawnError::Cancelled) => return,
                        Err(SpawnError::Failed(e)) => {
                            eprintln!("[barca] failed to spawn worker: {e}");
                            return;
                        }
                    }
                }
            };

            // Lease a batch: K sized from the head item's measured cost.
            // The ceiling input is the pull-eligible pool (ready items), not
            // pending work blocked on upstreams — items that can't be pulled
            // this wave mustn't let one worker drain the whole ready queue.
            let first = coord.next_ready().expect("ready_count checked above");
            let head_node = coord.item(first).step_id.display();
            let remaining = coord.ready_count() + 1;
            let k = cost
                .batch_size(&head_node, remaining, self.config.pool_size.max(1))
                .max(1);
            // Fill the batch up to K, but never pack estimated-heavy items
            // behind a light head: the batch has a work budget of K × the
            // head's cost (what K was computed for), and an item that would
            // blow it goes back for its own pull. Guards the over-batch
            // tail-block when a phase mixes light and heavy nodes.
            let head_est = cost.estimate(&head_node);
            let budget = head_est * k as f64 * 1.5;
            let mut acc = head_est;
            let mut batch = vec![first];
            while batch.len() < k {
                match coord.next_ready() {
                    Some(id) => {
                        let est = cost.estimate(&coord.item(id).step_id.display());
                        if acc + est > budget {
                            coord.return_leased(id);
                            break;
                        }
                        acc += est;
                        batch.push(id);
                    }
                    None => break,
                }
            }

            let msg = if batch.len() == 1 {
                let step = self.traced_step(coord.item(batch[0]), coord);
                serde_json::json!({"type": "execute", "step": step})
            } else {
                let steps: Vec<serde_json::Value> = batch
                    .iter()
                    .map(|&iid| self.traced_step(coord.item(iid), coord))
                    .collect();
                serde_json::json!({"type": "execute_batch", "steps": steps})
            };

            if self.trace_on {
                let names: Vec<String> = batch
                    .iter()
                    .map(|&iid| coord.item(iid).step_id.display())
                    .collect();
                eprintln!(
                    "[trace]  {:>8.1}ms  dispatch -> worker {wid}: {names:?}",
                    self.trace_start.elapsed().as_secs_f64() * 1000.0
                );
            }

            let handle = self
                .workers
                .get_mut(&wid)
                .expect("worker inserted or found above");
            if handle.cmd_tx.send(msg).await.is_err() {
                // Worker's I/O task is gone — undo the lease and drop the
                // worker; its Disconnected event does no further harm.
                for &item_id in batch.iter().rev() {
                    coord.return_leased(item_id);
                }
                if let Some(mut dead) = self.workers.remove(&wid) {
                    tokio::task::spawn_blocking(move || {
                        let _ = dead.child.kill();
                        let _ = dead.child.wait();
                    });
                }
                continue;
            }
            handle.leases = batch.into_iter().collect();
            handle.front_since = std::time::Instant::now();
        }
    }

    /// Remove what the branches of `item_id`'s parallel groups returned. Called when the step
    /// has ended (finished, failed, or its worker died): it has read every result it was
    /// going to read, lazily loaded frames included, since its own result is written by then.
    fn drop_branch_results(&mut self, item_id: ItemId) {
        if let Some(dirs) = self.branch_dirs.remove(&item_id) {
            tokio::task::spawn_blocking(move || {
                for dir in dirs {
                    std::fs::remove_dir_all(dir).ok();
                }
            });
        }
    }

    /// Stop workers that have nothing leased while the pool is over strength.
    ///
    /// A worker that calls `parallel()` is frozen and another is started in its place. When
    /// it is resumed there is one worker more than `pool_size`. The one that goes is whichever
    /// has no step leased, now or when it next finishes its batch: never one in the middle of
    /// a step.
    fn retire_surplus(&mut self) {
        while self.workers.len() > self.config.pool_size.max(1) {
            let Some(idle) = self
                .workers
                .iter()
                .find(|(_, w)| w.leases.is_empty())
                .map(|(&id, _)| id)
            else {
                return;
            };
            if let Some(mut handle) = self.workers.remove(&idle) {
                tokio::task::spawn_blocking(move || {
                    let _ = handle.child.kill();
                    let _ = handle.child.wait();
                });
            }
        }
    }

    /// Resume frozen workers whose parallel groups completed.
    async fn resume_frozen(&mut self, coord: &mut Coordinator) {
        // Partition: completed groups get drained out
        let mut i = 0;
        while i < self.frozen.len() {
            if coord.is_group_complete(self.frozen[i].group_id) {
                let fw = self.frozen.swap_remove(i);

                // The worker started in this one's place is not stopped here. It may be in
                // the middle of a step, and it may itself be the parent of a group by now:
                // killing it lost that step for good (nothing ran it again, and the run never
                // ended). The pool is over strength by one until a worker has nothing leased
                // (`retire_surplus`).

                // SIGCONT the original worker
                #[cfg(unix)]
                unsafe {
                    libc::kill(fw.child.id() as i32, libc::SIGCONT);
                }

                // Tell the parent how each branch ended (`branch_result`).
                let group = coord.group(fw.group_id);
                let failed: HashMap<ItemId, &str> = coord.failed_items().into_iter().collect();
                let results: Vec<ParallelResult> = group
                    .items
                    .iter()
                    .map(|iid| {
                        let outcome = if coord.is_done(*iid) {
                            Ok(coord.outputs().get(iid))
                        } else {
                            Err(failed.get(iid).copied().unwrap_or("failed"))
                        };
                        branch_result(outcome, fw.artifact_results)
                    })
                    .collect();
                let response = CoordinatorMessage::ParallelResponse { results };
                let msg = serde_json::to_value(&response).unwrap_or_default();
                let _ = fw.cmd_tx.send(msg).await;

                // Re-insert using the original worker_id — the worker_io_task
                // was spawned with this ID, so StepCompleted events arrive keyed
                // by it.
                self.workers.insert(
                    fw.original_worker_id,
                    WorkerHandle {
                        child: fw.child,
                        cmd_tx: fw.cmd_tx,
                        _task: fw._task,
                        // Still executing the parent task, with the rest of its batch
                        // behind it.
                        leases: std::iter::once(fw.parent_item).chain(fw.rest).collect(),
                        front_since: std::time::Instant::now(),
                    },
                );
                // Don't increment i — swap_remove moved the last element here
            } else {
                i += 1;
            }
        }
    }
}

// ─── Worker spawning ─────────────────────────────────────────────────────────

/// Why a worker did not start.
enum SpawnError {
    /// The run was cancelled while the worker was starting. The worker has been stopped.
    Cancelled,
    Failed(String),
}

/// How long a worker may take from being started to connecting.
const WORKER_CONNECT_LIMIT: Duration = Duration::from_secs(10);

/// Start one worker and wait for it to connect ([`start_worker`]).
///
/// The wait ends as soon as it cannot succeed: when the run is cancelled, and when the worker
/// process exits without connecting. It used to end only on the connection or after
/// [`WORKER_CONNECT_LIMIT`], so a Ctrl-C that ended a starting worker held the cancelled
/// command for those ten seconds (#292). A worker that does not become part of the pool is
/// killed and reaped here: none is left behind.
async fn spawn_worker(
    config: &IoConfig,
    socket_path: &Path,
    worker_id: usize,
    listener: &UnixListener,
    event_tx: &mpsc::Sender<IoEvent>,
    cancel: &CancellationToken,
) -> Result<WorkerHandle, SpawnError> {
    let mut cmd = Command::new(&config.python);
    cmd.args(["-m", "barca._worker", "--daemon"])
        .env("BARCA_SOCKET", socket_path.to_str().unwrap_or(""))
        .env("BARCA_WORKER", "1")
        .env("BARCA_WORKER_ID", worker_id.to_string())
        .env("BARCA_ARTIFACT_URI", &config.artifact_root)
        // A step's own print() output goes to barca's stderr, never stdout: stdout carries
        // only barca's result, so `barca run ... | jq` works when steps print.
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit())
        .stdin(Stdio::null());
    if let Some(ref opts) = config.storage_options_json {
        cmd.env("BARCA_STORAGE_OPTIONS", opts);
    }
    // Ctrl-C means something to a worker only while it runs a step. It starts outside the
    // terminal's job, so that one arriving while the interpreter starts is not a traceback,
    // and joins the job once it handles the signal itself.
    #[cfg(unix)]
    crate::helper_proc::start_outside_the_job(&mut cmd);
    start_worker(cmd, worker_id, listener, event_tx, cancel).await
}

/// Start `cmd` as worker `worker_id` and wait for it to connect to `listener`.
async fn start_worker(
    mut cmd: Command,
    worker_id: usize,
    listener: &UnixListener,
    event_tx: &mpsc::Sender<IoEvent>,
    cancel: &CancellationToken,
) -> Result<WorkerHandle, SpawnError> {
    let trace_on = std::env::var("BARCA_TRACE_TIMING").is_ok();
    let t_spawn = std::time::Instant::now();
    let mut child = crate::helper_proc::spawn_std(&mut cmd)
        .map_err(|e| SpawnError::Failed(format!("spawn: {e}")))?;
    if trace_on {
        eprintln!(
            "[trace]  worker {worker_id} process spawned in {:.1}ms",
            t_spawn.elapsed().as_secs_f64() * 1000.0
        );
    }

    let t_accept = std::time::Instant::now();
    let connected = tokio::select! {
        // A connection that is there is taken, whatever else is also true by now.
        biased;
        accepted = tokio::time::timeout(WORKER_CONNECT_LIMIT, listener.accept()) => match accepted {
            Ok(Ok((stream, _))) => Ok(stream),
            Ok(Err(e)) => Err(SpawnError::Failed(format!("accept: {e}"))),
            Err(_) => Err(SpawnError::Failed(format!(
                "timeout waiting for worker {worker_id} to connect"
            ))),
        },
        _ = cancel.cancelled() => Err(SpawnError::Cancelled),
        status = exited(&mut child) => Err(SpawnError::Failed(format!(
            "worker {worker_id} exited before it connected ({status})"
        ))),
    };
    let stream = match connected {
        Ok(stream) => stream,
        Err(e) => {
            // Not a member of the pool, so `shutdown` would never see it.
            tokio::task::spawn_blocking(move || {
                let _ = child.kill();
                let _ = child.wait();
            })
            .await
            .ok();
            return Err(e);
        }
    };
    if trace_on {
        eprintln!(
            "[trace]  worker {worker_id} connected (accept) in {:.1}ms",
            t_accept.elapsed().as_secs_f64() * 1000.0
        );
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<serde_json::Value>(16);
    let etx = event_tx.clone();
    let task = tokio::spawn(worker_io_task(worker_id, stream, cmd_rx, etx));

    Ok(WorkerHandle {
        child,
        cmd_tx,
        _task: task,
        leases: VecDeque::new(),
        front_since: std::time::Instant::now(),
    })
}

/// Resolves when `child` has exited, with its exit status as text.
async fn exited(child: &mut Child) -> String {
    let mut poll = tokio::time::interval(Duration::from_millis(10));
    loop {
        poll.tick().await;
        match child.try_wait() {
            Ok(Some(status)) => return status.to_string(),
            Ok(None) => {}
            Err(e) => return format!("its status could not be read: {e}"),
        }
    }
}

// ─── Process lifecycle ────────────────────────────────────────────────────────

/// Gracefully terminate a child process. Sends SIGTERM first to let the process
/// flush buffered stdout/stderr, then falls back to SIGKILL after a brief wait.
fn graceful_kill(child: &mut Child) {
    #[cfg(unix)]
    {
        // Send SIGTERM for a graceful shutdown.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        // Give the process a moment to flush and exit.
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
    }
    // Fallback: SIGKILL (or platform kill on non-unix).
    let _ = child.kill();
    let _ = child.wait();
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn build_step_json(item: &crate::coordinator::Item, coord: &Coordinator) -> serde_json::Value {
    // Start with dag_inputs from spec (cross-phase provided inputs), as plain
    // artifact-path strings.
    let mut inputs: serde_json::Map<String, serde_json::Value> = item
        .spec
        .dag_inputs
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    // Fill in-phase upstream artifacts: for params that don't have a value yet
    // (and aren't a fan-in collected param, filled below), look up the
    // coordinator's outputs using the upstream_inputs mapping.
    for (param, upstream_node_id) in &item.spec.upstream_inputs {
        if item.spec.collected_inputs.contains_key(param) {
            continue;
        }
        if inputs.get(param).is_some_and(|v| v.as_str() != Some("")) {
            continue;
        }
        for (&uid, artifact) in coord.outputs() {
            let upstream_item = coord.item(uid);
            if upstream_item.step_id.display() == *upstream_node_id {
                let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if !path.is_empty() {
                    inputs.insert(param.clone(), serde_json::Value::String(path.to_string()));
                }
                break;
            }
        }
    }

    // Fan-in (`collect()`) params: every partition artifact of the upstream,
    // sent as a `{"_collected": true, "artifacts": [...]}` marker — the same
    // shape batch mode's `_resolve_input` already understands — so the worker
    // deserializes the full list instead of a single artifact.
    for (param, orefs) in &item.spec.collected_inputs {
        let artifacts: Vec<serde_json::Value> = orefs
            .iter()
            .map(|oref| serde_json::json!({"path": oref.path, "format": oref.format}))
            .collect();
        inputs.insert(
            param.clone(),
            serde_json::json!({"_collected": true, "artifacts": artifacts}),
        );
    }

    serde_json::json!({
        "node_id": item.step_id.display(),
        "function_name": item.spec.function_name,
        "source_file": item.spec.source_file,
        "kind": &item.spec.kind,
        "inputs": inputs,
        "timeout_seconds": item.spec.timeout_seconds,
        "direct_args": item.spec.direct_args,
        "direct_kwargs": item.spec.direct_kwargs,
        "serializer": item.spec.serializer.as_deref(),
        "sinks": item.spec.sinks,
        "run_hash": item.spec.run_hash,
        "param_types": item
            .spec
            .param_types
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str()))
            .collect::<HashMap<String, &str>>(),
        "return_type": item.spec.return_type.map(|t| t.as_str()),
        // A `parallel()` branch: its result goes back to the step that called it.
        "branch": item.group.is_some(),
        // Where the branch writes that result, without the extension: the worker adds the one
        // for the format it picks.
        "branch_path": item
            .group
            .and_then(|g| coord.group(g).result_dir.as_ref())
            .map(|dir| dir.join(item.id.0.to_string())),
    })
}

/// What the step that called `parallel()` is told about one branch.
///
/// `outcome` is the branch's recorded artifact when it finished, or its error when it failed.
///
/// A branch that finished is answered with where its artifact is, and the calling worker
/// reads it with the code that reads any step's input: whatever a step can return, a branch
/// can return. (A small JSON result has no file: the branch's worker sent its text, which is
/// passed on as it is.) Nothing of a result is read or interpreted here. The coordinator used to read the file itself and pass the value inline, which
/// it could do for JSON only: any other value (a set, a date, a DataFrame: written as pickle
/// or parquet) reached the caller as `None`, with no error (#285), and a JSON value went
/// through this process's JSON types on the way (an integer beyond 64 bits arrived as a
/// float, a dict with its keys sorted, a NaN as `None`).
///
/// A worker that did not ask for artifact results (a barca Python package from before this
/// change, run by this binary) still gets JSON values inline. A value that cannot be passed
/// that way is an error for that worker, not `None`.
fn branch_result(
    outcome: Result<Option<&serde_json::Value>, &str>,
    artifact_results: bool,
) -> ParallelResult {
    let artifact = match outcome {
        Ok(artifact) => artifact,
        Err(error) => {
            return ParallelResult::Error {
                error: error.to_string(),
            };
        }
    };
    let field = |name: &str| {
        artifact
            .and_then(|a| a.get(name))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
    };
    // A small JSON result came in the branch's report and has no file (`ArtifactRef::json`).
    let text = artifact
        .and_then(|a| a.get("json"))
        .and_then(|v| v.as_str());
    let Some(format) = field("format") else {
        return ParallelResult::Error {
            error: "the branch finished but did not report its result".to_string(),
        };
    };
    let path = field("path");
    if path.is_none() && text.is_none() {
        return ParallelResult::Error {
            error: "the branch finished but did not report where its result is".to_string(),
        };
    }
    if artifact_results {
        return ParallelResult::Ok {
            result: None,
            artifact: Some(crate::protocol::BranchArtifact {
                path: path.unwrap_or_default().to_string(),
                format: format.to_string(),
                frame_type: field("frame_type").map(str::to_string),
                json: text.map(str::to_string),
            }),
        };
    }
    let path = path.unwrap_or_default();
    let inline = if format == "json" {
        std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())
            })
    } else {
        Err(format!("it was written as {format}, not JSON"))
    };
    match inline {
        Ok(value) => ParallelResult::Ok {
            result: Some(value),
            artifact: None,
        },
        Err(why) => ParallelResult::Error {
            error: format!(
                "the branch's return value cannot be passed to this worker ({why}). The barca \
                 Python package this worker runs is older than the barca command; install \
                 matching versions."
            ),
        },
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::ItemSpec;
    use std::collections::HashMap;

    #[test]
    fn build_step_json_includes_sinks() {
        let mut coord = Coordinator::new();
        let spec = ItemSpec {
            fn_ref: "f.py:a".to_string(),
            function_name: "a".to_string(),
            source_file: "f.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            serializer: Some("parquet".to_string()),
            run_hash: Some("abc123".to_string()),
            sinks: vec![
                crate::model::SinkDecl {
                    path: "abfss://cont@acct.dfs.core.windows.net/exports/a.parquet".to_string(),
                    serializer: Some(crate::model::SerializerKind::Parquet),
                },
                crate::model::SinkDecl {
                    path: "exports/a.pkl".to_string(),
                    serializer: None,
                },
            ],
            upstream_inputs: HashMap::new(),
            collected_inputs: HashMap::new(),
            param_types: HashMap::new(),
            return_type: None,
            kind: "asset".to_string(),
            is_dynamic: false,
        };
        let id = coord.add_item(crate::StepId::unpartitioned("f.py:a"), spec, Vec::new());
        let step = build_step_json(coord.item(id), &coord);

        assert_eq!(
            step["sinks"],
            serde_json::json!([
                {"path": "abfss://cont@acct.dfs.core.windows.net/exports/a.parquet", "serializer": "parquet"},
                {"path": "exports/a.pkl", "serializer": null},
            ])
        );
        assert_eq!(step["serializer"], serde_json::json!("parquet"));
    }

    #[test]
    fn build_step_json_includes_param_types_from_spec() {
        use crate::model::ValueType;

        let mut coord = Coordinator::new();
        let spec = ItemSpec {
            fn_ref: "f.py:downstream".to_string(),
            function_name: "downstream".to_string(),
            source_file: "f.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            serializer: None,
            sinks: Vec::new(),
            run_hash: None,
            upstream_inputs: HashMap::from([("orders".to_string(), "f.py:upstream".to_string())]),
            collected_inputs: HashMap::new(),
            param_types: HashMap::from([("orders".to_string(), ValueType::Polars)]),
            return_type: Some(ValueType::Polars),
            kind: "asset".to_string(),
            is_dynamic: false,
        };
        let id = coord.add_item(
            crate::StepId::unpartitioned("f.py:downstream"),
            spec,
            Vec::new(),
        );
        let step = build_step_json(coord.item(id), &coord);

        assert_eq!(step["param_types"]["orders"], "polars");
        assert_eq!(step["return_type"], "polars");
    }

    #[test]
    fn build_step_json_empty_sinks_serializes_as_empty_array() {
        let mut coord = Coordinator::new();
        let spec = ItemSpec {
            fn_ref: "f.py:a".to_string(),
            function_name: "a".to_string(),
            source_file: "f.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            serializer: None,
            sinks: Vec::new(),
            run_hash: None,
            upstream_inputs: HashMap::new(),
            collected_inputs: HashMap::new(),
            param_types: HashMap::new(),
            return_type: None,
            kind: "asset".to_string(),
            is_dynamic: false,
        };
        let id = coord.add_item(crate::StepId::unpartitioned("f.py:a"), spec, Vec::new());
        let step = build_step_json(coord.item(id), &coord);
        assert_eq!(step["sinks"], serde_json::json!([]));
        assert_eq!(step["run_hash"], serde_json::Value::Null);
    }

    /// Regression test for #93: a `collect()` param must be sent to the
    /// worker as the `{"_collected": true, "artifacts": [...]}` marker with
    /// every partition artifact, not collapsed to a single path.
    #[test]
    fn build_step_json_sends_collected_param_as_artifact_list() {
        let mut coord = Coordinator::new();
        let mut spec = ItemSpec {
            fn_ref: "f.py:sink".to_string(),
            function_name: "sink".to_string(),
            source_file: "f.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            serializer: None,
            sinks: Vec::new(),
            run_hash: None,
            upstream_inputs: HashMap::from([("data".to_string(), "f.py:source".to_string())]),
            collected_inputs: HashMap::new(),
            param_types: HashMap::new(),
            return_type: None,
            kind: "asset".to_string(),
            is_dynamic: false,
        };
        spec.collected_inputs.insert(
            "data".to_string(),
            vec![
                crate::dispatch::OutputRef {
                    path: "f--source_key_a.json".to_string(),
                    format: "json".to_string(),
                    size_bytes: 10,
                    elapsed_seconds: None,
                    content_hash: None,
                },
                crate::dispatch::OutputRef {
                    path: "f--source_key_b.json".to_string(),
                    format: "json".to_string(),
                    size_bytes: 12,
                    elapsed_seconds: None,
                    content_hash: None,
                },
            ],
        );
        let id = coord.add_item(crate::StepId::unpartitioned("f.py:sink"), spec, Vec::new());
        let step = build_step_json(coord.item(id), &coord);

        assert_eq!(
            step["inputs"]["data"],
            serde_json::json!({
                "_collected": true,
                "artifacts": [
                    {"path": "f--source_key_a.json", "format": "json"},
                    {"path": "f--source_key_b.json", "format": "json"},
                ],
            })
        );
    }

    /// A small JSON result arrives as text in the branch's report and is passed on exactly
    /// as it is: nothing here parses it (this text is not JSON a Rust parser accepts, and its
    /// key order is kept), and there is no file to read.
    #[test]
    fn a_small_json_branch_result_is_passed_on_as_text_unparsed() {
        let text = r#"{"b": 1, "a": NaN, "n": 1000000000000000000000000000000}"#;
        let reported = serde_json::json!({
            "path": "", "format": "json", "size_bytes": text.len(), "json": text
        });
        let ParallelResult::Ok {
            result: None,
            artifact: Some(sent),
        } = branch_result(Ok(Some(&reported)), true)
        else {
            panic!("expected an artifact answer");
        };
        assert_eq!(sent.json.as_deref(), Some(text));
        assert_eq!((sent.path.as_str(), sent.format.as_str()), ("", "json"));

        // Neither a path nor a text: an error, not a null.
        let nothing = serde_json::json!({"path": "", "format": "json", "size_bytes": 0});
        assert!(matches!(
            branch_result(Ok(Some(&nothing)), true),
            ParallelResult::Error { .. }
        ));
    }

    /// #285: a finished branch is answered with its artifact, whatever its format, and the
    /// coordinator does not read a file it could not pass on as it is.
    #[test]
    fn a_finished_branch_is_answered_with_where_its_artifact_is() {
        for (format, frame_type) in [
            ("json", None),
            ("pickle", None),
            ("parquet", Some("polars")),
        ] {
            let mut artifact = serde_json::json!({
                "path": "/nowhere/branch.out", "format": format, "size_bytes": 3
            });
            if let Some(t) = frame_type {
                artifact["frame_type"] = serde_json::json!(t);
            }
            let ParallelResult::Ok { result, artifact } = branch_result(Ok(Some(&artifact)), true)
            else {
                panic!("a finished {format} branch must be ok");
            };
            assert_eq!(result, None);
            assert_eq!(
                artifact,
                Some(crate::protocol::BranchArtifact {
                    path: "/nowhere/branch.out".to_string(),
                    format: format.to_string(),
                    frame_type: frame_type.map(str::to_string),
                    json: None,
                })
            );
        }
    }

    /// A worker from an older Python package gets JSON values inline as before, and an error
    /// (where it got `None`) for a value that cannot be passed that way.
    #[test]
    fn a_worker_that_did_not_ask_for_artifacts_gets_json_inline_or_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("a.json");
        std::fs::write(&json, r#"{"i": 1}"#).unwrap();
        let artifact = serde_json::json!({"path": json.to_str().unwrap(), "format": "json"});
        let ParallelResult::Ok { result, artifact } = branch_result(Ok(Some(&artifact)), false)
        else {
            panic!("a JSON result is passed inline");
        };
        assert_eq!(result, Some(serde_json::json!({"i": 1})));
        assert_eq!(artifact, None);

        let pickle = serde_json::json!({"path": "/nowhere/a.pkl", "format": "pickle"});
        let ParallelResult::Error { error } = branch_result(Ok(Some(&pickle)), false) else {
            panic!("a pickled result must not become a silent null");
        };
        assert!(
            error.contains("written as pickle") && error.contains("older than"),
            "{error}"
        );

        let nan = dir.path().join("nan.json");
        std::fs::write(&nan, "NaN").unwrap();
        let artifact = serde_json::json!({"path": nan.to_str().unwrap(), "format": "json"});
        assert!(matches!(
            branch_result(Ok(Some(&artifact)), false),
            ParallelResult::Error { .. }
        ));
    }

    #[test]
    fn a_failed_branch_is_answered_with_its_error_and_a_missing_artifact_is_an_error() {
        for artifact_results in [true, false] {
            let ParallelResult::Error { error } =
                branch_result(Err("ValueError: boom"), artifact_results)
            else {
                panic!("a failed branch is an error");
            };
            assert_eq!(error, "ValueError: boom");
            assert!(matches!(
                branch_result(Ok(None), artifact_results),
                ParallelResult::Error { .. }
            ));
        }
    }

    /// The worker is told which steps are branches: it reports a result it cannot write as
    /// an error for the caller, and records the frame type of one it can.
    #[test]
    fn build_step_json_marks_parallel_branches() {
        let mut coord = Coordinator::new();
        let spec = ItemSpec {
            fn_ref: "f.py:a".to_string(),
            function_name: "a".to_string(),
            source_file: "f.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            serializer: None,
            sinks: Vec::new(),
            run_hash: None,
            upstream_inputs: HashMap::new(),
            collected_inputs: HashMap::new(),
            param_types: HashMap::new(),
            return_type: None,
            kind: "task".to_string(),
            is_dynamic: false,
        };
        let parent = coord.add_item(
            crate::StepId::unpartitioned("f.py:a"),
            spec.clone(),
            Vec::new(),
        );
        assert_eq!(build_step_json(coord.item(parent), &coord)["branch"], false);
        let (_, children) = coord.on_parallel_requested(parent, vec![spec]);
        assert_eq!(
            build_step_json(coord.item(children[0]), &coord)["branch"],
            true
        );
    }

    /// #332: a branch is told where to write its result, in its group's own directory and
    /// under its own number.
    #[test]
    fn build_step_json_gives_a_branch_its_own_result_path() {
        let mut coord = Coordinator::new();
        let spec = ItemSpec {
            fn_ref: "f.py:a".to_string(),
            function_name: "a".to_string(),
            source_file: "f.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            serializer: None,
            sinks: Vec::new(),
            run_hash: None,
            upstream_inputs: HashMap::new(),
            collected_inputs: HashMap::new(),
            param_types: HashMap::new(),
            return_type: None,
            kind: "task".to_string(),
            is_dynamic: false,
        };
        let parent = coord.add_item(
            crate::StepId::unpartitioned("f.py:a"),
            spec.clone(),
            Vec::new(),
        );
        // The same call twice in one group: two branches, two paths.
        let (group, children) = coord.on_parallel_requested(parent, vec![spec.clone(), spec]);
        assert_eq!(
            build_step_json(coord.item(children[0]), &coord)["branch_path"],
            serde_json::Value::Null,
            "no directory, no path: the worker names the file as it used to"
        );
        coord.set_group_result_dir(group, PathBuf::from("/p/.barca/branches/r1-77/3"));
        let paths: Vec<String> = children
            .iter()
            .map(|&c| {
                build_step_json(coord.item(c), &coord)["branch_path"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            paths,
            [
                format!("/p/.barca/branches/r1-77/3/{}", children[0].0),
                format!("/p/.barca/branches/r1-77/3/{}", children[1].0),
            ]
        );
        assert_ne!(paths[0], paths[1]);
        assert_eq!(
            build_step_json(coord.item(parent), &coord)["branch_path"],
            serde_json::Value::Null
        );
    }

    /// Two runs never share a directory, and a run's directory names the process that owns it.
    #[test]
    fn a_runs_branch_results_are_under_its_own_id_and_pid() {
        let a = branch_results_root("run-a");
        let b = branch_results_root("run-b");
        assert_ne!(a, b);
        assert!(a.ends_with(format!(".barca/branches/run-a-{}", std::process::id())));
    }

    /// The sweep removes what a killed run left and nothing else: not a live run's results
    /// (this process is alive), not a name it did not make.
    #[cfg(unix)]
    #[test]
    fn the_sweep_removes_the_branch_results_of_dead_runs_only() {
        // A pid that existed and is certainly gone: a child that has been reaped.
        let mut child = crate::helper_proc::spawn_std(&mut Command::new("true")).unwrap();
        let dead = child.id();
        child.wait().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let of_dead = dir.path().join(format!("r1-{dead}"));
        let of_live = dir.path().join(format!("r2-{}", std::process::id()));
        let foreign = dir.path().join("notes");
        for d in [&of_dead, &of_live, &foreign] {
            std::fs::create_dir_all(d.join("0")).unwrap();
            std::fs::write(d.join("0").join("5.json"), "1").unwrap();
        }
        sweep_branch_results(dir.path());
        assert!(!of_dead.exists());
        assert!(of_live.join("0").join("5.json").exists());
        assert!(foreign.join("0").join("5.json").exists());
        // A project that never used parallel() has no such directory: nothing to do.
        sweep_branch_results(&dir.path().join("absent"));
    }

    fn test_listener(name: &str) -> (UnixListener, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("barca_test_{name}_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        (UnixListener::bind(&path).unwrap(), path)
    }

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    /// #292: a worker that ends without connecting (a Ctrl-C used to end one that was still
    /// starting) is reported when it ends, not after the ten seconds allowed for connecting.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_worker_that_exits_before_connecting_fails_the_start_at_once() {
        let (listener, path) = test_listener("exits_early");
        let (event_tx, _event_rx) = mpsc::channel(8);
        let started = std::time::Instant::now();
        let result = start_worker(
            sh("exit 3"),
            0,
            &listener,
            &event_tx,
            &CancellationToken::new(),
        )
        .await;
        let Err(SpawnError::Failed(why)) = result else {
            panic!("a worker that exited must not count as started");
        };
        assert!(
            why.contains("exited before it connected") && why.contains("exit status: 3"),
            "{why}"
        );
        assert!(
            started.elapsed() < WORKER_CONNECT_LIMIT / 2,
            "waited {:?} for a worker that had exited",
            started.elapsed()
        );
        let _ = std::fs::remove_file(path);
    }

    /// #292: cancelling a run while a worker starts ends the wait for it at once, and the
    /// worker, which never became part of the pool, is not left running.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_while_a_worker_starts_stops_waiting_and_stops_the_worker() {
        let (listener, path) = test_listener("cancel_start");
        let (event_tx, _event_rx) = mpsc::channel(8);
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // Never connects: a worker held in its start-up.
        let cmd = sh(&format!("echo $$ > {}; exec sleep 60", pid_file.display()));
        let cancel = CancellationToken::new();
        let canceller = {
            let (cancel, pid_file) = (cancel.clone(), pid_file.clone());
            tokio::spawn(async move {
                // Cancel once the worker is certainly running.
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                while !std::fs::read_to_string(&pid_file).is_ok_and(|s| s.ends_with('\n')) {
                    assert!(std::time::Instant::now() < deadline, "the worker never ran");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                cancel.cancel();
                std::time::Instant::now()
            })
        };
        let result = start_worker(cmd, 0, &listener, &event_tx, &cancel).await;
        let cancelled_at = canceller.await.unwrap();
        assert!(matches!(result, Err(SpawnError::Cancelled)));
        assert!(
            cancelled_at.elapsed() < WORKER_CONNECT_LIMIT / 2,
            "returned {:?} after the cancellation",
            cancelled_at.elapsed()
        );
        // Killed and reaped before `start_worker` returned.
        let pid: i64 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!crate::db::pid_alive(pid), "the worker is still running");
        let _ = std::fs::remove_file(path);
    }

    /// Regression test for the pool_size*200ms shutdown bug: `shutdown()`
    /// used to call `graceful_kill()` (SIGTERM, then an unconditional 200ms
    /// sleep before the first liveness check) once per worker in a plain
    /// loop, so N workers took ~N*200ms to tear down. Each dummy worker here
    /// traps SIGTERM (`trap '' TERM`), so it can only die via the SIGKILL
    /// fallback — forcing every worker through the full grace-period path
    /// and making the old O(pool_size) vs. new O(1) timing deterministic
    /// rather than racing against how fast a real process happens to react
    /// to SIGTERM.
    #[cfg(unix)]
    #[test]
    fn shutdown_terminates_all_workers_in_one_shared_grace_window() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let n = 4;
            let socket_path = std::env::temp_dir().join(format!(
                "barca_test_shutdown_{}_{}.sock",
                std::process::id(),
                n
            ));
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path).unwrap();
            let (event_tx, event_rx) = mpsc::channel(8);

            let mut workers = HashMap::new();
            let mut pids = Vec::new();
            for i in 0..n {
                // `exec`: the process that ignores SIGTERM is the child itself. Without it
                // the shell forks `sleep`, and killing the shell leaves the `sleep` running
                // for its 30 seconds with every descriptor it inherited.
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "trap '' TERM; exec sleep 30"]);
                let child = crate::helper_proc::spawn_std(&mut cmd).unwrap();
                pids.push(child.id());
                let (cmd_tx, _cmd_rx) = mpsc::channel(1);
                workers.insert(
                    i,
                    WorkerHandle {
                        child,
                        cmd_tx,
                        _task: tokio::spawn(async {}),
                        leases: VecDeque::new(),
                        front_since: std::time::Instant::now(),
                    },
                );
            }

            // Let every shell finish installing its `trap '' TERM` before we
            // start signaling — without this, a SIGTERM landing mid-startup
            // (default disposition, trap not yet installed) can kill the
            // process instantly and silently turn this into a no-op test.
            std::thread::sleep(Duration::from_millis(100));
            for &pid in &pids {
                let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
                assert!(
                    alive,
                    "worker pid {pid} died before shutdown() was even called"
                );
            }

            let pool = WorkerPool {
                config: IoConfig {
                    python: PathBuf::from("python3"),
                    pool_size: n,
                    run_id: "test-shutdown".to_string(),
                    datadog_job: None,
                    artifact_root: ".".to_string(),
                    storage_options_json: None,
                },
                socket_path: socket_path.clone(),
                listener,
                event_tx,
                event_rx,
                workers,
                frozen: Vec::new(),
                next_worker_id: n,
                trace_start: std::time::Instant::now(),
                trace_on: false,
                progress_interval: Duration::ZERO,
                running_hook: None,
                repeated_warnings: Vec::new(),
                branch_root: std::env::temp_dir().join("barca_test_shutdown_branches"),
                next_branch_group: 0,
                branch_dirs: HashMap::new(),
            };

            let start = std::time::Instant::now();
            pool.shutdown().await;
            let elapsed = start.elapsed();

            // Old code: n * 200ms sequential sleeps (~800ms for 4 workers
            // that never die on their own). New code: one shared ~200ms
            // grace window regardless of worker count, plus SIGKILL
            // overhead. Lower bound guards against the exact race this test
            // is designed to catch: if a worker died before its trap took
            // effect, shutdown would return almost instantly without ever
            // exercising the shared-grace-window path at all.
            assert!(
                elapsed >= Duration::from_millis(150) && elapsed < Duration::from_millis(500),
                "shutdown of {n} SIGTERM-ignoring workers took {elapsed:?} — expected one \
                 shared ~200ms grace window, not ~0 (race) or ~{}ms (one window per worker)",
                n * 200
            );

            for pid in pids {
                // kill(pid, 0) sends no signal, just checks liveness/permission;
                // a nonzero return (ESRCH) confirms the process was reaped.
                let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
                assert!(
                    !still_alive,
                    "worker pid {pid} should have been terminated by shutdown()"
                );
            }

            let _ = std::fs::remove_file(&socket_path);
        });
    }
}
