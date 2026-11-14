//! Assigning ready items to idle workers: leases, the pool's capacity and weights, and how
//! many keys of a batched call one worker takes.

use super::*;

impl WorkerPool {
    /// Close a worker's lease for `node_id`. Workers execute their batch in
    /// order, so this is normally the front of the deque; matching by node id
    /// keeps us honest if a worker ever reports out of order.
    pub(super) fn take_lease(
        &mut self,
        worker_id: usize,
        node_id: &str,
        coord: &Coordinator,
    ) -> Option<ItemId> {
        let handle = self.workers.get_mut(&worker_id)?;
        let pos = handle
            .leases
            .iter()
            .position(|&iid| coord.item(iid).step_id.display() == node_id)?;
        let taken = handle.leases.remove(pos);
        handle.front_since = std::time::Instant::now();
        taken
    }

    /// Return every unstarted lease on a dead/frozen worker to the queue front.
    /// Popping from the back preserves the original order across push_fronts.
    pub(super) fn return_leases(handle: &mut WorkerHandle, coord: &mut Coordinator) {
        while let Some(item_id) = handle.leases.pop_back() {
            coord.return_leased(item_id);
        }
    }

    /// Assign ready items to idle workers in cost-sized batches, starting
    /// workers on demand up to `pool_size`.
    ///
    /// When ready work finds no idle worker, every worker it can use is started at once
    /// ([`Self::start_workers`]) and none is waited for: each joins the pool when it connects
    /// (`IoEvent::Connected`), and this runs again then. Starting them one at a time, each
    /// waited for before the next, cost 30-40 ms per worker (#560).
    pub(super) async fn assign_ready(
        &mut self,
        coord: &mut Coordinator,
        cost: &CostModel,
        cancel: &CancellationToken,
    ) {
        self.retire_surplus();
        self.sync_share(coord);
        loop {
            // A cancelled run starts nothing more: no worker, no step.
            if coord.ready_count() == 0 || cancel.is_cancelled() {
                self.leave_line(coord);
                return;
            }
            // The capacity is full for the step next in line: nothing starts until enough of
            // what is running finishes, and nothing queued behind it goes first (#436).
            if !coord.head_fits(self.running_weight(coord), self.capacity()) {
                self.leave_line(coord);
                return;
            }
            // And the capacity the server's runs share, when there is one (#628): the step
            // waits in its line until it is admitted.
            if !self.admit_head(coord) {
                return;
            }

            // Find an idle worker, or start the ones the ready work can use.
            let Some(wid) = self
                .workers
                .iter()
                .find(|(_, w)| w.leases.is_empty() && !w.batch_open)
                .map(|(&id, _)| id)
            else {
                self.start_workers(coord, cost);
                return;
            };

            // Lease a batch: K sized from the head item's measured cost.
            let len = self.batch_len(
                coord,
                cost,
                &mut coord.ready_items().peekable(),
                coord.ready_count(),
            );
            let batch: Vec<ItemId> = (0..len)
                .map(|_| coord.next_ready().expect("batch_len counts ready items"))
                .collect();
            // Steps another run of the server is executing wait for it instead (#654).
            let batch = self.claim_batch(coord, batch);
            if batch.is_empty() {
                // Nothing is leased: what the shared capacity admitted for the head goes back.
                if let Some(share) = self.share.as_mut() {
                    share.grant = None;
                }
                self.sync_share(coord);
                continue;
            }

            let call = coord.call_id(batch[0]).map(str::to_string);
            let msg = if let Some(call) = &call {
                // A batched call (RFC-0013): its keys go together, as one call of the function.
                let steps: Vec<serde_json::Value> = batch
                    .iter()
                    .map(|&iid| self.traced_step(coord.item(iid), coord))
                    .collect();
                let mut msg = crate::wire::encode_steps(&steps);
                msg["call"] = serde_json::json!({"id": call});
                msg
            } else if batch.len() == 1 {
                let step = self.traced_step(coord.item(batch[0]), coord);
                serde_json::json!({"type": "execute", "step": crate::wire::normalize_step(&step)})
            } else {
                let steps: Vec<serde_json::Value> = batch
                    .iter()
                    .map(|&iid| self.traced_step(coord.item(iid), coord))
                    .collect();
                crate::wire::encode_steps(&steps)
            };

            if self.trace_on {
                let names: Vec<String> = batch
                    .iter()
                    .map(|&iid| coord.item(iid).step_id.display())
                    .collect();
                barca_model::errln!(
                    "[trace]  {:>8.1}ms  dispatch -> worker {wid}: {names:?}",
                    self.trace_start.elapsed().as_secs_f64() * 1000.0
                );
            }

            self.steps_dispatched += batch.len();
            let handle = self
                .workers
                .get_mut(&wid)
                .expect("worker inserted or found above");
            if handle.cmd_tx.send(msg).await.is_err() {
                // Worker's I/O task is gone — undo the lease and drop the
                // worker; its Disconnected event does no further harm.
                for &item_id in batch.iter().rev() {
                    coord.return_leased(item_id);
                }
                if let Some(mut dead) = self.workers.remove(&wid) {
                    tokio::task::spawn_blocking(move || {
                        let _ = dead.child.kill();
                        let _ = dead.child.wait();
                    });
                }
                continue;
            }
            handle.batch_open = batch.len() > 1 || call.is_some();
            handle.call = call.is_some();
            handle.leases = batch.into_iter().collect();
            handle.front_since = std::time::Instant::now();
            // The admitted weight is the leased step's now (`running_weight` counts it).
            if let Some(share) = self.share.as_mut() {
                share.grant = None;
            }
        }
    }

