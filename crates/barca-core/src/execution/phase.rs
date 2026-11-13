//! One phase of a run: deciding which of its steps run (cache, refresh, blocked keys) and
//! dispatching them to the worker pool.

use super::*;

pub(super) struct DecidePhase<'a> {
    pub(super) dag: &'a Dag,
    pub(super) policy: &'a CachePolicy,
    pub(super) cfg: &'a crate::config::ResolvedConfig,
    pub(super) phase_ref: &'a Phase,
    pub(super) decide_state: &'a mut DecideState,
    pub(super) cost_model: &'a mut crate::cost::CostModel,
    pub(super) store: &'a mut Option<StoreSync>,
    pub(super) step_reports: &'a mut Vec<StepReport>,
    pub(super) all_outputs: &'a mut HashMap<String, OutputRef>,
    pub(super) cached_node_ids: &'a mut std::collections::HashSet<String>,
    pub(super) cached_steps: &'a mut recover::CachedSteps,
    pub(super) held_cached_lines: &'a mut Vec<String>,
    pub(super) pb: &'a Option<indicatif::ProgressBar>,
    pub(super) agent_mode: bool,
}

pub(super) async fn decide_phase(ctx: DecidePhase<'_>) -> Result<Phase, BarcaError> {
    let DecidePhase {
        dag,
        policy,
        cfg,
        phase_ref,
        decide_state,
        cost_model,
        store,
        step_reports,
        all_outputs,
        cached_node_ids,
        cached_steps,
        held_cached_lines,
        pb,
        agent_mode,
    } = ctx;
    decide_state.register_phase(phase_ref);
    let mut uncached_streams: Vec<crate::planner::WorkerStream> = Vec::new();

    // With a server, ask its ledger about this phase's run hashes the first lookup could not
    // know (below a sensor, or dynamic partitions), before the database is opened.
    if let Some(ledger) = &cfg.ledger {
        ledger
            .prefetch(crate::cache::phase_keys(dag, phase_ref, decide_state))
            .await?;
    }
    // Open the DB only for this phase's cache lookups and release it before any
    // step runs, so other barca processes can use the metadata DB while Python
    // executes.
    let cache = crate::cache::CacheReader::for_config(cfg).await?;

    // Decide all upstream chunks before any consumer, independently of the worker split.
    let mut uncached: Vec<Vec<crate::planner::StreamStep>> =
        vec![Vec::new(); phase_ref.streams.len()];
    let mut due_costs = Vec::new();
    for (stream_idx, step) in in_dependency_order(dag, phase_ref) {
        let uncached_steps = &mut uncached[stream_idx];
        if step.kind == crate::NodeKind::Sensor {
            let id = step.step_id.display();
            if let Some((revision, output)) = crate::observation::previous(&cache, &id).await? {
                let output = crate::cache::accept_cache_hit(store, &id, output);
                decide_state.sensor_outputs.insert(id.clone(), revision);
                decide_state.sensor_previous.insert(id, output);
            }
        }
        let (step, decision) = decide_step(dag, policy, Some(&cache), decide_state, step).await?;
        let decision = localize_decision(decision, &step, store);
        step_reports.push(report_for(dag, &step, &decision, false));
        let display_id = step.step_id.display();
        match decision {
            Decision::Run(_) => {
                // What the store already holds for a sensor at this run hash: a poll whose data
                // did not change produces the same bytes, which are not uploaded again (#554).
                // Only a sensor: it runs every time and is never served from the store, so a
                // store object changed behind this machine's back is never read as its result.
                // An asset is uploaded whenever it runs (a refresh repairs the store).
                if let Some(s) = store.as_mut()
                    && step.partition_keys.is_empty()
                    && dag
                        .get_node(step.step_id.base_id())
                        .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
                    && let Some(run_hash) = decide_state.run_hashes().get(&display_id)
                    && let Some(known) = db::confirmed_object(&cache, &display_id, run_hash).await
                {
                    s.confirmed.insert(display_id.clone(), known);
                }
                uncached_steps.push(step)
            }
            Decision::Cached {
                oref,
                stale_root,
                held,
            } => {
                if let Some(root) = stale_root {
                    note(
                        pb,
                        &format!(
                            "[barca] warning: {}",
                            stale_warning(&display_id, &root, false)
                        ),
                    );
                }
                if agent_mode {
                    // Announce a step once, with its true outcome: when its
                    // artifact is known to be gone it may yet be computed
                    // again, so its line waits until that is settled.
                    if StoreSync::known_absent(store.as_ref(), &oref.path) {
                        held_cached_lines.push(display_id.clone());
                    } else {
                        crate::run_line!(Agent, "{}", cached_step_line(dag, &display_id));
                        crate::lines::Lines::current().cached_step(&display_id);
                    }
                }
                all_outputs.insert(display_id.clone(), oref);
                cached_node_ids.insert(display_id);
                // A `Manual` node read as it is cannot be computed again in this run: its inputs
                // are not planned.
                if !matches!(
                    held,
                    Some(crate::cache::Held::Manual { .. } | crate::cache::Held::Pinned)
                ) {
                    cached_steps.remember(step);
                }
            }
            Decision::Partitioned {
                cached, missing, ..
            } => {
                let any_cached = !cached.is_empty();
                for (pdisplay, oref) in cached {
                    all_outputs.insert(pdisplay.clone(), oref);
                    cached_node_ids.insert(pdisplay);
                }
                if !missing.is_empty() {
                    // Its run hashes, for the keys that run only; not a copy of every key.
                    due_costs.extend(
                        missing
                            .iter()
                            .map(|key| key.display_id(step.step_id.base_id())),
                    );
                    let mut partial = step.with_keys(missing);
                    partial.run_hashes = partial
                        .partition_keys
                        .iter()
                        .filter_map(|k| {
                            let id = k.display_id(&step.step_id.base);
                            step.run_hashes.get(&id).map(|h| (id, h.clone()))
                        })
                        .collect();
                    uncached_steps.push(partial);
                }
                if any_cached {
                    cached_steps.remember(step);
                }
            }
        }
    }
    if !due_costs.is_empty() {
        cost_model.seed_exact(db::cost_estimates_for(cache.conn(), &due_costs).await?);
    }
    for (stream, uncached_steps) in phase_ref.streams.iter().zip(uncached) {
        if !uncached_steps.is_empty() {
            uncached_streams.push(crate::planner::WorkerStream {
                stream_id: stream.stream_id.clone(),
                steps: uncached_steps,
            });
        }
    }

    drop(cache);

    Ok(Phase {
        reason: phase_ref.reason.clone(),
        streams: uncached_streams,
    })
}

