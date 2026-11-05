//! Unix socket message protocol for bidirectional communication between
//! the Rust executor (coordinator) and Python workers.
//!
//! ## Framing
//!
//! Length-prefixed JSON frames:
//! ```text
//! [4 bytes: u32 big-endian message length][JSON payload bytes]
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Write};

/// Negotiated worker/transfer wire version, independent of package and DB versions.
pub const PROTOCOL_VERSION: u32 = 5;

/// Negotiate before a helper can receive work. The caller bounds this together
/// with connection startup and owns reaping the child on every error path.
/// Returns the helper's hello: a worker's carries its `worker_id`, which pairs the
/// connection with the process the coordinator started.
pub(crate) async fn accept_hello<S>(stream: &mut S, role: &str) -> io::Result<serde_json::Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hello: Option<serde_json::Value> = read_frame(stream).await.map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid {role} protocol hello: {e}; reinstall barca in this Python environment"
            ),
        )
    })?;
    let version = hello
        .as_ref()
        .and_then(|v| v.get("protocol_version"))
        .and_then(|v| v.as_u64());
    let valid = hello.as_ref().is_some_and(|v| {
        v["type"] == "hello" && v["role"] == role && version == Some(u64::from(PROTOCOL_VERSION))
    });
    if !valid {
        let found = version.map_or_else(|| "unversioned".to_string(), |v| format!("v{v}"));
        let message = format!(
            "{role} protocol {found}, coordinator expects v{PROTOCOL_VERSION}; reinstall barca in the Python environment used for this run"
        );
        let _ = write_frame(stream, &serde_json::json!({"type": "protocol_error", "message": message, "protocol_version": PROTOCOL_VERSION})).await;
        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    }
    write_frame(stream, &serde_json::json!({"type": "hello", "role": "coordinator", "protocol_version": PROTOCOL_VERSION})).await?;
    Ok(hello.unwrap_or_default())
}

// ─── Worker → Coordinator ────────────────────────────────────────────────────

/// Messages sent from a Python worker to the Rust coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    /// A step completed successfully with an artifact.
    StepCompleted {
        node_id: String,
        artifact: Box<ArtifactRef>,
    },
    /// A step failed with a structured error.
    StepError {
        node_id: String,
        error_type: String,
        message: String,
        traceback: String,
        elapsed: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        import_seconds: Option<f64>,
        /// The worker is still sound and goes on with its batch: the step raised an
        /// exception that unwound cleanly. Absent (an older worker, or one that may not be
        /// sound: a failed import, `sys.exit()`, a step still running past its timeout), the
        /// coordinator stops the worker and starts a fresh one (#523).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        worker_continues: bool,
        /// Where the step failed (`dispatch::ErrorPhase`, by name). Absent from an older
        /// worker; a name this barca does not know is read as unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    /// A step was blocked because an upstream failed.
    Blocked { node_id: String, reason: String },
    /// A line of user stdout or stderr captured while a step was executing, and when the
    /// worker wrote it (`t`, seconds since the epoch). `stream` and `t` are absent from a
    /// worker older than stderr capture: stdout, received now.
    Log {
        node_id: String,
        line: String,
        #[serde(default)]
        stream: crate::LogStream,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        t: Option<f64>,
    },
    /// Worker is requesting parallel dispatch of sub-tasks.
    /// Worker blocks on socket read until it receives ParallelResponse.
    Submit {
        items: Vec<SubmitItem>,
        /// The worker reads each branch's result from its artifact, so the response names
        /// the artifacts ([`ParallelResult::Ok::artifact`]) and carries no values. A worker
        /// from an older barca Python package (0.19.0 or earlier) does not send this and gets the
        /// legacy request is refused; negotiated v3 workers always set this.
        #[serde(default)]
        artifact_results: bool,
    },
    /// Periodic heartbeat — worker is alive and working.
    Heartbeat,
    /// Library warnings the worker suppressed as repeats since its last report: first line of
    /// the warning → how many times. Sent ahead of a step result (`python/barca/_dedupe.py`).
    RepeatedWarnings { counts: HashMap<String, u64> },
}

/// A work item submitted via parallel().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitItem {
    /// Function reference: "module.py:function_name"
    pub fn_ref: String,
    /// Positional arguments (JSON values).
    pub args: Vec<serde_json::Value>,
    /// Keyword arguments (JSON values).
    pub kwargs: HashMap<String, serde_json::Value>,
}

