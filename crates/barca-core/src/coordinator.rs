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
    /// How much of the pool's capacity this item takes while it runs (`weight=`, #436).
    /// `parallel()` branches and partition keys carry their node's.
    pub weight: u32,
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
    /// A dict `partitions_from` source's value for this key (`crate::keyed`): param ->
    /// (the source's JSON artifact, the key). The worker reads the value from the artifact.
    pub keyed_inputs: HashMap<String, (String, String)>,
    /// The step's output is read by a `partitions_from(...)` for keys: the worker checks it is
    /// a list or a dict it can write as JSON (`crate::keyed`).
    pub partition_source: bool,
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
            weight: step.weight,
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
            keyed_inputs: HashMap::new(),
            partition_source: step.keyed.is_source,
        }
    }
}

// ─── Capacity ─────────────────────────────────────────────────────────────────

/// The weight an item counts for under `capacity`: its `weight=`, at least 1, and at most the
/// capacity. A step heavier than the capacity counts as the whole of it, so it starts only when
/// nothing else is running and then runs alone, rather than never (#436).
pub fn effective_weight(weight: u32, capacity: u32) -> u32 {
    weight.clamp(1, capacity.max(1))
}

/// Whether an item of `weight` may start while steps whose weights total `running` are running.
pub fn fits(running: u32, weight: u32, capacity: u32) -> bool {
    running + effective_weight(weight, capacity) <= capacity.max(1)
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
    /// The directory this group's branches write their results to: one per group, inside a
    /// directory of the run, so that a caller reads only what its own branches wrote (#332).
    /// `None` leaves branch results where they were before, named after the branch alone.
    pub result_dir: Option<std::path::PathBuf>,
}

// ─── Ready queue ──────────────────────────────────────────────────────────────

/// The global ready queue: FIFO order plus a membership set, so taking an item out (a
/// skipped dependent, an abandoned branch) costs O(1) instead of a scan of the queue (#461).
/// A removed item leaves a stale entry in `order`, dropped when it reaches the front.
#[derive(Debug, Default)]
struct ReadyQueue {
    order: VecDeque<ItemId>,
    /// The items actually queued. `order` holds each of them once, plus stale entries.
    queued: HashSet<ItemId>,
    /// Queue elements touched (pushed, popped, looked up or removed), so a test can hold the
    /// cost of skipping many failed keys to linear (#461).
    #[cfg(test)]
    ops: usize,
}

impl ReadyQueue {
    fn touch(&mut self) {
        #[cfg(test)]
        {
            self.ops += 1;
        }
    }

    /// Queue `id` at the back. An item already queued keeps its place.
    fn push_back(&mut self, id: ItemId) {
        self.touch();
        if self.queued.insert(id) {
            self.order.push_back(id);
        }
    }

    /// Queue `id` at the front. An item already queued keeps its place.
    fn push_front(&mut self, id: ItemId) {
        self.touch();
        if self.queued.insert(id) {
            self.order.push_front(id);
        }
    }

    /// The first queued item, dropping stale entries of removed items on the way.
    fn pop_front(&mut self) -> Option<ItemId> {
        loop {
            self.touch();
            let id = self.order.pop_front()?;
            if self.queued.remove(&id) {
                return Some(id);
            }
        }
    }

    fn contains(&mut self, id: ItemId) -> bool {
        self.touch();
        self.queued.contains(&id)
    }

    /// Take `id` out of the queue if it is there.
    fn remove(&mut self, id: ItemId) {
        self.touch();
        self.queued.remove(&id);
    }

    fn len(&self) -> usize {
        self.queued.len()
    }

