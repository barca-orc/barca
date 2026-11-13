use super::*;

fn spec(name: &str) -> ItemSpec {
    ItemSpec {
        fn_ref: format!("test.py:{name}"),
        function_name: name.to_string(),
        source_file: "test.py".to_string(),
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

fn dummy_step_id(name: &str) -> barca_model::StepId {
    barca_model::StepId {
        base: std::sync::Arc::from(format!("test.py:{name}")),
        partition: barca_model::model::PartitionKey(std::collections::BTreeMap::new()),
    }
}

#[test]
fn item_with_no_deps_goes_to_ready() {
    let mut c = Coordinator::new();
    let id = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    assert_eq!(c.ready_count(), 1);
    assert_eq!(c.next_ready(), Some(id));
    assert_eq!(c.ready_count(), 0);
}

#[test]
fn item_with_deps_goes_to_pending() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let _b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    assert_eq!(c.ready_count(), 1); // only a
    assert_eq!(c.pending_count(), 1); // b is pending
}

#[test]
fn completion_unblocks_dependents() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    assert_eq!(c.next_ready(), Some(a));
    c.on_item_completed(a);
    assert_eq!(c.next_ready(), Some(b));
}

#[test]
fn chain_executes_in_order() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    let d = c.add_item(
        dummy_step_id("c"),
        spec("c"),
        vec![Dep {
            upstream: b,
            kind: DepKind::Data,
        }],
    );

    assert_eq!(c.next_ready(), Some(a));
    c.on_item_completed(a);
    assert_eq!(c.next_ready(), Some(b));
    c.on_item_completed(b);
    assert_eq!(c.next_ready(), Some(d));
    c.on_item_completed(d);
    assert!(c.is_finished());
}

#[test]
fn fan_out_all_ready_at_once() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(dummy_step_id("b"), spec("b"), vec![]);
    let d = c.add_item(dummy_step_id("c"), spec("c"), vec![]);
    assert_eq!(c.ready_count(), 3);
    // All three are ready
    let ids: HashSet<_> = [c.next_ready(), c.next_ready(), c.next_ready()]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(ids, HashSet::from([a, b, d]));
}

#[test]
fn failure_cascades_through_data_deps() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let _b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    c.next_ready(); // pop a
    c.on_item_failed(a, "boom".into(), std::time::Duration::ZERO);
    assert!(c.is_finished()); // b is skipped
    assert_eq!(c.skipped_items().len(), 1);
}

#[test]
fn ordering_dep_satisfied_by_failure() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Ordering,
        }],
    );
    c.next_ready(); // pop a
    c.on_item_failed(a, "boom".into(), std::time::Duration::ZERO);
    // b should be unblocked (ordering dep satisfied by failure)
    assert_eq!(c.next_ready(), Some(b));
}

#[test]
fn parallel_creates_children_in_ready_queue() {
    let mut c = Coordinator::new();
    let parent = c.add_item(dummy_step_id("parent"), spec("parent"), vec![]);
    c.next_ready(); // pop parent, mark executing

    let (group_id, children) = c.on_parallel_requested(
        parent,
        vec![spec("child_0"), spec("child_1"), spec("child_2")],
    );

    assert_eq!(children.len(), 3);
    assert_eq!(c.ready_count(), 3);
    assert!(!c.is_group_complete(group_id));
}

/// #333: when the step that called parallel() is gone, its branches that have not
/// started never start, the group counts as resolved once the running ones are done,
/// and the run can finish.
#[test]
fn an_abandoned_group_drops_the_branches_that_have_not_started() {
    let mut c = Coordinator::new();
    let parent = c.add_item(
        barca_model::StepId::unpartitioned("p"),
        spec("p"),
        Vec::new(),
    );
    assert_eq!(c.next_ready(), Some(parent));
    let (group, children) =
        c.on_parallel_requested(parent, vec![spec("c0"), spec("c1"), spec("c2")]);
    let running = c.next_ready().unwrap();
    assert_eq!(running, children[0]);

    c.abandon_group(group);
    assert_eq!(
        c.ready_count(),
        0,
        "the two that had not started are dropped"
    );
    assert!(!c.is_group_complete(group), "one branch is still running");
    c.on_item_failed(
        parent,
        "worker disconnected".to_string(),
        std::time::Duration::ZERO,
    );
    assert!(!c.is_finished());
    c.on_item_completed(running);
    assert!(c.is_group_complete(group));
    assert!(c.is_finished());
}

#[test]
fn parallel_group_completes_when_all_children_done() {
    let mut c = Coordinator::new();
    let parent = c.add_item(dummy_step_id("parent"), spec("parent"), vec![]);
    c.next_ready();

    let (group_id, children) = c.on_parallel_requested(parent, vec![spec("c0"), spec("c1")]);

    let c0 = c.next_ready().unwrap();
    let c1 = c.next_ready().unwrap();
    assert_eq!(HashSet::from([c0, c1]), HashSet::from_iter(children));

    c.on_item_completed(c0);
    assert!(!c.is_group_complete(group_id));
    c.on_item_completed(c1);
    assert!(c.is_group_complete(group_id));
}