/// Reference to a materialized artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Run-owned bytes pinned before the reusable artifact path is published.
    /// Internal transfer source; never an artifact identity or persisted path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_snapshot: Option<String>,
    pub path: String,
    pub format: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
    /// Slow module import duration, included in elapsed_seconds; absent at <= 1 second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_seconds: Option<f64>,
    /// CPU time the task consumed (`time.process_time` delta) — the truest
    /// measure of work; feeds the coordinator's cost estimator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_seconds: Option<f64>,
    /// Peak RSS of the worker process in bytes at task completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_bytes: Option<u64>,
    /// Outcomes of `@sink` writes performed alongside this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sinks: Vec<SinkOutcome>,
    /// SHA-256 of the serialized artifact bytes. Sent for sensors only: their output is folded
    /// into their consumers' run hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Explicit sensor freshness signal, independent of the optional value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_observation: Option<serde_json::Value>,
    /// When the step finished (Unix seconds, worker clock) and how long it took, for telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_seconds: Option<f64>,
    /// For a `parallel()` branch that returned a frame: which reader gives the caller the
    /// type the branch returned (`pandas`, `polars`, `polars_lazy`, `pyarrow`, `duckdb`).
    /// A step's consumer picks its reader from its own annotation; a branch's caller has
    /// none for the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_type: Option<String>,
    /// The batched call (RFC-0013) that computed this key, and how many keys it computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_keys: Option<u64>,
    /// A key of a batched call: writing its artifact and sinks. Included in nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_seconds: Option<f64>,
    /// What the batched call spent before its keys were written; on the first key it wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_phases: Option<BatchPhases>,
}

/// Where one batched call's time went before its keys were written (RFC-0013).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchPhases {
    pub import_seconds: f64,
    pub load_seconds: f64,
    pub compute_seconds: f64,
    pub split_seconds: f64,
}

/// Outcome of a single `@sink` write. A sink that fails fails its step (0.30.0), so a
/// current worker reports only successes; "error" is what workers before 0.30.0 sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkOutcome {
    pub path: String,
    /// "ok" | "error"
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A dataset sink: how many files the write created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
    /// A dataset sink's size warning (too many directories, or files that are too small).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

// ─── Coordinator → Worker ────────────────────────────────────────────────────

/// Messages sent from the Rust coordinator to a Python worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinatorMessage {
    /// Response to a Submit request — results for all submitted items.
    ParallelResponse { results: Vec<ParallelResult> },
    /// Request the worker to shut down gracefully.
    Cancel { reason: String },
}

/// Result of a single parallel branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParallelResult {
    /// The branch returned an artifact file reference. Values never ride this response.
    Ok {
        artifact: BranchArtifact,
    },
    Error {
        error: String,
    },
}

/// Where a branch's return value is: the artifact its worker wrote, like any step's output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchArtifact {
    pub path: String,
    /// `json`, `pickle` or `parquet`.
    pub format: String,
    /// See [`ArtifactRef::frame_type`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_type: Option<String>,
}

// ─── Coordinator ↔ transfer helper ───────────────────────────────────────────

/// Requests to the artifact transfer helper (`python -m barca._transfer`).
/// Many may be in flight at once; replies are matched by `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferRequest {
    /// Upload a local artifact file to the artifact store.
    Put {
        id: u64,
        local: String,
        remote: String,
    },
    /// Download an artifact from the store to a local path (atomic).
    Get {
        id: u64,
        remote: String,
        local: String,
        /// The hash recorded for the artifact. A local copy with this hash is
        /// kept; any other is replaced by the store's copy.
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
    /// Is the store holding `root` there and listable? Answered `Done` only when its bucket,
    /// container or root directory positively answers a listing. Creates nothing.
    Probe { id: u64, root: String },
    /// Is the object at `remote` in the store? `Done` with its size (and, for a store that is
    /// a plain directory, its SHA-256 in `sha256`); an `Error` with `missing` when it is not
    /// there. Moves no bytes (RFC-0009: `get` repairs an `unsynced` target).
    Stat { id: u64, remote: String },
    /// Finish in-flight transfers, then exit.
    Shutdown,
}

/// Replies from the transfer helper. `Error` is final — the helper has
/// already retried transient failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferReply {
    Done {
        id: u64,
        size_bytes: u64,
        /// SHA-256 of the local file.
        #[serde(default)]
        sha256: Option<String>,
        /// False when a `Get` left the local file as it was.
        #[serde(default = "yes")]
        fetched: bool,
        /// True when the store's copy does not have the hash a `Get` carried.
        #[serde(default)]
        mismatch: bool,
    },
    Error {
        id: u64,
        message: String,
        /// Attempts the helper made (retries plus the first try).
        #[serde(default = "one")]
        attempts: u32,
        /// True when the transfer failed because its source does not exist (for a `Get`, the
        /// object is not in the store), as opposed to a store that could not be reached.
        #[serde(default)]
        missing: bool,
    },
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

