//! A size limit for `.barca/` (RFC-0012): measure, plan with [`plan::plan`], evict.
//!
//! `[cache] max_size` (or `BARCA_CACHE_MAX_SIZE`, default 10% of the filesystem) bounds
//! everything under the project's `.barca/`, every environment included. After every `get` and
//! `run`, after every run under `serve` and on `serve`'s minute checkpoint, [`after_command`]
//! checks the size; past the limit it evicts, in the order [`plan`] defines, down to
//! [`LOW_WATERMARK_PERCENT`] of it. `barca gc` ([`gc`]) runs the same plan on demand.
//!
//! **Measuring.** The check after a command must be cheap: it adds the recorded sizes of the
//! results the metadata DBs say are on disk, a walk of everything outside the artifact
//! directories (staging, `sql-cache`, the DB files), and a correction for the artifact
//! directories measured by the last full walk (files the DBs do not know about, moved-aside
//! directories, results fetched back after an eviction). A full walk ([`survey`]) runs when
//! that estimate is over the limit, at most a day after the last one, and under `barca gc`.
//!
//! **Never evicted:** the metadata DBs, their logs and locks and anything else barca does not
//! own (only files under `.barca/` that a row or a leftover names are deleted); this
//! command's outputs and inputs; results a run in flight in this process holds ([`Pin`]); with
//! another live run on this `.barca/`, anything but tiers 1 and 2; the current result of a
//! `Manual` node with no store copy.
//!
//! **Deleting** a file comes first, recording `evicted_at` second: a crash between the two
//! leaves a row whose file is missing, which a run handles as before (#252).

pub mod plan;
pub mod size;

pub use plan::{Eviction, Goal, LeftoverKind, Plan, Protections, Tier};
pub use size::MaxSize;

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::transfer::ArtifactLayout;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Eviction stops once the size is at or below this share of the limit.
pub const LOW_WATERMARK_PERCENT: u64 = 80;

/// How long eviction after a command may take; what is left waits for the next command.
const AFTER_COMMAND_BUDGET: Duration = Duration::from_secs(5);

/// A full walk at least this often, even when the estimate is under the limit.
const SURVEY_EVERY: Duration = Duration::from_secs(24 * 3600);

/// A `sql-cache` copy used this recently may be in use by a query: it is not evicted.
const SQL_CACHE_GRACE: Duration = Duration::from_secs(300);

/// What the last full walk found, kept in `.barca/gc-state.json`.
const STATE_FILE: &str = "gc-state.json";

/// The `cache` object of a `get`/`run` result: present when the check after the run found the
/// cache over its limit (with nothing evicted when nothing more could go).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheReport {
    /// Bytes under `.barca/` after eviction.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size_bytes: u64,
    /// The limit, in bytes.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub limit_bytes: u64,
    /// Files (and leftover directories) deleted.
    pub evicted_files: usize,
    /// Bytes deleted.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub evicted_bytes: u64,
}

// ─── Pins: runs in flight in this process ───────────────────────────────────

struct PinState {
    run_id: String,
    /// The project root the run is in: only runs on the same `.barca/` matter.
    root: PathBuf,
    /// False until the run's plan is known: until then, nothing but tiers 1 and 2 is evicted.
    resolved: bool,
    nodes: HashSet<String>,
    run_hashes: HashSet<String>,
}

static PINS: Mutex<BTreeMap<u64, PinState>> = Mutex::new(BTreeMap::new());
static NEXT_PIN: AtomicU64 = AtomicU64::new(1);

/// The results a run in flight in this process may read. `serve` runs several at once; each
/// holds one from before it plans until it returns.
pub(crate) struct Pin {
    id: u64,
}

pub(crate) fn pin(run_id: &str) -> Pin {
    pin_at(run_id, &std::env::current_dir().unwrap_or_default())
}

fn pin_at(run_id: &str, root: &Path) -> Pin {
    let id = NEXT_PIN.fetch_add(1, Ordering::Relaxed);
    pins().insert(
        id,
        PinState {
            run_id: run_id.to_string(),
            root: root.to_path_buf(),
            resolved: false,
            nodes: HashSet::new(),
            run_hashes: HashSet::new(),
        },
    );
    Pin { id }
}

fn pins() -> std::sync::MutexGuard<'static, BTreeMap<u64, PinState>> {
    PINS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Pin {
    /// The run's plan is known: protect the current results of these base node ids.
    pub(crate) fn nodes(&self, nodes: impl IntoIterator<Item = String>) {
        if let Some(state) = pins().get_mut(&self.id) {
            state.nodes.extend(nodes);
            state.resolved = true;
        }
    }

    /// Run hashes the run decided on (cached or to compute).
    pub(crate) fn run_hashes<'a>(&self, hashes: impl IntoIterator<Item = &'a String>) {
        if let Some(state) = pins().get_mut(&self.id) {
            for hash in hashes {
                if !state.run_hashes.contains(hash) {
                    state.run_hashes.insert(hash.clone());
                }
            }
        }
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        pins().remove(&self.id);
    }
}

