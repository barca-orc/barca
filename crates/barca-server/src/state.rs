//! Shared server state and in-memory run tracking.

use barca_core::CancellationToken;
use barca_core::RunEvent;
use barca_core::results::{AssetSummary, GetResult, PlanResult};
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
    /// Durable id assigned by the engine, including failed and cancelled runs.
    #[serde(skip)]
    pub db_run_id: Option<String>,
    #[serde(skip)]
    pub command: String,
    #[serde(skip)]
    pub target: Option<String>,
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
    pub loaded: Arc<tokio::sync::Mutex<Option<LoadedSources>>>,
    pub load_errors: Arc<RwLock<Vec<barca_core::load::LoadError>>>,
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
            loaded: Arc::new(tokio::sync::Mutex::new(None)),
            load_errors: Arc::new(RwLock::new(Vec::new())),
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
