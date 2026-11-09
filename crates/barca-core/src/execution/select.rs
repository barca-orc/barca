//! Selecting a phase's work: seeding fresh inputs and targets from recorded proofs, expanding
//! partition keys and deciding each key, narrowing blocked keys and recovering the inputs a
//! phase needs.

use super::*;

pub(super) async fn select_work<'p>(
    session: &mut RunSession<'_, '_>,
    work: Work<'p>,
    queue: &mut VecDeque<Work<'p>>,
    requested: &std::collections::HashSet<String>,
    phase_idx: usize,
) -> Result<Option<(Phase, bool)>, BarcaError> {
    let target_ids: Vec<&str> = session
        .prepared
        .targets
        .iter()
        .map(|(_, id)| id.as_str())
        .collect();
    // The steps to dispatch next, and whether they are cached steps being computed again.
    let selected = match work {
        Work::Ready(phase) => (phase, false),
        Work::Recompute(mut ids) => {
            // One of them may have been computed again since this was queued.
            ids.retain(|id| session.state.cached_node_ids.contains(id));
            if let Some(id) = ids.iter().find(|id| {
                session
                    .state
                    .decide_state
                    .pinned
                    .get(*id)
                    .is_some_and(|pin| pin.user.is_some())
            }) {
                return Err(BarcaError::Usage(crate::freshness::pin_lost(id)));
            }
            match session.state.cached_steps.phase_for(&ids) {
                Some(phase) => (phase, true),
                None => return Ok(None),
            }
        }
        Work::Returned => {
            seed_fresh_targets(session, requested).await?;
            // The output this command hands back is read by whoever ran it: what it
            // prints is made local, and every part of it must be on disk or in the store.
            let mut returned_refs: Vec<OutputRef> = final_output_of(
                &session.prepared.exec_plan,
                &target_ids,
                session.prepared.keep_going,
                &session.state.results.outputs,
            )
            .into_iter()
            .collect();
            for (_, outcome) in target_outcomes(
                &session.prepared.dag,
                &session.prepared.targets,
                &session.state.results.outputs,
                &session.state.results.failures,
                session.request.partition.as_ref(),
            ) {
                returned_refs.extend(outcome.final_output);
                returned_refs.extend(outcome.partitions.into_iter().flatten().map(|(_, o)| o));
            }
            returned_refs.extend(
                returned_partitions(
                    &session.prepared.dag,
                    &session.prepared.exec_plan,
                    &target_ids,
                    session.prepared.keep_going,
                    &session.state.results.outputs,
                    session.request.partition.as_ref(),
                )
                .into_iter()
                .flatten()
                .map(|(_, o)| o),
            );
            let mut returned: Vec<String> = returned_refs.into_iter().map(|o| o.path).collect();
            // A partitioned target's keys arrive through more than one of the lists above:
            // each file is looked at once (200k keys: 600k lookups otherwise).
            returned.sort_unstable();
            returned.dedup();
            let mut check: Vec<String> = session
                .state
                .cached_node_ids
                .iter()
                .filter(|id| requested.contains(recover::base_of(id)))
                .filter_map(|id| session.state.results.outputs.get(id))
                .map(|o| o.path.clone())
                .chain(returned.iter().cloned())
                .collect();
            check.sort_unstable();
            check.dedup();
            let lost = recover::lost(
                &mut session.store,
                &check,
                &returned,
                session.pb.as_ref(),
                &session.state.results.outputs,
                &session.state.cached_node_ids,
                &mut session.state.cached_steps,
            )
            .await;
            match lost {
                Ok(lost) if lost.is_empty() => {}
                Ok(lost) => {
                    queue.push_front(Work::Returned);
                    queue.push_front(Work::Recompute(
                        session.state.cached_steps.first_layer(&lost),
                    ));
                }
                Err(e) => {
                    session.state.transfer_error = Some(e);
                    return Ok(None);
                }
            }
            return Ok(None);
        }
        Work::Planned(phase) => {
            return expand_and_decide(session, phase, phase_idx, queue).await;
        }
        Work::Republish(ids) => {
            republish::republish_sinks(session, ids, queue).await?;
            return Ok(None);
        }
    };

    seed_fresh_inputs(session, &selected.0).await?;
    Ok(Some(selected))
}