    /// The capacity as a weight, at least 1.
    pub(super) fn capacity(&self) -> u32 {
        u32::try_from(self.config.capacity.max(1)).unwrap_or(u32::MAX)
    }

    /// The total weight of the steps running now: for each worker in the pool, the weight of
    /// what it has leased (a step with a weight above 1 is always leased alone, `batch_len`).
    /// A worker stopped in `parallel()` is not counted while its branches run, which take its
    /// weight (`Coordinator::on_parallel_requested`): counting both, a step could never get
    /// its branches started.
    pub(super) fn running_weight(&self, coord: &Coordinator) -> u32 {
        self.weight_against(coord, self.capacity())
    }

    /// [`Self::running_weight`], each step's weight at most `capacity`.
    pub(super) fn weight_against(&self, coord: &Coordinator, capacity: u32) -> u32 {
        self.workers
            .values()
            .map(|w| {
                w.leases
                    .iter()
                    .map(|&id| {
                        crate::coordinator::effective_weight(coord.item(id).spec.weight, capacity)
                    })
                    .max()
                    .unwrap_or(0)
            })
            .sum()
    }

    /// How many of the ready items `ready` yields (in queue order, `remaining` of them)
    /// make the next batch. Consumes those items from the iterator.
    ///
    /// K is sized from the head item's measured cost. The ceiling input is the pull-eligible
    /// pool (ready items), not pending work blocked on upstreams — items that can't be pulled
    /// this wave mustn't let one worker drain the whole ready queue. The batch is filled up
    /// to K, but estimated-heavy items are never packed behind a light head: the batch has a
    /// work budget of K × the head's cost (what K was computed for), and an item that would
    /// blow it waits for its own pull. Guards the over-batch tail-block when a phase mixes
    /// light and heavy nodes.
    pub(super) fn batch_len(
        &self,
        coord: &Coordinator,
        cost: &CostModel,
        ready: &mut std::iter::Peekable<impl Iterator<Item = ItemId>>,
        remaining: usize,
    ) -> usize {
        let Some(first) = ready.next() else {
            return 0;
        };
        // The keys of a batched call are leased together and alone (RFC-0013): they were
        // queued together once all of them were ready.
        if coord.call_id(first).is_some() {
            let mut len = 1;
            while let Some(&id) = ready.peek() {
                if !coord.same_call(first, id) {
                    break;
                }
                ready.next();
                len += 1;
            }
            return len;
        }
        // A step with a weight above 1 is leased on its own, and none is leased behind a
        // lighter one, so a worker's weight is the weight of the step it is running (#436).
        if coord.item(first).spec.weight > 1 {
            return 1;
        }
        let head_node = coord.item(first).step_id.display();
        // The phase's other steps (waiting on upstreams or on their stream) count toward
        // whether the work is trivial, not toward the ceiling.
        let backlog = coord.remaining_count().saturating_sub(coord.ready_count());
        let k = cost
            .batch_size_with_backlog(&head_node, remaining, backlog, self.config.pool_size.max(1))
            .max(1);
        let head_est = cost.estimate(&head_node);
        let budget = head_est * k as f64 * 1.5;
        let mut acc = head_est;
        let mut len = 1;
        while len < k {
            let Some(&id) = ready.peek() else {
                break;
            };
            if coord.item(id).spec.weight > 1 {
                break;
            }
            let est = cost.estimate(&coord.item(id).step_id.display());
            if acc + est > budget {
                break;
            }
            ready.next();
            acc += est;
            len += 1;
        }
        len
    }
}
