//! What a run's events say about its steps so far: kept per run beside its event channel
//! ([`crate::replay`]) and read by the `running` views ([`crate::running`]).

use barca_core::{RunEvent, StepId};
use std::collections::{HashMap, HashSet};

/// What one run's events say about its steps.
#[derive(Default)]
pub(crate) struct Progress {
    /// Nodes that printed a line or finished a step (any key).
    touched: HashSet<String>,
    /// Unpartitioned nodes whose step finished.
    finished: HashSet<String>,
    /// Partitioned nodes: the keys this run finished with a result.
    keys_done: HashMap<String, HashSet<String>>,
    /// When the run started executing (its `RunStarted` event), unix seconds.
    started: Option<f64>,
    /// When its latest step finished, unix seconds.
    last_step: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProgressSnapshot {
    pub executing: HashMap<String, usize>,
    pub touched: HashSet<String>,
    pub finished: HashSet<String>,
    pub keys_done: HashMap<String, usize>,
    pub started: Option<f64>,
    pub last_step: Option<f64>,
}
impl Progress {
    pub(crate) fn observe(&mut self, event: &RunEvent) {
        match event {
            RunEvent::RunStarted { .. } => {
                self.started = Some(barca_core::time::now_secs_f64());
            }
            RunEvent::Log { node_id, .. } => {
                self.touched
                    .insert(StepId::parse(node_id).base_id().to_string());
            }
            RunEvent::StepFinished { node_id, ok, .. } => {
                let id = StepId::parse(node_id);
                // A `parallel()` child is not a step of the plan.
                if id.partition.0.contains_key("_branch") {
                    return;
                }
                let base = id.base_id().to_string();
                self.last_step = Some(barca_core::time::now_secs_f64());
                self.touched.insert(base.clone());
                if id.partition.0.is_empty() {
                    self.finished.insert(base);
                } else if *ok {
                    self.keys_done.entry(base).or_default().insert(id.display());
                }
            }
            _ => {}
        }
    }
    pub(crate) fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            executing: HashMap::new(),
            touched: self.touched.clone(),
            finished: self.finished.clone(),
            keys_done: self
                .keys_done
                .iter()
                .map(|(node, keys)| (node.clone(), keys.len()))
                .collect(),
            started: self.started,
            last_step: self.last_step,
        }
    }
}