/// Print a note above the progress bar, or to stderr when no bar is visible. A hidden bar
/// (stderr is not a terminal, as when an agent or CI drives barca) silently swallows
/// `ProgressBar::println`, which used to make warnings vanish.
pub(crate) fn note(pb: &Option<indicatif::ProgressBar>, msg: &str) {
    crate::lines::Lines::current().note(pb.as_ref(), msg);
}

pub(super) struct DispatchPhase<'a> {
    pub(super) filtered_phase: &'a Phase,
    /// The batched calls of this phase (RFC-0013), as [`plan_calls`] made them.
    pub(super) calls: Vec<(String, Vec<String>)>,
    pub(super) provided: &'a HashMap<String, dispatch::ProvidedInput>,
    pub(super) pool: &'a mut crate::io_loop::WorkerPool,
    pub(super) cost_model: &'a mut crate::cost::CostModel,
    pub(super) decide_state: &'a DecideState,
    pub(super) store: &'a mut Option<StoreSync>,
    pub(super) recorder: &'a StepRecorder,
    pub(super) run_id: &'a str,
    pub(super) store_paths: &'a mut HashMap<String, String>,
    pub(super) dag: &'a Dag,
    pub(super) pb: &'a Option<indicatif::ProgressBar>,
    pub(super) agent_mode: bool,
    pub(super) total_estimated: f64,
    pub(super) elapsed_so_far: f64,
    pub(super) completed_steps: usize,
    pub(super) total_steps: usize,
    pub(super) event_tx: &'a Option<UnboundedSender<crate::RunEvent>>,
    pub(super) logs_buffer: &'a mut crate::run_logs::Recorder,
    pub(super) step_durations: &'a mut HashMap<String, (f64, f64)>,
    pub(super) cancel: &'a CancellationToken,
}

