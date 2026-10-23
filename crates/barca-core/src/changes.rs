//! What changed since a node's last successful materialization (#558).
//!
//! A run hash is one SHA-256 over several components: the function, the decorator parts that
//! count, the helper code the function reaches, the node's kind, its inputs' run hashes (and a
//! sensor's output), and its declared env values ([`crate::hash`]). One hash cannot say which
//! of them moved, so each successful materialization also records a short digest of every
//! component (`materializations.components`, a JSON object). `barca status` and `--dry-run`
//! compare the current components with the ones recorded by the node's last success and name
//! the ones that differ:
//!
//! - `code`: the function from `def` on;
//! - `decorator argument NAME`: a counted argument of `@asset` / `@sensor` / `@task`;
//! - `decorator NAME`: another counted decorator (`@sink`, `@functools.lru_cache`, ...);
//! - `helper LABEL`: a definition the function reaches (`pipelines/common.py:bucket_ltv`, or
//!   `pipeline.py:RATE` in the node's own file), or a module reached whole
//!   (`pipelines/common.py`);
//! - `input ID` / `sensor ID`: an upstream's run hash, or a sensor's run hash and output;
//! - `value ID`: the value a dict `partitions_from` source `ID` returned for this key, with the
//!   source's run hash ([`crate::keyed`]);
//! - `env NAME`: a declared environment variable's value;
//! - `kind`.
//!
//! The digests are never part of a cache key: they explain a miss, they do not decide one. A
//! row recorded before they existed has none, and the reason stays a plain `changed`.

use crate::cache::DecideState;
use crate::dag::Dag;
use crate::model::ExtractedNode;
use std::collections::BTreeMap;

/// The components of `node`'s definition hash and a digest of each: the counted decorator
/// parts, the function, the helpers in its cone, the kind.
pub(crate) fn definition_components(node: &ExtractedNode) -> Vec<(String, String)> {
    let mut parts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add = |key: String, text: &str| {
        parts
            .entry(key)
            .or_default()
            .push(crate::hash::short_digest(text.as_bytes()));
    };
    // The definition text is the counted parts, one per line, then the function from `def` on
    // ([`crate::definition::node_definition`]).
    let mut rest = node.source_text.as_str();
    loop {
        let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
        if let Some(argument) = line.strip_prefix("argument ") {
            let name = argument.split('=').next().unwrap_or(argument);
            add(format!("decorator argument {name}"), line);
        } else if let Some(decorator) = line.strip_prefix("decorator ") {
            let name = decorator.split('(').next().unwrap_or(decorator);
            add(format!("decorator {name}"), line);
        } else {
            break;
        }
        rest = after;
    }
    add("code".to_string(), rest);
    for (label, digest) in &node.cone_parts {
        // A definition of the node's own file is labelled by its bare name.
        let label = if label.contains(':') || label.ends_with(".py") {
            label.clone()
        } else {
            format!("{}:{label}", node.source_file)
        };
        parts
            .entry(format!("helper {label}"))
            .or_default()
            .push(digest.clone());
    }
    let mut out: Vec<(String, String)> = parts
        .into_iter()
        .map(|(key, mut digests)| {
            digests.sort();
            (key, digests.join(","))
        })
        .collect();
    out.push((
        "kind".to_string(),
        format!("{:?}", node.kind).to_lowercase(),
    ));
    out
}

/// Every component of the run hash of `display_id` (`pipeline.py:p` or `pipeline.py:p[k=a]`)
/// as this run computed it, as a JSON object. `None` for a node not in the DAG. The upstream
/// run hashes and sensor outputs are read from `state`, which holds them once the step is
/// hashed.
pub(crate) fn step_components(
    dag: &Dag,
    state: &DecideState,
    display_id: &str,
) -> Option<BTreeMap<String, String>> {
    let step = crate::StepId::parse(display_id);
    let node = dag.get_node(step.base_id())?;
    let mut out: BTreeMap<String, String> = node.components.iter().cloned().collect();
    let partition = (!step.partition.is_empty()).then(|| step.partition.suffix());
    let mut upstreams: Vec<&String> = node
        .resolved_inputs
        .values()
        .chain(node.resolved_collected.values())
        .collect();
    upstreams.sort();
    upstreams.dedup();
    for up in upstreams {
        let tokens = crate::cache::upstream_tokens(
            up,
            partition.as_deref(),
            state.run_hashes(),
            &state.sensor_outputs,
            crate::cache::is_cutoff(dag, up),
        );
        if tokens.is_empty() {
            continue;
        }
        let word = match dag.get_node(up).map(|n| n.kind()) {
            Some(crate::NodeKind::Sensor) => "sensor",
            _ => "input",
        };
        out.insert(
            format!("{word} {up}"),
            crate::hash::short_digest(tokens.join("\n").as_bytes()),
        );
    }
    // A dict `partitions_from` source's value for this key (`crate::keyed`).
    for (source, token) in state.keyed_tokens(step.base_id(), &step.partition.0) {
        out.insert(
            format!("value {source}"),
            crate::hash::short_digest(token.as_bytes()),
        );
    }
    for (name, value) in crate::envdeps::resolve(&node.extracted.env) {
        let value = serde_json::to_string(&value).unwrap_or_default();
        out.insert(
            format!("env {name}"),
            crate::hash::short_digest(value.as_bytes()),
        );
    }
    Some(out)
}

/// [`step_components`] as the JSON text recorded with a materialization.
pub(crate) fn step_components_json(
    dag: &Dag,
    state: &DecideState,
    display_id: &str,
) -> Option<String> {
    step_components(dag, state, display_id).and_then(|c| serde_json::to_string(&c).ok())
}

