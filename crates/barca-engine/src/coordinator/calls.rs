//! Batched calls (RFC-0013): which items share one worker call, and when a call's items are
//! settled or released for retry.

use super::*;

impl Coordinator {
    /// Group the items this phase will load into calls, before [`Self::load_phase`]: each call
    /// is `(its id, the display ids of its keys in key order)`. An item of a call is leased
    /// with the rest of its call, once every one of them is ready.
    pub fn assign_calls(&mut self, calls: Vec<(String, Vec<String>)>) {
        for (id, members) in calls {
            let index = self.calls.len();
            self.calls.push(Call {
                id,
                unreleased: members.len(),
                members: vec![None; members.len()],
                ..Default::default()
            });
            for (position, m) in members.into_iter().enumerate() {
                self.call_assign.insert(m, (index, position));
            }
        }
    }

    /// Declare the nodes (base ids) that take a list of keys, before [`Self::load_phase`]. A
    /// key of one that is loaded without a call assigned (by [`Self::assign_calls`]) gets a
    /// call of its own, with the id `u<n>`, `n` counting from `first_id`: the function of a
    /// batched node is never called with a single key (RFC-0013). They are listed by
    /// [`Self::unassigned_calls`].
    pub fn require_calls(&mut self, batched: HashSet<String>, first_id: u64) {
        self.batched = batched;
        self.next_unassigned = first_id;
    }

    /// The calls [`Self::require_calls`] made for keys that were assigned none: (the call's
    /// id, the key's display id), in load order. Empty when the planner assigned every key.
    pub fn unassigned_calls(&self) -> &[(String, String)] {
        &self.unassigned
    }

    /// Give a key of a batched node that was assigned no call a call of its own (see
    /// [`Self::require_calls`]).
    pub(super) fn call_for_unassigned(&mut self, id: ItemId, step_id: &barca_model::StepId) {
        if step_id.partition.is_empty() || !self.batched.contains(step_id.base_id()) {
            return;
        }
        let call = format!("u{}", self.next_unassigned);
        self.next_unassigned += 1;
        self.unassigned.push((call.clone(), step_id.display()));
        let index = self.calls.len();
        self.calls.push(Call {
            id: call,
            unreleased: 1,
            members: vec![Some(id)],
            ..Default::default()
        });
        self.call_of.insert(id, index);
    }

    /// Whether `item`'s function takes a list of keys: it is a key of a call, or of a node
    /// declared batched (see [`Self::require_calls`]).
    pub(crate) fn takes_keys(&self, item: ItemId) -> bool {
        self.call_of.contains_key(&item)
            || self.items.get(&item).is_some_and(|i| {
                !i.step_id.partition.is_empty() && self.batched.contains(i.step_id.base_id())
            })
    }

    /// After loading: a call whose keys were not all loaded waits only for the ones that were.
    pub(super) fn settle_calls(&mut self) {
        for c in 0..self.calls.len() {
            let loaded = self.calls[c].members.iter().flatten().count();
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
    pub(crate) fn call_id(&self, item: ItemId) -> Option<&str> {
        self.call_of.get(&item).map(|&c| self.calls[c].id.as_str())
    }

    /// Whether two items are keys of the same call.
    pub(crate) fn same_call(&self, a: ItemId, b: ItemId) -> bool {
        match (self.call_of.get(&a), self.call_of.get(&b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }

    /// `id` will never run (its upstream failed): its call does not wait for it.
    pub(super) fn count_off_call(&mut self, id: ItemId) {
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
    pub(super) fn release_call(&mut self, c: usize) {
        let held: HashSet<ItemId> = std::mem::take(&mut self.calls[c].held)
            .into_iter()
            .collect();
        for &m in self.calls[c].members.iter().flatten() {
            if held.contains(&m) {
                self.ready.push_back(m);
            }
        }
    }

    /// A key of a call was reported (or returned unstarted): one fewer outstanding, and the
    /// keys to retry go back together once the attempt is over.
    pub(super) fn call_reported(&mut self, id: ItemId) {
        let Some(&c) = self.call_of.get(&id) else {
            return;
        };
        let call = &mut self.calls[c];
        call.outstanding = call.outstanding.saturating_sub(1);
        self.release_retries(c);
    }

    pub(super) fn release_retries(&mut self, c: usize) {
        let call = &self.calls[c];
        if call.outstanding > 0
            || call.retry.is_empty()
            || call.retry.iter().any(|m| self.waiting_retry.contains(m))
        {
            return;
        }
        let retry = std::mem::take(&mut self.calls[c].retry);
        let size = self.calls[c].members.iter().flatten().count();
        if size > 1 {
            let limit = size.div_ceil(2);
            for (part, members) in retry.chunks(limit).enumerate() {
                let index = self.calls.len();
                self.calls.push(Call {
                    id: format!("{}-retry-{part}", self.calls[c].id),
                    members: members.iter().copied().map(Some).collect(),
                    ..Default::default()
                });
                for &m in members {
                    self.call_of.insert(m, index);
                    self.ready.push_back(m);
                }
            }
        } else {
            for m in retry {
                self.ready.push_back(m);
            }
        }
    }

    // ─── Scheduling ────────────────────────────────────────────────────────
}