#[test]
fn retry_pushes_back_to_ready() {
    let mut c = Coordinator::new();
    let mut s = spec("a");
    s.retries = 3; // 3 attempts total
    let a = c.add_item(dummy_step_id("a"), s, vec![]);
    c.next_ready();
    c.on_item_failed(a, "fail 1".into(), std::time::Duration::ZERO);
    // Should be back in ready (attempt 1 of 3)
    assert_eq!(c.ready_count(), 1);
    assert_eq!(c.next_ready(), Some(a));
}

#[test]
fn retry_exhausted_marks_failed() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]); // retries=1 → no retry
    c.next_ready();
    assert_eq!(
        c.on_item_failed(a, "boom".into(), std::time::Duration::ZERO),
        FailureAction::Failed
    );
    assert_eq!(c.failed_items().len(), 1);
    assert!(c.is_finished());
}

#[test]
fn failure_clock_spans_first_attempt_to_permanent_failure() {
    use std::time::Duration;
    let mut c = Coordinator::new();
    let mut s = spec("a");
    s.retries = 2;
    let a = c.add_item(dummy_step_id("a"), s, vec![]);

    c.next_ready();
    // The first attempt ran for 5 s; it is not yet permanent, so no clock.
    c.on_item_failed(a, "fail 1".into(), Duration::from_secs(5));
    assert_eq!(c.failure_clock(a), None);
    c.next_ready();
    let (started, wall) = match c.on_item_failed(a, "fail 2".into(), Duration::from_secs(3)) {
        FailureAction::Failed => c.failure_clock(a).unwrap(),
        other => panic!("expected a permanent failure, got {other:?}"),
    };
    // The wall time runs from the first attempt's start, so it covers both attempts.
    assert!(wall >= Duration::from_secs(5), "{wall:?}");
    assert!(wall < Duration::from_secs(6), "{wall:?}");
    let ago = std::time::SystemTime::now()
        .duration_since(started)
        .unwrap();
    assert!(
        ago >= Duration::from_secs(5) && ago < Duration::from_secs(6),
        "{ago:?}"
    );
}

#[test]
fn attempts_count_dispatches() {
    let mut c = Coordinator::new();
    let mut s = spec("a");
    s.retries = 3;
    let a = c.add_item(dummy_step_id("a"), s, vec![]);

    c.next_ready();
    assert_eq!(c.item(a).attempts, 1);
    assert_eq!(
        c.on_item_failed(a, "fail 1".into(), std::time::Duration::ZERO),
        FailureAction::RetryNow
    );
    c.next_ready();
    assert_eq!(c.item(a).attempts, 2);
    assert_eq!(
        c.on_item_failed(a, "fail 2".into(), std::time::Duration::ZERO),
        FailureAction::RetryNow
    );
    c.next_ready();
    assert_eq!(c.item(a).attempts, 3);
    // A success after two failures records 3 total attempts.
    c.on_item_completed(a);
    assert_eq!(c.item(a).attempts, 3);
    assert!(c.is_finished());
}

#[test]
fn backoff_parks_item_until_requeue() {
    let mut c = Coordinator::new();
    let mut s = spec("a");
    s.retries = 3;
    s.retry_backoff_seconds = 0.2;
    let a = c.add_item(dummy_step_id("a"), s, vec![]);

    c.next_ready();
    // delay = backoff * attempt: 0.2 * 1 after the first failure.
    assert_eq!(
        c.on_item_failed(a, "fail 1".into(), std::time::Duration::ZERO),
        FailureAction::RetryAfter(std::time::Duration::from_secs_f64(0.2)),
    );
    // Parked: not ready, not finished, until the caller requeues.
    assert_eq!(c.ready_count(), 0);
    assert!(!c.is_finished());
    c.requeue(a);
    assert_eq!(c.next_ready(), Some(a));

    // Second failure: delay = 0.2 * 2.
    assert_eq!(
        c.on_item_failed(a, "fail 2".into(), std::time::Duration::ZERO),
        FailureAction::RetryAfter(std::time::Duration::from_secs_f64(0.4)),
    );
    c.requeue(a);
    assert_eq!(c.next_ready(), Some(a));

    // Third failure exhausts the budget regardless of backoff.
    assert_eq!(
        c.on_item_failed(a, "fail 3".into(), std::time::Duration::ZERO),
        FailureAction::Failed
    );
    assert!(c.is_finished());
}

#[test]
fn return_leased_restores_queue_front_and_refunds_attempt() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(dummy_step_id("b"), spec("b"), vec![]);

    // Lease both (a batch of two), as a worker pull would.
    assert_eq!(c.next_ready(), Some(a));
    assert_eq!(c.next_ready(), Some(b));
    assert_eq!(c.item(b).attempts, 1);

    // Worker dies mid-batch: `b` was never started — return it.
    c.return_leased(b);
    assert_eq!(c.item(b).attempts, 0, "unstarted dispatch must be refunded");
    // It comes back at the front, ahead of anything queued later.
    let d = c.add_item(dummy_step_id("d"), spec("d"), vec![]);
    assert_eq!(c.next_ready(), Some(b));
    assert_eq!(c.next_ready(), Some(d));
    c.on_item_completed(a);
    c.on_item_completed(b);
    c.on_item_completed(d);
    assert!(c.is_finished());
}

#[test]
fn return_leased_of_unleased_item_is_noop() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    c.return_leased(a); // never leased — must not duplicate or underflow
    assert_eq!(c.ready_count(), 1);
    assert_eq!(c.next_ready(), Some(a));
    assert_eq!(c.next_ready(), None);
    assert_eq!(c.item(a).attempts, 1);
}

