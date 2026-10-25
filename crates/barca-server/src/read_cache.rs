//! What the read routes share between requests, so a poll that finds nothing changed costs
//! a few `stat` calls instead of a parse, a copy of the metadata DB and a cache decision for
//! every node and key.
//!
//! Three things are kept, each with the evidence that it is still right:
//!
//! - **The inspected graph** ([`AppState::inspect_dag`]): the graph `GET /state`, `GET /nodes`
//!   and the schema route decide cache states with. It is loaded again when a pipeline file, a
//!   helper module its steps read, or a path an import was looked up at changes, when `--watch`
//!   sees an edit, and, for a graph with dynamic partition keys (`partitions(<expression>)`,
//!   which can read anything), at least every [`DYNAMIC_KEYS_MAX_AGE`].
//! - **The snapshot of the metadata DB** ([`AppState::read_snapshot`]): one private copy shared
//!   by every request until the database or its WAL changes on disk.
//! - **Status results** ([`AppState::cached_status`]): what `barca status` computes, per graph,
//!   database state and request. One older than [`STATUS_MAX_AGE`] is computed again in the
//!   background, which bounds how long an artifact deleted by hand (outside barca) can still
//!   show as cached.
//!
//! A state of files is only trusted once it has been still for a moment: a file can be written
//! twice within the resolution of its timestamps, with the same size, and the second write
//! would not show ([`Stamp::settled`]). Until then nothing is kept.

use crate::handlers::SnapshotDb;
use crate::state::AppState;
use barca_core::BarcaError;
use barca_core::dag::Dag;
use barca_core::status::StatusResult;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How long a kept status result is used at most.
pub(crate) const STATUS_MAX_AGE: Duration = Duration::from_secs(30);

/// How long a graph with dynamic partition keys is used at most before they are evaluated
/// again.
pub(crate) const DYNAMIC_KEYS_MAX_AGE: Duration = Duration::from_secs(30);

/// How long source files must have been still before their state is trusted. Source files are
/// written by people and editors, rarely twice in a second.
const SOURCES_SETTLED: Duration = Duration::from_secs(2);

/// The same for the metadata DB, which barca writes many times a second during a run: longer
/// than the timestamp resolution of any filesystem barca runs on, short enough that the cache
/// works between runs.
const DB_SETTLED: Duration = Duration::from_millis(100);

/// One path's state: missing, or its identity, size and times.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileState {
    dev: u64,
    ino: u64,
    len: u64,
    modified: SystemTime,
    /// The change time, which a program cannot set: an edit that puts the old modification
    /// time back still moves it.
    changed: SystemTime,
}

impl FileState {
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        let changed = std::time::UNIX_EPOCH
            + Duration::new(
                u64::try_from(meta.ctime()).ok()?,
                u32::try_from(meta.ctime_nsec()).ok()?,
            );
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            modified: meta.modified().ok()?,
            changed,
        })
    }
}

/// The state of a list of paths, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp(Vec<(PathBuf, Option<FileState>)>);

impl Stamp {
    pub(crate) fn of(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self(
            paths
                .into_iter()
                .map(|p| {
                    let state = FileState::of(&p);
                    (p, state)
                })
                .collect(),
        )
    }

    /// The same paths, read again now.
    fn again(&self) -> Self {
        Self::of(self.0.iter().map(|(p, _)| p.clone()))
    }

    /// True when no path was modified or changed within `still` before `now`.
    fn settled(&self, now: SystemTime, still: Duration) -> bool {
        self.0.iter().filter_map(|(_, s)| s.as_ref()).all(|s| {
            [s.modified, s.changed]
                .iter()
                .all(|at| now.duration_since(*at).is_ok_and(|age| age > still))
        })
    }
}

/// The metadata database and its WAL.
fn db_stamp(db_path: &str) -> Stamp {
    Stamp::of([
        PathBuf::from(db_path),
        PathBuf::from(format!("{db_path}-wal")),
    ])
}