pub(super) async fn seed_fresh_inputs(
    session: &mut RunSession<'_, '_>,
    phase: &Phase,
) -> Result<(), BarcaError> {
    let Some(incremental) = &mut session.state.incremental else {
        return Ok(());
    };
    let dag = &session.prepared.dag;
    let mut needed = Vec::new();
    for step in phase.streams.iter().flat_map(|s| &s.steps) {
        let keys = match incremental.filters.get(step.step_id.base_id()) {
            Some((dim, keys)) if step.partition_keys.is_empty() => keys
                .iter()
                .map(|v| {
                    crate::PartitionKey(std::collections::BTreeMap::from([(
                        dim.clone(),
                        v.clone(),
                    )]))
                })
                .collect(),
            _ => step.partition_keys.clone(),
        };
        for up in step
            .inputs
            .values()
            .map(String::as_str)
            .chain(dag.partition_sources(step.step_id.base_id()))
        {
            if dag
                .get_node(up)
                .is_some_and(|n| n.extracted.partitions.is_empty())
            {
                needed.push((up.to_owned(), None));
            } else if dag
                .get_node(step.step_id.base_id())
                .is_some_and(|n| n.resolved_collected.values().any(|id| id == up))
            {
                if let Some(all) = incremental.target_keys(dag, up) {
                    needed.extend(all.into_iter().map(|key| (up.to_owned(), Some(key))));
                }
            } else {
                needed.extend(keys.iter().cloned().map(|key| (up.to_owned(), Some(key))));
            }
        }
    }
    incremental
        .seed(
            dag,
            session.request.cfg,
            needed,
            &mut session.state.decide_state,
            &mut session.state.results.outputs,
            &mut session.state.cached_node_ids,
            &mut session.state.cached_steps,
            &mut session.store,
        )
        .await
}

pub(super) async fn seed_fresh_targets(
    session: &mut RunSession<'_, '_>,
    targets: &std::collections::HashSet<String>,
) -> Result<(), BarcaError> {
    let Some(incremental) = &mut session.state.incremental else {
        return Ok(());
    };
    let dag = &session.prepared.dag;
    let mut needed = Vec::new();
    for target in targets {
        if dag
            .get_node(target)
            .is_some_and(|n| n.extracted.partitions.is_empty())
        {
            needed.push((target.clone(), None));
        } else if incremental.count_for(target) <= crate::results::LISTED_KEYS
            && let Some(keys) = incremental.target_keys(dag, target)
        {
            needed.extend(keys.into_iter().map(|key| (target.clone(), Some(key))));
        }
    }
    incremental
        .seed(
            dag,
            session.request.cfg,
            needed,
            &mut session.state.decide_state,
            &mut session.state.results.outputs,
            &mut session.state.cached_node_ids,
            &mut session.state.cached_steps,
            &mut session.store,
        )
        .await
}

