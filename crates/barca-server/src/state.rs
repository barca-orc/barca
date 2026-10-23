//! Shared server state and in-memory run tracking.

use barca_core::CancellationToken;
use barca_core::RunEvent;
use barca_core::db;
use barca_core::results::{AssetSummary, GetResult, PlanResult};
use dashmap::DashMap;
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::Semaphore;

/// How many runs may execute concurrently by default (one per available core).
fn default_run_concurrency() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Configuration for a `barca serve` instance. Built by the CLI and handed to
/// [`crate::serve`].
#[derive(Clone, Debug)]
pub struct ServeConfig {
    /// Python source files that define the DAG this server operates on.
    pub files: Vec<String>,
    /// Bind address (`--host`, default 127.0.0.1 — local only). There is no auth.
    pub host: IpAddr,
    /// Bind port (default 8274).
    pub port: u16,
    /// Dev-mode hot reload: re-parse the DAG when source files change.
    /// Off by default; has no effect on the production serving path.
    pub watch: bool,
    /// Whether the cron scheduler fires `Schedule(...)` assets. On by default;
    /// disabled with `barca serve --no-schedule`.
    pub schedule: bool,
    /// Timezone cron expressions are evaluated in: `local` (default), `utc`, or
    /// an IANA name like `America/New_York`. Set via `--timezone`.
    pub timezone: String,
    /// Python interpreter used for execution (and dynamic-partition resolution).
    pub python: PathBuf,
    /// Resolved barca configuration (environment, DB path, artifact root, state).
    pub resolved: barca_core::config::ResolvedConfig,
    /// Inspect-only mode (`--read-only`): endpoints that run or cancel work
    /// return 403, the scheduler never starts, and every DB read goes through a
    /// private snapshot — the metadata DB is never opened, locked, created, or
    /// written. Safe to point at a project another process is running.
    pub read_only: bool,
}

/// Lifecycle of an async run tracked by the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Accepted, not yet started.
    Pending,
    /// Executing in a background task.
    Running,
    /// Finished successfully.
    Complete,
    /// Finished with an error.
    Failed,
    /// Stopped mid-flight via `DELETE /runs/{id}` or server shutdown.
    Cancelled,
}

/// In-memory record of a single run, kept while it runs and for a while after. `handle` is
/// the database run id, allocated when the run is accepted.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunState {
    /// The run id (see `/runs/{run_id}`); the older `/status/{run_id}` calls it the handle.
    pub handle: String,
    /// The same id, once the run has written its database row.
    #[serde(skip)]
    pub db_run_id: Option<String>,
    #[serde(skip)]
    pub command: String,
    /// The targets as given, comma-separated (`None`: every asset and sensor).
    #[serde(skip)]
    pub target: Option<String>,
    /// The full node ids the targets resolved to when the run was accepted; empty for a run of
    /// every asset and sensor. The scheduler matches a due job against them, so a run counts
    /// as in flight for its node however the target was spelled.
    #[serde(skip)]
    pub nodes: Vec<String>,
    /// What started the run.
    #[serde(skip)]
    pub trigger: barca_core::outcome::Trigger,
    /// The files the request named (`[]`: the deployed set).
    #[serde(skip)]
    pub files: Vec<String>,
    pub status: RunStatus,
    /// Populated when `status == Complete` for a single-target run. Carries the DB run id,
    /// timing, output.
    pub result: Option<GetResult>,
    /// Populated when the run did not succeed.
    pub error: Option<String>,
    /// Unix epoch seconds when the run was accepted.
    pub started_at: f64,
    /// Unix epoch seconds when the run finished (success or failure).
    pub finished_at: Option<f64>,
    /// The run's full result once it finished.
    #[serde(skip)]
    pub outcome: Option<barca_core::outcome::Outcome>,
    /// Cancels this run's execution future. `DELETE /runs/{id}` triggers it;
    /// the run task observes it, terminates workers, and marks the run
    /// cancelled. Not part of the JSON status payload.
    #[serde(skip)]
    pub cancel: CancellationToken,
    /// `DELETE /runs/{id}` asked for it: a stopping server does not put it back in the queue.
    #[serde(skip)]
    pub cancel_requested: bool,
}

/// Cached inspection results, invalidated by the file watcher in `--watch`
/// mode. Trigger validation additionally checks source stamps on every request.
#[derive(Default)]
pub struct DagCache {
    pub assets: Option<Vec<AssetSummary>>,
    pub plan: Option<PlanResult>,
}