#[test]
fn remaining_count_tracks_ready_and_pending() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let _b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    assert_eq!(c.remaining_count(), 2);
    c.next_ready();
    assert_eq!(c.remaining_count(), 1); // a leased, b pending
    c.on_item_completed(a);
    assert_eq!(c.remaining_count(), 1); // b now ready
    c.next_ready();
    assert_eq!(c.remaining_count(), 0);
}

#[test]
fn requeue_of_unparked_item_is_noop() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    c.requeue(a); // a is in ready (never parked) — must not duplicate
    assert_eq!(c.ready_count(), 1);
    assert_eq!(c.next_ready(), Some(a));
    assert_eq!(c.next_ready(), None);
}

#[test]
fn diamond_dependency() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    let d = c.add_item(
        dummy_step_id("c"),
        spec("c"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    let e = c.add_item(
        dummy_step_id("d"),
        spec("d"),
        vec![
            Dep {
                upstream: b,
                kind: DepKind::Data,
            },
            Dep {
                upstream: d,
                kind: DepKind::Data,
            },
        ],
    );

    // a is ready
    assert_eq!(c.next_ready(), Some(a));
    c.on_item_completed(a);
    // b and c are now ready
    assert_eq!(c.ready_count(), 2);
    let r1 = c.next_ready().unwrap();
    let r2 = c.next_ready().unwrap();
    assert_eq!(HashSet::from([r1, r2]), HashSet::from([b, d]));
    c.on_item_completed(r1);
    assert_eq!(c.ready_count(), 0); // d still waiting
    c.on_item_completed(r2);
    assert_eq!(c.next_ready(), Some(e));
    c.on_item_completed(e);
    assert!(c.is_finished());
}

#[test]
fn is_finished_when_all_terminal() {
    let mut c = Coordinator::new();
    let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]);
    let b = c.add_item(
        dummy_step_id("b"),
        spec("b"),
        vec![Dep {
            upstream: a,
            kind: DepKind::Data,
        }],
    );
    assert!(!c.is_finished());
    c.next_ready();
    c.on_item_completed(a);
    assert!(!c.is_finished());
    c.next_ready();
    c.on_item_completed(b);
    assert!(c.is_finished());
}

#[test]
fn ten_thousand_items_no_panic() {
    let mut c = Coordinator::new();
    let mut prev = c.add_item(dummy_step_id("start"), spec("start"), vec![]);
    for i in 1..10_000 {
        prev = c.add_item(
            dummy_step_id(&format!("step_{i}")),
            spec(&format!("step_{i}")),
            vec![Dep {
                upstream: prev,
                kind: DepKind::Data,
            }],
        );
    }
    // Drain all
    let mut count = 0;
    loop {
        match c.next_ready() {
            Some(id) => {
                c.on_item_completed(id);
                count += 1;
            }
            None => {
                if c.is_finished() {
                    break;
                }
            }
        }
    }
    assert_eq!(count, 10_000);
}

/// One stream of partitioned steps over `keys`; each `(name, upstream)` step reads
/// `upstream`'s aligned key when it has one.
fn partition_stream(
    keys: &[String],
    steps: &[(&str, Option<&str>)],
) -> barca_compile::planner::Phase {
    use barca_compile::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
    use barca_model::{PartitionKey, StepId};
    let pks: Vec<PartitionKey> = keys
        .iter()
        .map(|k| PartitionKey::from(HashMap::from([("k".to_string(), k.clone())])))
        .collect();
    let steps = steps
        .iter()
        .map(|&(name, upstream)| StreamStep {
            step_id: StepId::unpartitioned(name),
            kind: barca_model::NodeKind::Asset,
            function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
            source_file: std::sync::Arc::from("f"),
            inputs: upstream
                .map(|u| HashMap::from([("data".to_string(), u.to_string())]))
                .unwrap_or_default(),
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes: HashMap::new(),
            timeout_seconds: 300,
            retries: 0,
            weight: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: pks.clone(),
            param_types: HashMap::new(),
            return_type: None,
            keyed: Default::default(),
        })
        .collect();
    Phase {
        reason: PhaseReason::Initial,
        streams: vec![WorkerStream {
            stream_id: "w0".to_string(),
            steps,
        }],
    }
}

fn keys(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{i:05}")).collect()
}

fn id_of(c: &Coordinator, display: &str) -> ItemId {
    c.items
        .values()
        .find(|it| it.step_id.display() == display)
        .unwrap_or_else(|| panic!("{display} not loaded"))
        .id
}

/// Two streams: `f:a` and `f:b`, `f:b[k]` reading `f:a[k]`; `f:b`'s keys in one call.
fn call_phase(n: usize, retries: u32, backoff: f64) -> (Coordinator, Vec<String>) {
    let ks = keys(n);
    let mut phase = partition_stream(&ks, &[("f:a", None)]);
    let mut b = partition_stream(&ks, &[("f:b", Some("f:a"))]);
    for st in &mut b.streams[0].steps {
        st.retries = retries;
        st.retry_backoff_seconds = backoff;
    }
    b.streams[0].stream_id = "w1".to_string();
    phase.streams.push(b.streams.remove(0));
    let mut c = Coordinator::new();
    let members: Vec<String> = ks.iter().map(|k| format!("f:b[k={k}]")).collect();
    c.assign_calls(vec![("b1".to_string(), members.clone())]);
    c.load_phase(&phase, &HashMap::new());
    (c, members)
}

