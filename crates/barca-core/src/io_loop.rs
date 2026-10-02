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
//! On parallel(), the requesting worker is SIGSTOP'd, a temp replacement is
//! spawned, and children enter the ready queue. When all children complete,
//! the temp is killed and the original is SIGCONT'd.
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
use crate::protocol::{CoordinatorMessage, ParallelResult, WorkerMessage, read_frame, write_frame};

// ─── Configuration ───────────────────────────────────────────────────────────

pub struct IoConfig {
    pub python: PathBuf,
    pub pool_size: usize,
    pub run_id: String,
    /// Local artifact directory workers write to and read from. Always local:
    /// a separate artifact store is synced by `transfer::TransferClient`.
    /// Set explicitly on every worker so env-separated layouts work
    /// regardless of the coordinator's own environment.
    pub artifact_root: String,
    /// Merged fsspec storage options (JSON), forwarded to workers (for remote
    /// `@sink` destinations).
    pub storage_options_json: Option<String>,
}

/// Callback invoked on each step completion with (node_id, artifact_json).
/// `Send` so the whole run future can be spawned onto a multi-thread runtime.
pub type StepCallback<'a> = Box<dyn FnMut(&str, &serde_json::Value) + Send + 'a>;

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
    /// The active worker ID that replaced this frozen one.
    replacement_id: usize,
}

// ─── Events ──────────────────────────────────────────────────────────────────

enum IoEvent {
    Message {
        worker_id: usize,
        msg: WorkerMessage,
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
                        if event_tx.send(IoEvent::Message { worker_id, msg }).await.is_err() {
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
}

impl WorkerPool {
    /// Bind the coordination socket. Workers spawn on demand during phases.
    /// Must be called from within a tokio runtime context.
    pub fn start(config: IoConfig) -> Result<Self, String> {
        let socket_path = crate::protocol::socket_path(&config.run_id, "main");
        std::fs::remove_file(&socket_path).ok();
        let listener = UnixListener::bind(&socket_path).map_err(|e| format!("socket bind: {e}"))?;
        let (event_tx, event_rx) = mpsc::channel::<IoEvent>(config.pool_size.max(1) * 8);
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
        })
    }

