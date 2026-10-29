//! Metadata backups and `barca state restore` (RFC-0009).
//!
//! With a remote store (`[remote] uri`), the store is this machine's backup and read-through
//! cache. Routine checkpoints, served-run ends and the server timer share a configurable
//! admission interval (default one minute). Final standalone/server backups bypass it.
//! Copies go to `<backups>/<name>.db` beside the database
//! (`.barca/backups/`) and then to `{uri}/{env}/state/backups/<name>.db` in the store, each with
//! a `<name>.json` sidecar ([`Sidecar`]). Names are the UTC time to the millisecond and a short
//! suffix ([`name_at`]). A copy that would be the same bytes as the newest one
//! is not taken. Each place keeps the [`BACKUPS_KEPT`] newest, pruned after a successful write.
//!
//! Nothing is ever downloaded before a run: this machine's database is the truth for this
//! machine. [`restore`] is the only way a database comes back from a backup, and it runs only
//! when asked.
//!
//! The copy is the database's main file, taken under the database's lock right after its
//! write-ahead log was folded in ([`crate::db::copy_for_backup`]), so a write in progress never
//! produces a torn copy. The store side is `python -m barca._state` (one process per backup),
//! so the fsspec extras stay the only cloud-auth surface.
//!
//! **Which machine.** A store without a server is one machine's backup (DIRECTION.md). Each
//! sidecar names the host and the project directory that wrote it ([`Writer`]). Pruning the
//! store keeps the newest backup of every other writer, a backup that finds another writer's
//! backup newer than its own previous one warns once per process ([`warn_interleaved`]), and
//! `barca state restore` without `--from` prefers this machine's newest backup.

use crate::BarcaError;
use crate::config::{BACKUPS_KEPT, ResolvedConfig};
use crate::helper_proc::Until;
use crate::helper_proc::{OutputFailed, run_to_output};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod cadence;
pub use cadence::Request;

/// What a backup's `<name>.json` sidecar says about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    /// The run that took it, when a run did (`null` for `barca serve`'s minute backup).
    pub run_id: Option<String>,
    /// Run and materialization rows in the copy.
    pub rows: u64,
    /// SHA-256 of the copy.
    pub digest: String,
    /// The barca that wrote it.
    pub barca_version: String,
    /// The metadata format of the copy (`PRAGMA user_version`; `specs/interfaces/metadata-schema.md`).
    pub db_format: i64,
    /// The host that wrote it (`null` in a sidecar from before 0.29).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The project directory that wrote it, absolute (`null` before 0.29).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Who writes backups: this host and this project directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writer {
    pub host: String,
    pub project: String,
}

impl Writer {
    /// This host, and the project directory whose metadata database is `db_path` (the
    /// directory holding its `.barca/`), absolute.
    pub fn of(db_path: &str) -> Self {
        let db = Path::new(db_path);
        let abs = if db.is_absolute() {
            db.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(db)
        };
        let dir = abs
            .ancestors()
            .find(|a| a.file_name().is_some_and(|n| n == ".barca"))
            .and_then(Path::parent)
            .or_else(|| abs.parent())
            .unwrap_or(&abs)
            .to_path_buf();
        let project = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self {
            host: crate::db::local_host(),
            project: project.to_string_lossy().into_owned(),
        }
    }

    /// Whether `host` and `project` (from a sidecar) are this writer. `None` when the sidecar
    /// does not say (written before 0.29).
    fn is(&self, host: Option<&str>, project: Option<&str>) -> Option<bool> {
        let host = host?;
        Some(host == self.host && project.is_none_or(|p| p == self.project))
    }

    /// `host` for another host, `host:/path` for another directory on this one.
    pub fn describe(&self, host: &str, project: Option<&str>) -> String {
        match project {
            Some(p) if host == self.host => format!("{host}:{p}"),
            _ => host.to_string(),
        }
    }
}

static INTERLEAVED_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The warning when `store` also holds `name`, taken by `other` after this writer's own newest
/// backup. Once per process.
pub fn warn_interleaved(store: &str, other: &str, name: &str) {
    use std::sync::atomic::Ordering;
    if INTERLEAVED_WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    crate::errln!(
        "[barca] warning: {store} also holds backups from {other} (the newest {name}), taken \
         after this machine's own: two machines are backing up to one store, and nothing \
         coordinates them. barca state restore picks this machine's newest backup unless given \
         --from. To share state between machines, use a server (barca docs server-mode); \
         otherwise give each machine its own [remote] uri"
    );
}

/// `2026-10-14T18-02-11.123Z-1a2b3c4d.db`: the UTC time to the millisecond, then the first eight
/// hex digits of a digest of this machine's host name, the database's path and the copy's
/// digest. Several directories backing up to one store location in the same millisecond still
/// get different names (#487); names sort by age ([`order_key`]).
pub(crate) fn name_at(at: chrono::DateTime<chrono::Utc>, db_path: &str, digest: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(crate::db::local_host().as_bytes());
    h.update(b"\n");
    h.update(db_path.as_bytes());
    h.update(b"\n");
    h.update(digest.as_bytes());
    let suffix: String = h.finalize()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{}Z-{suffix}.db", at.format("%Y-%m-%dT%H-%M-%S%.3f"))
}

/// Whether `name` has the form of a backup ([`name_at`]), or the to-the-second form 0.26.0
/// wrote (`2026-10-14T18-02-11Z.db`), which is still listed, restored and pruned. Nothing else
/// in a backup directory is ever listed or removed.
pub(crate) fn is_backup_name(name: &str) -> bool {
    let b = name.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let second = b.len() >= 23
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b'-'
        && digits(14..16)
        && b[16] == b'-'
        && digits(17..19);
    if !second {
        return false;
    }
    let rest = &b[19..];
    rest == b"Z.db"
        || (rest.len() == 17
            && rest[0] == b'.'
            && rest[1..4].iter().all(u8::is_ascii_digit)
            && &rest[4..6] == b"Z-"
            && rest[6..14]
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
            && &rest[14..] == b".db")
}

/// What backups sort by, oldest first: a to-the-second name counts as millisecond `.000`.
fn order_key(name: &str) -> String {
    match name.strip_suffix("Z.db") {
        Some(second) if second.len() == 19 => format!("{second}.000Z"),
        _ => name.to_string(),
    }
}

fn stem(name: &str) -> &str {
    name.strip_suffix(".db").unwrap_or(name)
}

fn sidecar_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.json", stem(name)))
}

/// Marks a local backup the store has not confirmed yet. Removed by a successful upload; while
/// it is there, the next backup uploads the newest copy even when nothing changed.
fn unsent_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.unsent", stem(name)))
}

/// Files owned by the local backup layout, relative to the project's `.barca/`.
/// A directory merely named `backups` and unrelated files are not quota exemptions.
pub(crate) fn owned_local_file(rel: &Path) -> bool {
    let Some(parts): Option<Vec<_>> = rel.iter().map(|p| p.to_str()).collect() else {
        return false;
    };
    let name = match parts.as_slice() {
        ["backups", name] => *name,
        ["envs", env, "backups", name]
            if !env.is_empty()
                && env
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) =>
        {
            *name
        }
        _ => return false,
    };
    if matches!(
        name,
        ".covered-run" | ".admission.lock" | ".last-attempt" | ".last-attempt.tmp"
    ) {
        return true;
    }
    if is_backup_name(name) {
        return true;
    }
    for suffix in [".json", ".unsent"] {
        if let Some(stem) = name.strip_suffix(suffix)
            && is_backup_name(&format!("{stem}.db"))
        {
            return true;
        }
    }
    if let Some(stem) = name
        .strip_prefix('.')
        .and_then(|s| s.strip_suffix(".json.tmp"))
    {
        return is_backup_name(&format!("{stem}.db"));
    }
    let tmp = name
        .strip_suffix("-wal")
        .or_else(|| name.strip_suffix("-shm"))
        .unwrap_or(name);
    tmp.strip_prefix(".backup-")
        .and_then(|s| s.strip_suffix(".tmp"))
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// The backups in `dir`, newest first.
fn local_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| is_backup_name(n))
        .collect();
    names.sort_unstable_by_key(|n| std::cmp::Reverse(order_key(n)));
    names
}