/// What the other runs in flight in this process hold.
#[derive(Default)]
struct Pinned {
    run_ids: HashSet<String>,
    unresolved: bool,
    nodes: HashSet<String>,
    run_hashes: HashSet<String>,
}

fn pinned_except(root: &Path, own: Option<u64>) -> Pinned {
    let mut out = Pinned::default();
    for (id, state) in pins().iter().filter(|(_, s)| s.root == root) {
        out.run_ids.insert(state.run_id.clone());
        if Some(*id) == own {
            continue;
        }
        out.unresolved |= !state.resolved;
        out.nodes.extend(state.nodes.iter().cloned());
        out.run_hashes.extend(state.run_hashes.iter().cloned());
    }
    out
}

impl Pinned {
    /// Whether `e` became untouchable since the plan was made.
    fn holds(&self, e: &Eviction) -> bool {
        if e.run_hashes.iter().any(|h| self.run_hashes.contains(h)) {
            return true;
        }
        e.tier >= Tier::StoreHeld
            && (self.unresolved
                || e.node
                    .as_deref()
                    .is_some_and(|n| self.nodes.contains(base_of(n))))
    }
}

fn base_of(id: &str) -> &str {
    id.split('[').next().unwrap_or(id)
}

// ─── The project's `.barca/` ────────────────────────────────────────────────

struct Project {
    root: PathBuf,
    /// `<root>/.barca`, absolute.
    barca: PathBuf,
    /// The same, with symlinks resolved (when it exists).
    barca_canon: Option<PathBuf>,
}

impl Project {
    /// The project in the current directory (the CLI and `serve` run from the project root).
    fn here() -> Result<Self, BarcaError> {
        let root = std::env::current_dir()
            .map_err(|e| BarcaError::Other(format!("cannot determine cwd: {e}")))?;
        Ok(Self::at(root))
    }

    fn at(root: PathBuf) -> Self {
        let barca = root.join(".barca");
        let barca_canon = std::fs::canonicalize(&barca).ok();
        Self {
            root,
            barca,
            barca_canon,
        }
    }

    /// `path` as an absolute path, if it is inside `.barca/`.
    fn owned(&self, path: &Path) -> Option<PathBuf> {
        let abs = crate::config::normalize_lexically(&self.root.join(path));
        let inside = abs.starts_with(&self.barca)
            || self
                .barca_canon
                .as_ref()
                .is_some_and(|c| abs.starts_with(c));
        (inside && abs != self.barca).then_some(abs)
    }

    fn artifact_trees(&self) -> Vec<PathBuf> {
        let mut trees = vec![self.barca.join("artifacts")];
        if let Ok(entries) = std::fs::read_dir(self.barca.join("envs")) {
            for entry in entries.flatten() {
                trees.push(entry.path().join("artifacts"));
            }
        }
        trees
    }
}

/// One environment's metadata DB.
struct EnvDb {
    env: String,
    db_path: String,
    /// The artifact store's layout, when it is separate from the local artifact directory.
    layout: Option<ArtifactLayout>,
}

/// Every environment with a metadata DB under `.barca/`.
fn envs(project: &Project) -> Vec<EnvDb> {
    let mut names = Vec::new();
    if project.barca.join("metadata.db").is_file() {
        names.push(crate::config::DEFAULT_ENV.to_string());
    }
    if let Ok(entries) = std::fs::read_dir(project.barca.join("envs")) {
        let mut found: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().join("metadata.db").is_file())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .collect();
        found.sort();
        names.extend(found);
    }
    names
        .into_iter()
        .map(|env| {
            let local = crate::db::env_local_paths(&env);
            let layout = crate::config::resolve_in(Some(&env), &project.root)
                .ok()
                .filter(|cfg| cfg.remote_artifacts())
                .and_then(|cfg| {
                    let local =
                        std::fs::canonicalize(project.root.join(&cfg.local_artifact_dir)).ok()?;
                    Some(ArtifactLayout::new(local, &cfg.artifact_root))
                });
            EnvDb {
                db_path: project.root.join(local.db_path).display().to_string(),
                env,
                layout,
            }
        })
        .collect()
}

// ─── Measuring ──────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Seconds since the epoch of the last full walk.
    surveyed_at: u64,
    /// Bytes in the artifact directories minus what the DBs say is there, at that walk.
    artifact_adjust: i64,
    /// When eviction last could not get under the limit with nothing else holding it back
    /// (the working set is larger than the limit), and the size it left. Until the size grows
    /// or [`STUCK_FOR`] passes, the check after a command does not plan again.
    #[serde(default)]
    short_at: u64,
    #[serde(default)]
    short_size: u64,
}

