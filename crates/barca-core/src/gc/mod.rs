//! A size limit for `.barca/` (RFC-0012): measure, plan with [`plan::plan`], evict.
//!
//! `[cache] max_size` (or `BARCA_CACHE_MAX_SIZE`, default 10% of the filesystem) bounds
//! everything under the project's `.barca/` except recognized local metadata backups,
//! every environment included (RFC-0015). Physical totals retain backup bytes. After every `get` and
//! `run`, after every run under `serve` and on `serve`'s minute checkpoint, [`after_command`]
//! checks the size; past the limit it evicts, in the order [`plan`] defines, down to
//! [`LOW_WATERMARK_PERCENT`] of it. `barca gc` ([`gc`]) runs the same plan on demand.
//!
//! **Measuring.** The check after a command must be cheap: it adds the recorded sizes of the
//! results the metadata DBs say are on disk, a walk of everything outside the artifact
//! directories (staging, `sql-cache`, `range-cache`, the DB files), and a correction for the artifact
//! directories measured by the last full walk (files the DBs do not know about, moved-aside
//! directories, results fetched back after an eviction). A full walk ([`survey`]) runs when
//! that estimate is over the limit, at most a day after the last one, and under `barca gc`.
//!
//! **Unreferenced files.** A file in an artifact directory that no metadata row of any
//! environment names is a leftover (tier 1): `barca state reset` and `barca state restore` leave
//! the files the old database named, and nothing else would ever delete them (#552). One
//! written in the last [`UNREFERENCED_GRACE`] is kept, and so is every one while a run is in
//! flight on this `.barca/` (its rows are not written yet).
//!
//! **The metadata DBs** cannot be evicted, so each is kept within its own share of the limit
//! first ([`crate::metadata_size`]).
//!
//! **Never evicted:** the metadata DBs (and the copies `state reset` and `state restore` move
//! aside), their logs and locks, the backups, and anything else barca does not own (only files
//! under `.barca/` that a row or a leftover names are deleted); this
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

/// Where lazy inputs keep the byte ranges they fetched (`barca._storage.RANGE_CACHE_DIR`).
pub const RANGE_CACHE_DIR: &str = "range-cache";

/// A `sql-cache` copy or a `range-cache` directory used this recently may be in use by a query:
/// it is not evicted.
const SQL_CACHE_GRACE: Duration = Duration::from_secs(300);