fn read_sidecar(path: &Path) -> Option<Sidecar> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The run whose end-of-run backup was skipped last (#554), in the local backup directory.
fn covered_path(dir: &Path) -> PathBuf {
    dir.join(".covered-run")
}

/// Whether the end-of-run backup of `run_id`, a run that recorded no new result, may be skipped
/// (#554): every other run is already in a backup the store confirmed. That is, the newest local
/// backup has no unsent marker, and the run recorded before `run_id` succeeded and either took
/// it or was itself skipped by this rule. A run that was killed, cancelled (even after its
/// backup, while its output was fetched), failed, or failed to back up is then backed up by the
/// next run, whatever that run did.
pub(crate) async fn covered(cfg: &ResolvedConfig, run_id: &str) -> bool {
    let dir = cfg.local_backup_dir();
    let Some(newest) = local_names(&dir).into_iter().next() else {
        return false;
    };
    if unsent_path(&dir, &newest).exists() {
        return false;
    }
    let Some((previous, status)) = previous_run(&cfg.db_path, run_id).await else {
        return false;
    };
    if status != "success" {
        return false;
    }
    let took_it = read_sidecar(&sidecar_path(&dir, &newest))
        .and_then(|s| s.run_id)
        .is_some_and(|r| r == previous);
    took_it || std::fs::read_to_string(covered_path(&dir)).is_ok_and(|r| r.trim() == previous)
}

/// Remember that `run_id`'s end-of-run backup was skipped by [`covered`].
pub(crate) fn mark_covered(cfg: &ResolvedConfig, run_id: &str) {
    std::fs::write(covered_path(&cfg.local_backup_dir()), run_id).ok();
}

/// The run recorded last before `run_id`, and its status.
async fn previous_run(db_path: &str, run_id: &str) -> Option<(String, String)> {
    let _g = crate::db::db_guard().await;
    let (_db, conn) = crate::db::open_conn(db_path).await.ok()?;
    let mut rows = conn
        .query(
            "SELECT run_id, status FROM runs WHERE run_id != ?1 ORDER BY rowid DESC LIMIT 1",
            [run_id],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    Some((row.get::<String>(0).ok()?, row.get::<String>(1).ok()?))
}

/// What one backup did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// Admission skipped expensive work; this result proves no backup coverage.
    pub skipped: bool,
    /// The copy written now, or `None` when the database was still the newest copy.
    pub name: Option<String>,
    /// Its size, when one was written.
    pub size_bytes: u64,
    /// The upload to the store, when one was made: `Err` says why it failed. The local copy
    /// stands either way, and the next backup uploads it.
    pub upload: Option<Result<String, String>>,
}

/// Take a backup of the database at `cfg.db_path` when a store is configured: write the local
/// copy (unless the database is still the newest copy), prune the local directory, then upload
/// the newest copy and prune the store, when it is not there yet. `Err` is a failure of the
/// local copy (the lock, the disk) or `until` stopping it ([`BarcaError::Cancelled`]); a failed
/// upload is [`Taken::upload`].
pub async fn take(
    python: &Path,
    cfg: &ResolvedConfig,
    run_id: Option<&str>,
    until: Until<'_>,
) -> Result<Taken, BarcaError> {
    take_requested(python, cfg, run_id, Request::Final, until).await
}

pub async fn take_requested(
    python: &Path,
    cfg: &ResolvedConfig,
    run_id: Option<&str>,
    request: Request,
    until: Until<'_>,
) -> Result<Taken, BarcaError> {
    let mut taken = Taken {
        skipped: false,
        name: None,
        size_bytes: 0,
        upload: None,
    };
    let Some(store_dir) = cfg.backup_uri.as_deref() else {
        return Ok(taken);
    };
    if !Path::new(&cfg.db_path).exists() {
        return Ok(taken);
    }
    let Some(_admission) = cadence::admit(cfg, request, until).await? else {
        taken.skipped = true;
        return Ok(taken);
    };
    let dir = cfg.local_backup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        BarcaError::Other(format!(
            "failed to create the backup directory {}: {e}",
            dir.display()
        ))
    })?;
    let newest = local_names(&dir).into_iter().next();
    let newest_digest = newest
        .as_deref()
        .and_then(|n| read_sidecar(&sidecar_path(&dir, n)))
        .map(|s| s.digest);
    let staged = dir.join(format!(".backup-{}.tmp", std::process::id()));
    let copied =
        crate::db::copy_for_backup(&cfg.db_path, &staged, newest_digest.as_deref(), until).await?;
    let name = match copied {
        Some(digest) => {
            let written = write_local(&dir, &cfg.db_path, &staged, digest, run_id).await;
            let _ = std::fs::remove_file(&staged);
            let _ = crate::db::remove_sidecars(&staged.to_string_lossy());
            let (name, size) = written?;
            taken.name = Some(name.clone());
            taken.size_bytes = size;
            prune_local(&dir);
            name
        }
        None => match newest {
            Some(name) if unsent_path(&dir, &name).exists() => name,
            _ => return Ok(taken),
        },
    };
    let uploading = std::time::Instant::now();
    taken.upload = Some(upload(python, cfg, &dir, &name, store_dir, until).await);
    if std::env::var_os("BARCA_TRACE_TIMING").is_some() {
        let bytes = std::fs::metadata(dir.join(&name))
            .map(|m| m.len())
            .unwrap_or(0);
        crate::errln!(
            "[timing] metadata backup upload: {bytes}-byte copy in {:.3} ms (confirmed={})",
            uploading.elapsed().as_secs_f64() * 1000.0,
            matches!(taken.upload, Some(Ok(_)))
        );
    }
    Ok(taken)
}

/// Give the staged copy its name and its sidecar. Returns the name and the size.
async fn write_local(
    dir: &Path,
    db_path: &str,
    staged: &Path,
    digest: String,
    run_id: Option<&str>,
) -> Result<(String, u64), BarcaError> {
    let failed = |what: &str, e: &dyn std::fmt::Display| {
        BarcaError::Other(format!("failed to {what} in {}: {e}", dir.display()))
    };
    let (rows, db_format) = count_rows(staged).await?;
    let name = name_at(chrono::Utc::now(), db_path, &digest);
    let writer = Writer::of(db_path);
    let sidecar = Sidecar {
        run_id: run_id.map(str::to_string),
        rows,
        digest,
        barca_version: env!("CARGO_PKG_VERSION").to_string(),
        db_format,
        host: Some(writer.host),
        project: Some(writer.project),
    };
    let json = serde_json::to_vec(&sidecar).map_err(|e| failed("write a backup sidecar", &e))?;
    let size = std::fs::metadata(staged)
        .map_err(|e| failed("read the backup copy", &e))?
        .len();
    // The marker first and the sidecar last: a copy is listed with its sidecar only once it is
    // whole, and an interrupted backup is uploaded by the next one.
    std::fs::write(unsent_path(dir, &name), b"").map_err(|e| failed("mark a backup", &e))?;
    std::fs::rename(staged, dir.join(&name)).map_err(|e| failed("name a backup", &e))?;
    let side_tmp = dir.join(format!(".{}.json.tmp", stem(&name)));
    std::fs::write(&side_tmp, json).map_err(|e| failed("write a backup sidecar", &e))?;
    std::fs::rename(&side_tmp, sidecar_path(dir, &name))
        .map_err(|e| failed("write a backup sidecar", &e))?;
    Ok((name, size))
}

/// Run and materialization rows in the database at `path`, and its metadata format. Opens a
/// copy no other process knows about, so it takes no lock.
async fn count_rows(path: &Path) -> Result<(u64, i64), BarcaError> {
    let failed = |e: libsql::Error| BarcaError::Db(format!("failed to read the backup copy: {e}"));
    let db = libsql::Builder::new_local(path)
        .build()
        .await
        .map_err(failed)?;
    let conn = db.connect().map_err(failed)?;
    let format = crate::db_schema::read_version(&conn).await?;
    let mut rows = conn
        .query(
            "SELECT (SELECT COUNT(*) FROM runs) + (SELECT COUNT(*) FROM materializations)",
            (),
        )
        .await
        .map_err(failed)?;
    let count = match rows.next().await.map_err(failed)? {
        Some(row) => row.get::<i64>(0).map_err(failed)?,
        None => 0,
    };
    Ok((count.max(0) as u64, format))
}

