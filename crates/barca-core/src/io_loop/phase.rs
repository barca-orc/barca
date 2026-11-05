//! Running one phase: the event loop over worker messages, progress and tracing hooks.

use super::*;

impl WorkerPool {
    /// Library warnings the workers printed once and then suppressed, with how many times
    /// each was suppressed, most repeated first. Drains the tally.
    pub(crate) fn take_repeated_warnings(&mut self) -> Vec<(String, u64)> {
        let mut out = std::mem::take(&mut self.repeated_warnings);
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// The run's Datadog exporter, when that integration is on.
    pub(crate) fn datadog(&self) -> Option<std::sync::Arc<crate::telemetry::datadog::Dispatch>> {
        self.config.datadog.clone()
    }

    /// Report steps that stay in flight longer than the progress interval, so a long
    /// step does not look like a hang.
    pub(crate) fn on_running(&mut self, hook: RunningHook) {
        self.running_hook = Some(hook);
    }

    /// Add the step's trace context without requiring a Python SDK.
    pub(super) fn traced_step(
        &self,
        item: &crate::coordinator::Item,
        coord: &Coordinator,
    ) -> serde_json::Value {
        let mut step = build_step_json(item, coord);
        if let Some(dispatch) = &self.config.datadog {
            // Dynamic parallel items have no trace of their own. Attach their Python
            // execution to the nearest reported ancestor's.
            let mut parent = item;
            while let Some(group) = parent.group {
                parent = coord.item(coord.group(group).parent);
            }
            step["datadog"] = dispatch.context(&parent.step_id, item.attempts);
        }
        step
    }

    /// `(node_id, seconds)` for every worker's in-flight step older than the interval.
    pub(super) fn running_steps(&self, coord: &Coordinator) -> Vec<(String, f64)> {
        let mut out: Vec<(String, f64)> = self
            .workers
            .values()
            .filter_map(|w| {
                let &iid = w.leases.front()?;
                let secs = w.front_since.elapsed();
                (secs >= self.progress_interval)
                    .then(|| (coord.item(iid).step_id.display(), secs.as_secs_f64()))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Drive one phase's coordinator to completion against the (persistent)
    /// pool. `cost` supplies batch sizes and absorbs the timings coming back.
    ///
    /// Cancelling `cancel` returns `Err("run cancelled")` promptly; workers
    /// stay alive until [`WorkerPool::shutdown`], which the caller runs on
    /// every exit path.
    pub(crate) async fn run_phase(
        &mut self,
        coord: &mut Coordinator,
        cost: &mut CostModel,
        mut on_step: Option<StepCallback<'_>>,
        mut on_event: Option<EventCallback<'_>>,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("run cancelled".to_string());
        }
        self.assign_ready(coord, cost, cancel).await;

        let mut ticker = if self.running_hook.is_some() && !self.progress_interval.is_zero() {
            let mut t = tokio::time::interval(self.progress_interval);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            t.tick().await; // the first tick fires immediately
            Some(t)
        } else {
            None
        };

        // Polls the workers still starting (`fail_dead_starts`).
        let mut start_watch = tokio::time::interval(Duration::from_millis(10));
        start_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            if coord.is_finished() {
                break;
            }
            // Before the check below: a worker whose start was cancelled is not a pool that
            // failed to start.
            if cancel.is_cancelled() {
                return Err("run cancelled".to_string());
            }
            // Give the shared capacity back what the last event ended, and the claims on steps
            // that are no longer leased.
            self.sync_share(coord);
            self.sync_claims(coord);
            let waiting_for_share = self.share.as_ref().is_some_and(|s| s.waiting);
            if self.workers.is_empty()
                && self.pending.is_empty()
                && self.frozen.is_empty()
                && coord.ready_count() > 0
                && !waiting_for_share
            {
                // Ready work exists but spawning failed — nothing will ever
                // produce a completion event.
                return Err("no workers available and work remains".to_string());
            }

            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    // Cooperative cancellation: the caller's shutdown()
                    // terminates every worker (frozen ones included).
                    return Err("run cancelled".to_string());
                }
                ev = self.event_rx.recv() => match ev {
                    Some(e) => e,
                    None => break,
                },
                // A worker that is starting fails when its process exits or its deadline
                // passes, whichever comes first.
                // The capacity the server's runs share moved: the step this run waits with may
                // be admitted now.
                _ = share_changed(&mut self.share), if waiting_for_share => {
                    self.assign_ready(coord, cost, cancel).await;
                    continue;
                }
                _ = start_watch.tick(), if !self.pending.is_empty() => {
                    // Not started again here: a worker that cannot start would be started
                    // over and over. With no worker left, the check above ends the phase.
                    self.fail_dead_starts();
                    continue;
                }
                _ = async {
                    match ticker.as_mut() {
                        Some(t) => { t.tick().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let running = self.running_steps(coord);
                    if !running.is_empty()
                        && let Some(hook) = self.running_hook.as_mut() {
                            hook(&running);
                        }
                    continue;
                }
            };

            match event {
                IoEvent::Message { worker_id, msg } => match *msg {
                    WorkerMessage::StepCompleted {
                        ref node_id,
                        ref artifact,
                    } => {
                        if self.trace_on {
                            crate::errln!(
                                "[trace]  {:>8.1}ms  StepCompleted <- worker {worker_id}: {node_id} (self-reported elapsed={:.1}ms cpu={:.1}ms)",
                                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                                artifact.elapsed_seconds.unwrap_or(0.0) * 1000.0,
                                artifact.cpu_seconds.unwrap_or(0.0) * 1000.0,
                            );
                        }
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            crate::errln!(
                                "[barca] StepCompleted for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        // Feed the estimator: within-run adaptation means the
                        // very next pull sizes K from this observation.
                        if let Some(wall) = artifact.elapsed_seconds {
                            cost.observe(
                                node_id,
                                wall,
                                artifact.cpu_seconds.unwrap_or(wall),
                                artifact.max_rss_bytes.unwrap_or(0),
                            );
                        }

                        // Record output and fire progress callback
                        let artifact_val = serde_json::to_value(artifact).unwrap_or_default();
                        self.finish_claim(coord, item_id, &artifact_val);
                        coord.record_output(item_id, artifact_val.clone());
                        if let Some(ref mut cb) = on_step {
                            cb(node_id, &artifact_val, coord.item(item_id).attempts);
                        }
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::StepFinished {
                                node_id: node_id.clone(),
                                ok: true,
                                cached: false,
                                elapsed_seconds: artifact.elapsed_seconds,
                                error: None,
                                import_seconds: artifact.import_seconds,
                            });
                        }
                        coord.on_item_completed(item_id);
                        self.drop_branch_results(item_id);

                        // Check if any frozen worker's group is now complete
                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::StepError {
                        ref node_id,
                        error_type,
                        message,
                        traceback,
                        elapsed,
                        import_seconds,
                        worker_continues,
                        phase,
                    } => {
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            crate::errln!(
                                "[barca] StepError for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        // Surface the full picture: exception type, message, and
                        // the (barca-frame-filtered) traceback from the worker.
                        let mut error = format!("{error_type}: {message}");
                        if !traceback.trim().is_empty() {
                            error.push('\n');
                            error.push_str(&traceback);
                        }
                        if let Some(ref mut ev) = on_event {
                            // The attempt's traceback is part of the step's stderr, as Python
                            // would have printed it. Recorded, not echoed: the failure line
                            // says it already.
                            for line in traceback_lines(&error_type, &message, &traceback) {
                                ev(RunEvent::log(
                                    node_id.clone(),
                                    line,
                                    crate::LogStream::Stderr,
                                ));
                            }
                            ev(RunEvent::StepFinished {
                                node_id: node_id.clone(),
                                ok: false,
                                cached: false,
                                elapsed_seconds: Some(elapsed),
                                error: Some(error.clone()),
                                import_seconds,
                            });
                        }
                        if let FailureAction::RetryAfter(delay) = coord.on_item_failed_in(
                            item_id,
                            error,
                            Duration::from_secs_f64(elapsed.max(0.0)),
                            phase
                                .as_deref()
                                .and_then(crate::dispatch::ErrorPhase::parse),
                        ) {
                            schedule_retry(&self.event_tx, item_id, delay);
                        }
                        self.drop_branch_results(item_id);

                        // A step that raised leaves its worker sound, and the worker goes on
                        // with its batch (#523). One that may not be sound (a failed import,
                        // `sys.exit()`, still running past its timeout) is stopped so retries
                        // and remaining work get a fresh interpreter: unstarted leases go back
                        // to the queue front, and the 200ms SIGTERM grace runs off the event
                        // loop.
                        if worker_continues {
                            self.failures_survived += 1;
                        } else if let Some(mut handle) = self.workers.remove(&worker_id) {
                            Self::return_leases(&mut handle, coord);
                            tokio::task::spawn_blocking(move || {
                                graceful_kill(&mut handle.child);
                            });
                        }

                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Blocked {
                        ref node_id,
                        reason,
                    } => {
                        let ran_for = self
                            .workers
                            .get(&worker_id)
                            .map_or(Duration::ZERO, |h| h.front_since.elapsed());
                        let Some(item_id) = self.take_lease(worker_id, node_id, coord) else {
                            crate::errln!(
                                "[barca] Blocked for '{node_id}' from worker {worker_id} \
                                 with no matching lease — ignoring"
                            );
                            continue;
                        };

                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(item_id, format!("blocked: {reason}"), ran_for)
                        {
                            schedule_retry(&self.event_tx, item_id, delay);
                        }

                        self.resume_frozen(coord).await;
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Submit {
                        items,
                        artifact_results,
                    } => {
                        let Some(handle) = self.workers.get(&worker_id) else {
                            crate::errln!(
                                "[barca] Submit from unknown worker {worker_id}, ignoring"
                            );
                            continue;
                        };
                        let Some(&item_id) = handle.leases.front() else {
                            crate::errln!(
                                "[barca] Submit from worker {worker_id} with no lease, ignoring"
                            );
                            continue;
                        };

                        let specs: Vec<ItemSpec> = items
                            .into_iter()
                            .map(|si| {
                                let (source_file, function_name) = si
                                    .fn_ref
                                    .rsplit_once(':')
                                    .map(|(f, n)| (f.to_string(), n.to_string()))
                                    .unwrap_or_else(|| (String::new(), si.fn_ref.clone()));
                                ItemSpec {
                                    fn_ref: si.fn_ref,
                                    function_name,
                                    source_file,
                                    direct_args: si.args,
                                    direct_kwargs: si.kwargs,
                                    dag_inputs: HashMap::new(),
                                    timeout_seconds: 300,
                                    retries: 1,
                                    // Set to the caller's by `on_parallel_requested`.
                                    weight: 1,
                                    retry_backoff_seconds: 0.0,
                                    serializer: None,
                                    sinks: Vec::new(),
                                    run_hash: None,
                                    upstream_inputs: HashMap::new(),
                                    collected_inputs: HashMap::new(),
                                    param_types: HashMap::new(),
                                    return_type: None,
                                    kind: "task".to_string(),
                                    keyed_inputs: HashMap::new(),
                                    partition_source: false,
                                    previous_sensor: None,
                                }
                            })
                            .collect();

                        let (group_id, _child_ids) = coord.on_parallel_requested(item_id, specs);
                        // The group's own directory for what its branches return, removed
                        // when the step that called parallel() ends (`drop_branch_results`).
                        let result_dir = self.branch_root.join(self.next_branch_group.to_string());
                        self.next_branch_group += 1;
                        if let Err(e) = std::fs::create_dir_all(&result_dir) {
                            crate::errln!("[barca] could not create {}: {e}", result_dir.display());
                        }
                        coord.set_group_result_dir(group_id, result_dir.clone());
                        self.branch_dirs
                            .entry(item_id)
                            .or_default()
                            .push(result_dir);

                        // The parent blocks frozen on its group. Whatever else this
                        // worker had leased stays leased to it (see `FrozenWorker::rest`).
                        let mut handle = self
                            .workers
                            .remove(&worker_id)
                            .expect("worker existence checked above");
                        handle.leases.pop_front();
                        let rest = std::mem::take(&mut handle.leases);

                        // SIGSTOP the requesting worker, move it to frozen list
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(handle.child.id() as i32, libc::SIGSTOP);
                        }

                        // The frozen worker no longer counts towards the pool: the
                        // `assign_ready` below starts its replacement when the branches
                        // need one (no idle worker), without waiting for it to connect.
                        self.frozen.push(FrozenWorker {
                            child: handle.child,
                            cmd_tx: handle.cmd_tx,
                            _task: handle._task,
                            parent_item: item_id,
                            front_since: handle.front_since,
                            group_id,
                            original_worker_id: worker_id,
                            rest,
                            artifact_results,
                            batch_open: handle.batch_open,
                        });

                        // Assign ready items (children are now in the ready queue)
                        self.assign_ready(coord, cost, cancel).await;
                    }
                    WorkerMessage::Heartbeat => {}
                    WorkerMessage::Log {
                        node_id,
                        line,
                        stream,
                        t,
                    } => {
                        // A line of user stdout or stderr — forward live, stamped with when
                        // the worker wrote it; the caller persists it. Does not change
                        // worker/coordinator state.
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::Log {
                                node_id,
                                line,
                                stream,
                                ts: Some(t.map_or_else(
                                    crate::events::log_timestamp,
                                    crate::events::epoch_timestamp,
                                )),
                            });
                        }
                    }
                    WorkerMessage::RepeatedWarnings { counts } => {
                        for (text, n) in counts {
                            match self.repeated_warnings.iter_mut().find(|(t, _)| *t == text) {
                                Some((_, total)) => *total += n,
                                None => self.repeated_warnings.push((text, n)),
                            }
                        }
                    }
                },
                IoEvent::BatchFinished { worker_id } => {
                    if let Some(handle) = self.workers.get_mut(&worker_id) {
                        handle.batch_open = false;
                    }
                    self.resume_frozen(coord).await;
                    self.assign_ready(coord, cost, cancel).await;
                }
                IoEvent::Disconnected { worker_id } => {
                    // Worker crashed — its in-flight item failed; unstarted
                    // leases return to the queue for another worker.
                    if let Some(mut handle) = self.workers.remove(&worker_id) {
                        // A batched call runs all its keys at once: a dead worker fails each.
                        let in_flight_items: Vec<ItemId> = if handle.call {
                            handle.leases.drain(..).collect()
                        } else {
                            handle.leases.pop_front().into_iter().collect()
                        };
                        let status = if in_flight_items.is_empty() {
                            None
                        } else {
                            Some(reap_briefly(&mut handle.child).await)
                        };
                        for in_flight in in_flight_items {
                            let ran_for = handle.front_since.elapsed();
                            let node_id = coord.item(in_flight).step_id.display();
                            let error = disconnect_message(&node_id, status.flatten(), false);
                            if let Some(ref mut ev) = on_event {
                                ev(RunEvent::StepFinished {
                                    node_id,
                                    ok: false,
                                    cached: false,
                                    elapsed_seconds: Some(ran_for.as_secs_f64()),
                                    error: Some(error.clone()),
                                    import_seconds: None,
                                });
                            }
                            if let FailureAction::RetryAfter(delay) =
                                coord.on_item_failed(in_flight, error, ran_for)
                            {
                                schedule_retry(&self.event_tx, in_flight, delay);
                            }
                            self.drop_branch_results(in_flight);
                        }
                        Self::return_leases(&mut handle, coord);
                        tokio::task::spawn_blocking(move || {
                            let _ = handle.child.kill();
                            let _ = handle.child.wait();
                        });
                    } else if let Some(at) = self
                        .frozen
                        .iter()
                        .position(|fw| fw.original_worker_id == worker_id)
                    {
                        // A worker that died while frozen in parallel() (killed, out of
                        // memory). It is a dead worker like any other: its step failed. This
                        // used to go unnoticed, since only running workers were looked up,
                        // and when its branches finished there was nobody to resume: the run
                        // never ended (#333).
                        let mut fw = self.frozen.swap_remove(at);
                        let ran_for = fw.front_since.elapsed();
                        let node_id = coord.item(fw.parent_item).step_id.display();
                        // A stopped process is not reported by `try_wait` (no WUNTRACED), so
                        // a frozen worker is only named here once it has really exited.
                        let status = reap_briefly(&mut fw.child).await;
                        let error = disconnect_message(&node_id, status, true);
                        if let Some(ref mut ev) = on_event {
                            ev(RunEvent::StepFinished {
                                node_id,
                                ok: false,
                                cached: false,
                                elapsed_seconds: Some(ran_for.as_secs_f64()),
                                error: Some(error.clone()),
                                import_seconds: None,
                            });
                        }
                        coord.abandon_group(fw.group_id);
                        if let FailureAction::RetryAfter(delay) =
                            coord.on_item_failed(fw.parent_item, error, ran_for)
                        {
                            schedule_retry(&self.event_tx, fw.parent_item, delay);
                        }
                        self.drop_branch_results(fw.parent_item);
                        // The rest of its batch never started and nobody has it in hand now.
                        while let Some(item_id) = fw.rest.pop_back() {
                            coord.return_leased(item_id);
                        }
                        tokio::task::spawn_blocking(move || {
                            let _ = fw.child.kill();
                            let _ = fw.child.wait();
                        });
                    }
                    self.resume_frozen(coord).await;
                    self.assign_ready(coord, cost, cancel).await;
                }
                IoEvent::RetryReady { item_id } => {
                    coord.requeue(item_id);
                    self.assign_ready(coord, cost, cancel).await;
                }
                IoEvent::Settled { item_id, artifact } => {
                    if self.settle(coord, item_id, artifact, &mut on_event) {
                        self.assign_ready(coord, cost, cancel).await;
                    }
                }
                IoEvent::Connected {
                    worker_id,
                    pid,
                    stream,
                } => {
                    // A connection that fails its handshake starts nobody new here: a worker
                    // that cannot start would be started over and over.
                    if self.on_connected(worker_id, pid, stream) {
                        self.assign_ready(coord, cost, cancel).await;
                    }
                }
            }
        }

        Ok(())
    }
}
