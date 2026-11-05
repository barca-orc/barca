//! How items end: completion, failure and the cascade that skips dependents, and the outputs
//! recorded for them.

use super::*;

impl Coordinator {
    /// Mark an item as completed. Unblocks dependents into the ready queue.
    pub(crate) fn on_item_completed(&mut self, item_id: ItemId) {
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
    pub(crate) fn on_item_failed(
        &mut self,
        item_id: ItemId,
        error: String,
        elapsed: std::time::Duration,
    ) -> FailureAction {
        self.on_item_failed_in(item_id, error, elapsed, None)
    }

    /// [`Coordinator::on_item_failed`] for an attempt whose worker said where it failed.
    pub(crate) fn on_item_failed_in(
        &mut self,
        item_id: ItemId,
        error: String,
        elapsed: std::time::Duration,
        phase: Option<crate::dispatch::ErrorPhase>,
    ) -> FailureAction {
        self.executing.remove(&item_id);

        let now = std::time::SystemTime::now();
        let attempt_start = now.checked_sub(elapsed).unwrap_or(now);
        let first = *self
            .first_attempt_at
            .entry(item_id)
            .or_insert(attempt_start);

        let can_split = self
            .call_of
            .get(&item_id)
            .is_some_and(|&c| self.calls[c].members.iter().flatten().count() > 1);
        let item = self.items.get_mut(&item_id).unwrap();

        if can_split || item.attempts < item.spec.retries {
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
            if let Some(phase) = phase {
                self.failure_phases.insert(item_id, phase);
            }
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

    pub(crate) fn failed_items(&self) -> Vec<(ItemId, &str)> {
        self.failed
            .iter()
            .map(|(id, msg)| (*id, msg.as_str()))
            .collect()
    }

    /// When a permanently failed item's first attempt started, and the wall time from then
    /// to its permanent failure (retries and backoff included).
    pub(crate) fn failure_clock(
        &self,
        item_id: ItemId,
    ) -> Option<(std::time::SystemTime, std::time::Duration)> {
        self.failure_clocks.get(&item_id).copied()
    }

    /// Where a permanently failed item failed, when its worker reported it.
    pub(crate) fn failure_phase(&self, item_id: ItemId) -> Option<crate::dispatch::ErrorPhase> {
        self.failure_phases.get(&item_id).copied()
    }

    pub(crate) fn skipped_items(&self) -> Vec<ItemId> {
        self.skipped.iter().copied().collect()
    }

    pub(crate) fn is_done(&self, item_id: ItemId) -> bool {
        self.done.contains(&item_id)
    }

    /// Record an output artifact for a completed item.
    pub(crate) fn record_output(&mut self, item_id: ItemId, artifact: serde_json::Value) {
        if let Some(item) = self.items.get(&item_id) {
            self.output_by_display
                .insert(item.step_id.display(), item_id);
        }
        self.outputs.insert(item_id, artifact);
    }

    /// The recorded output of the item whose display id is `display_id`, if any.
    pub(crate) fn output_for_display(&self, display_id: &str) -> Option<&serde_json::Value> {
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
    pub(super) fn cascade_failure(&mut self, item_id: ItemId) {
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
    pub(super) fn unblock_dependents(&mut self, item_id: ItemId) {
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