/// Keep the [`BACKUPS_KEPT`] newest local backups.
fn prune_local(dir: &Path) {
    for old in local_names(dir).iter().skip(BACKUPS_KEPT) {
        let _ = std::fs::remove_file(dir.join(old));
        let _ = std::fs::remove_file(sidecar_path(dir, old));
        let _ = std::fs::remove_file(unsent_path(dir, old));
    }
}

/// Upload the local backup `name` to `store_dir` and prune the store. `Ok` is where it went
/// (credentials redacted); `Err` says why it did not.
async fn upload(
    python: &Path,
    cfg: &ResolvedConfig,
    dir: &Path,
    name: &str,
    store_dir: &str,
    until: Until<'_>,
) -> Result<String, String> {
    let display = crate::transfer::diagnostic_uri(store_dir);
    // A store that is a plain directory (a network share) must be there: a share that is not
    // mounted leaves an empty mount point, and writing into it would put the backup on this
    // disk, where nobody looks for it.
    if let Some(root) = store_root_dir(cfg, store_dir)
        && !root.is_dir()
    {
        return Err(format!(
            "backup to {display} failed: the store directory {} is not there; nothing was written",
            root.display()
        ));
    }
    let mut cmd = state_helper(python, cfg);
    cmd.arg("backup")
        .arg(dir.join(name))
        .arg(sidecar_path(dir, name))
        .arg(store_dir)
        .arg(BACKUPS_KEPT.to_string());
    let out = helper(cmd, until)
        .await
        .map_err(|e| format!("backup to {display} failed: {e}"))?;
    // Backups another writer took after this one's previous backup: two machines on one store.
    if let Ok(done) = serde_json::from_slice::<Uploaded>(&out)
        && let Some(newest) = done.others.first()
    {
        let here = Writer::of(&cfg.db_path);
        warn_interleaved(
            &display,
            &here.describe(&newest.host, newest.project.as_deref()),
            &newest.name,
        );
    }
    // Confirmed: no copy here is waiting for the store any more.
    for old in local_names(dir) {
        let _ = std::fs::remove_file(unsent_path(dir, &old));
    }
    Ok(format!("{}/{name}", display.trim_end_matches('/')))
}

/// What `barca._state backup` reports.
#[derive(Debug, Deserialize)]
struct Uploaded {
    /// Backups another writer took after this writer's previous one, newest first.
    #[serde(default)]
    others: Vec<OtherBackup>,
}

#[derive(Debug, Deserialize)]
struct OtherBackup {
    name: String,
    host: String,
    project: Option<String>,
}

/// For a store that is a plain directory, its root (`[remote] uri`): the backup prefix less
/// `/{env}/state/backups`. `None` for an object store.
fn store_root_dir(cfg: &ResolvedConfig, store_dir: &str) -> Option<PathBuf> {
    let local = crate::transfer::local_path(store_dir)?;
    let suffix = Path::new(&cfg.env).join("state").join("backups");
    let mut root = local.to_path_buf();
    if root.ends_with(&suffix) {
        for _ in 0..3 {
            root.pop();
        }
    }
    Some(root)
}

/// `python -m barca._state`, with the resolved storage options (the one place it is launched).
pub(crate) fn state_helper(python: &Path, cfg: &ResolvedConfig) -> tokio::process::Command {
    crate::helper_proc::python_module(python, "barca._state", cfg.storage_options_json.as_deref())
}

/// Run a `barca._state` helper to its end. `Err` is its own error line (or why it did not run).
async fn helper(cmd: tokio::process::Command, until: Until<'_>) -> Result<Vec<u8>, String> {
    let out = run_to_output(cmd, until.cancel, until.remaining())
        .await
        .map_err(|e| match e {
            OutputFailed::Cancelled => "stopped".to_string(),
            OutputFailed::TimedOut(limit) => {
                format!("it did not finish within {}s", limit.as_secs())
            }
            OutputFailed::Spawn(e) => format!("failed to start the state helper: {e}"),
        })?;
    if !out.status.success() {
        return Err(crate::helper_proc::helper_cause(&out.stderr));
    }
    Ok(out.stdout)
}

// ─── Listing ─────────────────────────────────────────────────────────────────

/// One backup, as `barca state backups` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupEntry {
    /// 1 for the newest; what `barca state restore --from N` takes.
    pub index: usize,
    /// `2026-10-14T18-02-11.123Z-1a2b3c4d.db` (`2026-10-14T18-02-11Z.db` from 0.26.0).
    pub name: String,
    /// `local`, `store` or `both`.
    #[serde(rename = "where")]
    pub location: String,
    pub size_bytes: u64,
    pub run_id: Option<String>,
    pub rows: Option<u64>,
    /// SHA-256 of the copy, from its sidecar.
    pub digest: Option<String>,
    /// With a copy in both places: whether the two agree (same size and digest). `null` when
    /// there is only one.
    pub agree: Option<bool>,
    pub barca_version: Option<String>,
    pub db_format: Option<i64>,
    /// The host that took it, from its sidecar (`null` for a backup from before 0.29).
    pub host: Option<String>,
    /// The project directory that took it, absolute (`null` before 0.29).
    pub project: Option<String>,
    /// Whether this host and project directory took it (`null` when the sidecar does not say).
    pub mine: Option<bool>,
}

/// The newest backup another writer took after this writer's newest one, when there is one
/// (and this writer has one: a machine with none of its own may be taking over from another).
pub fn interleaved(backups: &[BackupEntry]) -> Option<&BackupEntry> {
    let own = backups.iter().position(|b| b.mine == Some(true))?;
    backups[..own]
        .iter()
        .find(|b| b.mine == Some(false) && b.host.is_some())
}

/// What `barca state backups` prints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backups {
    /// Newest first.
    pub backups: Vec<BackupEntry>,
    /// This environment's local backup directory.
    pub local_dir: String,
    /// The store's backup prefix (credentials redacted), or `null` without a store.
    pub store: Option<String>,
    /// Why the store could not be listed; the list then holds the local copies only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StoreListing {
    backups: Vec<StoreBackup>,
}

#[derive(Debug, Deserialize)]
struct StoreBackup {
    name: String,
    size_bytes: u64,
    sidecar: Option<Sidecar>,
}

/// One backup, wherever its copies are.
struct Found {
    local: Option<(u64, Option<Sidecar>)>,
    store: Option<(u64, Option<Sidecar>)>,
}

/// Every backup of this environment, local and in the store, merged and newest first. A store
/// that cannot be listed is [`Backups::store_error`], not an error.
pub async fn list(python: &Path, cfg: &ResolvedConfig) -> Result<Backups, BarcaError> {
    let (found, store_error) = find(python, cfg).await;
    Ok(entries(cfg, &found, store_error))
}

async fn find(python: &Path, cfg: &ResolvedConfig) -> (BTreeMap<String, Found>, Option<String>) {
    let dir = cfg.local_backup_dir();
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for name in local_names(&dir) {
        let Ok(meta) = std::fs::metadata(dir.join(&name)) else {
            continue;
        };
        found.insert(
            name.clone(),
            Found {
                local: Some((meta.len(), read_sidecar(&sidecar_path(&dir, &name)))),
                store: None,
            },
        );
    }
    let mut store_error = None;
    if let Some(store_dir) = cfg.backup_uri.as_deref() {
        match list_store(python, cfg, store_dir).await {
            Ok(listed) => {
                for b in listed
                    .backups
                    .into_iter()
                    .filter(|b| is_backup_name(&b.name))
                {
                    found
                        .entry(b.name)
                        .or_insert(Found {
                            local: None,
                            store: None,
                        })
                        .store = Some((b.size_bytes, b.sidecar));
                }
            }
            Err(why) => store_error = Some(why),
        }
    }
    (found, store_error)
}

async fn list_store(
    python: &Path,
    cfg: &ResolvedConfig,
    store_dir: &str,
) -> Result<StoreListing, String> {
    let mut cmd = state_helper(python, cfg);
    cmd.arg("list").arg(store_dir);
    let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(60));
    let display = crate::transfer::diagnostic_uri(store_dir);
    let out = helper(
        cmd,
        Until {
            cancel: None,
            deadline: Some(std::time::Instant::now() + limit),
        },
    )
    .await
    .map_err(|e| format!("could not list {display}: {e}"))?;
    serde_json::from_slice(&out).map_err(|e| format!("could not list {display}: {e}"))
}

