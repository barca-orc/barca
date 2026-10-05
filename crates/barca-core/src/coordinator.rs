//! Execution coordinator with global ready queue.
//!
//! Pure logic layer — no I/O, no processes, no sockets. Tracks items, dependencies,
//! and parallel groups. Workers are stateless — the I/O layer asks for the next
//! ready item and reports completions.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

// ─── ID types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupId(pub u64);

// ─── Dependency ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Data,
    Ordering,
}

#[derive(Debug, Clone)]
pub struct Dep {
    pub upstream: ItemId,
    pub kind: DepKind,
}

// ─── Item specification ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ItemSpec {
    pub fn_ref: String,
    pub function_name: String,
    pub source_file: String,
    pub direct_args: Vec<serde_json::Value>,
    pub direct_kwargs: HashMap<String, serde_json::Value>,
    pub dag_inputs: HashMap<String, String>,
    pub timeout_seconds: u32,
    pub retries: u32,
    pub retry_backoff_seconds: f64,
    pub serializer: Option<String>,
    /// Sink outputs from stacked `@sink(...)` decorators.
    pub sinks: Vec<crate::model::SinkDecl>,
    /// Content-addressing run hash for this item's artifact (None → legacy
    /// node-id-keyed layout, e.g. parallel() children).
    pub run_hash: Option<String>,
    /// Original step.inputs mapping: param_name → upstream_node_id.
    /// Used at dispatch time to resolve in-phase upstream artifacts.
    pub upstream_inputs: HashMap<String, String>,
    /// Fan-in (`collect()`) params resolved cross-phase: param_name → every
    /// partition artifact of the upstream base id, sorted by path for
    /// determinism (mirrors `dispatch::build_provided_inputs`). Kept separate
    /// from `dag_inputs` (which is one artifact path per param) because a
    /// collected param resolves to a *list* of artifacts.
    pub collected_inputs: HashMap<String, Vec<crate::dispatch::OutputRef>>,
    /// Parameter frame types from function annotations.
    pub param_types: HashMap<String, crate::model::ValueType>,
    /// Return frame type from the function's return annotation.
    pub return_type: Option<crate::model::ValueType>,
    pub kind: String,
    pub is_dynamic: bool,
}

impl ItemSpec {
    pub fn from_step(step: &crate::planner::StreamStep) -> Self {
        Self {
            fn_ref: format!("{}:{}", step.source_file, step.function_name),
            function_name: step.function_name.to_string(),
            source_file: step.source_file.to_string(),
            direct_args: Vec::new(),
            direct_kwargs: HashMap::new(),
            dag_inputs: HashMap::new(),
            timeout_seconds: step.timeout_seconds,
            retries: step.retries,
            retry_backoff_seconds: step.retry_backoff_seconds,
            serializer: step.serializer.as_ref().map(|s| s.to_string()),
            sinks: step.sinks.clone(),
            run_hash: step.run_hashes.get(&step.step_id.display()).cloned(),
            upstream_inputs: step.inputs.clone(),
            collected_inputs: HashMap::new(),
            param_types: step.param_types.clone(),
            return_type: step.return_type,
            kind: format!("{:?}", step.kind).to_lowercase(),
            is_dynamic: false,
        }
    }
}

// ─── Failure handling ─────────────────────────────────────────────────────────

/// What `on_item_failed` decided. `RetryAfter` items are parked in
/// `waiting_retry` — the caller owns the timer and must call `requeue()`
/// once the backoff elapses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FailureAction {
    /// Re-queued immediately (no backoff configured).
    RetryNow,
    /// Parked for backoff; schedule `requeue(item_id)` after this delay.
    RetryAfter(std::time::Duration),
    /// Retry budget exhausted — permanently failed, dependents cascaded.
    Failed,
}

// ─── Item ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Item {
    pub id: ItemId,
    pub step_id: crate::StepId,
    pub spec: ItemSpec,
    pub attempts: u32,
    pub group: Option<GroupId>,
    pub deps: Vec<Dep>,
}