/// The paths whose change can change `dag`: the pipeline files, every project file loading
/// looked at, and the directories they are in (a file created in one changes what an import
/// resolves to).
fn source_paths(files: &[String], dag: &Dag) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = files
        .iter()
        .map(PathBuf::from)
        .chain(dag.files_read.iter().cloned())
        .collect();
    let dirs: Vec<PathBuf> = paths
        .iter()
        .map(|p| barca_core::load::source_dir(p))
        .collect();
    paths.extend(dirs);
    paths.sort();
    paths.dedup();
    paths
}

struct InspectedDag {
    dag: Arc<Dag>,
    sources: Stamp,
    /// [`AppState::dag_generation`] when it was loaded: `--watch` bumps it on every edit.
    generation: u64,
    loaded_at: Instant,
    dynamic_keys: bool,
}

struct SharedSnapshot {
    db: Stamp,
    snapshot: Arc<SnapshotDb>,
}

/// What a status result was computed from.
#[derive(Clone)]
struct StatusKey {
    dag: Arc<Dag>,
    db: Stamp,
}

impl StatusKey {
    fn same(&self, other: &StatusKey) -> bool {
        Arc::ptr_eq(&self.dag, &other.dag) && self.db == other.db
    }
}

/// A status request: its targets, sample size and whether shapes are read.
pub(crate) type StatusQuery = (Vec<String>, usize, bool);

#[derive(Default)]
struct StatusResults {
    /// What every result here was computed from; a new key starts over.
    key: Option<StatusKey>,
    /// Each request's result, with when it was computed.
    results: HashMap<StatusQuery, (Instant, Arc<StatusResult>)>,
    /// Requests whose result is being computed again in the background.
    refreshing: std::collections::HashSet<StatusQuery>,
    /// Typical durations, `GET /state`'s other expensive part, with when they were read.
    durations: Option<(Instant, Arc<HashMap<String, crate::state::Durations>>)>,
}

/// What [`AppState::cached_status`] finds kept for a request.
enum Kept {
    Fresh(Arc<StatusResult>),
    /// Older than [`STATUS_MAX_AGE`]: answered, and computed again in the background.
    Old(Arc<StatusResult>),
    Nothing,
}

/// The kept reads. One per server, shared by every handler (`AppState::reads`).
#[derive(Default)]
pub struct ReadCache {
    dag: Mutex<Option<InspectedDag>>,
    snapshot: tokio::sync::Mutex<Option<SharedSnapshot>>,
    status: Mutex<StatusResults>,
    /// Held while a status result is computed, so that concurrent polls compute it once.
    computing: tokio::sync::Mutex<()>,
    /// When `GET /state` was last asked (unix seconds): [`keep_warm`] works only while
    /// someone is looking.
    last_poll: std::sync::atomic::AtomicI64,
    /// How many times a graph was loaded for inspection and a status was computed (tests).
    pub(crate) dag_loads: std::sync::atomic::AtomicUsize,
    pub(crate) status_computes: std::sync::atomic::AtomicUsize,
}

impl ReadCache {
    /// Note a `GET /state` from a client.
    pub(crate) fn polled(&self) {
        self.last_poll
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    }

    fn polled_within(&self, window: Duration) -> bool {
        let last = self.last_poll.load(Ordering::Relaxed);
        last > 0 && chrono::Utc::now().timestamp() - last <= window.as_secs() as i64
    }
}

/// How often [`keep_warm`] looks for a change.
const WARM_EVERY: Duration = Duration::from_secs(1);

/// [`keep_warm`] works while a client asked for `GET /state` within this long (the web UI asks
/// every 10 seconds while it is open).
const WARM_WHILE_POLLED: Duration = Duration::from_secs(60);

/// How long the metadata DB must have been still before [`keep_warm`] computes for it: a run
/// writes it many times a second, and its polls compute what they need.
const WARM_AFTER_STILL: Duration = Duration::from_secs(1);

