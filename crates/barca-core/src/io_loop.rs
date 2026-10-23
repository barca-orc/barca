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
//! Workers start together: when ready work has no idle worker, every worker the ready
//! queue can use (up to `pool_size`) is started at once, and each connection is taken as
//! it arrives (identified by the `worker_id` in its hello), so start-ups overlap instead of
//! adding up (#560). Work goes to whichever worker connects first.
//!
//! On parallel(), the requesting worker is SIGSTOP'd, a replacement is
//! started, and children enter the ready queue. When all children complete,
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
    pub upload_snapshot_prefix: Option<String>,
    pub python: PathBuf,
    pub pool_size: usize,
    /// The most the weights of the steps running at once may add up to (#436): the pool size
    /// unless `[execution] capacity` or `BARCA_CAPACITY` sets it.
    pub capacity: usize,
    pub run_id: String,
    /// Present only when the Datadog integration successfully configured.
    pub datadog: Option<std::sync::Arc<crate::telemetry::datadog::Dispatch>>,
    /// Local artifact directory workers write to and read from. Always local:
    /// a separate artifact store is synced by `transfer::TransferClient`.
    /// Set explicitly on every worker so env-separated layouts work
    /// regardless of the coordinator's own environment.
    pub artifact_root: String,
    /// Merged fsspec storage options (JSON), forwarded to workers (for remote
    /// `@sink` destinations).
    pub storage_options_json: Option<String>,
    /// `--agent` (and every run `barca serve` starts): workers announce each module they
    /// import (`[barca] importing <module>`); otherwise they say nothing of their own (#496).
    pub agent_mode: bool,
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
    batch_open: bool,
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
    front_since: std::time::Instant,
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
    batch_open: bool,
}

// ─── Events ──────────────────────────────────────────────────────────────────

enum IoEvent {
    BatchFinished {
        worker_id: usize,
    },
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
    /// A process connected to the coordination socket and negotiated (or failed to).
    /// `worker_id` is from its hello, `pid` from the socket (`SO_PEERCRED`).
    Connected {
        worker_id: Option<usize>,
        pid: Option<u32>,
        stream: Result<UnixStream, String>,
    },
}

/// The task that takes connections, stopped when dropped: a pool dropped on some path that did
/// not shut it down (a long-lived `barca serve` runs many) leaves no task holding its socket.
struct Acceptor(JoinHandle<()>);

impl Drop for Acceptor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A worker that has been started and has not connected yet.
struct PendingWorker {
    child: Child,
    started: std::time::Instant,
}

