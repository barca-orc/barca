//! Minimal stress test for the Unix socket accept pattern.
//! Tests the exact same construct used in async_parallel_dispatch:
//! one shared listener, N workers connecting simultaneously.
//!
//! Run with: cargo test --test socket_stress -- --nocapture

use std::process::Command;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;

/// Minimal "worker" that connects to a socket, sends one message, disconnects.
/// Written as a Python one-liner to avoid any barca import overhead.
fn spawn_connector(socket_path: &str, worker_id: usize) -> std::process::Child {
    let mut cmd = Command::new("python3");
    cmd.args([
            "-c",
            &format!(
                r#"
import socket, struct, json, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("{socket_path}")
msg = json.dumps({{"type": "step_completed", "node_id": "w{worker_id}", "artifact": {{"path": "", "format": "json", "size_bytes": 0}}}}).encode()
s.sendall(struct.pack(">I", len(msg)) + msg)
s.close()
"#
            ),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    barca_core::helper_proc::spawn_std(&mut cmd).unwrap()
}

#[tokio::test]
async fn accept_4_workers() {
    let path = format!("/tmp/barca-test-{}-4.sock", std::process::id());
    std::fs::remove_file(&path).ok();
    let listener = UnixListener::bind(&path).unwrap();

    // Spawn 4 connectors
    let mut children: Vec<_> = (0..4).map(|i| spawn_connector(&path, i)).collect();

    // Accept all 4
    let t0 = Instant::now();
    let mut streams = Vec::new();
    for _ in 0..4 {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("accept timeout")
            .expect("accept error");
        streams.push(stream);
    }
    let accept_time = t0.elapsed();
    println!("4 accepts in {:?}", accept_time);
    assert!(
        accept_time < Duration::from_secs(3),
        "accepts too slow: {accept_time:?}"
    );

    // Read one message from each
    for mut stream in streams {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).await.unwrap();
        let msg: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(msg["type"], "step_completed");
    }

    for c in &mut children {
        c.wait().unwrap();
    }
    std::fs::remove_file(&path).ok();
    println!("4 workers: OK");
}

#[tokio::test]
async fn accept_16_workers() {
    let path = format!("/tmp/barca-test-{}-16.sock", std::process::id());
    std::fs::remove_file(&path).ok();
    let listener = UnixListener::bind(&path).unwrap();

    let mut children: Vec<_> = (0..16).map(|i| spawn_connector(&path, i)).collect();

    let t0 = Instant::now();
    let mut streams = Vec::new();
    for _ in 0..16 {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("accept timeout — worker failed to connect")
            .expect("accept error");
        streams.push(stream);
    }
    let accept_time = t0.elapsed();
    println!("16 accepts in {:?}", accept_time);
    assert!(
        accept_time < Duration::from_secs(5),
        "accepts too slow: {accept_time:?}"
    );

    // Verify all messages received
    let mut ids = Vec::new();
    for mut stream in streams {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).await.unwrap();
        let msg: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        ids.push(msg["node_id"].as_str().unwrap().to_string());
    }
    assert_eq!(ids.len(), 16);
    println!("16 workers: OK, ids = {ids:?}");

    for c in &mut children {
        c.wait().unwrap();
    }
    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn accept_16_workers_with_barca_worker() {
    //! This test uses actual barca workers (python -m barca._worker) with a
    //! minimal batch JSON. Tests the real Python startup + socket connect path.
    let path = format!("/tmp/barca-test-{}-barca.sock", std::process::id());
    std::fs::remove_file(&path).ok();
    let listener = UnixListener::bind(&path).unwrap();

    // Create a minimal batch file that the worker can process (one no-op step)
    let batch = serde_json::json!({
        "stream_id": "test-0",
        "artifact_dir": "/tmp/barca-test-artifacts",
        "steps": [{
            "node_id": "test.py:noop",
            "function_name": "noop",
            "source_file": "/dev/null",
            "kind": "task",
            "inputs": {},
            "timeout_seconds": 5,
            "direct_args": [],
            "direct_kwargs": {},
            "serializer": "json",
        }],
        "provided_inputs": {}
    });

    // Find python with barca installed
    let python = std::env::current_dir().unwrap().join(".venv/bin/python");
    if !python.exists() {
        println!("SKIP: no .venv/bin/python found");
        std::fs::remove_file(&path).ok();
        return;
    }

    // Spawn 16 barca workers pointing at this socket
    let batch_json = serde_json::to_string(&batch).unwrap();
    let mut children = Vec::new();
    for i in 0..16 {
        let batch_path = format!("/tmp/barca-test-batch-{i}.json");
        std::fs::write(&batch_path, &batch_json).unwrap();
        let mut cmd = Command::new(&python);
        cmd.args(["-m", "barca._worker", &batch_path])
            .env("BARCA_SOCKET", &path)
            .env("BARCA_WORKER", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let child = barca_core::helper_proc::spawn_std(&mut cmd);
        match child {
            Ok(c) => children.push((c, batch_path)),
            Err(e) => {
                println!("SKIP: failed to spawn worker {i}: {e}");
                std::fs::remove_file(&path).ok();
                return;
            }
        }
    }

    // Accept all 16 connections
    let t0 = Instant::now();
    let mut connected = 0;
    for i in 0..16 {
        match tokio::time::timeout(Duration::from_secs(10), listener.accept()).await {
            Ok(Ok(_)) => {
                connected += 1;
            }
            Ok(Err(e)) => {
                println!("accept {i} error: {e}");
            }
            Err(_) => {
                println!("accept {i} TIMEOUT after {:?}", t0.elapsed());
                break;
            }
        }
    }
    let accept_time = t0.elapsed();
    println!("Accepted {connected}/16 barca workers in {:?}", accept_time);

    // Clean up
    for (c, batch_path) in children.iter_mut() {
        let _ = c.kill();
        let _ = c.wait();
        std::fs::remove_file(batch_path.as_str()).ok();
    }
    std::fs::remove_file(&path).ok();

    assert_eq!(connected, 16, "Not all workers connected");
}
