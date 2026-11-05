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
        cached_partitions: Default::default(),
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

fn spans(run: &RunReport, key_spans: &HashSet<String>) -> Vec<Value> {
    datadog().trace(run, key_spans)[0]
        .as_array()
        .unwrap()
        .clone()
}

fn named<'a>(spans: &'a [Value], name: &str) -> Vec<&'a Value> {
    spans.iter().filter(|s| s["name"] == name).collect()
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
    let root = &spans(&scheduled, &HashSet::new())[0];
    assert_eq!(root["meta"]["barca.trigger"], "schedule");
    assert_eq!(root["meta"]["barca.scheduled_at"], "2027-01-15T08:00:00Z");
    assert_eq!(root["meta"]["barca.schedule_id"], "p.py:publish");
    let manual = &spans(&run("success", Vec::new()), &HashSet::new())[0];
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
fn every_span_is_one_service_a_worker_and_carries_the_deployment_tags() {
    let payload = spans(
        &run(
            "success",
            vec![
                step("p.py:orders", StepOutcome::Ran),
                step("p.py:weekly[week=w1]", StepOutcome::Ran),
            ],
        ),
        &["p.py:weekly[week=w1]".to_string()].into(),
    );
    assert_eq!(
        payload
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["barca.run", "barca.step", "barca.step", "barca.partition"]
    );
    for span in &payload {
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
        assert_eq!(span["trace_id"], payload[0]["trace_id"]);
    }
    // The chunk root carries the keep decision and the trace id's upper half; only it.
    let root = &payload[0];
    assert_eq!(root["metrics"]["_sampling_priority_v1"], 2);
    assert_eq!(root["meta"]["_dd.p.dm"], "-4");
    let tid = root["meta"]["_dd.p.tid"].as_str().unwrap();
    assert_eq!(tid.len(), 16);
    assert_eq!(u64::from_str_radix(tid, 16).unwrap(), trace_id_high("r1"));
    assert!(
        payload[1..]
            .iter()
            .all(|s| s["meta"].get("_dd.p.tid").is_none())
    );
}

#[test]
fn the_run_resource_is_the_command_and_the_targets_short_names_or_the_files() {
    let one = spans(&run("success", Vec::new()), &HashSet::new());
    assert_eq!(one[0]["resource"], "run publish");
    assert_eq!(one[0]["meta"]["barca.targets"], "publish");
    assert_eq!(one[0]["meta"]["barca.files"], "p.py");
    assert_eq!(one[0]["meta"]["barca.job"], "p.py:publish");
    assert_eq!(one[0]["meta"]["barca.target"], "publish");
    assert!(one[0].get("parent_id").is_none());

    let mut two = run("success", Vec::new());
    two.command = "get".to_string();
    two.targets = vec!["daily_revenue".to_string(), "orders".to_string()];
    assert_eq!(
        spans(&two, &HashSet::new())[0]["resource"],
        "get daily_revenue,orders"
    );

    let mut whole = run("success", Vec::new());
    whole.command = "get".to_string();
    whole.targets = Vec::new();
    whole.target = None;
    whole.job = "all".to_string();
    let payload = spans(&whole, &HashSet::new());
    assert_eq!(payload[0]["resource"], "get p.py");
    assert!(payload[0]["meta"].get("barca.targets").is_none());
    whole.files = Vec::new();
    assert_eq!(spans(&whole, &HashSet::new())[0]["resource"], "get all");
}

