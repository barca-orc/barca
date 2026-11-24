//! The graph this server runs and inspects: the configured source files, loaded once and
//! again when they change on disk.

use crate::config::ServeConfig;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

/// How long source files must have been still before their state is trusted. Source files are
/// written by people and editors, rarely twice in a second.
pub(crate) const SOURCES_SETTLED: std::time::Duration = std::time::Duration::from_secs(2);

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

    /// True when every file was last modified or changed more than
    /// [`SOURCES_SETTLED`] before `now`, the same wait the read cache
    /// gives the sources before it keeps a graph.
    ///
    /// A file's times are only as fine as the filesystem's clock (a few milliseconds on
    /// Linux). A file read just after it was written can be written again
    /// within the same instant with the same size, and its stamp would not show it. So a
    /// stamp that recent is not kept: the files are read again next time, until they have
    /// been still for a moment.
    pub fn settled(&self, now: std::time::SystemTime) -> bool {
        self.0.iter().all(|file| {
            [file.modified, file.changed].iter().all(|at| {
                now.duration_since(*at)
                    .is_ok_and(|age| age > SOURCES_SETTLED)
            })
        })
    }
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

/// The single selected graph owner for inspection, schedule and execution.
pub struct LoadedSources {
    stamp: Vec<Option<SourceStamp>>,
    dag: Arc<barca_service::dag::Dag>,
}

/// The loaded graph of one server, and what did not load.
#[derive(Clone)]
pub struct Sources {
    config: Arc<ServeConfig>,
    pub loaded: Arc<tokio::sync::Mutex<Option<LoadedSources>>>,
    pub load_errors: Arc<RwLock<Vec<barca_service::load::LoadError>>>,
    /// Bumped by the `--watch` file watcher on every DAG invalidation, so the
    /// scheduler can re-read its job set without a restart.
    pub generation: Arc<AtomicU64>,
}

impl Sources {
    pub fn new(config: Arc<ServeConfig>) -> Self {
        Self {
            config,
            loaded: Arc::new(tokio::sync::Mutex::new(None)),
            load_errors: Arc::new(RwLock::new(Vec::new())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn source_stamp(&self) -> Vec<Option<SourceStamp>> {
        self.config
            .files
            .iter()
            .map(|file| SourceStamp::of(std::slice::from_ref(file)))
            .collect()
    }

    pub(crate) fn graph_stale(&self) -> Option<bool> {
        let loaded = self.loaded.try_lock().ok()?;
        loaded
            .as_ref()
            .map(|current| current.stamp != self.source_stamp())
    }

    /// Observe the selected graph without refreshing sources or waiting for loading.
    pub(crate) fn loaded_node_count(&self) -> Option<usize> {
        self.loaded
            .try_lock()
            .ok()
            .and_then(|loaded| loaded.as_ref().map(|sources| sources.dag.node_count()))
    }

    pub async fn loaded_dag(
        &self,
    ) -> Result<Arc<barca_service::dag::Dag>, barca_service::BarcaError> {
        self.refresh_dag(false, barca_service::process::Until::done())
            .await
    }

    /// Execution/status must reread helper cones too; configured-file stamps alone
    /// cannot prove the result hash is unchanged after a helper module edit.
    pub async fn current_dag(
        &self,
    ) -> Result<Arc<barca_service::dag::Dag>, barca_service::BarcaError> {
        self.refresh_dag(true, barca_service::process::Until::done())
            .await
    }

    /// Refresh for an accepted run: cancellation also interrupts loading its resolvers.
    pub async fn current_dag_until(
        &self,
        until: barca_service::process::Until<'_>,
    ) -> Result<Arc<barca_service::dag::Dag>, barca_service::BarcaError> {
        self.refresh_dag(true, until).await
    }

    async fn refresh_dag(
        &self,
        force: bool,
        until: barca_service::process::Until<'_>,
    ) -> Result<Arc<barca_service::dag::Dag>, barca_service::BarcaError> {
        let mut loaded = until.admit(self.loaded.lock()).await?;
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
            let (dag, errors) = barca_service::load::build_partial_dag_until(
                &self.config.files,
                &self.config.python,
                until,
            )
            .await?;
            if before != self.source_stamp() {
                continue;
            }
            {
                let mut old_errors = self.load_errors.write().unwrap();
                if *old_errors != errors {
                    for error in &errors {
                        barca_service::errln!(
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
            return Ok(dag);
        }
        Err(barca_service::BarcaError::Usage(
            "source files changed during loading; retry after the edit completes".into(),
        ))
    }
}