/// The size, modification time and change time of each source file, in the order of
/// `ServeConfig::files`. The change time is the one a program cannot set: an edit that keeps
/// the size and puts the old modification time back (`touch -r`, some sync tools) still
/// moves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStamp(Vec<FileStamp>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    modified: std::time::SystemTime,
    changed: std::time::SystemTime,
    len: u64,
}

impl SourceStamp {
    /// The current state of `files`. `None` when one of them is not a readable regular file
    /// (a directory's own time says nothing about the files in it): then nothing is cached.
    pub fn of(files: &[String]) -> Option<Self> {
        files
            .iter()
            .map(|f| {
                use std::os::unix::fs::MetadataExt;
                let meta = std::fs::metadata(f).ok().filter(|m| m.is_file())?;
                let changed = std::time::UNIX_EPOCH
                    + std::time::Duration::new(
                        u64::try_from(meta.ctime()).ok()?,
                        u32::try_from(meta.ctime_nsec()).ok()?,
                    );
                Some(FileStamp {
                    modified: meta.modified().ok()?,
                    changed,
                    len: meta.len(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }

    /// True when every file was last modified or changed more than [`Self::SETTLED`] before
    /// `now`.
    ///
    /// A file's times are only as fine as the filesystem's clock (a few milliseconds on
    /// Linux). A file read just after it was written can be written again
    /// within the same instant with the same size, and its stamp would not show it. So a
    /// stamp that recent is not kept: the files are read again next time, until they have
    /// been still for a moment.
    pub fn settled(&self, now: std::time::SystemTime) -> bool {
        self.0.iter().all(|file| {
            [file.modified, file.changed]
                .iter()
                .all(|at| now.duration_since(*at).is_ok_and(|age| age > Self::SETTLED))
        })
    }

    const SETTLED: std::time::Duration = std::time::Duration::from_secs(2);
}

#[cfg(test)]
mod stamp_tests {
    use super::SourceStamp;
    use std::time::{Duration, SystemTime};

    #[test]
    fn a_stamp_is_kept_only_once_its_files_have_been_still_for_a_moment() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.py").display().to_string();
        std::fs::write(&file, "x = 1\n").unwrap();
        let stamp = SourceStamp::of(std::slice::from_ref(&file)).unwrap();
        let now = SystemTime::now();
        assert!(!stamp.settled(now), "just written");
        assert!(!stamp.settled(now + Duration::from_secs(1)));
        assert!(stamp.settled(now + Duration::from_secs(5)));
        // A clock that went backwards proves nothing either.
        assert!(!stamp.settled(now - Duration::from_secs(60)));
        // An edit that keeps the size and puts the old modification time back is another
        // stamp too: the change time moved. (The times are a few milliseconds coarse, so
        // the edit is made a moment later.)
        let before = SourceStamp::of(std::slice::from_ref(&file)).unwrap();
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&file, "x = 2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let after = SourceStamp::of(std::slice::from_ref(&file)).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            modified
        );
        assert_ne!(
            after, before,
            "same size, same modification time, other content"
        );
        // A change of size or time is another stamp; a directory or a missing file has none.
        std::fs::write(&file, "x = 12\n").unwrap();
        assert_ne!(SourceStamp::of(std::slice::from_ref(&file)).unwrap(), stamp);
        assert!(SourceStamp::of(&[dir.path().display().to_string()]).is_none());
        assert!(SourceStamp::of(&[format!("{file}.missing")]).is_none());
    }
}

/// One row of `GET /state`: the node's `barca status` entry plus what the
/// table needs beyond it — typical durations and the next cron fire time.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeState {
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub status: barca_core::status::NodeStatus,
    /// Typical wall time over the most recent successful materializations.
    pub durations: Option<Durations>,
    /// Next cron fire time (unix epoch seconds) in the server's `--timezone`, if scheduled.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub next_run: Option<i64>,
    /// The run working on this node now, whoever started it (`null` when none is).
    pub running: Option<crate::running::NodeRunning>,
}

/// Median and p95 wall time over a node's most recent successful materializations.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Durations {
    pub median_seconds: f64,
    pub p95_seconds: f64,
    /// How many materializations these are computed from (at most 20).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub samples: usize,
}

/// Live event channel for one run. Every event is numbered (the SSE `id:`) and kept for the
/// run's lifetime, so a subscriber reads the whole run from the start whenever it connects,
/// and a subscriber that falls behind reads on from where it was: nothing is dropped and
/// nothing is sent twice. Durable history lives in the DB; this is the live tap.
#[derive(Clone)]
pub struct RunChannel {
    backlog: Arc<Mutex<Vec<RunEvent>>>,
    /// How many events have been emitted; subscribers wait on it. Held only by the channel's
    /// handles, so when the last one is dropped the subscribers see it closed.
    emitted: Arc<tokio::sync::watch::Sender<usize>>,
    /// About how many bytes the backlog holds ([`RunEvent::approx_bytes`]): what a finished
    /// run costs the server's memory budget (#623).
    bytes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Default for RunChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl RunChannel {
    pub fn new() -> Self {
        Self {
            backlog: Arc::new(Mutex::new(Vec::new())),
            emitted: Arc::new(tokio::sync::watch::channel(0).0),
            bytes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// About how many bytes the events emitted so far hold.
    pub fn bytes(&self) -> usize {
        self.bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record an event and wake the subscribers.
    pub fn emit(&self, event: RunEvent) {
        self.bytes
            .fetch_add(event.approx_bytes(), std::sync::atomic::Ordering::Relaxed);
        let mut backlog = self.backlog.lock().unwrap();
        backlog.push(event);
        self.emitted.send_replace(backlog.len());
    }

    /// Every event emitted so far, in order.
    pub fn snapshot(&self) -> Vec<RunEvent> {
        self.backlog.lock().unwrap().clone()
    }

    /// The events from number `from` on, each with its number, as they are emitted. Ends after
    /// the run's terminal `run_finished`, or, once every event emitted has been sent, when the
    /// channel is dropped (server shutdown, eviction) before the run finished.
    pub fn subscribe(
        &self,
        from: usize,
    ) -> impl futures::Stream<Item = (usize, RunEvent)> + Send + 'static + use<> {
        struct Cursor {
            next: usize,
            ready: std::collections::VecDeque<(usize, RunEvent)>,
            done: bool,
            closed: bool,
            emitted: tokio::sync::watch::Receiver<usize>,
            backlog: Arc<Mutex<Vec<RunEvent>>>,
        }
        let cursor = Cursor {
            next: from,
            ready: Default::default(),
            done: false,
            closed: false,
            emitted: self.emitted.subscribe(),
            backlog: self.backlog.clone(),
        };
        futures::stream::unfold(cursor, |mut cursor| async move {
            loop {
                if let Some(item) = cursor.ready.pop_front() {
                    cursor.done = item.1.is_terminal();
                    return Some((item, cursor));
                }
                if cursor.done {
                    return None;
                }
                // Mark the current count seen before reading, so an emit after the read
                // wakes `changed` below.
                cursor.emitted.borrow_and_update();
                {
                    let backlog = cursor.backlog.lock().unwrap();
                    if backlog.len() > cursor.next {
                        let from = cursor.next;
                        cursor.ready.extend(
                            backlog[from..]
                                .iter()
                                .cloned()
                                .enumerate()
                                .map(|(i, e)| (from + i, e)),
                        );
                        cursor.next = backlog.len();
                        continue;
                    }
                }
                if cursor.closed {
                    return None;
                }
                if cursor.emitted.changed().await.is_err() {
                    // Dropped: send what is left, then end.
                    cursor.closed = true;
                }
            }
        })
    }
}

/// Durable, mutable view of one scheduled job, published by the scheduler and
/// read by `GET /schedule`. The volatile bits (next fire time, live run status)
/// are computed at request time from `cron` and `last_handle`.
#[derive(Clone, Debug, Serialize)]
pub struct JobStatus {
    /// Full node id (the run target).
    pub id: String,
    /// The node's cron expression.
    pub cron: String,
    /// Node kind (asset / sensor / task).
    pub kind: barca_core::NodeKind,
    /// Last time the scheduler fired this job (unix epoch seconds), if ever.
    pub last_fired: Option<i64>,
    /// Handle of the most recent run the scheduler triggered for this job.
    pub last_handle: Option<String>,
    /// Last time a due tick of this job was skipped (unix epoch seconds) because a run of
    /// its node was already in flight. Kept in memory only; a restart clears it.
    pub last_skipped: Option<i64>,
    /// Handle of the in-flight run that caused `last_skipped` (`None` for a paused job).
    pub skipped_for: Option<String>,
    /// Why the last due tick was skipped: `in_flight` or `paused`.
    pub skip_reason: Option<&'static str>,
    /// Consecutive failed ticks (RFC-0008 rule 9); persisted in `schedule_failures`.
    pub failures: u32,
    /// `Schedule(..., max_failures=N)`: at this many consecutive failed ticks the job is
    /// skipped and reported `failing`.
    pub max_failures: u32,
}

/// Cloneable application state shared across all axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub loaded: Arc<tokio::sync::Mutex<Option<LoadedSources>>>,
    pub load_errors: Arc<RwLock<Vec<barca_core::load::LoadError>>>,
    pub config: Arc<ServeConfig>,
    pub runs: Arc<DashMap<String, RunState>>,
    pub cache: Arc<RwLock<DagCache>>,
    /// Live event channels per run handle (logs + step/run lifecycle).
    pub events: Arc<DashMap<String, RunChannel>>,
    /// Bounds how many runs execute concurrently (`--max-runs`). Runs execute Python in
    /// parallel; the shared metadata.db is kept race-free by a process-wide DB
    /// lock in `barca-core`, not by serializing whole runs.
    pub run_slots: Arc<Semaphore>,
    /// The runs accepted and not started, in order, and whether the queue is paused.
    pub queue: Arc<crate::queue::Queue>,
    /// Cancelled when a drain ends without the runs finishing (its deadline, or a second
    /// SIGTERM/SIGINT); every run token is a child of this, so in-flight runs stop (workers
    /// terminated, runs marked cancelled with [`Self::cancel_reason`]).
    pub shutdown: CancellationToken,
    /// Cancelled by the first SIGTERM/SIGINT: the server drains. New runs are refused with
    /// `503`, the scheduler stops and `/health` says `draining`; runs in flight go on.
    pub draining: CancellationToken,
    /// Why the server cancelled its runs (`server shutdown`), set before [`Self::shutdown`]
    /// is cancelled. Every run executes inside its scope, so the run records the reason.
    pub cancel_reason: barca_core::interrupt::CancelReason,
    /// Every run task, so a drain can wait for them to end.
    pub run_tasks: tokio_util::task::TaskTracker,
    /// Live scheduler view, published by the scheduler and read by `GET /schedule`.
    pub schedule: Arc<RwLock<Vec<JobStatus>>>,
    /// Bumped by the `--watch` file watcher on every DAG invalidation, so the
    /// scheduler can re-read its job set without a restart.
    pub dag_generation: Arc<AtomicU64>,
    /// Paused schedules by node id, read from `schedule_state` on first use and kept in step
    /// with it by the pause and resume routes.
    pub pauses: Arc<tokio::sync::OnceCell<RwLock<HashMap<String, db::SchedulePause>>>>,
    /// When this server started (unix seconds), for `GET /project`'s `deployed_at`.
    pub started_at: i64,
    /// The same, RFC 3339 with milliseconds (`GET /health` `started_at`): a client that
    /// reconnects tells a restarted server from the one it was following.
    pub started: String,
    /// Why the metadata DB cannot be used, while it cannot: set when a read-only server finds
    /// it unusable (it keeps serving) or the scheduler stops on a metadata error, cleared when a
    /// read-only request reads it again. `GET /health` `metadata_error`.
    pub metadata_error: Arc<RwLock<Option<String>>>,
    /// Set when the scheduler stopped on an error; `GET /health` `scheduler_enabled`.
    pub scheduler_stopped: Arc<AtomicBool>,
    /// What the read routes keep between requests ([`crate::read_cache`]).
    pub reads: Arc<crate::read_cache::ReadCache>,
}

impl AppState {
    pub fn new(config: ServeConfig) -> Self {
        let run_slot_count = config
            .resolved
            .server_max_runs
            .unwrap_or_else(default_run_concurrency)
            .max(1);
        let now = chrono::Utc::now();
        Self {
            loaded: Arc::new(tokio::sync::Mutex::new(None)),
            load_errors: Arc::new(RwLock::new(Vec::new())),
            config: Arc::new(config),
            runs: Arc::new(DashMap::new()),
            cache: Arc::new(RwLock::new(DagCache::default())),
            events: Arc::new(DashMap::new()),
            run_slots: Arc::new(Semaphore::new(run_slot_count)),
            queue: Arc::new(crate::queue::Queue::new(run_slot_count)),
            shutdown: CancellationToken::new(),
            draining: CancellationToken::new(),
            cancel_reason: barca_core::interrupt::CancelReason::new(),
            run_tasks: tokio_util::task::TaskTracker::new(),
            schedule: Arc::new(RwLock::new(Vec::new())),
            dag_generation: Arc::new(AtomicU64::new(0)),
            pauses: Arc::new(tokio::sync::OnceCell::new()),
            started_at: now.timestamp(),
            started: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            metadata_error: Arc::new(RwLock::new(None)),
            scheduler_stopped: Arc::new(AtomicBool::new(false)),
            reads: Arc::default(),
        }
    }
}

impl AppState {
    /// Whether this server fires `Schedule(...)` nodes: configured to (not `--no-schedule`,
    /// not `--read-only`) and the scheduler has not stopped on an error.
    pub fn scheduler_enabled(&self) -> bool {
        self.config.schedule
            && !self.config.read_only
            && !self.scheduler_stopped.load(Ordering::Relaxed)
    }

    /// The scheduler stopped for `why`, a metadata DB error: `/health` says so.
    pub fn scheduler_failed(&self, why: String) {
        self.scheduler_stopped.store(true, Ordering::Relaxed);
        *self.metadata_error.write().unwrap() = Some(why);
    }

    /// Record what reading the metadata DB found: an unusable database is reported on
    /// `/health` until a read succeeds again. Other errors (a lock, I/O) leave it as it was.
    pub fn note_metadata<T>(
        &self,
        read: Result<T, barca_core::BarcaError>,
    ) -> Result<T, barca_core::BarcaError> {
        match &read {
            Ok(_) if !self.scheduler_stopped.load(Ordering::Relaxed) => {
                *self.metadata_error.write().unwrap() = None;
            }
            Err(e @ barca_core::BarcaError::MetadataUnusable { .. }) => {
                *self.metadata_error.write().unwrap() = Some(e.to_string());
            }
            _ => {}
        }
        read
    }

    /// Whether the server is draining (or past it): no queued run starts.
    pub fn is_draining(&self) -> bool {
        self.draining.is_cancelled()
    }

    /// The error a cancelled run reports: `run cancelled`, plus the reason when the server
    /// cancelled it (`run cancelled: server shutdown`).
    pub fn cancelled_message(&self) -> String {
        match self.cancel_reason.get() {
            Some(reason) => format!("run cancelled: {reason}"),
            None => "run cancelled".to_string(),
        }
    }

    /// The seconds a client refused during a drain is told to wait (`Retry-After`): the drain
    /// timeout, by when this server is gone and its replacement is likely up. At least 1.
    pub fn retry_after_secs(&self) -> u64 {
        self.config.resolved.drain_timeout_secs.max(1)
    }
}

/// The single selected graph owner for inspection, schedule and execution.
pub struct LoadedSources {
    stamp: Vec<Option<SourceStamp>>,
    dag: Arc<barca_core::dag::Dag>,
}

impl AppState {
    fn source_stamp(&self) -> Vec<Option<SourceStamp>> {
        self.config
            .files
            .iter()
            .map(|file| SourceStamp::of(std::slice::from_ref(file)))
            .collect()
    }

    /// Observe the selected graph without refreshing sources or waiting for loading.
    pub(crate) fn loaded_node_count(&self) -> Option<usize> {
        self.loaded
            .try_lock()
            .ok()
            .and_then(|loaded| loaded.as_ref().map(|sources| sources.dag.node_count()))
    }

    pub async fn loaded_dag(&self) -> Result<Arc<barca_core::dag::Dag>, barca_core::BarcaError> {
        self.refresh_dag(false).await
    }

    /// Execution/status must reread helper cones too; configured-file stamps alone
    /// cannot prove the result hash is unchanged after a helper module edit.
    pub async fn current_dag(&self) -> Result<Arc<barca_core::dag::Dag>, barca_core::BarcaError> {
        self.refresh_dag(true).await
    }

    async fn refresh_dag(
        &self,
        force: bool,
    ) -> Result<Arc<barca_core::dag::Dag>, barca_core::BarcaError> {
        let mut loaded = self.loaded.lock().await;
        let stamp = self.source_stamp();
        if !force
            && let Some(current) = loaded.as_ref()
            && current.stamp == stamp
            && stamp
                .iter()
                .flatten()
                .all(|stamp| stamp.settled(std::time::SystemTime::now()))
        {
            return Ok(current.dag.clone());
        }
        for _ in 0..3 {
            let before = self.source_stamp();
            let (dag, errors) =
                barca_core::load::build_partial_dag(&self.config.files, &self.config.python)
                    .await?;
            if before != self.source_stamp() {
                continue;
            }
            {
                let mut old_errors = self.load_errors.write().unwrap();
                if *old_errors != errors {
                    for error in &errors {
                        barca_core::errln!(
                            "[barca] not loaded: {}: {}{}",
                            error.file,
                            error.error,
                            if error.affected_nodes.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", error.affected_nodes.join(", "))
                            }
                        );
                    }
                }
                *old_errors = errors;
            }
            let dag = Arc::new(dag);
            *loaded = Some(LoadedSources {
                stamp: before,
                dag: dag.clone(),
            });
            let mut cache = self.cache.write().unwrap();
            cache.assets = None;
            cache.plan = None;

            return Ok(dag);
        }
        Err(barca_core::BarcaError::Usage(
            "source files changed during loading; retry after the edit completes".into(),
        ))
    }
}

/// Current time as unix epoch seconds (no external date crate needed).
pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
