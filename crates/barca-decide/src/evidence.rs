//! Shared rebuildable freshness evidence: execution publishes it and inspection validates it.
use barca_model::{
    dag::Dag,
    model::{Freshness, NodeKind},
    source_observation::Source,
    step_result::OutputRef,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Proof {
    pub signature: String,
    pub count: usize,
    pub output_hash: Option<String>,
    #[serde(default)]
    pub observation_hash: Option<String>,
    pub output: Option<OutputRef>,
    pub run_hash: Option<String>,
    pub source: Option<Arc<Source>>,
    /// Cached historical selections that differ from the last materialization.
    #[serde(default)]
    pub selected: HashMap<String, String>,
}

pub fn key(cfg: &barca_model::config::ResolvedConfig) -> String {
    format!(
        "freshness-v3:{:x}",
        Sha256::digest(serde_json::to_vec(&(&cfg.artifact_root, &cfg.local_artifact_dir)).unwrap())
    )
}
pub fn signatures(dag: &Dag, ids: &HashSet<String>) -> HashMap<String, String> {
    let mut signatures = HashMap::new();
    for id in dag.order_for(ids.iter().map(String::as_str)) {
        // A planned node is in the DAG; a proof recorded for a node the source no longer
        // has is never asked about (`ids` are the plan's), and would read as changed.
        let Some(n) = dag.get_node(id).filter(|_| ids.contains(id)) else {
            continue;
        };
        let mut upstream: Vec<_> = dag
            .upstream(id)
            .into_iter()
            .map(|up| {
                (
                    up,
                    if crate::cache::is_cutoff(dag, up)
                        || dag.get_node(up).is_some_and(|n| {
                            matches!(n.decl.freshness, Freshness::Manual)
                                && n.kind() == NodeKind::Asset
                        })
                    {
                        None
                    } else {
                        signatures.get(up)
                    },
                )
            })
            .collect();
        upstream.sort_by_key(|(id, _)| *id);
        let env = barca_model::envdeps::hash_input(&barca_model::envdeps::resolve(&n.decl.env));
        let bytes = serde_json::to_vec(
            &serde_json::to_value(&(
                n.fingerprint.definition_hash.as_str(),
                &n.resolved_inputs,
                &n.resolved_collected,
                &n.decl.freshness,
                &n.decl.sinks,
                &n.decl.partitions,
                env,
                upstream,
            ))
            .unwrap(),
        )
        .unwrap();
        signatures.insert(id.to_owned(), format!("{:x}", Sha256::digest(bytes)));
    }
    signatures
}
