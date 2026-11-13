//! The worker pool's side of what a run's host shares between runs ([`crate::run_host`]):
//! this run's part of a shared capacity (#628) and its claims on shared asset steps (#654).
//! Under [`crate::run_host::LocalHost`] the pool holds neither and every method here returns
//! at once.

use std::collections::HashMap;

use tokio::task::JoinHandle;

use super::{EventCallback, IoEvent, WorkerPool};
use crate::coordinator::{Coordinator, ItemId};
use barca_model::events::RunEvent;

/// This run's part of a capacity the server's runs share (`barca_core::serve::capacity`).
///
/// `held` is what the run has taken from it: the weight of the steps its workers have leased
/// (as [`WorkerPool::running_weight`] counts them, against the shared capacity) plus `grant`,
/// the weight admitted for the head of the ready queue while a worker starts to take it. It is
/// brought back in line with what the run is running after every event
/// ([`WorkerPool::sync_share`]); everything is given back when the run's pool goes.
pub(super) struct Share {
    pub(super) host: std::sync::Arc<dyn crate::run_host::RunHost>,
    /// The shared capacity's total weight.
    pub(super) total: u32,
    pub(super) changed: tokio::sync::watch::Receiver<u64>,
    pub(super) run_id: String,
    pub(super) held: u32,
    pub(super) grant: Option<u32>,
    /// The run's head step waits in the shared capacity's line.
    pub(super) waiting: bool,
}

impl Drop for Share {
    fn drop(&mut self) {
        self.host.leave_capacity(&self.run_id);
    }
}

/// This run's claims on the asset steps the server's runs share (`barca_core::serve::claims`).
pub(super) struct Steps {
    pub(super) host: std::sync::Arc<dyn crate::run_host::RunHost>,
    pub(super) run_id: String,
    /// The claims this run holds: the steps its workers have leased.
    pub(super) held: std::collections::HashSet<crate::run_host::StepKey>,
    /// Steps of this run waiting on another run's claim, and the task that waits.
    pub(super) awaiting: HashMap<ItemId, JoinHandle<()>>,
    /// Steps another run finished for this one, by display id: cache hits, not executed here.
    pub(super) from_others: Vec<String>,
}

impl Drop for Steps {
    fn drop(&mut self) {
        for (_, task) in self.awaiting.drain() {
            task.abort();
        }
        self.host.leave_steps(&self.run_id);
    }
}

/// What identifies `item` as work runs may share: an asset step with a run hash, not a
/// `parallel()` branch. `None` for a task, a sensor or a branch.
fn share_key(item: &crate::coordinator::Item) -> Option<crate::run_host::StepKey> {
    if item.spec.kind != "asset" || item.group.is_some() {
        return None;
    }
    let hash = item.spec.run_hash.clone()?;
    Some((item.step_id.display(), hash))
}