/// How long a check that could not get under the limit is not repeated (unless the size grows
/// by more than 1% of the limit, or [`STUCK_SLACK`], whichever is more).
const STUCK_FOR: Duration = Duration::from_secs(600);
const STUCK_SLACK: u64 = 1 << 20;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_state(project: &Project) -> Option<State> {
    let raw = std::fs::read_to_string(project.barca.join(STATE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_state(project: &Project, state: &State) {
    let path = project.barca.join(STATE_FILE);
    let tmp = project
        .barca
        .join(format!("{STATE_FILE}.{}.tmp", std::process::id()));
    let Ok(body) = serde_json::to_string(state) else {
        return;
    };
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// What a walk of `.barca/` found.
#[derive(Default)]
struct Walk {
    /// Bytes outside the artifact directories.
    other: u64,
    /// Bytes inside them (only when walked).
    artifacts: u64,
    moved_aside: Vec<plan::Leftover>,
}

fn modified_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bytes under `path` (not following symlinks) and the newest modification time in it.
fn du(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    let mut newest = modified_secs(&meta);
    if !meta.is_dir() {
        return (meta.len(), newest);
    }
    let mut bytes = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let (b, m) = du(&entry.path());
            bytes += b;
            newest = newest.max(m);
        }
    }
    (bytes, newest)
}

/// Walk `.barca/`; the artifact directories only when `artifacts` is set.
fn walk(project: &Project, artifacts: bool) -> Walk {
    let trees: HashSet<PathBuf> = project.artifact_trees().into_iter().collect();
    let mut out = Walk::default();
    fn visit(dir: &Path, in_tree: bool, artifacts: bool, trees: &HashSet<PathBuf>, out: &mut Walk) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let tree = in_tree || trees.contains(&path);
            if tree && !artifacts {
                continue;
            }
            // This module's own bookkeeping (a few bytes) is not counted, so that measuring
            // twice gives the same size.
            if !in_tree
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(STATE_FILE))
            {
                continue;
            }
            if meta.is_dir() {
                if tree
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.contains(MOVED_ASIDE))
                {
                    let (bytes, modified) = du(&path);
                    out.artifacts += bytes;
                    out.moved_aside.push(plan::Leftover {
                        kind: LeftoverKind::MovedAside,
                        path,
                        with: Vec::new(),
                        bytes,
                        modified,
                    });
                    continue;
                }
                visit(&path, tree, artifacts, trees, out);
            } else if tree {
                out.artifacts += meta.len();
            } else {
                out.other += meta.len();
            }
        }
    }
    visit(&project.barca, false, artifacts, &trees, &mut out);
    out
}

/// What a directory found at an artifact's path is renamed to (`barca._storage.make_way`).
const MOVED_ASIDE: &str = ".moved-aside";

/// `.barca/staging/<pid>/` of processes that are not running.
fn staging_leftovers(project: &Project) -> Vec<plan::Leftover> {
    let Ok(entries) = std::fs::read_dir(project.barca.join("staging")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let pid: i64 = e.file_name().to_str()?.parse().ok()?;
            (pid != i64::from(std::process::id()) && !crate::db::pid_alive(pid)).then(|| {
                let (bytes, modified) = du(&e.path());
                plan::Leftover {
                    kind: LeftoverKind::Staging,
                    path: e.path(),
                    with: Vec::new(),
                    bytes,
                    modified,
                }
            })
        })
        .collect()
}

/// `.barca/sql-cache/` copies (each with its `.version` stamp), not used in the last few
/// minutes.
fn sql_cache_leftovers(project: &Project) -> Vec<plan::Leftover> {
    let mut files: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
    fn collect(dir: &Path, out: &mut Vec<(PathBuf, std::fs::Metadata)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => collect(&path, out),
                Ok(m) => out.push((path, m)),
                Err(_) => {}
            }
        }
    }
    collect(&project.barca.join("sql-cache"), &mut files);
    let by_path: HashMap<&Path, &std::fs::Metadata> =
        files.iter().map(|(p, m)| (p.as_path(), m)).collect();
    let fresh_after = now_secs().saturating_sub(SQL_CACHE_GRACE.as_secs());
    let mut out = Vec::new();
    for (path, meta) in &files {
        let name = path.to_string_lossy();
        let (data, stamp) = match name.strip_suffix(".version") {
            // A stamp goes with its copy; alone it is a leftover of its own.
            Some(copy) if by_path.contains_key(Path::new(copy)) => continue,
            Some(_) => (path.clone(), None),
            None => {
                let stamp = PathBuf::from(format!("{name}.version"));
                let found = by_path.contains_key(stamp.as_path()).then_some(stamp);
                (path.clone(), found)
            }
        };
        let stamp_meta = stamp.as_deref().and_then(|s| by_path.get(s).copied());
        let modified = modified_secs(meta).max(stamp_meta.map(modified_secs).unwrap_or(0));
        if modified > fresh_after {
            continue;
        }
        out.push(plan::Leftover {
            kind: LeftoverKind::SqlCache,
            path: data,
            bytes: meta.len() + stamp_meta.map(|m| m.len()).unwrap_or(0),
            with: stamp.into_iter().collect(),
            modified,
        });
    }
    out
}

