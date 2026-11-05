use super::*;

#[test]
fn malformed_agent_url_diagnostics_do_not_echo_secret_values() {
    for url in [
        "https://user:secret@host/?token=secret",
        "http://[broken?token=secret",
        "http://host:secret",
        "http://?token=secret",
    ] {
        let error = agent_from(Some(url), None, None).unwrap_err();
        assert!(!error.contains("secret"), "{error}");
    }
}

fn datadog() -> Datadog {
    Datadog {
        agent: Agent::Tcp {
            host: "localhost".to_string(),
            port: 8126,
        },
        service: "planning".to_string(),
        env: Some("staging".to_string()),
        version: Some("1.2.3".to_string()),
        tags: vec![("team".to_string(), "data".to_string())],
        project: Some("shop".to_string()),
        statsd: Ok(dogstatsd::Target::Udp {
            host: "localhost".to_string(),
            port: 8125,
        }),
    }
}

fn node(base: &str) -> Arc<NodeContext> {
    Arc::new(NodeContext::unknown(base))
}

fn step(node_id: &str, outcome: StepOutcome) -> StepReport {
    let id = crate::StepId::parse(node_id);
    StepReport {
        node_id: node_id.to_string(),
        node: node(id.base_id()),
        partition: (!id.partition.is_empty()).then(|| id.partition.suffix()),
        outcome,
        start_unix_ns: 1_000,
        duration_ns: 500,
        attempts: Some(1),
        run_hash: Some("abc".to_string()),
        upstream: Vec::new(),
        size_bytes: Some(42),
        digest: None,
        cpu_seconds: None,
        max_rss_bytes: None,
        error_type: None,
        error_message: None,
        error_traceback: None,
    }
}

fn failed(node_id: &str, message: &str) -> StepReport {
    let mut s = step(node_id, StepOutcome::Failed);
    s.error_type = Some("ValueError".to_string());
    s.error_message = Some(message.to_string());
    s.error_traceback = Some(format!(
        "  File \"p.py\", line 3\n    raise ValueError({message:?})"
    ));
    s
}

fn run(status: &str, steps: Vec<StepReport>) -> RunReport {
    RunReport {
        run_id: "r1".to_string(),
        command: "run".to_string(),
        target: Some("publish".to_string()),
        job: "p.py:publish".to_string(),
        env: None,
        targets: vec!["publish".to_string()],
        files: vec!["p.py".to_string()],
        status: status.to_string(),
        reason: None,
        in_flight_run: None,
        start_unix_ns: 900,
        duration_ns: 2_000,
        steps_total: steps.len(),
        steps_executed: 1,
        steps_cached: 1,
        steps,
        trigger: Default::default(),
    }
}

/// The run's trace: its one span.
fn root(run: &RunReport) -> Value {
    let body = datadog().run_trace(run);
    let traces: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(traces.as_array().unwrap().len(), 1, "one trace per request");
    assert_eq!(traces[0].as_array().unwrap().len(), 1, "the run span alone");
    Datadog::root_of(&body)
}

/// A step's trace in run `r1`: its one span.
fn step_root(step: &StepReport) -> Value {
    let body = datadog().step_trace("r1", "p.py:publish", step);
    let traces: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        traces[0].as_array().unwrap().len(),
        1,
        "the step span alone"
    );
    Datadog::root_of(&body)
}

