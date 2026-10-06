//! Dev-mode file watcher (`--watch`). Invalidates the cached DAG/plan whenever a
//! source file changes so `/assets` and `/plan` reflect edits without a restart.
//!
//! This is a local-development convenience only. In production the server is
//! started without `--watch`, no watcher thread is spawned, and the static
//! analysis cache simply persists for the process lifetime.

use crate::state::AppState;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// The scheduler observes `AppState.dag_generation` to reload its job set.

/// Minimum interval between cache invalidations (milliseconds).
const DEBOUNCE_MS: u64 = 250;

/// Spawn a watcher over the parent directories of the configured source files.
/// The returned `RecommendedWatcher` must be kept alive for watching to continue.
pub fn spawn(state: AppState) -> notify::Result<RecommendedWatcher> {
    let cache = state.cache.clone();
    let generation = state.dag_generation.clone();
    let last_invalidation = Arc::new(AtomicU64::new(0));

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };

        // Only invalidate on changes to .py files.
        let has_py = event
            .paths
            .iter()
            .any(|p| p.extension().is_some_and(|ext| ext == "py"));
        if !has_py {
            return;
        }

        // Simple debounce: skip if we invalidated less than DEBOUNCE_MS ago.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let prev = last_invalidation.load(Ordering::Relaxed);
        if now_ms.saturating_sub(prev) < DEBOUNCE_MS {
            return;
        }
        last_invalidation.store(now_ms, Ordering::Relaxed);

        if let Ok(mut c) = cache.write() {
            c.assets = None;
            c.plan = None;
        }
        // Signal the scheduler to re-read its job set on its next tick.
        generation.fetch_add(1, Ordering::Relaxed);
    })?;

    // Watch each file's parent directory (non-recursively). Editors frequently
    // replace files atomically, which directory-level watching catches reliably.
    let mut watched: Vec<std::path::PathBuf> = Vec::new();
    for f in &state.config.files {
        let dir = barca_core::commands::source_dir(Path::new(f));
        if watched.contains(&dir) {
            continue;
        }
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
        watched.push(dir);
    }

    Ok(watcher)
}