// ─── Framing functions ───────────────────────────────────────────────────────

/// Encode a message as a length-prefixed JSON frame.
pub fn encode_message<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    let payload =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if payload.len() > 256 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Write a message to a stream (socket/pipe).
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> io::Result<()> {
    let frame = encode_message(msg)?;
    writer.write_all(&frame)?;
    writer.flush()
}

/// Read a message from a stream (socket/pipe). Blocks until complete.
/// Returns None on EOF (clean disconnect).
pub fn read_message<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> io::Result<Option<T>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;

    // Reject oversized payloads before allocating.
    if len > 256 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes"),
        ));
    }

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    let msg = serde_json::from_slice(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(msg))
}

/// Async [`write_message`]: write one length-prefixed JSON frame.
pub async fn write_frame<W, T>(writer: &mut W, msg: &T) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: Serialize,
{
    use tokio::io::AsyncWriteExt;
    let frame = encode_message(msg)?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

/// Async [`read_message`]: read one length-prefixed JSON frame.
/// Returns `None` on a clean EOF at a frame boundary.
pub async fn read_frame<R, T>(reader: &mut R) -> io::Result<Option<T>>
where
    R: tokio::io::AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 256 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ─── Socket path helper ──────────────────────────────────────────────────────

/// The longest Unix socket path `bind` accepts, in bytes: `sun_path` is 104 bytes on macOS
/// and the BSDs and 108 on Linux, and holds the terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SOCKET_PATH_MAX: usize = 107;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SOCKET_PATH_MAX: usize = 103;

/// A unique socket path for one coordination socket of this process:
/// `<dir>/<pid>-<run_id>-<worker_id>.sock`, where `<dir>` is `barca-<uid>` (created 0700,
/// owned by this user) under `$TMPDIR`, or under `/tmp` when the `$TMPDIR` path would be
/// longer than [`SOCKET_PATH_MAX`] (#557). Errs, naming the path, the limit and the fix,
/// when neither directory gives a path that fits.
///
/// The pid lets [`sweep_dead_sockets`] remove the sockets of a coordinator that was killed.
pub fn socket_path(run_id: &str, worker_id: &str) -> Result<std::path::PathBuf, String> {
    let name = format!("{}-{run_id}-{worker_id}.sock", std::process::id());
    socket_path_in(&[std::env::temp_dir(), "/tmp".into()], &name)
}

fn socket_path_in(bases: &[std::path::PathBuf], name: &str) -> Result<std::path::PathBuf, String> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut first = None;
    for base in bases {
        let dir = base.join(format!("barca-{uid}"));
        let path = dir.join(name);
        first.get_or_insert_with(|| path.clone());
        if path.as_os_str().len() > SOCKET_PATH_MAX || !private_dir(&dir, uid) {
            continue;
        }
        sweep_dead_sockets(&dir);
        return Ok(path);
    }
    let path = first.unwrap_or_default();
    Err(format!(
        "no usable directory for barca's coordination socket: {} is {} bytes, over the \
         platform's {SOCKET_PATH_MAX}-byte limit for a socket path, and /tmp is not writable; \
         set TMPDIR to a shorter directory",
        path.display(),
        path.as_os_str().len()
    ))
}

/// Create `dir` 0700, or accept it when it exists as a real directory owned by `uid`
/// (tightening its mode to 0700). Anything else (another user's directory, a symlink) is
/// refused, so another user can neither read nor replace the sockets.
fn private_dir(dir: &std::path::Path, uid: u32) -> bool {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let _ = std::fs::DirBuilder::new().mode(0o700).create(dir);
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if !meta.is_dir() || meta.uid() != uid {
        return false;
    }
    if meta.mode() & 0o077 != 0
        && std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_err()
    {
        return false;
    }
    true
}

/// Remove the sockets (and their `.warnings` directories) of coordinators that are gone
/// (killed, out of memory): a run that ends any other way removes its own. A name this code
/// did not make is left alone.
fn sweep_dead_sockets(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.ends_with(".sock") || name.ends_with(".sock.warnings")) {
            continue;
        }
        let owner = name
            .split_once('-')
            .and_then(|(pid, _)| pid.parse::<i64>().ok());
        if owner
            .is_some_and(|pid| pid != i64::from(std::process::id()) && !crate::db::pid_alive(pid))
        {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).ok();
            } else {
                std::fs::remove_file(&path).ok();
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