fn drain(c: &mut Coordinator) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(id) = c.next_ready() {
        out.push(c.item(id).step_id.display());
    }
    out
}

/// RFC-0013: the keys of a call are queued together, once every one of them can run.
#[test]
fn a_call_is_released_when_all_its_keys_are_ready() {
    let (mut c, members) = call_phase(3, 1, 0.0);
    let a: Vec<String> = drain(&mut c);
    assert_eq!(a.len(), 3);
    assert!(a.iter().all(|d| d.starts_with("f:a")));
    for (i, d) in a.iter().enumerate() {
        let id = id_of(&c, d);
        c.on_item_completed(id);
        if i < 2 {
            assert_eq!(c.ready_count(), 0, "released before every key was ready");
        }
    }
    let b = drain(&mut c);
    assert_eq!(b, members);
    assert_eq!(c.call_id(id_of(&c, &b[0])), Some("b1"));
    assert!(c.same_call(id_of(&c, &b[0]), id_of(&c, &b[2])));
}

#[test]
fn a_key_that_will_never_run_does_not_hold_back_its_call() {
    let (mut c, _) = call_phase(3, 1, 0.0);
    let a = drain(&mut c);
    let first = id_of(&c, &a[0]);
    assert_eq!(
        c.on_item_failed(first, "boom".into(), std::time::Duration::ZERO),
        FailureAction::Failed
    );
    for d in &a[1..] {
        let id = id_of(&c, d);
        c.on_item_completed(id);
    }
    assert_eq!(drain(&mut c), ["f:b[k=00001]", "f:b[k=00002]"]);
}

/// A retry reruns only the keys that failed, together, once the attempt is over.
#[test]
fn the_failed_keys_of_a_call_are_retried_together() {
    for backoff in [0.0, 1.0] {
        let (mut c, members) = call_phase(3, 2, backoff);
        for d in drain(&mut c) {
            let id = id_of(&c, &d);
            c.on_item_completed(id);
        }
        let b = drain(&mut c);
        assert_eq!(b, members);
        let ids: Vec<ItemId> = b.iter().map(|d| id_of(&c, d)).collect();
        let action = c.on_item_failed(ids[0], "x".into(), std::time::Duration::ZERO);
        assert_eq!(c.ready_count(), 0, "retried before the attempt was over");
        c.on_item_completed(ids[1]);
        c.on_item_failed(ids[2], "x".into(), std::time::Duration::ZERO);
        if backoff > 0.0 {
            assert!(matches!(action, FailureAction::RetryAfter(_)));
            assert_eq!(c.ready_count(), 0);
            c.requeue(ids[0]);
            assert_eq!(c.ready_count(), 0, "retried before every backoff passed");
            c.requeue(ids[2]);
        }
        assert_eq!(drain(&mut c), [members[0].clone(), members[2].clone()]);
    }
}

#[test]
fn failed_calls_split_without_configured_retries_and_stop_at_single_keys() {
    let (mut c, _) = call_phase(4, 0, 0.0);
    for d in drain(&mut c) {
        let id = id_of(&c, &d);
        c.on_item_completed(id);
    }
    for expected_size in [2, 1] {
        let ids: Vec<_> = drain(&mut c).iter().map(|d| id_of(&c, d)).collect();
        assert_eq!(ids.len(), 4);
        for id in &ids {
            assert_eq!(
                c.on_item_failed(*id, "OOM".into(), std::time::Duration::ZERO),
                FailureAction::RetryNow
            );
        }
        for id in &ids {
            assert_eq!(
                ids.iter().filter(|other| c.same_call(*id, **other)).count(),
                expected_size
            );
        }
    }
    for d in drain(&mut c) {
        let id = id_of(&c, &d);
        assert_eq!(
            c.on_item_failed(id, "OOM".into(), std::time::Duration::ZERO),
            FailureAction::Failed
        );
    }
    assert_eq!(c.ready_count(), 0);
}

/// #345: each key of a step used to wait on every key of the step before it in its stream
/// through its own edge, K × K edges per pair of steps, which made a cold 20,000-key chain
/// take minutes and gigabytes before the first step ran. The edges must grow with K.
#[test]
fn partition_chain_edges_grow_linearly_with_keys() {
    let chain = [
        ("f:fetch", None),
        ("f:normalize", Some("f:fetch")),
        ("f:score", Some("f:normalize")),
    ];
    let edges = |n: usize| {
        let mut c = Coordinator::new();
        assert_eq!(
            c.load_phase(&partition_stream(&keys(n), &chain), &HashMap::new()),
            3 * n
        );
        c.edge_count()
    };
    let (small, large) = (edges(500), edges(5_000));
    assert!(large <= 8 * 5_000, "{large} edges for 5,000 keys");
    assert!(
        large <= 11 * small,
        "edges grew from {small} to {large} for ten times the keys"
    );
}