// ─── Parallel group ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParallelGroup {
    pub id: GroupId,
    pub parent: ItemId,
    pub items: Vec<ItemId>,
    pub completed_count: usize,
}

// ─── Coordinator ──────────────────────────────────────────────────────────────

pub struct Coordinator {
    /// All items.
    items: HashMap<ItemId, Item>,
    /// Reverse dep graph: item → items that depend on it.
    dependents: HashMap<ItemId, Vec<ItemId>>,
    /// Items waiting for deps: item → set of unsatisfied upstream ids.
    pending: HashMap<ItemId, HashSet<ItemId>>,
    /// Global ready queue — items ready to execute, not yet assigned.
    ready: VecDeque<ItemId>,
    /// Items currently being executed (by some worker — we don't track which).
    executing: HashSet<ItemId>,
    /// Completed items.
    done: HashSet<ItemId>,
    /// Permanently failed items.
    failed: HashMap<ItemId, String>,
    /// Skipped items (upstream data dep failed).
    skipped: HashSet<ItemId>,
    /// Items parked for retry backoff — not ready until requeue() is called.
    waiting_retry: HashSet<ItemId>,
    /// Output artifacts from completed items.
    outputs: HashMap<ItemId, serde_json::Value>,
    /// Parallel groups.
    groups: HashMap<GroupId, ParallelGroup>,
    /// ID generators.
    next_item_id: u64,
    next_group_id: u64,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl Coordinator {
    // ─── Construction ──────────────────────────────────────────────────────

    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
            dependents: HashMap::new(),
            pending: HashMap::new(),
            ready: VecDeque::new(),
            executing: HashSet::new(),
            done: HashSet::new(),
            failed: HashMap::new(),
            skipped: HashSet::new(),
            waiting_retry: HashSet::new(),
            outputs: HashMap::new(),
            groups: HashMap::new(),
            next_item_id: 0,
            next_group_id: 0,
        }
    }

    /// Add an item. If all deps satisfied, push to ready queue.
    /// If has unsatisfied deps, add to pending.
    pub fn add_item(&mut self, step_id: crate::StepId, spec: ItemSpec, deps: Vec<Dep>) -> ItemId {
        let id = self.reserve_item_id();
        self.finalize_item(id, step_id, spec, deps);
        id
    }

    /// Reserve an [`ItemId`] without creating the item yet. Lets a caller
    /// register an item's id (e.g. in a lookup table other items' deps will
    /// resolve against) before that item's own deps are known — see
    /// [`Coordinator::load_phase`]'s two-pass construction.
    fn reserve_item_id(&mut self) -> ItemId {
        let id = ItemId(self.next_item_id);
        self.next_item_id += 1;
        id
    }

    /// Create the item for a previously [`reserve_item_id`](Self::reserve_item_id)'d
    /// id: inserts it, registers dependents, and places it on the ready or
    /// pending queue. This is `add_item`'s body, split out so `load_phase` can
    /// reserve every id up front and only then resolve deps.
    fn finalize_item(
        &mut self,
        id: ItemId,
        step_id: crate::StepId,
        spec: ItemSpec,
        deps: Vec<Dep>,
    ) {
        let item = Item {
            id,
            step_id,
            spec,
            attempts: 0,
            group: None,
            deps: deps.clone(),
        };
        self.items.insert(id, item);

        // Register in dependents map
        for dep in &deps {
            self.dependents.entry(dep.upstream).or_default().push(id);
        }

        // Determine unsatisfied deps
        let unsatisfied: HashSet<ItemId> = deps
            .iter()
            .filter(|d| {
                let upstream = d.upstream;
                match d.kind {
                    DepKind::Data => !self.done.contains(&upstream),
                    DepKind::Ordering => {
                        !self.done.contains(&upstream)
                            && !self.failed.contains_key(&upstream)
                            && !self.skipped.contains(&upstream)
                    }
                }
            })
            .map(|d| d.upstream)
            .collect();

        if unsatisfied.is_empty() {
            let should_skip = deps.iter().any(|d| {
                d.kind == DepKind::Data
                    && (self.failed.contains_key(&d.upstream) || self.skipped.contains(&d.upstream))
            });
            if should_skip {
                self.skipped.insert(id);
            } else {
                self.ready.push_back(id);
            }
        } else {
            self.pending.insert(id, unsatisfied);
        }
    }

    // ─── Scheduling ────────────────────────────────────────────────────────

    /// Pop the next ready item. Returns None if nothing is ready.
    /// `attempts` counts dispatches, so it is incremented here.
    pub fn next_ready(&mut self) -> Option<ItemId> {
        let id = self.ready.pop_front()?;
        self.executing.insert(id);
        if let Some(item) = self.items.get_mut(&id) {
            item.attempts += 1;
        }
        Some(id)
    }

    /// Move an item parked by a `RetryAfter` decision back to the ready queue.
    pub fn requeue(&mut self, item_id: ItemId) {
        if self.waiting_retry.remove(&item_id) {
            self.ready.push_back(item_id);
        }
    }

    /// Return a leased-but-never-started item to the front of the ready queue.
    ///
    /// Batch pulls lease several items per worker; when the worker dies (or is
    /// killed after a step error) only the in-flight item failed — the
    /// unstarted remainder goes back for another worker, and the dispatch that
    /// never happened doesn't consume retry budget.
    pub fn return_leased(&mut self, item_id: ItemId) {
        if self.executing.remove(&item_id) {
            if let Some(item) = self.items.get_mut(&item_id) {
                item.attempts = item.attempts.saturating_sub(1);
            }
            self.ready.push_front(item_id);
        }
    }

    /// How many items are ready to be assigned.
    pub fn ready_count(&self) -> usize {
        self.ready.len()
    }

    /// How many items still have work ahead of them (ready now or blocked on
    /// upstreams). Note: batch sizing uses `ready_count` (the pull-eligible
    /// pool), not this — blocked items mustn't inflate a batch.
    pub fn remaining_count(&self) -> usize {
        self.ready.len() + self.pending.len() + self.waiting_retry.len()
    }

    // ─── Event handlers ───────────────────────────────────────────────────

    /// Mark an item as completed. Unblocks dependents into the ready queue.
    pub fn on_item_completed(&mut self, item_id: ItemId) {
        self.executing.remove(&item_id);
        self.done.insert(item_id);

        // Check if item belongs to a parallel group
        if let Some(group_id) = self.items[&item_id].group {
            let group = self.groups.get_mut(&group_id).unwrap();
            group.completed_count += 1;
        }

        // Unblock dependents
        self.unblock_dependents(item_id);
    }

    /// Mark an item as failed. Retries if budget remains, else cascades.
    /// Backoff delay grows linearly: `retry_backoff_seconds * attempts`.
    pub fn on_item_failed(&mut self, item_id: ItemId, error: String) -> FailureAction {
        self.executing.remove(&item_id);

        let item = self.items.get_mut(&item_id).unwrap();

        if item.attempts < item.spec.retries {
            let backoff = item.spec.retry_backoff_seconds * item.attempts as f64;
            if backoff > 0.0 {
                self.waiting_retry.insert(item_id);
                FailureAction::RetryAfter(std::time::Duration::from_secs_f64(backoff))
            } else {
                self.ready.push_back(item_id);
                FailureAction::RetryNow
            }
        } else {
            // Permanent failure
            self.failed.insert(item_id, error);

            if let Some(group_id) = self.items[&item_id].group {
                let group = self.groups.get_mut(&group_id).unwrap();
                group.completed_count += 1;
            }

            self.cascade_failure(item_id);
            FailureAction::Failed
        }
    }

    /// Handle parallel() request. Creates a group of child items and adds them
    /// to the ready queue. Returns (group_id, child_ids).
    pub fn on_parallel_requested(
        &mut self,
        parent_id: ItemId,
        specs: Vec<ItemSpec>,
    ) -> (GroupId, Vec<ItemId>) {
        self.executing.remove(&parent_id);

        let group_id = GroupId(self.next_group_id);
        self.next_group_id += 1;

        let mut child_ids = Vec::with_capacity(specs.len());
        for spec in specs {
            let child_id = ItemId(self.next_item_id);
            self.next_item_id += 1;

            let step_id = crate::StepId {
                base: std::sync::Arc::from(spec.fn_ref.as_str()),
                partition: crate::model::PartitionKey(BTreeMap::from([(
                    "_branch".to_string(),
                    child_id.0.to_string(),
                )])),
            };
            let item = Item {
                id: child_id,
                step_id,
                spec,
                attempts: 0,
                group: Some(group_id),
                deps: Vec::new(),
            };
            self.items.insert(child_id, item);
            self.ready.push_back(child_id);
            child_ids.push(child_id);
        }

        let group = ParallelGroup {
            id: group_id,
            parent: parent_id,
            items: child_ids.clone(),
            completed_count: 0,
        };
        self.groups.insert(group_id, group);

        (group_id, child_ids)
    }

    /// Check if a parallel group is fully complete (all children done or failed).
    pub fn is_group_complete(&self, group_id: GroupId) -> bool {
        let group = &self.groups[&group_id];
        group.completed_count == group.items.len()
    }

    // ─── Phase loading ────────────────────────────────────────────────────

    /// Load all steps from a phase into the coordinator.
    /// Resolves in-phase dependencies and fills dag_inputs from provided inputs.
    /// Partitioned steps are expanded: one item per partition key.
    /// Returns the number of items added.
    ///
    /// Two passes, deliberately: pass 1 reserves every item's [`ItemId`] and
    /// registers it in `node_to_item` *before* pass 2 resolves any
    /// dependency. A single interleaved pass would resolve a step's
    /// dependencies against whatever's been registered so far — correct only
    /// if producers always precede their consumers in stream order. That
    /// holds for the planner's initial phases, but not for phases rebuilt by
    /// `dispatch::expand_pending_partitions` (dynamic `partitions_from`
    /// partitions): its load-balancing bin-packing distributes partition-key
    /// chunks across streams by size alone, with no awareness of cross-step
    /// data dependencies, so a consumer's chunk can land in a stream
    /// processed before its producer's. With two passes, resolution order no
    /// longer depends on stream order at all.
    pub fn load_phase(
        &mut self,
        phase: &crate::planner::Phase,
        provided: &HashMap<String, crate::dispatch::ProvidedInput>,
    ) -> usize {
        enum Pending<'p> {
            Plain {
                step: &'p crate::planner::StreamStep,
                step_id: crate::StepId,
                item_id: ItemId,
                prev_items: Vec<ItemId>,
            },
            Partition {
                step: &'p crate::planner::StreamStep,
                pk: &'p crate::model::PartitionKey,
                partition_step_id: crate::StepId,
                item_id: ItemId,
                prev_items: Vec<ItemId>,
            },
        }

        let mut node_to_item: HashMap<String, ItemId> = HashMap::new();
        let mut pending: Vec<Pending> = Vec::new();

        // Pass 1: reserve ids and register every display id this phase will
        // produce. Within-stream ordering chains (`prev_items`) only ever
        // reference the immediately preceding step in the same stream, which
        // has already been reserved by this point in the walk — they don't
        // need a second pass.
        for stream in &phase.streams {
            let mut prev_items: Vec<ItemId> = Vec::new();
            for step in &stream.steps {
                if step.partition_keys.is_empty() {
                    let step_id = step.step_id.clone();
                    let display_id = step_id.display();
                    let item_id = self.reserve_item_id();
                    node_to_item.insert(display_id, item_id);
                    pending.push(Pending::Plain {
                        step,
                        step_id,
                        item_id,
                        prev_items: prev_items.clone(),
                    });
                    prev_items = vec![item_id];
                } else {
                    let mut partition_item_ids: Vec<ItemId> = Vec::new();

                    for pk in &step.partition_keys {
                        let partition_step_id =
                            crate::StepId::new(step.step_id.base.clone(), pk.clone());
                        let partition_display = partition_step_id.display();
                        let item_id = self.reserve_item_id();
                        node_to_item.insert(partition_display, item_id);
                        partition_item_ids.push(item_id);
                        pending.push(Pending::Partition {
                            step,
                            pk,
                            partition_step_id,
                            item_id,
                            prev_items: prev_items.clone(),
                        });
                    }

                    prev_items = partition_item_ids;
                }
            }
        }

        // Pass 2: every id this phase will create is now in `node_to_item`,
        // so dependency resolution no longer depends on visit order.
        let count = pending.len();
        for p in pending {
            match p {
                Pending::Plain {
                    step,
                    step_id,
                    item_id,
                    prev_items,
                } => {
                    let mut deps = Vec::new();
                    for upstream_id in step.inputs.values() {
                        if let Some(&upstream_item) = node_to_item.get(upstream_id) {
                            deps.push(Dep {
                                upstream: upstream_item,
                                kind: DepKind::Data,
                            });
                        }
                    }
                    for &p in &prev_items {
                        if !deps.iter().any(|d| d.upstream == p) {
                            deps.push(Dep {
                                upstream: p,
                                kind: DepKind::Ordering,
                            });
                        }
                    }

                    let mut spec = ItemSpec::from_step(step);
                    for (param_name, upstream_id) in &step.inputs {
                        if let Some(pi) = provided.get(upstream_id) {
                            match pi {
                                crate::dispatch::ProvidedInput::Single(oref) => {
                                    spec.dag_inputs
                                        .insert(param_name.clone(), oref.path.clone());
                                }
                                crate::dispatch::ProvidedInput::Collected(orefs) => {
                                    spec.collected_inputs
                                        .insert(param_name.clone(), orefs.clone());
                                }
                            }
                        }
                    }

                    self.finalize_item(item_id, step_id, spec, deps);
                }
                Pending::Partition {
                    step,
                    pk,
                    partition_step_id,
                    item_id,
                    prev_items,
                } => {
                    let partition_display = partition_step_id.display();
                    let mut deps = Vec::new();
                    // Resolve partition-aligned upstream dependencies
                    for (param_name, upstream_id) in &step.inputs {
                        if param_name.starts_with('_') {
                            continue;
                        }
                        // Try partition-aligned first: upstream_id[pk]
                        let aligned_id = pk.display_id(upstream_id);
                        if let Some(&upstream_item) = node_to_item.get(&aligned_id) {
                            deps.push(Dep {
                                upstream: upstream_item,
                                kind: DepKind::Data,
                            });
                        } else if let Some(&upstream_item) = node_to_item.get(upstream_id) {
                            deps.push(Dep {
                                upstream: upstream_item,
                                kind: DepKind::Data,
                            });
                        }
                    }
                    // Chain ordering from previous step's partitions
                    for &p in &prev_items {
                        if !deps.iter().any(|d| d.upstream == p) {
                            deps.push(Dep {
                                upstream: p,
                                kind: DepKind::Ordering,
                            });
                        }
                    }

                    let mut spec = ItemSpec::from_step(step);
                    spec.run_hash = step.run_hashes.get(&partition_display).cloned();
                    // Add partition values as direct_kwargs so they're
                    // injected into the function call.
                    for (k, v) in &pk.0 {
                        spec.direct_kwargs
                            .insert(k.clone(), serde_json::Value::String(v.clone()));
                    }
                    // Fill dag_inputs from provided (cross-phase) or
                    // rely on in-phase resolution at dispatch time via upstream_inputs.
                    for (param_name, upstream_id) in &step.inputs {
                        if param_name.starts_with('_') {
                            continue;
                        }
                        let aligned_id = pk.display_id(upstream_id);
                        // The upstream's own id is the fallback for an unpartitioned upstream
                        // only. When this phase produces the aligned key, `provided` may still
                        // hold the upstream's base id as a list of its *cached* keys (the
                        // collect() fallback in `build_provided_inputs`); taking it would hand
                        // this key a list instead of its own key's output (#189: a new upstream
                        // key, run in the same phase as its consumer key).
                        let in_phase = node_to_item.contains_key(&aligned_id);
                        if let Some(pi) = provided.get(&aligned_id).or_else(|| {
                            if in_phase {
                                None
                            } else {
                                provided.get(upstream_id)
                            }
                        }) {
                            match pi {
                                crate::dispatch::ProvidedInput::Single(oref) => {
                                    spec.dag_inputs
                                        .insert(param_name.clone(), oref.path.clone());
                                }
                                crate::dispatch::ProvidedInput::Collected(orefs) => {
                                    spec.collected_inputs
                                        .insert(param_name.clone(), orefs.clone());
                                }
                            }
                        }
                        // Point upstream_inputs at the id this phase (or a prior one)
                        // actually produces, for in-phase resolution at dispatch time: the
                        // partition-aligned id for a partitioned upstream, the upstream's own
                        // id for an unpartitioned one (#170: always using the aligned id left
                        // an unpartitioned input unresolved, so the call lacked it).
                        let produced =
                            |id: &str| node_to_item.contains_key(id) || provided.contains_key(id);
                        let resolved = if !produced(&aligned_id) && produced(upstream_id) {
                            upstream_id.clone()
                        } else {
                            aligned_id
                        };
                        spec.upstream_inputs.insert(param_name.clone(), resolved);
                    }

                    self.finalize_item(item_id, partition_step_id, spec, deps);
                }
            }
        }

        count
    }

    // ─── Queries ──────────────────────────────────────────────────────────

    /// Returns true when all items are in a terminal state.
    pub fn is_finished(&self) -> bool {
        self.items.keys().all(|id| {
            self.done.contains(id) || self.failed.contains_key(id) || self.skipped.contains(id)
        })
    }

    /// Return a reference to an item.
    pub fn item(&self, id: ItemId) -> &Item {
        &self.items[&id]
    }

    /// Return a reference to a parallel group.
    pub fn group(&self, id: GroupId) -> &ParallelGroup {
        &self.groups[&id]
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn done_count(&self) -> usize {
        self.done.len()
    }

    pub fn failed_items(&self) -> Vec<(ItemId, &str)> {
        self.failed
            .iter()
            .map(|(id, msg)| (*id, msg.as_str()))
            .collect()
    }

    pub fn skipped_items(&self) -> Vec<ItemId> {
        self.skipped.iter().copied().collect()
    }

    pub fn is_done(&self, item_id: ItemId) -> bool {
        self.done.contains(&item_id)
    }

    /// Record an output artifact for a completed item.
    pub fn record_output(&mut self, item_id: ItemId, artifact: serde_json::Value) {
        self.outputs.insert(item_id, artifact);
    }

    /// Return all recorded outputs.
    pub fn outputs(&self) -> &HashMap<ItemId, serde_json::Value> {
        &self.outputs
    }

    // ─── Internal helpers ─────────────────────────────────────────────────

    /// Cascade a failure through data-dependency edges.
    fn cascade_failure(&mut self, item_id: ItemId) {
        let dependents = self.dependents.get(&item_id).cloned().unwrap_or_default();

        for dep_id in dependents {
            if self.done.contains(&dep_id)
                || self.failed.contains_key(&dep_id)
                || self.skipped.contains(&dep_id)
            {
                continue;
            }

            let dep_kind = self.items[&dep_id]
                .deps
                .iter()
                .find(|d| d.upstream == item_id)
                .map(|d| d.kind);

            match dep_kind {
                Some(DepKind::Data) => {
                    self.skipped.insert(dep_id);
                    self.pending.remove(&dep_id);
                    self.ready.retain(|id| *id != dep_id);
                    self.cascade_failure(dep_id);
                }
                Some(DepKind::Ordering) => {
                    if let Some(unsatisfied) = self.pending.get_mut(&dep_id) {
                        unsatisfied.remove(&item_id);
                        if unsatisfied.is_empty() {
                            self.pending.remove(&dep_id);
                            let should_skip = self.items[&dep_id].deps.iter().any(|d| {
                                d.kind == DepKind::Data
                                    && (self.failed.contains_key(&d.upstream)
                                        || self.skipped.contains(&d.upstream))
                            });
                            if should_skip {
                                self.skipped.insert(dep_id);
                                self.cascade_failure(dep_id);
                            } else {
                                self.ready.push_back(dep_id);
                            }
                        }
                    }
                }
                None => {}
            }
        }
    }

    /// Unblock dependents after successful completion.
    fn unblock_dependents(&mut self, item_id: ItemId) {
        let dependents = self.dependents.get(&item_id).cloned().unwrap_or_default();

        for dep_id in dependents {
            if self.done.contains(&dep_id)
                || self.failed.contains_key(&dep_id)
                || self.skipped.contains(&dep_id)
            {
                continue;
            }

            if let Some(unsatisfied) = self.pending.get_mut(&dep_id) {
                unsatisfied.remove(&item_id);
                if unsatisfied.is_empty() {
                    self.pending.remove(&dep_id);
                    let should_skip = self.items[&dep_id].deps.iter().any(|d| {
                        d.kind == DepKind::Data
                            && (self.failed.contains_key(&d.upstream)
                                || self.skipped.contains(&d.upstream))
                    });
                    if should_skip {
                        self.skipped.insert(dep_id);
                        self.cascade_failure(dep_id);
                    } else {
                        self.ready.push_back(dep_id);
                    }
                }
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
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
            retry_backoff_seconds: 0.0,
            serializer: None,
            sinks: Vec::new(),
            run_hash: None,
            upstream_inputs: HashMap::new(),
            collected_inputs: HashMap::new(),
            param_types: HashMap::new(),
            return_type: None,
            kind: "asset".to_string(),
            is_dynamic: false,
        }
    }

    fn dummy_step_id(name: &str) -> crate::StepId {
        crate::StepId {
            base: std::sync::Arc::from(format!("test.py:{name}")),
            partition: crate::model::PartitionKey(std::collections::BTreeMap::new()),
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
        c.on_item_failed(a, "boom".into());
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
        c.on_item_failed(a, "boom".into());
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
        c.on_item_failed(a, "fail 1".into());
        // Should be back in ready (attempt 1 of 3)
        assert_eq!(c.ready_count(), 1);
        assert_eq!(c.next_ready(), Some(a));
    }

    #[test]
    fn retry_exhausted_marks_failed() {
        let mut c = Coordinator::new();
        let a = c.add_item(dummy_step_id("a"), spec("a"), vec![]); // retries=1 → no retry
        c.next_ready();
        assert_eq!(c.on_item_failed(a, "boom".into()), FailureAction::Failed);
        assert_eq!(c.failed_items().len(), 1);
        assert!(c.is_finished());
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
            c.on_item_failed(a, "fail 1".into()),
            FailureAction::RetryNow
        );
        c.next_ready();
        assert_eq!(c.item(a).attempts, 2);
        assert_eq!(
            c.on_item_failed(a, "fail 2".into()),
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
            c.on_item_failed(a, "fail 1".into()),
            FailureAction::RetryAfter(std::time::Duration::from_secs_f64(0.2)),
        );
        // Parked: not ready, not finished, until the caller requeues.
        assert_eq!(c.ready_count(), 0);
        assert!(!c.is_finished());
        c.requeue(a);
        assert_eq!(c.next_ready(), Some(a));

        // Second failure: delay = 0.2 * 2.
        assert_eq!(
            c.on_item_failed(a, "fail 2".into()),
            FailureAction::RetryAfter(std::time::Duration::from_secs_f64(0.4)),
        );
        c.requeue(a);
        assert_eq!(c.next_ready(), Some(a));

        // Third failure exhausts the budget regardless of backoff.
        assert_eq!(c.on_item_failed(a, "fail 3".into()), FailureAction::Failed);
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

    #[test]
    fn load_phase_assigns_run_hashes_per_item_and_partition() {
        use crate::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
        use crate::{PartitionKey, StepId};

        let pk_a = PartitionKey::from(HashMap::from([("t".to_string(), "A".to_string())]));
        let pk_b = PartitionKey::from(HashMap::from([("t".to_string(), "B".to_string())]));

        let mut plain_hashes = HashMap::new();
        plain_hashes.insert("f:plain".to_string(), "hash-plain".to_string());
        let mut part_hashes = HashMap::new();
        part_hashes.insert("f:part[t=A]".to_string(), "hash-a".to_string());
        part_hashes.insert("f:part[t=B]".to_string(), "hash-b".to_string());

        let mk = |name: &str, hashes: HashMap<String, String>, pks: Vec<PartitionKey>| StreamStep {
            step_id: StepId::unpartitioned(name),
            kind: crate::NodeKind::Asset,
            function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
            source_file: std::sync::Arc::from("f"),
            inputs: HashMap::new(),
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes: hashes,
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: pks,
            param_types: HashMap::new(),
            return_type: None,
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
        use crate::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
        use crate::{PartitionKey, StepId};

        let pk = |k: &str| PartitionKey::from(HashMap::from([("t".to_string(), k.to_string())]));

        let mk = |name: &str, inputs: HashMap<String, String>, pks: Vec<PartitionKey>| StreamStep {
            step_id: StepId::unpartitioned(name),
            kind: crate::NodeKind::Asset,
            function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
            source_file: std::sync::Arc::from("f"),
            inputs,
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: pks,
            param_types: HashMap::new(),
            return_type: None,
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
        use crate::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
        use crate::{PartitionKey, StepId};

        let pk = |k: &str| PartitionKey::from(HashMap::from([("k".to_string(), k.to_string())]));
        let mk = |name: &str, inputs: HashMap<String, String>, pks: Vec<PartitionKey>| StreamStep {
            step_id: StepId::unpartitioned(name),
            kind: crate::NodeKind::Asset,
            function_name: std::sync::Arc::from(name.rsplit(':').next().unwrap()),
            source_file: std::sync::Arc::from("f"),
            inputs,
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: pks,
            param_types: HashMap::new(),
            return_type: None,
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
        use crate::StepId;
        use crate::dispatch::{OutputRef, ProvidedInput};
        use crate::planner::{Phase, PhaseReason, StreamStep, WorkerStream};

        let step = StreamStep {
            step_id: StepId::unpartitioned("f:sink"),
            kind: crate::NodeKind::Asset,
            function_name: std::sync::Arc::from("sink"),
            source_file: std::sync::Arc::from("f"),
            inputs: HashMap::from([("data".to_string(), "f:source".to_string())]),
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes: HashMap::new(),
            timeout_seconds: 300,
            retries: 1,
            retry_backoff_seconds: 0.0,
            partition_keys: vec![],
            param_types: HashMap::new(),
            return_type: None,
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
        };
        let oref_b = OutputRef {
            path: "f--source_key_b.json".to_string(),
            format: "json".to_string(),
            size_bytes: 12,
            elapsed_seconds: None,
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
}