/// Wait until the shared capacity changes, when the run waits in its line; never otherwise.
pub(super) async fn share_changed(share: &mut Option<Share>) {
    match share {
        Some(s) if s.waiting => {
            if s.changed.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        _ => std::future::pending::<()>().await,
    }
}

impl WorkerPool {
    /// The steps another run of the server finished while this run waited for them
    /// (`barca_core::serve::claims`), by display id, since the last call: cache hits for this run.
    pub fn take_from_other_runs(&mut self) -> Vec<String> {
        self.steps
            .as_mut()
            .map(|s| std::mem::take(&mut s.from_others))
            .unwrap_or_default()
    }

    /// Another run's claim on `item_id`, which this run waited for, ended
    /// ([`IoEvent::Settled`]). With the owner's artifact the step is a cache hit here, as if it
    /// had been found cached when this run planned it; without one this run executes it (the
    /// wait cost it no attempt). False when the run no longer waits for it (nothing changed).
    pub(super) fn settle(
        &mut self,
        coord: &mut Coordinator,
        item_id: ItemId,
        artifact: Option<serde_json::Value>,
        on_event: &mut Option<EventCallback<'_>>,
    ) -> bool {
        let Some(steps) = self.steps.as_mut() else {
            return false;
        };
        if steps.awaiting.remove(&item_id).is_none() {
            return false;
        }
        match artifact {
            Some(artifact) => {
                let node_id = coord.item(item_id).step_id.display();
                steps.from_others.push(node_id.clone());
                coord.record_output(item_id, artifact);
                if let Some(ev) = on_event {
                    ev(RunEvent::StepFinished {
                        node_id,
                        ok: true,
                        cached: true,
                        elapsed_seconds: None,
                        error: None,
                        import_seconds: None,
                    });
                }
                coord.on_item_completed(item_id);
            }
            None => coord.return_leased(item_id),
        }
        true
    }

    /// Claim the asset steps of `batch` for this run (`barca_core::serve::claims`) and return the ones
    /// to lease. A step another run is executing is kept out: it waits for that run's claim
    /// to end ([`IoEvent::Settled`]), leased to no worker.
    pub(super) fn claim_batch(&mut self, coord: &Coordinator, batch: Vec<ItemId>) -> Vec<ItemId> {
        let Some(steps) = self.steps.as_mut() else {
            return batch;
        };
        let mut lease = Vec::with_capacity(batch.len());
        for item_id in batch {
            let Some(key) = share_key(coord.item(item_id)) else {
                lease.push(item_id);
                continue;
            };
            match steps.host.claim_step(&steps.run_id, &key) {
                crate::run_host::StepClaim::Mine => {
                    steps.held.insert(key);
                    lease.push(item_id);
                }
                crate::run_host::StepClaim::Theirs { owner, settled } => {
                    if self.trace_on {
                        barca_model::errln!(
                            "[trace]  {:>8.1}ms  {} waits for run {owner}, which is computing it",
                            self.trace_start.elapsed().as_secs_f64() * 1000.0,
                            key.0
                        );
                    }
                    let tx = self.event_tx.clone();
                    let task = tokio::spawn(async move {
                        let artifact = settled.await;
                        let _ = tx.send(IoEvent::Settled { item_id, artifact }).await;
                    });
                    steps.awaiting.insert(item_id, task);
                }
            }
        }
        lease
    }

    /// `item_id` finished on one of this run's workers: the runs waiting on its claim get
    /// `artifact`.
    pub(super) fn finish_claim(
        &mut self,
        coord: &Coordinator,
        item_id: ItemId,
        artifact: &serde_json::Value,
    ) {
        let Some(steps) = self.steps.as_mut() else {
            return;
        };
        let Some(key) = share_key(coord.item(item_id)) else {
            return;
        };
        if steps.held.remove(&key) {
            let mut artifact = artifact.clone();
            // This run's own upload copy, removed once uploaded: not the waiters' to read.
            if let Some(map) = artifact.as_object_mut() {
                map.remove("upload_snapshot");
            }
            steps.host.finish_step(&steps.run_id, &key, artifact);
        }
    }

    /// Release the claims on steps no worker of this run has leased any more (they failed,
    /// their worker died, or they went back to the queue): a run waiting on one executes it.
    pub(super) fn sync_claims(&mut self, coord: &Coordinator) {
        let Some(steps) = self.steps.as_mut() else {
            return;
        };
        if steps.held.is_empty() {
            return;
        }
        let leased: std::collections::HashSet<crate::run_host::StepKey> = self
            .workers
            .values()
            .flat_map(|w| w.leases.iter().copied())
            .chain(
                self.frozen
                    .iter()
                    .flat_map(|f| std::iter::once(f.parent_item).chain(f.rest.iter().copied())),
            )
            .filter_map(|id| share_key(coord.item(id)))
            .collect();
        let host = &steps.host;
        let run_id = &steps.run_id;
        steps.held.retain(|key| {
            let keep = leased.contains(key);
            if !keep {
                host.release_step(run_id, key);
            }
            keep
        });
    }

    /// The weight the head of the ready queue counts for against the shared capacity.
    fn head_share_weight(&self, coord: &Coordinator) -> Option<(u32, String)> {
        let share = self.share.as_ref()?;
        let head = coord.ready_items().next()?;
        let item = coord.item(head);
        Some((
            share.host.weight_of(item.spec.weight),
            item.step_id.display(),
        ))
    }

    /// Whether the head of the ready queue is admitted by the capacity the server's runs share
    /// (always, without one). Admitted, its weight is held as `grant` until a worker leases
    /// it; not admitted, the run waits in the line and tries again when the capacity changes.
    pub(super) fn admit_head(&mut self, coord: &Coordinator) -> bool {
        let Some((weight, node)) = self.head_share_weight(coord) else {
            return true;
        };
        let share = self.share.as_mut().expect("head_share_weight found one");
        match share.grant {
            Some(granted) if granted == weight => return true,
            // The head changed since it was admitted (a step returned to the front): give the
            // weight back and ask for the new head's.
            Some(granted) => {
                share.host.release_capacity(&share.run_id, granted);
                share.held = share.held.saturating_sub(granted);
                share.grant = None;
            }
            None => {}
        }
        // Seen before asking, so a change after this is not missed by `share_changed`.
        share.changed.borrow_and_update();
        if share.host.try_acquire(&share.run_id, &node, weight) {
            share.held += weight;
            share.grant = Some(weight);
            share.waiting = false;
            true
        } else {
            share.waiting = true;
            false
        }
    }

    /// The head of the ready queue does not wait for the shared capacity any more (nothing is
    /// ready, the run is cancelled, or the step does not fit the run's own capacity): leave the
    /// line, and give back weight admitted for a head that is not going to be leased.
    pub(super) fn leave_line(&mut self, coord: &Coordinator) {
        let Some(share) = self.share.as_mut() else {
            return;
        };
        if share.waiting {
            share.host.withdraw(&share.run_id);
            share.waiting = false;
        }
        if share.grant.take().is_some() {
            self.sync_share(coord);
        }
    }

    /// Bring what this run holds of the shared capacity in line with what it runs: the weight
    /// of its leased steps (against the shared capacity) and the weight admitted for its head.
    /// Ended steps give their weight back here; a step resumed after its `parallel()` branches
    /// takes its weight back here ([`crate::run_host::RunHost::force_capacity`]).
    pub(super) fn sync_share(&mut self, coord: &Coordinator) {
        let Some(total) = self.share.as_ref().map(|s| s.total) else {
            return;
        };
        let running = self.weight_against(coord, total);
        let share = self.share.as_mut().expect("checked above");
        let wanted = running + share.grant.unwrap_or(0);
        if wanted < share.held {
            share
                .host
                .release_capacity(&share.run_id, share.held - wanted);
        } else if wanted > share.held {
            share
                .host
                .force_capacity(&share.run_id, wanted - share.held);
        }
        share.held = wanted;
    }
}
