use super::*;
use crate::coordinator::ItemSpec;
use std::collections::HashMap;

#[test]
fn a_failed_attempt_reads_as_python_prints_it() {
    assert_eq!(
        traceback_lines(
            "ValueError",
            "bad input",
            "  File \"p.py\", line 3, in a\n    raise ValueError(\"bad input\")"
        ),
        [
            "Traceback (most recent call last):",
            "  File \"p.py\", line 3, in a",
            "    raise ValueError(\"bad input\")",
            "ValueError: bad input",
        ]
    );
    assert_eq!(traceback_lines("KeyError", "", ""), ["KeyError"]);
}

#[test]
fn a_dead_workers_exit_status_names_the_signal_or_code() {
    use std::os::unix::process::ExitStatusExt;
    let killed = std::process::ExitStatus::from_raw(libc::SIGKILL);
    assert_eq!(
        disconnect_message("p.py:big", Some(killed), false),
        "worker for p.py:big exited with SIGKILL (possibly the OOM killer)"
    );
    let segv = std::process::ExitStatus::from_raw(libc::SIGSEGV);
    assert_eq!(
        disconnect_message("p.py:big", Some(segv), true),
        "worker for p.py:big exited with SIGSEGV while it waited for its parallel() branches"
    );
    let code = std::process::ExitStatus::from_raw(3 << 8);
    assert_eq!(
        disconnect_message("p.py:big", Some(code), false),
        "worker for p.py:big exited with code 3"
    );
    assert_eq!(
        disconnect_message("p.py:big", None, false),
        "worker disconnected"
    );
    assert_eq!(signal_name(64), "signal 64");
}

#[tokio::test]
async fn a_live_process_is_not_reaped_and_a_dead_one_is() {
    let mut sleeper = crate::helper_proc::spawn_std(Command::new("sleep").arg("5")).unwrap();
    assert!(reap_briefly(&mut sleeper).await.is_none());
    sleeper.kill().unwrap();
    let status = reap_briefly(&mut sleeper)
        .await
        .expect("killed process reaped");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}

#[tokio::test]
async fn completed_receipts_do_not_overtake_their_stdout_frames() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    let (commands, rx) = mpsc::channel(2);
    let (events, mut event_rx) = mpsc::channel(16);
    let task = tokio::spawn(worker_io_task(0, stream, rx, events));
    commands
        .send(crate::wire::encode_steps(&[
            serde_json::json!({"node_id":"a"}),
            serde_json::json!({"node_id":"b"}),
        ]))
        .await
        .unwrap();
    let assigned: serde_json::Value = read_frame(&mut peer).await.unwrap().unwrap();
    let mut peer = peer.into_std().unwrap();
    peer.set_nonblocking(false).unwrap();
    let a = serde_json::json!({"path":"/a.json","format":"json","size_bytes":1});
    let b = serde_json::json!({"path":"/b.json","format":"json","size_bytes":1});
    // Queue a valid producer sequence before this single-threaded runtime polls
    // the receiver. The journal is ahead of socket consumption, not socket writes.
    crate::protocol::write_message(
        &mut peer,
        &serde_json::json!({"type":"log","node_id":"a","line":"a output"}),
    )
    .unwrap();
    std::fs::write(
        assigned["result_journal"].as_str().unwrap(),
        format!("{}\n", serde_json::json!(["a", "step_completed", 0.1, a])),
    )
    .unwrap();
    crate::protocol::write_message(
        &mut peer,
        &serde_json::json!({"type":"result_checkpoint","completed":1}),
    )
    .unwrap();
    crate::protocol::write_message(
        &mut peer,
        &serde_json::json!({"type":"log","node_id":"b","line":"b output"}),
    )
    .unwrap();
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(assigned["result_journal"].as_str().unwrap())
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!(["b", "step_completed", 0.2, b])
        )
        .unwrap();
    }
    crate::protocol::write_message(&mut peer, &serde_json::json!({"type":"results","node_ids":["a","b"],"statuses":["step_completed","step_completed"],"elapsed_seconds":[0.1,0.2],"details":crate::wire::encode_table(&[a,b],true)})).unwrap();
    let mut seen = Vec::new();
    for _ in 0..5 {
        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        seen.push(match event {
            IoEvent::Message { msg, .. } => match *msg {
                WorkerMessage::Log { node_id, .. } => format!("log:{node_id}"),
                WorkerMessage::StepCompleted { node_id, .. } => format!("done:{node_id}"),
                other => panic!("unexpected {other:?}"),
            },
            IoEvent::BatchFinished { .. } => "finished".to_string(),
            _ => panic!("unexpected event"),
        });
    }
    drop(commands);
    drop(peer);
    task.await.unwrap();
    assert_eq!(seen, ["log:a", "done:a", "log:b", "done:b", "finished"]);
}

