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
pub const PROTOCOL_VERSION: u32 = 3;

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
        artifact: ArtifactRef,
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
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A fixture directory under `/tmp`, not `$TMPDIR`: these tests need socket paths that fit
    /// [`SOCKET_PATH_MAX`], and the test runner's own `$TMPDIR` may be long (#571).
    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("bt")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn a_long_tmpdir_falls_back_to_a_short_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = short_tempdir();
        let long = root.path().join("d".repeat(150));
        std::fs::create_dir_all(&long).unwrap();
        let short = root.path().join("s");
        std::fs::create_dir_all(&short).unwrap();
        let path = socket_path_in(&[long.clone(), short.clone()], "1-run-main.sock").unwrap();
        assert!(path.starts_with(&short), "{}", path.display());
        assert!(path.as_os_str().len() <= SOCKET_PATH_MAX);
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // A socket can be bound there.
        std::os::unix::net::UnixListener::bind(&path).unwrap();
    }

    #[test]
    fn no_short_directory_is_an_error_naming_the_path_limit_and_fix() {
        let root = short_tempdir();
        let long = root.path().join("d".repeat(150));
        let err = socket_path_in(std::slice::from_ref(&long), "1-run-main.sock").unwrap_err();
        assert!(err.contains(&long.display().to_string()), "{err}");
        assert!(err.contains(&SOCKET_PATH_MAX.to_string()), "{err}");
        assert!(err.contains("set TMPDIR to a shorter directory"), "{err}");
    }

    #[test]
    fn a_dead_coordinators_sockets_are_swept() {
        let root = short_tempdir();
        // SAFETY: getuid cannot fail.
        let dir = root
            .path()
            .join(format!("barca-{}", unsafe { libc::getuid() }));
        std::fs::create_dir(&dir).unwrap();
        // pid 0x7fff_fff0 is above any real pid_max.
        let dead = dir.join("2147483632-run-main.sock");
        std::fs::write(&dead, b"").unwrap();
        std::fs::create_dir(dir.join("2147483632-run-main.sock.warnings")).unwrap();
        let live = dir.join(format!("{}-other-main.sock", std::process::id()));
        std::fs::write(&live, b"").unwrap();
        let foreign = dir.join("notes.txt");
        std::fs::write(&foreign, b"").unwrap();
        socket_path_in(&[root.path().to_path_buf()], "x.sock").unwrap();
        assert!(!dead.exists());
        assert!(!dir.join("2147483632-run-main.sock.warnings").exists());
        assert!(live.exists() && foreign.exists());
    }

    #[test]
    fn test_encode_decode_worker_message() {
        let msg = WorkerMessage::StepCompleted {
            node_id: "load_data".to_string(),
            artifact: ArtifactRef {
                upload_snapshot: None,
                import_seconds: None,
                path: "/tmp/artifacts/load_data.json".to_string(),
                format: "json".to_string(),
                size_bytes: 1024,
                elapsed_seconds: Some(0.42),
                cpu_seconds: None,
                max_rss_bytes: None,
                content_hash: None,
                finished_at: None,
                wall_seconds: None,
                frame_type: None,

                sinks: Vec::new(),
            },
        };

        let frame = encode_message(&msg).unwrap();
        let mut cursor = Cursor::new(frame);
        let decoded: Option<WorkerMessage> = read_message(&mut cursor).unwrap();
        let decoded = decoded.expect("should decode a message");

        match decoded {
            WorkerMessage::StepCompleted { node_id, artifact } => {
                assert_eq!(node_id, "load_data");
                assert_eq!(artifact.path, "/tmp/artifacts/load_data.json");
                assert_eq!(artifact.format, "json");
                assert_eq!(artifact.size_bytes, 1024);
                assert_eq!(artifact.elapsed_seconds, Some(0.42));
            }
            _ => panic!("expected StepCompleted"),
        }
    }

    #[test]
    fn test_encode_decode_coordinator_message() {
        let msg = CoordinatorMessage::ParallelResponse {
            results: vec![
                ParallelResult::Ok {
                    artifact: BranchArtifact {
                        path: "/a.json".into(),
                        format: "json".into(),
                        frame_type: None,
                    },
                },
                ParallelResult::Error {
                    error: "timeout".to_string(),
                },
            ],
        };

        let frame = encode_message(&msg).unwrap();
        let mut cursor = Cursor::new(frame);
        let decoded: Option<CoordinatorMessage> = read_message(&mut cursor).unwrap();
        let decoded = decoded.expect("should decode a message");

        match decoded {
            CoordinatorMessage::ParallelResponse { results } => {
                assert_eq!(results.len(), 2);
                match &results[0] {
                    ParallelResult::Ok { artifact, .. } => {
                        assert_eq!(artifact.path, "/a.json");
                    }
                    _ => panic!("expected Ok"),
                }
                match &results[1] {
                    ParallelResult::Error { error } => {
                        assert_eq!(error, "timeout");
                    }
                    _ => panic!("expected Error"),
                }
            }
            _ => panic!("expected ParallelResponse"),
        }
    }

    #[test]
    fn test_artifact_ref_with_sinks_round_trips() {
        let msg = WorkerMessage::StepCompleted {
            node_id: "n".to_string(),
            artifact: ArtifactRef {
                upload_snapshot: None,
                import_seconds: None,
                path: "abfss://cont@acct.dfs.core.windows.net/arts/n.parquet".to_string(),
                format: "parquet".to_string(),
                size_bytes: 42,
                elapsed_seconds: None,
                cpu_seconds: None,
                max_rss_bytes: None,
                content_hash: None,
                finished_at: None,
                wall_seconds: None,
                frame_type: None,

                sinks: vec![
                    SinkOutcome {
                        path: "exports/out.parquet".to_string(),
                        status: "ok".to_string(),
                        size_bytes: Some(42),
                        error: None,
                        files: None,
                        warning: None,
                    },
                    SinkOutcome {
                        path: "s3://b/x.pkl".to_string(),
                        status: "error".to_string(),
                        size_bytes: None,
                        error: Some("ImportError: s3fs".to_string()),
                        files: None,
                        warning: None,
                    },
                ],
            },
        };
        let frame = encode_message(&msg).unwrap();
        let mut cursor = Cursor::new(frame);
        let decoded: WorkerMessage = read_message(&mut cursor).unwrap().unwrap();
        match decoded {
            WorkerMessage::StepCompleted { artifact, .. } => {
                assert_eq!(artifact.sinks.len(), 2);
                assert_eq!(artifact.sinks[0].status, "ok");
                assert_eq!(
                    artifact.sinks[1].error.as_deref(),
                    Some("ImportError: s3fs")
                );
            }
            _ => panic!("expected StepCompleted"),
        }
    }

    #[test]
    fn test_repeated_warnings_decodes_what_the_worker_sends() {
        // The exact frame `python/barca/_runtime.py` sends ahead of a step result.
        let frame = r#"{"type":"repeated_warnings","counts":{"Could not parse .netrc file":214}}"#;
        match serde_json::from_str::<WorkerMessage>(frame).unwrap() {
            WorkerMessage::RepeatedWarnings { counts } => {
                assert_eq!(counts["Could not parse .netrc file"], 214);
            }
            _ => panic!("expected RepeatedWarnings"),
        }
    }

    #[test]
    fn test_artifact_ref_without_sinks_still_decodes() {
        // Legacy workers don't send a `sinks` key — it must default to empty.
        let legacy = r#"{"type":"step_completed","node_id":"n","artifact":{"path":"/a/n.json","format":"json","size_bytes":7}}"#;
        let decoded: WorkerMessage = serde_json::from_str(legacy).unwrap();
        match decoded {
            WorkerMessage::StepCompleted { artifact, .. } => {
                assert!(artifact.sinks.is_empty());
                assert_eq!(artifact.size_bytes, 7);
            }
            _ => panic!("expected StepCompleted"),
        }
    }

    #[test]
    fn test_encode_decode_submit() {
        let msg = WorkerMessage::Submit {
            artifact_results: true,
            items: vec![
                SubmitItem {
                    fn_ref: "tasks.py:process_chunk".to_string(),
                    args: vec![serde_json::json!(1), serde_json::json!("hello")],
                    kwargs: HashMap::from([
                        ("timeout".to_string(), serde_json::json!(30)),
                        ("retries".to_string(), serde_json::json!(3)),
                    ]),
                },
                SubmitItem {
                    fn_ref: "tasks.py:process_chunk".to_string(),
                    args: vec![serde_json::json!(2)],
                    kwargs: HashMap::new(),
                },
            ],
        };

        let frame = encode_message(&msg).unwrap();
        let mut cursor = Cursor::new(frame);
        let decoded: Option<WorkerMessage> = read_message(&mut cursor).unwrap();
        let decoded = decoded.expect("should decode a message");

        match decoded {
            WorkerMessage::Submit {
                items,
                artifact_results,
            } => {
                assert!(artifact_results);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].fn_ref, "tasks.py:process_chunk");
                assert_eq!(items[0].args.len(), 2);
                assert_eq!(items[0].kwargs.get("timeout"), Some(&serde_json::json!(30)));
                assert_eq!(items[1].args.len(), 1);
                assert!(items[1].kwargs.is_empty());
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn test_read_write_stream() {
        let msg = WorkerMessage::StepError {
            node_id: "transform".to_string(),
            error_type: "ValueError".to_string(),
            message: "invalid input".to_string(),
            traceback: "Traceback (most recent call last):\n  ...".to_string(),
            elapsed: 1.5,
            import_seconds: Some(1.1),
            worker_continues: true,
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &msg).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded: Option<WorkerMessage> = read_message(&mut cursor).unwrap();
        let decoded = decoded.expect("should decode a message");

        match decoded {
            WorkerMessage::StepError {
                node_id,
                error_type,
                message,
                traceback,
                elapsed,
                import_seconds,
                worker_continues,
            } => {
                assert!(worker_continues);
                assert_eq!(node_id, "transform");
                assert_eq!(error_type, "ValueError");
                assert_eq!(message, "invalid input");
                assert!(traceback.contains("Traceback"));
                assert!((elapsed - 1.5).abs() < f64::EPSILON);
                assert_eq!(import_seconds, Some(1.1));
            }
            _ => panic!("expected StepError"),
        }
    }

    #[test]
    fn test_read_eof_returns_none() {
        let empty: &[u8] = &[];
        let mut cursor = Cursor::new(empty);
        let result: io::Result<Option<WorkerMessage>> = read_message(&mut cursor);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_reject_oversized_message() {
        // Craft a frame header claiming > 256MB
        let huge_len: u32 = 256 * 1024 * 1024 + 1;
        let mut frame = Vec::new();
        frame.extend_from_slice(&huge_len.to_be_bytes());
        // Add a few bytes so read_exact for the length succeeds
        frame.extend_from_slice(&[0u8; 16]);

        let mut cursor = Cursor::new(frame);
        let result: io::Result<Option<WorkerMessage>> = read_message(&mut cursor);
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("message too large"));
    }

    #[test]
    fn test_multiple_messages_on_stream() {
        let messages = vec![
            WorkerMessage::Heartbeat,
            WorkerMessage::StepCompleted {
                node_id: "step_1".to_string(),
                artifact: ArtifactRef {
                    upload_snapshot: None,
                    import_seconds: None,
                    path: "/tmp/step_1.json".to_string(),
                    format: "json".to_string(),
                    size_bytes: 512,
                    elapsed_seconds: None,
                    cpu_seconds: None,
                    max_rss_bytes: None,
                    content_hash: None,
                    finished_at: None,
                    wall_seconds: None,
                    frame_type: None,

                    sinks: Vec::new(),
                },
            },
            WorkerMessage::Blocked {
                node_id: "step_2".to_string(),
                reason: "upstream step_1 failed".to_string(),
            },
        ];

        let mut buffer = Vec::new();
        for msg in &messages {
            write_message(&mut buffer, msg).unwrap();
        }

        let mut cursor = Cursor::new(buffer);

        // Read first: Heartbeat
        let msg1: WorkerMessage = read_message(&mut cursor).unwrap().unwrap();
        assert!(matches!(msg1, WorkerMessage::Heartbeat));

        // Read second: StepCompleted
        let msg2: WorkerMessage = read_message(&mut cursor).unwrap().unwrap();
        match msg2 {
            WorkerMessage::StepCompleted { node_id, .. } => assert_eq!(node_id, "step_1"),
            _ => panic!("expected StepCompleted"),
        }

        // Read third: Blocked
        let msg3: WorkerMessage = read_message(&mut cursor).unwrap().unwrap();
        match msg3 {
            WorkerMessage::Blocked { node_id, reason } => {
                assert_eq!(node_id, "step_2");
                assert_eq!(reason, "upstream step_1 failed");
            }
            _ => panic!("expected Blocked"),
        }

        // Read fourth: EOF
        let msg4: Option<WorkerMessage> = read_message(&mut cursor).unwrap();
        assert!(msg4.is_none());
    }

    #[test]
    fn test_log_message_roundtrip() {
        // The wire shape the Python worker sends: {"type":"log","node_id":..,"line":..}
        // Include a non-ASCII char (UTF-8) to confirm framing handles it.
        let raw = r#"{"type":"log","node_id":"a.py:load","line":"loading…"}"#.as_bytes();
        let mut framed = Vec::new();
        framed.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        framed.extend_from_slice(raw);

        let mut cursor = Cursor::new(framed);
        let decoded: WorkerMessage = read_message(&mut cursor).unwrap().unwrap();
        match decoded {
            WorkerMessage::Log {
                node_id,
                line,
                stream,
                t,
            } => {
                assert_eq!(t, None);
                assert_eq!(node_id, "a.py:load");
                assert_eq!(line, "loading…");
                assert_eq!(stream, crate::LogStream::Stdout);
            }
            _ => panic!("expected Log"),
        }

        // And it re-encodes to the same tagged shape.
        let msg = WorkerMessage::Log {
            node_id: "a.py:load".to_string(),
            line: "loading…".to_string(),
            stream: crate::LogStream::Stderr,
            t: Some(1.5),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"log""#));
        assert!(json.contains(r#""line":"loading…""#));
        assert!(json.contains(r#""stream":"stderr""#));
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let msg = WorkerMessage::Heartbeat;
        let frame = encode_message(&msg).unwrap();

        // Heartbeat should be very small (just the tag)
        // 4 bytes length prefix + a small JSON payload
        assert!(frame.len() < 50, "heartbeat frame should be compact");

        let mut cursor = Cursor::new(frame);
        let decoded: Option<WorkerMessage> = read_message(&mut cursor).unwrap();
        assert!(matches!(decoded, Some(WorkerMessage::Heartbeat)));
    }

    #[test]
    fn test_parallel_result_ok_and_error() {
        let msg = CoordinatorMessage::ParallelResponse {
            results: vec![
                ParallelResult::Ok {
                    artifact: BranchArtifact {
                        path: "/a.json".into(),
                        format: "json".into(),
                        frame_type: None,
                    },
                },
                ParallelResult::Error {
                    error: "division by zero".to_string(),
                },
                ParallelResult::Ok {
                    artifact: BranchArtifact {
                        path: "/artifacts/work_branch_3.pkl".to_string(),
                        format: "pickle".to_string(),
                        frame_type: None,
                    },
                },
            ],
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &msg).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded: CoordinatorMessage = read_message(&mut cursor).unwrap().unwrap();

        match decoded {
            CoordinatorMessage::ParallelResponse { results } => {
                assert_eq!(results.len(), 3);

                // First: Ok with array
                match &results[0] {
                    ParallelResult::Ok { artifact, .. } => {
                        assert_eq!(artifact.path, "/a.json");
                    }
                    _ => panic!("expected Ok"),
                }

                // Second: Error
                match &results[1] {
                    ParallelResult::Error { error } => {
                        assert_eq!(error, "division by zero");
                    }
                    _ => panic!("expected Error"),
                }

                // Third: Ok, by artifact
                match &results[2] {
                    ParallelResult::Ok { artifact } => {
                        assert_eq!(artifact.format, "pickle");
                    }
                    _ => panic!("expected Ok"),
                }
            }
            _ => panic!("expected ParallelResponse"),
        }
    }

    // python/barca/_runtime.py and python/barca/__init__.py send and parse these exact
    // shapes; pin them.
    #[test]
    fn parallel_wire_shapes() {
        // Missing artifact capability is decoded only so the coordinator can refuse it.
        let old: WorkerMessage = serde_json::from_str(r#"{"type":"submit","items":[]}"#).unwrap();
        assert!(matches!(
            old,
            WorkerMessage::Submit {
                artifact_results: false,
                ..
            }
        ));
        let new: WorkerMessage =
            serde_json::from_str(r#"{"type":"submit","items":[],"artifact_results":true}"#)
                .unwrap();
        assert!(matches!(
            new,
            WorkerMessage::Submit {
                artifact_results: true,
                ..
            }
        ));

        let by_artifact = ParallelResult::Ok {
            artifact: BranchArtifact {
                path: "/a/b.parquet".to_string(),
                format: "parquet".to_string(),
                frame_type: Some("polars".to_string()),
            },
        };
        assert_eq!(
            serde_json::to_value(&by_artifact).unwrap(),
            serde_json::json!({
                "status": "ok",
                "artifact": {"path": "/a/b.parquet", "format": "parquet", "frame_type": "polars"},
            })
        );
        for inline in [
            serde_json::json!({"status":"ok","result":1}),
            serde_json::json!({"status":"ok","artifact":{"path":"/a.json","format":"json","json":"1"}}),
        ] {
            assert!(serde_json::from_value::<ParallelResult>(inline).is_err());
        }

        // The frame type a branch's worker reports survives the coordinator.
        let reported: ArtifactRef = serde_json::from_str(
            r#"{"path":"p","format":"parquet","size_bytes":1,"frame_type":"pandas"}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&reported).unwrap()["frame_type"],
            "pandas"
        );
    }

    // ─── Transfer helper wire format ────────────────────────────────────────
    // python/barca/_transfer.py parses these exact shapes; pin them.

    #[test]
    fn transfer_request_wire_shapes() {
        let put = TransferRequest::Put {
            id: 7,
            local: "/w/.barca/artifacts/a/h.json".into(),
            remote: "s3://b/p/default/artifacts/a/h.json".into(),
        };
        assert_eq!(
            serde_json::to_value(&put).unwrap(),
            serde_json::json!({"type": "put", "id": 7,
                "local": "/w/.barca/artifacts/a/h.json",
                "remote": "s3://b/p/default/artifacts/a/h.json"})
        );
        let get = TransferRequest::Get {
            id: 8,
            remote: "s3://b/x".into(),
            local: "/w/x".into(),
            sha256: None,
        };
        assert_eq!(
            serde_json::to_value(&get).unwrap(),
            serde_json::json!({"type": "get", "id": 8, "remote": "s3://b/x", "local": "/w/x"})
        );
        let checked = TransferRequest::Get {
            id: 9,
            remote: "s3://b/x".into(),
            local: "/w/x".into(),
            sha256: Some("ab12".into()),
        };
        assert_eq!(
            serde_json::to_value(&checked).unwrap()["sha256"],
            serde_json::json!("ab12")
        );
        assert_eq!(
            serde_json::to_value(&TransferRequest::Shutdown).unwrap(),
            serde_json::json!({"type": "shutdown"})
        );
    }

    #[test]
    fn transfer_reply_parses_done_and_error() {
        let timed: WorkerMessage = serde_json::from_str(
            r#"{"type":"step_completed","node_id":"f:a","artifact":{"path":"p","format":"json","size_bytes":1,"finished_at":1700000000.25,"wall_seconds":0.5}}"#,
        )
        .unwrap();
        // The coordinator re-serialises the artifact; the worker's clock has to survive that.
        let WorkerMessage::StepCompleted { artifact, .. } = timed else {
            panic!("expected a result message");
        };
        let round_trip = serde_json::to_value(&artifact).unwrap();
        assert_eq!(round_trip["finished_at"], 1700000000.25);
        assert_eq!(round_trip["wall_seconds"], 0.5);

        let done: TransferReply =
            serde_json::from_str(r#"{"type":"done","id":3,"size_bytes":42}"#).unwrap();
        assert!(matches!(
            done,
            TransferReply::Done {
                id: 3,
                size_bytes: 42,
                sha256: None,
                fetched: true,
                mismatch: false,
            }
        ));
        let hashed: TransferReply = serde_json::from_str(
            r#"{"type":"done","id":5,"size_bytes":1,"sha256":"ab12","fetched":false,"mismatch":true}"#,
        )
        .unwrap();
        assert!(matches!(
            hashed,
            TransferReply::Done { sha256: Some(h), fetched: false, mismatch: true, .. } if h == "ab12"
        ));
        let err: TransferReply = serde_json::from_str(
            r#"{"type":"error","id":4,"message":"PermissionError: no","attempts":3}"#,
        )
        .unwrap();
        match err {
            TransferReply::Error {
                id,
                message,
                attempts,
                missing,
            } => {
                assert_eq!(id, 4);
                assert_eq!(message, "PermissionError: no");
                assert_eq!(attempts, 3);
                // Not said to be missing: the object may well be there.
                assert!(!missing);
            }
            _ => panic!("expected Error"),
        }
        let probe = serde_json::to_value(TransferRequest::Probe {
            id: 9,
            root: "s3://b/p/default/artifacts".to_string(),
        })
        .unwrap();
        assert_eq!(
            probe,
            serde_json::json!({"type": "probe", "id": 9, "root": "s3://b/p/default/artifacts"})
        );
        let stat = serde_json::to_value(TransferRequest::Stat {
            id: 10,
            remote: "s3://b/p/default/artifacts/n/h.json".to_string(),
        })
        .unwrap();
        assert_eq!(
            stat,
            serde_json::json!({"type": "stat", "id": 10, "remote": "s3://b/p/default/artifacts/n/h.json"})
        );
        // A reply without attempts (older helper) counts as one attempt.
        let bare: TransferReply =
            serde_json::from_str(r#"{"type":"error","id":5,"message":"x"}"#).unwrap();
        assert!(matches!(bare, TransferReply::Error { attempts: 1, .. }));
        let gone: TransferReply = serde_json::from_str(
            r#"{"type":"error","id":6,"message":"FileNotFoundError: x","attempts":1,"missing":true}"#,
        )
        .unwrap();
        assert!(matches!(gone, TransferReply::Error { missing: true, .. }));
    }

    #[tokio::test]
    async fn async_frames_round_trip_and_eof_is_none() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msg = TransferRequest::Get {
            id: 1,
            remote: "r".into(),
            local: "l".into(),
            sha256: None,
        };
        write_frame(&mut a, &msg).await.unwrap();
        drop(a);
        let got: serde_json::Value = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!(got["type"], "get");
        let eof: Option<serde_json::Value> = read_frame(&mut b).await.unwrap();
        assert!(eof.is_none());
    }

    #[tokio::test]
    async fn async_frame_interops_with_sync_encoding() {
        // A frame produced by the sync encoder (what the Python side mirrors)
        // must decode with the async reader.
        let frame = encode_message(&TransferRequest::Shutdown).unwrap();
        let (mut a, mut b) = tokio::io::duplex(64);
        tokio::io::AsyncWriteExt::write_all(&mut a, &frame)
            .await
            .unwrap();
        let got: serde_json::Value = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!(got, serde_json::json!({"type": "shutdown"}));
    }

    #[tokio::test]
    async fn helper_hello_requires_matching_version_and_role() {
        for role in ["worker", "transfer"] {
            for version in [0, 2, PROTOCOL_VERSION, PROTOCOL_VERSION + 1] {
                let (mut client, mut server) = tokio::io::duplex(4096);
                write_frame(&mut client, &serde_json::json!({"type": "hello", "role": role, "protocol_version": version})).await.unwrap();
                let accepted = accept_hello(&mut server, role).await;
                let response: serde_json::Value = read_frame(&mut client).await.unwrap().unwrap();
                if version == PROTOCOL_VERSION {
                    assert!(accepted.is_ok());
                    assert_eq!(
                        response,
                        serde_json::json!({"type":"hello", "role":"coordinator", "protocol_version":PROTOCOL_VERSION})
                    );
                } else {
                    let error = accepted.unwrap_err().to_string();
                    assert!(error.contains(&format!("protocol v{version}")), "{error}");
                    assert!(error.contains("reinstall barca"), "{error}");
                    assert_eq!(response["type"], "protocol_error");
                }
            }
        }
        for hello in [
            serde_json::json!({"type":"heartbeat"}),
            serde_json::json!({"type":"hello", "role":"transfer", "protocol_version":PROTOCOL_VERSION}),
            serde_json::json!({"type":"hello", "role":"worker", "protocol_version":"3"}),
        ] {
            let (mut client, mut server) = tokio::io::duplex(4096);
            write_frame(&mut client, &hello).await.unwrap();
            assert!(
                accept_hello(&mut server, "worker")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("reinstall barca")
            );
        }
    }
}