/// An artifact file no row names that was written this recently may belong to a step whose
/// row is about to be written: it is not evicted.
pub const UNREFERENCED_GRACE: Duration = Duration::from_secs(60);

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
    /// Another run in this process is in flight on this `.barca/`.
    others: bool,
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
        out.others = true;
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
        // A run that started since the plan was made may be writing files no row names yet.
        if self.others && e.leftover == Some(LeftoverKind::Unreferenced) {
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

    /// `path` (absolute) spelled from `self.barca` when it is spelled from its resolved form, so
    /// a recorded path and a walked one compare equal.
    fn key(&self, path: &Path) -> PathBuf {
        match self
            .barca_canon
            .as_ref()
            .and_then(|c| path.strip_prefix(c).ok())
        {
            Some(rest) if self.barca_canon.as_deref() != Some(self.barca.as_path()) => {
                self.barca.join(rest)
            }
            _ => path.to_path_buf(),
        }
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

/// Every environment with a metadata DB under `.barca/`. An empty `metadata.db` records no
/// results and is left alone (`serve` warns about a stray one).
fn envs(project: &Project) -> Vec<EnvDb> {
    let has_db = |dir: &Path| {
        let db = dir.join("metadata.db");
        db.is_file() && !crate::db::empty_file(&db.display().to_string())
    };
    let mut names = Vec::new();
    if has_db(&project.barca) {
        names.push(crate::config::DEFAULT_ENV.to_string());
    }
    if let Ok(entries) = std::fs::read_dir(project.barca.join("envs")) {
        let mut found: Vec<String> = entries
            .flatten()
            .filter(|e| has_db(&e.path()))
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Measurement {
    size_bytes: u64,
    backup_bytes: u64,
}

impl Measurement {
    fn budget_bytes(self) -> u64 {
        self.size_bytes.saturating_sub(self.backup_bytes)
    }
}

/// Project-wide storage, using maintained cache evidence and a bounded backup survey.
/// No artifact walk or database scan is performed by status.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskUsage {
    pub size_bytes: Option<u64>,
    pub budget_bytes: Option<u64>,
    pub backup_bytes: Option<u64>,
    pub limit_bytes: Option<u64>,
}

pub fn disk_usage(cfg: &ResolvedConfig) -> DiskUsage {
    let Ok(project) = Project::here() else {
        return DiskUsage::default();
    };
    disk_usage_in(&project, cfg.cache_max_size)
}

fn disk_usage_in(project: &Project, max_size: MaxSize) -> DiskUsage {
    let backup_bytes = measure_backups(project);
    let budget_bytes = read_state(project)
        .and_then(|s| s.measurement)
        .map(Measurement::budget_bytes);
    DiskUsage {
        size_bytes: budget_bytes
            .zip(backup_bytes)
            .map(|(budget, backups)| budget.saturating_add(backups)),
        budget_bytes,
        backup_bytes,
        limit_bytes: max_size.resolve(&project.barca),
    }
}

/// Fixed backup layout only; unexpected large directories return unknown rather than
/// walking without a bound. Symlinks are never followed or exempted from the budget.
fn measure_backups(project: &Project) -> Option<u64> {
    const MAX_ENTRIES: usize = 4096;
    fn entries(path: &Path) -> Option<Option<std::fs::ReadDir>> {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(None),
            Ok(meta) if !meta.is_dir() => return None,
            Err(_) => return None,
            _ => {}
        }
        std::fs::read_dir(path).ok().map(Some)
    }
    let mut remaining = MAX_ENTRIES;
    let mut directories = vec![project.barca.join("backups")];
    if let Some(envs) = entries(&project.barca.join("envs"))? {
        for env in envs {
            remaining = remaining.checked_sub(1)?;
            let env = env.ok()?;
            if env.file_type().ok()?.is_dir() {
                directories.push(env.path().join("backups"));
            }
        }
    }
    let mut bytes = 0u64;
    for dir in directories {
        if let Some(files) = entries(&dir)? {
            for file in files {
                remaining = remaining.checked_sub(1)?;
                let file = file.ok()?;
                let rel = file.path();
                if crate::backup::owned_local_file(rel.strip_prefix(&project.barca).ok()?) {
                    let meta = std::fs::symlink_metadata(&rel).ok()?;
                    if meta.is_file() {
                        bytes = bytes.saturating_add(meta.len());
                    }
                }
            }
        }
    }
    Some(bytes)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Physical/cache accounting at the last maintained measurement (RFC-0015).
    #[serde(default)]
    measurement: Option<Measurement>,
    /// Version 1 excludes owned metadata backups from quota comparisons.
    #[serde(default)]
    accounting_version: u32,
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
    /// The size the last check after a command found under the limit, and that limit: a
    /// command that wrote nothing ([`Own::wrote_nothing`]) under the same limit is not checked
    /// again (#560). 0 when unknown.
    #[serde(default)]
    checked_size: u64,
    #[serde(default)]
    checked_limit: u64,
    /// By metadata DB: when trimming last found nothing more to remove with the file still over
    /// its share, and the size it left ([`trim_metadata`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    metadata_short: BTreeMap<String, (u64, u64)>,
    /// When the artifact store was last checked and found there, by environment, store URI and
    /// storage options: seconds since the epoch ([`store_checked_recently`], #593).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    store_checks: Vec<StoreCheck>,
    /// Conservative artifact-byte estimates through each environment's last observed row.
    /// New polls inspect only appended rows. A full survey corrects duplicate paths.
    #[serde(default)]
    recorded: BTreeMap<String, Recorded>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Recorded {
    through: i64,
    bytes: u64,
}

/// One successful check of an artifact store (#593).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoreCheck {
    env: String,
    uri: String,
    /// SHA-256 of the storage options (endpoint, credentials) the check used; empty for none.
    #[serde(default)]
    options: String,
    at: u64,
}

impl StoreCheck {
    /// The store `cfg` uses, checked at `at`.
    fn of(cfg: &crate::config::ResolvedConfig, at: u64) -> Self {
        use sha2::{Digest, Sha256};
        let options = cfg
            .storage_options_json
            .as_deref()
            .map(|o| format!("{:x}", Sha256::digest(o.as_bytes())))
            .unwrap_or_default();
        Self {
            env: cfg.env.clone(),
            uri: cfg.artifact_root.clone(),
            options,
            at,
        }
    }

    fn same_store(&self, other: &Self) -> bool {
        (&self.env, &self.uri, &self.options) == (&other.env, &other.uri, &other.options)
    }
}

/// How long a successful check of the artifact store is trusted by a run that needs no
/// transfer (#593): such a run starts no transfer helper.
pub const STORE_CHECK_WINDOW: Duration = Duration::from_secs(300);

/// Whether the artifact store of `cfg` (its environment, store URI and storage options) was
/// checked and found there less than [`STORE_CHECK_WINDOW`] ago, by this project's
/// `.barca/gc-state.json`. A clock that went back counts as not recent.
pub(crate) fn store_checked_recently(cfg: &crate::config::ResolvedConfig) -> bool {
    let Ok(project) = Project::here() else {
        return false;
    };
    let now = now_secs();
    let store = StoreCheck::of(cfg, 0);
    read_state(&project).is_some_and(|s| {
        s.store_checks.iter().any(|c| {
            c.same_store(&store) && c.at <= now && now - c.at < STORE_CHECK_WINDOW.as_secs()
        })
    })
}

/// Remember that the artifact store of `cfg` was just checked and found there (#593).
pub(crate) fn remember_store_check(cfg: &crate::config::ResolvedConfig) {
    if let Ok(project) = Project::here() {
        update_store_checks(&project, StoreCheck::of(cfg, now_secs()), true);
    }
}

/// Forget the last check of the artifact store of `cfg`: a transfer to it failed, so the next
/// run checks it again (#593).
pub(crate) fn forget_store_check(cfg: &crate::config::ResolvedConfig) {
    if let Ok(project) = Project::here() {
        update_store_checks(&project, StoreCheck::of(cfg, 0), false);
    }
}

fn update_store_checks(project: &Project, check: StoreCheck, keep: bool) {
    if !project.barca.is_dir() {
        return;
    }
    let mut state = read_state(project).unwrap_or_default();
    let before = state.store_checks.clone();
    state.store_checks.retain(|c| !c.same_store(&check));
    if keep {
        state.store_checks.push(check);
    }
    if state.store_checks != before {
        write_state(project, &state);
    }
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

/// Record that a check found `size` under `limit` ([`State::checked_size`]).
fn checked(project: &Project, size: u64, limit: u64) {
    if let Some(mut state) = read_state(project)
        && ((state.checked_size, state.checked_limit) != (size, limit)
            || state.accounting_version != 1)
    {
        state.checked_size = size;
        state.checked_limit = limit;
        state.accounting_version = 1;
        write_state(project, &state);
    }
}

fn record_measurement(project: &Project, measured: Measurement) {
    if let Some(mut state) = read_state(project) {
        state.measurement = Some(measured);
        state.accounting_version = 1;
        write_state(project, &state);
    }
}

/// Forget the last full walk, so the next check walks `.barca/` again: the metadata database
/// was replaced (`barca state reset`, `barca state restore`), and the recorded sizes the fast
/// estimate adds up no longer describe the files on disk.
pub fn forget_survey() {
    if let Ok(project) = Project::here() {
        forget_survey_in(&project);
    }
}

fn forget_survey_in(project: &Project) {
    let _ = std::fs::remove_file(project.barca.join(STATE_FILE));
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
    /// Every file in the artifact directories (outside moved-aside ones): path, bytes,
    /// modification time.
    files: Vec<(PathBuf, u64, u64)>,
    /// The bytes outside the artifact directories, by what holds them.
    outside: BTreeMap<Held, u64>,
}

/// What holds the space under `.barca/`, as `barca gc` reports it (`held`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Held {
    /// Artifact files a metadata row names.
    Results,
    /// Artifact files no metadata row names (tier-1 leftovers, or kept for now).
    Unreferenced,
    /// The metadata databases, their logs and locks: never evicted.
    Metadata,
    /// Databases `barca state reset` and `barca state restore` moved aside: never evicted.
    DbCopies,
    /// Metadata backups (`backups/`), five per environment: never evicted.
    Backups,
    /// Everything else: staging, `sql-cache`, `range-cache`, moved-aside artifact directories.
    Other,
}

impl Held {
    /// The name in JSON.
    pub fn code(self) -> &'static str {
        match self {
            Held::Results => "results",
            Held::Unreferenced => "unreferenced",
            Held::Metadata => "metadata",
            Held::DbCopies => "db_copies",
            Held::Backups => "backups",
            Held::Other => "other",
        }
    }

    /// What eviction may free from it: nothing for the databases and backups.
    fn evictable(self) -> bool {
        matches!(self, Held::Results | Held::Unreferenced | Held::Other)
    }
}

/// What a file outside the artifact directories is, by its path below `.barca/`.
fn outside_kind(barca: &Path, path: &Path) -> Held {
    let rel = path.strip_prefix(barca).unwrap_or(path);
    if crate::backup::owned_local_file(rel)
        && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
    {
        return Held::Backups;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name.strip_prefix("metadata.db") {
        Some(rest) if rest.contains(".reset-") || rest.contains(".pre-restore") => Held::DbCopies,
        Some(_) => Held::Metadata,
        None if name == "serve.lock" => Held::Metadata,
        None => Held::Other,
    }
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
    fn visit(
        barca: &Path,
        dir: &Path,
        in_tree: bool,
        artifacts: bool,
        trees: &HashSet<PathBuf>,
        out: &mut Walk,
    ) {
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
                visit(barca, &path, tree, artifacts, trees, out);
            } else if tree {
                out.artifacts += meta.len();
                out.files.push((path, meta.len(), modified_secs(&meta)));
            } else {
                out.other += meta.len();
                *out.outside.entry(outside_kind(barca, &path)).or_default() += meta.len();
            }
        }
    }
    visit(
        &project.barca,
        &project.barca,
        false,
        artifacts,
        &trees,
        &mut out,
    );
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

/// `.barca/range-cache/` directories (the ranges kept of one store object each, #577), not
/// used in the last few minutes: a lazy input may be reading one.
fn range_cache_leftovers(project: &Project) -> Vec<plan::Leftover> {
    let Ok(entries) = std::fs::read_dir(project.barca.join(RANGE_CACHE_DIR)) else {
        return Vec::new();
    };
    let fresh_after = now_secs().saturating_sub(SQL_CACHE_GRACE.as_secs());
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let (mut bytes, mut modified) = (0, modified_secs(&meta));
        for file in std::fs::read_dir(&path).into_iter().flatten().flatten() {
            if let Ok(m) = file.metadata() {
                bytes += m.len();
                modified = modified.max(modified_secs(&m));
            }
        }
        if modified > fresh_after {
            continue;
        }
        out.push(plan::Leftover {
            kind: LeftoverKind::RangeCache,
            path,
            bytes,
            with: Vec::new(),
            modified,
        });
    }
    out
}

