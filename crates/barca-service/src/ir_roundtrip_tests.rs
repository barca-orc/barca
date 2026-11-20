//! The graph as plain data and back (#1056, D3): a graph taken apart, written as JSON, read and
//! put together again plans and hashes exactly as the one the Python frontend built.
//!
//! This is what "the IR is a type a second frontend could produce" means in a test: whatever
//! planning, hashing and the dry run read is in [`crate::dag::DagParts`], and nothing they read
//! is reachable only through the parser. The JSON here is serde's derive of internal types. It
//! is not a format: nothing reads or writes it outside this test.
//!
//! What does not go through, by decision: `Dag::files_read`, which describes the load and not
//! the graph (`docs/engineering/ir.md`). The test copies it across so that the comparison is of
//! the graph alone.

use crate::dag::{Dag, DagParts};
use barca_model::ir::{BatchSpec, EdgeKind, Freshness, Invocation, NodeKind, PartitionSpec};
use std::path::{Path, PathBuf};

/// A `python` that runs the real interpreter with this checkout's `barca` importable: loading
/// the fixture evaluates `partitions(region_keys())`, and the dry run `batches_by(continent)`.
fn python(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python")
        .canonicalize()
        .unwrap();
    let python = dir.join("python");
    std::fs::write(
        &python,
        format!(
            "#!/bin/sh\nPYTHONPATH='{}' exec python3 \"$@\"\n",
            source.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    python
}

struct Fixture {
    dir: tempfile::TempDir,
    files: Vec<String>,
    python: PathBuf,
    cfg: crate::config::ResolvedConfig,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/ir_roundtrip");
        let file = dir.path().join("pipeline.py");
        std::fs::copy(source.join("pipeline.py"), &file).unwrap();
        let files = vec![file.display().to_string()];
        let python = python(dir.path());
        let mut cfg =
            crate::config::resolve_with(None, dir.path(), crate::config::Vars::NONE).unwrap();
        cfg.db_path = dir.path().join(".barca/metadata.db").display().to_string();
        std::fs::create_dir_all(dir.path().join(".barca")).unwrap();
        crate::db::init_db(&cfg.db_path).await.unwrap();
        Fixture {
            dir,
            files,
            python,
            cfg,
        }
    }

    fn id(&self, name: &str) -> String {
        format!("{}/{name}", self.dir.path().display())
    }

    /// Everything a command derives from the graph without running a step, as JSON: the plan
    /// (`barca plan`) at several pool sizes and narrowed to a key, every node's run hash
    /// (`barca status`), and the dry run of a `get` and of a `run` with their batch calls.
    async fn derived(&self, dag: &Dag) -> serde_json::Value {
        let mut plans = Vec::new();
        for pool_size in [1, 2, 8] {
            let sizing = crate::sizing::Sizing {
                pool_size,
                pool_source: "cores",
                capacity: pool_size,
                capacity_source: "pool size",
            };
            plans.push(serde_json::to_value(crate::queries::plan_from_dag(dag, sizing)).unwrap());
        }
        let selection = crate::selection::PartitionSelection::parse("region=emea").unwrap();
        let sizing = crate::sizing::Sizing {
            pool_size: 4,
            pool_source: "cores",
            capacity: 4,
            capacity_source: "pool size",
        };
        let narrowed = crate::queries::plan_selected(dag, sizing, Some(&selection)).unwrap();

        let status = crate::status::status_from_dag(&self.cfg, &[], dag, &self.python, 0, false)
            .await
            .unwrap();

        let mut dry_runs = Vec::new();
        for (label, target) in [
            ("get", self.id("pipeline.py:report")),
            ("get", "daily_scores".to_string()),
            ("get", self.id("pipeline.py:restock")),
            ("get", self.id("pipeline.py:quote")),
            ("get", self.id("pipeline.py:grid")),
            ("get", self.id("pipeline.py:priced")),
            ("run", self.id("pipeline.py:publish")),
        ] {
            // `priced` reads `rates`, which is `Manual` and never computed: its dry run is a
            // usage error, and it has to be the same one.
            let result = crate::predict::explain_dag(
                dag,
                &self.cfg,
                std::slice::from_ref(&target),
                crate::cache::CachePolicy::CacheAware,
                label,
                None,
                &self.files,
            )
            .await;
            dry_runs.push(match result {
                Ok(mut result) => {
                    let batch = crate::batch::BatchOverride::default();
                    crate::batch::annotate(dag, &mut result.steps, &batch, &self.python).unwrap();
                    serde_json::to_value(&result).unwrap()
                }
                Err(error) => serde_json::json!({ "error": error.to_string() }),
            });
        }
        serde_json::json!({
            "plans": plans,
            "narrowed": narrowed,
            "status": status,
            "dry_runs": dry_runs,
        })
    }
}

/// The fixture has one of everything the graph carries, so the comparison below is of all of
/// it: a fixture that lost a case would make the round trip pass for less.
fn assert_representative(fixture: &Fixture, dag: &Dag) {
    let node = |name: &str| {
        dag.get_node(&fixture.id(name))
            .or_else(|| dag.get_node(name))
            .unwrap_or_else(|| panic!("no node {name}"))
    };
    let kinds: Vec<NodeKind> = dag.graph.node_weights().map(|n| n.decl.kind).collect();
    for kind in [NodeKind::Asset, NodeKind::Sensor, NodeKind::Task] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    let edges: Vec<EdgeKind> = dag.graph.edge_weights().copied().collect();
    for kind in [
        EdgeKind::Direct,
        EdgeKind::Collect,
        EdgeKind::PartitionSource,
    ] {
        assert!(edges.contains(&kind), "{kind:?}");
    }
    // Partitions: literal, computed (evaluated to a list while loading), taken from a
    // partitioned asset, derived from the list an asset returns, and two dimensions.
    let keys = |name: &str, dim: &str| match &node(name).decl.partitions[dim] {
        PartitionSpec::Static { values } => values.len(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(keys("pipeline.py:sales", "region"), 3);
    assert_eq!(keys("pipeline.py:stock", "region"), 3);
    assert_eq!(keys("pipeline.py:margin", "region"), 3);
    assert_eq!(
        node("pipeline.py:margin").decl.inherited_partitions,
        ["region"]
    );
    assert_eq!(
        node("pipeline.py:score").decl.inherited_partitions,
        ["region"]
    );
    assert!(matches!(
        node("pipeline.py:quote").decl.partitions["ticker"],
        PartitionSpec::DerivedFrom { .. }
    ));
    assert_eq!(node("pipeline.py:grid").decl.partitions.len(), 2);
    // Batches: a size, all, named groups and a label function.
    let batch = |name: &str| node(name).decl.batch.clone().unwrap();
    assert!(matches!(
        batch("pipeline.py:margin"),
        BatchSpec::Size { size: 2 }
    ));
    assert!(matches!(batch("pipeline.py:grid"), BatchSpec::All));
    assert!(matches!(
        batch("pipeline.py:restock"),
        BatchSpec::Groups { .. }
    ));
    assert!(matches!(batch("pipeline.py:score"), BatchSpec::By { .. }));
    // Schedules, with their options; Manual; the scheduling numbers.
    let inbox = &node("pipeline.py:inbox").decl;
    assert!(matches!(inbox.freshness, Freshness::Schedule(_)));
    assert_eq!(
        (inbox.max_failures, inbox.schedule_timeout),
        (Some(3), Some(60))
    );
    let scores = &node("daily_scores").decl;
    assert_eq!(scores.explicit_name.as_deref(), Some("daily_scores"));
    assert_eq!(scores.schedule_batch.as_deref(), Some("all"));
    assert!(scores.sinks[0].is_dataset());
    assert_eq!(node("pipeline.py:report").decl.sinks.len(), 2);
    let rates = &node("pipeline.py:rates").decl;
    assert_eq!(rates.freshness, Freshness::Manual);
    assert!(rates.description.is_some() && !rates.tags.is_empty());
    let tickers = &node("pipeline.py:tickers").decl;
    assert!(tickers.cutoff && tickers.weight == 2 && tickers.retries == 3);
    assert_eq!(tickers.retry_backoff_seconds, 1.5);
    assert_eq!(tickers.env, ["FIXTURE_MARKET"]);
    assert!(!node("pipeline.py:stock").decl.deterministic);
    assert!(node("pipeline.py:sales").decl.artifact_serializer.is_some());
    // The fingerprint, the invocation and the findings.
    let report = node("pipeline.py:report");
    assert_eq!(report.fingerprint.definition_hash.len(), 64);
    assert!(
        report
            .fingerprint
            .components
            .iter()
            .any(|(k, _)| k == "code")
    );
    let Invocation::Python(function) = &report.invoke;
    assert_eq!(function.function_name, "report");
    assert!(function.source_file.ends_with("pipeline.py") && function.line > 0);
    assert_eq!(report.findings.unused_inputs, ["unused"]);
    assert_eq!(
        node("pipeline.py:tickers").undeclared_env(),
        ["FIXTURE_UNDECLARED"]
    );
    // Groups, one nested in nothing and one bare.
    assert_eq!(dag.groups.len(), 3);
}

#[tokio::test]
async fn a_graph_written_as_json_and_read_back_plans_and_hashes_the_same() {
    let fixture = Fixture::new().await;
    let dag = crate::load::build_dag(&fixture.files, &fixture.python)
        .await
        .unwrap();
    assert_representative(&fixture, &dag);

    let json = serde_json::to_string(&dag.to_parts()).unwrap();
    let parts: DagParts = serde_json::from_str(&json).unwrap();
    let mut back = Dag::from_parts(parts).unwrap();
    // Not the graph's: which files the load read, for `barca serve` to watch.
    back.files_read = dag.files_read.clone();

    // The data is the same data: nodes and edges in the same order, field for field.
    assert_eq!(
        serde_json::to_value(back.to_parts()).unwrap(),
        serde_json::from_str::<serde_json::Value>(&json).unwrap()
    );
    assert_eq!(back.topo_order(), dag.topo_order());

    // And everything derived from it is the same: the plans, every run hash, the dry runs.
    let (before, after) = (fixture.derived(&dag).await, fixture.derived(&back).await);
    for part in ["plans", "narrowed", "status", "dry_runs"] {
        assert_eq!(before[part], after[part], "{part}");
    }
    // The comparison is of something: run hashes in the status and the dry runs, and plans
    // with the steps of every node.
    let hashes = before.to_string().matches("\"run_hash\":\"").count();
    assert!(hashes >= 10, "{hashes} run hashes");
    assert!(before["plans"][0]["total_steps"].as_u64().unwrap() >= dag.node_count() as u64);
    let predicted = before["dry_runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| run.get("error").is_none())
        .count();
    assert_eq!(predicted, 6, "all but the dry run of `priced` gave steps");
}

#[test]
fn plain_data_that_is_not_a_graph_is_refused() {
    use crate::dag::{DagError, Edge};
    let fixture_node = |id: &str, kind: NodeKind| -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "decl": {
                "kind": kind, "explicit_name": null, "description": null, "tags": {},
                "freshness": {"type": "Always"}, "max_failures": null, "schedule_timeout": null,
                "schedule_batch": null, "partitions": {}, "inherited_partitions": [],
                "batch": null, "sinks": [], "artifact_serializer": null, "env": [],
                "timeout_seconds": 300, "retries": 1, "retry_backoff_seconds": 0.0,
                "weight": 1, "deterministic": true, "cutoff": false,
            },
            "resolved_inputs": {}, "resolved_collected": {},
            "fingerprint": {"definition_hash": "h", "components": []},
            "invoke": {
                "language": "python", "source_file": "p.py", "function_name": id, "line": 1,
                "param_types": {}, "return_type": null,
            },
            "findings": {"unused_inputs": [], "env_reads": []},
        })
    };
    let graph = |nodes: &[(&str, NodeKind)], edges: Vec<Edge>| {
        let nodes: Vec<_> = nodes
            .iter()
            .map(|(id, kind)| fixture_node(id, *kind))
            .collect();
        let parts: DagParts = serde_json::from_value(serde_json::json!({
            "nodes": nodes, "edges": edges, "groups": [], "cannot_run": {},
        }))
        .unwrap();
        Dag::from_parts(parts)
    };
    let edge = |from, to| Edge {
        from,
        to,
        kind: EdgeKind::Direct,
    };
    let (asset, task) = (NodeKind::Asset, NodeKind::Task);

    // A graph written by hand, with no Python frontend in sight, is a graph.
    let dag = graph(&[("a", asset), ("b", asset)], vec![edge(0, 1)]).unwrap();
    assert_eq!(dag.upstream("b"), ["a"]);

    assert!(matches!(
        graph(&[("a", asset), ("a", asset)], vec![]),
        Err(DagError::DuplicateKey { .. })
    ));
    assert!(matches!(
        graph(&[("a", asset), ("b", asset)], vec![edge(0, 1), edge(1, 0)]),
        Err(DagError::CycleDetected)
    ));
    assert!(matches!(
        graph(&[("a", task), ("b", asset)], vec![edge(0, 1)]),
        Err(DagError::TaskAsInput { .. })
    ));
    assert!(matches!(
        graph(&[("a", asset)], vec![edge(0, 3)]),
        Err(DagError::Malformed(_))
    ));
}