/// The recorded size of the results the DB says are on disk (each file once).
async fn recorded_bytes(conn: &turso::Connection) -> Result<u64, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to measure the cache: {e}"));
    let mut rows = conn
        .query(
            "SELECT COALESCE(SUM(sz), 0) FROM (SELECT MAX(COALESCE(artifact_size_bytes, 0)) AS sz \
             FROM materializations WHERE status = 'success' AND artifact_path IS NOT NULL \
             AND evicted_at IS NULL GROUP BY artifact_path)",
            (),
        )
        .await
        .map_err(failed)?;
    let total = rows
        .next()
        .await
        .map_err(failed)?
        .map(|r| r.get::<i64>(0).unwrap_or(0))
        .unwrap_or(0);
    while rows.next().await.map_err(failed)?.is_some() {}
    Ok(total.max(0) as u64)
}

/// Open one environment's DB, bringing its schema up to date (the additive columns this
/// module reads), and run `f` on it.
async fn with_db<T>(
    db_path: &str,
    f: impl AsyncFnOnce(&turso::Connection) -> Result<T, BarcaError>,
) -> Result<T, BarcaError> {
    crate::db::init_db(db_path).await?;
    let _g = crate::db::db_guard().await;
    let (_db, conn) = crate::db::open_conn(db_path).await?;
    f(&conn).await
}

/// The fast size: recorded sizes, the small directories, and the last walk's correction.
async fn estimate(project: &Project, envs: &[EnvDb], state: &State) -> Result<u64, BarcaError> {
    let mut recorded = 0u64;
    for env in envs {
        recorded += with_db(&env.db_path, async |conn| recorded_bytes(conn).await).await?;
    }
    let other = walk(project, false).other;
    Ok((recorded as i64 + state.artifact_adjust).max(0) as u64 + other)
}

/// Everything a plan needs, from a full walk.
struct Survey {
    total: u64,
    rows: Vec<plan::Row>,
    leftovers: Vec<plan::Leftover>,
}

/// The successful rows of one environment, with their files.
async fn rows_of(project: &Project, env: &EnvDb) -> Result<(Vec<plan::Row>, u64), BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read {}: {e}", env.db_path));
    let raw = with_db(&env.db_path, async |conn| {
        let recorded = recorded_bytes(conn).await?;
        let mut rows = conn
            .query(
                "SELECT id, node_id, run_hash, COALESCE(artifact_path, ''), \
                 COALESCE(created_at, ''), last_read_at, output_hash IS NOT NULL \
                 FROM materializations WHERE status = 'success'",
                (),
            )
            .await
            .map_err(failed)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(failed)? {
            out.push((
                row.get::<i64>(0).map_err(failed)?,
                row.get::<String>(1).map_err(failed)?,
                row.get::<String>(2).ok(),
                row.get::<String>(3).unwrap_or_default(),
                row.get::<String>(4).unwrap_or_default(),
                row.get::<String>(5).ok(),
                row.get::<i64>(6).unwrap_or(0) != 0,
            ));
        }
        Ok((out, recorded))
    })
    .await?;
    let (raw, recorded) = raw;
    let rows = raw
        .into_iter()
        .map(
            |(id, node_id, run_hash, recorded, created_at, last_read_at, digest)| {
                let (file, store_held) = local_file(project, env, &recorded, digest);
                let meta = file.as_ref().and_then(|f| std::fs::metadata(f).ok());
                let present = meta.as_ref().is_some_and(|m| m.is_file());
                plan::Row {
                    env: env.env.clone(),
                    id,
                    node_id,
                    run_hash,
                    recorded,
                    file,
                    bytes: meta.filter(|_| present).map(|m| m.len()).unwrap_or(0),
                    present,
                    created_at,
                    last_read_at,
                    store_held,
                }
            },
        )
        .collect();
    Ok((rows, recorded))
}

/// The file on this disk that holds the artifact recorded as `recorded`, if barca may delete
/// it, and whether the artifact store holds a verified copy.
fn local_file(
    project: &Project,
    env: &EnvDb,
    recorded: &str,
    digest: bool,
) -> (Option<PathBuf>, bool) {
    if recorded.is_empty() {
        return (None, false);
    }
    if let Some(layout) = &env.layout
        && let Some(mirror) = layout.local_for(recorded)
    {
        // Recorded in the store with a confirmed upload digest. A store that is a directory on
        // this machine is looked at; a remote one is taken at its upload receipt.
        let in_store =
            digest && crate::transfer::local_path(recorded).is_none_or(|stored| stored.is_file());
        return (project.owned(&mirror), in_store);
    }
    match crate::transfer::local_path(recorded) {
        Some(path) => (project.owned(path), false),
        None => (None, false),
    }
}