/// The gate keeps the stream's chain ordering: a key of the next step starts only once
/// every key of the previous step has finished one way or another, and a failed key still
/// skips only the keys that read it.
#[test]
fn partitions_wait_for_the_whole_previous_step_in_their_stream() {
    let phase = partition_stream(
        &keys(3),
        &[("f:a", None), ("f:b", Some("f:a")), ("f:c", None)],
    );
    let mut c = Coordinator::new();
    c.load_phase(&phase, &HashMap::new());
    let a = |k: usize| id_of(&c, &format!("f:a[k={k:05}]"));
    let b = |k: usize| id_of(&c, &format!("f:b[k={k:05}]"));
    let (a0, a1, a2, b0, b1, b2) = (a(0), a(1), a(2), b(0), b(1), b(2));
    assert_eq!(c.ready_count(), 3);
    for _ in 0..3 {
        c.next_ready().unwrap();
    }

    c.on_item_completed(a0);
    c.on_item_completed(a1);
    assert_eq!(c.ready_count(), 0, "b waits for every key of a");
    assert_eq!(
        c.on_item_failed(a2, "boom".into(), std::time::Duration::ZERO),
        FailureAction::Failed
    );
    let mut ready: Vec<ItemId> = std::iter::from_fn(|| c.next_ready()).collect();
    ready.sort_by_key(|id| id.0);
    assert_eq!(ready, vec![b0, b1]);
    assert!(c.skipped_items().contains(&b2));

    c.on_item_completed(b0);
    assert_eq!(
        c.ready_count(),
        0,
        "c waits for b[1] although it reads nothing"
    );
    c.on_item_completed(b1);
    assert_eq!(c.ready_count(), 3);
    while let Some(id) = c.next_ready() {
        c.on_item_completed(id);
    }
    assert!(c.is_finished());
}

/// #461: skipping a failed key's dependent used to scan the whole ready queue, so half of
/// N keys failing while the other half waited cost about N²/4 queue touches. Counts queue
/// operations, not wall time, and checks the skipped set: exactly the keys that read a
/// failed key.
#[test]
fn skipping_dependents_of_many_failed_keys_is_linear_in_the_keys() {
    let ops = |n: usize| {
        let phase = partition_stream(&keys(n), &[("f:a", None), ("f:b", Some("f:a"))]);
        let mut c = Coordinator::new();
        assert_eq!(c.load_phase(&phase, &HashMap::new()), 2 * n);
        // Lease the first half of a's keys and fail them while the second half is queued.
        let leased: Vec<ItemId> = (0..n / 2).map(|_| c.next_ready().unwrap()).collect();
        assert_eq!(c.ready_count(), n - n / 2);
        for &id in &leased {
            assert_eq!(
                c.on_item_failed(id, "boom".into(), std::time::Duration::ZERO),
                FailureAction::Failed
            );
        }
        assert_eq!(c.ready_count(), n - n / 2, "only b's keys are skipped");
        while let Some(id) = c.next_ready() {
            c.on_item_completed(id);
        }
        assert!(c.is_finished());

        let display = |id: &ItemId| c.item(*id).step_id.display();
        let mut failed: Vec<String> = leased.iter().map(display).collect();
        failed.sort();
        let mut skipped: Vec<String> = c.skipped_items().iter().map(display).collect();
        skipped.sort();
        let expected: Vec<String> = failed.iter().map(|a| a.replacen("f:a", "f:b", 1)).collect();
        assert_eq!(
            skipped, expected,
            "a failed key skips only the key that reads it"
        );
        assert_eq!(c.done_count(), 2 * (n - n / 2));
        c.ready.ops
    };
    let (small, large) = (ops(1_000), ops(10_000));
    assert!(
        large <= 5 * 10_000,
        "{large} queue operations for 10,000 keys"
    );
    assert!(
        large <= 11 * small,
        "queue operations grew from {small} to {large} for ten times the keys"
    );
}

#[test]
fn a_recorded_output_is_found_by_its_display_id() {
    let mut c = Coordinator::new();
    c.load_phase(
        &partition_stream(&keys(2), &[("f:a", None)]),
        &HashMap::new(),
    );
    let a1 = id_of(&c, "f:a[k=00001]");
    c.next_ready();
    c.next_ready();
    c.on_item_completed(a1);
    c.record_output(a1, serde_json::json!({"path": "/x"}));
    assert_eq!(
        c.output_for_display("f:a[k=00001]"),
        Some(&serde_json::json!({"path": "/x"}))
    );
    assert_eq!(c.output_for_display("f:a[k=00000]"), None);
}

