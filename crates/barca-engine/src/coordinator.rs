//! Execution coordinator with global ready queue.
//!
//! Pure logic layer — no I/O, no processes, no sockets. Tracks items, dependencies,
//! and parallel groups. Workers are stateless — the I/O layer asks for the next
//! ready item and reports completions.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

mod calls;
mod groups;
mod load;
mod outcomes;

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
    pub sinks: Vec<barca_model::model::SinkDecl>,
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
    pub param_types: HashMap<String, barca_model::model::ValueType>,
    /// Return frame type from the function's return annotation.
    pub return_type: Option<barca_model::model::ValueType>,
    pub kind: String,
    /// A dict `partitions_from` source's value for this key (`barca_model::keyed`): param ->
    /// (the source's JSON artifact, the key). The worker reads the value from the artifact.
    pub keyed_inputs: HashMap<String, (String, String)>,
    /// The step's output is read by a `partitions_from(...)` for keys: the worker checks it is
    /// a list or a dict it can write as JSON (`barca_model::keyed`).
    pub partition_source: bool,
    pub previous_sensor: Option<crate::dispatch::OutputRef>,
}

impl ItemSpec {
    pub(crate) fn from_step(step: &barca_compile::planner::StreamStep) -> Self {
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
            keyed_inputs: HashMap::new(),
            partition_source: step.keyed.is_source,
            previous_sensor: None,
        }
    }
}

// ─── Capacity ─────────────────────────────────────────────────────────────────

/// The weight an item counts for under `capacity`: its `weight=`, at least 1, and at most the
/// capacity. A step heavier than the capacity counts as the whole of it, so it starts only when
/// nothing else is running and then runs alone, rather than never (#436).
pub(crate) fn effective_weight(weight: u32, capacity: u32) -> u32 {
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
pub(crate) enum FailureAction {
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
    pub step_id: barca_model::StepId,
    pub spec: ItemSpec,
    pub attempts: u32,
    pub group: Option<GroupId>,
    pub deps: Vec<Dep>,
}

// ─── Parallel group ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParallelGroup {
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
    /// For a permanently failed item whose worker said so: where its last attempt failed.
    failure_phases: HashMap<ItemId, crate::dispatch::ErrorPhase>,
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
    call_assign: HashMap<String, (usize, usize)>,
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
    members: Vec<Option<ItemId>>,
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
            failure_phases: HashMap::new(),
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

    /// Pop the next ready item. Returns None if nothing is ready.
    /// `attempts` counts dispatches, so it is incremented here.
    pub(crate) fn next_ready(&mut self) -> Option<ItemId> {
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
    pub(crate) fn return_leased(&mut self, item_id: ItemId) {
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
    pub(crate) fn head_fits(&self, running: u32, capacity: u32) -> bool {
        self.ready
            .iter()
            .next()
            .is_some_and(|id| fits(running, self.items[&id].spec.weight, capacity))
    }

    /// The ready items in the order `next_ready` hands them out, left in the queue.
    pub fn sensor_previous(&mut self, previous: &HashMap<String, crate::dispatch::OutputRef>) {
        for item in self.items.values_mut() {
            if item.spec.kind == "sensor" {
                item.spec.previous_sensor = previous.get(&item.step_id.display()).cloned();
            }
        }
    }

    pub(crate) fn ready_items(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.ready.iter()
    }

    /// How many items are ready to be assigned.
    pub(crate) fn ready_count(&self) -> usize {
        self.ready.len()
    }

    /// How many items still have work ahead of them (ready now or blocked on
    /// upstreams). Note: batch sizing uses `ready_count` (the pull-eligible
    /// pool), not this — blocked items mustn't inflate a batch.
    pub(crate) fn remaining_count(&self) -> usize {
        self.ready.len() + self.pending.len() + self.waiting_retry.len()
    }

    // ─── Event handlers ───────────────────────────────────────────────────

    /// Returns true when all items are in a terminal state.
    pub(crate) fn is_finished(&self) -> bool {
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

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(crate) fn done_count(&self) -> usize {
        self.done.len()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