/// Walk everything and read every environment's rows; remember the correction for
/// [`estimate`].
async fn survey(project: &Project, envs: &[EnvDb]) -> Result<Survey, BarcaError> {
    let walked = walk(project, true);
    let mut rows = Vec::new();
    let mut recorded = 0u64;
    for env in envs {
        let (env_rows, env_recorded) = rows_of(project, env).await?;
        rows.extend(env_rows);
        recorded += env_recorded;
    }
    write_state(
        project,
        &State {
            surveyed_at: now_secs(),
            artifact_adjust: walked.artifacts as i64 - recorded as i64,
            ..Default::default()
        },
    );
    let mut leftovers = staging_leftovers(project);
    leftovers.extend(walked.moved_aside);
    leftovers.extend(sql_cache_leftovers(project));
    Ok(Survey {
        total: walked.other + walked.artifacts,
        rows,
        leftovers,
    })
}

// ─── Protections ────────────────────────────────────────────────────────────

/// What a command protects of its own: its outputs, what it read and returned, and its run
/// hashes; and the `Manual` nodes of its DAG.
#[derive(Debug, Default, Clone)]
pub(crate) struct Own {
    pub recorded: HashSet<String>,
    pub run_hashes: HashSet<String>,
    pub manual: HashSet<String>,
}

/// The `freshness=Manual` nodes of the project, read statically (no import) from the files a
/// walk of the project finds and the files the recorded node ids name. `None` when one of
/// those files cannot be read or parsed: then no node is known not to be `Manual`.
fn manual_nodes(project: &Project, rows: &[plan::Row]) -> Option<HashSet<String>> {
    let toml = crate::config::load_toml(&project.root)
        .ok()?
        .unwrap_or_default();
    let mut files: HashSet<String> =
        crate::discover::discover(&project.root, &[], &toml.discovery.unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .collect();
    for row in rows {
        if let Some((file, _)) = base_of(&row.node_id).rsplit_once(':')
            && file.ends_with(".py")
            && !files.contains(file)
            && project.root.join(file).is_file()
        {
            files.insert(file.to_string());
        }
    }
    let mut out = HashSet::new();
    for file in files {
        let source = std::fs::read_to_string(project.root.join(&file)).ok()?;
        for node in crate::parse::extract_nodes(&source, &file).ok()? {
            if node.freshness == crate::model::Freshness::Manual {
                out.insert(node.continuity_key());
            }
        }
    }
    Some(out)
}

/// Runs in progress on this `.barca/` in other processes (any environment).
async fn live_elsewhere(envs: &[EnvDb], ours: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for env in envs {
        let live = with_db(&env.db_path, async |conn| {
            Ok(crate::db::live_runs(conn, &env.db_path).await)
        })
        .await
        .unwrap_or_default();
        out.extend(live.into_iter().filter(|id| !ours.contains(id)));
    }
    out.sort();
    out
}

async fn protections(
    project: &Project,
    envs: &[EnvDb],
    rows: &[plan::Row],
    own: &Own,
    pinned: &Pinned,
) -> (Protections, Option<String>) {
    let elsewhere = live_elsewhere(envs, &pinned.run_ids).await;
    let restricted = if let Some(first) = elsewhere.first() {
        Some(format!("run {first} is in progress"))
    } else if pinned.unresolved {
        Some("a run in this process is planning".to_string())
    } else {
        None
    };
    let manual = manual_nodes(project, rows);
    let protect = Protections {
        paths: HashSet::new(),
        recorded: own.recorded.clone(),
        run_hashes: own
            .run_hashes
            .iter()
            .chain(&pinned.run_hashes)
            .cloned()
            .collect(),
        nodes: pinned.nodes.clone(),
        manual_known: manual.is_some(),
        manual: manual
            .unwrap_or_default()
            .into_iter()
            .chain(own.manual.iter().cloned())
            .collect(),
        max_tier: restricted.is_some().then_some(Tier::Superseded),
        keep_staging: !elsewhere.is_empty(),
    };
    (protect, restricted)
}

// ─── Evicting ───────────────────────────────────────────────────────────────

/// What deleting a plan did.
#[derive(Default)]
struct Done {
    evicted: Vec<Eviction>,
    failed: Vec<(PathBuf, String)>,
}

fn remove(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Delete the plan's entries in order, then record the evicted rows. Stops at `deadline`; skips
/// what `held` says a run now holds.
async fn delete(
    plan: Plan,
    envs: &[EnvDb],
    deadline: Option<Instant>,
    held: impl Fn(&Eviction) -> bool,
) -> Done {
    let mut done = Done::default();
    for eviction in plan.evictions {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if held(&eviction) {
            continue;
        }
        match remove(&eviction.path) {
            Ok(()) => {
                for extra in &eviction.with {
                    let _ = remove(extra);
                }
                done.evicted.push(eviction);
            }
            Err(e) => done.failed.push((eviction.path.clone(), e.to_string())),
        }
    }
    let mut by_env: HashMap<&str, Vec<&str>> = HashMap::new();
    for (env, recorded) in done.evicted.iter().flat_map(|e| &e.rows) {
        by_env.entry(env).or_default().push(recorded);
    }
    for env in envs {
        if let Some(paths) = by_env.get(env.env.as_str()) {
            mark_evicted(&env.db_path, paths).await;
        }
    }
    done
}

async fn mark_evicted(db_path: &str, paths: &[&str]) {
    let _g = crate::db::db_guard().await;
    let Ok((_db, conn)) = crate::db::open_conn(db_path).await else {
        return;
    };
    if conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
        return;
    }
    let mut ok = true;
    for path in paths {
        ok = conn
            .execute(
                "UPDATE materializations SET evicted_at = datetime('now') \
                 WHERE artifact_path = ?1 AND status = 'success'",
                [*path],
            )
            .await
            .is_ok();
        if !ok {
            break;
        }
    }
    let _ = conn
        .execute(if ok { "COMMIT" } else { "ROLLBACK" }, ())
        .await;
}

/// Files and bytes per tier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierTotals {
    pub files: usize,
    pub bytes: u64,
}