#[test]
fn agent_address_comes_from_the_url_or_host_and_port() {
    let tcp = |host: &str, port| Agent::Tcp {
        host: host.to_string(),
        port,
    };
    assert_eq!(agent_from(None, None, None), Ok(tcp("localhost", 8126)));
    assert_eq!(
        agent_from(None, Some("dd"), Some("9000")),
        Ok(tcp("dd", 9000))
    );
    assert_eq!(
        agent_from(Some("http://agent:8127/"), Some("ignored"), None),
        Ok(tcp("agent", 8127))
    );
    assert_eq!(
        agent_from(Some("http://agent"), None, None),
        Ok(tcp("agent", 8126))
    );
    assert_eq!(
        agent_from(Some("unix:///var/run/datadog/apm.socket"), None, None),
        Ok(Agent::Unix {
            path: "/var/run/datadog/apm.socket".to_string()
        })
    );
    assert_eq!(
        agent_from(Some("HTTP://agent:8127"), None, None),
        Ok(tcp("agent", 8127))
    );
    assert_eq!(
        agent_from(Some("http://[::1]:8127/x"), None, None),
        Ok(tcp("::1", 8127))
    );
    assert_eq!(
        agent_from(Some("http://[::1]"), None, None),
        Ok(tcp("::1", 8126))
    );
    assert!(agent_from(Some("http://[::1"), None, None).is_err());
    assert!(agent_from(Some("http://[::1]junk:8127"), None, None).is_err());
    assert_eq!(
        agent_from(Some("http://agent:8127#frag"), None, None),
        Ok(tcp("agent", 8127))
    );
    let with_password = agent_from(Some("http://user:secret@agent:8127"), None, None);
    assert!(!with_password.unwrap_err().contains("secret"));
    assert!(agent_from(Some("https://agent:8126"), None, None).is_err());
    assert!(agent_from(None, None, Some("many")).is_err());
}

#[test]
fn dd_trace_enabled_is_on_when_unset_and_otherwise_only_for_true_or_one() {
    for off in ["false", "FALSE", "False", "0", "", "no", "off", "\"false\""] {
        assert!(!tracing_enabled(Some(off)), "{off:?}");
    }
    for on in ["true", "TRUE", "True", "1"] {
        assert!(tracing_enabled(Some(on)), "{on:?}");
    }
    assert!(tracing_enabled(None));
}

#[test]
fn tags_split_on_commas_and_spaces() {
    assert_eq!(
        parse_tags("team:data, client:acme tier:1 novalue"),
        vec![
            ("team".to_string(), "data".to_string()),
            ("client".to_string(), "acme".to_string()),
            ("tier".to_string(), "1".to_string()),
        ]
    );
}

#[test]
fn a_scheduled_run_names_its_trigger_tick_and_schedule() {
    let mut scheduled = run("success", Vec::new());
    scheduled.trigger = crate::outcome::Trigger::schedule("p.py:publish", 1_800_000_000);
    let root = root(&scheduled);
    assert_eq!(root["meta"]["barca.trigger"], "schedule");
    assert_eq!(root["meta"]["barca.scheduled_at"], "2027-01-15T08:00:00Z");
    assert_eq!(root["meta"]["barca.schedule_id"], "p.py:publish");
    let manual = super::super::datadog::tests::root(&run("success", Vec::new()));
    assert_eq!(manual["meta"]["barca.trigger"], "manual");
    assert!(manual["meta"].get("barca.scheduled_at").is_none());
    assert!(manual["meta"].get("barca.schedule_id").is_none());
}

#[test]
fn ids_are_stable_non_zero_and_fit_in_63_bits() {
    let a = span_id(&["step", "r1", "p.py:orders"]);
    assert_eq!(a, span_id(&["step", "r1", "p.py:orders"]));
    assert_ne!(a, span_id(&["step", "r1", "p.py:other"]));
    assert_ne!(a, span_id(&["step", "r1p.py:", "orders"]));
    assert!(a > 0 && a < (1 << 63));
    // Every step of a run has a trace of its own, apart from the run's and each other's; a
    // partition key is a step of its own.
    let run = trace_id("r1");
    let orders = step_trace_id("r1", "p.py:orders");
    let key = step_trace_id("r1", "p.py:weekly[week=w1]");
    assert!(run != orders && orders != key && key != step_trace_id("r1", "p.py:weekly"));
    assert_eq!(orders, step_trace_id("r1", "p.py:orders"));
    assert_ne!(orders, step_trace_id("r2", "p.py:orders"));
    assert_ne!(step_trace_id_high("r1", "p.py:orders"), trace_id_high("r1"));
}