fn entries(
    cfg: &ResolvedConfig,
    found: &BTreeMap<String, Found>,
    store_error: Option<String>,
) -> Backups {
    let here = Writer::of(&cfg.db_path);
    let mut newest_first: Vec<(&String, &Found)> = found.iter().collect();
    newest_first.sort_unstable_by_key(|(name, _)| std::cmp::Reverse(order_key(name)));
    let backups = newest_first
        .into_iter()
        .enumerate()
        .map(|(i, (name, f))| {
            let sidecar = f
                .local
                .as_ref()
                .and_then(|(_, s)| s.clone())
                .or_else(|| f.store.as_ref().and_then(|(_, s)| s.clone()));
            let agree = match (&f.local, &f.store) {
                (Some((ls, lside)), Some((ss, sside))) => Some(
                    ls == ss
                        && match (lside, sside) {
                            (Some(l), Some(s)) => l.digest == s.digest,
                            _ => true,
                        },
                ),
                _ => None,
            };
            BackupEntry {
                index: i + 1,
                name: name.clone(),
                location: match (&f.local, &f.store) {
                    (Some(_), Some(_)) => "both",
                    (Some(_), None) => "local",
                    _ => "store",
                }
                .to_string(),
                size_bytes: f.local.as_ref().or(f.store.as_ref()).map_or(0, |(s, _)| *s),
                run_id: sidecar.as_ref().and_then(|s| s.run_id.clone()),
                rows: sidecar.as_ref().map(|s| s.rows),
                digest: sidecar.as_ref().map(|s| s.digest.clone()),
                agree,
                barca_version: sidecar.as_ref().map(|s| s.barca_version.clone()),
                db_format: sidecar.as_ref().map(|s| s.db_format),
                host: sidecar.as_ref().and_then(|s| s.host.clone()),
                project: sidecar.as_ref().and_then(|s| s.project.clone()),
                mine: sidecar
                    .as_ref()
                    .and_then(|s| here.is(s.host.as_deref(), s.project.as_deref())),
            }
        })
        .collect();
    Backups {
        backups,
        local_dir: cfg.local_backup_dir().to_string_lossy().into_owned(),
        store: cfg
            .backup_uri
            .as_deref()
            .map(crate::transfer::diagnostic_uri),
        store_error,
    }
}

// ─── Restore ─────────────────────────────────────────────────────────────────

/// What `barca state restore` did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Restored {
    /// The backup put in place.
    pub restored: String,
    /// `local` or `store`: which copy was used; `file` for `--from <path>`.
    pub from: String,
    /// That copy's path or store location (credentials redacted).
    pub source: String,
    /// The database it replaced, now at `<db>.pre-restore`; `null` when there was none.
    pub pre_restore: Option<String>,
    pub rows: Option<u64>,
    pub run_id: Option<String>,
    /// The host that took the backup (`null` for `--from <path>` or a backup from before 0.29).
    pub host: Option<String>,
    /// The project directory that took it (`null` likewise).
    pub project: Option<String>,
}

/// Put a backup in place of the local database, only when asked. `from` is a path to a
/// database file (one [`reset`] moved aside, say), an index into the listing (1 = newest) or a
/// backup name (`.db` optional); `None` is the newest.
///
/// The chosen copy is staged next to the database and validated
/// ([`crate::state_validate`]: whole file, integrity, barca's tables, a format this version
/// can use) before anything is touched. The local copy is tried first when there is one, then
/// the store's. Then, under the database's lock, the current database (its log folded in) moves
/// to `<db>.pre-restore`, replacing an older one, and the copy takes its place.
///
/// Refused while `barca serve` holds this environment ([`ServeLock`]), as an infra error.
/// No backup at all, an unknown `from`, and a copy that is not valid are usage errors.
pub async fn restore(
    python: &Path,
    cfg: &ResolvedConfig,
    from: Option<&str>,
) -> Result<Restored, BarcaError> {
    if let Some(holder) = ServeLock::holder(cfg) {
        return Err(BarcaError::Other(format!(
            "cannot restore while barca serve is using this environment ({holder}). Nothing was \
             changed.\nStop the server, run barca state restore again, then start the server."
        )));
    }
    if let Some(sel) = from
        && Path::new(sel).is_file()
    {
        return restore_file(cfg, Path::new(sel)).await;
    }
    let (found, store_error) = find(python, cfg).await;
    let listing = entries(cfg, &found, store_error.clone());
    if listing.backups.is_empty() {
        let mut msg = format!(
            "no backup to restore: there is none in {}",
            listing.local_dir
        );
        match (&listing.store, &store_error) {
            (Some(store), Some(why)) => {
                return Err(BarcaError::Other(format!(
                    "{msg}, and the store {store} could not be listed: {why}"
                )));
            }
            (Some(store), None) => msg.push_str(&format!(" or {store}")),
            (None, _) => msg.push_str(
                " and no store is configured ([remote] uri). Backups are taken only with a \
                 store (barca docs remote)",
            ),
        }
        return Err(BarcaError::Usage(msg));
    }
    let index = match from {
        None => newest_mine(cfg, &listing.backups),
        Some(sel) => select(&listing.backups, sel)?,
    };
    let chosen = &listing.backups[index];
    let copies = &found[&chosen.name];
    let dir = cfg.local_backup_dir();
    let staged = PathBuf::from(format!("{}.restore-{}", cfg.db_path, std::process::id()));
    let mut refusals = Vec::new();
    for (place, here) in [
        ("local", copies.local.is_some()),
        ("store", copies.store.is_some()),
    ] {
        if !here {
            continue;
        }
        let source = match place {
            "local" => dir.join(&chosen.name).to_string_lossy().into_owned(),
            _ => crate::transfer::diagnostic_uri(&format!(
                "{}/{}",
                cfg.backup_uri
                    .as_deref()
                    .unwrap_or_default()
                    .trim_end_matches('/'),
                chosen.name
            )),
        };
        let staged_ok = match place {
            "local" => std::fs::copy(dir.join(&chosen.name), &staged)
                .map(drop)
                .map_err(|e| format!("could not copy {source}: {e}")),
            _ => fetch(python, cfg, &chosen.name, &staged).await,
        };
        if let Err(why) = staged_ok {
            refusals.push(format!("{place}: {why}"));
            discard(&staged);
            continue;
        }
        match validate(&staged).await {
            Ok(()) => {
                let pre_restore = swap(&cfg.db_path, &staged).await;
                discard(&staged);
                let pre_restore = pre_restore?;
                return Ok(Restored {
                    restored: chosen.name.clone(),
                    from: place.to_string(),
                    source,
                    pre_restore,
                    rows: chosen.rows,
                    run_id: chosen.run_id.clone(),
                    host: chosen.host.clone(),
                    project: chosen.project.clone(),
                });
            }
            Err(Invalid::Machine(e)) => {
                discard(&staged);
                return Err(e);
            }
            Err(Invalid::Copy(invalid)) => {
                discard(&staged);
                if invalid.kind == crate::state_validate::InvalidKind::Compatibility {
                    return Err(BarcaError::Usage(format!(
                        "backup {} cannot be restored by this barca: {}. Nothing was changed.",
                        chosen.name, invalid.why
                    )));
                }
                refusals.push(format!("{place} copy: {}", invalid.why));
            }
        }
    }
    let next = listing
        .backups
        .get(index + 1)
        .map(|b| {
            format!(
                " Restore the next newest instead: barca state restore --from {}",
                b.index
            )
        })
        .unwrap_or_default();
    Err(BarcaError::Usage(format!(
        "backup {} is not a database barca can use ({}). Nothing was changed.{next}",
        chosen.name,
        refusals.join("; ")
    )))
}

/// The backup `restore` takes without `--from`: this writer's newest, when it has one, saying
/// so when other writers' backups are newer; otherwise the newest.
fn newest_mine(cfg: &ResolvedConfig, backups: &[BackupEntry]) -> usize {
    let here = Writer::of(&cfg.db_path);
    match backups.iter().position(|b| b.mine == Some(true)) {
        Some(0) | None => {
            if let Some(b) = backups.first()
                && b.mine == Some(false)
                && let Some(host) = &b.host
            {
                crate::errln!(
                    "[barca] restoring the newest backup, {}, which {} took (this machine has \
                     none of its own)",
                    b.name,
                    here.describe(host, b.project.as_deref())
                );
            }
            0
        }
        Some(i) => {
            let mut others: Vec<String> = backups[..i]
                .iter()
                .map(|b| match &b.host {
                    Some(host) => here.describe(host, b.project.as_deref()),
                    None => "an older barca".to_string(),
                })
                .collect();
            others.sort();
            others.dedup();
            crate::errln!(
                "[barca] restoring this machine's newest backup, {} (#{}); the {} newer {} \
                 taken by {}. barca state restore --from 1 restores the newest",
                backups[i].name,
                backups[i].index,
                i,
                if i == 1 { "one was" } else { "ones were" },
                others.join(", ")
            );
            i
        }
    }
}