#[test]
fn a_step_is_named_by_its_node_and_carries_its_place_in_the_pipeline() {
    let mut orders = step("p.py:orders", StepOutcome::Cached);
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
    let payload = spans(&run("success", vec![orders]), &HashSet::new());
    let step = &payload[1];
    assert_eq!(step["name"], "barca.step");
    assert_eq!(step["resource"], "orders");
    assert_eq!(step["parent_id"], payload[0]["span_id"]);
    assert_eq!(step["span_id"], step_span_id("r1", "p.py:orders"));
    assert_eq!(step["error"], 0);
    let meta = &step["meta"];
    assert_eq!(meta["barca.asset"], "orders");
    assert_eq!(meta["barca.node_id"], "p.py:orders");
    assert_eq!(meta["barca.file"], "p.py");
    assert_eq!(meta["barca.group"], "Sales/Daily");
    assert_eq!(meta["barca.kind"], "asset");
    assert_eq!(meta["barca.freshness"], "schedule");
    assert_eq!(meta["barca.schedule"], "0 6 * * *");
    assert_eq!(meta["barca.outcome"], "cached");
    assert_eq!(meta["barca.cached"], "true");
    assert_eq!(meta["barca.run_hash"], "abc");
    assert_eq!(meta["barca.upstream"], "p.py:raw,p.py:rates");
    assert_eq!(meta["barca.digest"], "sha");
    assert_eq!(meta["barca.job"], "p.py:publish");
    assert!(meta.get("barca.partition").is_none());
    let metrics = &step["metrics"];
    assert_eq!(metrics["_dd.measured"], 1);
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
    let payload = spans(&run("failed", vec![first, later, skipped]), &HashSet::new());
    let root = &payload[0];
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

    let steps = named(&payload, "barca.step");
    let publish = steps.iter().find(|s| s["resource"] == "publish").unwrap();
    assert_eq!(publish["error"], 1);
    assert_eq!(publish["meta"]["barca.outcome"], "failed");
    assert_eq!(publish["meta"]["error.type"], "ValueError");
    let stack = publish["meta"]["error.stack"].as_str().unwrap();
    assert!(stack.starts_with("[cut]") && stack.ends_with("  File \"p.py\", line 3"));
    assert!(stack.chars().count() < MAX_STACK_CHARS + 20);
    assert!(publish["metrics"].get("barca.attempts").is_none());
    let report = steps.iter().find(|s| s["resource"] == "report").unwrap();
    assert_eq!(report["meta"]["barca.outcome"], "skipped");
    assert_eq!(report["error"], 0);
}