pub(super) async fn expand_and_decide<'p>(
    session: &mut RunSession<'_, '_>,
    phase: &'p Phase,
    phase_idx: usize,
    queue: &mut VecDeque<Work<'p>>,
) -> Result<Option<(Phase, bool)>, BarcaError> {
    let planned_phase = phase;
    // Multi-target run after a failure: drop the steps that depend on a failed
    // step before they are decided; the rest of the phase still runs.
    let unblocked_phase;
    let phase = if !session.state.failed_bases.is_empty() {
        let mut p = phase.clone();
        let narrowed = narrow_blocked_keys(session, &mut p);
        recover::drop_blocked(
            &mut p,
            |base| {
                (!narrowed.contains(base))
                    .then(|| {
                        blocking_failure(&session.prepared.dag, base, &session.state.failed_bases)
                    })
                    .flatten()
            },
            |st, up| {
                let base = st.step_id.base_id();
                session.state.results.reports.push(StepReport {
                    id: base.to_string(),
                    kind: kind_str(session.prepared.dag.get_node(base).map(|n| n.kind())),
                    status: Some(StepStatus::Skipped.to_string()),
                    reason: Some("upstream_failed".to_string()),
                    detail: Some(format!("depends on '{}', which failed", short_name(up))),
                    ..Default::default()
                });
                session.state.skipped_bases.insert(base.to_string());
            },
        );
        unblocked_phase = p;
        &unblocked_phase
    } else {
        phase
    };

    let original_phase = phase;
    let incremental_phase;
    let omitted;
    let incremental_reports;
    let phase = if let Some(incremental) = &mut session.state.incremental {
        let (p, reports, cached) = incremental.prune(&session.prepared.dag, phase);
        incremental_reports = reports;
        incremental_phase = p;
        omitted = cached;
        &incremental_phase
    } else {
        omitted = 0;
        incremental_reports = Vec::new();
        phase
    };
    seed_fresh_inputs(session, phase).await?;
    // Membership artifacts remain necessary even when every member is fresh.
    // Seed only these sources, without loading a fresh fan-in's collected inputs.
    let mut membership_phase = original_phase.clone();
    for step in membership_phase
        .streams
        .iter_mut()
        .flat_map(|s| &mut s.steps)
    {
        step.inputs.clear();
    }
    seed_fresh_inputs(session, &membership_phase).await?;
    // `partitions_from` sources are read from disk during expansion.
    let sources: Vec<String> =
        dispatch::partition_sources(original_phase, &session.state.results.outputs)
            .into_iter()
            .map(|o| o.path.clone())
            .collect();
    let lost = recover::lost(
        &mut session.store,
        &sources,
        &sources,
        session.pb.as_ref(),
        &session.state.results.outputs,
        &session.state.cached_node_ids,
        &mut session.state.cached_steps,
    )
    .await;
    match lost {
        Ok(lost) if lost.is_empty() => {}
        Ok(lost) => {
            queue.push_front(Work::Planned(planned_phase));
            queue.push_front(Work::Recompute(
                session.state.cached_steps.first_layer(&lost),
            ));
            return Ok(None);
        }
        Err(e) => {
            session.state.transfer_error = Some(e);
            return Ok(None);
        }
    }

    if session.request.agent_mode {
        session.state.held_cached_lines.extend(
            incremental_reports
                .iter()
                .filter(|r| r.partitions.is_none())
                .map(|r| r.id.clone()),
        );
    }
    session.state.results.reports.extend(incremental_reports);
    let expansion = dispatch::expand_pending_partitions_filtered(
        &session.prepared.dag,
        phase,
        &session.state.results.outputs,
        session.prepared.pool_size,
        |base, dim, source| {
            session
                .state
                .incremental
                .as_ref()
                .and_then(|i| i.expanded_source(base, source))
                .or_else(|| {
                    dispatch::selected_source(
                        &session.state.results.outputs,
                        session.request.partition.as_ref(),
                        dim,
                        source,
                    )
                })
        },
        |base, dims| {
            if let Some(incremental) = &session.state.incremental {
                incremental.filter(base, dims);
            }
            if let Some(selection) = &session.request.partition {
                selection.prefilter(dims);
            }
            blocked::filter_dims(&session.state.blocked_keys, base, dims);
        },
    );
    // A `partitions_from` source that cannot be read for keys fails the steps reading it.
    for (base, message) in expansion.failures {
        fail_unexpanded(session, base, message);
    }
    let mut expanded_phase = expansion.phase;
    if let (Some(narrowing), Some(expanded)) =
        (session.state.narrowing.as_mut(), expanded_phase.as_mut())
    {
        narrowing.narrow_projected(expanded, &expansion.key_counts);
    }
    if !session.prepared.user_pins.is_empty() {
        let expanded = expanded_phase.get_or_insert_with(|| phase.clone());
        let targets: Vec<&str> = session
            .prepared
            .targets
            .iter()
            .map(|(_, id)| id.as_str())
            .collect();
        crate::pins::prune_expanded_inputs(
            &session.prepared.dag,
            expanded,
            &targets,
            &session.prepared.user_pins,
        );
    }
    let phase_ref = expanded_phase.as_ref().unwrap_or(phase);
    // The whole key set of each asset with a dataset sink, before the cache drops the keys
    // that do not run.
    if let Some(incremental) = &session.state.incremental {
        for base in incremental.cached.keys() {
            if let Some(node) = session.prepared.dag.get_node(base)
                && node.extracted.sinks.iter().any(|s| s.is_dataset())
                && incremental.membership_changed(&session.prepared.dag, base)
                && let Some(keys) = incremental.target_keys(&session.prepared.dag, base)
            {
                crate::dataset_sinks::collect_membership(
                    &mut session.state.dataset_keys,
                    base,
                    &node.extracted.sinks,
                    keys,
                );
            }
        }
    }
    crate::dataset_sinks::collect(&mut session.state.dataset_keys, phase_ref, |base| {
        session
            .state
            .incremental
            .as_ref()
            .is_some_and(|i| i.cached.contains_key(base))
            || session
                .state
                .narrowing
                .as_ref()
                .is_some_and(|n| n.nodes.contains_key(base))
    });

    // Dynamic partitions (`partitions_from`) are a single placeholder step
    // in the plan-time count but expand to their real per-key count here —
    // reconcile `total_steps` so the ETA math below can't underflow and
    // the printed summary reflects what actually ran.
    if expanded_phase.is_some() || omitted > 0 {
        let expanded_count = phase_step_count(phase_ref) + omitted;
        let planned_count = phase_step_count(original_phase);
        if expanded_count > planned_count {
            session.state.total_steps += expanded_count - planned_count;
            if let Some(bar) = session.pb.as_ref() {
                bar.set_length(session.state.total_steps as u64);
            }
        }
    }

    if phase_ref.streams.is_empty() {
        trace_point!(session.started.trace, "phase{phase_idx}_cache_check_done");
        return Ok(None);
    }
    seed_fresh_inputs(session, phase_ref).await?;
    let decided = decide_phase(DecidePhase {
        dag: &session.prepared.dag,
        policy: &session.request.policy,
        cfg: session.request.cfg,
        phase_ref,
        decide_state: &mut session.state.decide_state,
        cost_model: &mut session.cost_model,
        store: &mut session.store,
        step_reports: &mut session.state.results.reports,
        all_outputs: &mut session.state.results.outputs,
        cached_node_ids: &mut session.state.cached_node_ids,
        cached_steps: &mut session.state.cached_steps,
        held_cached_lines: &mut session.state.held_cached_lines,
        pb: &session.pb,
        agent_mode: session.request.agent_mode,
    })
    .await?;
    trace_point!(session.started.trace, "phase{phase_idx}_cache_check_done");
    if let Some(pin) = &session.state.pin {
        pin.run_hashes(session.state.decide_state.run_hashes().values());
    }
    // Cache hits removed from `decided` still contribute validated references,
    // including when another step in this phase must execute.
    if let Some(incremental) = &mut session.state.incremental {
        incremental.observe(
            &session.prepared.dag,
            phase,
            &session.state.results.outputs,
            session.state.decide_state.run_hashes(),
            &session.state.decide_state.sensor_outputs,
            &session.state.results.observations,
        );
    }
    // The cache hits of steps with sinks: each sink that holds another run hash's output is
    // republished from the cached artifact, after this phase's steps have run (#662).
    let republish: Vec<String> = phase_ref
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .filter(|st| !st.sinks.is_empty())
        .flat_map(recover::output_ids)
        .filter(|id| session.state.cached_node_ids.contains(id))
        .collect();
    if !republish.is_empty() {
        queue.push_front(Work::Republish(republish));
    }
    if decided.streams.is_empty() {
        return Ok(None);
    }
    Ok(Some((decided, false)))
}