    /// The queued items in pop order. An item removed and queued again has a stale entry
    /// ahead of its live one; `pop_front` takes it at the first, so this yields it there.
    fn iter(&self) -> impl Iterator<Item = ItemId> + '_ {
        let mut seen = HashSet::new();
        self.order
            .iter()
            .copied()
            .filter(move |id| self.queued.contains(id) && seen.insert(*id))
    }
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
    ready: ReadyQueue,
    /// Items currently being executed (by some worker — we don't track which).
    executing: HashSet<ItemId>,
    /// Completed items.
    done: HashSet<ItemId>,
    /// Permanently failed items.
    failed: HashMap<ItemId, String>,
    /// When each failed item's first attempt started (wall clock), kept across retries.
    first_attempt_at: HashMap<ItemId, std::time::SystemTime>,
    /// For a permanently failed item: when its first attempt started and the wall time from
    /// then to the permanent failure, retries and backoff included.
    failure_clocks: HashMap<ItemId, (std::time::SystemTime, std::time::Duration)>,
    /// Skipped items (upstream data dep failed).
    skipped: HashSet<ItemId>,
    /// Items parked for retry backoff — not ready until requeue() is called.
    waiting_retry: HashSet<ItemId>,
    /// Output artifacts from completed items.
    outputs: HashMap<ItemId, serde_json::Value>,
    /// Parallel groups.
    groups: HashMap<GroupId, ParallelGroup>,
    /// Ordering gates, by gate id: how many member items are not yet terminal (done, failed or
    /// skipped). A gate stands for "every partition of the previous step in this stream", so a
    /// step of K partitions after another of K costs K + K edges instead of K × K (#345). Gate
    /// ids come from the item id space but are never items; an item waits on a gate through an
    /// [`DepKind::Ordering`] dep, and the gate releases its waiters when it reaches zero.
    gates: HashMap<ItemId, usize>,
    /// Member item → the gates it counts toward.
    member_gates: HashMap<ItemId, Vec<ItemId>>,
    /// Item id of the latest recorded output for each display id, so dispatch resolves an
    /// in-phase upstream in one lookup instead of scanning every output (#345).
    output_by_display: HashMap<String, ItemId>,
    /// Batched calls (RFC-0013): the call each item belongs to, and each call's state.
    call_of: HashMap<ItemId, usize>,
    calls: Vec<Call>,
    /// Calls assigned before the phase is loaded: display id -> call index.
    call_assign: HashMap<String, usize>,
    /// ID generators.
    next_item_id: u64,
    next_group_id: u64,
}

