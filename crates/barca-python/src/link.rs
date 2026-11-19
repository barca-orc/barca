//! The Python frontend's last step: from extracted nodes to the graph.
//!
//! Everything here answers a question about the source and not about the graph: which node a
//! reference written in a file means ([`crate::node_refs`]), what `partitions_from(...)` of a
//! partitioned asset copies onto its consumer, what a definition's fingerprint is
//! ([`crate::definition`]), and which definitions to leave out when some are broken. The
//! result is handed to [`Dag::from_parts`] as nodes with resolved inputs and a list of edges;
//! the graph ([`barca_model::dag`]) names nothing in this module or in the frontend.
//!
//! [`Dag::build`], [`Dag::isolate`] and [`Dag::isolate_with`] are defined here, on the graph's
//! type, because their callers (and many tests) have always written `crate::link::build(&nodes)`. They
//! take the Python frontend's nodes, so when the frontend is a crate of its own they become
//! functions of that crate.

use petgraph::graph::DiGraph;
use std::collections::HashMap;

use barca_model::hash;
use barca_model::ir::{
    DagNode, EdgeKind, Fingerprint, Invocation, NodeDecl, NodeFindings, NodeKind, PythonFunction,
};

use crate::extracted::ExtractedNode;
use crate::node_refs::{Resolver, file_dir, norm_path};
use barca_model::dag::{Dag, DagError, DagParts, Edge, check_data_edge};

/// Reject mixed static/evaluated and runtime-derived dimensions before Python evaluation.
pub(crate) fn validate_partition_dimensions(nodes: &[ExtractedNode]) -> Result<(), DagError> {
    use barca_model::model::PartitionSpec;
    for node in nodes {
        let derived = node
            .partitions
            .iter()
            .filter_map(|(dim, spec)| match spec {
                PartitionSpec::DerivedFrom { source_ref } => Some((dim, source_ref)),
                _ => None,
            })
            .min_by_key(|(dim, _)| *dim);
        if let Some((_, source_ref)) = derived
            && node
                .partitions
                .values()
                .any(|spec| !matches!(spec, PartitionSpec::DerivedFrom { .. }))
        {
            return Err(DagError::PartitionsFrom {
                node: node.continuity_key(),
                upstream: source_ref.resolution_name().to_string(),
                problem: "must be the asset's only partition dimension when combined with \
                          partitions(): mixing partitions() and partitions_from() dimensions \
                          is not supported"
                    .to_string(),
                fix: format!(
                    "Use only partitions_from() dimensions on '{}', or declare every \
                     dimension explicitly with partitions([...]).",
                    node.function_name
                ),
            });
        }
    }
    Ok(())
}