/// The bytes the recorded results of `db_path` take: each artifact path once, at its largest
/// recorded size, over the successful rows not evicted ([`scan`]).
async fn recorded_bytes(db_path: &str) -> Result<u64, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to measure the cache: {e}"));
    let rows = scan(
        db_path,
        "artifact_path, COALESCE(artifact_size_bytes, 0)",
        "status = 'success' AND artifact_path IS NOT NULL AND evicted_at IS NULL",
        |row| {
            Ok((
                row.get::<String>(1).map_err(failed)?,
                row.get::<i64>(2).unwrap_or(0).max(0) as u64,
            ))
        },
    )
    .await?;
    let mut largest: HashMap<String, u64> = HashMap::new();
    for (path, bytes) in rows {
        let size = largest.entry(path).or_default();
        *size = (*size).max(bytes);
    }
    Ok(largest.values().sum())
}

/// Open one environment's DB, bringing its schema up to date (the additive columns this
/// module reads), and run `f` on it.
async fn with_db<T>(
    db_path: &str,
    f: impl AsyncFnOnce(&libsql::Connection) -> Result<T, BarcaError>,
) -> Result<T, BarcaError> {
    crate::db::init_db(db_path).await?;
    let _g = crate::db::db_guard().await;
    let (_db, conn) = crate::db::open_conn(db_path).await?;
    f(&conn).await
}

