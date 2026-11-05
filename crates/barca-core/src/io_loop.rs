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

mod leases;
mod lifecycle;
mod parallel;
mod phase;
mod shared;

use crate::protocol::build_step_json;
use lifecycle::*;
use parallel::*;
use shared::{Share, Steps, share_changed};

// ─── Configuration ───────────────────────────────────────────────────────────

pub struct IoConfig {
    pub upload_snapshot_prefix: Option<String>,
    pub python: PathBuf,
    pub pool_size: usize,
    /// The most the weights of the steps running at once may add up to (#436): the pool size
    /// unless `[execution] capacity` or `BARCA_CAPACITY` sets it.
    pub capacity: usize,
    /// What hosts the run ([`crate::run_host`]). Under `barca serve`: a step is leased only once
    /// the capacity the server's runs share admits it too (`[server] capacity`, #628), and a
    /// step another run is executing is waited for instead of executed again (#654).
    pub host: std::sync::Arc<dyn crate::run_host::RunHost>,
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
    /// The leases are the keys of one batched call (RFC-0013): they all run at once, so a
    /// worker that dies fails all of them.
    call: bool,
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
    /// Another run's claim on a step this run waited for ended ([`crate::serve::claims`]): with the
    /// owner's artifact when it finished the step, `None` when this run must execute it.
    Settled {
        item_id: ItemId,
        artifact: Option<serde_json::Value>,
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
    /// Workers removed from the pool whose kill/wait jobs are still running.
    reaping: Vec<tokio::task::JoinHandle<()>>,
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
    /// Under a capacity the server's runs share: this run's part of it.
    share: Option<Share>,
    /// Under `barca serve`: this run's claims on the asset steps its runs share.
    steps: Option<Steps>,
}

/// Where workers claim "first in this run to print this warning": a directory next to the
/// coordination socket. `python/barca/_dedupe.py` derives the same path from `BARCA_SOCKET`.
fn warning_claims_dir(socket_path: &Path) -> PathBuf {
    let mut dir = socket_path.as_os_str().to_owned();
    dir.push(".warnings");
    PathBuf::from(dir)
}

impl WorkerPool {}

// ─── Worker spawning ─────────────────────────────────────────────────────────

/// How long a worker may take from being started to connecting.
const WORKER_CONNECT_LIMIT: Duration = Duration::from_secs(10);

/// How long a disconnect waits for the worker's exit status. The socket can close a moment
/// before the kernel has finished tearing the process down; a worker that is still alive
/// after this is reported with the plain "worker disconnected" wording.
const EXIT_STATUS_WAIT: Duration = Duration::from_millis(300);

#[cfg(test)]
mod tests;