/// Record `base` as a failed step that never ran: its `partitions_from` source could not be read
/// for keys (`dispatch::expand_pending_partitions`).
pub(super) fn fail_unexpanded(session: &mut RunSession<'_, '_>, base: String, message: String) {
    fail_step(session, base, message, "PartitionSourceError");
}

/// Record `base` as a failed step that never ran, with `error_type`.
pub(super) fn fail_step(
    session: &mut RunSession<'_, '_>,
    base: String,
    message: String,
    error_type: &str,
) {
    if session.request.agent_mode {
        crate::run_line!(Agent, "{}", failed_step_line(&base, &message));
    }
    session.state.failed_bases.insert(base.clone());
    session.state.whole_failure = true;
    if session.state.step_failure.is_none() {
        session.state.step_failure = Some((base.clone(), message.clone()));
    }
    session
        .state
        .results
        .record_failure(base, error_type, message, 1, None, None);
}

/// What this run knows about its failures so far, for `blocked`.
pub(super) fn failures<'s>(session: &'s RunSession<'_, '_>) -> blocked::Failures<'s> {
    blocked::Failures {
        dag: &session.prepared.dag,
        failed_bases: &session.state.failed_bases,
        failed_keys: &session.state.failed_keys,
        blocked_keys: &session.state.blocked_keys,
        skipped: &session.state.skipped_bases,
    }
}

