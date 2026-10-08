//! Shared server state and in-memory run tracking.

use barca_core::CancellationToken;
use barca_core::RunEvent;
use barca_core::commands::{AssetSummary, GetResult, PlanResult};
use dashmap::DashMap;
use serde::Serialize;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{Semaphore, broadcast};

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
    /// Bind address (defaults to 127.0.0.1 — local only, no auth).
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
    /// Stopped mid-flight via `DELETE /run/{id}` or server shutdown.
    Cancelled,
}

/// In-memory record of a single run. The server-side `handle` is the polling id
/// returned by `POST /run`; the real DB run id lives inside `result` once complete.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunState {
    /// Server-side polling handle (see `/status/{run_id}`).
    pub handle: String,
    pub status: RunStatus,
    /// Populated when `status == Complete`. Carries the DB run id, timing, output.
    pub result: Option<GetResult>,
    /// Populated when `status == Failed`.
    pub error: Option<String>,
    /// Unix epoch seconds when the run was accepted.
    pub started_at: f64,
    /// Unix epoch seconds when the run finished (success or failure).
    pub finished_at: Option<f64>,
    /// Cancels this run's execution future. `DELETE /run/{id}` triggers it;
    /// the run task observes it, terminates workers, and marks the run
    /// cancelled. Not part of the JSON status payload.
    #[serde(skip)]
    pub cancel: CancellationToken,
}

/// Cached static-analysis results, invalidated by the file watcher in `--watch`
/// mode. Without `--watch` the cache simply persists for the process lifetime.
#[derive(Default)]
pub struct DagCache {
    pub assets: Option<Vec<AssetSummary>>,
    pub plan: Option<PlanResult>,
    /// The nodes a trigger's target is checked against, with the state of the source files
    /// they were read from. Unlike the two above it is never older than the source: a check
    /// uses it only while every file still has the size and modification time recorded here,
    /// so it names exactly the nodes a run started now would find.
    pub targets: Option<TargetIndex>,
}

/// Every node's id and kind, as read from source files in the state `stamp` describes.
#[derive(Clone)]
pub struct TargetIndex {
    pub stamp: SourceStamp,
    pub nodes: Arc<Vec<(String, barca_core::NodeKind)>>,
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

/// Live event channel for one run: a broadcast for subscribers plus a backlog
/// so a client that subscribes a beat after the run starts still receives the
/// events emitted before it connected (the "replay backlog, then stream live"
/// pattern). Durable history lives in the DB; this is the live tap.
#[derive(Clone)]
pub struct RunChannel {
    tx: broadcast::Sender<RunEvent>,
    backlog: Arc<Mutex<Vec<RunEvent>>>,
}

impl Default for RunChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl RunChannel {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            tx,
            backlog: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Record an event to the backlog and broadcast it to live subscribers.
    /// Push-before-send under the lock so a subscriber snapshotting the backlog
    /// (while holding the lock) can't miss or duplicate an event.
    pub fn emit(&self, event: RunEvent) {
        let mut backlog = self.backlog.lock().unwrap();
        backlog.push(event.clone());
        let _ = self.tx.send(event);
    }

    /// Snapshot the current backlog and subscribe to live events atomically.
    pub fn snapshot_and_subscribe(&self) -> (Vec<RunEvent>, broadcast::Receiver<RunEvent>) {
        let backlog = self.backlog.lock().unwrap();
        let snapshot = backlog.clone();
        let rx = self.tx.subscribe();
        (snapshot, rx)
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
}

/// Cloneable application state shared across all axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServeConfig>,
    pub runs: Arc<DashMap<String, RunState>>,
    pub cache: Arc<RwLock<DagCache>>,
    /// Live event channels per run handle (logs + step/run lifecycle).
    pub events: Arc<DashMap<String, RunChannel>>,
    /// Bounds how many runs execute concurrently. Runs execute Python in
    /// parallel; the shared metadata.db is kept race-free by a process-wide DB
    /// lock in `barca-core`, not by serializing whole runs.
    pub run_slots: Arc<Semaphore>,
    /// Total permits in `run_slots` — lets shutdown wait for in-flight runs by
    /// re-acquiring every permit.
    pub run_slot_count: usize,
    /// Cancelled on graceful shutdown; every run token is a child of this, so
    /// Ctrl-C stops in-flight runs (workers terminated, runs marked cancelled).
    pub shutdown: CancellationToken,
    /// Live scheduler view, published by the scheduler and read by `GET /schedule`.
    pub schedule: Arc<RwLock<Vec<JobStatus>>>,
    /// Bumped by the `--watch` file watcher on every DAG invalidation, so the
    /// scheduler can re-read its job set without a restart.
    pub dag_generation: Arc<AtomicU64>,
}

impl AppState {
    pub fn new(config: ServeConfig) -> Self {
        let run_slot_count = default_run_concurrency();
        Self {
            config: Arc::new(config),
            runs: Arc::new(DashMap::new()),
            cache: Arc::new(RwLock::new(DagCache::default())),
            events: Arc::new(DashMap::new()),
            run_slots: Arc::new(Semaphore::new(run_slot_count)),
            run_slot_count,
            shutdown: CancellationToken::new(),
            schedule: Arc::new(RwLock::new(Vec::new())),
            dag_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Current time as unix epoch seconds (no external date crate needed).
pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
