//! What a step's trace says about it: where its node sits in the pipeline ([`NodeContext`])
//! and the trace context a worker's `barca.execute` span joins ([`StepTrace::context`]). The
//! worker loop attaches it to each step it sends; the Datadog exporter
//! (`barca_core::telemetry::datadog`) sends the traces themselves.

use barca_model::trace_ids::{step_span_id, step_trace_id, step_trace_id_high};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// barca's own version, the `barca.version` tag of every span and metric.
pub const BARCA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where a node sits in the pipeline: what every report of one of its steps says about it,
/// whichever partition key or attempt the step is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeContext {
    /// The node id without a partition key, e.g. `pipeline.py:orders`.
    pub base_id: String,
    /// The node's short name, `orders`, unique within the pipeline. When two files define a
    /// node of the same name, both are named by their id, `pipeline.py:orders`.
    pub name: String,
    /// The file that defines the node, as barca names it (`pipeline.py`).
    pub file: String,
    /// The names of the `group()`s the node is in, outermost first, joined by `/`
    /// (`Experiment/Training`). `None` for a node outside every group.
    pub group: Option<String>,
    /// `asset`, `task` or `sensor`.
    pub kind: &'static str,
    /// `always`, `manual` or `schedule`.
    pub freshness: &'static str,
    /// The cron expression of a scheduled node.
    pub schedule: Option<String>,
}

impl NodeContext {
    /// A context for an id barca has no definition for (a node removed while a server ran).
    pub fn unknown(base_id: &str) -> Self {
        Self {
            base_id: base_id.to_string(),
            name: short_name(base_id).to_string(),
            file: base_id
                .rsplit_once(':')
                .map_or("", |(file, _)| file)
                .to_string(),
            group: None,
            kind: "asset",
            freshness: "always",
            schedule: None,
        }
    }
}

/// The part of a node id after its file: `orders` for `pipeline.py:orders`.
pub fn short_name(base_id: &str) -> &str {
    base_id.rsplit_once(':').map_or(base_id, |(_, name)| name)
}

/// The [`NodeContext`] of every node of `dag`, by base id.
pub fn node_contexts(
    dag: &barca_compile::dag::Dag,
) -> std::collections::HashMap<String, NodeContext> {
    use barca_model::model::{Freshness, NodeKind};
    use std::collections::HashMap;

    let mut name_count: HashMap<&str, usize> = HashMap::new();
    for id in dag.topo_order() {
        *name_count.entry(short_name(id)).or_default() += 1;
    }
    // Groups name their members by node id or group id; a member has at most one parent.
    let mut parent: HashMap<&str, &barca_compile::groups::NodeGroup> = HashMap::new();
    for group in &dag.groups {
        for member in &group.members {
            parent.insert(member.as_str(), group);
        }
    }
    let group_path = |id: &str| {
        let mut names = Vec::new();
        let mut at = id;
        while let Some(group) = parent.get(at) {
            if names.len() > dag.groups.len() {
                break; // a cycle; groups are validated against these, but never loop
            }
            names.push(group.name.as_str());
            at = group.id.as_str();
        }
        names.reverse();
        (!names.is_empty()).then(|| names.join("/"))
    };

    let mut out = HashMap::new();
    for id in dag.topo_order() {
        let Some(node) = dag.get_node(id) else {
            continue;
        };
        let short = short_name(id);
        let (freshness, schedule) = match &node.extracted.freshness {
            Freshness::Always => ("always", None),
            Freshness::Manual => ("manual", None),
            Freshness::Schedule(cron) => ("schedule", Some(cron.0.clone())),
        };
        out.insert(
            id.to_string(),
            NodeContext {
                base_id: id.to_string(),
                name: if name_count.get(short).copied().unwrap_or(0) > 1 {
                    id.to_string()
                } else {
                    short.to_string()
                },
                file: id.rsplit_once(':').map_or("", |(file, _)| file).to_string(),
                group: group_path(id),
                kind: match node.kind() {
                    NodeKind::Asset => "asset",
                    NodeKind::Task => "task",
                    NodeKind::Sensor => "sensor",
                },
                freshness,
                schedule,
            },
        );
    }
    out
}

/// The tags that place a node in the pipeline.
pub fn node_meta(meta: &mut Map<String, Value>, node: &NodeContext) {
    meta.insert("barca.asset".into(), json!(node.name));
    meta.insert("barca.file".into(), json!(node.file));
    meta.insert("barca.kind".into(), json!(node.kind));
    meta.insert("barca.freshness".into(), json!(node.freshness));
    if let Some(group) = &node.group {
        meta.insert("barca.group".into(), json!(group));
    }
    if let Some(cron) = &node.schedule {
        meta.insert("barca.schedule".into(), json!(cron));
    }
}

/// One run's trace context: what each step sent to a worker carries, so the worker's
/// `barca.execute` span joins the step's trace.
#[derive(Debug, Clone)]
pub struct StepTrace {
    pub run_id: String,
    /// `DD_SERVICE`, or `barca`: the one service every span reports as.
    pub service: String,
    pub project: Option<String>,
    pub job: String,
    pub nodes: HashMap<String, Arc<NodeContext>>,
}

impl StepTrace {
    pub fn new(
        dag: &barca_compile::dag::Dag,
        job: &str,
        run_id: &str,
        service: String,
        project: Option<String>,
    ) -> Self {
        Self {
            run_id: run_id.to_string(),
            service,
            project,
            job: job.to_string(),
            nodes: node_contexts(dag)
                .into_iter()
                .map(|(id, ctx)| (id, Arc::new(ctx)))
                .collect(),
        }
    }

    pub fn node(&self, base: &str) -> Arc<NodeContext> {
        self.nodes
            .get(base)
            .cloned()
            .unwrap_or_else(|| Arc::new(NodeContext::unknown(base)))
    }

    /// The `datadog` object of a step sent to a worker: the step's trace and its root span.
    /// `reported` is the step whose trace the execution belongs in (itself, or a `parallel()`
    /// call's nearest reported ancestor).
    pub fn context(&self, reported: &barca_model::StepId, attempt: u32) -> Value {
        let base = reported.base_id();
        let display = reported.display();
        let node = self.node(base);
        let mut tags = Map::new();
        tags.insert("barca.run_id".into(), json!(self.run_id));
        tags.insert("barca.job".into(), json!(self.job));
        tags.insert("barca.node_id".into(), json!(display));
        node_meta(&mut tags, &node);
        if !reported.partition.is_empty() {
            tags.insert("barca.partition".into(), json!(reported.partition.suffix()));
        }
        tags.insert("barca.version".into(), json!(BARCA_VERSION));
        if let Some(project) = &self.project {
            tags.insert("barca.project".into(), json!(project));
        }
        json!({
            "trace_id": step_trace_id(&self.run_id, &display),
            "trace_id_high": step_trace_id_high(&self.run_id, &display),
            "parent_id": step_span_id(&self.run_id, &display),
            "service": self.service,
            "resource": node.name,
            "tags": tags,
            "attempt": attempt,
        })
    }
}