/// Take every connection to the coordination socket, each negotiated in a task of its own so
/// that one slow or silent process holds up no other. Ends when the pool drops its handle.
async fn accept_connections(listener: UnixListener, event_tx: mpsc::Sender<IoEvent>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Transient (out of descriptors): a worker still starting is failed by its
            // deadline if this does not clear.
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        let tx = event_tx.clone();
        tokio::spawn(async move {
            let mut stream = stream;
            let pid = stream
                .peer_cred()
                .ok()
                .and_then(|c| c.pid())
                .and_then(|p| u32::try_from(p).ok());
            let negotiated = tokio::time::timeout(
                WORKER_CONNECT_LIMIT,
                crate::protocol::accept_hello(&mut stream, "worker"),
            )
            .await;
            let (worker_id, stream) = match negotiated {
                Ok(Ok(hello)) => (
                    hello
                        .get("worker_id")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|id| usize::try_from(id).ok()),
                    Ok(stream),
                ),
                Ok(Err(e)) => (None, Err(format!("connection/protocol: {e}"))),
                // The worker's own start deadline reports this.
                Err(_) => return,
            };
            let _ = tx
                .send(IoEvent::Connected {
                    worker_id,
                    pid,
                    stream,
                })
                .await;
        });
    }
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
    let mut journal: Option<crate::result_journal::ResultJournal> = None;
    loop {
        tokio::select! {
            result = read_frame::<_, serde_json::Value>(&mut stream) => {
                // File receipts can be ahead of socket consumption. Only publish a
                // counted prefix after its earlier stdout frames were consumed. Submit
                // blocks the producer, and EOF follows all complete socket frames.
                let through = match &result {
                    Ok(Some(frame)) if frame["type"] == "result_checkpoint" => frame.get("completed").and_then(serde_json::Value::as_u64).and_then(|n| usize::try_from(n).ok()),
                    Ok(Some(frame)) if frame["type"] == "results" => frame["node_ids"].as_array().map(Vec::len),
                    Ok(Some(frame)) if frame["type"] == "submit" => Some(usize::MAX),
                    Ok(None) | Err(_) => Some(usize::MAX),
                    _ => None,
                };
                if let (Some(receipts), Some(count)) = (journal.as_mut(), through) {
                    let Ok(messages) = receipts.read_through(count) else {
                        let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await;
                        break;
                    };
                    for msg in messages {
                        if event_tx.send(IoEvent::Message { worker_id, msg: Box::new(msg) }).await.is_err() { return; }
                    }
                }
                match result {
                    Ok(Some(frame)) => {
                        if frame["type"] == "result_checkpoint" { continue; }
                        let batched = frame["type"] == "results";
                        let messages = if batched { crate::wire::decode_results(&frame) }
                            else if matches!(frame["type"].as_str(), Some("step_completed" | "step_error" | "blocked")) { Err("v3 results require a compact result frame".into()) }
                            else { serde_json::from_value(frame).map(|message| vec![message]).map_err(|e| e.to_string()) };
                        let Ok(messages) = messages else {
                            let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await;
                            break;
                        };
                        for msg in messages {
                            let accepted = match journal.as_mut() {
                                Some(receipts) => receipts.accept(&msg), None => Ok(true),
                            };
                            match accepted {
                                Ok(false) => continue,
                                Ok(true) => {},
                                Err(_) => { let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await; return; }
                            }
                            if event_tx.send(IoEvent::Message { worker_id, msg: Box::new(msg) }).await.is_err() { return; }
                        }
                        if batched && journal.take().is_some() && event_tx.send(IoEvent::BatchFinished {worker_id}).await.is_err() { return; }
                    }
                    Ok(None) | Err(_) => {
                        let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await;
                        break;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(mut msg) => {
                        if msg["type"] == "execute_batch" {
                            match crate::result_journal::ResultJournal::new(&mut msg) {
                                Ok(receipts) => journal = Some(receipts),
                                Err(_) => { let _ = event_tx.send(IoEvent::Disconnected { worker_id }).await; break; }
                            }
                        }
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
    /// Takes connections to the socket ([`accept_connections`]); stopped at shutdown, or when
    /// the pool is dropped without one.
    acceptor: Acceptor,
    event_tx: mpsc::Sender<IoEvent>,
    event_rx: mpsc::Receiver<IoEvent>,
    workers: HashMap<usize, WorkerHandle>,
    /// Workers started and not connected yet, by worker id.
    pending: HashMap<usize, PendingWorker>,
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
    /// Failed steps whose worker went on to its next step instead of being replaced (#523).
    failures_survived: usize,
    /// Steps encoded and sent to a worker, counting a step again each time it is re-sent
    /// after its worker stopped with it still leased. Each failed step used to stop its
    /// worker and re-send the rest of its batch, so with many failures this grew with keys
    /// × batch size (#523).
    steps_dispatched: usize,
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
        let socket_path = crate::protocol::socket_path(&config.run_id, "main")?;
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
        let acceptor = Acceptor(tokio::spawn(accept_connections(listener, event_tx.clone())));
        Ok(Self {
            config,
            socket_path,
            acceptor,
            event_tx,
            event_rx,
            workers: HashMap::new(),
            pending: HashMap::new(),
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
            failures_survived: 0,
            steps_dispatched: 0,
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
        if let Some(dispatch) = &self.config.datadog {
            // Dynamic parallel items have no span of their own in the run report.
            // Attach their Python execution to the nearest reported ancestor.
            let mut parent = item;
            while let Some(group) = parent.group {
                parent = coord.item(coord.group(group).parent);
            }
            if let Some(context) =
                dispatch.context(&self.config.run_id, &parent.step_id, item.attempts)
            {
                step["datadog"] = context;
            }
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

        // Polls the workers still starting (`fail_dead_starts`).
        let mut start_watch = tokio::time::interval(Duration::from_millis(10));
        start_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            if coord.is_finished() {
                break;
            }
            // Before the check below: a worker whose start was cancelled is not a pool that
            // failed to start.
            if cancel.is_cancelled() {
                return Err("run cancelled".to_string());
            }
            if self.workers.is_empty()
                && self.pending.is_empty()
                && self.frozen.is_empty()
                && coord.ready_count() > 0
            {
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
                // A worker that is starting fails when its process exits or its deadline
                // passes, whichever comes first.
                _ = start_watch.tick(), if !self.pending.is_empty() => {
                    // Not started again here: a worker that cannot start would be started
                    // over and over. With no worker left, the check above ends the phase.
                    self.fail_dead_starts();
                    continue;
                }
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
                            crate::errln!(
                                "[trace]  {:>8.1}ms  StepCompleted <- worker {worker_id}: {node_id} (self-reported elapsed={:.1}ms cpu={:.1}ms)",
                                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                                artifact.elapsed_seconds.unwrap_or(0.0) * 1000.0,
                                artifact.cpu_seconds.unwrap_or(0.0) * 1000.0,
                            );
                        }
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            crate::errln!(
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
                                cached: false,
                                elapsed_seconds: artifact.elapsed_seconds,
                                error: None,
                                import_seconds: artifact.import_seconds,
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
                        elapsed,
                        import_seconds,
                        worker_continues,
                    } => {
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            crate::errln!(
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
                            // The attempt's traceback is part of the step's stderr, as Python
                            // would have printed it. Recorded, not echoed: the failure line
                            // says it already.
                            for line in traceback_lines(&error_type, &message, &traceback) {
                                ev(RunEvent::log(
                                    node_id.clone(),
                                    line,
                                    crate::LogStream::Stderr,
                                ));
                            }
                            ev(RunEvent::StepFinished {
                                node_id: node_id.clone(),
                                ok: false,
                                cached: false,
                                elapsed_seconds: Some(elapsed),
                                error: Some(error.clone()),
                                import_seconds,
                            });
                        }
                        if let FailureAction::RetryAfter(delay) = coord.on_item_failed(
                            item_id,
                            error,
                            Duration::from_secs_f64(elapsed.max(0.0)),
                        ) {
                            schedule_retry(&self.event_tx, item_id, delay);
                        }
                        self.drop_branch_results(item_id);

                        // A step that raised leaves its worker sound, and the worker goes on
                        // with its batch (#523). One that may not be sound (a failed import,
                        // `sys.exit()`, still running past its timeout) is stopped so retries
                        // and remaining work get a fresh interpreter: unstarted leases go back
                        // to the queue front, and the 200ms SIGTERM grace runs off the event
                        // loop.
                        if worker_continues {
                            self.failures_survived += 1;
                        } else if let Some(mut handle) = self.workers.remove(&worker_id) {
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
                        let ran_for = self
                            .workers
                            .get(&worker_id)
                            .map_or(Duration::ZERO, |h| h.front_since.elapsed());
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            crate::errln!(
                                "[barca] Blocked for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(item_id, format!("blocked: {reason}"), ran_for)
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
                            crate::errln!(
                                "[barca] Submit from unknown worker {worker_id}, ignoring"
                            );
                            continue;
                        };
                        let Some(&item_id) = handle.leases.front() else {
                            crate::errln!(
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
                                    // Set to the caller's by `on_parallel_requested`.
                                    weight: 1,
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
                                    keyed_inputs: HashMap::new(),
                                    partition_source: false,
                                }
                            })
                            .collect();

                        let (group_id, _child_ids) = coord.on_parallel_requested(item_id, specs);
                        // The group's own directory for what its branches return, removed
                        // when the step that called parallel() ends (`drop_branch_results`).
                        let result_dir = self.branch_root.join(self.next_branch_group.to_string());
                        self.next_branch_group += 1;
                        if let Err(e) = std::fs::create_dir_all(&result_dir) {
                            crate::errln!("[barca] could not create {}: {e}", result_dir.display());
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

                        // The frozen worker no longer counts towards the pool: the
                        // `assign_ready` below starts its replacement when the branches
                        // need one (no idle worker), without waiting for it to connect.
                        self.frozen.push(FrozenWorker {
                            child: handle.child,
                            cmd_tx: handle.cmd_tx,
                            _task: handle._task,
                            parent_item: item_id,
                            front_since: handle.front_since,
                            group_id,
                            original_worker_id: worker_id,
                            rest,
                            artifact_results,
                            batch_open: handle.batch_open,
                        });

                        // Assign ready items (children are now in the ready queue)
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Heartbeat => {}
                    WorkerMessage::Log {
                        node_id,
                        line,
                        stream,
                        t,
                    } => {
                        // A line of user stdout or stderr — forward live, stamped with when
                        // the worker wrote it; the caller persists it. Does not change
                        // worker/coordinator state.
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::Log {
                                node_id,
                                line,
                                stream,
                                ts: Some(t.map_or_else(
                                    crate::events::log_timestamp,
                                    crate::events::epoch_timestamp,
                                )),
                            });
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
                IoEvent::BatchFinished { worker_id } => {
                    if let Some(handle) = self.workers.get_mut(&worker_id) {
                        handle.batch_open = false;
                    }
                    self.resume_frozen(coord).await;
                    self.assign_ready(coord, cost, cancel).await;
                }
                IoEvent::Disconnected { worker_id } => {
                    // Worker crashed — its in-flight item failed; unstarted
                    // leases return to the queue for another worker.
                    if let Some(mut handle) = self.workers.remove(&worker_id) {
                        if let Some(in_flight) = handle.leases.pop_front() {
                            let ran_for = handle.front_since.elapsed();
                            let node_id = coord.item(in_flight).step_id.display();
                            let status = reap_briefly(&mut handle.child).await;
                            let error = disconnect_message(&node_id, status, false);
                            if let Some(ref mut ev) = on_event {
                                ev(RunEvent::StepFinished {
                                    node_id,
                                    ok: false,
                                    cached: false,
                                    elapsed_seconds: Some(ran_for.as_secs_f64()),
                                    error: Some(error.clone()),
                                    import_seconds: None,
                                });
                            }
                            if let FailureAction::RetryAfter(delay) =
                                coord.on_item_failed(in_flight, error, ran_for)
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
                        let ran_for = fw.front_since.elapsed();
                        let node_id = coord.item(fw.parent_item).step_id.display();
                        // A stopped process is not reported by `try_wait` (no WUNTRACED), so
                        // a frozen worker is only named here once it has really exited.
                        let status = reap_briefly(&mut fw.child).await;
                        let error = disconnect_message(&node_id, status, true);
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::StepFinished {
                                node_id,
                                ok: false,
                                cached: false,
                                elapsed_seconds: Some(ran_for.as_secs_f64()),
                                error: Some(error.clone()),
                                import_seconds: None,
                            });
                        }
                        coord.abandon_group(fw.group_id);
                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(fw.parent_item, error, ran_for)
                        {
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
                IoEvent::Connected {
                    worker_id,
                    pid,
                    stream,
                } => {
                    // A connection that fails its handshake starts nobody new here: a worker
                    // that cannot start would be started over and over.
                    if self.on_connected(worker_id, pid, stream) {
                        self.assign_ready(coord, cost, cancel).await;
                    }
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
        if self.trace_on {
            // How many workers the run started, against the failed steps that kept theirs (a
            // failing step costs no worker start), and how many steps were sent to workers,
            // re-sends included (#523).
            crate::errln!(
                "[trace]  {:>8.1}ms  workers started {}, failed steps that kept their worker {}, steps dispatched {}",
                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                self.next_worker_id,
                self.failures_survived,
                self.steps_dispatched
            );
        }
        drop(self.acceptor);
        let workers = self.workers;
        let frozen = self.frozen;
        let pending = self.pending;
        let kill_task = tokio::task::spawn_blocking(move || {
            let mut children: Vec<Child> =
                Vec::with_capacity(workers.len() + frozen.len() + pending.len());

            // Started and never connected (a cancelled run, or one that ended first): not
            // members of the pool, and still stopped and reaped here.
            #[cfg(unix)]
            for (_, p) in pending {
                unsafe {
                    libc::kill(p.child.id() as i32, libc::SIGTERM);
                }
                children.push(p.child);
            }
            #[cfg(not(unix))]
            for (_, p) in pending {
                children.push(p.child);
            }

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
                    // A worker exits within a millisecond or two of SIGTERM: polling every
                    // 5 ms made each run's shutdown about 5 ms (#560).
                    std::thread::sleep(Duration::from_millis(1));
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

    /// Assign ready items to idle workers in cost-sized batches, starting
    /// workers on demand up to `pool_size`.
    ///
    /// When ready work finds no idle worker, every worker it can use is started at once
    /// ([`Self::start_workers`]) and none is waited for: each joins the pool when it connects
    /// (`IoEvent::Connected`), and this runs again then. Starting them one at a time, each
    /// waited for before the next, cost 30-40 ms per worker (#560).
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
            // The capacity is full for the step next in line: nothing starts until enough of
            // what is running finishes, and nothing queued behind it goes first (#436).
            if !coord.head_fits(self.running_weight(coord), self.capacity()) {
                return;
            }

            // Find an idle worker, or start the ones the ready work can use.
            let Some(wid) = self
                .workers
                .iter()
                .find(|(_, w)| w.leases.is_empty() && !w.batch_open)
                .map(|(&id, _)| id)
            else {
                self.start_workers(coord, cost);
                return;
            };

            // Lease a batch: K sized from the head item's measured cost.
            let len = self.batch_len(
                coord,
                cost,
                &mut coord.ready_items().peekable(),
                coord.ready_count(),
            );
            let batch: Vec<ItemId> = (0..len)
                .map(|_| coord.next_ready().expect("batch_len counts ready items"))
                .collect();

            let msg = if batch.len() == 1 {
                let step = self.traced_step(coord.item(batch[0]), coord);
                serde_json::json!({"type": "execute", "step": crate::wire::normalize_step(&step)})
            } else {
                let steps: Vec<serde_json::Value> = batch
                    .iter()
                    .map(|&iid| self.traced_step(coord.item(iid), coord))
                    .collect();
                crate::wire::encode_steps(&steps)
            };

            if self.trace_on {
                let names: Vec<String> = batch
                    .iter()
                    .map(|&iid| coord.item(iid).step_id.display())
                    .collect();
                crate::errln!(
                    "[trace]  {:>8.1}ms  dispatch -> worker {wid}: {names:?}",
                    self.trace_start.elapsed().as_secs_f64() * 1000.0
                );
            }

            self.steps_dispatched += batch.len();
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
            handle.batch_open = batch.len() > 1;
            handle.leases = batch.into_iter().collect();
            handle.front_since = std::time::Instant::now();
        }
    }

    /// The capacity as a weight, at least 1.
    fn capacity(&self) -> u32 {
        u32::try_from(self.config.capacity.max(1)).unwrap_or(u32::MAX)
    }

    /// The total weight of the steps running now: for each worker in the pool, the weight of
    /// what it has leased (a step with a weight above 1 is always leased alone, `batch_len`).
    /// A worker stopped in `parallel()` is not counted while its branches run, which take its
    /// weight (`Coordinator::on_parallel_requested`): counting both, a step could never get
    /// its branches started.
    fn running_weight(&self, coord: &Coordinator) -> u32 {
        let capacity = self.capacity();
        self.workers
            .values()
            .map(|w| {
                w.leases
                    .iter()
                    .map(|&id| {
                        crate::coordinator::effective_weight(coord.item(id).spec.weight, capacity)
                    })
                    .max()
                    .unwrap_or(0)
            })
            .sum()
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
        while self.workers.len() + self.pending.len() > self.config.pool_size.max(1) {
            let Some(idle) = self
                .workers
                .iter()
                .find(|(_, w)| w.leases.is_empty() && !w.batch_open)
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

    /// How many of the ready items `ready` yields (in queue order, `remaining` of them)
    /// make the next batch. Consumes those items from the iterator.
    ///
    /// K is sized from the head item's measured cost. The ceiling input is the pull-eligible
    /// pool (ready items), not pending work blocked on upstreams — items that can't be pulled
    /// this wave mustn't let one worker drain the whole ready queue. The batch is filled up
    /// to K, but estimated-heavy items are never packed behind a light head: the batch has a
    /// work budget of K × the head's cost (what K was computed for), and an item that would
    /// blow it waits for its own pull. Guards the over-batch tail-block when a phase mixes
    /// light and heavy nodes.
    fn batch_len(
        &self,
        coord: &Coordinator,
        cost: &CostModel,
        ready: &mut std::iter::Peekable<impl Iterator<Item = ItemId>>,
        remaining: usize,
    ) -> usize {
        let Some(first) = ready.next() else {
            return 0;
        };
        // A step with a weight above 1 is leased on its own, and none is leased behind a
        // lighter one, so a worker's weight is the weight of the step it is running (#436).
        if coord.item(first).spec.weight > 1 {
            return 1;
        }
        let head_node = coord.item(first).step_id.display();
        // The phase's other steps (waiting on upstreams or on their stream) count toward
        // whether the work is trivial, not toward the ceiling.
        let backlog = coord.remaining_count().saturating_sub(coord.ready_count());
        let k = cost
            .batch_size_with_backlog(&head_node, remaining, backlog, self.config.pool_size.max(1))
            .max(1);
        let head_est = cost.estimate(&head_node);
        let budget = head_est * k as f64 * 1.5;
        let mut acc = head_est;
        let mut len = 1;
        while len < k {
            let Some(&id) = ready.peek() else {
                break;
            };
            if coord.item(id).spec.weight > 1 {
                break;
            }
            let est = cost.estimate(&coord.item(id).step_id.display());
            if acc + est > budget {
                break;
            }
            ready.next();
            acc += est;
            len += 1;
        }
        len
    }

    /// Start, all at once, the workers the ready queue can use that no worker already
    /// connected or starting will take: one per batch it would be leased as, up to
    /// `pool_size` in all. A one-step run starts one worker. None is waited for.
    fn start_workers(&mut self, coord: &Coordinator, cost: &CostModel) {
        let room = self
            .config
            .pool_size
            .max(1)
            .saturating_sub(self.workers.len() + self.pending.len());
        if room == 0 {
            return;
        }
        // Each worker still starting takes the first batch left when it connects.
        // Only as many as the capacity lets start: a worker for a step that has to wait would
        // sit idle (#436).
        let capacity = self.capacity();
        let mut running = self.running_weight(coord);
        let mut ready = coord.ready_items().peekable();
        let mut taken = 0;
        let mut batches = 0;
        while taken < coord.ready_count() && batches < self.pending.len() + room {
            let Some(&head) = ready.peek() else {
                break;
            };
            let weight = coord.item(head).spec.weight;
            if !crate::coordinator::fits(running, weight, capacity) {
                break;
            }
            running += crate::coordinator::effective_weight(weight, capacity);
            taken += self.batch_len(coord, cost, &mut ready, coord.ready_count() - taken);
            batches += 1;
        }
        let wanted = batches.saturating_sub(self.pending.len()).min(room);
        if self.trace_on && wanted > 0 {
            crate::errln!(
                "[trace]  {:>8.1}ms  starting {wanted} worker(s) ({} already starting)",
                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                self.pending.len()
            );
        }
        for _ in 0..wanted {
            let wid = self.next_worker_id;
            self.next_worker_id += 1;
            let cmd = worker_command(&self.config, &self.socket_path, wid);
            if let Err(e) = self.launch(cmd, wid) {
                crate::errln!("[barca] failed to spawn worker: {e}");
                return;
            }
        }
    }

    /// Start `cmd` as worker `worker_id`, without waiting for it to connect.
    fn launch(&mut self, mut cmd: Command, worker_id: usize) -> Result<(), String> {
        let started = std::time::Instant::now();
        let child = crate::helper_proc::spawn_std(&mut cmd).map_err(|e| format!("spawn: {e}"))?;
        if self.trace_on {
            crate::errln!(
                "[trace]  worker {worker_id} process spawned in {:.1}ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        self.pending
            .insert(worker_id, PendingWorker { child, started });
        Ok(())
    }

    /// A connection arrived: it joins the pool as the worker its hello names (or, without
    /// a `worker_id`, the one whose process it comes from). One that matches no worker still
    /// starting (stopped already, or not barca's) is closed. Returns whether a worker joined.
    fn on_connected(
        &mut self,
        worker_id: Option<usize>,
        pid: Option<u32>,
        stream: Result<UnixStream, String>,
    ) -> bool {
        let wid = worker_id
            .filter(|id| self.pending.contains_key(id))
            .or_else(|| {
                let pid = pid?;
                self.pending
                    .iter()
                    .find(|(_, p)| p.child.id() == pid)
                    .map(|(&id, _)| id)
            });
        let Some(wid) = wid else {
            if self.trace_on {
                crate::errln!(
                    "[trace]  connection from an unknown process (worker id {worker_id:?}, pid {pid:?}) closed"
                );
            }
            return false;
        };
        let pending = self.pending.remove(&wid).expect("matched above");
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                crate::errln!("[barca] failed to spawn worker: worker {wid} {e}");
                Self::reap(pending.child);
                return false;
            }
        };
        if self.trace_on {
            crate::errln!(
                "[trace]  worker {wid} connected (accept) in {:.1}ms",
                pending.started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let (cmd_tx, cmd_rx) = mpsc::channel::<serde_json::Value>(16);
        let task = tokio::spawn(worker_io_task(wid, stream, cmd_rx, self.event_tx.clone()));
        self.workers.insert(
            wid,
            WorkerHandle {
                child: pending.child,
                cmd_tx,
                _task: task,
                leases: VecDeque::new(),
                batch_open: false,
                front_since: std::time::Instant::now(),
            },
        );
        true
    }

    /// Fail the starting workers whose process has exited, or that have not connected within
    /// [`WORKER_CONNECT_LIMIT`]. Each is reported and reaped. Returns why each failed.
    fn fail_dead_starts(&mut self) -> Vec<String> {
        let mut failed = Vec::new();
        for (&wid, p) in &mut self.pending {
            match p.child.try_wait() {
                Ok(Some(status)) => failed.push((
                    wid,
                    format!("worker {wid} exited before it connected ({status})"),
                )),
                Err(e) => failed.push((
                    wid,
                    format!("worker {wid} exited before it connected (its status could not be read: {e})"),
                )),
                Ok(None) if p.started.elapsed() >= WORKER_CONNECT_LIMIT => failed.push((
                    wid,
                    format!(
                        "timeout waiting for worker {wid} to connect and announce protocol v{}; reinstall barca in this Python environment",
                        crate::protocol::PROTOCOL_VERSION
                    ),
                )),
                Ok(None) => {}
            }
        }
        failed
            .into_iter()
            .map(|(wid, why)| {
                crate::errln!("[barca] failed to spawn worker: {why}");
                if let Some(p) = self.pending.remove(&wid) {
                    Self::reap(p.child);
                }
                why
            })
            .collect()
    }

    /// Kill and reap a worker that is not part of the pool, off the event loop.
    fn reap(mut child: Child) {
        tokio::task::spawn_blocking(move || {
            let _ = child.kill();
            let _ = child.wait();
        });
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
                        batch_open: fw.batch_open,
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

/// How long a worker may take from being started to connecting.
const WORKER_CONNECT_LIMIT: Duration = Duration::from_secs(10);

/// The command that starts worker `worker_id` against the socket at `socket_path`.
fn worker_command(config: &IoConfig, socket_path: &Path, worker_id: usize) -> Command {
    let mut cmd = crate::helper_proc::python_module_std(
        &config.python,
        "barca._worker",
        config.storage_options_json.as_deref(),
    );
    cmd.arg("--daemon")
        .env("BARCA_SOCKET", socket_path.to_str().unwrap_or(""))
        .env("BARCA_WORKER", "1")
        .env("BARCA_WORKER_ID", worker_id.to_string())
        .env("BARCA_ARTIFACT_URI", &config.artifact_root)
        // A step's own print() output goes to barca's stderr, never stdout: stdout carries
        // only barca's result, so `barca run ... | jq` works when steps print.
        // Both streams are one destination, as they always were; when barca's stderr is a
        // pipe it is a pipe barca owns, so a reader that leaves cannot fail the step or
        // kill a process it starts (`crate::term`, "What barca's child processes write to").
        .stdout(crate::term::child_output())
        .stderr(crate::term::child_output())
        .stdin(Stdio::null());

    if config.agent_mode {
        cmd.env("BARCA_WORKER_AGENT", "1");
    } else {
        cmd.env_remove("BARCA_WORKER_AGENT");
    }
    if let Some(prefix) = &config.upload_snapshot_prefix {
        cmd.env("BARCA_UPLOAD_SNAPSHOT_PREFIX", prefix);
    } else {
        cmd.env_remove("BARCA_UPLOAD_SNAPSHOT_PREFIX");
    }

    // Ctrl-C means something to a worker only while it runs a step. It starts outside the
    // terminal's job, so that one arriving while the interpreter starts is not a traceback,
    // and joins the job once it handles the signal itself.
    #[cfg(unix)]
    crate::helper_proc::start_outside_the_job(&mut cmd);
    cmd
}

// ─── Process lifecycle ────────────────────────────────────────────────────────

/// How long a disconnect waits for the worker's exit status. The socket can close a moment
/// before the kernel has finished tearing the process down; a worker that is still alive
/// after this is reported with the plain "worker disconnected" wording.
const EXIT_STATUS_WAIT: Duration = Duration::from_millis(300);

/// The exit status of a worker whose socket closed, polled without blocking for up to
/// [`EXIT_STATUS_WAIT`]. `None` when the process has not exited by then.
async fn reap_briefly(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + EXIT_STATUS_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            _ => return None,
        }
    }
}

/// The name of a signal that ends a worker, `signal N` for one not listed.
fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGTERM => "SIGTERM",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGINT => "SIGINT",
        libc::SIGHUP => "SIGHUP",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGALRM => "SIGALRM",
        libc::SIGSYS => "SIGSYS",
        libc::SIGTRAP => "SIGTRAP",
        _ => return format!("signal {signal}"),
    };
    name.to_string()
}

/// The step error for a worker that disconnected while running `node` (or, `frozen`, while
/// it waited for its `parallel()` branches): the exit status when there is one, so a worker
/// killed by the kernel reads as such and not as a bare disconnect (#433).
/// A failed attempt as Python prints an uncaught exception: the traceback (barca's frames left
/// out) under its header, then `ErrorType: message`. Recorded as the step's stderr lines.
fn traceback_lines(error_type: &str, message: &str, traceback: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if !traceback.trim().is_empty() {
        lines.push("Traceback (most recent call last):".to_string());
        lines.extend(traceback.lines().map(str::to_string));
    }
    let mut last = format!("{error_type}: {message}");
    if message.is_empty() {
        last = error_type.to_string();
    }
    lines.extend(last.lines().map(str::to_string));
    lines
}

fn disconnect_message(
    node: &str,
    status: Option<std::process::ExitStatus>,
    frozen: bool,
) -> String {
    use std::os::unix::process::ExitStatusExt;
    let waiting = if frozen {
        " while it waited for its parallel() branches"
    } else {
        ""
    };
    let how = match status {
        Some(s) => match (s.signal(), s.code()) {
            (Some(libc::SIGKILL), _) => "SIGKILL (possibly the OOM killer)".to_string(),
            (Some(sig), _) => signal_name(sig),
            (None, Some(code)) => format!("code {code}"),
            (None, None) => return format!("worker disconnected{waiting}"),
        },
        None => return format!("worker disconnected{waiting}"),
    };
    format!("worker for {node} exited with {how}{waiting}")
}

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
        if let Some(artifact) = coord.output_for_display(upstream_node_id) {
            let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() {
                inputs.insert(param.clone(), serde_json::Value::String(path.to_string()));
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

    // A dict `partitions_from` source's value for this key: the worker reads `key` of the
    // source's artifact, and passes it when the function takes the parameter.
    for (param, (path, key)) in &item.spec.keyed_inputs {
        inputs.insert(
            param.clone(),
            serde_json::json!({"_keyed": true, "path": path, "key": key}),
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
        // Read by a `partitions_from(...)` for keys: the worker checks what it returns.
        "partition_source": item.spec.partition_source,
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
    if !artifact_results || artifact.is_some_and(|a| a.get("json").is_some()) {
        return ParallelResult::Error {
            error:
                "branch results require artifact file references in protocol v3; reinstall barca"
                    .into(),
        };
    }
    let (Some(format), Some(path)) = (field("format"), field("path")) else {
        return ParallelResult::Error {
            error: "the branch finished but did not report its result file".into(),
        };
    };
    ParallelResult::Ok {
        artifact: crate::protocol::BranchArtifact {
            path: path.to_string(),
            format: format.to_string(),
            frame_type: field("frame_type").map(str::to_string),
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
    fn a_failed_attempt_reads_as_python_prints_it() {
        assert_eq!(
            traceback_lines(
                "ValueError",
                "bad input",
                "  File \"p.py\", line 3, in a\n    raise ValueError(\"bad input\")"
            ),
            [
                "Traceback (most recent call last):",
                "  File \"p.py\", line 3, in a",
                "    raise ValueError(\"bad input\")",
                "ValueError: bad input",
            ]
        );
        assert_eq!(traceback_lines("KeyError", "", ""), ["KeyError"]);
    }

    #[test]
    fn a_dead_workers_exit_status_names_the_signal_or_code() {
        use std::os::unix::process::ExitStatusExt;
        let killed = std::process::ExitStatus::from_raw(libc::SIGKILL);
        assert_eq!(
            disconnect_message("p.py:big", Some(killed), false),
            "worker for p.py:big exited with SIGKILL (possibly the OOM killer)"
        );
        let segv = std::process::ExitStatus::from_raw(libc::SIGSEGV);
        assert_eq!(
            disconnect_message("p.py:big", Some(segv), true),
            "worker for p.py:big exited with SIGSEGV while it waited for its parallel() branches"
        );
        let code = std::process::ExitStatus::from_raw(3 << 8);
        assert_eq!(
            disconnect_message("p.py:big", Some(code), false),
            "worker for p.py:big exited with code 3"
        );
        assert_eq!(
            disconnect_message("p.py:big", None, false),
            "worker disconnected"
        );
        assert_eq!(signal_name(64), "signal 64");
    }

    #[tokio::test]
    async fn a_live_process_is_not_reaped_and_a_dead_one_is() {
        let mut sleeper = crate::helper_proc::spawn_std(Command::new("sleep").arg("5")).unwrap();
        assert!(reap_briefly(&mut sleeper).await.is_none());
        sleeper.kill().unwrap();
        let status = reap_briefly(&mut sleeper)
            .await
            .expect("killed process reaped");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    #[tokio::test]
    async fn completed_receipts_do_not_overtake_their_stdout_frames() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let (commands, rx) = mpsc::channel(2);
        let (events, mut event_rx) = mpsc::channel(16);
        let task = tokio::spawn(worker_io_task(0, stream, rx, events));
        commands
            .send(crate::wire::encode_steps(&[
                serde_json::json!({"node_id":"a"}),
                serde_json::json!({"node_id":"b"}),
            ]))
            .await
            .unwrap();
        let assigned: serde_json::Value = read_frame(&mut peer).await.unwrap().unwrap();
        let mut peer = peer.into_std().unwrap();
        peer.set_nonblocking(false).unwrap();
        let a = serde_json::json!({"path":"/a.json","format":"json","size_bytes":1});
        let b = serde_json::json!({"path":"/b.json","format":"json","size_bytes":1});
        // Queue a valid producer sequence before this single-threaded runtime polls
        // the receiver. The journal is ahead of socket consumption, not socket writes.
        crate::protocol::write_message(
            &mut peer,
            &serde_json::json!({"type":"log","node_id":"a","line":"a output"}),
        )
        .unwrap();
        std::fs::write(
            assigned["result_journal"].as_str().unwrap(),
            format!("{}\n", serde_json::json!(["a", "step_completed", 0.1, a])),
        )
        .unwrap();
        crate::protocol::write_message(
            &mut peer,
            &serde_json::json!({"type":"result_checkpoint","completed":1}),
        )
        .unwrap();
        crate::protocol::write_message(
            &mut peer,
            &serde_json::json!({"type":"log","node_id":"b","line":"b output"}),
        )
        .unwrap();
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(assigned["result_journal"].as_str().unwrap())
                .unwrap();
            writeln!(
                file,
                "{}",
                serde_json::json!(["b", "step_completed", 0.2, b])
            )
            .unwrap();
        }
        crate::protocol::write_message(&mut peer, &serde_json::json!({"type":"results","node_ids":["a","b"],"statuses":["step_completed","step_completed"],"elapsed_seconds":[0.1,0.2],"details":crate::wire::encode_table(&[a,b],true)})).unwrap();
        let mut seen = Vec::new();
        for _ in 0..5 {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .unwrap()
                .unwrap();
            seen.push(match event {
                IoEvent::Message { msg, .. } => match *msg {
                    WorkerMessage::Log { node_id, .. } => format!("log:{node_id}"),
                    WorkerMessage::StepCompleted { node_id, .. } => format!("done:{node_id}"),
                    other => panic!("unexpected {other:?}"),
                },
                IoEvent::BatchFinished { .. } => "finished".to_string(),
                _ => panic!("unexpected event"),
            });
        }
        drop(commands);
        drop(peer);
        task.await.unwrap();
        assert_eq!(seen, ["log:a", "done:a", "log:b", "done:b", "finished"]);
    }

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
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
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
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
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
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
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
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
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

    #[test]
    fn inline_branch_results_are_refused() {
        let inline = serde_json::json!({"path":"", "format":"json", "size_bytes":3, "json":"123"});
        assert!(matches!(
            branch_result(Ok(Some(&inline)), true),
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
            let ParallelResult::Ok { artifact } = branch_result(Ok(Some(&artifact)), true) else {
                panic!("a finished {format} branch must be ok");
            };
            assert_eq!(
                artifact,
                crate::protocol::BranchArtifact {
                    path: "/nowhere/branch.out".to_string(),
                    format: format.to_string(),
                    frame_type: frame_type.map(str::to_string),
                }
            );
        }
    }

    #[test]
    fn legacy_inline_requests_are_refused() {
        let artifact = serde_json::json!({"path":"/a.json", "format":"json", "size_bytes":3});
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
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
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
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
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

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    fn test_spec(name: &str) -> ItemSpec {
        ItemSpec {
            fn_ref: format!("p.py:{name}"),
            function_name: name.to_string(),
            source_file: "p.py".to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            weight: 1,
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
            keyed_inputs: HashMap::new(),
            partition_source: false,
        }
    }

    fn test_config(run_id: &str, pool_size: usize) -> IoConfig {
        IoConfig {
            upload_snapshot_prefix: None,
            python: PathBuf::from("python3"),
            pool_size,
            capacity: pool_size,
            run_id: format!("{run_id}-{}", std::process::id()),
            datadog: None,
            artifact_root: ".".to_string(),
            storage_options_json: None,
            agent_mode: false,
        }
    }

    /// #292: a worker that ends without connecting (a Ctrl-C used to end one that was still
    /// starting) is reported when it ends, not after the ten seconds allowed for connecting.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_worker_that_exits_before_connecting_fails_the_start_at_once() {
        let mut pool = WorkerPool::start(test_config("exits-early", 1)).unwrap();
        pool.launch(sh("exit 3"), 0).unwrap();
        let started = std::time::Instant::now();
        let why = loop {
            let failed = pool.fail_dead_starts();
            if let Some(why) = failed.into_iter().next() {
                break why;
            }
            assert!(
                started.elapsed() < WORKER_CONNECT_LIMIT / 2,
                "still waiting"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert!(
            why.contains("exited before it connected") && why.contains("exit status: 3"),
            "{why}"
        );
        assert!(pool.pending.is_empty());
        pool.shutdown().await;
    }

    /// #292: a worker that is still starting when the run ends (cancelled, say) is not left
    /// running: `shutdown` stops and reaps it, though it never joined the pool.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_stops_a_worker_that_never_connected() {
        let mut pool = WorkerPool::start(test_config("never-connects", 1)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // Never connects: a worker held in its start-up.
        pool.launch(
            sh(&format!("echo $$ > {}; exec sleep 60", pid_file.display())),
            0,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !std::fs::read_to_string(&pid_file).is_ok_and(|s| s.ends_with('\n')) {
            assert!(std::time::Instant::now() < deadline, "the worker never ran");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let at = std::time::Instant::now();
        pool.shutdown().await;
        assert!(
            at.elapsed() < WORKER_CONNECT_LIMIT / 2,
            "{:?}",
            at.elapsed()
        );
        let pid: i64 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!crate::db::pid_alive(pid), "the worker is still running");
    }

    /// Connect to the pool's socket as a worker would, with `worker_id` in the hello.
    async fn connect_as(path: &Path, worker_id: Option<usize>) -> UnixStream {
        let mut stream = UnixStream::connect(path).await.unwrap();
        let mut hello = serde_json::json!({
            "type": "hello", "role": "worker",
            "protocol_version": crate::protocol::PROTOCOL_VERSION,
        });
        if let Some(id) = worker_id {
            hello["worker_id"] = id.into();
        }
        write_frame(&mut stream, &hello).await.unwrap();
        stream
    }

    /// #560: workers start together and connect in any order. Each connection joins the pool
    /// as the worker its hello names, so the process a worker handle stops is the one that
    /// connection talks to. A connection naming no worker that is starting is closed.
    #[cfg(unix)]
    #[tokio::test]
    async fn workers_that_connect_out_of_order_are_paired_by_their_hello() {
        let mut pool = WorkerPool::start(test_config("pairing", 3)).unwrap();
        for wid in 0..3 {
            pool.launch(sh("exec sleep 30"), wid).unwrap();
        }
        let pids: HashMap<usize, u32> = pool
            .pending
            .iter()
            .map(|(&wid, p)| (wid, p.child.id()))
            .collect();
        let socket = pool.socket_path.clone();
        let mut clients = Vec::new();
        for wid in [Some(2), Some(0), Some(7), Some(1)] {
            clients.push(connect_as(&socket, wid).await);
            let Some(IoEvent::Connected {
                worker_id,
                pid,
                stream,
            }) = pool.event_rx.recv().await
            else {
                panic!("expected a connection");
            };
            assert_eq!(worker_id, wid);
            assert_eq!(pool.on_connected(worker_id, pid, stream), wid != Some(7));
        }
        assert!(pool.pending.is_empty());
        assert_eq!(pool.workers.len(), 3);
        for (wid, pid) in pids {
            assert_eq!(pool.workers[&wid].child.id(), pid, "worker {wid}");
        }
        // The stranger's connection was acknowledged, then closed.
        let stranger = &mut clients[2];
        let ack: Option<serde_json::Value> = read_frame(stranger).await.unwrap();
        assert_eq!(ack.unwrap()["type"], "hello");
        let closed: Result<Option<serde_json::Value>, _> = read_frame(stranger).await;
        assert!(matches!(closed, Ok(None) | Err(_)));
        pool.shutdown().await;
    }

    /// #560: ready work with no idle worker starts every worker it can use at once, without
    /// waiting on any (one per batch, up to `pool_size`); a one-step run starts one.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_workers_a_wave_needs_start_together_and_no_more() {
        for (ready, pool_size, started) in [(1, 4, 1), (3, 4, 3), (6, 4, 4)] {
            let mut config = test_config("together", pool_size);
            // A stand-in for the interpreter that never connects.
            config.python = PathBuf::from("/bin/true");
            let mut pool = WorkerPool::start(config).unwrap();
            let mut coord = Coordinator::new();
            for i in 0..ready {
                let name = format!("n{i}");
                coord.add_item(
                    crate::StepId {
                        base: std::sync::Arc::from(format!("p.py:{name}")),
                        partition: crate::model::PartitionKey(Default::default()),
                    },
                    test_spec(&name),
                    vec![],
                );
            }
            // Heavy enough that each step is a batch of its own.
            let mut cost = CostModel::new();
            for i in 0..ready {
                cost.observe(&format!("p.py:n{i}"), 5.0, 5.0, 0);
            }
            let cancel = CancellationToken::new();
            pool.assign_ready(&mut coord, &cost, &cancel).await;
            assert_eq!(
                pool.pending.len(),
                started,
                "{ready} ready, pool {pool_size}"
            );
            assert!(pool.workers.is_empty());
            // Nothing is leased before a worker connects.
            assert_eq!(coord.ready_count(), ready);
            // Again, with the same workers still starting: nobody more is started.
            pool.assign_ready(&mut coord, &cost, &cancel).await;
            assert_eq!(pool.pending.len(), started);
            pool.shutdown().await;
        }
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
            let n = 16;
            let resources = tempfile::tempdir().unwrap();
            let socket_path = resources.path().join("worker.sock");
            let branch_root = resources.path().join("branches");
            std::fs::create_dir(&branch_root).unwrap();
            std::fs::write(branch_root.join("owned-result"), "result").unwrap();
            let _ = std::fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path).unwrap();
            let (event_tx, event_rx) = mpsc::channel(8);

            let mut workers = HashMap::new();
            let mut pids = Vec::new();
            let mut readiness = Vec::new();
            for i in 0..n {
                // `exec`: the process that ignores SIGTERM is the child itself. Without it
                // the shell forks `sleep`, and killing the shell leaves the `sleep` running
                // for its 30 seconds with every descriptor it inherited.
                let mut cmd = Command::new("sh");
                let ready = resources.path().join(format!("worker-{i}.ready"));
                cmd.args([
                    "-c",
                    "trap '' TERM; : > \"$1\"; exec sleep 30",
                    "barca-shutdown",
                ])
                .arg(&ready);
                readiness.push(ready);
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
                        batch_open: false,
                        front_since: std::time::Instant::now(),
                    },
                );
            }

            // The shell acknowledges readiness only after installing its ignore
            // disposition. exec keeps its PID/disposition; startup is not timed.
            let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
            let ready = loop {
                if std::time::Instant::now() >= ready_deadline
                    || workers
                        .values_mut()
                        .any(|worker| !matches!(worker.child.try_wait(), Ok(None)))
                {
                    break false;
                }
                if readiness.iter().all(|marker| marker.exists()) {
                    break true;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            if !ready {
                for worker in workers.values_mut() {
                    worker.child.kill().ok();
                    worker.child.wait().ok();
                }
            }
            assert!(
                ready,
                "workers did not acknowledge SIGTERM-ignore readiness"
            );

            let pool = WorkerPool {
                config: IoConfig {
                    upload_snapshot_prefix: None,
                    python: PathBuf::from("python3"),
                    pool_size: n,
                    capacity: n,
                    run_id: "test-shutdown".to_string(),
                    datadog: None,
                    artifact_root: ".".to_string(),
                    storage_options_json: None,
                    agent_mode: false,
                },
                socket_path: socket_path.clone(),
                acceptor: Acceptor(tokio::spawn(accept_connections(listener, event_tx.clone()))),
                event_tx,
                event_rx,
                workers,
                pending: HashMap::new(),
                frozen: Vec::new(),
                next_worker_id: n,
                trace_start: std::time::Instant::now(),
                trace_on: false,
                progress_interval: Duration::ZERO,
                running_hook: None,
                repeated_warnings: Vec::new(),
                branch_root: branch_root.clone(),
                next_branch_group: 0,
                branch_dirs: HashMap::new(),
                failures_survived: 0,
                steps_dispatched: 0,
            };

            let start = std::time::Instant::now();
            pool.shutdown().await;
            let elapsed = start.elapsed();

            // Sixteen ready workers make a sequential grace loop take 3.2s.
            // One quarter of that allows loaded two-CPU scheduling while still
            // rejecting per-worker waits. The lower bound proves ignore/fallback.
            let sequential = Duration::from_millis(n as u64 * 200);
            assert!(
                elapsed >= Duration::from_millis(150) && elapsed < sequential / 4,
                "shutdown of {n} ready SIGTERM-ignoring workers took {elapsed:?} — expected one \
                 shared ~200ms grace window, not ~0 (race) or {sequential:?} (one per worker)"
            );
            assert!(!socket_path.exists(), "owned worker socket remains");
            assert!(!branch_root.exists(), "owned branch results remain");

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
