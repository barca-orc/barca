//! Why a run executed what it did (#925): as each phase of a run is decided, every step that is
//! not a cache hit gets the reason the engine has for it, and the run keeps one aggregate of
//! them (`barca_model::run_explain::PlanReasons`): by `(category, subject)`, how many nodes and
//! steps. It is recorded on the run's row while the run goes and with its outcome, so
//! `GET /runs/{run_id}` and `barca history RUN_ID` say why a run is a full rebuild.
//!
//! The reasons come from the decision itself (a task, a sensor, a refresh, a missing artifact)
//! or, for a step with no cache hit, from its history ([`barca_db::db::step_histories`]): no
//! success ever (never run, or new keys of a partitioned node), the last attempt at this run
//! hash failed, or what changed since the last success, the same component diff
//! `barca status` and `--dry-run` make ([`crate::changes`]). Only steps that are not cache hits
//! are looked up, so a run served from cache pays nothing.

use crate::cache::{DecideState, RunReason};
use barca_compile::dag::Dag;
use barca_model::run_explain::{
    EXAMINED_KEYS_PER_NODE, MAX_REASONS, PlanReason, PlanReasons, ReasonCategory,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// One reason: its category and subject.
pub(crate) type Reason = (ReasonCategory, Option<String>);

/// The reasons of a run so far.
#[derive(Default, Clone, Debug)]
pub struct PlanTally {
    entries: HashMap<Reason, Entry>,
    nodes: HashSet<String>,
    steps: u64,
    /// Changed since [`PlanTally::take_changed`] last returned it.
    dirty: bool,
    /// Time spent looking up reasons, for `BARCA_TRACE_TIMING`.
    pub lookup: std::time::Duration,
}

#[derive(Clone, Debug)]
struct Entry {
    nodes: HashSet<String>,
    steps: u64,
    example: String,
}

impl PlanTally {
    /// `steps` steps of node `base`, each executed for every one of `reasons`.
    pub fn add(&mut self, base: &str, reasons: &[Reason], steps: u64) {
        if steps == 0 {
            return;
        }
        self.dirty = true;
        self.steps += steps;
        if !self.nodes.contains(base) {
            self.nodes.insert(base.to_string());
        }
        for reason in reasons {
            let entry = self.entries.entry(reason.clone()).or_insert_with(|| Entry {
                nodes: HashSet::new(),
                steps: 0,
                example: base.to_string(),
            });
            entry.steps += steps;
            if !entry.nodes.contains(base) {
                entry.nodes.insert(base.to_string());
            }
        }
    }

    /// The aggregate, most steps first; `None` before any step was decided to run.
    pub(crate) fn summary(&self) -> Option<PlanReasons> {
        if self.steps == 0 {
            return None;
        }
        let mut reasons: Vec<PlanReason> = self
            .entries
            .iter()
            .map(|((category, subject), e)| PlanReason {
                category: *category,
                subject: subject.clone(),
                nodes: e.nodes.len() as u64,
                steps: e.steps,
                example: e.example.clone(),
            })
            .collect();
        reasons.sort_by(|a, b| {
            b.steps
                .cmp(&a.steps)
                .then(b.nodes.cmp(&a.nodes))
                .then(a.category.cmp(&b.category))
                .then(a.subject.cmp(&b.subject))
        });
        let omitted = reasons.len().saturating_sub(MAX_REASONS) as u64;
        reasons.truncate(MAX_REASONS);
        Some(PlanReasons {
            nodes: self.nodes.len() as u64,
            steps: self.steps,
            reasons,
            omitted,
        })
    }

    /// The aggregate as JSON when it changed since the last call.
    pub fn take_changed(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        self.json()
    }

    /// The aggregate as JSON, `None` before any step was decided to run.
    pub fn json(&self) -> Option<String> {
        serde_json::to_string(&self.summary()?).ok()
    }
}

/// The reason a decision gives by itself, or `None` when the step simply had no cache hit
/// ([`RunReason::NotMaterialized`]) and its history says why.
pub fn of_run_reason(base: &str, reason: &RunReason) -> Option<Reason> {
    Some(match reason {
        RunReason::Task => (ReasonCategory::Task, None),
        RunReason::Sensor => (ReasonCategory::Sensor, None),
        RunReason::Refresh => (ReasonCategory::Refresh, Some(base.to_string())),
        RunReason::RefreshCascade { root } => (ReasonCategory::RefreshCascade, Some(root.clone())),
        RunReason::RefreshAll => (ReasonCategory::RefreshAll, None),
        RunReason::ArtifactMissing => (ReasonCategory::ArtifactMissing, None),
        RunReason::Evicted => (ReasonCategory::Evicted, None),
        RunReason::NotMaterialized => return None,
    })
}

/// The steps of one phase with no cache hit, whose history says why.
#[derive(Default)]
pub struct Missed {
    /// Unpartitioned steps (display ids).
    pub steps: Vec<String>,
    /// Partitioned steps: the base id, its keys that run (display ids), and whether some of
    /// its keys were cache hits.
    pub keys: Vec<(String, Vec<String>, bool)>,
}

impl Missed {
    pub(crate) fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.keys.is_empty()
    }
}