/// Resolve `partitions_from(upstream)` where `upstream` is itself partitioned (#189).
///
/// The consumer takes the upstream's partition spec (so the same keys, static or resolved at
/// run time), and each key reads that key of the upstream under the upstream's name, unless an
/// `inputs=` entry already names the upstream (then it arrives under that parameter). Both
/// steps are then partitioned by the same dimension, so the coordinator wires each consumer key
/// to the producer key with the same partition key, as for any partition-aligned input.
///
/// `partitions_from(<unpartitioned asset>)` is left alone: its keys are the values of the list
/// that asset returns, read at dispatch time (`dispatch::expand_pending_partitions`).
fn resolve_partitions_from(nodes: &[ExtractedNode]) -> Result<Vec<ExtractedNode>, DagError> {
    use crate::extracted::DeclaredInput;
    use barca_model::model::{NodeRef, PartitionSpec};

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Todo,
        Active,
        Done,
    }

    fn resolve(
        i: usize,
        nodes: &mut [ExtractedNode],
        refs: &Resolver,
        state: &mut [State],
    ) -> Result<(), DagError> {
        if state[i] != State::Todo {
            return Ok(()); // done, or a cycle (reported by `Dag::build`)
        }
        state[i] = State::Active;
        let derived: Vec<(String, NodeRef)> = nodes[i]
            .partitions
            .iter()
            .filter_map(|(dim, spec)| match spec {
                PartitionSpec::DerivedFrom { source_ref } => {
                    Some((dim.clone(), source_ref.clone()))
                }
                _ => None,
            })
            .collect();
        let mut derived = derived;
        derived.sort_by(|a, b| a.0.cmp(&b.0));
        for (dim, source_ref) in derived {
            let source = source_ref.resolution_name().to_string();
            let j = match refs.resolve_input(&nodes[i], &source, &source_ref) {
                Ok(j) => j,
                // Unknown: left for `Dag::build` (the source may be a plain list). Several
                // candidates or a bad import is an error now, as for any input.
                Err(DagError::UnresolvedInput { .. }) => continue,
                Err(e) => return Err(e),
            };
            resolve(j, nodes, refs, state)?;
            if nodes[j].partitions.is_empty() {
                continue; // keys come from the list the source returns
            }
            let node = nodes[i].continuity_key();
            let err = |problem: String, fix: String| DagError::PartitionsFrom {
                node: node.clone(),
                upstream: source.clone(),
                problem,
                fix,
            };
            if nodes[j].partitions.len() != 1 {
                let mut dims: Vec<&String> = nodes[j].partitions.keys().collect();
                dims.sort();
                return Err(err(
                    format!(
                        "is not supported: '{source}' has {} partition dimensions ({})",
                        dims.len(),
                        dims.iter()
                            .map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!(
                        "Declare the partitions of '{}' explicitly with partitions([...]).",
                        nodes[i].function_name
                    ),
                ));
            }
            let (src_dim, src_spec) = nodes[j]
                .partitions
                .iter()
                .next()
                .map(|(d, s)| (d.clone(), s.clone()))
                .expect("one dimension");
            if src_dim != dim {
                return Err(err(
                    format!(
                        "is declared under dimension '{dim}', but '{source}' is partitioned by \
                         '{src_dim}'"
                    ),
                    format!(
                        "Use the upstream's dimension name: \
                         `partitions={{\"{src_dim}\": partitions_from({source})}}`, with a \
                         parameter named '{src_dim}'."
                    ),
                ));
            }
            if nodes[i].partitions.len() != 1 {
                return Err(err(
                    "must be the asset's only partition dimension".to_string(),
                    format!(
                        "Remove the other dimensions from '{}', or declare its keys explicitly \
                         with partitions([...]).",
                        nodes[i].function_name
                    ),
                ));
            }
            let consumer = &mut nodes[i];
            consumer.inherited_partitions.push(dim.clone());
            consumer.partitions.insert(dim, src_spec);
            let file = consumer.source_file.clone();
            let named = consumer.inputs.iter().any(|inp| {
                !inp.collected && matches!(refs.resolve(&file, &inp.upstream), Ok(k) if k == j)
            });
            if !named {
                consumer.inputs.push(DeclaredInput {
                    param_name: source.clone(),
                    upstream: source_ref.clone(),
                    collected: false,
                });
            }
        }
        state[i] = State::Done;
        Ok(())
    }

    let mut out = nodes.to_vec();
    let refs = Resolver::new(&out);
    let mut state = vec![State::Todo; out.len()];
    for i in 0..out.len() {
        resolve(i, &mut out, &refs, &mut state)?;
    }
    Ok(out)
}

/// The graph's node for an extracted one: its declaration, its fingerprint and how to call it,
/// each field moved to the part of the node it belongs to. What only the frontend needed stays
/// behind: the references as written (`inputs`, now `resolved_*`), the text and cone the
/// fingerprint was computed from, `byte_offset`, `is_unsafe` and `parallel_calls`.
fn to_ir(
    node: ExtractedNode,
    id: String,
    resolved_inputs: HashMap<String, String>,
    resolved_collected: HashMap<String, String>,
) -> DagNode {
    // The definition hash: what determines the node's result. `source_text` is the function
    // from `def` on plus the decorator parts that count, in canonical form
    // (`crate::definition::RULES` is the list: inputs, serializer, sinks, ...), and `cone_hash`
    // is the helper code both reach. The kind is the one thing added here.
    let metadata = serde_json::json!({ "kind": node.kind }).to_string();
    let fingerprint = Fingerprint {
        definition_hash: hash::definition_hash(&node.source_text, &node.cone_hash, &metadata),
        components: crate::definition::definition_components(&node),
    };
    DagNode {
        id,
        decl: NodeDecl {
            kind: node.kind,
            explicit_name: node.explicit_name,
            description: node.description,
            tags: node.tags,
            freshness: node.freshness,
            max_failures: node.max_failures,
            schedule_timeout: node.schedule_timeout,
            schedule_batch: node.schedule_batch,
            partitions: node.partitions,
            inherited_partitions: node.inherited_partitions,
            batch: node.batch,
            sinks: node.sinks,
            artifact_serializer: node.artifact_serializer,
            env: node.env,
            timeout_seconds: node.timeout_seconds,
            retries: node.retries,
            retry_backoff_seconds: node.retry_backoff_seconds,
            weight: node.weight,
            deterministic: node.deterministic,
            cutoff: node.cutoff,
        },
        resolved_inputs,
        resolved_collected,
        fingerprint,
        invoke: Invocation::Python(PythonFunction {
            source_file: node.source_file,
            function_name: node.function_name,
            line: node.line,
            param_types: node.param_types,
            return_type: node.return_type,
        }),
        findings: NodeFindings {
            unused_inputs: node.unused_inputs,
            env_reads: node.env_reads,
        },
    }
}