#[test]
fn a_log_line_joins_its_step_trace_or_the_run_trace() {
    assert_eq!(
        log_ids("r1", Some("p.py:a")),
        (step_trace_id("r1", "p.py:a"), step_span_id("r1", "p.py:a"))
    );
    assert_eq!(
        log_ids("r1", Some("p.py:a[k=1]")),
        (
            step_trace_id("r1", "p.py:a[k=1]"),
            step_span_id("r1", "p.py:a[k=1]")
        )
    );
    assert_eq!(log_ids("r1", None), (trace_id("r1"), run_span_id("r1")));
}

#[tokio::test]
async fn put_accepts_a_2xx_and_reports_anything_else() {
    async fn exchange(reply: &'static str) -> (Result<(), String>, String) {
        let (client, mut server) = tokio::io::duplex(4096);
        let agent = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.ends_with(b"{}") {
                let n = server.read(&mut buf).await.unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            server.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8(seen).unwrap()
        });
        let result = put(client, "agent:8126", b"{}").await;
        (result, agent.await.unwrap())
    }

    let (ok, request) = exchange("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    assert_eq!(ok, Ok(()));
    assert!(request.starts_with("PUT /v0.4/traces HTTP/1.1\r\nHost: agent:8126\r\n"));
    assert!(request.contains("Content-Length: 2\r\n"));

    let (bad, _) = exchange("HTTP/1.1 415 Unsupported Media Type\r\n\r\n").await;
    assert!(bad.unwrap_err().contains("415"));
}
#[test]
fn every_trace_is_one_service_a_worker_and_carries_the_deployment_tags() {
    let report = run(
        "success",
        vec![
            step("p.py:orders", StepOutcome::Ran),
            step("p.py:weekly[week=w1]", StepOutcome::Ran),
        ],
    );
    let spans = [
        root(&report),
        step_root(&report.steps[0]),
        step_root(&report.steps[1]),
    ];
    assert_eq!(
        spans
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["barca.run", "barca.step", "barca.step"]
    );
    for span in &spans {
        assert_eq!(span["service"], "planning", "{span}");
        assert_eq!(span["type"], "worker");
        let meta = &span["meta"];
        assert_eq!(meta["span.kind"], "internal");
        assert_eq!(meta["component"], "barca");
        assert_eq!(meta["env"], "staging");
        assert_eq!(meta["version"], "1.2.3");
        assert_eq!(meta["team"], "data");
        assert_eq!(meta["barca.version"], BARCA_VERSION);
        assert_eq!(meta["barca.project"], "shop");
        assert_eq!(meta["barca.run_id"], "r1");
        // Each is the root of a trace of its own, kept, with the trace id's upper half.
        assert!(span.get("parent_id").is_none(), "{span}");
        assert_eq!(span["metrics"]["_sampling_priority_v1"], 2);
        assert_eq!(span["meta"]["_dd.p.dm"], "-4");
        assert_eq!(span["meta"]["_dd.p.tid"].as_str().unwrap().len(), 16);
    }
    let ids: HashSet<u64> = spans
        .iter()
        .map(|s| s["trace_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "three traces");
    let tid = spans[0]["meta"]["_dd.p.tid"].as_str().unwrap();
    assert_eq!(u64::from_str_radix(tid, 16).unwrap(), trace_id_high("r1"));
    let tid = spans[1]["meta"]["_dd.p.tid"].as_str().unwrap();
    assert_eq!(
        u64::from_str_radix(tid, 16).unwrap(),
        step_trace_id_high("r1", "p.py:orders")
    );
}

#[test]
fn the_run_resource_is_the_command_and_the_targets_short_names_or_the_files() {
    let one = root(&run("success", Vec::new()));
    assert_eq!(one["resource"], "run publish");
    assert_eq!(one["meta"]["barca.targets"], "publish");
    assert_eq!(one["meta"]["barca.files"], "p.py");
    assert_eq!(one["meta"]["barca.job"], "p.py:publish");
    assert_eq!(one["meta"]["barca.target"], "publish");
    assert_eq!(one["trace_id"], trace_id("r1"));
    assert_eq!(one["span_id"], run_span_id("r1"));

    let mut two = run("success", Vec::new());
    two.command = "get".to_string();
    two.targets = vec!["daily_revenue".to_string(), "orders".to_string()];
    assert_eq!(root(&two)["resource"], "get daily_revenue,orders");

    let mut whole = run("success", Vec::new());
    whole.command = "get".to_string();
    whole.targets = Vec::new();
    whole.target = None;
    whole.job = "all".to_string();
    let span = root(&whole);
    assert_eq!(span["resource"], "get p.py");
    assert!(span["meta"].get("barca.targets").is_none());
    whole.files = Vec::new();
    assert_eq!(root(&whole)["resource"], "get all");
}

#[test]
fn a_step_is_the_root_of_its_own_trace_named_by_its_node_with_its_place_in_the_pipeline() {
    let mut orders = step("p.py:orders", StepOutcome::Ran);
    orders.node = Arc::new(NodeContext {
        group: Some("Sales/Daily".to_string()),
        freshness: "schedule",
        schedule: Some("0 6 * * *".to_string()),
        ..NodeContext::unknown("p.py:orders")
    });
    orders.upstream = vec!["p.py:raw".to_string(), "p.py:rates".to_string()];
    orders.digest = Some("sha".to_string());
    orders.cpu_seconds = Some(0.5);
    orders.max_rss_bytes = Some(1024);
    let step = step_root(&orders);
    assert_eq!(step["name"], "barca.step");
    assert_eq!(step["resource"], "orders");
    assert_eq!(step["trace_id"], step_trace_id("r1", "p.py:orders"));
    assert_eq!(step["span_id"], step_span_id("r1", "p.py:orders"));
    assert!(step.get("parent_id").is_none());
    assert_eq!(step["start"], 1_000);
    assert_eq!(step["duration"], 500);
    assert_eq!(step["error"], 0);
    let meta = &step["meta"];
    assert_eq!(meta["barca.asset"], "orders");
    assert_eq!(meta["barca.node_id"], "p.py:orders");
    assert_eq!(meta["barca.file"], "p.py");
    assert_eq!(meta["barca.group"], "Sales/Daily");
    assert_eq!(meta["barca.kind"], "asset");
    assert_eq!(meta["barca.freshness"], "schedule");
    assert_eq!(meta["barca.schedule"], "0 6 * * *");
    assert_eq!(meta["barca.outcome"], "ran");
    assert_eq!(meta["barca.run_hash"], "abc");
    assert_eq!(meta["barca.upstream"], "p.py:raw,p.py:rates");
    assert_eq!(meta["barca.digest"], "sha");
    assert_eq!(meta["barca.job"], "p.py:publish");
    assert!(meta.get("barca.partition").is_none());
    assert!(meta.get("error.type").is_none());
    let metrics = &step["metrics"];
    assert_eq!(metrics["barca.bytes"], 42);
    assert_eq!(metrics["barca.attempts"], 1);
    assert_eq!(metrics["barca.cpu_seconds"], 0.5);
    assert_eq!(metrics["barca.max_rss_bytes"], 1024);
}

#[test]
fn node_names_are_short_unless_two_files_share_one() {
    let source = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("@asset\ndef {n}():\n    return 1\n"))
            .collect::<String>()
    };
    let mut nodes = crate::parse::extract_nodes(
        &format!("from barca import asset\n{}", source(&["orders", "rates"])),
        "a.py",
    )
    .unwrap();
    nodes.extend(
        crate::parse::extract_nodes(
            &format!("from barca import asset\n{}", source(&["orders"])),
            "b.py",
        )
        .unwrap(),
    );
    let mut dag = crate::dag::Dag::build(&nodes).unwrap();
    dag.groups = vec![
        crate::groups::NodeGroup {
            id: "group:a.py:outer".into(),
            name: "Outer".into(),
            description: String::new(),
            members: vec!["group:a.py:inner".into()],
            output: "a.py:rates".into(),
        },
        crate::groups::NodeGroup {
            id: "group:a.py:inner".into(),
            name: "Inner".into(),
            description: String::new(),
            members: vec!["a.py:rates".into()],
            output: "a.py:rates".into(),
        },
    ];
    let contexts = crate::telemetry::node_contexts(&dag);
    assert_eq!(contexts["a.py:rates"].name, "rates");
    assert_eq!(contexts["a.py:orders"].name, "a.py:orders");
    assert_eq!(contexts["b.py:orders"].name, "b.py:orders");
    assert_eq!(contexts["a.py:rates"].group.as_deref(), Some("Outer/Inner"));
    assert_eq!(contexts["a.py:orders"].group, None);
    assert_eq!(contexts["a.py:rates"].file, "a.py");
    assert_eq!(contexts["a.py:rates"].freshness, "always");
}