#[test]
fn a_cancelled_or_skipped_run_says_why() {
    let mut cancelled = run("cancelled", Vec::new());
    cancelled.reason = Some("server shutdown".to_string());
    let root = &spans(&cancelled, &HashSet::new())[0];
    assert_eq!(root["error"], 1);
    assert_eq!(root["meta"]["barca.reason"], "server shutdown");
    assert_eq!(root["meta"]["error.type"], "Cancelled");
    assert_eq!(root["meta"]["error.message"], "server shutdown");

    let mut skipped = run("skipped", Vec::new());
    skipped.reason = Some("in_flight".to_string());
    skipped.in_flight_run = Some("abc123".to_string());
    let payload = spans(&skipped, &HashSet::new());
    assert_eq!(payload.len(), 1);
    assert_eq!(payload[0]["error"], 0);
    assert_eq!(payload[0]["meta"]["barca.status"], "skipped");
    assert_eq!(payload[0]["meta"]["barca.reason"], "in_flight");
    assert_eq!(payload[0]["meta"]["barca.in_flight_run"], "abc123");
    assert!(payload[0]["meta"].get("error.type").is_none());

    let ok = &spans(&run("success", Vec::new()), &HashSet::new())[0];
    assert!(ok["meta"].get("barca.reason").is_none());
    assert!(ok["meta"].get("error.message").is_none());
    let broken = &spans(&run("failed", Vec::new()), &HashSet::new())[0];
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
fn a_partitioned_node_is_one_step_with_a_child_per_key_that_has_a_span() {
    let keys = partitioned(5, 3, 1);
    let traced: HashSet<String> = keys[..2].iter().map(|k| k.node_id.clone()).collect();
    let payload = spans(&run("failed", keys), &traced);
    let steps = named(&payload, "barca.step");
    assert_eq!(steps.len(), 1);
    let step = steps[0];
    assert_eq!(step["resource"], "weekly");
    assert_eq!(step["span_id"], step_span_id("r1", "p.py:weekly"));
    assert_eq!(step["error"], 1);
    assert_eq!(step["meta"]["barca.outcome"], "failed");
    assert_eq!(step["meta"]["barca.node_id"], "p.py:weekly");
    assert!(step["meta"].get("barca.partition").is_none());
    assert_eq!(
        step["meta"]["error.message"], "[week=w00003] bad week 3",
        "the one failed key names itself"
    );
    assert_eq!(step["metrics"]["barca.partitions.total"], 5);
    assert_eq!(step["metrics"]["barca.partitions.executed"], 3);
    assert_eq!(step["metrics"]["barca.partitions.cached"], 1);
    assert_eq!(step["metrics"]["barca.partitions.failed"], 1);
    assert_eq!(step["metrics"]["barca.bytes"], 42 * 5);
    // The span covers the keys that did work, not the cached ones.
    assert_eq!(step["start"], 1_000);
    assert_eq!(step["duration"], 30 + 100);

    // Two traced keys that ran, and the failed key, which was not traced.
    let keys = named(&payload, "barca.partition");
    assert_eq!(keys.len(), 3);
    for key in &keys {
        assert_eq!(key["resource"], "weekly", "no partition key in a resource");
        assert_eq!(key["parent_id"], step["span_id"]);
        assert_eq!(key["meta"]["barca.asset"], "weekly");
        let id = key["meta"]["barca.node_id"].as_str().unwrap();
        assert_eq!(key["span_id"], step_span_id("r1", id));
        assert!(key["metrics"].get("_dd.measured").is_none());
    }
    assert_eq!(keys[0]["meta"]["barca.partition"], "week=w00000");
    assert_eq!(keys[2]["meta"]["barca.partition"], "week=w00003");
    assert_eq!(keys[2]["error"], 1);
    assert_eq!(keys[2]["meta"]["error.message"], "bad week 3");
    assert!(keys[2]["meta"]["error.stack"].is_string());
    assert_eq!(
        payload[0]["meta"]["error.message"],
        "weekly[week=w00003]: bad week 3"
    );
    // Resources stay bounded however many keys there are.
    let resources: HashSet<&str> = payload
        .iter()
        .map(|s| s["resource"].as_str().unwrap())
        .collect();
    assert_eq!(resources, ["run publish", "weekly"].into());
}

#[test]
fn many_failed_keys_each_get_a_span_and_only_the_first_ones_carry_detail() {
    let keys = partitioned(20_000, 0, 20_000);
    let report = run("failed", keys);
    let bodies = datadog().bodies(&report, &HashSet::new());
    let all: Vec<Value> = bodies
        .iter()
        .flat_map(|b| {
            assert!(b.len() <= MAX_REQUEST_BYTES, "{} bytes", b.len());
            let chunk: Value = serde_json::from_slice(b).unwrap();
            let spans = chunk[0].as_array().unwrap().clone();
            // Each request is a chunk whose first span says how to keep the trace.
            assert_eq!(spans[0]["metrics"]["_sampling_priority_v1"], 2);
            assert!(spans[0]["meta"]["_dd.p.tid"].is_string());
            spans
        })
        .collect();
    let keys = named(&all, "barca.partition");
    assert_eq!(keys.len(), 20_000);
    let detailed = keys
        .iter()
        .filter(|k| k["meta"].get("error.stack").is_some());
    assert_eq!(detailed.count(), DETAILED_FAILURES_PER_NODE);
    let step = named(&all, "barca.step")[0];
    assert_eq!(step["metrics"]["barca.partitions.failed"], 20_000);
    assert!(
        step["meta"]["error.message"]
            .as_str()
            .unwrap()
            .starts_with("20000 of 20000 partitions failed; first [week=w00000] bad week 0")
    );
    assert!(bodies.len() > 1);
    let total: usize = bodies.iter().map(Vec::len).sum();
    assert!(total < 25 << 20, "{total} bytes");
}

#[test]
fn key_spans_are_capped_per_node_and_per_run_and_remembered_for_retries() {
    let run_id = "cap-test";
    let claimed = (0..KEY_SPANS_PER_NODE + 50)
        .filter(|i| claim_key_span(run_id, "p.py:a", &format!("p.py:a[k={i}]")))
        .count();
    assert_eq!(claimed, KEY_SPANS_PER_NODE);
    assert!(
        claim_key_span(run_id, "p.py:a", "p.py:a[k=0]"),
        "a retry keeps its span"
    );
    assert!(!claim_key_span(run_id, "p.py:a", "p.py:a[k=999]"));
    let mut total = KEY_SPANS_PER_NODE;
    for node in 0.. {
        let base = format!("p.py:n{node}");
        let got = (0..KEY_SPANS_PER_NODE)
            .filter(|i| claim_key_span(run_id, &base, &format!("{base}[k={i}]")))
            .count();
        total += got;
        if got < KEY_SPANS_PER_NODE {
            break;
        }
    }
    assert_eq!(total, KEY_SPANS_PER_RUN);
    // A line about a key with a span joins that span; about one without, the node's.
    assert_eq!(
        log_span_id(run_id, Some("p.py:a[k=0]")),
        step_span_id(run_id, "p.py:a[k=0]")
    );
    assert_eq!(
        log_span_id(run_id, Some("p.py:a[k=999]")),
        step_span_id(run_id, "p.py:a")
    );
    assert_eq!(
        log_span_id(run_id, Some("p.py:b")),
        step_span_id(run_id, "p.py:b")
    );
    assert_eq!(log_span_id(run_id, None), run_span_id(run_id));
    let taken = take_key_spans(run_id);
    assert_eq!(taken.len(), KEY_SPANS_PER_RUN);
    assert!(take_key_spans(run_id).is_empty());
}

#[test]
fn a_worker_gets_the_parent_resource_and_tags_of_its_step() {
    let nodes: HashMap<String, Arc<NodeContext>> = [
        ("p.py:orders".to_string(), node("p.py:orders")),
        ("p.py:weekly".to_string(), node("p.py:weekly")),
    ]
    .into();
    let dispatch = Dispatch {
        service: "planning".to_string(),
        project: Some("shop".to_string()),
        job: "p.py:orders".to_string(),
        nodes,
    };
    let run_id = "dispatch-test";
    let ctx = dispatch
        .context(run_id, &crate::StepId::parse("p.py:orders"), 2)
        .unwrap();
    assert_eq!(ctx["trace_id"], trace_id(run_id));
    assert_eq!(ctx["trace_id_high"], trace_id_high(run_id));
    assert_eq!(ctx["parent_id"], step_span_id(run_id, "p.py:orders"));
    assert_eq!(ctx["resource"], "orders");
    assert_eq!(ctx["service"], "planning");
    assert_eq!(ctx["attempt"], 2);
    assert_eq!(ctx["tags"]["barca.asset"], "orders");
    assert_eq!(ctx["tags"]["barca.node_id"], "p.py:orders");
    assert_eq!(ctx["tags"]["barca.version"], BARCA_VERSION);
    assert_eq!(ctx["tags"]["barca.project"], "shop");
    assert!(ctx["tags"].get("barca.partition").is_none());

    let key = crate::StepId::parse("p.py:weekly[week=w1]");
    let ctx = dispatch.context(run_id, &key, 1).unwrap();
    assert_eq!(
        ctx["parent_id"],
        step_span_id(run_id, "p.py:weekly[week=w1]")
    );
    assert_eq!(ctx["resource"], "weekly");
    assert_eq!(ctx["tags"]["barca.partition"], "week=w1");
    for i in 0..KEY_SPANS_PER_NODE {
        dispatch.context(
            run_id,
            &crate::StepId::parse(&format!("p.py:weekly[week=x{i}]")),
            1,
        );
    }
    let past_cap = crate::StepId::parse("p.py:weekly[week=late]");
    assert!(dispatch.context(run_id, &past_cap, 1).is_none());
    take_key_spans(run_id);
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