/// `barca serve`'s background task: compute `GET /state` once the server is up (and its
/// database still), and again
/// whenever the sources or the database changed and have settled, while someone is polling and
/// no run of this server is going, so a poll finds it computed instead of waiting for it. With
/// nothing changed, a check is a few `stat` calls.
pub(crate) async fn keep_warm(state: AppState) {
    // The first time without waiting for a client, once start-up's own writes have settled.
    let mut started = false;
    loop {
        tokio::time::sleep(WARM_EVERY).await;
        if state.is_draining() || state.shutdown.is_cancelled() {
            return;
        }
        let busy = state.runs.iter().any(|r| {
            matches!(
                r.status,
                crate::state::RunStatus::Pending | crate::state::RunStatus::Running
            )
        });
        if busy
            || (started && !state.reads.polled_within(WARM_WHILE_POLLED))
            || !state
                .db_state()
                .settled(SystemTime::now(), WARM_AFTER_STILL)
        {
            continue;
        }
        started = true;
        let _ = crate::handlers::state_rows(&state).await;
    }
}

impl AppState {
    /// The graph to decide cache states with: the sources as they are now, helper modules
    /// included. Loaded again only when something it was loaded from changed (see the module
    /// documentation); otherwise the kept one.
    pub async fn inspect_dag(&self) -> Result<Arc<Dag>, BarcaError> {
        let generation = self.dag_generation.load(Ordering::Relaxed);
        let kept = {
            let kept = self.reads.dag.lock().unwrap();
            kept.as_ref().and_then(|k| {
                let fresh = k.generation == generation
                    && !(k.dynamic_keys && k.loaded_at.elapsed() >= DYNAMIC_KEYS_MAX_AGE);
                fresh.then(|| (k.dag.clone(), k.sources.clone()))
            })
        };
        if let Some((dag, sources)) = kept
            && sources.again() == sources
        {
            return Ok(dag);
        }
        let dag = self.current_dag().await?;
        self.reads.dag_loads.fetch_add(1, Ordering::Relaxed);
        let sources = Stamp::of(source_paths(&self.config.files, &dag));
        // An edit within the last moment may not show in the stamp yet: keep nothing.
        let keep = sources.settled(SystemTime::now(), SOURCES_SETTLED);
        let mut kept = self.reads.dag.lock().unwrap();
        *kept = keep.then(|| InspectedDag {
            dynamic_keys: has_dynamic_keys(&dag),
            dag: dag.clone(),
            sources,
            generation,
            loaded_at: Instant::now(),
        });
        Ok(dag)
    }

    /// A private snapshot of the metadata DB, shared with every other request until the
    /// database changes on disk.
    pub(crate) async fn read_snapshot(&self) -> Result<Arc<SnapshotDb>, BarcaError> {
        let db_path = &self.config.resolved.db_path;
        let mut shared = self.reads.snapshot.lock().await;
        let now = db_stamp(db_path);
        if let Some(s) = shared.as_ref()
            && s.db == now
        {
            return Ok(s.snapshot.clone());
        }
        let snapshot = Arc::new(crate::handlers::snapshot_of(db_path).await?);
        // Taken after the stamp: a write in between makes the next request take another copy.
        *shared = now
            .settled(SystemTime::now(), DB_SETTLED)
            .then(|| SharedSnapshot {
                db: now,
                snapshot: snapshot.clone(),
            });
        Ok(snapshot)
    }

    /// The state of the metadata DB on disk now, for keying what is computed from it.
    pub(crate) fn db_state(&self) -> Stamp {
        db_stamp(&self.config.resolved.db_path)
    }

