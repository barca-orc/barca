//! Loading a phase into the coordinator: items, their dependency edges and ordering.

use super::*;

impl Coordinator {
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
    pub(super) fn reserve_item_id(&mut self) -> ItemId {
        let id = ItemId(self.next_item_id);
        self.next_item_id += 1;
        id
    }

    /// Create the item for a previously [`reserve_item_id`](Self::reserve_item_id)'d
    /// id: inserts it, registers dependents, and places it on the ready or
    /// pending queue. This is `add_item`'s body, split out so `load_phase` can
    /// reserve every id up front and only then resolve deps.
    pub(super) fn finalize_item(
        &mut self,
        id: ItemId,
        step_id: crate::StepId,
        spec: ItemSpec,
        deps: Vec<Dep>,
    ) {
        if let Some(&(call, position)) = self.call_assign.get(&step_id.display()) {
            self.call_of.insert(id, call);
            self.calls[call].members[position] = Some(id);
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
    pub(super) fn push_ordering_deps(
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

    /// How many dependency edges the loaded items hold. Grows linearly with the number of
    /// partition keys; the regression check for #345 holds it to that.
    #[cfg(test)]
    pub(super) fn edge_count(&self) -> usize {
        self.items
            .values()
            .map(|item| item.deps.len())
            .sum::<usize>()
            + self.gates.values().sum::<usize>()
    }

    // ─── Queries ──────────────────────────────────────────────────────────
}