/// Isolate graph-invalid definitions while retaining their unrelated neighbours.
/// Dependency edges are resolved before removing anything, so removal cannot redirect
/// an input to a different same-named definition.
pub(crate) fn isolate(
    nodes: &[ExtractedNode],
    failed_files: &[String],
) -> (Dag, Vec<(String, String)>) {
    isolate_with(nodes, failed_files, Vec::new())
}

/// [`isolate`], with nodes already known to be broken: `failed` (node id, error)
/// are left out with their error, and so is everything downstream of them.
pub(crate) fn isolate_with(
    nodes: &[ExtractedNode],
    failed_files: &[String],
    failed: Vec<(String, String)>,
) -> (Dag, Vec<(String, String)>) {
    use std::collections::HashSet;
    let refs = Resolver::new(nodes);
    let mut edges = Vec::new();
    let mut failures = failed;
    for node in nodes {
        let id = node.continuity_key();
        let inputs = node
            .inputs
            .iter()
            .map(|i| (&i.param_name, &i.upstream, true))
            .chain(node.partitions.iter().filter_map(|(dim, p)| match p {
                barca_model::model::PartitionSpec::DerivedFrom { source_ref } => {
                    Some((dim, source_ref, false))
                }
                _ => None,
            }));
        for (param, upstream, required) in inputs {
            // Follow the resolver's priority: a failed lower-priority source
            // cannot hide an earlier healthy binding, and an earlier failed
            // source cannot redirect to a later healthy same-named node.
            let candidates = match upstream {
                barca_model::model::NodeRef::Canonical(value)
                    if !refs.by_id.contains_key(value) =>
                {
                    value.rsplit_once(':').map(|(file, name)| {
                        (
                            vec![
                                std::path::PathBuf::from(file),
                                file_dir(&node.source_file).join(file),
                            ],
                            name,
                        )
                    })
                }
                barca_model::model::NodeRef::Imported { module, name } => Some((
                    Resolver::module_files(&node.source_file, module),
                    name.as_str(),
                )),
                _ => None,
            };
            let mut failed = None;
            if let Some((candidates, name)) = candidates {
                for candidate in candidates {
                    failed = failed_files.iter().find(|file| {
                        norm_path(&candidate) == norm_path(std::path::Path::new(file))
                    });
                    if failed.is_some() || refs.in_file(&candidate, name).is_some() {
                        break;
                    }
                }
            }
            if let Some(file) = failed {
                failures.push((
                    id.clone(),
                    format!("input '{param}' references unloaded source '{file}'"),
                ));
                continue;
            }
            match refs.resolve_input(node, param, upstream) {
                Ok(i) => edges.push((refs.ids[i].clone(), id.clone())),
                Err(DagError::UnresolvedInput { .. }) if !required => {}
                Err(e) => failures.push((id.clone(), e.to_string())),
            }
        }
    }
    let mut excluded: HashSet<String> = failures.iter().map(|(id, _)| id.clone()).collect();
    loop {
        // Close over the original bindings, never over newly selected names.
        loop {
            let before = excluded.len();
            for (upstream, dependent) in &edges {
                if excluded.contains(upstream) && excluded.insert(dependent.clone()) {
                    failures.push((
                        dependent.clone(),
                        format!("upstream '{upstream}' is not loaded"),
                    ));
                }
            }
            if before == excluded.len() {
                break;
            }
        }
        let selected: Vec<_> = nodes
            .iter()
            .filter(|n| !excluded.contains(&n.continuity_key()))
            .cloned()
            .collect();
        match build(&selected) {
            Ok(dag) => return (dag, failures),
            Err(error) => {
                let offenders: Vec<String> = match &error {
                    DagError::UnresolvedInput { node, .. }
                    | DagError::AmbiguousInput { node, .. }
                    | DagError::UnresolvedImport { node, .. }
                    | DagError::PartitionedInputToUnpartitioned { node, .. }
                    | DagError::PartitionsFrom { node, .. } => vec![node.clone()],
                    // Not from `build`: only plain data can be malformed.
                    DagError::Malformed(_) => Vec::new(),
                    DagError::SensorWithInputs { sensor } => vec![sensor.clone()],
                    DagError::TaskAsInput { downstream, .. } => vec![downstream.clone()],
                    DagError::DuplicateKey { key, .. } => vec![key.clone()],
                    DagError::CycleDetected => {
                        let mut graph = DiGraph::<String, ()>::new();
                        let indices: HashMap<_, _> = selected
                            .iter()
                            .map(|n| {
                                let id = n.continuity_key();
                                let idx = graph.add_node(id.clone());
                                (id, idx)
                            })
                            .collect();
                        for (upstream, dependent) in &edges {
                            if let (Some(&u), Some(&d)) =
                                (indices.get(upstream), indices.get(dependent))
                            {
                                graph.add_edge(u, d, ());
                            }
                        }
                        petgraph::algo::tarjan_scc(&graph)
                            .into_iter()
                            .filter(|component| {
                                component.len() > 1
                                    || graph.contains_edge(component[0], component[0])
                            })
                            .flatten()
                            .map(|i| graph[i].clone())
                            .collect()
                    }
                };
                assert!(
                    !offenders.is_empty(),
                    "DAG validation must identify its invalid definitions"
                );
                for id in offenders {
                    assert!(excluded.insert(id.clone()), "isolation must make progress");
                    failures.push((id, error.to_string()));
                }
            }
        }
    }
}

