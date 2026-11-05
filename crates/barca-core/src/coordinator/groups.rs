//! `parallel()` groups: the branches a frozen caller waits for, their result directory and the
//! gates that hold dependents until every member is terminal.

use super::*;

impl Coordinator {
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

    /// Make a gate over `members`, counting those not yet terminal. `None` when every member
    /// already is, so there is nothing to wait for.
    pub(super) fn open_gate(&mut self, members: &[ItemId]) -> Option<ItemId> {
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

    pub(super) fn is_terminal(&self, id: ItemId) -> bool {
        self.done.contains(&id) || self.failed.contains_key(&id) || self.skipped.contains(&id)
    }

    /// `item_id` has become terminal: count it off every gate it is a member of, and release
    /// the waiters of each gate that has no member left to wait for.
    pub(super) fn settle_gates(&mut self, item_id: ItemId) {
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
}