pub(super) async fn dispatch_phase(
    ctx: DispatchPhase<'_>,
) -> (
    crate::coordinator::Coordinator,
    Result<(), String>,
    f64,
    usize,
    usize,
) {
    let DispatchPhase {
        filtered_phase,
        provided,
        calls,
        pool,
        cost_model,
        decide_state,
        store,
        recorder,
        run_id,
        store_paths,
        dag,
        pb,
        agent_mode,
        total_estimated,
        mut elapsed_so_far,
        mut completed_steps,
        mut total_steps,
        event_tx,
        logs_buffer,
        step_durations,
        cancel,
    } = ctx;
    let mut coord = crate::coordinator::Coordinator::new();
    coord.assign_calls(calls);
    let loaded = coord.load_phase(filtered_phase, provided);
    coord.sensor_previous(&decide_state.sensor_previous);
    let expected: usize = filtered_phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|st| {
            if st.partition_keys.is_empty() {
                1
            } else {
                st.partition_keys.len()
            }
        })
        .sum();
    assert_eq!(
        loaded, expected,
        "plan/coordinator step count mismatch: loaded {loaded}, expected {expected}"
    );

    // Progress callback — update bar as each step completes.
    let run_hashes = decide_state.run_hashes();
    let datadog = pool.datadog();
    let on_step_cb: crate::io_loop::StepCallback<'_> = Box::new(
        |node_id: &str, artifact: &serde_json::Value, attempts: u32| {
            let handling = std::time::Instant::now();
            // The step's trace goes out now, not at the end of the run. parallel() children
            // (no run hash) have no trace of their own.
            if let Some(datadog) = &datadog
                && let Some(run_hash) = run_hashes.get(node_id)
            {
                datadog.step_finished(node_id, artifact, attempts, run_hash);
            }
            // Hand the finished step to the recorder. The worker reports a step only after
            // its artifact is in place (an atomic rename), so the row never points at a
            // missing file. Steps without a run hash are parallel() children, which are
            // never recorded. Remote rows are queued below only on confirmed upload.
            if store.is_none()
                && let Some(run_hash) = run_hashes.get(node_id)
            {
                recorder.record(
                    NewMaterialization::from_artifact(node_id, run_hash, artifact, attempts)
                        .with_observation(crate::observation::from_artifact(
                            artifact,
                            decide_state.sensor_outputs.get(node_id),
                            run_id,
                            node_id,
                        ))
                        .with_components(crate::changes::step_components_json(
                            dag,
                            decide_state,
                            node_id,
                        )),
                );
            }
            // A sink that failed failed the step (the worker reports it as the step's error).
            // A dataset sink's size warning: a partition column with too many values.
            if let Some(sinks) = artifact.get("sinks").and_then(|v| v.as_array()) {
                for warning in sinks
                    .iter()
                    .filter_map(|s| s.get("warning").and_then(|v| v.as_str()))
                {
                    note(pb, &format!("[barca] warning: {warning}"));
                }
            }
            // Upload plan-step artifacts in the background while the run
            // continues. parallel() children (no run hash) are never
            // recorded, so they stay local.
            if let Some(s) = store.as_mut()
                && let Some(run_hash) = decide_state.run_hashes().get(node_id)
                && let Some(path) = artifact.get("path").and_then(|v| v.as_str())
                && let Some(at) = s.layout.store_for(path)
            {
                // False reuses an old reference; it still needs fetching if its mirror is
                // gone. Every materialized result supersedes any pending cache fetch.
                if artifact.get("sensor_changed").and_then(|v| v.as_bool()) != Some(false) {
                    s.produced(path);
                }
                let record = recorder.after_upload(
                    NewMaterialization::from_artifact(node_id, run_hash, artifact, attempts)
                        .with_observation(crate::observation::from_artifact(
                            artifact,
                            decide_state.sensor_outputs.get(node_id),
                            run_id,
                            node_id,
                        ))
                        .with_components(crate::changes::step_components_json(
                            dag,
                            decide_state,
                            node_id,
                        )),
                    at.clone(),
                );
                let snapshot = artifact
                    .get("upload_snapshot")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let source = snapshot.as_deref().unwrap_or(path);
                let cleanup = snapshot.clone();
                let digest = artifact.get("content_hash").and_then(|v| v.as_str());
                if let Some(digest) = digest
                    && s.holds_already(node_id, &at, digest)
                {
                    // A sensor poll that returned what the store already holds at this location,
                    // as this machine recorded on a confirmed upload: record the step now, upload
                    // nothing (#554).
                    record(Some(digest.to_string()));
                    if let Some(path) = &snapshot {
                        std::fs::remove_file(path).ok();
                    }
                    // Identical bytes still carry a new observation when explicitly changed.
                    if artifact.get("sensor_changed").and_then(|v| v.as_bool()) != Some(true) {
                        s.unchanged_observations += 1;
                    }
                    store_paths.insert(node_id.to_string(), at);
                } else if let Some(at) = s.client.upload_confirmed(
                    node_id,
                    path,
                    source,
                    Box::new(move |receipt| {
                        record(receipt.sha256.clone());
                        if let Some(path) = cleanup {
                            std::fs::remove_file(path).ok();
                        }
                    }),
                ) {
                    store_paths.insert(node_id.to_string(), at);
                }
            }
            let elapsed_s = artifact.get("elapsed_seconds").and_then(|v| v.as_f64());
            if let Some(e) = elapsed_s {
                elapsed_so_far += e;
            }
            completed_steps += 1;
            // parallel() children complete as extra steps the plan didn't count: grow the
            // total so the counters and the ETA never run past it.
            let grown = reconcile_total(total_steps, completed_steps);
            if grown != total_steps {
                total_steps = grown;
                if let Some(bar) = pb.as_ref() {
                    bar.set_length(total_steps as u64);
                }
            }
            if let Some(bar) = pb.as_ref() {
                bar.set_position(completed_steps as u64);
                let remaining = if total_estimated > 0.0 {
                    (total_estimated - elapsed_so_far).max(0.0)
                } else if completed_steps > 0 {
                    let avg = elapsed_so_far / completed_steps as f64;
                    avg * total_steps.saturating_sub(completed_steps) as f64
                } else {
                    0.0
                };
                let short_name = node_id.rsplit(':').next().unwrap_or(node_id);
                if remaining > 0.5 {
                    bar.set_prefix(format!("{}left", fmt_eta(remaining)));
                } else {
                    bar.set_prefix("   done ");
                }
                bar.set_message(format!("{short_name} done"));
            } else if agent_mode {
                crate::run_line!(
                    Agent,
                    "[barca] step:{} completed {:.1}s ({}/{}){}{}",
                    node_id,
                    elapsed_s.unwrap_or(0.0),
                    completed_steps,
                    total_steps,
                    env_suffix(dag, node_id),
                    if attempts > 1 {
                        format!(" attempts={attempts}")
                    } else {
                        String::new()
                    }
                );
            }
            recorder.note_busy(handling);
        },
    );

    // Event sink — buffer log lines for DB persistence, and forward every
    // event live to the caller's channel (the HTTP server) if present.
    let event_tx_phase = event_tx.clone();
    let logs_sink = logs_buffer;
    let on_event_cb: crate::io_loop::EventCallback<'_> = Box::new(move |ev: crate::RunEvent| {
        if let crate::RunEvent::Log {
            ref node_id,
            ref line,
            stream,
            ref ts,
        } = ev
        {
            // Under `BARCA_LOG=json`, stdout becomes a JSON line; stderr was already written
            // to the worker's own stderr as it is.
            if stream == crate::LogStream::Stdout {
                crate::jsonlog::step_output(node_id, line);
            }
            logs_sink.push(crate::run_logs::RecordedLine {
                node_id: node_id.clone(),
                line: line.clone(),
                stream,
                ts: ts.clone(),
            });
        }
        if let crate::RunEvent::StepFinished {
            ref node_id,
            elapsed_seconds: Some(elapsed),
            import_seconds,
            ..
        } = ev
        {
            let id = crate::StepId::parse(node_id);
            if !id.partition.0.contains_key("_branch") {
                let duration = step_durations.entry(id.base_id().to_string()).or_default();
                duration.0 += elapsed;
                duration.1 += import_seconds.unwrap_or(0.0);
            }
        }
        if let Some(ref tx) = event_tx_phase {
            let _ = tx.send(ev);
        }
    });

    // Drive this phase against the persistent pool. The cost model both
    // sizes the batch pulls and absorbs the timings coming back.
    let phase_err = pool
        .run_phase(
            &mut coord,
            cost_model,
            Some(on_step_cb),
            Some(on_event_cb),
            cancel,
        )
        .await;
    (
        coord,
        phase_err,
        elapsed_so_far,
        completed_steps,
        total_steps,
    )
}