/// Look up why each of `missed` had no cache hit, and add it to the run's tally.
pub async fn explain(
    cache: &crate::cache::CacheReader,
    dag: &Dag,
    state: &mut DecideState,
    missed: Missed,
) {
    if missed.is_empty() {
        return;
    }
    let started = std::time::Instant::now();
    explain_missed(cache, dag, state, missed).await;
    state.plan_tally.lookup += started.elapsed();
}

async fn explain_missed(
    cache: &crate::cache::CacheReader,
    dag: &Dag,
    state: &mut DecideState,
    missed: Missed,
) {
    let not_examined = [(ReasonCategory::NotExamined, None)];
    // A server's ledger answered the lookups: this machine's history is not the one to ask.
    if cache.ledger().is_some() {
        for id in &missed.steps {
            state
                .plan_tally
                .add(barca_model::StepId::parse(id).base_id(), &not_examined, 1);
        }
        for (base, keys, _) in &missed.keys {
            state.plan_tally.add(base, &not_examined, keys.len() as u64);
        }
        return;
    }
    let mut ids: Vec<&str> = missed.steps.iter().map(String::as_str).collect();
    for (_, keys, _) in &missed.keys {
        ids.extend(keys.iter().take(EXAMINED_KEYS_PER_NODE).map(String::as_str));
    }
    let histories = barca_db::db::step_histories(cache, &ids).await;
    let mut explainer = Explainer {
        cache,
        dag,
        state,
        definitions: HashMap::new(),
    };
    let mut add: Vec<(String, Vec<Reason>, u64)> = Vec::new();
    for id in &missed.steps {
        let reasons = explainer.reasons(id, histories.get(id), false).await;
        add.push((
            barca_model::StepId::parse(id).base_id().to_string(),
            reasons,
            1,
        ));
    }
    for (base, keys, any_cached) in &missed.keys {
        let examined = &keys[..keys.len().min(EXAMINED_KEYS_PER_NODE)];
        let succeeded = *any_cached
            || examined
                .iter()
                .any(|k| histories.get(k).is_some_and(|h| h.last_success.is_some()))
            || barca_db::db::any_key_succeeded(cache, base).await;
        // Keys with the same reasons are added together.
        let mut groups: BTreeMap<Vec<Reason>, u64> = BTreeMap::new();
        for (i, key) in examined.iter().enumerate() {
            if i % 1024 == 1023 {
                tokio::task::yield_now().await;
            }
            let reasons = explainer.reasons(key, histories.get(key), succeeded).await;
            *groups.entry(reasons).or_default() += 1;
        }
        let rest = keys.len() - examined.len();
        if rest > 0 {
            *groups.entry(not_examined.to_vec()).or_default() += rest as u64;
        }
        for (reasons, n) in groups {
            add.push((base.clone(), reasons, n));
        }
    }
    for (base, reasons, n) in add {
        state.plan_tally.add(&base, &reasons, n);
    }
}

struct Explainer<'a> {
    cache: &'a crate::cache::CacheReader,
    dag: &'a Dag,
    state: &'a DecideState,
    /// A recorded definition hash -> what differs between it and the current definition of
    /// the node (`None`: its digests are not recorded), per node.
    definitions: HashMap<(String, String), Option<Vec<String>>>,
}