/// Build a DAG from extracted nodes. Validates all constraints.
pub fn build(nodes: &[ExtractedNode]) -> Result<Dag, DagError> {
    validate_partition_dimensions(nodes)?;
    let resolved = resolve_partitions_from(nodes)?;
    let nodes = resolved.as_slice();
    let refs = Resolver::new(nodes);

    // First pass: check for duplicate keys.
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        let id = refs.ids[i].as_str();
        if let Some(&existing) = index.get(id) {
            return Err(DagError::DuplicateKey {
                key: id.to_string(),
                first: nodes[existing].source_file.clone(),
                second: node.source_file.clone(),
            });
        }
        // Validate: sensors cannot have inputs.
        if node.kind == NodeKind::Sensor && !node.inputs.is_empty() {
            return Err(DagError::SensorWithInputs {
                sensor: id.to_string(),
            });
        }
        index.insert(id, i);
    }

    // Second pass: resolve inputs into edges.
    let mut edges = Vec::new();
    let mut resolved_maps = Vec::with_capacity(nodes.len());
    for (downstream, node) in nodes.iter().enumerate() {
        let downstream_key = refs.ids[downstream].as_str();
        let mut resolved_inputs = HashMap::new();
        let mut resolved_collected = HashMap::new();
        for input in &node.inputs {
            let upstream = refs.resolve_input(node, &input.param_name, &input.upstream)?;
            let upstream_key = refs.ids[upstream].as_str();
            check_data_edge(
                (
                    upstream_key,
                    nodes[upstream].kind,
                    !nodes[upstream].partitions.is_empty(),
                ),
                (downstream_key, node.kind, !node.partitions.is_empty()),
                input.collected,
                || input.param_name.as_str(),
            )?;
            edges.push(Edge {
                from: upstream,
                to: downstream,
                kind: if input.collected {
                    EdgeKind::Collect
                } else {
                    EdgeKind::Direct
                },
            });
            // Record the resolved mapping on the node.
            if input.collected {
                resolved_collected.insert(input.param_name.clone(), upstream_key.to_string());
            } else {
                resolved_inputs.insert(input.param_name.clone(), upstream_key.to_string());
            }
        }

        // Add partition_source edges for partitions_from.
        for spec in node.partitions.values() {
            if let barca_model::model::PartitionSpec::DerivedFrom { source_ref } = spec
                && let Ok(source) = refs.resolve(&node.source_file, source_ref)
            {
                edges.push(Edge {
                    from: source,
                    to: downstream,
                    kind: EdgeKind::PartitionSource,
                });
            }
        }
        resolved_maps.push((resolved_inputs, resolved_collected));
    }

    drop(index);
    let ids = refs.ids;
    let nodes = resolved
        .into_iter()
        .zip(ids)
        .zip(resolved_maps)
        .map(|((node, id), (inputs, collected))| to_ir(node, id, inputs, collected))
        .collect();
    Dag::from_parts(DagParts {
        nodes,
        edges,
        ..DagParts::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> ExtractedNode {
        ExtractedNode {
            kind: NodeKind::Asset,
            function_name: name.to_string(),
            explicit_name: None,
            freshness: barca_model::model::Freshness::Always,
            max_failures: None,
            schedule_timeout: None,
            inputs: smallvec::SmallVec::new(),
            partitions: HashMap::new(),
            sinks: smallvec::SmallVec::new(),
            timeout_seconds: 300,
            retries: 1,
            cutoff: false,
            weight: 1,
            deterministic: true,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: HashMap::new(),
            is_unsafe: false,
            source_file: "test.py".to_string(),
            byte_offset: 0,
            line: 0,
            source_text: "def a(): return 1".to_string(),
            cone_hash: String::new(),
            cone_parts: Vec::new(),
            artifact_serializer: None,
            param_types: HashMap::new(),
            return_type: None,
            parallel_calls: Vec::new(),
            env: Vec::new(),
            unused_inputs: Vec::new(),
            env_reads: Vec::new(),
            inherited_partitions: Vec::new(),
            batch: None,
            schedule_batch: None,
        }
    }

    fn build_files(files: &[(&str, &str)]) -> Result<Dag, DagError> {
        let mut nodes = Vec::new();
        for (path, src) in files {
            nodes.extend(crate::parse::extract_nodes(src, path).unwrap());
        }
        crate::link::build(&nodes)
    }

    fn input_of(dag: &Dag, node: &str, param: &str) -> String {
        dag.get_node(node)
            .unwrap_or_else(|| panic!("no node {node}"))
            .resolved_inputs
            .get(param)
            .unwrap_or_else(|| panic!("{node} has no input {param}"))
            .clone()
    }

    const SRC_ASSET: &str =
        "from barca import asset\n\n@asset()\ndef src() -> int:\n    return 1\n";

    #[test]
    fn a_name_defined_in_the_same_file_wins_over_other_files() {
        for order in [["a.py", "b.py"], ["b.py", "a.py"]] {
            let a = format!(
                "{SRC_ASSET}\n@asset(inputs={{\"s\": src}})\ndef out(s: int) -> int:\n    return s\n"
            );
            let files: Vec<(&str, &str)> = order
                .iter()
                .map(|f| {
                    if *f == "a.py" {
                        ("a.py", a.as_str())
                    } else {
                        ("b.py", SRC_ASSET)
                    }
                })
                .collect();
            let dag = build_files(&files).unwrap();
            assert_eq!(
                input_of(&dag, "a.py:out", "s"),
                "a.py:src",
                "order {order:?}"
            );
        }
    }

    #[test]
    fn an_imported_name_resolves_to_the_module_it_is_imported_from() {
        let cases = [
            (
                "pipelines/reconcile.py",
                "from pipelines.sources import src\n",
                "src",
            ),
            (
                "pipelines/reconcile.py",
                "from .sources import src\n",
                "src",
            ),
            ("pipelines/reconcile.py", "from sources import src\n", "src"),
            (
                "pipelines/reconcile.py",
                "from pipelines.sources import src as up\n",
                "up",
            ),
            (
                "pipelines/reconcile.py",
                "import pipelines.sources as s\n",
                "s.src",
            ),
            (
                "pipelines/reconcile.py",
                "import pipelines.sources\n",
                "pipelines.sources.src",
            ),
            (
                "pipelines/deep/reconcile.py",
                "from ..sources import src\n",
                "src",
            ),
        ];
        for (path, import, expr) in cases {
            let consumer = format!(
                "from barca import asset\n{import}\n@asset(inputs={{\"s\": {expr}}})\ndef out(s: int) -> int:\n    return s\n"
            );
            // A decoy with the same function name elsewhere must not be picked.
            let dag = build_files(&[
                ("other/sources.py", SRC_ASSET),
                ("pipelines/sources.py", SRC_ASSET),
                (path, consumer.as_str()),
            ])
            .unwrap_or_else(|e| panic!("{import}: {e}"));
            assert_eq!(
                input_of(&dag, &format!("{path}:out"), "s"),
                "pipelines/sources.py:src",
                "{import}"
            );
        }
    }

    #[test]
    fn collect_of_an_imported_attribute_resolves() {
        let up = "from barca import asset, partitions\n\n@asset(partitions={\"k\": partitions([\"a\", \"b\"])})\ndef src(k: str) -> int:\n    return 1\n";
        let consumer = "from barca import asset, collect\nimport up\n\n@asset(inputs={\"s\": collect(up.src)})\ndef total(s: list) -> int:\n    return sum(s)\n";
        let dag = build_files(&[("up.py", up), ("c.py", consumer)]).unwrap();
        let n = dag.get_node("c.py:total").unwrap();
        assert_eq!(
            n.resolved_collected.get("s").map(String::as_str),
            Some("up.py:src")
        );
    }

    #[test]
    fn an_imported_name_that_is_not_a_node_there_is_an_error() {
        let helper = "def src():\n    return 1\n";
        let consumer = "from barca import asset\nfrom helpers import src\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let err = build_files(&[("helpers.py", helper), ("c.py", consumer)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'src' is imported from helpers"), "{msg}");
        assert!(msg.contains("no @asset/@task/@sensor named 'src'"), "{msg}");
    }

    #[test]
    fn a_reexported_name_falls_back_to_the_unique_node_with_that_name() {
        // `from pkg import src` where pkg/__init__.py re-exports it from pkg/impl.py.
        let consumer = "from barca import asset\nfrom pkg import src\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let dag = build_files(&[("pkg/impl.py", SRC_ASSET), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:out", "s"), "pkg/impl.py:src");
    }

    #[test]
    fn a_bare_name_defined_in_several_other_files_is_ambiguous() {
        let consumer = "from barca import asset\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let err = build_files(&[("a.py", SRC_ASSET), ("b.py", SRC_ASSET), ("c.py", consumer)])
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("a.py:src") && msg.contains("b.py:src"),
            "{msg}"
        );
        assert!(msg.contains("asset_ref"), "{msg}");
    }

    #[test]
    fn a_bare_name_defined_once_elsewhere_still_resolves() {
        let consumer = "from barca import asset\n\n@asset(inputs={\"s\": src})\ndef out(s: int) -> int:\n    return s\n";
        let dag = build_files(&[("a.py", SRC_ASSET), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:out", "s"), "a.py:src");
    }

    #[test]
    fn asset_ref_picks_the_named_file() {
        let consumer = "from barca import asset, asset_ref\n\n@asset(inputs={\"s\": asset_ref(\"b.py:src\")})\ndef out(s: int) -> int:\n    return s\n";
        let dag =
            build_files(&[("a.py", SRC_ASSET), ("b.py", SRC_ASSET), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:out", "s"), "b.py:src");
        // A path relative to the referencing file's directory also works.
        let nested = "from barca import asset, asset_ref\n\n@asset(inputs={\"s\": asset_ref(\"b.py:src\")})\ndef out(s: int) -> int:\n    return s\n";
        let dag = build_files(&[
            ("p/b.py", SRC_ASSET),
            ("q/b.py", SRC_ASSET),
            ("p/c.py", nested),
        ])
        .unwrap();
        assert_eq!(input_of(&dag, "p/c.py:out", "s"), "p/b.py:src");
    }

    #[test]
    fn partitions_from_an_imported_asset_resolves_to_that_file() {
        let up = "from barca import asset, partitions\n\n@asset(partitions={\"k\": partitions([\"a\", \"b\"])})\ndef src(k: str) -> int:\n    return 1\n";
        let consumer = "from barca import asset, partitions_from\nfrom up import src\n\n@asset(partitions={\"k\": partitions_from(src)})\ndef per(k: str, src: int) -> int:\n    return src\n";
        let dag = build_files(&[("decoy.py", up), ("up.py", up), ("c.py", consumer)]).unwrap();
        assert_eq!(input_of(&dag, "c.py:per", "src"), "up.py:src");
    }

    fn build_src(src: &str) -> Result<Dag, DagError> {
        crate::link::build(&crate::parse::extract_nodes(src, "t.py").unwrap())
    }

    const SALES: &str = "from barca import asset, collect, partitions, partitions_from\n\n\
@asset(partitions={\"region\": partitions([\"emea\", \"amer\"])})\n\
def sales(region: str) -> dict:\n    return {}\n\n";

    #[test]
    fn partitions_from_a_partitioned_asset_takes_its_keys_and_reads_it_per_key() {
        let dag = build_src(&format!(
            "{SALES}@asset(partitions={{\"region\": partitions_from(sales)}})\n\
def margin(region: str, sales: dict) -> dict:\n    return sales\n\n\
@asset(partitions={{\"region\": partitions_from(margin)}})\n\
def pct(region: str, margin: dict) -> dict:\n    return margin\n"
        ))
        .unwrap();
        let sales = &dag.get_node("t.py:sales").unwrap().decl.partitions;
        for (id, upstream) in [("t.py:margin", "sales"), ("t.py:pct", "margin")] {
            let n = dag.get_node(id).unwrap();
            assert_eq!(&n.decl.partitions, sales, "{id} has the keys of sales");
            assert_eq!(
                n.resolved_inputs.get(upstream).map(String::as_str),
                Some(format!("t.py:{upstream}").as_str()),
                "{id} reads {upstream} under its name"
            );
        }
    }

    #[test]
    fn partitions_from_an_inputs_entry_names_the_parameter() {
        let dag = build_src(&format!(
            "{SALES}@asset(inputs={{\"s\": sales}}, partitions={{\"region\": partitions_from(sales)}})\n\
def margin(region: str, s: dict) -> dict:\n    return s\n"
        ))
        .unwrap();
        let n = dag.get_node("t.py:margin").unwrap();
        assert_eq!(n.resolved_inputs.len(), 1);
        assert_eq!(n.resolved_inputs["s"], "t.py:sales");
    }

    #[test]
    fn partitions_from_a_list_asset_is_unchanged() {
        let dag = build_src(
            "from barca import asset, partitions_from\n\n\
@asset()\ndef keys() -> list:\n    return []\n\n\
@asset(partitions={\"k\": partitions_from(keys)})\ndef part(k: str) -> str:\n    return k\n",
        )
        .unwrap();
        let n = dag.get_node("t.py:part").unwrap();
        assert!(n.resolved_inputs.is_empty());
        assert!(matches!(
            n.decl.partitions["k"],
            barca_model::model::PartitionSpec::DerivedFrom { .. }
        ));
    }

    #[test]
    fn partitions_from_under_another_dimension_name_is_an_error() {
        let err = build_src(&format!(
            "{SALES}@asset(partitions={{\"r\": partitions_from(sales)}})\n\
def margin(r: str, sales: dict) -> dict:\n    return sales\n"
        ))
        .unwrap_err();
        assert!(matches!(err, DagError::PartitionsFrom { .. }), "{err}");
        assert!(err.to_string().contains("'region'"), "{err}");
        assert!(
            err.remediation()
                .unwrap()
                .contains("partitions_from(sales)")
        );
    }

    #[test]
    fn partitions_from_needs_one_dimension_on_both_sides() {
        let several = build_src(
            "from barca import asset, partitions, partitions_from\n\n\
@asset(partitions={\"a\": partitions([\"x\"]), \"b\": partitions([\"y\"])})\n\
def grid(a: str, b: str) -> str:\n    return a\n\n\
@asset(partitions={\"a\": partitions_from(grid)})\n\
def down(a: str, grid: str) -> str:\n    return grid\n",
        )
        .unwrap_err();
        assert!(
            several
                .to_string()
                .contains("2 partition dimensions (a, b)"),
            "{several}"
        );
        let mixed = build_src(&format!(
            "{SALES}@asset(partitions={{\"region\": partitions_from(sales), \"tier\": partitions([\"1\"])}})\n\
def margin(region: str, tier: str, sales: dict) -> dict:\n    return sales\n"
        ))
        .unwrap_err();
        assert!(
            mixed.to_string().contains("only partition dimension"),
            "{mixed}"
        );
    }

    #[test]
    fn partitioned_input_to_unpartitioned_consumer_is_an_error() {
        let err = build_src(&format!(
            "{SALES}@asset(inputs={{\"xs\": sales}})\ndef total(xs: list) -> int:\n    return 0\n"
        ))
        .unwrap_err();
        assert!(
            matches!(err, DagError::PartitionedInputToUnpartitioned { .. }),
            "{err}"
        );
        let fix = err.remediation().unwrap();
        assert!(
            fix.contains("collect(sales)") && fix.contains("partitions_from(sales)"),
            "{fix}"
        );
        // collect() is the way to read every partition.
        build_src(&format!(
            "{SALES}@asset(inputs={{\"xs\": collect(sales)}})\ndef total(xs: list) -> int:\n    return 0\n"
        ))
        .unwrap();
    }

    #[test]
    fn subgraph_many_is_the_union_of_cones_with_shared_upstream_once() {
        let src = "from barca import asset\n\n\
@asset()\ndef src() -> int:\n    return 1\n\n\
@asset(inputs={\"s\": src})\ndef a(s: int) -> int:\n    return s\n\n\
@asset(inputs={\"s\": src})\ndef b(s: int) -> int:\n    return s\n\n\
@asset()\ndef other() -> int:\n    return 2\n";
        let nodes = crate::parse::extract_nodes(src, "t.py").unwrap();
        let dag = crate::link::build(&nodes).unwrap();
        let union = dag.subgraph_many(&["t.py:a", "t.py:b"]);
        assert_eq!(union.len(), 3);
        assert_eq!(union[0], "t.py:src", "dependencies come first");
        assert!(union.contains(&"t.py:a") && union.contains(&"t.py:b"));
        assert!(!union.contains(&"t.py:other"));
        assert_eq!(dag.subgraph("t.py:a"), dag.subgraph_many(&["t.py:a"]));
        assert!(dag.subgraph_many(&["t.py:nope"]).is_empty());
    }

    /// The definition hash of `a` in a one-file pipeline, through the parser.
    fn definition_hash_of(source: &str) -> String {
        let source = &format!(
            "from barca import asset, sensor, task, sink, partitions, partitions_from\n{source}"
        );
        let nodes = crate::parse::extract_nodes(source, "test.py").unwrap();
        let dag = crate::link::build(&nodes).unwrap();
        dag.get_node("test.py:a")
            .unwrap()
            .fingerprint
            .definition_hash
            .clone()
    }

    const A: &str = "def a():\n    return 1\n";

    // Cached steps never reach a worker, so a new or edited sink has to change the hash, or it
    // would silently not be written.
    #[test]
    fn definition_hash_changes_when_sink_added() {
        assert_ne!(
            definition_hash_of(&format!("@asset()\n{A}")),
            definition_hash_of(&format!("@asset()\n@sink(\"exports/a.parquet\")\n{A}")),
        );
    }

    #[test]
    fn definition_hash_changes_when_sink_edited() {
        let hash = |sink: &str| definition_hash_of(&format!("@asset()\n@sink({sink})\n{A}"));
        assert_ne!(
            hash("\"exports/a.parquet\""),
            hash("\"exports/a.parquet\", serializer=\"pickle\""),
        );
        assert_ne!(hash("\"exports/a.parquet\""), hash("\"exports/b.parquet\""));
    }

    #[test]
    fn definition_hash_changes_when_serializer_changed() {
        assert_ne!(
            definition_hash_of(&format!("@asset()\n{A}")),
            definition_hash_of(&format!("@asset(serializer=\"parquet\")\n{A}")),
        );
    }

    /// The definition hash is over `source_text` (the canonical definition, see
    /// `crate::definition`), the cone and the kind. The fields the parser reads from arguments
    /// that do not count (`freshness`, `retries`, `description`, ...) are not part of it.
    #[test]
    fn definition_hash_ignores_what_does_not_count() {
        let plain = node("a");
        let mut other = node("a");
        other.freshness = barca_model::model::Freshness::Manual;
        other.retries = 5;
        other.retry_backoff_seconds = 2.0;
        other.timeout_seconds = 10;
        other.description = Some("d".to_string());
        other.tags.insert("team".to_string(), "a".to_string());
        other.env.push("HOME".to_string());
        let d1 = crate::link::build(std::slice::from_ref(&plain)).unwrap();
        let d2 = crate::link::build(std::slice::from_ref(&other)).unwrap();
        assert_eq!(
            d1.get_node("test.py:a")
                .unwrap()
                .fingerprint
                .definition_hash,
            d2.get_node("test.py:a")
                .unwrap()
                .fingerprint
                .definition_hash,
        );

        let mut sensor = node("a");
        sensor.kind = NodeKind::Sensor;
        let d3 = crate::link::build(std::slice::from_ref(&sensor)).unwrap();
        assert_ne!(
            d1.get_node("test.py:a")
                .unwrap()
                .fingerprint
                .definition_hash,
            d3.get_node("test.py:a")
                .unwrap()
                .fingerprint
                .definition_hash,
            "the kind counts"
        );
    }

    /// Issue #283, end to end through the parser: decorator edits that do not change the
    /// result leave the definition hash alone; edits that can change it do not.
    #[test]
    fn definition_hash_covers_the_result_and_nothing_else() {
        // The partition dimensions are parameters of the function (#549).
        const A: &str = "def a(region=None, area=None):\n    return 1\n";
        let base = definition_hash_of(&format!(
            "@asset(partitions={{\"region\": partitions([\"us\", \"eu\"])}})\n{A}"
        ));
        for same in [
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\", \"apac\"])})\n",
            "@asset(partitions={\"region\": partitions([\"eu\"])})\n",
            "@asset(partitions={\"region\": partitions([\"eu\", \"us\"])})\n",
            "@asset(partitions={\"region\": partitions(REGIONS)})\n",
            "@asset(partitions={\"region\": partitions(regions())})\n",
            "@asset(partitions={\"region\": partitions([r for r in REGIONS])})\n",
            "@asset(\n    partitions = {'region': partitions(['us', 'eu',]),},  # keys\n)\n",
            "@asset(description=\"d\", partitions={\"region\": partitions([\"us\", \"eu\"])})\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, tags={\"a\": \"b\"}, retries=3)\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, freshness=Manual)\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, timeout_seconds=5, retry_backoff=1.5)\n",
        ] {
            assert_eq!(base, definition_hash_of(&format!("{same}{A}")), "{same}");
        }
        for different in [
            "@asset(partitions={\"area\": partitions([\"us\", \"eu\"])})\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, serializer=\"pickle\")\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})\n@sink(\"a.json\")\n",
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})\n@functools.cache\n",
            "@asset()\n",
        ] {
            assert_ne!(
                base,
                definition_hash_of(&format!("{different}{A}")),
                "{different}"
            );
        }
        assert_ne!(
            base,
            definition_hash_of(
                "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})\ndef a(region=None, area=None):\n    return 2\n"
            ),
            "the body counts"
        );
    }
}