    /// `compute()`'s status result for `query` over `dag` and the database in state `db`,
    /// kept until either changes. Concurrent callers compute it once. A result older than
    /// [`STATUS_MAX_AGE`] is still answered, once, while it is computed again in the
    /// background, so a poll never waits for the recomputation that bounds its age.
    pub(crate) async fn cached_status<F, Fut>(
        &self,
        dag: &Arc<Dag>,
        db: &Stamp,
        query: StatusQuery,
        compute: F,
    ) -> Result<Arc<StatusResult>, BarcaError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<StatusResult, BarcaError>> + Send + 'static,
    {
        let key = StatusKey {
            dag: dag.clone(),
            db: db.clone(),
        };
        match self.kept_status(&key, &query) {
            Kept::Fresh(result) => return Ok(result),
            Kept::Old(result) => {
                let state = self.clone();
                let db = db.clone();
                tokio::spawn(async move {
                    let _computing = state.reads.computing.lock().await;
                    let fresh = off_the_workers(compute).await;
                    let mut kept = state.reads.status.lock().unwrap();
                    kept.refreshing.remove(&query);
                    if let Ok(result) = fresh
                        && kept.key.as_ref().is_some_and(|k| k.same(&key))
                        && db.settled(SystemTime::now(), DB_SETTLED)
                    {
                        state.reads.status_computes.fetch_add(1, Ordering::Relaxed);
                        kept.results
                            .insert(query, (Instant::now(), Arc::new(result)));
                    }
                });
                return Ok(result);
            }
            Kept::Nothing => {}
        }
        let _computing = self.reads.computing.lock().await;
        if let Kept::Fresh(result) | Kept::Old(result) = self.kept_status(&key, &query) {
            return Ok(result);
        }
        let result = Arc::new(off_the_workers(compute).await?);
        self.reads.status_computes.fetch_add(1, Ordering::Relaxed);
        if db.settled(SystemTime::now(), DB_SETTLED) {
            let mut kept = self.reads.status.lock().unwrap();
            renew(&mut kept, &key);
            kept.results.insert(query, (Instant::now(), result.clone()));
        }
        Ok(result)
    }

    /// Typical durations for the database in state `db`, kept like a status result (read
    /// again once older than [`STATUS_MAX_AGE`]; they are cheap).
    pub(crate) async fn cached_durations<F, Fut>(
        &self,
        dag: &Arc<Dag>,
        db: &Stamp,
        compute: F,
    ) -> Result<Arc<HashMap<String, crate::state::Durations>>, BarcaError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<
                Output = Result<HashMap<String, crate::state::Durations>, BarcaError>,
            >,
    {
        let key = StatusKey {
            dag: dag.clone(),
            db: db.clone(),
        };
        {
            let kept = self.reads.status.lock().unwrap();
            if kept.key.as_ref().is_some_and(|k| k.same(&key))
                && let Some((born, d)) = &kept.durations
                && born.elapsed() < STATUS_MAX_AGE
            {
                return Ok(d.clone());
            }
        }
        let durations = Arc::new(compute().await?);
        if db.settled(SystemTime::now(), DB_SETTLED) {
            let mut kept = self.reads.status.lock().unwrap();
            renew(&mut kept, &key);
            kept.durations = Some((Instant::now(), durations.clone()));
        }
        Ok(durations)
    }

    /// What is kept for `query` under `key`; an old result is handed out for one background
    /// recomputation at a time, and fresh-looking until it lands.
    fn kept_status(&self, key: &StatusKey, query: &StatusQuery) -> Kept {
        let mut kept = self.reads.status.lock().unwrap();
        if !kept.key.as_ref().is_some_and(|k| k.same(key)) {
            return Kept::Nothing;
        }
        let Some((born, result)) = kept.results.get(query).cloned() else {
            return Kept::Nothing;
        };
        if born.elapsed() < STATUS_MAX_AGE || !kept.refreshing.insert(query.clone()) {
            return Kept::Fresh(result);
        }
        Kept::Old(result)
    }
}

/// Run `compute()` on a blocking-pool thread: deciding the cache state of 200k keys takes
/// seconds of CPU with few waits, and on a runtime worker it would hold up the requests queued
/// behind it there (a `/health` waited for a whole status computation).
async fn off_the_workers<F, Fut>(compute: F) -> Result<StatusResult, BarcaError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<StatusResult, BarcaError>> + Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(compute()))
        .await
        .map_err(|e| BarcaError::Other(format!("status computation failed: {e}")))?
}

/// Start `kept` over when it holds results for another key than `key`.
fn renew(kept: &mut StatusResults, key: &StatusKey) {
    if !kept.key.as_ref().is_some_and(|k| k.same(key)) {
        *kept = StatusResults {
            key: Some(key.clone()),
            ..Default::default()
        };
    }
}