    /// Report steps that stay in flight longer than the progress interval, so a long
    /// step does not look like a hang.
    pub fn on_running(&mut self, hook: RunningHook) {
        self.running_hook = Some(hook);
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
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("run cancelled".to_string());
        }
        self.assign_ready(coord, cost).await;

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
                    if !running.is_empty() {
                        if let Some(hook) = self.running_hook.as_mut() {
                            hook(&running);
                        }
                    }
                    continue;
                }
            };

            match event {
                IoEvent::Message { worker_id, msg } => match msg {
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
                            cb(node_id, &artifact_val);
                        }
                        coord.on_item_completed(item_id);

                        // Check if any frozen worker's group is now complete
                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost).await;
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
                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(item_id, error)
                        {
                            schedule_retry(&self.event_tx, item_id, delay);
                        }

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
                        self.assign_ready(coord, cost).await;
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
                        self.assign_ready(coord, cost).await;
                    }
                    WorkerMessage::Submit { items } => {
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

                        // The parent blocks frozen on its group; anything else
                        // this worker had leased goes back to the queue.
                        let mut handle = self
                            .workers
                            .remove(&worker_id)
                            .expect("worker existence checked above");
                        handle.leases.pop_front();
                        Self::return_leases(&mut handle, coord);

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
                        )
                        .await
                        {
                            Ok(replacement) => {
                                self.workers.insert(replacement_id, replacement);
                            }
                            Err(e) => {
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
                            replacement_id,
                        });

                        // Assign ready items (children are now in the ready queue)
                        self.assign_ready(coord, cost).await;
                    }
                    WorkerMessage::Heartbeat => {}
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
                        }
                        Self::return_leases(&mut handle, coord);
                        tokio::task::spawn_blocking(move || {
                            let _ = handle.child.kill();
                            let _ = handle.child.wait();
                        });
                    }
                    self.resume_frozen(coord).await;
                    self.assign_ready(coord, cost).await;
                }
                IoEvent::RetryReady { item_id } => {
                    coord.requeue(item_id);
                    self.assign_ready(coord, cost).await;
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
        std::fs::remove_file(&self.socket_path).ok();
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
    async fn assign_ready(&mut self, coord: &mut Coordinator, cost: &CostModel) {
        loop {
            if coord.ready_count() == 0 {
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
                    )
                    .await
                    {
                        Ok(handle) => {
                            self.workers.insert(wid, handle);
                            wid
                        }
                        Err(e) => {
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
                let step = build_step_json(coord.item(batch[0]), coord);
                serde_json::json!({"type": "execute", "step": step})
            } else {
                let steps: Vec<serde_json::Value> = batch
                    .iter()
                    .map(|&iid| build_step_json(coord.item(iid), coord))
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

    /// Resume frozen workers whose parallel groups completed.
    async fn resume_frozen(&mut self, coord: &mut Coordinator) {
        // Partition: completed groups get drained out
        let mut i = 0;
        while i < self.frozen.len() {
            if coord.is_group_complete(self.frozen[i].group_id) {
                let fw = self.frozen.swap_remove(i);

                // Kill the replacement worker. Its in-flight item (if any) is
                // deliberately returned rather than failed: the kill is ours,
                // not the task's — re-running is safe under the pure-asset
                // contract (at-least-once), and no retry budget is charged
                // for an interruption the task didn't cause.
                if let Some(mut replacement) = self.workers.remove(&fw.replacement_id) {
                    if let Some(in_flight) = replacement.leases.pop_front() {
                        coord.return_leased(in_flight);
                    }
                    Self::return_leases(&mut replacement, coord);
                    let _ = replacement.child.kill();
                    let _ = replacement.child.wait();
                }

                // SIGCONT the original worker
                #[cfg(unix)]
                unsafe {
                    libc::kill(fw.child.id() as i32, libc::SIGCONT);
                }

                // Build ParallelResponse with results for each child.
                // Read actual JSON values from the artifact files so the parent
                // task receives the real return values (not Null).
                let group = coord.group(fw.group_id);
                let results: Vec<ParallelResult> = group
                    .items
                    .iter()
                    .map(|&iid| {
                        if coord.is_done(iid) {
                            let val = coord
                                .outputs()
                                .get(&iid)
                                .and_then(|artifact| {
                                    let fmt = artifact
                                        .get("format")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let path =
                                        artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
                                    // Workers always write locally, so the
                                    // child's artifact is on this disk.
                                    if fmt == "json" && !path.is_empty() {
                                        std::fs::read_to_string(path)
                                            .ok()
                                            .and_then(|s| serde_json::from_str(&s).ok())
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(serde_json::Value::Null);
                            ParallelResult::Ok { result: val }
                        } else {
                            let error = coord
                                .failed_items()
                                .into_iter()
                                .find(|(fid, _)| *fid == iid)
                                .map(|(_, msg)| msg.to_string())
                                .unwrap_or_else(|| "failed".to_string());
                            ParallelResult::Error { error }
                        }
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
                        // Still executing the parent task.
                        leases: VecDeque::from([fw.parent_item]),
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

async fn spawn_worker(
    config: &IoConfig,
    socket_path: &Path,
    worker_id: usize,
    listener: &UnixListener,
    event_tx: &mpsc::Sender<IoEvent>,
) -> Result<WorkerHandle, String> {
    let mut cmd = Command::new(&config.python);
    cmd.args(["-m", "barca._worker", "--daemon"])
        .env("BARCA_SOCKET", socket_path.to_str().unwrap_or(""))
        .env("BARCA_WORKER", "1")
        .env("BARCA_WORKER_ID", worker_id.to_string())
        .env("BARCA_ARTIFACT_URI", &config.artifact_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null());
    if let Some(ref opts) = config.storage_options_json {
        cmd.env("BARCA_STORAGE_OPTIONS", opts);
    }
    let trace_on = std::env::var("BARCA_TRACE_TIMING").is_ok();
    let t_spawn = std::time::Instant::now();
    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    if trace_on {
        eprintln!(
            "[trace]  worker {worker_id} process spawned in {:.1}ms",
            t_spawn.elapsed().as_secs_f64() * 1000.0
        );
    }

    let t_accept = std::time::Instant::now();
    let stream = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .map_err(|_| format!("timeout waiting for worker {worker_id} to connect"))?
        .map_err(|e| format!("accept: {e}"))?
        .0;
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
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(200));
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => {}
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
    })
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
                },
                crate::dispatch::OutputRef {
                    path: "f--source_key_b.json".to_string(),
                    format: "json".to_string(),
                    size_bytes: 12,
                    elapsed_seconds: None,
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
                let child = Command::new("sh")
                    .args(["-c", "trap '' TERM; sleep 30"])
                    .spawn()
                    .unwrap();
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