/// The components that differ between `recorded` (the JSON a materialization recorded) and
/// `current`, in the order the module documentation lists them. `None` when `recorded` cannot be
/// read or nothing differs.
pub(crate) fn diff(recorded: &str, current: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let recorded: BTreeMap<String, String> = serde_json::from_str(recorded).ok()?;
    let mut changed: Vec<&String> = recorded
        .keys()
        .chain(current.keys())
        .filter(|key| recorded.get(*key) != current.get(*key))
        .collect();
    changed.sort_by_key(|key| (rank(key), *key));
    changed.dedup();
    (!changed.is_empty()).then(|| changed.into_iter().cloned().collect())
}

fn rank(key: &str) -> usize {
    const ORDER: &[&str] = &[
        "code",
        "decorator argument ",
        "decorator ",
        "helper ",
        "input ",
        "sensor ",
        "value ",
        "env ",
        "kind",
    ];
    ORDER
        .iter()
        .position(|prefix| key.starts_with(prefix))
        .unwrap_or(ORDER.len())
}

/// What changed in `display_id`'s run hash since its last successful materialization, when that
/// row recorded its components. `None` when it did not (recorded before 0.29), when there is no
/// such row, or against a server's ledger (its rows are other machines').
pub(crate) async fn since_last_success(
    cache: Option<&crate::db::CacheReader>,
    dag: &Dag,
    state: &DecideState,
    display_id: &str,
) -> Option<Vec<String>> {
    let cache = cache?;
    if cache.ledger().is_some() {
        return None;
    }
    let recorded = crate::db::last_success_components(cache, display_id).await?;
    let current = step_components(dag, state, display_id)?;
    diff(&recorded, &current)
}

/// The words for a list of changed components: the first, and how many more.
pub fn summary(changed: &[String]) -> String {
    match changed {
        [] => String::new(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} (+{} more)", rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(source_text: &str, cone_parts: &[(&str, &str)]) -> ExtractedNode {
        ExtractedNode {
            kind: crate::NodeKind::Asset,
            function_name: "clean".to_string(),
            explicit_name: None,
            freshness: crate::Freshness::Always,
            max_failures: None,
            schedule_timeout: None,
            inputs: Default::default(),
            partitions: Default::default(),
            sinks: Default::default(),
            timeout_seconds: 300,
            retries: 1,
            cutoff: false,
            weight: 1,
            deterministic: true,
            retry_backoff_seconds: 0.0,
            description: None,
            tags: Default::default(),
            is_unsafe: false,
            source_file: "pipeline.py".to_string(),
            byte_offset: 0,
            source_text: source_text.to_string(),
            cone_hash: String::new(),
            cone_parts: cone_parts
                .iter()
                .map(|(l, d)| (l.to_string(), d.to_string()))
                .collect(),
            artifact_serializer: None,
            param_types: Default::default(),
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

    fn keys(components: &[(String, String)]) -> Vec<&str> {
        components.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn the_definition_splits_into_decorators_arguments_code_helpers_and_kind() {
        let n = node(
            "decorator functools.lru_cache(maxsize=None)\ndecorator sink(\"a.json\")\n\
             decorator sink(\"b.json\")\nargument serializer=\"json\"\ndef clean():\n    return 1",
            &[("RATE", "aa"), ("pipelines/common.py:bucket", "bb")],
        );
        let c = definition_components(&n);
        assert_eq!(
            keys(&c),
            [
                "code",
                "decorator argument serializer",
                "decorator functools.lru_cache",
                "decorator sink",
                "helper pipeline.py:RATE",
                "helper pipelines/common.py:bucket",
                "kind",
            ]
        );
        let code = &c.iter().find(|(k, _)| k == "code").unwrap().1;
        assert_eq!(
            code,
            &crate::hash::short_digest(b"def clean():\n    return 1")
        );
        // Two sinks share one entry.
        assert!(
            c.iter()
                .any(|(k, v)| k == "decorator sink" && v.contains(','))
        );
    }

    #[test]
    fn a_change_in_one_component_names_only_it() {
        let before = node(
            "argument serializer=\"json\"\ndef clean():\n    return 1",
            &[("pipelines/common.py:bucket", "bb")],
        );
        let after = node(
            "argument serializer=\"json\"\ndef clean():\n    return 1",
            &[("pipelines/common.py:bucket", "cc")],
        );
        let recorded: BTreeMap<_, _> = definition_components(&before).into_iter().collect();
        let current: BTreeMap<_, _> = definition_components(&after).into_iter().collect();
        assert_eq!(
            diff(&serde_json::to_string(&recorded).unwrap(), &current),
            Some(vec!["helper pipelines/common.py:bucket".to_string()])
        );
        assert_eq!(
            diff(&serde_json::to_string(&current).unwrap(), &current),
            None
        );
    }

    #[test]
    fn added_and_removed_components_count_and_code_comes_first() {
        let recorded = r#"{"code":"1","helper a.py:x":"2","input p.py:up":"3"}"#;
        let current: BTreeMap<String, String> = [
            ("code", "9"),
            ("helper a.py:y", "2"),
            ("input p.py:up", "4"),
            ("kind", "asset"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(
            diff(recorded, &current).unwrap(),
            [
                "code",
                "helper a.py:x",
                "helper a.py:y",
                "input p.py:up",
                "kind"
            ]
        );
        assert_eq!(diff("not json", &current), None);
    }

    #[test]
    fn the_summary_names_the_first_and_counts_the_rest() {
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(summary(&list(&["code"])), "code");
        assert_eq!(summary(&list(&["code", "kind", "env X"])), "code (+2 more)");
    }
}