fn per_tier<'a>(evictions: impl IntoIterator<Item = &'a Eviction>) -> BTreeMap<Tier, TierTotals> {
    let mut out = BTreeMap::new();
    for e in evictions {
        let t: &mut TierTotals = out.entry(e.tier).or_default();
        t.files += 1;
        t.bytes += e.bytes;
    }
    out
}

/// `290 superseded results, 22 local copies held by the store`.
fn tier_words(totals: &BTreeMap<Tier, TierTotals>) -> String {
    totals
        .iter()
        .map(|(tier, t)| format!("{} {}", t.files, tier.words()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn warn_failed(failed: &[(PathBuf, String)]) {
    if let Some((path, error)) = failed.first() {
        let more = match failed.len() - 1 {
            0 => String::new(),
            n => format!(" (and {n} more)"),
        };
        crate::errln!(
            "[barca] warning: could not evict {}: {error}{more}; skipped",
            path.display()
        );
    }
}

static WORKING_SET_WARNED: AtomicBool = AtomicBool::new(false);

/// Once per process: what may be evicted was not enough to get under the limit.
fn warn_working_set(after: u64, limit: u64) {
    if after > limit && !WORKING_SET_WARNED.swap(true, Ordering::Relaxed) {
        crate::errln!(
            "[barca] warning: cache working set {} is larger than max_size {}; results will be \
             recomputed often",
            size::human(after),
            size::human(limit)
        );
    }
}

static LAST_MEASURED: Mutex<Option<(u64, u64)>> = Mutex::new(None);

fn remember(size: u64, limit: u64) {
    *LAST_MEASURED.lock().unwrap_or_else(|p| p.into_inner()) = Some((size, limit));
}

/// The size and limit the last check in this process measured (`GET /health`).
pub fn last_measured() -> Option<(u64, u64)> {
    *LAST_MEASURED.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn target_of(limit: u64) -> u64 {
    limit / 100 * LOW_WATERMARK_PERCENT + limit % 100 * LOW_WATERMARK_PERCENT / 100
}

// ─── Entry points ───────────────────────────────────────────────────────────

/// The check after a command (and on `serve`'s minute checkpoint, with no `own`): over the
/// limit, evict down to [`LOW_WATERMARK_PERCENT`] of it, for at most a few seconds. Never fails
/// the command: a problem is one warning on stderr. Returns the `cache` report when
/// `.barca/` was over the limit.
pub(crate) async fn after_command(
    cfg: &ResolvedConfig,
    own: &Own,
    pin: Option<&Pin>,
) -> Option<CacheReport> {
    if cfg.cache_max_size == MaxSize::Off {
        return None;
    }
    let started = Instant::now();
    let checked = check(cfg.cache_max_size, own, pin).await;
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        crate::errln!(
            "[trace] cache size check {:.1}ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    match checked {
        Ok(report) => report,
        Err(e) => {
            crate::errln!("[barca] warning: cache size check failed: {e}");
            None
        }
    }
}

/// `serve`'s minute checkpoint: [`after_command`] with nothing of its own to protect.
pub async fn checkpoint(cfg: &ResolvedConfig) -> Option<CacheReport> {
    after_command(cfg, &Own::default(), None).await
}

async fn check(
    max_size: MaxSize,
    own: &Own,
    pin: Option<&Pin>,
) -> Result<Option<CacheReport>, BarcaError> {
    check_in(&Project::here()?, max_size, own, pin).await
}

async fn check_in(
    project: &Project,
    max_size: MaxSize,
    own: &Own,
    pin: Option<&Pin>,
) -> Result<Option<CacheReport>, BarcaError> {
    if !project.barca.is_dir() {
        return Ok(None);
    }
    let Some(limit) = max_size.resolve(&project.barca) else {
        return Ok(None);
    };
    let envs = envs(project);
    let state = read_state(project);
    let fresh = state
        .as_ref()
        .is_some_and(|s| now_secs().saturating_sub(s.surveyed_at) < SURVEY_EVERY.as_secs());
    if let (true, Some(state)) = (fresh, &state) {
        let size = estimate(project, &envs, state).await?;
        remember(size, limit);
        if size <= limit {
            return Ok(None);
        }
        // Eviction could not get under the limit a moment ago and nothing has grown since:
        // planning again would find the same.
        let stuck = now_secs().saturating_sub(state.short_at) < STUCK_FOR.as_secs()
            && size
                <= state
                    .short_size
                    .saturating_add((limit / 100).max(STUCK_SLACK));
        if stuck {
            return Ok(Some(CacheReport {
                size_bytes: size,
                limit_bytes: limit,
                evicted_files: 0,
                evicted_bytes: 0,
            }));
        }
    }
    let survey = survey(project, &envs).await?;
    remember(survey.total, limit);
    if survey.total <= limit {
        return Ok(None);
    }
    let pinned = pinned_except(&project.root, pin.map(|p| p.id));
    let (protect, restricted) = protections(project, &envs, &survey.rows, own, &pinned).await;
    let plan = plan::plan(
        survey.total,
        &survey.rows,
        &survey.leftovers,
        &protect,
        Goal::DownTo {
            target: target_of(limit),
        },
    );
    let short = plan.short;
    let evictable = plan.evictions.len();
    let own_pin = pin.map(|p| p.id);
    let done = delete(
        plan,
        &envs,
        Some(Instant::now() + AFTER_COMMAND_BUDGET),
        |e| pinned_except(&project.root, own_pin).holds(e),
    )
    .await;
    warn_failed(&done.failed);
    let evicted_bytes: u64 = done.evicted.iter().map(|e| e.bytes).sum();
    let size_after = survey.total.saturating_sub(evicted_bytes);
    remember(size_after, limit);
    if !done.evicted.is_empty() {
        crate::errln!(
            "[barca] cache {} over its {} limit; evicted {} file{} ({}): {}",
            size::human(survey.total),
            size::human(limit),
            done.evicted.len(),
            if done.evicted.len() == 1 { "" } else { "s" },
            size::human(evicted_bytes),
            tier_words(&per_tier(&done.evicted))
        );
    }
    if short && restricted.is_none() && done.evicted.len() == evictable {
        if protect.manual_known {
            warn_working_set(size_after, limit);
        }
        if let Some(mut state) = read_state(project) {
            state.short_at = now_secs();
            state.short_size = size_after;
            write_state(project, &state);
        }
    }
    Ok(Some(CacheReport {
        size_bytes: size_after,
        limit_bytes: limit,
        evicted_files: done.evicted.len(),
        evicted_bytes,
    }))
}

/// What `barca gc` was asked to do.
#[derive(Debug, Clone, Default)]
pub struct GcOptions {
    /// `--max-size`: a one-off limit in place of the configured one.
    pub max_size: Option<MaxSize>,
    pub dry_run: bool,
    /// `--env`: evict only this environment's results (leftovers are shared).
    pub env: Option<String>,
}

/// One entry of `barca gc`'s list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcEntry {
    pub tier: String,
    pub path: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// Files and bytes of one tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcTier {
    /// `leftovers`, `superseded`, `store_held` or `current`.
    pub tier: String,
    pub files: usize,
    pub bytes: u64,
}

/// A file `barca gc` could not delete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcFailure {
    pub path: String,
    pub error: String,
}

/// The result of `barca gc` (or what `--dry-run` predicts it would do).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcResult {
    pub dry_run: bool,
    /// Bytes under `.barca/` before.
    pub size_bytes: u64,
    /// The limit (`null` when off: then only leftovers and superseded results are evicted).
    pub limit_bytes: Option<u64>,
    /// What eviction aims for: [`LOW_WATERMARK_PERCENT`] of the limit (`null` when off).
    pub target_bytes: Option<u64>,
    /// Bytes under `.barca/` after.
    pub after_bytes: u64,
    pub evicted_files: usize,
    pub evicted_bytes: u64,
    /// Files and bytes per tier evicted, in eviction order.
    pub tiers: Vec<GcTier>,
    /// Why only tiers 1 and 2 were considered (another run in progress), or `null`.
    pub restricted: Option<String>,
    /// Every entry, in eviction order.
    pub evicted: Vec<GcEntry>,
    /// Entries that could not be deleted (skipped).
    pub failed: Vec<GcFailure>,
}

/// `barca gc`: evict now, to [`LOW_WATERMARK_PERCENT`] of the limit, or with the limit off
/// every leftover and superseded result. A metadata DB that cannot be read is an error.
pub async fn gc(cfg: &ResolvedConfig, opts: &GcOptions) -> Result<GcResult, BarcaError> {
    gc_in(&Project::here()?, cfg.cache_max_size, opts).await
}

async fn gc_in(
    project: &Project,
    configured: MaxSize,
    opts: &GcOptions,
) -> Result<GcResult, BarcaError> {
    let max_size = opts.max_size.unwrap_or(configured);
    let limit = max_size.resolve(&project.barca);
    if matches!(max_size, MaxSize::Percent(_)) && limit.is_none() {
        return Err(BarcaError::Other(format!(
            "cannot read the size of the filesystem {} is on, so a limit of {} cannot be \
             resolved; give one in bytes (--max-size 20GB)",
            project.barca.display(),
            max_size.describe()
        )));
    }
    let mut envs = envs(project);
    if let Some(only) = &opts.env {
        envs.retain(|e| &e.env == only);
    }
    let survey = if project.barca.is_dir() {
        survey(project, &envs).await?
    } else {
        Survey {
            total: 0,
            rows: Vec::new(),
            leftovers: Vec::new(),
        }
    };
    let pinned = pinned_except(&project.root, None);
    let (mut protect, restricted) =
        protections(project, &envs, &survey.rows, &Own::default(), &pinned).await;
    let goal = match limit {
        Some(limit) => Goal::DownTo {
            target: target_of(limit),
        },
        None => {
            protect.max_tier = Some(Tier::Superseded);
            Goal::Everything
        }
    };
    let plan = plan::plan(
        survey.total,
        &survey.rows,
        &survey.leftovers,
        &protect,
        goal,
    );
    let (evicted, failed) = if opts.dry_run {
        (plan.evictions, Vec::new())
    } else {
        let done = delete(plan, &envs, None, |e| {
            pinned_except(&project.root, None).holds(e)
        })
        .await;
        (done.evicted, done.failed)
    };
    let evicted_bytes: u64 = evicted.iter().map(|e| e.bytes).sum();
    if let Some(limit) = limit {
        remember(survey.total.saturating_sub(evicted_bytes), limit);
    }
    Ok(GcResult {
        dry_run: opts.dry_run,
        size_bytes: survey.total,
        limit_bytes: limit,
        target_bytes: limit.map(target_of),
        after_bytes: survey.total.saturating_sub(evicted_bytes),
        evicted_files: evicted.len(),
        evicted_bytes,
        tiers: per_tier(&evicted)
            .into_iter()
            .map(|(tier, totals)| GcTier {
                tier: tier.code().to_string(),
                files: totals.files,
                bytes: totals.bytes,
            })
            .collect(),
        restricted,
        evicted: evicted
            .iter()
            .map(|e| GcEntry {
                tier: e.tier.code().to_string(),
                path: project_relative(project, &e.path),
                bytes: e.bytes,
                node: e.node.clone(),
            })
            .collect(),
        failed: failed
            .into_iter()
            .map(|(path, error)| GcFailure {
                path: project_relative(project, &path),
                error,
            })
            .collect(),
    })
}

fn project_relative(project: &Project, path: &Path) -> String {
    path.strip_prefix(&project.root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Size, limit and free space, for `barca doctor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub size_bytes: u64,
    pub limit_bytes: Option<u64>,
    /// The configured limit as written (`10% of the filesystem`, `20 GB`, `off`).
    pub max_size: String,
    /// Where it came from: `BARCA_CACHE_MAX_SIZE`, `[cache] max_size` or `default`.
    pub source: String,
    pub filesystem_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

/// Measure `.barca/` with a full walk (no eviction).
pub fn usage(cfg: &ResolvedConfig) -> Result<Usage, BarcaError> {
    let project = Project::here()?;
    let walked = walk(&project, true);
    let fs = size::filesystem(&project.barca);
    Ok(Usage {
        size_bytes: walked.other + walked.artifacts,
        limit_bytes: cfg.cache_max_size.resolve(&project.barca),
        max_size: cfg.cache_max_size.describe(),
        source: cfg.cache_max_size_source.to_string(),
        filesystem_bytes: fs.map(|f| f.total),
        available_bytes: fs.map(|f| f.available),
    })
}

#[cfg(test)]
mod tests;