#[test]
fn build_step_json_includes_sinks() {
    let mut coord = Coordinator::new();
    let spec = ItemSpec {
        fn_ref: "f.py:a".to_string(),
        function_name: "a".to_string(),
        source_file: "f.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: Some("parquet".to_string()),
        run_hash: Some("abc123".to_string()),
        sinks: vec![
            barca_model::model::SinkDecl {
                path: "abfss://cont@acct.dfs.core.windows.net/exports/a.parquet".to_string(),
                serializer: Some(barca_model::model::SerializerKind::Parquet),
                ..Default::default()
            },
            barca_model::model::SinkDecl {
                path: "exports/a.pkl".to_string(),
                serializer: None,
                ..Default::default()
            },
        ],
        upstream_inputs: HashMap::new(),
        collected_inputs: HashMap::new(),
        param_types: HashMap::new(),
        return_type: None,
        kind: "asset".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    };
    let id = coord.add_item(
        barca_model::StepId::unpartitioned("f.py:a"),
        spec,
        Vec::new(),
    );
    let step = build_step_json(coord.item(id), &coord);

    assert_eq!(
        step["sinks"],
        serde_json::json!([
            {"path": "abfss://cont@acct.dfs.core.windows.net/exports/a.parquet", "serializer": "parquet"},
            {"path": "exports/a.pkl", "serializer": null},
        ])
    );
    assert_eq!(step["serializer"], serde_json::json!("parquet"));
}

#[test]
fn build_step_json_includes_param_types_from_spec() {
    use barca_model::model::ValueType;

    let mut coord = Coordinator::new();
    let spec = ItemSpec {
        fn_ref: "f.py:downstream".to_string(),
        function_name: "downstream".to_string(),
        source_file: "f.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: None,
        sinks: Vec::new(),
        run_hash: None,
        upstream_inputs: HashMap::from([("orders".to_string(), "f.py:upstream".to_string())]),
        collected_inputs: HashMap::new(),
        param_types: HashMap::from([("orders".to_string(), ValueType::Polars)]),
        return_type: Some(ValueType::Polars),
        kind: "asset".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    };
    let id = coord.add_item(
        barca_model::StepId::unpartitioned("f.py:downstream"),
        spec,
        Vec::new(),
    );
    let step = build_step_json(coord.item(id), &coord);

    assert_eq!(step["param_types"]["orders"], "polars");
    assert_eq!(step["return_type"], "polars");
}

#[test]
fn build_step_json_empty_sinks_serializes_as_empty_array() {
    let mut coord = Coordinator::new();
    let spec = ItemSpec {
        fn_ref: "f.py:a".to_string(),
        function_name: "a".to_string(),
        source_file: "f.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: None,
        sinks: Vec::new(),
        run_hash: None,
        upstream_inputs: HashMap::new(),
        collected_inputs: HashMap::new(),
        param_types: HashMap::new(),
        return_type: None,
        kind: "asset".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    };
    let id = coord.add_item(
        barca_model::StepId::unpartitioned("f.py:a"),
        spec,
        Vec::new(),
    );
    let step = build_step_json(coord.item(id), &coord);
    assert_eq!(step["sinks"], serde_json::json!([]));
    assert_eq!(step["run_hash"], serde_json::Value::Null);
}