#[test]
fn a_failed_run_names_its_first_failed_asset_and_error_on_the_root() {
    let mut first = failed("p.py:publish", &"x".repeat(MAX_MESSAGE_CHARS + 500));
    first.error_traceback = Some(format!("{}  File \"p.py\", line 3", "y".repeat(5000)));
    first.attempts = None;
    let mut later = failed("p.py:audit", "later");
    later.start_unix_ns = 5_000;
    let mut skipped = step("p.py:report", StepOutcome::Skipped);
    skipped.duration_ns = 0;
    let report = run("failed", vec![first, later, skipped]);
    let root = root(&report);
    assert_eq!(root["error"], 1);
    assert_eq!(root["meta"]["barca.status"], "failed");
    assert_eq!(root["meta"]["barca.failed_asset"], "publish");
    assert_eq!(root["meta"]["barca.failed_node_id"], "p.py:publish");
    assert_eq!(root["meta"]["error.type"], "ValueError");
    let message = root["meta"]["error.message"].as_str().unwrap();
    assert!(message.starts_with("publish: xxx") && message.ends_with("[cut]"));
    assert!(root["meta"].get("error.stack").is_none());
    assert_eq!(root["metrics"]["barca.steps.failed"], 2);
    assert_eq!(root["metrics"]["barca.steps.skipped"], 1);

    // The failed step's own trace carries the error, its message and traceback cut.
    let publish = step_root(&report.steps[0]);
    assert_eq!(publish["error"], 1);
    assert_eq!(publish["meta"]["barca.outcome"], "failed");
    assert_eq!(publish["meta"]["error.type"], "ValueError");
    let stack = publish["meta"]["error.stack"].as_str().unwrap();
    assert!(stack.starts_with("[cut]") && stack.ends_with("  File \"p.py\", line 3"));
    assert!(stack.chars().count() < MAX_STACK_CHARS + 20);
    assert!(publish["metrics"].get("barca.attempts").is_none());
}