/// One call of a batched node's function (RFC-0013): keys that are leased to one worker
/// together and computed by one call. The call is released to the ready queue once every key
/// of it is ready (or will never run); a retry releases the keys that failed together.
#[derive(Debug, Default)]
struct Call {
    /// The call's id, unique in the run (`batch` on its keys' materializations).
    id: String,
    /// Its keys, in key order.
    members: Vec<ItemId>,
    /// How many members are not yet ready (nor terminal).
    unreleased: usize,
    /// Members counted off `unreleased`.
    counted: HashSet<ItemId>,
    /// Ready members waiting for the rest.
    held: Vec<ItemId>,
    /// Members leased and not yet reported.
    outstanding: usize,
    /// Members that failed an attempt and will be tried again, released together once the
    /// attempt is over and no backoff is pending.
    retry: Vec<ItemId>,
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
            ready: ReadyQueue::default(),
            executing: HashSet::new(),
            done: HashSet::new(),
            failed: HashMap::new(),
            first_attempt_at: HashMap::new(),
            failure_clocks: HashMap::new(),
            skipped: HashSet::new(),
            waiting_retry: HashSet::new(),
            outputs: HashMap::new(),
            groups: HashMap::new(),
            gates: HashMap::new(),
            member_gates: HashMap::new(),
            output_by_display: HashMap::new(),
            call_of: HashMap::new(),
            calls: Vec::new(),
            call_assign: HashMap::new(),
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
        if let Some(&call) = self.call_assign.get(&step_id.display()) {
            self.call_of.insert(id, call);
            self.calls[call].members.push(id);
        }
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
                self.count_off_call(id);
            } else {
                self.mark_ready(id);
            }
        } else {
            self.pending.insert(id, unsatisfied);
        }
    }

    // ─── Batched calls (RFC-0013) ──────────────────────────────────────────

    /// Group the items this phase will load into calls, before [`Self::load_phase`]: each call
    /// is `(its id, the display ids of its keys in key order)`. An item of a call is leased
    /// with the rest of its call, once every one of them is ready.
    pub fn assign_calls(&mut self, calls: Vec<(String, Vec<String>)>) {
        for (id, members) in calls {
            let index = self.calls.len();
            self.calls.push(Call {
                id,
                unreleased: members.len(),
                ..Default::default()
            });
            for m in members {
                self.call_assign.insert(m, index);
            }
        }
    }

    /// After loading: a call whose keys were not all loaded waits only for the ones that were.
    fn settle_calls(&mut self) {
        for c in 0..self.calls.len() {
            let loaded = self.calls[c].members.len();
            let call = &mut self.calls[c];
            let missing = call
                .unreleased
                .saturating_sub(loaded.saturating_sub(call.counted.len()));
            if missing > 0 {
                call.unreleased -= missing;
                if call.unreleased == 0 {
                    self.release_call(c);
                }
            }
        }
        self.call_assign.clear();
    }

    /// The id of the call `item` belongs to, if it is a key of a batched call.
    pub fn call_id(&self, item: ItemId) -> Option<&str> {
        self.call_of.get(&item).map(|&c| self.calls[c].id.as_str())
    }

    /// Whether two items are keys of the same call.
    pub fn same_call(&self, a: ItemId, b: ItemId) -> bool {
        match (self.call_of.get(&a), self.call_of.get(&b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }

    /// `id` can run: queued now, or held until the rest of its call can run too.
    fn mark_ready(&mut self, id: ItemId) {
        let Some(&c) = self.call_of.get(&id) else {
            self.ready.push_back(id);
            return;
        };
        let call = &mut self.calls[c];
        if call.counted.insert(id) {
            call.held.push(id);
            call.unreleased = call.unreleased.saturating_sub(1);
            if call.unreleased == 0 {
                self.release_call(c);
            }
        } else {
            self.ready.push_back(id);
        }
    }

    /// `id` will never run (its upstream failed): its call does not wait for it.
    fn count_off_call(&mut self, id: ItemId) {
        let Some(&c) = self.call_of.get(&id) else {
            return;
        };
        let call = &mut self.calls[c];
        if call.counted.insert(id) {
            call.unreleased = call.unreleased.saturating_sub(1);
            if call.unreleased == 0 {
                self.release_call(c);
            }
        }
    }

    /// Queue the held keys of call `c` together, in key order.
    fn release_call(&mut self, c: usize) {
        let mut held = std::mem::take(&mut self.calls[c].held);
        let order: HashMap<ItemId, usize> = self.calls[c]
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| (*m, i))
            .collect();
        held.sort_by_key(|m| order.get(m).copied().unwrap_or(usize::MAX));
        for m in held {
            self.ready.push_back(m);
        }
    }

    /// A key of a call was reported (or returned unstarted): one fewer outstanding, and the
    /// keys to retry go back together once the attempt is over.
    fn call_reported(&mut self, id: ItemId) {
        let Some(&c) = self.call_of.get(&id) else {
            return;
        };
        let call = &mut self.calls[c];
        call.outstanding = call.outstanding.saturating_sub(1);
        self.release_retries(c);
    }

    fn release_retries(&mut self, c: usize) {
        let call = &self.calls[c];
        if call.outstanding > 0
            || call.retry.is_empty()
            || call.retry.iter().any(|m| self.waiting_retry.contains(m))
        {
            return;
        }
        let retry = std::mem::take(&mut self.calls[c].retry);
        for m in retry {
            self.ready.push_back(m);
        }
    }

    // ─── Scheduling ────────────────────────────────────────────────────────

    /// Pop the next ready item. Returns None if nothing is ready.
    /// `attempts` counts dispatches, so it is incremented here.
    pub fn next_ready(&mut self) -> Option<ItemId> {
        let id = self.ready.pop_front()?;
        self.executing.insert(id);
        if let Some(&c) = self.call_of.get(&id) {
            self.calls[c].outstanding += 1;
        }
        if let Some(item) = self.items.get_mut(&id) {
            item.attempts += 1;
        }
        Some(id)
    }

    /// Move an item parked by a `RetryAfter` decision back to the ready queue.
    pub fn requeue(&mut self, item_id: ItemId) {
        if self.waiting_retry.remove(&item_id) {
            match self.call_of.get(&item_id) {
                Some(&c) => self.release_retries(c),
                None => self.ready.push_back(item_id),
            }
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
            if let Some(&c) = self.call_of.get(&item_id) {
                let call = &mut self.calls[c];
                call.outstanding = call.outstanding.saturating_sub(1);
            }
            self.ready.push_front(item_id);
        }
    }

    /// Whether the item at the head of the ready queue may start while steps whose weights
    /// total `running` are running (`fits`). `false` when nothing is ready.
    ///
    /// Only the head is ever considered: an item that does not fit holds back the items queued
    /// behind it, light ones included, until enough of what is running finishes. Letting
    /// lighter items past it would keep the capacity filled with them and a heavy step could
    /// wait for the rest of the run (#436).
    pub fn head_fits(&self, running: u32, capacity: u32) -> bool {
        self.ready
            .iter()
            .next()
            .is_some_and(|id| fits(running, self.items[&id].spec.weight, capacity))
    }

    /// The ready items in the order `next_ready` hands them out, left in the queue.
    pub fn ready_items(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.ready.iter()
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
        self.call_reported(item_id);

        // Check if item belongs to a parallel group
        if let Some(group_id) = self.items[&item_id].group {
            let group = self.groups.get_mut(&group_id).unwrap();
            group.completed_count += 1;
        }

        // Unblock dependents
        self.unblock_dependents(item_id);
    }

    /// Mark an item as failed after an attempt that ran for `elapsed`. Retries if budget
    /// remains, else cascades. Backoff delay grows linearly: `retry_backoff_seconds * attempts`.
    pub fn on_item_failed(
        &mut self,
        item_id: ItemId,
        error: String,
        elapsed: std::time::Duration,
    ) -> FailureAction {
        self.executing.remove(&item_id);

        let now = std::time::SystemTime::now();
        let attempt_start = now.checked_sub(elapsed).unwrap_or(now);
        let first = *self
            .first_attempt_at
            .entry(item_id)
            .or_insert(attempt_start);

        let item = self.items.get_mut(&item_id).unwrap();

        if item.attempts < item.spec.retries {
            let backoff = item.spec.retry_backoff_seconds * item.attempts as f64;
            if let Some(&c) = self.call_of.get(&item_id) {
                // A key of a call: tried again with the call's other failed keys, once every
                // key of this attempt is reported.
                self.calls[c].retry.push(item_id);
                let action = if backoff > 0.0 {
                    self.waiting_retry.insert(item_id);
                    FailureAction::RetryAfter(std::time::Duration::from_secs_f64(backoff))
                } else {
                    FailureAction::RetryNow
                };
                self.call_reported(item_id);
                return action;
            }
            if backoff > 0.0 {
                self.waiting_retry.insert(item_id);
                FailureAction::RetryAfter(std::time::Duration::from_secs_f64(backoff))
            } else {
                self.ready.push_back(item_id);
                FailureAction::RetryNow
            }
        } else {
            self.call_reported(item_id);
            // Permanent failure
            self.failed.insert(item_id, error);
            self.failure_clocks.insert(
                item_id,
                (first, now.duration_since(first).unwrap_or(elapsed)),
            );

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
        // A branch runs the caller's kind of work: it takes the caller's weight (#436).
        let weight = self.items[&parent_id].spec.weight;

        let mut child_ids = Vec::with_capacity(specs.len());
        for mut spec in specs {
            spec.weight = weight;
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
            result_dir: None,
        };
        self.groups.insert(group_id, group);

        (group_id, child_ids)
    }

    /// Say where a group's branches write their results (see [`ParallelGroup::result_dir`]).
    pub fn set_group_result_dir(&mut self, group_id: GroupId, dir: std::path::PathBuf) {
        if let Some(group) = self.groups.get_mut(&group_id) {
            group.result_dir = Some(dir);
        }
    }

    /// Give up on a group whose caller is gone: its branches that have not started will not
    /// start. Those already running finish, and nobody reads what they return.
    pub fn abandon_group(&mut self, group_id: GroupId) {
        let Some(group) = self.groups.get(&group_id) else {
            return;
        };
        let branches = group.items.clone();
        let waiting: Vec<ItemId> = branches
            .into_iter()
            .filter(|id| self.ready.contains(*id))
            .collect();
        for id in &waiting {
            self.ready.remove(*id);
            self.skipped.insert(*id);
        }
        if let Some(group) = self.groups.get_mut(&group_id) {
            group.completed_count += waiting.len();
        }
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
                prev: Option<usize>,
            },
            Partition {
                step: &'p crate::planner::StreamStep,
                pk: &'p crate::model::PartitionKey,
                partition_step_id: crate::StepId,
                item_id: ItemId,
                prev: Option<usize>,
            },
        }

        let mut node_to_item: HashMap<String, ItemId> = HashMap::new();
        let mut pending: Vec<Pending> = Vec::new();
        // The items of each step in each stream, shared by every item of the next step in that
        // stream (`prev` indexes it) instead of copied into each one.
        let mut prev_sets: Vec<Vec<ItemId>> = Vec::new();
        // The ordering gate standing for each prev set of more than one item, made on first use.
        let mut set_gates: Vec<Option<Option<ItemId>>> = Vec::new();

        // Pass 1: reserve ids and register every display id this phase will
        // produce. Within-stream ordering chains (`prev_items`) only ever
        // reference the immediately preceding step in the same stream, which
        // has already been reserved by this point in the walk — they don't
        // need a second pass.
        for stream in &phase.streams {
            let mut prev: Option<usize> = None;
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
                        prev,
                    });
                    prev_sets.push(vec![item_id]);
                    prev = Some(prev_sets.len() - 1);
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
                            prev,
                        });
                    }

                    prev_sets.push(partition_item_ids);
                    prev = Some(prev_sets.len() - 1);
                }
            }
        }

        // Pass 2: every id this phase will create is now in `node_to_item`,
        // so dependency resolution no longer depends on visit order.
        let count = pending.len();
        set_gates.resize(prev_sets.len(), None);
        for p in pending {
            match p {
                Pending::Plain {
                    step,
                    step_id,
                    item_id,
                    prev,
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
                    self.push_ordering_deps(&mut deps, prev, &prev_sets, &mut set_gates);

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
                    prev,
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
                    self.push_ordering_deps(&mut deps, prev, &prev_sets, &mut set_gates);

                    let mut spec = ItemSpec::from_step(step);
                    spec.run_hash = step.run_hashes.get(&partition_display).cloned();
                    // Add partition values as direct_kwargs so they're
                    // injected into the function call.
                    for (k, v) in &pk.0 {
                        spec.direct_kwargs
                            .insert(k.clone(), serde_json::Value::String(v.clone()));
                    }
                    // A dict `partitions_from` source: this key's value, read by the worker.
                    for ks in step.keyed.direct() {
                        if let (Some(param), Some(key)) = (&ks.param, pk.0.get(&ks.dim)) {
                            spec.keyed_inputs
                                .insert(param.clone(), (ks.path.clone(), key.clone()));
                        }
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
        self.settle_calls();

        count
    }

    /// Order an item after every item of the previous step in its stream (`prev` indexes
    /// `prev_sets`), as the stream's chain ordering requires. One item is depended on directly,
    /// unless it is already a data dependency; several are depended on through one shared
    /// gate, so the edges grow with the number of keys, not its square (#345).
    fn push_ordering_deps(
        &mut self,
        deps: &mut Vec<Dep>,
        prev: Option<usize>,
        prev_sets: &[Vec<ItemId>],
        set_gates: &mut [Option<Option<ItemId>>],
    ) {
        let Some(set) = prev else {
            return;
        };
        match prev_sets[set].as_slice() {
            [] => {}
            [only] => {
                if !deps.iter().any(|d| d.upstream == *only) {
                    deps.push(Dep {
                        upstream: *only,
                        kind: DepKind::Ordering,
                    });
                }
            }
            members => {
                let gate = *set_gates[set].get_or_insert_with(|| self.open_gate(members));
                if let Some(gate) = gate {
                    deps.push(Dep {
                        upstream: gate,
                        kind: DepKind::Ordering,
                    });
                }
            }
        }
    }

    /// Make a gate over `members`, counting those not yet terminal. `None` when every member
    /// already is, so there is nothing to wait for.
    fn open_gate(&mut self, members: &[ItemId]) -> Option<ItemId> {
        let waiting: Vec<ItemId> = members
            .iter()
            .copied()
            .filter(|id| !self.is_terminal(*id))
            .collect();
        if waiting.is_empty() {
            return None;
        }
        let gate = self.reserve_item_id();
        self.gates.insert(gate, waiting.len());
        for member in waiting {
            self.member_gates.entry(member).or_default().push(gate);
        }
        Some(gate)
    }

    fn is_terminal(&self, id: ItemId) -> bool {
        self.done.contains(&id) || self.failed.contains_key(&id) || self.skipped.contains(&id)
    }

    /// `item_id` has become terminal: count it off every gate it is a member of, and release
    /// the waiters of each gate that has no member left to wait for.
    fn settle_gates(&mut self, item_id: ItemId) {
        let Some(gates) = self.member_gates.remove(&item_id) else {
            return;
        };
        for gate in gates {
            let Some(remaining) = self.gates.get_mut(&gate) else {
                continue;
            };
            *remaining -= 1;
            if *remaining == 0 {
                self.gates.remove(&gate);
                self.unblock_dependents(gate);
            }
        }
    }

    /// How many dependency edges the loaded items hold. Grows linearly with the number of
    /// partition keys; the regression check for #345 holds it to that.
    #[cfg(test)]
    fn edge_count(&self) -> usize {
        self.items
            .values()
            .map(|item| item.deps.len())
            .sum::<usize>()
            + self.gates.values().sum::<usize>()
    }

    // ─── Queries ──────────────────────────────────────────────────────────

    /// Returns true when all items are in a terminal state.
    pub fn is_finished(&self) -> bool {
        // The three sets hold only items, so while their sizes add up to fewer than the items
        // some item is not terminal. This keeps the check, made once per event, from scanning
        // every item until the run is nearly over (#345).
        if self.done.len() + self.failed.len() + self.skipped.len() < self.items.len() {
            return false;
        }
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

    /// When a permanently failed item's first attempt started, and the wall time from then
    /// to its permanent failure (retries and backoff included).
    pub fn failure_clock(
        &self,
        item_id: ItemId,
    ) -> Option<(std::time::SystemTime, std::time::Duration)> {
        self.failure_clocks.get(&item_id).copied()
    }

    pub fn skipped_items(&self) -> Vec<ItemId> {
        self.skipped.iter().copied().collect()
    }

    pub fn is_done(&self, item_id: ItemId) -> bool {
        self.done.contains(&item_id)
    }

    /// Record an output artifact for a completed item.
    pub fn record_output(&mut self, item_id: ItemId, artifact: serde_json::Value) {
        if let Some(item) = self.items.get(&item_id) {
            self.output_by_display
                .insert(item.step_id.display(), item_id);
        }
        self.outputs.insert(item_id, artifact);
    }

    /// The recorded output of the item whose display id is `display_id`, if any.
    pub fn output_for_display(&self, display_id: &str) -> Option<&serde_json::Value> {
        self.output_by_display
            .get(display_id)
            .and_then(|id| self.outputs.get(id))
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
                    self.ready.remove(dep_id);
                    self.count_off_call(dep_id);
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
                                self.count_off_call(dep_id);
                                self.cascade_failure(dep_id);
                            } else {
                                self.mark_ready(dep_id);
                            }
                        }
                    }
                }
                None => {}
            }
        }
        self.settle_gates(item_id);
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
                        self.count_off_call(dep_id);
                        self.cascade_failure(dep_id);
                    } else {
                        self.mark_ready(dep_id);
                    }
                }
            }
        }
        self.settle_gates(item_id);
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
            is_dynamic: false,
            keyed_inputs: HashMap::new(),
            partition_source: false,
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
        let parent = c.add_item(crate::StepId::unpartitioned("p"), spec("p"), Vec::new());
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
    fn partition_stream(keys: &[String], steps: &[(&str, Option<&str>)]) -> crate::planner::Phase {
        use crate::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
        use crate::{PartitionKey, StepId};
        let pks: Vec<PartitionKey> = keys
            .iter()
            .map(|k| PartitionKey::from(HashMap::from([("k".to_string(), k.clone())])))
            .collect();
        let steps = steps
            .iter()
            .map(|&(name, upstream)| StreamStep {
                step_id: StepId::unpartitioned(name),
                kind: crate::NodeKind::Asset,
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
            let expected: Vec<String> =
                failed.iter().map(|a| a.replacen("f:a", "f:b", 1)).collect();
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
        use crate::planner::{Phase, PhaseReason, StreamStep, WorkerStream};
        use crate::{PartitionKey, StepId};

        let pk = |k: &str| PartitionKey::from(HashMap::from([("k".to_string(), k.to_string())]));
        let step = StreamStep {
            step_id: StepId::unpartitioned("f:heavy"),
            kind: crate::NodeKind::Asset,
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
}