/// Rows of `materializations` where `filter`, as `each` maps them, [`SCAN_CHUNK`] at a time in id
/// order, each chunk read under barca's lock and the lock let go between them. `select` lists
/// the columns after `id`, which is column 0. One query over every row held the lock until its
/// last row: a survey of 2.8M rows held a server's runs that long, and a run could not even
/// start.
async fn scan<T>(
    db_path: &str,
    select: &str,
    filter: &str,
    mut each: impl FnMut(&libsql::Row) -> Result<T, BarcaError>,
) -> Result<Vec<T>, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read {db_path}: {e}"));
    crate::db::init_db(db_path).await?;
    let sql = format!(
        "SELECT id, {select} FROM materializations WHERE id > ?1 AND ({filter}) \
         ORDER BY id LIMIT {SCAN_CHUNK}"
    );
    let mut out = Vec::new();
    // Every row: ids start at 1, but nothing stops a database from holding a 0 or below.
    let mut after = i64::MIN;
    loop {
        let read = {
            let _g = crate::db::db_guard().await;
            let (_db, conn) = crate::db::open_conn(db_path).await?;
            let mut rows = conn.query(&sql, [after]).await.map_err(failed)?;
            let mut read = 0usize;
            while let Some(row) = rows.next().await.map_err(failed)? {
                after = row.get::<i64>(0).map_err(failed)?;
                out.push(each(&row)?);
                read += 1;
            }
            read
        };
        if read < SCAN_CHUNK {
            return Ok(out);
        }
        tokio::task::yield_now().await;
    }
}

/// Rows read under the lock at a time by [`scan`]: tens of milliseconds.
const SCAN_CHUNK: usize = 20_000;

/// The fast size: recorded sizes, the small directories, and the last walk's correction.
async fn estimate(
    project: &Project,
    envs: &[EnvDb],
    state: &State,
) -> Result<Measurement, BarcaError> {
    let mut recorded = 0u64;
    let mut updated = BTreeMap::new();
    for env in envs {
        let previous = state.recorded.get(&env.db_path);
        let estimate = with_db(&env.db_path, async |conn| {
            let mut rows = conn.query(
                "SELECT COALESCE((SELECT id FROM materializations ORDER BY id DESC LIMIT 1),0)", ()
            ).await.map_err(|e| BarcaError::Db(format!("failed to measure the cache: {e}")))?;
            let through = rows.next().await
                .map_err(|e| BarcaError::Db(format!("failed to measure the cache: {e}")))?
                .and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0);
            drop(rows);
            let bytes = match previous.filter(|p| p.through <= through) {
                Some(previous) => Some({
                    // An upper bound: reused paths can be counted again. Near the limit,
                    // the full survey measures actual files before planning eviction.
                    let mut rows = conn.query(
                        "SELECT COALESCE(SUM(artifact_size_bytes),0) FROM materializations NOT INDEXED WHERE id > ?1 AND status = 'success' AND artifact_path IS NOT NULL AND evicted_at IS NULL",
                        [previous.through]
                    ).await.map_err(|e| BarcaError::Db(format!("failed to measure the cache: {e}")))?;
                    let added = rows.next().await
                        .map_err(|e| BarcaError::Db(format!("failed to measure the cache: {e}")))?
                        .and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0).max(0) as u64;
                    previous.bytes.saturating_add(added)
                }),
                None => None,
            };
            Ok((through, bytes))
        }).await?;
        // Every row's: read a chunk at a time, outside the lock this one held.
        let estimate = match estimate {
            (through, Some(bytes)) => Recorded { through, bytes },
            (through, None) => Recorded {
                through,
                bytes: recorded_bytes(&env.db_path).await?,
            },
        };
        recorded = recorded.saturating_add(estimate.bytes);
        updated.insert(env.db_path.clone(), estimate);
    }
    let walked = walk(project, false);
    let measured = Measurement {
        size_bytes: (recorded as i64 + state.artifact_adjust).max(0) as u64 + walked.other,
        backup_bytes: walked.outside.get(&Held::Backups).copied().unwrap_or(0),
    };
    if let Some(mut latest) = read_state(project) {
        latest.recorded = updated;
        latest.measurement = Some(measured);
        write_state(project, &latest);
    }
    Ok(measured)
}

/// Everything a plan needs, from a full walk.
struct Survey {
    total: u64,
    rows: Vec<plan::Row>,
    leftovers: Vec<plan::Leftover>,
    /// The total by what holds it.
    held: BTreeMap<Held, u64>,
}

