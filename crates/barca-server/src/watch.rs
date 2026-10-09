//! Dev-mode file watcher (`--watch`). Invalidates the cached DAG/plan whenever a
//! source file changes so `/assets` and `/plan` reflect edits without a restart.
//!
//! This is a local-development convenience only. In production the server is
//! started without `--watch`, no watcher thread is spawned, and the static
//! analysis cache simply persists for the process lifetime.

use crate::state::AppState;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::atomic::Ordering;

// The scheduler observes `AppState.dag_generation` to reload its job set.

/// Minimum interval between cache invalidations (milliseconds).
const DEBOUNCE_MS: u64 = 250;

/// Owns both native notifications and the one coalesced async refresh task.
pub struct SourceWatcher {
    _watcher: RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SourceWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Watch original configured sources, including files currently excluded from the graph.
pub fn spawn(state: AppState) -> notify::Result<SourceWatcher> {
    let cache = state.cache.clone();
    let generation = state.dag_generation.clone();
    // A watch channel stores one pending refresh, regardless of editor event volume.
    let (changed, mut changes) = tokio::sync::watch::channel(());
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        // Reading source during reload is not another edit: ignore access events
        // to prevent the loader and watcher continuously waking one another.
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }

        // Only invalidate on changes to .py files.
        let has_py = event
            .paths
            .iter()
            .any(|p| p.extension().is_some_and(|ext| ext == "py"));
        if !has_py {
            return;
        }

        if let Ok(mut c) = cache.write() {
            c.assets = None;
            c.plan = None;
        }
        // Signal the scheduler to re-read its job set on its next tick.
        generation.fetch_add(1, Ordering::Relaxed);
        changed.send_replace(());
    })?;

    // Watch each file's parent directory (non-recursively). Editors frequently
    // replace files atomically, which directory-level watching catches reliably.
    let mut watched: Vec<std::path::PathBuf> = Vec::new();
    for f in &state.config.files {
        let dir = barca_core::load::source_dir(Path::new(f));
        if watched.contains(&dir) {
            continue;
        }
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
        watched.push(dir);
    }

    let task = tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS)).await;
            if let Err(error) = state.loaded_dag().await {
                barca_core::errln!("[barca] source reload failed: {error}");
            }
        }
    });
    Ok(SourceWatcher {
        _watcher: watcher,
        task,
    })
}
