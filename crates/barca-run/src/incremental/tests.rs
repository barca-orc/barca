use super::*;
use barca_compile::dag::Dag;
use barca_compile::planner::{ResourceConfig, decompose, plan};

fn dag_of(src: &str) -> Dag {
    Dag::build(&barca_compile::parse::extract_nodes(src, "p.py").unwrap()).unwrap()
}

fn phase_of(dag: &Dag) -> Phase {
    let config = ResourceConfig {
        pool_size: 2,
        concurrency_groups: HashMap::new(),
    };
    plan(dag, &decompose(dag), &config).phases.remove(0)
}

/// An `Incremental` as `open` would build it for `phase`, with a proof recorded for every
/// planned node (as if the previous run cached them all).
fn incremental_for(phase: &Phase) -> Incremental {
    let mut templates = HashMap::new();
    let mut signatures = HashMap::new();
    let mut previous = Snapshot::default();
    for step in phase.streams.iter().flat_map(|s| &s.steps) {
        let id = step.step_id.base_id().to_owned();
        templates.insert(id.clone(), step.with_keys(Vec::new()));
        signatures.insert(id.clone(), "sig".to_string());
        previous.nodes.insert(
            id,
            Proof {
                signature: "sig".to_string(),
                count: 1,
                run_hash: Some("h".to_string()),
                ..Default::default()
            },
        );
    }
    Incremental {
        key: "k".into(),
        generation: 1,
        previous,
        next: Snapshot::default(),
        signatures,
        changes: HashMap::new(),
        templates,
        cached: HashMap::new(),
        filters: HashMap::new(),
        seeded: HashSet::new(),
        held: HashSet::new(),
    }
}

/// A node the run holds (paused under a tick, pinned) is never proven fresh, whatever its
/// proof says: it goes through the cache, which reads it as it is, and so does what reads it.
/// A proof that survived a hold would call the node fresh after `barca resume`, for inputs
/// that changed while it was held (#988).
#[test]
fn a_held_node_is_not_proven_fresh_and_neither_is_what_reads_it() {
    let dag = dag_of(
        "from barca import asset\n\n@asset\ndef a():\n    return 1\n\n@asset(inputs={\"a\": a})\ndef b(a):\n    return a\n\n@asset(inputs={\"b\": b})\ndef c(b):\n    return b\n",
    );
    let phase = phase_of(&dag);
    let mut inc = incremental_for(&phase);
    inc.held.insert("p.py:b".to_string());
    let (pruned, reports, _) = inc.prune(&dag, &phase);
    let cached: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(cached, ["p.py:a"]);
    let mut left: Vec<_> = pruned
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|s| s.step_id.base_id().to_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["p.py:b", "p.py:c"]);
}

/// A node with a recorded proof that the source no longer defines (the plan is stale, or the
/// proof outlived the node) reads as not cached rather than panicking (#800).
#[test]
fn a_recorded_node_removed_from_the_dag_is_not_cached() {
    let before = dag_of(
        "from barca import asset\n\n@asset\ndef a():\n    return 1\n\n@asset(inputs={\"a\": a})\ndef gone(a):\n    return a\n",
    );
    let phase = phase_of(&before);
    let after = dag_of("from barca import asset\n\n@asset\ndef a():\n    return 1\n");
    assert!(after.get_node("p.py:gone").is_none());

    let mut inc = incremental_for(&phase);
    assert!(matches!(inc.change(&after, "p.py:gone"), Change::All));
    // What the phase planned for it: an unexpanded step has no keys.
    assert_eq!(inc.count(&after, "p.py:gone", &phase), 0);

    let (pruned, reports, omitted) = inc.prune(&after, &phase);
    let cached: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        cached,
        ["p.py:a"],
        "only the node still in the DAG is cached"
    );
    assert_eq!(omitted, 1);
    let left: Vec<_> = pruned
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|s| s.step_id.base_id().to_owned())
        .collect();
    assert_eq!(left, ["p.py:gone"], "the removed node's step stays planned");
}