/// Regression test for #93: a `collect()` param must be sent to the
/// worker as the `{"_collected": true, "artifacts": [...]}` marker with
/// every partition artifact, not collapsed to a single path.
#[test]
fn build_step_json_sends_collected_param_as_artifact_list() {
    let mut coord = Coordinator::new();
    let mut spec = ItemSpec {
        fn_ref: "f.py:sink".to_string(),
        function_name: "sink".to_string(),
        source_file: "f.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: None,
        sinks: Vec::new(),
        run_hash: None,
        upstream_inputs: HashMap::from([("data".to_string(), "f.py:source".to_string())]),
        collected_inputs: HashMap::new(),
        param_types: HashMap::new(),
        return_type: None,
        kind: "asset".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    };
    spec.collected_inputs.insert(
        "data".to_string(),
        vec![
            crate::dispatch::OutputRef {
                path: "f--source_key_a.json".to_string(),
                format: "json".to_string(),
                size_bytes: 10,
                elapsed_seconds: None,
                content_hash: None,
            },
            crate::dispatch::OutputRef {
                path: "f--source_key_b.json".to_string(),
                format: "json".to_string(),
                size_bytes: 12,
                elapsed_seconds: None,
                content_hash: None,
            },
        ],
    );
    let id = coord.add_item(
        barca_model::StepId::unpartitioned("f.py:sink"),
        spec,
        Vec::new(),
    );
    let step = build_step_json(coord.item(id), &coord);

    assert_eq!(
        step["inputs"]["data"],
        serde_json::json!({
            "_collected": true,
            "artifacts": [
                {"path": "f--source_key_a.json", "format": "json"},
                {"path": "f--source_key_b.json", "format": "json"},
            ],
        })
    );
}

#[test]
fn inline_branch_results_are_refused() {
    let inline = serde_json::json!({"path":"", "format":"json", "size_bytes":3, "json":"123"});
    assert!(matches!(
        branch_result(Ok(Some(&inline)), true),
        ParallelResult::Error { .. }
    ));
}

/// #285: a finished branch is answered with its artifact, whatever its format, and the
/// coordinator does not read a file it could not pass on as it is.
#[test]
fn a_finished_branch_is_answered_with_where_its_artifact_is() {
    for (format, frame_type) in [
        ("json", None),
        ("pickle", None),
        ("parquet", Some("polars")),
    ] {
        let mut artifact = serde_json::json!({
            "path": "/nowhere/branch.out", "format": format, "size_bytes": 3
        });
        if let Some(t) = frame_type {
            artifact["frame_type"] = serde_json::json!(t);
        }
        let ParallelResult::Ok { artifact } = branch_result(Ok(Some(&artifact)), true) else {
            panic!("a finished {format} branch must be ok");
        };
        assert_eq!(
            artifact,
            crate::protocol::BranchArtifact {
                path: "/nowhere/branch.out".to_string(),
                format: format.to_string(),
                frame_type: frame_type.map(str::to_string),
            }
        );
    }
}

#[test]
fn legacy_inline_requests_are_refused() {
    let artifact = serde_json::json!({"path":"/a.json", "format":"json", "size_bytes":3});
    assert!(matches!(
        branch_result(Ok(Some(&artifact)), false),
        ParallelResult::Error { .. }
    ));
}

#[test]
fn a_failed_branch_is_answered_with_its_error_and_a_missing_artifact_is_an_error() {
    for artifact_results in [true, false] {
        let ParallelResult::Error { error } =
            branch_result(Err("ValueError: boom"), artifact_results)
        else {
            panic!("a failed branch is an error");
        };
        assert_eq!(error, "ValueError: boom");
        assert!(matches!(
            branch_result(Ok(None), artifact_results),
            ParallelResult::Error { .. }
        ));
    }
}

/// The worker is told which steps are branches: it reports a result it cannot write as
/// an error for the caller, and records the frame type of one it can.
#[test]
fn build_step_json_marks_parallel_branches() {
    let mut coord = Coordinator::new();
    let spec = ItemSpec {
        fn_ref: "f.py:a".to_string(),
        function_name: "a".to_string(),
        source_file: "f.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: None,
        sinks: Vec::new(),
        run_hash: None,
        upstream_inputs: HashMap::new(),
        collected_inputs: HashMap::new(),
        param_types: HashMap::new(),
        return_type: None,
        kind: "task".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    };
    let parent = coord.add_item(
        barca_model::StepId::unpartitioned("f.py:a"),
        spec.clone(),
        Vec::new(),
    );
    assert_eq!(build_step_json(coord.item(parent), &coord)["branch"], false);
    let (_, children) = coord.on_parallel_requested(parent, vec![spec]);
    assert_eq!(
        build_step_json(coord.item(children[0]), &coord)["branch"],
        true
    );
}