fn has_dynamic_keys(dag: &Dag) -> bool {
    dag.topo_order().into_iter().any(|id| {
        dag.get_node(id).is_some_and(|n| {
            n.extracted
                .partitions
                .values()
                .any(|p| matches!(p, barca_core::model::PartitionSpec::Dynamic { .. }))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIPELINE: &str = "from barca import asset\nfrom helpers import compute\n\n\
        @asset()\ndef first() -> dict:\n    return {\"n\": compute()}\n\n\
        @asset(inputs={\"first\": first})\ndef second(first: dict) -> dict:\n    return first\n";

    fn state_in(dir: &Path) -> AppState {
        let file = dir.join("pipeline.py");
        std::fs::write(&file, PIPELINE).unwrap();
        std::fs::write(dir.join("helpers.py"), "def compute():\n    return 1\n").unwrap();
        let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
        // Match the production layout: WAL sidecar churn belongs below .barca,
        // outside the import directory stamps whose changes invalidate the graph.
        std::fs::create_dir_all(dir.join(".barca")).unwrap();
        resolved.db_path = dir.join(".barca/metadata.db").display().to_string();
        AppState::new(crate::ServeConfig {
            files: vec![file.display().to_string()],
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: "/nonexistent-barca-python".into(),
            resolved,
            read_only: false,
        })
    }

    async fn poll(state: &AppState) -> Vec<crate::state::NodeState> {
        crate::handlers::state(axum::extract::State(state.clone()))
            .await
            .map_err(|e| format!("{e:?}"))
            .unwrap()
            .0
    }

    fn loads(state: &AppState) -> (usize, usize) {
        (
            state.reads.dag_loads.load(Ordering::Relaxed),
            state.reads.status_computes.load(Ordering::Relaxed),
        )
    }

    /// The regression guard: a poll that finds nothing changed neither parses the sources nor
    /// decides a cache state again. An edit to a helper module the steps read, or a database
    /// write, is seen by the next poll.
    #[tokio::test]
    async fn an_unchanged_poll_reuses_the_graph_and_the_status() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(dir.path());
        barca_core::db::init_db(&state.config.resolved.db_path)
            .await
            .unwrap();
        std::thread::sleep(SOURCES_SETTLED + Duration::from_millis(100));

        let first = poll(&state).await;
        assert_eq!(loads(&state), (1, 1));
        for _ in 0..3 {
            poll(&state).await;
        }
        assert_eq!(
            loads(&state),
            (1, 1),
            "nothing changed: nothing is parsed or decided again"
        );

        // A helper module edit changes `first`'s run hash.
        std::fs::write(
            dir.path().join("helpers.py"),
            "def compute():\n    return 2\n",
        )
        .unwrap();
        poll(&state).await;
        assert_eq!(loads(&state), (2, 2), "the helper edit is seen at once");

        // A database write (another process's run) is seen at once.
        std::thread::sleep(SOURCES_SETTLED + Duration::from_millis(100));
        poll(&state).await;
        let before = loads(&state);
        barca_core::db::upsert_schedule_state(&state.config.resolved.db_path, "p.py:first", 1)
            .await
            .unwrap();
        poll(&state).await;
        assert_eq!(loads(&state), (before.0, before.1 + 1), "{first:?}");
    }

    #[test]
    fn a_stamp_sees_an_edit_a_new_file_and_a_removed_one() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.py");
        let b = dir.path().join("b.py");
        std::fs::write(&a, "x = 1\n").unwrap();
        let stamp = Stamp::of([a.clone(), b.clone()]);
        assert_eq!(stamp.again(), stamp);
        std::fs::write(&a, "x = 22\n").unwrap();
        assert_ne!(stamp.again(), stamp, "an edit");
        let stamp = stamp.again();
        std::fs::write(&b, "").unwrap();
        assert_ne!(
            stamp.again(),
            stamp,
            "a file created where an import looked"
        );
        let stamp = stamp.again();
        std::fs::remove_file(&a).unwrap();
        assert_ne!(stamp.again(), stamp, "a removed file");
    }

    #[test]
    fn a_stamp_is_trusted_only_once_its_files_have_been_still() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.py");
        std::fs::write(&a, "x = 1\n").unwrap();
        let stamp = Stamp::of([a.clone()]);
        let now = SystemTime::now();
        assert!(!stamp.settled(now, Duration::from_secs(2)));
        assert!(stamp.settled(now + Duration::from_secs(3), Duration::from_secs(2)));
    }
}