/// Drop from `phase` the keys that a failed key blocks (#764): each is reported `skipped` with
/// reason `upstream_key_failed`, and the rest of its node runs. Returns the bases narrowed this
/// way; the other bases below a failure are blocked whole (`recover::drop_blocked`).
pub(super) fn narrow_blocked_keys(
    session: &mut RunSession<'_, '_>,
    phase: &mut Phase,
) -> std::collections::HashSet<String> {
    let prepared = session.prepared;
    let in_phase: std::collections::HashSet<&str> = phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .map(|st| st.step_id.base_id())
        .collect();
    // In dependency order: a key blocked in one step blocks the same key of the step below it.
    let order: Vec<String> = prepared
        .dag
        .topo_order()
        .into_iter()
        .filter(|id| in_phase.contains(id))
        .map(str::to_string)
        .collect();
    let mut narrowed = std::collections::HashSet::new();
    let mut dropped = std::collections::HashSet::new();
    for base in order {
        if blocking_failure(&prepared.dag, &base, &session.state.failed_bases).is_none() {
            continue;
        }
        match blocked::how(&failures(session), &base, &dropped) {
            blocked::Blocked::Whole => {
                dropped.insert(base);
            }
            blocked::Blocked::Keys(keys) => {
                let set: std::collections::HashSet<String> =
                    keys.iter().map(|k| k.key.clone()).collect();
                blocked::drop_keys(phase, &base, &set);
                // A phase is prepared more than once (expanded, then made ready): each key is
                // reported the first time.
                let recorded = session.state.blocked_keys.entry(base.clone()).or_default();
                for k in keys.iter().filter(|k| !recorded.contains(&k.key)) {
                    let report = blocked::report(&prepared.dag, &base, k);
                    session.state.results.reports.push(report);
                }
                session
                    .state
                    .blocked_keys
                    .entry(base.clone())
                    .or_default()
                    .extend(set);
                narrowed.insert(base);
            }
        }
    }
    narrowed
}

