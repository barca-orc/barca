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
    },
    /// A step was blocked because an upstream failed.
    Blocked { node_id: String, reason: String },
    /// Worker is requesting parallel dispatch of sub-tasks.
    /// Worker blocks on socket read until it receives ParallelResponse.
    Submit { items: Vec<SubmitItem> },
    /// Periodic heartbeat — worker is alive and working.
    Heartbeat,
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
    pub path: String,
    pub format: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
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
}

/// Outcome of a single `@sink` write. Sink failures never fail the parent
/// asset — they are reported here and surfaced in logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkOutcome {
    pub path: String,
    /// "ok" | "error"
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ParallelResult {
    Ok { result: serde_json::Value },
    Error { error: String },
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
    },
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
    },
    Error {
        id: u64,
        message: String,
        /// Attempts the helper made (retries plus the first try).
        #[serde(default = "one")]
        attempts: u32,
    },
}

fn one() -> u32 {
    1
}

// ─── Framing functions ───────────────────────────────────────────────────────

/// Encode a message as a length-prefixed JSON frame.
pub fn encode_message<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    let payload =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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

    // Safety: reject absurdly large messages (> 256MB)
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

/// Generate a unique socket path for a worker.
pub fn socket_path(run_id: &str, worker_id: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("barca-sockets");
    std::fs::create_dir_all(&dir).ok();
    dir.join(format!("{run_id}-{worker_id}.sock"))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_encode_decode_worker_message() {
        let msg = WorkerMessage::StepCompleted {
            node_id: "load_data".to_string(),
            artifact: ArtifactRef {
                path: "/tmp/artifacts/load_data.json".to_string(),
                format: "json".to_string(),
                size_bytes: 1024,
                elapsed_seconds: Some(0.42),
                cpu_seconds: None,
                max_rss_bytes: None,
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
                    result: serde_json::json!({"value": 42}),
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
                    ParallelResult::Ok { result } => {
                        assert_eq!(result, &serde_json::json!({"value": 42}));
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
                path: "abfss://cont@acct.dfs.core.windows.net/arts/n.parquet".to_string(),
                format: "parquet".to_string(),
                size_bytes: 42,
                elapsed_seconds: None,
                cpu_seconds: None,
                max_rss_bytes: None,
                sinks: vec![
                    SinkOutcome {
                        path: "exports/out.parquet".to_string(),
                        status: "ok".to_string(),
                        size_bytes: Some(42),
                        error: None,
                    },
                    SinkOutcome {
                        path: "s3://b/x.pkl".to_string(),
                        status: "error".to_string(),
                        size_bytes: None,
                        error: Some("ImportError: s3fs".to_string()),
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
            WorkerMessage::Submit { items } => {
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
            } => {
                assert_eq!(node_id, "transform");
                assert_eq!(error_type, "ValueError");
                assert_eq!(message, "invalid input");
                assert!(traceback.contains("Traceback"));
                assert!((elapsed - 1.5).abs() < f64::EPSILON);
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
                    path: "/tmp/step_1.json".to_string(),
                    format: "json".to_string(),
                    size_bytes: 512,
                    elapsed_seconds: None,
                    cpu_seconds: None,
                    max_rss_bytes: None,
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
                    result: serde_json::json!([1, 2, 3]),
                },
                ParallelResult::Error {
                    error: "division by zero".to_string(),
                },
                ParallelResult::Ok {
                    result: serde_json::json!(null),
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
                    ParallelResult::Ok { result } => {
                        assert_eq!(result, &serde_json::json!([1, 2, 3]));
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

                // Third: Ok with null
                match &results[2] {
                    ParallelResult::Ok { result } => {
                        assert!(result.is_null());
                    }
                    _ => panic!("expected Ok"),
                }
            }
            _ => panic!("expected ParallelResponse"),
        }
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
        };
        assert_eq!(
            serde_json::to_value(&get).unwrap(),
            serde_json::json!({"type": "get", "id": 8, "remote": "s3://b/x", "local": "/w/x"})
        );
        assert_eq!(
            serde_json::to_value(&TransferRequest::Shutdown).unwrap(),
            serde_json::json!({"type": "shutdown"})
        );
    }

    #[test]
    fn transfer_reply_parses_done_and_error() {
        let done: TransferReply =
            serde_json::from_str(r#"{"type":"done","id":3,"size_bytes":42}"#).unwrap();
        assert!(matches!(
            done,
            TransferReply::Done {
                id: 3,
                size_bytes: 42
            }
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
            } => {
                assert_eq!(id, 4);
                assert_eq!(message, "PermissionError: no");
                assert_eq!(attempts, 3);
            }
            _ => panic!("expected Error"),
        }
        // A reply without attempts (older helper) counts as one attempt.
        let bare: TransferReply =
            serde_json::from_str(r#"{"type":"error","id":5,"message":"x"}"#).unwrap();
        assert!(matches!(bare, TransferReply::Error { attempts: 1, .. }));
    }

    #[tokio::test]
    async fn async_frames_round_trip_and_eof_is_none() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msg = TransferRequest::Get {
            id: 1,
            remote: "r".into(),
            local: "l".into(),
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
}