/// The index of the backup `sel` names: a 1-based position in the listing, or a name with or
/// without `.db`.
fn select(backups: &[BackupEntry], sel: &str) -> Result<usize, BarcaError> {
    if let Ok(n) = sel.parse::<usize>() {
        if n >= 1 && n <= backups.len() {
            return Ok(n - 1);
        }
        return Err(BarcaError::Usage(format!(
            "--from {n}: there are {} backups (1 is the newest); barca state backups lists them",
            backups.len()
        )));
    }
    let want = if sel.ends_with(".db") {
        sel.to_string()
    } else {
        format!("{sel}.db")
    };
    backups.iter().position(|b| b.name == want).ok_or_else(|| {
        BarcaError::Usage(format!(
            "--from {sel}: no such backup; barca state backups lists them"
        ))
    })
}

async fn fetch(
    python: &Path,
    cfg: &ResolvedConfig,
    name: &str,
    staged: &Path,
) -> Result<(), String> {
    let store_dir = cfg.backup_uri.as_deref().unwrap_or_default();
    let uri = format!("{}/{name}", store_dir.trim_end_matches('/'));
    let mut cmd = state_helper(python, cfg);
    cmd.arg("fetch").arg(&uri).arg(staged);
    helper(cmd, Until::done()).await.map(drop).map_err(|e| {
        format!(
            "could not download {}: {e}",
            crate::transfer::diagnostic_uri(&uri)
        )
    })
}

enum Invalid {
    Copy(crate::state_validate::Invalid),
    Machine(BarcaError),
}

/// Validate the staged copy and fold whatever the migrations wrote into its main file, so it
/// is one self-contained database.
async fn validate(staged: &Path) -> Result<(), Invalid> {
    let path = staged.to_string_lossy();
    let (db, conn) = match crate::state_validate::open(&path, crate::state_validate::Pages::Check)
        .await
        .map_err(Invalid::Machine)?
    {
        Ok(valid) => valid,
        Err(invalid) => return Err(Invalid::Copy(invalid)),
    };
    crate::db::checkpoint(&conn)
        .await
        .map_err(Invalid::Machine)?;
    drop(conn);
    drop(db);
    if !crate::db::wal_is_clean(&path) {
        return Err(Invalid::Machine(BarcaError::Db(format!(
            "the write-ahead log of {path} is not empty after a checkpoint"
        ))));
    }
    crate::db::remove_sidecars(&path).map_err(Invalid::Machine)
}

fn discard(staged: &Path) {
    let _ = std::fs::remove_file(staged);
    let _ = crate::db::remove_sidecars(&staged.to_string_lossy());
}

/// Under the database's lock: move the current database (log folded in when it can be) to
/// `<db>.pre-restore`, then the staged copy into place. Returns the pre-restore path, when
/// there was a database to move.
async fn swap(db_path: &str, staged: &Path) -> Result<Option<String>, BarcaError> {
    let io = |what: String, e: std::io::Error| BarcaError::Db(format!("failed to {what}: {e}"));
    let _lock = crate::db::lock_db(db_path).await?;
    let pre = format!("{db_path}.pre-restore");
    let had = Path::new(db_path).exists();
    if had {
        // A database that cannot be opened is moved aside as it is, with its log beside it.
        let _ = crate::db::fold_log(db_path).await;
        for suffix in ["", "-wal", "-shm"] {
            let old = format!("{pre}{suffix}");
            match std::fs::remove_file(&old) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(format!("remove {old}"), e)),
            }
        }
        std::fs::rename(db_path, &pre).map_err(|e| io(format!("move {db_path} to {pre}"), e))?;
        if !crate::db::wal_is_clean(db_path) {
            std::fs::rename(format!("{db_path}-wal"), format!("{pre}-wal"))
                .map_err(|e| io(format!("move {db_path}-wal aside"), e))?;
        }
    }
    crate::db::remove_sidecars(db_path)?;
    std::fs::File::open(staged)
        .and_then(|f| f.sync_all())
        .map_err(|e| io(format!("sync {}", staged.display()), e))?;
    std::fs::rename(staged, db_path)
        .map_err(|e| io(format!("move {} to {db_path}", staged.display()), e))?;
    crate::gc::forget_survey();
    if let Some(parent) = Path::new(db_path).parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        std::fs::File::open(parent).and_then(|d| d.sync_all()).ok();
    }
    Ok(had.then_some(pre))
}

// ─── Reset ───────────────────────────────────────────────────────────────────

/// What `barca state reset` (and the server's `POST /state/reset`) did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "ResetResult"))]
pub struct Reset {
    /// The metadata database, now a new empty one.
    pub database: String,
    /// Where the database it replaced was moved: `<db>.reset-<UTC>` in the same directory.
    /// `null` when there was no database.
    pub moved_to: Option<String>,
    /// Every file moved: the database and, when they were there, its `-wal` and `-shm` beside
    /// it as `<moved_to>-wal` and `<moved_to>-shm`.
    pub moved: Vec<String>,
    /// The command that puts the moved database back (`barca state restore --from <moved_to>`,
    /// with `barca serve` stopped). `null` when nothing was moved.
    pub undo: Option<String>,
}