/// The successful rows of one environment, with their files. `walked` is the size of each file
/// a walk just found, by [`Project::key`]: a row's file is looked up there, and looked at on
/// disk only when the walk did not see it (an artifact tree behind a symlink, which the walk
/// does not follow, or a file written since). A survey of 2.8M rows looked at a file for each.
async fn rows_of(
    project: &Project,
    env: &EnvDb,
    walked: Option<&HashMap<PathBuf, u64>>,
) -> Result<(Vec<plan::Row>, u64), BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read {}: {e}", env.db_path));
    let recorded = recorded_bytes(&env.db_path).await?;
    let raw = scan(
        &env.db_path,
        "node_id, run_hash, COALESCE(artifact_path, ''), COALESCE(created_at, ''), \
         last_read_at, output_hash IS NOT NULL",
        "status = 'success'",
        |row| {
            Ok((
                row.get::<i64>(0).map_err(failed)?,
                row.get::<String>(1).map_err(failed)?,
                row.get::<String>(2).ok(),
                row.get::<String>(3).unwrap_or_default(),
                row.get::<String>(4).unwrap_or_default(),
                row.get::<String>(5).ok(),
                row.get::<i64>(6).unwrap_or(0) != 0,
            ))
        },
    )
    .await?;
    let rows = raw
        .into_iter()
        .map(
            |(id, node_id, run_hash, recorded, created_at, last_read_at, digest)| {
                let (file, store_held) = local_file(project, env, &recorded, digest);
                let seen = file
                    .as_ref()
                    .and_then(|f| walked.and_then(|w| w.get(&project.key(f))).copied());
                let (present, bytes) = match seen {
                    Some(bytes) => (true, bytes),
                    None => {
                        let meta = file.as_ref().and_then(|f| std::fs::metadata(f).ok());
                        let present = meta.as_ref().is_some_and(|m| m.is_file());
                        (
                            present,
                            meta.filter(|_| present).map(|m| m.len()).unwrap_or(0),
                        )
                    }
                };
                plan::Row {
                    env: env.env.clone(),
                    id,
                    node_id,
                    run_hash,
                    recorded,
                    file,
                    bytes,
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

/// The files on this disk that some row of `env` names (any status, evicted or not), keyed
/// by [`Project::key`]. `None` when a row names a store location this machine cannot map to
/// its local copy (the environment's configuration does not resolve): then no file is known
/// not to be named.
async fn named_of(project: &Project, env: &EnvDb) -> Result<Option<Vec<PathBuf>>, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read {}: {e}", env.db_path));
    let mut seen = HashSet::new();
    let recorded: Vec<String> = scan(
        &env.db_path,
        "artifact_path",
        "artifact_path IS NOT NULL AND artifact_path != ''",
        |row| row.get::<String>(1).map_err(failed),
    )
    .await?
    .into_iter()
    .filter(|path| seen.insert(path.clone()))
    .collect();
    let mut out = Vec::with_capacity(recorded.len());
    for path in recorded {
        match local_file(project, env, &path, false).0 {
            Some(file) => out.push(project.key(&file)),
            // A store location with no local mirror known: its copy here cannot be told apart.
            None if env.layout.is_none() && crate::transfer::local_path(&path).is_none() => {
                return Ok(None);
            }
            None => {}
        }
    }
    Ok(Some(out))
}

/// The artifact files no row of any environment names, as leftovers. Empty when that cannot
/// be known (see [`named_of`]). Files written in the last [`UNREFERENCED_GRACE`] are left out
/// (they are still counted as [`Held::Unreferenced`]).
async fn unreferenced(
    project: &Project,
    files: &[(PathBuf, u64, u64)],
) -> Result<(Vec<plan::Leftover>, u64), BarcaError> {
    let mut named: HashSet<PathBuf> = HashSet::new();
    let mut known = true;
    // A file older than every metadata database cannot be waiting for its row: the run that
    // wrote it recorded into a database that is gone (`barca state reset`, `restore`).
    let mut born: Option<SystemTime> = None;
    for env in envs(project) {
        let created = std::fs::metadata(&env.db_path)
            .and_then(|m| m.created())
            .ok();
        born = match (born, created) {
            (Some(b), Some(c)) => Some(b.min(c)),
            (None, c) if c.is_some() => c,
            // A database whose creation time cannot be read: no file is known to predate it.
            _ => Some(UNIX_EPOCH),
        };
        match named_of(project, &env).await? {
            Some(paths) => named.extend(paths),
            None => known = false,
        }
    }
    let mut candidates: Vec<&(PathBuf, u64, u64)> = files
        .iter()
        .filter(|(path, _, _)| !named.contains(&project.key(path)))
        .collect();
    // A path spelled through a symlink below `.barca/`: compare resolved paths, only when there
    // is something to compare.
    if !candidates.is_empty() {
        let resolved: HashSet<PathBuf> = named
            .iter()
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        candidates.retain(|(path, _, _)| {
            std::fs::canonicalize(path).map_or(true, |c| !resolved.contains(&c))
        });
    }
    let bytes = candidates.iter().map(|(_, b, _)| b).sum();
    if !known {
        return Ok((Vec::new(), 0));
    }
    let fresh_after = now_secs().saturating_sub(UNREFERENCED_GRACE.as_secs());
    let leftovers = candidates
        .into_iter()
        .filter(|(path, _, modified)| {
            *modified <= fresh_after
                || born.is_some_and(|born| {
                    std::fs::symlink_metadata(path)
                        .and_then(|m| m.modified())
                        .is_ok_and(|m| m < born)
                })
        })
        .map(|(path, bytes, modified)| plan::Leftover {
            kind: LeftoverKind::Unreferenced,
            path: path.clone(),
            with: Vec::new(),
            bytes: *bytes,
            modified: *modified,
        })
        .collect();
    Ok((leftovers, bytes))
}

/// Walk everything and read every environment's rows; remember the correction for
/// [`estimate`].
async fn survey(project: &Project, envs: &[EnvDb]) -> Result<Survey, BarcaError> {
    let walked = walk(project, true);
    let sizes: HashMap<PathBuf, u64> = walked
        .files
        .iter()
        .map(|(path, bytes, _)| (project.key(path), *bytes))
        .collect();
    let mut rows = Vec::new();
    let mut recorded = 0u64;
    for env in envs {
        let (env_rows, env_recorded) = rows_of(project, env, Some(&sizes)).await?;
        rows.extend(env_rows);
        recorded += env_recorded;
    }
    let (unnamed, unnamed_bytes) = unreferenced(project, &walked.files).await?;
    let moved_aside: u64 = walked.moved_aside.iter().map(|l| l.bytes).sum();
    let mut held = walked.outside.clone();
    *held.entry(Held::Other).or_default() += moved_aside;
    *held.entry(Held::Unreferenced).or_default() += unnamed_bytes;
    *held.entry(Held::Results).or_default() += walked
        .artifacts
        .saturating_sub(moved_aside)
        .saturating_sub(unnamed_bytes);
    write_state(
        project,
        &State {
            measurement: Some(Measurement {
                size_bytes: walked.other + walked.artifacts,
                backup_bytes: walked.outside.get(&Held::Backups).copied().unwrap_or(0),
            }),
            accounting_version: 1,
            surveyed_at: now_secs(),
            recorded: BTreeMap::new(),
            artifact_adjust: walked.artifacts as i64 - recorded as i64,
            store_checks: read_state(project)
                .map(|s| s.store_checks)
                .unwrap_or_default(),
            ..Default::default()
        },
    );
    let mut leftovers = staging_leftovers(project);
    leftovers.extend(walked.moved_aside);
    leftovers.extend(unnamed);
    leftovers.extend(sql_cache_leftovers(project));
    leftovers.extend(range_cache_leftovers(project));
    held.retain(|_, bytes| *bytes > 0);
    Ok(Survey {
        total: walked.other + walked.artifacts,
        rows,
        leftovers,
        held,
    })
}

// ─── Protections ────────────────────────────────────────────────────────────

/// What a command protects of its own: its outputs, what it read and returned, and its run
/// hashes; and the `Manual` nodes of its DAG.
#[derive(Debug, Default, Clone)]
pub(crate) struct Own {
    pub recorded: HashSet<String>,
    pub run_hashes: HashSet<String>,
    /// Current results proven fresh without materializing each key's cache state.
    pub nodes: HashSet<String>,
    pub manual: HashSet<String>,
    /// The command added nothing to `.barca/`: no step ran and nothing was fetched from a
    /// store. Its check is skipped when the last check, under the same limit, found the cache
    /// under it (`State::checked_size`).
    pub wrote_nothing: bool,
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

/// The recorded locations of this host's results whose upload is still pending (#390), in every
/// environment: the local copy is the only one, so it is never evicted until the upload succeeds.
/// The second value is set when a database could not be read; eviction then keeps to leftovers.
async fn pending_uploads(envs: &[EnvDb]) -> (HashSet<String>, Option<String>) {
    let mut out = HashSet::new();
    let mut unread = None;
    for env in envs {
        match crate::pending_uploads::list(&env.db_path).await {
            Ok(rows) => out.extend(rows.into_iter().map(|p| p.store_path)),
            Err(e) => unread = Some(e.to_string()),
        }
    }
    (out, unread)
}

async fn protections(
    project: &Project,
    envs: &[EnvDb],
    rows: &[plan::Row],
    own: &Own,
    pinned: &Pinned,
) -> (Protections, Option<String>) {
    let elsewhere = live_elsewhere(envs, &pinned.run_ids).await;
    let (pending, pending_unread) = pending_uploads(envs).await;
    let pending_failed = pending_unread.is_some();
    let restricted = if let Some(first) = elsewhere.first() {
        Some(format!("run {first} is in progress"))
    } else if let Some(error) = pending_unread {
        Some(format!("the pending uploads could not be read ({error})"))
    } else if pinned.unresolved {
        Some("a run in this process is planning".to_string())
    } else {
        None
    };
    let manual = manual_nodes(project, rows);
    let recorded: HashSet<String> = own.recorded.iter().cloned().chain(pending).collect();
    // The same, as files: a file this command read may be one no row of this machine names (a
    // server ledger hit fetched from the store).
    let mut paths = HashSet::new();
    for env in envs {
        for r in &recorded {
            if let Some(file) = local_file(project, env, r, false).0 {
                paths.insert(project.key(&file));
                paths.insert(file);
            }
        }
    }
    let protect = Protections {
        paths,
        recorded,
        run_hashes: own
            .run_hashes
            .iter()
            .chain(&pinned.run_hashes)
            .cloned()
            .collect(),
        nodes: pinned.nodes.iter().chain(&own.nodes).cloned().collect(),
        manual_known: manual.is_some(),
        manual: manual
            .unwrap_or_default()
            .into_iter()
            .chain(own.manual.iter().cloned())
            .collect(),
        max_tier: match (&restricted, pending_failed) {
            (_, true) => Some(Tier::Leftovers),
            (Some(_), false) => Some(Tier::Superseded),
            (None, false) => None,
        },
        keep_staging: !elsewhere.is_empty(),
        keep_unreferenced: !elsewhere.is_empty()
            || pinned.others
            || pinned.unresolved
            || pending_failed,
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
    let _ = crate::db::with_tx(&conn, "BEGIN IMMEDIATE", "eviction marks", async {
        for path in paths {
            conn.execute(
                "UPDATE materializations SET evicted_at = datetime('now') \
                 WHERE artifact_path = ?1 AND status = 'success'",
                [*path],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to mark an eviction: {e}")))?;
        }
        Ok(())
    })
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
    let checked = off_the_event_loop(check(cfg.cache_max_size, own, pin)).await;
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

/// `fut`, on a thread of its own under a multi-threaded runtime (`barca serve`'s): a survey walks
/// every file of `.barca/` and looks at the file of every row, minutes of blocking calls over
/// hundreds of thousands of artifacts, which on one of the server's two event-loop threads held
/// up its requests and its runs. A single-threaded runtime (a CLI command) has nothing else to
/// run meanwhile and polls it as it is.
async fn off_the_event_loop<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }
        _ => fut.await,
    }
}

/// Keep each metadata DB within its share of `limit` ([`crate::metadata_size`]). One that cannot
/// get there with everything removable gone is not looked at again until [`STUCK_FOR`] passes,
/// or until it has grown a tenth. Never fails a command. True when one ran out of `time` while
/// it was still removing things. One waiting for a moment with no run alive does not count:
/// that can last, and artifacts cannot go unbounded meanwhile.
async fn trim_metadata(
    project: &Project,
    envs: &[EnvDb],
    limit: Option<u64>,
    time: Option<Duration>,
) -> bool {
    let mut under_way = false;
    let allowance = crate::metadata_size::allowance(limit);
    for env in envs {
        let size = std::fs::metadata(&env.db_path).map_or(0, |m| m.len());
        if size <= allowance {
            continue;
        }
        let short = read_state(project).and_then(|s| s.metadata_short.get(&env.db_path).copied());
        if time.is_some()
            && short.is_some_and(|(at, bytes)| {
                now_secs().saturating_sub(at) < STUCK_FOR.as_secs() && size <= bytes / 10 * 11
            })
        {
            continue;
        }
        match crate::metadata_size::trim(&env.db_path, allowance, time).await {
            Ok(Some(done)) => {
                under_way |= done.out_of_time && done.removed();
                if done.after_bytes < done.before_bytes || (done.finished && done.removed()) {
                    crate::errln!(
                        "[barca] metadata {} over its {} share of the limit: now {}; cleared \
                         {} component records and {} run reports, removed {} old results and \
                         {} old runs",
                        size::human(done.before_bytes),
                        size::human(allowance),
                        size::human(done.after_bytes),
                        done.components,
                        done.outcomes,
                        done.rows,
                        done.runs
                    );
                }
                let mut state = read_state(project).unwrap_or_default();
                if done.finished && done.after_bytes > allowance {
                    state
                        .metadata_short
                        .insert(env.db_path.clone(), (now_secs(), done.after_bytes));
                } else {
                    state.metadata_short.remove(&env.db_path);
                }
                write_state(project, &state);
            }
            Ok(None) => {}
            Err(e) => crate::errln!("[barca] warning: could not trim {}: {e}", env.db_path),
        }
    }
    under_way
}

/// What the metadata DBs hold past their share of `limit`. Eviction leaves this out of what it
/// must free: the DB is [`trim_metadata`]'s to shrink, and deleting artifacts cannot do it, so
/// counting it would delete artifacts that fit (a 41 GB file against a 13 GB limit evicts them
/// all). Artifacts stay within the limit less the DB's share either way.
fn metadata_excess(envs: &[EnvDb], limit: Option<u64>) -> u64 {
    // Under this a database is little more than its schema, which no trim can remove.
    const SMALL: u64 = 1 << 20;
    let allowance = crate::metadata_size::allowance(limit).max(SMALL);
    envs.iter()
        .map(|env| {
            let bytes = |path: &str| std::fs::metadata(path).map_or(0, |m| m.len());
            (bytes(&env.db_path) + bytes(&format!("{}-wal", env.db_path))).saturating_sub(allowance)
        })
        .sum()
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

/// The `.barca/` directories this process is surveying now ([`check_in`]).
static SURVEYING: std::sync::Mutex<Option<HashSet<PathBuf>>> = std::sync::Mutex::new(None);

/// A survey of one `.barca/` under way, until dropped.
struct Surveying(PathBuf);

impl Surveying {
    /// `None` when this process surveys `barca` already.
    fn start(barca: &Path) -> Option<Self> {
        let mut surveying = SURVEYING.lock().unwrap();
        surveying
            .get_or_insert_with(HashSet::new)
            .insert(barca.to_path_buf())
            .then(|| Self(barca.to_path_buf()))
    }
}

impl Drop for Surveying {
    fn drop(&mut self) {
        if let Some(surveying) = SURVEYING.lock().unwrap().as_mut() {
            surveying.remove(&self.0);
        }
    }
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
    if trim_metadata(project, &envs, Some(limit), Some(AFTER_COMMAND_BUDGET)).await {
        // The file is about to shrink: evicting against its size now would delete artifacts
        // that fit.
        return Ok(None);
    }
    let state = read_state(project);
    let fresh = state
        .as_ref()
        .is_some_and(|s| now_secs().saturating_sub(s.surveyed_at) < SURVEY_EVERY.as_secs());
    if let (true, Some(state)) = (fresh, &state) {
        if own.wrote_nothing
            && state.accounting_version == 1
            && state.checked_limit == limit
            && state.checked_size > 0
            && state.checked_size <= limit
        {
            remember(
                state
                    .measurement
                    .map_or(state.checked_size, |m| m.size_bytes),
                limit,
            );
            return Ok(None);
        }
        let measured = estimate(project, &envs, state).await?;
        let size = measured
            .budget_bytes()
            .saturating_sub(metadata_excess(&envs, Some(limit)));
        remember(measured.size_bytes, limit);
        if size <= limit {
            checked(project, size, limit);
            return Ok(None);
        }
        // Eviction could not get under the limit a moment ago and nothing has grown since:
        // planning again would find the same.
        let stuck = state.accounting_version == 1
            && now_secs().saturating_sub(state.short_at) < STUCK_FOR.as_secs()
            && size
                <= state
                    .short_size
                    .saturating_add((limit / 100).max(STUCK_SLACK));
        if stuck {
            return Ok(Some(CacheReport {
                size_bytes: measured.size_bytes,
                limit_bytes: limit,
                evicted_files: 0,
                evicted_bytes: 0,
            }));
        }
    }
    // One survey at a time in this process: a run that ends while `serve` surveys would walk
    // the same files again, and wait for it before it returns. The survey under way records
    // what this one would.
    let Some(_surveying) = Surveying::start(&project.barca) else {
        return Ok(None);
    };
    let survey = survey(project, &envs).await?;
    let backup_bytes = survey.held.get(&Held::Backups).copied().unwrap_or(0);
    let budget_bytes = survey
        .total
        .saturating_sub(backup_bytes)
        .saturating_sub(metadata_excess(&envs, Some(limit)));
    remember(survey.total, limit);
    if budget_bytes <= limit {
        checked(project, budget_bytes, limit);
        return Ok(None);
    }
    let pinned = pinned_except(&project.root, pin.map(|p| p.id));
    let (protect, restricted) = protections(project, &envs, &survey.rows, own, &pinned).await;
    let plan = plan::plan(
        budget_bytes,
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
            size::human(budget_bytes),
            size::human(limit),
            done.evicted.len(),
            if done.evicted.len() == 1 { "" } else { "s" },
            size::human(evicted_bytes),
            tier_words(&per_tier(&done.evicted))
        );
    }
    if short && restricted.is_none() && done.evicted.len() == evictable {
        if protect.manual_known {
            warn_working_set(size_after.saturating_sub(backup_bytes), limit);
        }
        if let Some(mut state) = read_state(project) {
            state.short_at = now_secs();
            state.short_size = size_after.saturating_sub(backup_bytes);
            state.measurement = Some(Measurement {
                size_bytes: size_after,
                backup_bytes,
            });
            write_state(project, &state);
        }
    }
    record_measurement(
        project,
        Measurement {
            size_bytes: size_after,
            backup_bytes,
        },
    );
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

/// What holds part of the space left under `.barca/` after `barca gc`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcHeld {
    /// `results`, `unreferenced`, `metadata`, `db_copies`, `backups` or `other`.
    pub what: String,
    pub bytes: u64,
    /// Whether eviction may ever free it (`false` for the databases and backups).
    pub evictable: bool,
}

/// What an eviction frees, in [`Held`] terms.
fn held_of(e: &Eviction) -> Held {
    match e.leftover {
        Some(LeftoverKind::Unreferenced) => Held::Unreferenced,
        Some(_) => Held::Other,
        None => Held::Results,
    }
}

/// `held` after `evicted`, largest first.
fn held_after(mut held: BTreeMap<Held, u64>, evicted: &[Eviction]) -> Vec<GcHeld> {
    for e in evicted {
        if let Some(bytes) = held.get_mut(&held_of(e)) {
            *bytes = bytes.saturating_sub(e.bytes);
        }
    }
    let mut out: Vec<(Held, u64)> = held.into_iter().filter(|(_, b)| *b > 0).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.into_iter()
        .map(|(what, bytes)| GcHeld {
            what: what.code().to_string(),
            bytes,
            evictable: what.evictable(),
        })
        .collect()
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
    /// Bytes subject to the cache quota; excludes recognized metadata backups.
    pub budget_before_bytes: u64,
    pub budget_after_bytes: u64,
    pub backup_bytes: u64,
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
    /// What holds the space left (`after_bytes`), largest first.
    pub held: Vec<GcHeld>,
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
    if !opts.dry_run {
        trim_metadata(project, &envs, limit, None).await;
    }
    let survey = if project.barca.is_dir() {
        survey(project, &envs).await?
    } else {
        Survey {
            total: 0,
            rows: Vec::new(),
            leftovers: Vec::new(),
            held: BTreeMap::new(),
        }
    };
    let pinned = pinned_except(&project.root, None);
    let backup_bytes = survey.held.get(&Held::Backups).copied().unwrap_or(0);
    let budget_before_bytes = survey.total.saturating_sub(backup_bytes);
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
        budget_before_bytes.saturating_sub(metadata_excess(&envs, limit)),
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
    let held = held_after(survey.held, &evicted);
    if let Some(limit) = limit {
        remember(survey.total.saturating_sub(evicted_bytes), limit);
    }
    if !opts.dry_run {
        record_measurement(
            project,
            Measurement {
                size_bytes: survey.total.saturating_sub(evicted_bytes),
                backup_bytes,
            },
        );
    }
    Ok(GcResult {
        dry_run: opts.dry_run,
        size_bytes: survey.total,
        budget_before_bytes,
        budget_after_bytes: budget_before_bytes.saturating_sub(evicted_bytes),
        backup_bytes,
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
        held,
    })
}

fn project_relative(project: &Project, path: &Path) -> String {
    path.strip_prefix(&project.root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Bytes under `dir` (an artifact directory), for `barca state reset` to say what stays.
pub fn bytes_under(dir: &Path) -> u64 {
    du(dir).0
}

/// Size, limit and free space, for `barca doctor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub size_bytes: u64,
    pub budget_bytes: u64,
    pub backup_bytes: u64,
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
    let size_bytes = walked.other + walked.artifacts;
    let backup_bytes = walked.outside.get(&Held::Backups).copied().unwrap_or(0);
    Ok(Usage {
        size_bytes,
        budget_bytes: size_bytes.saturating_sub(backup_bytes),
        backup_bytes,
        limit_bytes: cfg.cache_max_size.resolve(&project.barca),
        max_size: cfg.cache_max_size.describe(),
        source: cfg.cache_max_size_source.to_string(),
        filesystem_bytes: fs.map(|f| f.total),
        available_bytes: fs.map(|f| f.available),
    })
}

#[cfg(test)]
mod tests;