#[test]
fn a_cancelled_or_skipped_run_says_why() {
    let mut cancelled = run("cancelled", Vec::new());
    cancelled.reason = Some("server shutdown".to_string());
    let span = root(&cancelled);
    assert_eq!(span["error"], 1);
    assert_eq!(span["meta"]["barca.reason"], "server shutdown");
    assert_eq!(span["meta"]["error.type"], "Cancelled");
    assert_eq!(span["meta"]["error.message"], "server shutdown");

    let mut skipped = run("skipped", Vec::new());
    skipped.reason = Some("in_flight".to_string());
    skipped.in_flight_run = Some("abc123".to_string());
    let span = root(&skipped);
    assert_eq!(span["error"], 0);
    assert_eq!(span["meta"]["barca.status"], "skipped");
    assert_eq!(span["meta"]["barca.reason"], "in_flight");
    assert_eq!(span["meta"]["barca.in_flight_run"], "abc123");
    assert!(span["meta"].get("error.type").is_none());

    let ok = root(&run("success", Vec::new()));
    assert!(ok["meta"].get("barca.reason").is_none());
    assert!(ok["meta"].get("error.message").is_none());
    let broken = root(&run("failed", Vec::new()));
    assert_eq!(broken["meta"]["error.type"], "RunFailed");
}

/// A partitioned node with `n` keys: the first `ran` ran, the next `failed` failed, the
/// rest were cached.
fn partitioned(n: usize, ran: usize, failures: usize) -> Vec<StepReport> {
    (0..n)
        .map(|i| {
            let id = format!("p.py:weekly[week=w{i:05}]");
            let mut s = if i < ran {
                step(&id, StepOutcome::Ran)
            } else if i < ran + failures {
                failed(&id, &format!("bad week {i}"))
            } else {
                step(&id, StepOutcome::Cached)
            };
            s.start_unix_ns = 1_000 + i as u64 * 10;
            s.duration_ns = 100;
            s
        })
        .collect()
}