/// #332: a branch is told where to write its result, in its group's own directory and
/// under its own number.
#[test]
fn build_step_json_gives_a_branch_its_own_result_path() {
    let mut coord = Coordinator::new();
    let spec = ItemSpec {
        fn_ref: "f.py:a".to_string(),
        function_name: "a".to_string(),
        source_file: "f.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: None,
        sinks: Vec::new(),
        run_hash: None,
        upstream_inputs: HashMap::new(),
        collected_inputs: HashMap::new(),
        param_types: HashMap::new(),
        return_type: None,
        kind: "task".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    };
    let parent = coord.add_item(
        barca_model::StepId::unpartitioned("f.py:a"),
        spec.clone(),
        Vec::new(),
    );
    // The same call twice in one group: two branches, two paths.
    let (group, children) = coord.on_parallel_requested(parent, vec![spec.clone(), spec]);
    assert_eq!(
        build_step_json(coord.item(children[0]), &coord)["branch_path"],
        serde_json::Value::Null,
        "no directory, no path: the worker names the file as it used to"
    );
    coord.set_group_result_dir(group, PathBuf::from("/p/.barca/branches/r1-77/3"));
    let paths: Vec<String> = children
        .iter()
        .map(|&c| {
            build_step_json(coord.item(c), &coord)["branch_path"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        paths,
        [
            format!("/p/.barca/branches/r1-77/3/{}", children[0].0),
            format!("/p/.barca/branches/r1-77/3/{}", children[1].0),
        ]
    );
    assert_ne!(paths[0], paths[1]);
    assert_eq!(
        build_step_json(coord.item(parent), &coord)["branch_path"],
        serde_json::Value::Null
    );
}

/// Two runs never share a directory, and a run's directory names the process that owns it.
#[test]
fn a_runs_branch_results_are_under_its_own_id_and_pid() {
    let a = branch_results_root("run-a");
    let b = branch_results_root("run-b");
    assert_ne!(a, b);
    assert!(a.ends_with(format!(".barca/branches/run-a-{}", std::process::id())));
}

/// The sweep removes what a killed run left and nothing else: not a live run's results
/// (this process is alive), not a name it did not make.
#[cfg(unix)]
#[test]
fn the_sweep_removes_the_branch_results_of_dead_runs_only() {
    // A pid that existed and is certainly gone: a child that has been reaped.
    let mut child = crate::helper_proc::spawn_std(&mut Command::new("true")).unwrap();
    let dead = child.id();
    child.wait().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let of_dead = dir.path().join(format!("r1-{dead}"));
    let of_live = dir.path().join(format!("r2-{}", std::process::id()));
    let foreign = dir.path().join("notes");
    for d in [&of_dead, &of_live, &foreign] {
        std::fs::create_dir_all(d.join("0")).unwrap();
        std::fs::write(d.join("0").join("5.json"), "1").unwrap();
    }
    sweep_branch_results(dir.path());
    assert!(!of_dead.exists());
    assert!(of_live.join("0").join("5.json").exists());
    assert!(foreign.join("0").join("5.json").exists());
    // A project that never used parallel() has no such directory: nothing to do.
    sweep_branch_results(&dir.path().join("absent"));
}

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

fn test_spec(name: &str) -> ItemSpec {
    ItemSpec {
        fn_ref: format!("p.py:{name}"),
        function_name: name.to_string(),
        source_file: "p.py".to_string(),
        direct_args: Vec::new(),
        direct_kwargs: HashMap::new(),
        dag_inputs: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        serializer: None,
        sinks: Vec::new(),
        run_hash: None,
        upstream_inputs: HashMap::new(),
        collected_inputs: HashMap::new(),
        param_types: HashMap::new(),
        return_type: None,
        kind: "asset".to_string(),
        keyed_inputs: HashMap::new(),
        partition_source: false,
        previous_sensor: None,
    }
}

fn test_config(run_id: &str, pool_size: usize) -> IoConfig {
    IoConfig {
        upload_snapshot_prefix: None,
        python: PathBuf::from("python3"),
        pool_size,
        capacity: pool_size,
        host: crate::run_host::LocalHost::shared(),
        run_id: format!("{run_id}-{}", std::process::id()),
        datadog: None,
        artifact_root: ".".to_string(),
        storage_options_json: None,
        agent_mode: false,
    }
}

/// #292: a worker that ends without connecting (a Ctrl-C used to end one that was still
/// starting) is reported when it ends, not after the ten seconds allowed for connecting.
#[cfg(unix)]
#[tokio::test]
async fn a_worker_that_exits_before_connecting_fails_the_start_at_once() {
    let mut pool = WorkerPool::start(test_config("exits-early", 1)).unwrap();
    pool.launch(sh("exit 3"), 0).unwrap();
    let started = std::time::Instant::now();
    let why = loop {
        let failed = pool.fail_dead_starts();
        if let Some(why) = failed.into_iter().next() {
            break why;
        }
        assert!(
            started.elapsed() < WORKER_CONNECT_LIMIT / 2,
            "still waiting"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(
        why.contains("exited before it connected") && why.contains("exit status: 3"),
        "{why}"
    );
    assert!(pool.pending.is_empty());
    pool.shutdown().await;
}

/// #292: a worker that is still starting when the run ends (cancelled, say) is not left
/// running: `shutdown` stops and reaps it, though it never joined the pool.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_stops_a_worker_that_never_connected() {
    let mut pool = WorkerPool::start(test_config("never-connects", 1)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    // Never connects: a worker held in its start-up.
    pool.launch(
        sh(&format!("echo $$ > {}; exec sleep 60", pid_file.display())),
        0,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !std::fs::read_to_string(&pid_file).is_ok_and(|s| s.ends_with('\n')) {
        assert!(std::time::Instant::now() < deadline, "the worker never ran");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let at = std::time::Instant::now();
    pool.shutdown().await;
    assert!(
        at.elapsed() < WORKER_CONNECT_LIMIT / 2,
        "{:?}",
        at.elapsed()
    );
    let pid: i64 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        !barca_model::process::pid_alive(pid),
        "the worker is still running"
    );
}

/// Connect to the pool's socket as a worker would, with `worker_id` in the hello.
async fn connect_as(path: &Path, worker_id: Option<usize>) -> UnixStream {
    let mut stream = UnixStream::connect(path).await.unwrap();
    let mut hello = serde_json::json!({
        "type": "hello", "role": "worker",
        "protocol_version": crate::protocol::PROTOCOL_VERSION,
    });
    if let Some(id) = worker_id {
        hello["worker_id"] = id.into();
    }
    write_frame(&mut stream, &hello).await.unwrap();
    stream
}

/// #560: workers start together and connect in any order. Each connection joins the pool
/// as the worker its hello names, so the process a worker handle stops is the one that
/// connection talks to. A connection naming no worker that is starting is closed.
#[cfg(unix)]
#[tokio::test]
async fn workers_that_connect_out_of_order_are_paired_by_their_hello() {
    let mut pool = WorkerPool::start(test_config("pairing", 3)).unwrap();
    for wid in 0..3 {
        pool.launch(sh("exec sleep 30"), wid).unwrap();
    }
    let pids: HashMap<usize, u32> = pool
        .pending
        .iter()
        .map(|(&wid, p)| (wid, p.child.id()))
        .collect();
    let socket = pool.socket_path.clone();
    let mut clients = Vec::new();
    for wid in [Some(2), Some(0), Some(7), Some(1)] {
        clients.push(connect_as(&socket, wid).await);
        let Some(IoEvent::Connected {
            worker_id,
            pid,
            stream,
        }) = pool.event_rx.recv().await
        else {
            panic!("expected a connection");
        };
        assert_eq!(worker_id, wid);
        assert_eq!(pool.on_connected(worker_id, pid, stream), wid != Some(7));
    }
    assert!(pool.pending.is_empty());
    assert_eq!(pool.workers.len(), 3);
    for (wid, pid) in pids {
        assert_eq!(pool.workers[&wid].child.id(), pid, "worker {wid}");
    }
    // The stranger's connection was acknowledged, then closed.
    let stranger = &mut clients[2];
    let ack: Option<serde_json::Value> = read_frame(stranger).await.unwrap();
    assert_eq!(ack.unwrap()["type"], "hello");
    let closed: Result<Option<serde_json::Value>, _> = read_frame(stranger).await;
    assert!(matches!(closed, Ok(None) | Err(_)));
    pool.shutdown().await;
}

/// #560: ready work with no idle worker starts every worker it can use at once, without
/// waiting on any (one per batch, up to `pool_size`); a one-step run starts one.
#[cfg(unix)]
#[tokio::test]
async fn the_workers_a_wave_needs_start_together_and_no_more() {
    for (ready, pool_size, started) in [(1, 4, 1), (3, 4, 3), (6, 4, 4)] {
        let mut config = test_config("together", pool_size);
        // A stand-in for the interpreter that never connects. Found on PATH: macOS has
        // no /bin/true, only /usr/bin/true.
        config.python = PathBuf::from("true");
        let mut pool = WorkerPool::start(config).unwrap();
        let mut coord = Coordinator::new();
        for i in 0..ready {
            let name = format!("n{i}");
            coord.add_item(
                barca_model::StepId {
                    base: std::sync::Arc::from(format!("p.py:{name}")),
                    partition: barca_model::model::PartitionKey(Default::default()),
                },
                test_spec(&name),
                vec![],
            );
        }
        // Heavy enough that each step is a batch of its own.
        let mut cost = CostModel::new();
        for i in 0..ready {
            cost.observe(&format!("p.py:n{i}"), 5.0, 5.0, 0);
        }
        let cancel = CancellationToken::new();
        pool.assign_ready(&mut coord, &cost, &cancel).await;
        assert_eq!(
            pool.pending.len(),
            started,
            "{ready} ready, pool {pool_size}"
        );
        assert!(pool.workers.is_empty());
        // Nothing is leased before a worker connects.
        assert_eq!(coord.ready_count(), ready);
        // Again, with the same workers still starting: nobody more is started.
        pool.assign_ready(&mut coord, &cost, &cancel).await;
        assert_eq!(pool.pending.len(), started);
        pool.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_waits_for_workers_already_removed_from_the_pool() {
    let mut pool = WorkerPool::start(test_config("reaping-at-shutdown", 1)).unwrap();
    let mut child = crate::helper_proc::spawn_std(&mut sh("exec sleep 60")).unwrap();
    let pid = child.id();
    let (release, wait) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (reaped, gone) = tokio::sync::oneshot::channel();
    // A rejected worker has left `pending`, but its blocking kill/wait job
    // may not finish before the rest of shutdown. Hold that exact boundary.
    pool.reaping.push(tokio::task::spawn_blocking(move || {
        started.send(()).ok();
        wait.recv().ok();
        child.kill().ok();
        child.wait().ok();
        reaped.send(()).ok();
    }));
    ready.await.unwrap();
    let mut shutdown = tokio::spawn(pool.shutdown());
    let returned_early = tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
        .await
        .is_ok();
    release.send(()).unwrap();
    gone.await.unwrap();
    if !returned_early {
        shutdown.await.unwrap();
    }
    assert!(
        !returned_early,
        "shutdown returned while a rejected worker was still alive"
    );
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
}

/// Regression test for the pool_size*200ms shutdown bug: `shutdown()`
/// used to call `graceful_kill()` (SIGTERM, then an unconditional 200ms
/// sleep before the first liveness check) once per worker in a plain
/// loop, so N workers took ~N*200ms to tear down. Each dummy worker here
/// traps SIGTERM (`trap '' TERM`), so it can only die via the SIGKILL
/// fallback — forcing every worker through the full grace-period path
/// and making the old O(pool_size) vs. new O(1) timing deterministic
/// rather than racing against how fast a real process happens to react
/// to SIGTERM.
#[cfg(unix)]
#[test]
fn shutdown_terminates_all_workers_in_one_shared_grace_window() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let n = 16;
        let resources = tempfile::tempdir().unwrap();
        let socket_path = resources.path().join("worker.sock");
        let branch_root = resources.path().join("branches");
        std::fs::create_dir(&branch_root).unwrap();
        std::fs::write(branch_root.join("owned-result"), "result").unwrap();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (event_tx, event_rx) = mpsc::channel(8);

        let mut workers = HashMap::new();
        let mut pids = Vec::new();
        let mut readiness = Vec::new();
        for i in 0..n {
            // `exec`: the process that ignores SIGTERM is the child itself. Without it
            // the shell forks `sleep`, and killing the shell leaves the `sleep` running
            // for its 30 seconds with every descriptor it inherited.
            let mut cmd = Command::new("sh");
            let ready = resources.path().join(format!("worker-{i}.ready"));
            cmd.args([
                "-c",
                "trap '' TERM; : > \"$1\"; exec sleep 30",
                "barca-shutdown",
            ])
            .arg(&ready);
            readiness.push(ready);
            let child = crate::helper_proc::spawn_std(&mut cmd).unwrap();
            pids.push(child.id());
            let (cmd_tx, _cmd_rx) = mpsc::channel(1);
            workers.insert(
                i,
                WorkerHandle {
                    child,
                    cmd_tx,
                    _task: tokio::spawn(async {}),
                    leases: VecDeque::new(),
                    call: false,
                    batch_open: false,
                    front_since: std::time::Instant::now(),
                },
            );
        }

        // The shell acknowledges readiness only after installing its ignore
        // disposition. exec keeps its PID/disposition; startup is not timed.
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
        let ready = loop {
            if std::time::Instant::now() >= ready_deadline
                || workers
                    .values_mut()
                    .any(|worker| !matches!(worker.child.try_wait(), Ok(None)))
            {
                break false;
            }
            if readiness.iter().all(|marker| marker.exists()) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        if !ready {
            for worker in workers.values_mut() {
                worker.child.kill().ok();
                worker.child.wait().ok();
            }
        }
        assert!(
            ready,
            "workers did not acknowledge SIGTERM-ignore readiness"
        );

        let pool = WorkerPool {
            config: IoConfig {
                upload_snapshot_prefix: None,
                python: PathBuf::from("python3"),
                pool_size: n,
                capacity: n,
                host: crate::run_host::LocalHost::shared(),
                run_id: "test-shutdown".to_string(),
                datadog: None,
                artifact_root: ".".to_string(),
                storage_options_json: None,
                agent_mode: false,
            },
            socket_path: socket_path.clone(),
            acceptor: Acceptor(tokio::spawn(accept_connections(listener, event_tx.clone()))),
            event_tx,
            event_rx,
            workers,
            pending: HashMap::new(),
            frozen: Vec::new(),
            reaping: Vec::new(),
            next_worker_id: n,
            trace_start: std::time::Instant::now(),
            trace_on: false,
            progress_interval: Duration::ZERO,
            running_hook: None,
            repeated_warnings: Vec::new(),
            branch_root: branch_root.clone(),
            next_branch_group: 0,
            branch_dirs: HashMap::new(),
            failures_survived: 0,
            steps_dispatched: 0,
            share: None,
            steps: None,
        };

        let start = std::time::Instant::now();
        pool.shutdown().await;
        let elapsed = start.elapsed();

        // Sixteen ready workers make a sequential grace loop take 3.2s.
        // One quarter of that allows loaded two-CPU scheduling while still
        // rejecting per-worker waits. The lower bound proves ignore/fallback.
        let sequential = Duration::from_millis(n as u64 * 200);
        assert!(
            elapsed >= Duration::from_millis(150) && elapsed < sequential / 4,
            "shutdown of {n} ready SIGTERM-ignoring workers took {elapsed:?} — expected one \
                 shared ~200ms grace window, not ~0 (race) or {sequential:?} (one per worker)"
        );
        assert!(!socket_path.exists(), "owned worker socket remains");
        assert!(!branch_root.exists(), "owned branch results remain");

        for pid in pids {
            // kill(pid, 0) sends no signal, just checks liveness/permission;
            // a nonzero return (ESRCH) confirms the process was reaped.
            let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            assert!(
                !still_alive,
                "worker pid {pid} should have been terminated by shutdown()"
            );
        }

        let _ = std::fs::remove_file(&socket_path);
    });
}