impl Explainer<'_> {
    /// Why `id` had no cache hit. `node_succeeded`: for a key, whether another key of its node
    /// has a success (then a key without one is new, not never run).
    async fn reasons(
        &mut self,
        id: &str,
        history: Option<&barca_db::db::StepHistory>,
        node_succeeded: bool,
    ) -> Vec<Reason> {
        let partitioned = id.contains('[');
        let never = || {
            vec![if partitioned && node_succeeded {
                (ReasonCategory::NewKeys, None)
            } else {
                (ReasonCategory::NeverRun, None)
            }]
        };
        let Some(history) = history else {
            return never();
        };
        let current_hash = self.state.run_hashes().get(id).map(String::as_str);
        if history.latest_status == "failed" && current_hash == Some(&history.latest_run_hash) {
            return vec![(ReasonCategory::Failed, None)];
        }
        let Some(success) = &history.last_success else {
            return never();
        };
        // Recorded at this run hash and still no hit: its artifact cannot be read.
        if current_hash == Some(success.run_hash.as_str()) {
            return vec![(ReasonCategory::ArtifactMissing, None)];
        }
        let changed = match &success.components {
            Some(recorded) => self.changed(id, recorded).await,
            None => None,
        };
        match changed {
            Some(changed) if !changed.is_empty() => changed
                .into_iter()
                .map(|c| (ReasonCategory::of_component(&c), Some(c)))
                .collect(),
            _ => vec![(ReasonCategory::Changed, None)],
        }
    }

    /// What differs between the components `recorded` and `id`'s now: the definition's diffed
    /// once per recorded definition, the step's own each time.
    async fn changed(&mut self, id: &str, recorded: &str) -> Option<Vec<String>> {
        let base = barca_model::StepId::parse(id).base_id().to_string();
        let node = self.dag.get_node(&base)?;
        let mut own: BTreeMap<String, String> = serde_json::from_str(recorded).ok()?;
        let Some(definition) = own.remove(crate::changes::DEFINITION_KEY) else {
            // Recorded before 0.37.1: everything in the row.
            let current = crate::changes::step_components(self.dag, self.state, id)?;
            return crate::changes::diff(recorded, &current);
        };
        let mut changed = Vec::new();
        if definition != node.fingerprint.definition_hash {
            let key = (base, definition);
            if !self.definitions.contains_key(&key) {
                let diff = barca_db::db::definition_components(self.cache, &key.1)
                    .await
                    .map(|old| {
                        let current: BTreeMap<String, String> =
                            node.fingerprint.components.iter().cloned().collect();
                        crate::changes::diff(&old, &current).unwrap_or_default()
                    });
                self.definitions.insert(key.clone(), diff);
            }
            changed.extend(self.definitions[&key].clone()?);
        }
        let current = crate::changes::own_components(self.dag, self.state, id)?;
        let own = serde_json::to_string(&own).ok()?;
        changed.extend(crate::changes::diff(&own, &current).unwrap_or_default());
        changed.sort_by_key(|key| (crate::changes::rank(key), key.clone()));
        changed.dedup();
        Some(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tally_counts_nodes_once_and_steps_per_reason() {
        let mut t = PlanTally::default();
        let code = (
            ReasonCategory::CodeChanged,
            Some("helper a.py:f".to_string()),
        );
        let input = (
            ReasonCategory::InputsChanged,
            Some("input a.py:x".to_string()),
        );
        t.add("a.py:p", std::slice::from_ref(&code), 1);
        t.add("a.py:q", &[code.clone(), input.clone()], 1);
        t.add("a.py:k", &[(ReasonCategory::NewKeys, None)], 40);
        t.add("a.py:k", &[(ReasonCategory::NewKeys, None)], 2);
        let s = t.summary().unwrap();
        assert_eq!((s.nodes, s.steps, s.omitted), (3, 44, 0));
        assert_eq!(s.reasons[0].category, ReasonCategory::NewKeys);
        assert_eq!((s.reasons[0].nodes, s.reasons[0].steps), (1, 42));
        assert_eq!(s.reasons[1].subject.as_deref(), Some("helper a.py:f"));
        assert_eq!((s.reasons[1].nodes, s.reasons[1].steps), (2, 2));
        assert_eq!(s.reasons[1].example, "a.py:p");
        assert_eq!((s.reasons[2].nodes, s.reasons[2].steps), (1, 1));
    }

    #[test]
    fn the_tally_is_bounded_and_says_what_it_left_out() {
        let mut t = PlanTally::default();
        for i in 0..(MAX_REASONS + 5) {
            t.add(
                &format!("a.py:n{i}"),
                &[(
                    ReasonCategory::InputsChanged,
                    Some(format!("input a.py:u{i}")),
                )],
                (i + 1) as u64,
            );
        }
        let s = t.summary().unwrap();
        assert_eq!(s.reasons.len(), MAX_REASONS);
        assert_eq!(s.omitted, 5);
        // Most steps first: the smallest five are the ones left out.
        assert_eq!(s.reasons[0].steps, (MAX_REASONS + 5) as u64);
        assert_eq!(s.reasons.last().unwrap().steps, 6);
    }

    #[test]
    fn nothing_decided_is_no_summary_and_a_change_is_reported_once() {
        let mut t = PlanTally::default();
        assert!(t.summary().is_none());
        assert!(t.take_changed().is_none());
        t.add("a.py:p", &[(ReasonCategory::Task, None)], 1);
        assert!(t.take_changed().is_some());
        assert!(t.take_changed().is_none());
    }
}
