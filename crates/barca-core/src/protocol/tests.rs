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
        artifact: Box::new(ArtifactRef {
            upload_snapshot: None,
            import_seconds: None,
            path: "/tmp/artifacts/load_data.json".to_string(),
            format: "json".to_string(),
            size_bytes: 1024,
            elapsed_seconds: Some(0.42),
            cpu_seconds: None,
            max_rss_bytes: None,
            content_hash: None,
            sensor_changed: None,
            source_observation: None,
            finished_at: None,
            wall_seconds: None,
            frame_type: None,
            batch: None,
            batch_keys: None,
            write_seconds: None,
            batch_phases: None,

            sinks: Vec::new(),
        }),
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
        artifact: Box::new(ArtifactRef {
            upload_snapshot: None,
            import_seconds: None,
            path: "abfss://cont@acct.dfs.core.windows.net/arts/n.parquet".to_string(),
            format: "parquet".to_string(),
            size_bytes: 42,
            elapsed_seconds: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            content_hash: None,
            sensor_changed: None,
            source_observation: None,
            finished_at: None,
            wall_seconds: None,
            frame_type: None,
            batch: None,
            batch_keys: None,
            write_seconds: None,
            batch_phases: None,

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
        }),
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
        phase: Some("load_inputs".to_string()),
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
            phase,
        } => {
            assert!(worker_continues);
            assert_eq!(phase.as_deref(), Some("load_inputs"));
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
            artifact: Box::new(ArtifactRef {
                upload_snapshot: None,
                import_seconds: None,
                path: "/tmp/step_1.json".to_string(),
                format: "json".to_string(),
                size_bytes: 512,
                elapsed_seconds: None,
                cpu_seconds: None,
                max_rss_bytes: None,
                content_hash: None,
                sensor_changed: None,
                source_observation: None,
                finished_at: None,
                wall_seconds: None,
                frame_type: None,
                batch: None,
                batch_keys: None,
                write_seconds: None,
                batch_phases: None,

                sinks: Vec::new(),
            }),
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
        serde_json::from_str(r#"{"type":"submit","items":[],"artifact_results":true}"#).unwrap();
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
async fn pre_batch_helpers_are_refused_before_receiving_work() {
    for role in ["worker", "transfer"] {
        let (mut client, mut server) = tokio::io::duplex(4096);
        write_frame(
            &mut client,
            &serde_json::json!({
                "type": "hello", "role": role, "protocol_version": 3
            }),
        )
        .await
        .unwrap();
        assert!(
            accept_hello(&mut server, role).await.is_err(),
            "protocol 3 helpers cannot execute the new one-call batch semantics"
        );
        let response: serde_json::Value = read_frame(&mut client).await.unwrap().unwrap();
        assert_eq!(response["type"], "protocol_error");
        assert!(
            response["message"]
                .as_str()
                .unwrap()
                .contains("reinstall barca")
        );
    }
}

#[tokio::test]
async fn helper_hello_requires_matching_version_and_role() {
    for role in ["worker", "transfer"] {
        for version in [0, 2, 3, 4, PROTOCOL_VERSION, PROTOCOL_VERSION + 1] {
            let (mut client, mut server) = tokio::io::duplex(4096);
            write_frame(
                &mut client,
                &serde_json::json!({"type": "hello", "role": role, "protocol_version": version}),
            )
            .await
            .unwrap();
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