/// Start the metadata database over: under the database's lock, move it and its `-wal` and
/// `-shm` aside to `<db>.reset-<UTC>` in the same directory, and create an empty database in
/// its place. Nothing is deleted, artifacts are left alone, and a database that cannot be
/// opened (corrupt, a format this barca does not read) is moved as it is.
///
/// `admit` is called under the lock, before anything moves, with the runs the database says
/// are running in a process that is alive on this machine; an error from it refuses the reset
/// and nothing changes. The caller decides what counts as in flight (the CLI: any such run;
/// the server: those, and its own pending and running runs).
pub async fn reset<E: From<BarcaError>>(
    cfg: &ResolvedConfig,
    admit: impl FnOnce(&[String]) -> Result<(), E>,
) -> Result<Reset, E> {
    let db_path = cfg.db_path.as_str();
    if let Some(dir) = Path::new(db_path).parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| BarcaError::Db(format!("failed to create {}: {e}", dir.display())))?;
    }
    let _lock = crate::db::lock_db(db_path).await?;
    let had = Path::new(db_path).exists();
    let running = if had {
        match crate::db::connect_locked(db_path).await {
            Some((_db, conn)) => crate::db::runs_alive_here(&conn, db_path).await,
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    admit(&running)?;
    let mut moved = Vec::new();
    let mut moved_to = None;
    if had {
        // Fold the log in when it can be, so the moved file is one database on its own; a
        // database that cannot be opened moves with its log beside it.
        let _ = crate::db::fold_log(db_path).await;
        let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S%.3fZ");
        let aside = format!("{db_path}.reset-{stamp}");
        for suffix in ["", "-wal", "-shm"] {
            if Path::new(&format!("{aside}{suffix}")).exists() {
                return Err(BarcaError::Db(format!(
                    "{aside}{suffix} already exists; retry in a moment. Nothing was changed."
                ))
                .into());
            }
        }
        for suffix in ["", "-wal", "-shm"] {
            let from = format!("{db_path}{suffix}");
            let to = format!("{aside}{suffix}");
            match std::fs::rename(&from, &to) {
                Ok(()) => moved.push(to),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() => {}
                Err(e) => {
                    return Err(
                        BarcaError::Db(format!("failed to move {from} to {to}: {e}")).into(),
                    );
                }
            }
        }
        moved_to = Some(aside);
    }
    crate::db::init_locked(db_path).await?;
    crate::gc::forget_survey();
    if let Some(parent) = Path::new(db_path).parent() {
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        std::fs::File::open(parent).and_then(|d| d.sync_all()).ok();
    }
    let undo = moved_to.as_ref().map(|aside| {
        let mut cmd = format!(
            "barca state restore --from {}",
            crate::envelope::shell_quote(aside)
        );
        if cfg.env != crate::config::DEFAULT_ENV {
            cmd.push_str(&format!(
                " --env {}",
                crate::envelope::shell_quote(&cfg.env)
            ));
        }
        cmd
    });
    Ok(Reset {
        database: db_path.to_string(),
        moved_to,
        moved,
        undo,
    })
}

/// [`restore`] from a database file named by its path (as [`reset`] leaves one), with its
/// `-wal` beside it when there is one.
async fn restore_file(cfg: &ResolvedConfig, file: &Path) -> Result<Restored, BarcaError> {
    let source = std::fs::canonicalize(file)
        .unwrap_or_else(|_| file.to_path_buf())
        .to_string_lossy()
        .into_owned();
    if Path::new(&source) == Path::new(&cfg.db_path)
        || std::fs::canonicalize(&cfg.db_path).is_ok_and(|db| db == Path::new(&source))
    {
        return Err(BarcaError::Usage(format!(
            "--from {}: that is the current database. Nothing was changed.",
            file.display()
        )));
    }
    let staged = PathBuf::from(format!("{}.restore-{}", cfg.db_path, std::process::id()));
    let staged_str = staged.to_string_lossy().into_owned();
    let copied = std::fs::copy(file, &staged).map(drop).and_then(|()| {
        let wal = format!("{source}-wal");
        if !crate::db::wal_is_clean(&source) {
            std::fs::copy(&wal, format!("{staged_str}-wal")).map(drop)?;
        }
        Ok(())
    });
    if let Err(e) = copied {
        discard(&staged);
        return Err(BarcaError::Other(format!(
            "could not copy {source}: {e}. Nothing was changed."
        )));
    }
    match validate(&staged).await {
        Ok(()) => {
            let pre_restore = swap(&cfg.db_path, &staged).await;
            discard(&staged);
            Ok(Restored {
                restored: file
                    .file_name()
                    .map_or_else(|| source.clone(), |n| n.to_string_lossy().into_owned()),
                from: "file".to_string(),
                source,
                pre_restore: pre_restore?,
                rows: None,
                run_id: None,
                host: None,
                project: None,
            })
        }
        Err(Invalid::Machine(e)) => {
            discard(&staged);
            Err(e)
        }
        Err(Invalid::Copy(invalid)) => {
            discard(&staged);
            Err(BarcaError::Usage(format!(
                "{} is not a database barca can use: {}. Nothing was changed.",
                file.display(),
                invalid.why
            )))
        }
    }
}

// ─── barca serve's hold on an environment ────────────────────────────────────

/// Held by `barca serve` for its lifetime, so `barca state restore` can refuse to replace the
/// database under it: an exclusive lock on `serve.lock` beside the database, which says who
/// holds it. The lock goes with the process, however it ends.
pub struct ServeLock {
    _file: std::fs::File,
}

impl ServeLock {
    fn path(cfg: &ResolvedConfig) -> PathBuf {
        Path::new(&cfg.db_path)
            .parent()
            .map_or_else(|| PathBuf::from("serve.lock"), |d| d.join("serve.lock"))
    }

    /// Take the lock, naming this server (`what`, e.g. its address). `None` when it cannot be
    /// taken (another server holds it, or the file cannot be created): serving goes on.
    pub fn acquire(cfg: &ResolvedConfig, what: &str) -> Option<Self> {
        use std::io::{Seek, Write};
        let path = Self::path(cfg);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok()?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .ok()?;
        file.try_lock().ok()?;
        file.set_len(0).ok()?;
        file.rewind().ok()?;
        write!(
            file,
            "barca serve, pid {} on {}, {what}",
            std::process::id(),
            crate::db::local_host()
        )
        .ok()?;
        Some(Self { _file: file })
    }

    /// Who holds the lock, when someone does.
    pub fn holder(cfg: &ResolvedConfig) -> Option<String> {
        let path = Self::path(cfg);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .ok()?;
        match file.try_lock() {
            Ok(()) => None,
            Err(std::fs::TryLockError::WouldBlock) => Some(
                std::fs::read_to_string(&path)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "barca serve".to_string()),
            ),
            Err(std::fs::TryLockError::Error(_)) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_by_age_and_only_backup_names_count() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-14T18:02:11.123Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let name = name_at(at, "/a/.barca/metadata.db", "d1");
        assert!(name.starts_with("2026-10-14T18-02-11.123Z-"), "{name}");
        assert!(name.ends_with(".db") && name.len() == 36, "{name}");
        assert!(is_backup_name(&name));
        // The same millisecond from another directory, or another copy, is another name (#487).
        assert_ne!(name, name_at(at, "/b/.barca/metadata.db", "d1"));
        assert_ne!(name, name_at(at, "/a/.barca/metadata.db", "d2"));
        // 0.26.0's to-the-second names still count, and sort among the new ones by time.
        assert!(is_backup_name("2026-10-14T18-02-11Z.db"));
        let mut names = vec![
            "2026-10-14T18-02-10.999Z-00000000.db".to_string(),
            name.clone(),
            "2026-10-14T18-02-11Z.db".to_string(),
            "2026-10-14T18-02-12Z.db".to_string(),
        ];
        names.sort_unstable_by_key(|n| std::cmp::Reverse(order_key(n)));
        assert_eq!(
            names,
            [
                "2026-10-14T18-02-12Z.db",
                name.as_str(),
                "2026-10-14T18-02-11Z.db",
                "2026-10-14T18-02-10.999Z-00000000.db",
            ]
        );
        for other in [
            "metadata.db",
            "2026-10-14T18-02-11Z.json",
            "2026-10-14T18-02-11Z.db.tmp",
            "2026-10-14T18-02-11.123Z-1a2b3c4d.json",
            "2026-10-14T18-02-11.123Z-1A2B3C4D.db",
            "2026-10-14T18-02-11.123Z-1a2b3c.db",
            ".backup-1.tmp",
            "2026-10-14T18:02:11Z.db",
        ] {
            assert!(!is_backup_name(other), "{other}");
        }
    }

    fn cfg(dir: &Path) -> ResolvedConfig {
        let mut cfg = crate::config::resolve_in(None, dir).unwrap();
        cfg.db_path = dir
            .join(".barca/metadata.db")
            .to_string_lossy()
            .into_owned();
        cfg.backup_uri = Some(
            dir.join("store/default/state/backups")
                .to_string_lossy()
                .into_owned(),
        );
        // A plain-directory store must be there for a backup to go to it.
        std::fs::create_dir_all(dir.join("store")).unwrap();
        cfg
    }

    /// The real `barca._state` helper from this checkout, through the test's python3.
    fn python(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../python")
            .canonicalize()
            .unwrap();
        let path = dir.join("python");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nexport PYTHONPATH='{}'\nexec python3 \"$@\"\n",
                source.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn with_runs(cfg: &ResolvedConfig, n: usize) {
        std::fs::create_dir_all(Path::new(&cfg.db_path).parent().unwrap()).unwrap();
        crate::db::init_db(&cfg.db_path).await.unwrap();
        for i in 0..n {
            let id = format!("r{i}-{}", crate::db::generate_run_id());
            crate::db::create_run(&cfg.db_path, &id, "get", "[]", None, Some(1))
                .await
                .unwrap();
            crate::db::finish_run(&cfg.db_path, &id, "success", 0, 0, 0.1)
                .await
                .unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn routine_admission_precedes_copy_and_failed_uploads_consume_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        let python = python(dir.path());
        cfg.backup_interval =
            crate::config::BackupInterval(Some(std::time::Duration::from_secs(3600)));
        with_runs(&cfg, 1).await;
        // A missing local store is a real upload failure, with an unsent local copy.
        cfg.backup_uri = Some(
            dir.path()
                .join("missing/default/state/backups")
                .to_string_lossy()
                .into_owned(),
        );
        let first = take_requested(
            &python,
            &cfg,
            Some("first"),
            Request::Routine,
            Until::done(),
        )
        .await
        .unwrap();
        assert!(!first.skipped && first.name.is_some());
        assert!(matches!(first.upload, Some(Err(_))));
        // A copy attempt would fail on these invalid DB bytes. Not-due admission must
        // return without even opening/hashing them, and cannot claim unchanged coverage.
        std::fs::write(&cfg.db_path, b"not a database").unwrap();
        let skipped = take_requested(
            &python,
            &cfg,
            Some("later"),
            Request::Routine,
            Until::done(),
        )
        .await
        .unwrap();
        assert!(skipped.skipped && skipped.name.is_none() && skipped.upload.is_none());
        assert!(take(&python, &cfg, None, Until::done()).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_backup_is_written_both_places_and_skipped_when_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let python = python(dir.path());
        with_runs(&cfg, 2).await;
        let taken = take(&python, &cfg, Some("r-1"), Until::done())
            .await
            .unwrap();
        let name = taken.name.clone().expect("a copy");
        assert!(matches!(taken.upload, Some(Ok(_))), "{taken:?}");
        let local = cfg.local_backup_dir();
        let store = PathBuf::from(cfg.backup_uri.as_ref().unwrap());
        assert!(local.join(&name).is_file() && store.join(&name).is_file());
        assert!(!unsent_path(&local, &name).exists());
        let sidecar: Sidecar = serde_json::from_slice(
            &std::fs::read(store.join(sidecar_path(Path::new(""), &name))).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar.run_id.as_deref(), Some("r-1"));
        assert_eq!(sidecar.rows, 2);
        assert_eq!(sidecar.db_format, crate::db_schema::VERSION);
        assert_eq!(
            sidecar.digest,
            crate::db::file_sha256(&local.join(&name)).unwrap()
        );
        // Nothing changed: nothing is written or uploaded.
        let again = take(&python, &cfg, None, Until::done()).await.unwrap();
        assert_eq!(
            again,
            Taken {
                skipped: false,
                name: None,
                size_bytes: 0,
                upload: None
            }
        );
        assert_eq!(local_names(&local), vec![name]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn five_copies_are_kept_in_each_place() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let python = python(dir.path());
        with_runs(&cfg, 1).await;
        let local = cfg.local_backup_dir();
        let store = PathBuf::from(cfg.backup_uri.as_ref().unwrap());
        // Seven older copies in each place, named for seven earlier seconds.
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        for i in 0..7 {
            let name = format!("2020-01-01T00-00-0{i}Z.db");
            for d in [&local, &store] {
                std::fs::write(d.join(&name), b"old").unwrap();
                std::fs::write(sidecar_path(d, &name), b"{}").unwrap();
            }
        }
        std::fs::write(store.join("metadata.db"), b"not a backup").unwrap();
        let taken = take(&python, &cfg, None, Until::done()).await.unwrap();
        assert!(matches!(taken.upload, Some(Ok(_))), "{taken:?}");
        for d in [&local, &store] {
            let names = local_names(d);
            assert_eq!(names.len(), BACKUPS_KEPT, "{}", d.display());
            assert_eq!(names[0], taken.name.clone().unwrap());
            assert_eq!(names[4], "2020-01-01T00-00-03Z.db");
            assert!(!sidecar_path(d, "2020-01-01T00-00-02Z.db").exists());
        }
        assert!(
            store.join("metadata.db").exists(),
            "only backups are pruned"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn two_directories_backing_up_to_one_store_in_the_same_second_keep_both() {
        // #487: two project directories share one store location and back up at once.
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store/default/state/backups");
        let python = python(root.path());
        let mut cfgs = Vec::new();
        for project in ["a", "b"] {
            let dir = root.path().join(project);
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = cfg(&dir);
            cfg.backup_uri = Some(store.to_string_lossy().into_owned());
            with_runs(&cfg, 1).await;
            cfgs.push(cfg);
        }
        std::fs::create_dir_all(root.path().join("store")).unwrap();
        // At once: the two names fall in the same second (and usually the same millisecond).
        let (a, b) = tokio::join!(
            take(&python, &cfgs[0], Some("run-a"), Until::done()),
            take(&python, &cfgs[1], Some("run-b"), Until::done()),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert!(matches!(a.upload, Some(Ok(_))), "{a:?}");
        assert!(matches!(b.upload, Some(Ok(_))), "{b:?}");
        let (a, b) = (a.name.unwrap(), b.name.unwrap());
        assert_ne!(a, b);
        // Both are in the store, each with its own sidecar, and both directories list both.
        let mut stored = local_names(&store);
        stored.sort();
        let mut want = vec![a.clone(), b.clone()];
        want.sort();
        assert_eq!(stored, want);
        for (name, run) in [(&a, "run-a"), (&b, "run-b")] {
            let side = read_sidecar(&sidecar_path(&store, name)).unwrap();
            assert_eq!(side.run_id.as_deref(), Some(run));
        }
        let listed = list(&python, &cfgs[1]).await.unwrap();
        let names: Vec<&str> = listed.backups.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&a.as_str()) && names.contains(&b.as_str()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn two_projects_on_one_store_name_their_backups_and_restore_prefers_its_own() {
        // #553: each sidecar names the host and project directory that took it; a listing
        // marks this one's, finds the other's newer one, and restore without --from takes
        // this one's newest.
        let root = tempfile::tempdir().unwrap();
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        let store = root_path.join("store/default/state/backups");
        let python = python(&root_path);
        let mut cfgs = Vec::new();
        for project in ["a", "b"] {
            let dir = root_path.join(project);
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = cfg(&dir);
            cfg.backup_uri = Some(store.to_string_lossy().into_owned());
            with_runs(&cfg, 1).await;
            cfgs.push(cfg);
        }
        std::fs::create_dir_all(root_path.join("store")).unwrap();
        let (a, b) = (&cfgs[0], &cfgs[1]);
        let first_a = take(&python, a, Some("a1"), Until::done())
            .await
            .unwrap()
            .name
            .unwrap();
        let side = read_sidecar(&sidecar_path(&store, &first_a)).unwrap();
        assert_eq!(side.host.as_deref(), Some(crate::db::local_host().as_str()));
        assert_eq!(
            side.project.as_deref(),
            Some(root_path.join("a").to_str().unwrap())
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        with_runs(b, 1).await;
        let from_b = take(&python, b, Some("b1"), Until::done())
            .await
            .unwrap()
            .name
            .unwrap();
        let listed = list(&python, a).await.unwrap();
        let by_name = |n: &str| listed.backups.iter().find(|e| e.name == n).unwrap();
        assert_eq!(by_name(&first_a).mine, Some(true));
        assert_eq!(by_name(&from_b).mine, Some(false));
        assert_eq!(
            by_name(&from_b).project.as_deref(),
            Some(root_path.join("b").to_str().unwrap())
        );
        assert_eq!(
            interleaved(&listed.backups).map(|e| e.name.as_str()),
            Some(from_b.as_str())
        );
        // B has a backup of its own now, but A's is older than it: nothing interleaved for B.
        assert!(interleaved(&list(&python, b).await.unwrap().backups).is_none());
        let restored = restore(&python, a, None).await.unwrap();
        assert_eq!(restored.restored, first_a);
        assert_eq!(restored.run_id.as_deref(), Some("a1"));
        assert_eq!(
            restored.project.as_deref(),
            Some(root_path.join("a").to_str().unwrap())
        );
        // --from 1 is still the newest, whoever took it.
        let restored = restore(&python, a, Some("1")).await.unwrap();
        assert_eq!(restored.restored, from_b);
    }

    #[test]
    fn a_sidecar_from_before_hosts_were_recorded_still_reads() {
        let old: Sidecar = serde_json::from_str(
            r#"{"run_id":"r1","rows":2,"digest":"d","barca_version":"0.28.3","db_format":3}"#,
        )
        .unwrap();
        assert_eq!((old.host, old.project), (None, None));
        let here = Writer::of("/p/.barca/envs/dev/metadata.db");
        assert_eq!(here.project, "/p");
        assert_eq!(here.is(None, None), None);
        assert_eq!(here.is(Some(&here.host.clone()), Some("/p")), Some(true));
        assert_eq!(here.is(Some(&here.host.clone()), Some("/q")), Some(false));
        assert_eq!(here.is(Some("elsewhere"), Some("/p")), Some(false));
        assert_eq!(here.describe("elsewhere", Some("/p")), "elsewhere");
        assert_eq!(
            here.describe(&here.host.clone(), Some("/q")),
            format!("{}:/q", here.host)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_upload_keeps_the_local_copy_and_the_next_backup_sends_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        let python = python(dir.path());
        with_runs(&cfg, 1).await;
        // A store that is a file where its directory belongs cannot take the upload.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"").unwrap();
        let good = cfg
            .backup_uri
            .replace(blocked.join("backups").to_string_lossy().into_owned());
        let taken = take(&python, &cfg, None, Until::done()).await.unwrap();
        let name = taken.name.clone().unwrap();
        assert!(matches!(taken.upload, Some(Err(_))), "{taken:?}");
        assert!(unsent_path(&cfg.local_backup_dir(), &name).exists());
        // Nothing changed since, but the copy is still waiting for the store.
        cfg.backup_uri = good;
        let retried = take(&python, &cfg, None, Until::done()).await.unwrap();
        assert_eq!(retried.name, None);
        assert!(matches!(retried.upload, Some(Ok(_))), "{retried:?}");
        assert!(
            PathBuf::from(cfg.backup_uri.as_ref().unwrap())
                .join(&name)
                .is_file()
        );
        assert!(!unsent_path(&cfg.local_backup_dir(), &name).exists());
    }

    #[tokio::test]
    async fn a_backup_waiting_for_the_lock_stops_at_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        with_runs(&cfg, 1).await;
        let lock = std::fs::File::options()
            .write(true)
            .open(format!("{}.lock", cfg.db_path))
            .unwrap();
        lock.lock().unwrap();
        let result = take(
            Path::new("must-not-start"),
            &cfg,
            None,
            Until {
                cancel: None,
                deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(100)),
            },
        )
        .await;
        assert!(matches!(result, Err(BarcaError::Cancelled)));
        assert!(local_names(&cfg.local_backup_dir()).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_validates_moves_the_current_database_aside_and_refuses_bad_copies() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let python = python(dir.path());
        // No backup at all: a usage error.
        assert!(matches!(
            restore(&python, &cfg, None).await,
            Err(BarcaError::Usage(_))
        ));
        with_runs(&cfg, 1).await;
        let first = take(&python, &cfg, None, Until::done())
            .await
            .unwrap()
            .name
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        with_runs(&cfg, 2).await;
        let second = take(&python, &cfg, None, Until::done())
            .await
            .unwrap()
            .name
            .unwrap();
        assert_ne!(first, second);
        let listed = list(&python, &cfg).await.unwrap();
        assert_eq!(
            listed
                .backups
                .iter()
                .map(|b| (b.index, b.name.as_str(), b.location.as_str(), b.agree))
                .collect::<Vec<_>>(),
            vec![
                (1, second.as_str(), "both", Some(true)),
                (2, first.as_str(), "both", Some(true))
            ]
        );
        // By index: the older one, with one run.
        let restored = restore(&python, &cfg, Some("2")).await.unwrap();
        assert_eq!(
            (restored.restored.as_str(), restored.from.as_str()),
            (first.as_str(), "local")
        );
        assert_eq!(crate::db::count_runs(&cfg.db_path).await.unwrap(), 1);
        let pre = restored.pre_restore.unwrap();
        assert_eq!(crate::db::count_runs(&pre).await.unwrap(), 3);
        // By name, from the store when the local copy is gone; the newest is the default.
        std::fs::remove_file(cfg.local_backup_dir().join(&second)).unwrap();
        let restored = restore(&python, &cfg, None).await.unwrap();
        assert_eq!(
            (restored.restored.as_str(), restored.from.as_str()),
            (second.as_str(), "store")
        );
        assert_eq!(crate::db::count_runs(&cfg.db_path).await.unwrap(), 3);
        // A damaged copy is refused, naming the next newest; nothing is touched.
        let store = PathBuf::from(cfg.backup_uri.as_ref().unwrap());
        std::fs::write(store.join(&second), b"garbage").unwrap();
        let before = std::fs::read(&cfg.db_path).unwrap();
        let refused = restore(&python, &cfg, Some(second.trim_end_matches(".db"))).await;
        match refused {
            Err(BarcaError::Usage(msg)) => assert!(msg.contains("--from 2"), "{msg}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(&cfg.db_path).unwrap(), before);
        // An unknown name or index is a usage error.
        for sel in ["9", "nope"] {
            assert!(matches!(
                restore(&python, &cfg, Some(sel)).await,
                Err(BarcaError::Usage(_))
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reset_moves_the_database_aside_starts_empty_and_the_moved_file_restores() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let python = python(dir.path());
        with_runs(&cfg, 2).await;
        // A run in progress in a live process (this one) is handed to `admit`, which refuses.
        let live = crate::db::generate_run_id();
        crate::db::create_run(&cfg.db_path, &live, "get", "[]", None, Some(1))
            .await
            .unwrap();
        let before = std::fs::read(&cfg.db_path).unwrap();
        let refused = reset(&cfg, |running| {
            assert_eq!(running, std::slice::from_ref(&live));
            Err(BarcaError::Other("in flight".into()))
        })
        .await;
        assert!(matches!(refused, Err(BarcaError::Other(_))));
        assert_eq!(
            std::fs::read(&cfg.db_path).unwrap(),
            before,
            "refused: untouched"
        );
        crate::db::finish_run(&cfg.db_path, &live, "success", 0, 0, 0.1)
            .await
            .unwrap();

        let done = reset(&cfg, |running| {
            assert!(running.is_empty(), "{running:?}");
            Ok::<_, BarcaError>(())
        })
        .await
        .unwrap();
        let moved = done.moved_to.clone().unwrap();
        let name = Path::new(&moved).file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("metadata.db.reset-"), "{name}");
        assert_eq!(Path::new(&moved).parent(), Path::new(&cfg.db_path).parent());
        assert_eq!(done.moved[0], moved);
        assert_eq!(done.database, cfg.db_path);
        assert_eq!(
            done.undo.as_deref(),
            Some(format!("barca state restore --from {moved}").as_str())
        );
        assert_eq!(crate::db::count_runs(&cfg.db_path).await.unwrap(), 0);
        assert_eq!(crate::db::count_runs(&moved).await.unwrap(), 3);

        // The moved file restores, by its path.
        let restored = restore(&python, &cfg, Some(&moved)).await.unwrap();
        assert_eq!(restored.from, "file");
        assert_eq!(crate::db::count_runs(&cfg.db_path).await.unwrap(), 3);
        assert!(Path::new(&moved).exists(), "restore copies; the file stays");
        // A file that is not a database is a usage error, and nothing changes.
        let junk = dir.path().join("junk.db");
        std::fs::write(&junk, b"not a database").unwrap();
        assert!(matches!(
            restore(&python, &cfg, Some(&junk.to_string_lossy())).await,
            Err(BarcaError::Usage(_))
        ));
        assert_eq!(crate::db::count_runs(&cfg.db_path).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn reset_moves_a_database_that_cannot_be_opened_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        // No database yet: an empty one is created, nothing is moved.
        let none = reset(&cfg, |_| Ok::<_, BarcaError>(())).await.unwrap();
        assert_eq!((none.moved_to, none.undo), (None, None));
        assert!(none.moved.is_empty());
        std::fs::write(&cfg.db_path, b"garbage, not SQLite").unwrap();
        std::fs::write(format!("{}-wal", cfg.db_path), b"a log").unwrap();
        let done = reset(&cfg, |_| Ok::<_, BarcaError>(())).await.unwrap();
        let moved = done.moved_to.unwrap();
        assert_eq!(done.moved, vec![moved.clone(), format!("{moved}-wal")]);
        assert_eq!(std::fs::read(&moved).unwrap(), b"garbage, not SQLite");
        assert_eq!(std::fs::read(format!("{moved}-wal")).unwrap(), b"a log");
        assert_eq!(crate::db::count_runs(&cfg.db_path).await.unwrap(), 0);
    }

    #[test]
    fn restore_is_refused_while_a_server_holds_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        assert_eq!(ServeLock::holder(&cfg), None);
        let held = ServeLock::acquire(&cfg, "http://127.0.0.1:8274").unwrap();
        let holder = ServeLock::holder(&cfg).unwrap();
        assert!(holder.contains("http://127.0.0.1:8274"), "{holder}");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let refused = rt.block_on(restore(Path::new("unused"), &cfg, None));
        assert!(matches!(refused, Err(BarcaError::Other(ref m)) if m.contains("barca serve")));
        drop(held);
        // The lock goes with the last descriptor of its open file. A child that another test
        // thread is starting at this instant holds a copy until its exec closes it, so the
        // release is waited for, with a bound, not assumed to be immediate (#524).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ServeLock::holder(&cfg).is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "still held 10s after the server's lock was dropped"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