#[test]
fn a_partition_key_is_a_step_of_its_own_with_the_key_in_a_tag_not_the_resource() {
    let keys = partitioned(4, 2, 1);
    let spans: Vec<Value> = keys[..3].iter().map(step_root).collect();
    for (span, key) in spans.iter().zip(&keys) {
        assert_eq!(span["name"], "barca.step");
        assert_eq!(span["resource"], "weekly", "no partition key in a resource");
        assert_eq!(span["meta"]["barca.asset"], "weekly");
        assert_eq!(span["meta"]["barca.node_id"], key.node_id.as_str());
        assert_eq!(
            span["meta"]["barca.partition"],
            key.partition.as_deref().unwrap()
        );
        assert_eq!(span["trace_id"], step_trace_id("r1", &key.node_id));
        assert_eq!(span["span_id"], step_span_id("r1", &key.node_id));
        assert!(span.get("parent_id").is_none());
        assert_eq!(span["start"], key.start_unix_ns);
        assert_eq!(span["duration"], 100);
        assert_eq!(span["metrics"]["barca.attempts"], 1);
    }
    let ids: HashSet<u64> = spans
        .iter()
        .map(|s| s["trace_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "one trace per key");
    assert_eq!(spans[0]["meta"]["barca.partition"], "week=w00000");
    assert_eq!(spans[2]["meta"]["barca.partition"], "week=w00002");
    assert_eq!(spans[2]["error"], 1);
    assert_eq!(spans[2]["meta"]["error.message"], "bad week 2");
    assert!(spans[2]["meta"]["error.stack"].is_string());
    // The run names the failed key.
    let report = run("failed", keys);
    assert_eq!(
        root(&report)["meta"]["error.message"],
        "weekly[week=w00002]: bad week 2"
    );
    // Only the keys that ran or failed have a trace: the cached one sends nothing.
    let nothing_sent = HashSet::new();
    let pending: Vec<&str> = to_send(&report, &nothing_sent)
        .map(|s| s.node_id.as_str())
        .collect();
    assert_eq!(
        pending,
        [
            "p.py:weekly[week=w00000]",
            "p.py:weekly[week=w00001]",
            "p.py:weekly[week=w00002]"
        ]
    );
}

#[tokio::test]
async fn the_end_of_the_run_sends_only_what_the_live_path_did_not() {
    let report = run(
        "failed",
        vec![
            step("p.py:orders", StepOutcome::Ran),
            failed("p.py:publish", "no"),
            step("p.py:cached", StepOutcome::Cached),
            step("p.py:report", StepOutcome::Skipped),
        ],
    );
    let run_id = "sent-test";
    note_sent(run_id, "p.py:orders".to_string(), tokio::spawn(async {}));
    let Sent { steps, deliveries } = take_sent(run_id);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(steps, ["p.py:orders".to_string()].into());
    let rest: Vec<&str> = to_send(&report, &steps)
        .map(|s| s.node_id.as_str())
        .collect();
    assert_eq!(
        rest,
        ["p.py:publish"],
        "the failure nobody sent; never a cached or skipped step"
    );
    assert!(
        take_sent(run_id).steps.is_empty(),
        "a run is forgotten once taken"
    );
}

fn dispatch() -> Dispatch {
    Dispatch {
        run_id: "dispatch-test".to_string(),
        service: "planning".to_string(),
        project: Some("shop".to_string()),
        job: "p.py:orders".to_string(),
        nodes: [
            ("p.py:orders".to_string(), node("p.py:orders")),
            ("p.py:weekly".to_string(), node("p.py:weekly")),
        ]
        .into(),
        upstream: [("p.py:weekly".to_string(), vec!["p.py:orders".to_string()])].into(),
        exporter: None,
    }
}

#[test]
fn a_worker_gets_its_steps_trace_root_resource_and_tags() {
    let dispatch = dispatch();
    let run_id = "dispatch-test";
    let ctx = dispatch.context(&crate::StepId::parse("p.py:orders"), 2);
    assert_eq!(ctx["trace_id"], step_trace_id(run_id, "p.py:orders"));
    assert_eq!(
        ctx["trace_id_high"],
        step_trace_id_high(run_id, "p.py:orders")
    );
    assert_eq!(ctx["parent_id"], step_span_id(run_id, "p.py:orders"));
    assert_eq!(ctx["resource"], "orders");
    assert_eq!(ctx["service"], "planning");
    assert_eq!(ctx["attempt"], 2);
    assert_eq!(ctx["tags"]["barca.asset"], "orders");
    assert_eq!(ctx["tags"]["barca.node_id"], "p.py:orders");
    assert_eq!(ctx["tags"]["barca.run_id"], run_id);
    assert_eq!(ctx["tags"]["barca.version"], BARCA_VERSION);
    assert_eq!(ctx["tags"]["barca.project"], "shop");
    assert!(ctx["tags"].get("barca.partition").is_none());

    // A key joins the key's own trace, however many keys the node has.
    for i in 0..3_000 {
        let key = crate::StepId::parse(&format!("p.py:weekly[week=w{i}]"));
        let ctx = dispatch.context(&key, 1);
        assert_eq!(ctx["trace_id"], step_trace_id(run_id, &key.display()));
        assert_eq!(ctx["parent_id"], step_span_id(run_id, &key.display()));
        assert_eq!(ctx["resource"], "weekly");
        assert_eq!(ctx["tags"]["barca.partition"], format!("week=w{i}"));
    }
}

#[test]
fn a_finished_step_is_reported_by_its_workers_clock_and_what_it_wrote() {
    let dispatch = dispatch();
    let artifact = json!({
        "path": "x.json", "size_bytes": 7, "content_hash": "sha-7",
        "finished_at": 1_700_000_010.5, "wall_seconds": 2.5,
        "cpu_seconds": 1.25, "max_rss_bytes": 4096
    });
    let step = dispatch.finished_report("p.py:weekly[week=w1]", &artifact, 3, "hash1");
    assert_eq!(step.outcome, StepOutcome::Ran);
    assert_eq!(step.node_id, "p.py:weekly[week=w1]");
    assert_eq!(step.node.name, "weekly");
    assert_eq!(step.partition.as_deref(), Some("week=w1"));
    assert_eq!(step.start_unix_ns, 1_700_000_008_000_000_000);
    assert_eq!(step.duration_ns, 2_500_000_000);
    assert_eq!(step.attempts, Some(3));
    assert_eq!(step.run_hash.as_deref(), Some("hash1"));
    assert_eq!(step.upstream, ["p.py:orders"]);
    assert_eq!(step.size_bytes, Some(7));
    assert_eq!(step.digest.as_deref(), Some("sha-7"));
    assert_eq!(step.cpu_seconds, Some(1.25));
    assert_eq!(step.max_rss_bytes, Some(4096));

    // Without the worker's clock the step ends now and lasts its elapsed seconds.
    let before = crate::time::unix_nanos(std::time::SystemTime::now()) as u64;
    let step = dispatch.finished_report("p.py:orders", &json!({"elapsed_seconds": 1.0}), 1, "h");
    assert!(step.start_unix_ns + step.duration_ns >= before);
    assert_eq!(step.duration_ns, 1_000_000_000);
    assert!(step.upstream.is_empty());
}

#[test]
fn a_failed_step_is_reported_from_its_first_attempt_with_its_exception() {
    let dispatch = dispatch();
    let failure = crate::dispatch::StepFailure {
        node_id: "p.py:orders".to_string(),
        error: crate::dispatch::StepError {
            error_type: "WorkerError".to_string(),
            // As a worker reports an exception: `Type: message`, then the frames.
            message: "ValueError: no orders\n  File \"p.py\", line 3, in orders\n    raise ValueError(\"no orders\")".to_string(),
            traceback: String::new(),
            phase: None,
            attempts: 3,
            started_unix_ns: Some(5_000),
            elapsed_seconds: Some(1.5),
        },
    };
    let step = dispatch.failed_report(&failure);
    assert_eq!(step.outcome, StepOutcome::Failed);
    assert_eq!(step.start_unix_ns, 5_000);
    assert_eq!(step.duration_ns, 1_500_000_000);
    assert_eq!(step.attempts, Some(3));
    assert_eq!(step.error_type.as_deref(), Some("ValueError"));
    assert_eq!(step.error_message.as_deref(), Some("no orders"));
    assert!(
        step.error_traceback
            .as_deref()
            .unwrap()
            .contains("raise ValueError")
    );
    // Its trace is an error that says so.
    let span = Datadog::root_of(&datadog().step_trace("dispatch-test", "p.py:orders", &step));
    assert_eq!(span["error"], 1);
    assert_eq!(span["meta"]["error.type"], "ValueError");
    assert_eq!(span["meta"]["error.message"], "no orders");

    // An untimed failure (recorded without running the step) is a point at the time it was
    // recorded.
    let mut untimed = failure.clone();
    untimed.error.started_unix_ns = None;
    untimed.error.elapsed_seconds = None;
    let step = dispatch.failed_report(&untimed);
    assert_eq!(step.duration_ns, 0);
    assert!(step.start_unix_ns > 1_700_000_000_000_000_000);
}

#[test]
fn serve_lifecycle_is_gauges_and_events_tagged_like_spans() {
    use super::super::ServerPhase;
    let report = |phase| ServerReport {
        phase,
        start_unix_ns: 123,
        files: 2,
        nodes: None,
        schedules: 3,
        read_only: false,
        watch: true,
        scheduling: true,
    };
    let start = datadog().server_packets(&report(ServerPhase::Start));
    let tags = "#service:planning,team:data,env:staging,version:1.2.3,\
                    barca.version:"
        .to_string()
        + BARCA_VERSION
        + ",barca.project:shop,barca.read_only:false,barca.watch:true,barca.scheduling:true";
    assert_eq!(start[0], format!("barca.serve.up:1|g|{tags}"));
    assert_eq!(start[1], format!("barca.serve.schedules:3|g|{tags}"));
    assert_eq!(start[2], format!("barca.serve.files:2|g|{tags}"));
    assert!(start[3].starts_with("_e{19,") && start[3].contains(":barca serve started|"));
    assert_eq!(start.len(), 4);
    let beat = datadog().server_packets(&report(ServerPhase::Heartbeat));
    assert_eq!(beat.len(), 3, "a heartbeat is gauges only");
    let stop = datadog().server_packets(&report(ServerPhase::Stop));
    assert!(stop[0].starts_with("barca.serve.up:0|g|"));
    assert!(stop[3].contains("barca serve stopped"));
    let mut known = report(ServerPhase::Heartbeat);
    known.nodes = Some(7);
    assert!(datadog().server_packets(&known)[3].starts_with("barca.serve.nodes:7|g"));
    // No run, source path or address leaks into a lifecycle metric.
    assert!(
        start
            .iter()
            .all(|p| !p.contains("run_id") && !p.contains(".py"))
    );
}

#[tokio::test]
async fn serve_lifecycle_reaches_a_dogstatsd_listener() {
    use super::super::ServerPhase;
    let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut dd = datadog();
    dd.statsd = Ok(dogstatsd::Target::Udp {
        host: "127.0.0.1".to_string(),
        port: listener.local_addr().unwrap().port(),
    });
    let report = ServerReport {
        phase: ServerPhase::Stop,
        start_unix_ns: 1,
        files: 1,
        nodes: None,
        schedules: 0,
        read_only: true,
        watch: false,
        scheduling: false,
    };
    dd.export_server(&report).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = listener.recv(&mut buf).await.unwrap();
    assert!(
        std::str::from_utf8(&buf[..n])
            .unwrap()
            .starts_with("barca.serve.up:0|g|#")
    );

    dd.statsd = Err("DD_DOGSTATSD_URL is not supported".to_string());
    assert!(dd.export_server(&report).await.is_err());
}