#[test]
fn load_phase_assigns_run_hashes_per_item_and_partition() {
    use barca_compile::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
    use barca_model::{PartitionKey, StepId};

    let pk_a = PartitionKey::from(HashMap::from([("t".to_string(), "A".to_string())]));
    let pk_b = PartitionKey::from(HashMap::from([("t".to_string(), "B".to_string())]));

    let mut plain_hashes = HashMap::new();
    plain_hashes.insert("f:plain".to_string(), "hash-plain".to_string());
    let mut part_hashes = HashMap::new();
    part_hashes.insert("f:part[t=A]".to_string(), "hash-a".to_string());
    part_hashes.insert("f:part[t=B]".to_string(), "hash-b".to_string());

    let mk = |name: &str, hashes: HashMap<String, String>, pks: Vec<PartitionKey>| StreamStep {
        step_id: StepId::unpartitioned(name),
        kind: barca_model::NodeKind::Asset,
        function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
        source_file: std::sync::Arc::from("f"),
        inputs: HashMap::new(),
        pending_partitions: HashMap::new(),
        serializer: None,
        sinks: vec![],
        run_hashes: hashes,
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        partition_keys: pks,
        param_types: HashMap::new(),
        return_type: None,
        keyed: Default::default(),
    };

    let phase = Phase {
        reason: PhaseReason::Initial,
        streams: vec![WorkerStream {
            stream_id: "w0".to_string(),
            steps: vec![
                mk("f:plain", plain_hashes, vec![]),
                mk("f:part", part_hashes, vec![pk_a, pk_b]),
            ],
        }],
    };

    let mut c = Coordinator::new();
    let loaded = c.load_phase(&phase, &HashMap::new());
    assert_eq!(loaded, 3);

    let mut seen: Vec<(String, Option<String>)> = Vec::new();
    while let Some(id) = c.next_ready() {
        let item = c.item(id);
        seen.push((item.step_id.display(), item.spec.run_hash.clone()));
        c.on_item_completed(id);
    }
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("f:part[t=A]".to_string(), Some("hash-a".to_string())),
            ("f:part[t=B]".to_string(), Some("hash-b".to_string())),
            ("f:plain".to_string(), Some("hash-plain".to_string())),
        ]
    );
}

/// Regression test for a bug where a partitioned consumer's data dependency
/// on its partitioned producer is silently dropped when the producer's
/// partition instance is loaded into the coordinator *after* the
/// consumer's — which is exactly what `dispatch::expand_pending_partitions`'s
/// load-balancing bin-packing can produce for dynamic (`partitions_from`)
/// partitions, since it distributes partition-key chunks across streams by
/// size alone, with no awareness of cross-step data dependencies.
///
/// `load_phase` wires each partitioned item's deps by looking up its
/// upstream in `node_to_item`, which is only populated for items already
/// visited by the `for stream { for step { .. } }` walk — so stream order
/// must never matter for correctness, only for scheduling. This phase
/// deliberately interleaves `fetch`/`enrich` partition instances across
/// streams so that two of the three `enrich` instances are loaded *before*
/// the `fetch` instance they depend on.
#[test]
fn load_phase_wires_partition_aligned_deps_regardless_of_stream_order() {
    use barca_compile::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
    use barca_model::{PartitionKey, StepId};

    let pk = |k: &str| PartitionKey::from(HashMap::from([("t".to_string(), k.to_string())]));

    let mk = |name: &str, inputs: HashMap<String, String>, pks: Vec<PartitionKey>| StreamStep {
        step_id: StepId::unpartitioned(name),
        kind: barca_model::NodeKind::Asset,
        function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
        source_file: std::sync::Arc::from("f"),
        inputs,
        pending_partitions: HashMap::new(),
        serializer: None,
        sinks: vec![],
        run_hashes: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        partition_keys: pks,
        param_types: HashMap::new(),
        return_type: None,
        keyed: Default::default(),
    };

    let no_inputs = HashMap::new();
    let enrich_inputs = HashMap::from([("data".to_string(), "f:fetch".to_string())]);

    // Interleaved like a real bin-packing result: enrich[B] and enrich[C]
    // are loaded in streams that precede the stream loading fetch[B] and
    // fetch[C] respectively. Only enrich[A] follows fetch[A].
    let phase = Phase {
        reason: PhaseReason::Initial,
        streams: vec![
            WorkerStream {
                stream_id: "w0".to_string(),
                steps: vec![
                    mk("f:fetch", no_inputs.clone(), vec![pk("A")]),
                    mk("f:enrich", enrich_inputs.clone(), vec![pk("B")]),
                ],
            },
            WorkerStream {
                stream_id: "w1".to_string(),
                steps: vec![
                    mk("f:fetch", no_inputs.clone(), vec![pk("B")]),
                    mk("f:enrich", enrich_inputs.clone(), vec![pk("C")]),
                ],
            },
            WorkerStream {
                stream_id: "w2".to_string(),
                steps: vec![
                    mk("f:fetch", no_inputs, vec![pk("C")]),
                    mk("f:enrich", enrich_inputs, vec![pk("A")]),
                ],
            },
        ],
    };

    let mut c = Coordinator::new();
    let loaded = c.load_phase(&phase, &HashMap::new());
    assert_eq!(loaded, 6);

    for k in ["A", "B", "C"] {
        let fetch_id = format!("f:fetch[t={k}]");
        let enrich_id = format!("f:enrich[t={k}]");
        let fetch_item = c
            .items
            .values()
            .find(|it| it.step_id.display() == fetch_id)
            .unwrap_or_else(|| panic!("{fetch_id} not loaded"));
        let enrich_item = c
            .items
            .values()
            .find(|it| it.step_id.display() == enrich_id)
            .unwrap_or_else(|| panic!("{enrich_id} not loaded"));

        assert!(
            enrich_item
                .deps
                .iter()
                .any(|d| d.upstream == fetch_item.id && d.kind == DepKind::Data),
            "{enrich_id} is missing its Data dependency on {fetch_id} — \
                 stream order must not affect partition-aligned dependency wiring"
        );
    }
}