pub(super) async fn recover_phase_inputs<'p>(
    session: &mut RunSession<'_, '_>,
    mut filtered_phase: Phase,
    recompute: bool,
    queue: &mut VecDeque<Work<'p>>,
    phase_idx: usize,
) -> Result<Option<(Phase, HashMap<String, dispatch::ProvidedInput>)>, BarcaError> {
    // A phase that waited for a recompute may since have lost an upstream to a failure
    // (several targets keep going around one): its blocked steps do not run.
    if !session.state.failed_bases.is_empty() {
        let narrowed = narrow_blocked_keys(session, &mut filtered_phase);
        recover::drop_blocked(
            &mut filtered_phase,
            |base| {
                (!narrowed.contains(base))
                    .then(|| {
                        blocking_failure(&session.prepared.dag, base, &session.state.failed_bases)
                    })
                    .flatten()
            },
            |st, _| {
                session
                    .state
                    .skipped_bases
                    .insert(st.step_id.base_id().to_string());
                if recompute {
                    // Its artifact is still missing and it cannot be computed: it is no
                    // longer an output of this run.
                    for id in recover::output_ids(st) {
                        recover::mark_recomputed(
                            &mut session.state.results.reports,
                            &id,
                            false,
                            false,
                        );
                        session.state.cached_node_ids.remove(&id);
                        session.state.results.outputs.remove(&id);
                    }
                }
            },
        );
        if filtered_phase.streams.is_empty() {
            return Ok(None);
        }
    }

    // Make this phase's inputs available: exactly the artifacts its steps were provided.
    // Cache hits recorded by other machines are fetched (less the parquet inputs every
    // reader in the phase scans lazily, which are read in place). An input that is
    // neither on disk nor in the store has its step computed again first.
    let mut provided =
        dispatch::build_provided_inputs(&filtered_phase, &session.state.results.outputs);
    let check = recover::input_paths(&provided);
    if let Some(s) = session.store.as_ref() {
        s.read_in_place(
            &mut provided,
            &dispatch::lazily_read_inputs(&filtered_phase),
        );
    }
    let fetch = recover::input_paths(&provided);
    let lost = recover::lost(
        &mut session.store,
        &check,
        &fetch,
        session.pb.as_ref(),
        &session.state.results.outputs,
        &session.state.cached_node_ids,
        &mut session.state.cached_steps,
    )
    .await;
    trace_point!(session.started.trace, "phase{phase_idx}_inputs_local");
    let lost = match lost {
        Ok(lost) => lost,
        Err(e) => {
            if !recompute {
                // Decided to run, but never dispatched: say so (see below the loop).
                queue.push_front(Work::Ready(filtered_phase));
            }
            session.state.transfer_error = Some(e);
            return Ok(None);
        }
    };
    if !lost.is_empty() {
        queue.push_front(if recompute {
            let steps = filtered_phase.streams.iter().flat_map(|s| &s.steps);
            Work::Recompute(steps.flat_map(recover::output_ids).collect())
        } else {
            Work::Ready(filtered_phase)
        });
        queue.push_front(Work::Recompute(
            session.state.cached_steps.first_layer(&lost),
        ));
        return Ok(None);
    }

    if recompute {
        // These are no longer served from cache: they run, and are recorded, like any
        // other step of this run. A result whose file the cache size limit deleted says so.
        let ids: Vec<String> = filtered_phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .flat_map(recover::output_ids)
            .collect();
        let paths: Vec<&str> = ids
            .iter()
            .filter_map(|id| {
                session
                    .state
                    .results
                    .outputs
                    .get(id)
                    .map(|o| o.path.as_str())
            })
            .collect();
        let evicted = db::evicted_among(&session.request.cfg.db_path, &paths).await;
        let mut lost: Vec<(String, String)> = Vec::new();
        for id in ids {
            let was_evicted = session
                .state
                .results
                .outputs
                .get(&id)
                .is_some_and(|o| evicted.contains(&o.path));
            recover::mark_recomputed(&mut session.state.results.reports, &id, false, was_evicted);
            session.state.cached_node_ids.remove(&id);
            if let Some(oref) = session.state.results.outputs.remove(&id) {
                lost.push((id, oref.path));
            }
        }
        for line in recover::recompute_warnings(&lost, &evicted) {
            note(&session.pb, &line);
        }
    }
    session.state.read_paths.extend(check);

    Ok(Some((filtered_phase, provided)))
}