/// Regression test for #170: a partitioned step whose upstream is UNpartitioned and loaded
/// in the same phase must resolve that input to the upstream's own id. `load_phase` used to
/// point every partitioned step's `upstream_inputs` at the partition-aligned id
/// (`f:multiplier[k=a]`), which no item ever produces, so the worker was called without the
/// argument (`TypeError: leaf() missing 1 required positional argument: 'm'`).
#[test]
fn load_phase_resolves_unpartitioned_upstream_of_partitioned_step() {
    use barca_compile::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
    use barca_model::{PartitionKey, StepId};

    let pk = |k: &str| PartitionKey::from(HashMap::from([("k".to_string(), k.to_string())]));
    let mk = |name: &str, inputs: HashMap<String, String>, pks: Vec<PartitionKey>| StreamStep {
        step_id: StepId::unpartitioned(name),
        kind: barca_model::NodeKind::Asset,
        function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
        source_file: std::sync::Arc::from("f"),
        inputs,
        pending_partitions: HashMap::new(),
        serializer: None,
        sinks: vec![],
        run_hashes: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        partition_keys: pks,
        param_types: HashMap::new(),
        return_type: None,
        keyed: Default::default(),
    };

    // Unpartitioned producer + partitioned producer + partitioned consumer of both.
    let consumer_inputs = HashMap::from([
        ("m".to_string(), "f:multiplier".to_string()),
        ("f".to_string(), "f:fetch".to_string()),
    ]);
    let phase = Phase {
        reason: PhaseReason::Initial,
        streams: vec![WorkerStream {
            stream_id: "w0".to_string(),
            steps: vec![
                mk("f:multiplier", HashMap::new(), vec![]),
                mk("f:fetch", HashMap::new(), vec![pk("a"), pk("b")]),
                mk("f:leaf", consumer_inputs, vec![pk("a"), pk("b")]),
            ],
        }],
    };

    let mut c = Coordinator::new();
    assert_eq!(c.load_phase(&phase, &HashMap::new()), 5);

    let id_of = |c: &Coordinator, display: &str| {
        c.items
            .values()
            .find(|it| it.step_id.display() == display)
            .unwrap_or_else(|| panic!("{display} not loaded"))
            .id
    };
    let multiplier = id_of(&c, "f:multiplier");
    for k in ["a", "b"] {
        let leaf = c.item(id_of(&c, &format!("f:leaf[k={k}]")));
        assert_eq!(
            leaf.spec.upstream_inputs.get("m").map(String::as_str),
            Some("f:multiplier"),
            "leaf[k={k}] must read the unpartitioned multiplier's own artifact"
        );
        assert_eq!(
            leaf.spec.upstream_inputs.get("f").cloned(),
            Some(format!("f:fetch[k={k}]")),
            "leaf[k={k}] must read its own partition of fetch"
        );
        assert!(
            leaf.deps
                .iter()
                .any(|d| d.upstream == multiplier && d.kind == DepKind::Data)
        );
    }
}

/// Regression test for #93: a `collect()` consumer's cross-phase fan-in
/// input must resolve to *every* partition artifact, not just the first
/// (`ProvidedInput::Collected(orefs).first()` silently truncated the list
/// to one partition's value). Since #97's planner fix, a collect()
/// consumer always lands in a phase after its producer, so this is the
/// only path fan-in resolution takes — `provided` is exactly what
/// `dispatch::build_provided_inputs` hands `load_phase` for a prior
/// phase's completed outputs.
#[test]
fn load_phase_delivers_full_collected_list_cross_phase() {
    use crate::dispatch::{OutputRef, ProvidedInput};
    use barca_compile::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
    use barca_model::StepId;

    let step = StreamStep {
        step_id: StepId::unpartitioned("f:sink"),
        kind: barca_model::NodeKind::Asset,
        function_name: std::sync::Arc::from("sink"),
        source_file: std::sync::Arc::from("f"),
        inputs: HashMap::from([("data".to_string(), "f:source".to_string())]),
        pending_partitions: HashMap::new(),
        serializer: None,
        sinks: vec![],
        run_hashes: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 1,
        retry_backoff_seconds: 0.0,
        partition_keys: vec![],
        param_types: HashMap::new(),
        return_type: None,
        keyed: Default::default(),
    };
    let phase = Phase {
        reason: PhaseReason::FanIn {
            node_id: "f:sink".to_string(),
        },
        streams: vec![WorkerStream {
            stream_id: "w0".to_string(),
            steps: vec![step],
        }],
    };

    let oref_a = OutputRef {
        path: "f--source_key_a.json".to_string(),
        format: "json".to_string(),
        size_bytes: 10,
        elapsed_seconds: None,
        content_hash: None,
    };
    let oref_b = OutputRef {
        path: "f--source_key_b.json".to_string(),
        format: "json".to_string(),
        size_bytes: 12,
        elapsed_seconds: None,
        content_hash: None,
    };
    let provided = HashMap::from([(
        "f:source".to_string(),
        ProvidedInput::Collected(vec![oref_a.clone(), oref_b.clone()]),
    )]);

    let mut c = Coordinator::new();
    let loaded = c.load_phase(&phase, &provided);
    assert_eq!(loaded, 1);

    let item = c
        .items
        .values()
        .find(|it| it.step_id.display() == "f:sink")
        .unwrap();
    assert!(
        !item.spec.dag_inputs.contains_key("data"),
        "a collected param must not be truncated into dag_inputs as a single path"
    );
    assert_eq!(
        item.spec.collected_inputs.get("data"),
        Some(&vec![oref_a, oref_b])
    );
}

// ─── Capacity (#436) ────────────────────────────────────────────────────

fn weighted(name: &str, weight: u32) -> ItemSpec {
    ItemSpec {
        weight,
        ..spec(name)
    }
}

#[test]
fn a_weight_above_the_capacity_counts_as_all_of_it() {
    assert_eq!(effective_weight(0, 4), 1);
    assert_eq!(effective_weight(3, 4), 3);
    assert_eq!(effective_weight(9, 4), 4);
    assert!(fits(2, 2, 4));
    assert!(!fits(3, 2, 4));
    // Too heavy for the capacity: it starts when nothing runs, and then only it runs.
    assert!(fits(0, 9, 4));
    assert!(!fits(1, 9, 4));
    assert!(!fits(4, 1, 4));
}

/// Start what fits from the head of the queue, as the pool does; returns what started.
fn admit(c: &mut Coordinator, running: &mut u32, capacity: u32) -> Vec<ItemId> {
    let mut started = Vec::new();
    while c.head_fits(*running, capacity) {
        let id = c.next_ready().unwrap();
        *running += effective_weight(c.item(id).spec.weight, capacity);
        started.push(id);
    }
    started
}

#[test]
fn three_heavy_items_run_two_at_a_time_under_a_capacity_of_four() {
    let mut c = Coordinator::new();
    let heavy: Vec<ItemId> = ["h1", "h2", "h3"]
        .iter()
        .map(|n| c.add_item(dummy_step_id(n), weighted(n, 2), vec![]))
        .collect();
    let mut running = 0;
    assert_eq!(admit(&mut c, &mut running, 4), heavy[..2]);
    c.on_item_completed(heavy[0]);
    running -= 2;
    assert_eq!(admit(&mut c, &mut running, 4), heavy[2..]);
}

/// A heavy item waiting for room is not overtaken by lighter items queued after it, or a
/// stream of light work could keep it waiting for the rest of the run.
#[test]
fn a_heavy_item_starts_before_the_light_ones_queued_behind_it() {
    let mut c = Coordinator::new();
    let l1 = c.add_item(dummy_step_id("l1"), spec("l1"), vec![]);
    let l2 = c.add_item(dummy_step_id("l2"), spec("l2"), vec![]);
    let heavy = c.add_item(dummy_step_id("h"), weighted("h", 2), vec![]);
    let l3 = c.add_item(dummy_step_id("l3"), spec("l3"), vec![]);
    let mut running = 0;
    assert_eq!(admit(&mut c, &mut running, 2), vec![l1, l2]);

    // One slot frees: the heavy item does not fit yet, and l3 waits behind it.
    c.on_item_completed(l1);
    running -= 1;
    assert_eq!(admit(&mut c, &mut running, 2), vec![]);
    // A light item made ready now queues behind it too.
    let l4 = c.add_item(dummy_step_id("l4"), spec("l4"), vec![]);
    assert_eq!(admit(&mut c, &mut running, 2), vec![]);

    c.on_item_completed(l2);
    running -= 1;
    assert_eq!(admit(&mut c, &mut running, 2), vec![heavy]);
    c.on_item_completed(heavy);
    running -= 2;
    assert_eq!(admit(&mut c, &mut running, 2), vec![l3, l4]);
}

#[test]
fn parallel_branches_take_the_weight_of_the_step_that_called_parallel() {
    let mut c = Coordinator::new();
    let parent = c.add_item(dummy_step_id("p"), weighted("p", 3), vec![]);
    c.next_ready();
    let (_, children) = c.on_parallel_requested(parent, vec![spec("c0"), spec("c1")]);
    for child in children {
        assert_eq!(c.item(child).spec.weight, 3);
    }
}

#[test]
fn every_partition_key_takes_its_nodes_weight() {
    use barca_compile::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
    use barca_model::{PartitionKey, StepId};

    let pk = |k: &str| PartitionKey::from(HashMap::from([("k".to_string(), k.to_string())]));
    let step = StreamStep {
        step_id: StepId::unpartitioned("f:heavy"),
        kind: barca_model::NodeKind::Asset,
        function_name: std::sync::Arc::from("heavy"),
        source_file: std::sync::Arc::from("f"),
        inputs: HashMap::new(),
        pending_partitions: HashMap::new(),
        serializer: None,
        sinks: vec![],
        run_hashes: HashMap::new(),
        timeout_seconds: 300,
        retries: 1,
        weight: 2,
        retry_backoff_seconds: 0.0,
        partition_keys: vec![pk("a"), pk("b"), pk("c")],
        param_types: HashMap::new(),
        return_type: None,
        keyed: Default::default(),
    };
    let phase = Phase {
        reason: PhaseReason::Initial,
        streams: vec![WorkerStream {
            stream_id: "w0".to_string(),
            steps: vec![step],
        }],
    };
    let mut c = Coordinator::new();
    assert_eq!(c.load_phase(&phase, &HashMap::new()), 3);
    assert!(c.items.values().all(|it| it.spec.weight == 2));
    // Under a capacity of 4, two keys at a time.
    let mut running = 0;
    assert_eq!(admit(&mut c, &mut running, 4).len(), 2);
}
