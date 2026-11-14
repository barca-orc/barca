//! Dry-run prediction: what `barca get --dry-run` / `barca plan` say each step would do,
//! phase by phase, without running anything.

use super::*;

/// [`crate::queries::explain`] on an already-built DAG (`barca status` reuses its DAG for the node listing).
pub(crate) async fn explain_dag(
    dag: &Dag,
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    policy: CachePolicy,
    command_label: &str,
    partition: Option<&crate::selection::PartitionSelection>,
    file_args: &[String],
) -> Result<ExplainResult, BarcaError> {
    // A dry run predicts `barca get` / `barca run`, which read a stale `Manual` upstream as it is.
    let rules = crate::freshness::RunRules::request();
    Ok(explain_dag_with_membership(
        dag,
        cfg,
        target_names,
        policy,
        command_label,
        partition,
        &rules,
        file_args,
    )
    .await?
    .0)
}

/// Full actual prediction membership, moved without changing public reports.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn explain_dag_with_membership(
    dag: &Dag,
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    policy: CachePolicy,
    command_label: &str,
    partition: Option<&crate::selection::PartitionSelection>,
    rules: &crate::freshness::RunRules,
    file_args: &[String],
) -> Result<(ExplainResult, crate::cache::ExpandedMembership), BarcaError> {
    let (explained, members, _) = explain_dag_with_keys(
        dag,
        cfg,
        target_names,
        policy,
        command_label,
        partition,
        rules,
        file_args,
    )
    .await?;
    Ok((explained, members))
}

/// [`explain_dag_with_membership`], and the keys of each partitioned node the dry run predicts
/// would run: every one, where the report previews a few.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn explain_dag_with_keys(
    dag: &Dag,
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    policy: CachePolicy,
    command_label: &str,
    partition: Option<&crate::selection::PartitionSelection>,
    rules: &crate::freshness::RunRules,
    file_args: &[String],
) -> Result<
    (
        ExplainResult,
        crate::cache::ExpandedMembership,
        crate::cache::ExpandedMembership,
    ),
    BarcaError,
> {
    let t0 = std::time::Instant::now();
    let mut targets = resolve_targets(dag, target_names, command_label)?;
    // The `Manual` nodes a real run would read as they are, and the cone it would plan.
    let Planned { cone, pinned, .. } =
        pin_manual(dag, &mut targets, cfg, &policy, rules, file_args).await?;
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let pool_size = sizing_for(cfg, &crate::run_host::LocalHost).pool_size;
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let mut exec_plan = match &cone {
        Some(cone) => crate::queries::filter_plan_to_subgraph(
            crate::planner::plan_from_dag_narrowed(dag, &config, partition),
            cone,
        ),
        None => plan_for_targets(dag, &target_ids, &config, command_label, partition),
    };
    let mut narrowing = match partition {
        Some(selection) => Some(selection.apply(dag, &mut exec_plan, &target_ids)?),
        None => None,
    };
    if let CachePolicy::RefreshSelective { names, .. } = &policy {
        validate_refresh_names(
            dag,
            &target_ids,
            names,
            command_label == "get",
            cone.as_deref(),
        )?;
        if let Some(n) = &narrowing {
            n.check_refresh(names)?;
        }
    }

    dag.check_imports(exec_plan.step_ids())?;
    let warnings = crate::warnings::for_plan(dag, &exec_plan);
    crate::status::trace_since("dry run: plan", t0);

    // With a server, the dry run is decided against its ledger: check the server (a dry run
    // posts nothing) and ask once for every run hash known before anything runs. The ledger's
    // answers are read through a cache reader, so the database is created if there is none.
    if let Some(ledger) = crate::ledger::attached(cfg) {
        ledger.prepare(cfg, false).await?;
        ledger
            .prefetch(crate::cache::cone_keys(dag, &exec_plan.phases))
            .await?;
        crate::layout::ensure_env_dirs(&cfg.env)?;
        db::init_db(&cfg.db_path).await?;
    }
    // No metadata DB yet means nothing is cached. Do not create one just to look.
    let cache = if std::path::Path::new(&cfg.db_path).exists() {
        Some(crate::cache::CacheReader::for_config(cfg).await?)
    } else {
        None
    };

    let mut state = DecideState::default();
    state.pinned = pinned;
    // A dry run executes nothing, so it cannot know what a sensor will return. It predicts with
    // each sensor's last recorded output (#183); a consumer of a sensor with none is unknown.
    // A `Manual` asset it would refresh is predicted the same way (#548).
    if let Some(cache) = &cache {
        let sensors: Vec<&str> = exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|st| st.step_id.base_id())
            // A cutoff asset is not predicted: what reads it is decided by the bytes it wrote
            // when it is cached, and is unknown when it would run (`cutoff_pending`, #628).
            .filter(|id| crate::cache::folds_output(dag, id) && !crate::cache::is_cutoff(dag, id))
            .collect();
        state.sensor_outputs = db::last_output_hashes(cache, &sensors).await?;
    }
    let mut all_outputs: HashMap<String, OutputRef> = HashMap::new();
    // A sensor a `partitions_from(...)` reads for keys is predicted the same way: its consumers
    // are expanded from its last recorded output, so a dict source tells which keys changed.
    if let Some(cache) = &cache {
        let sources = exec_plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .filter(|st| st.keyed.is_source && st.kind == crate::NodeKind::Sensor);
        for st in sources {
            let id = st.step_id.base_id();
            if let Some(last) = db::latest_success(cache, id).await
                && std::path::Path::new(&last.output.path).exists()
            {
                all_outputs.insert(id.to_string(), last.output);
            }
        }
    }
    // Results the cache size limit evicted: predicted as `evicted` rather than missing.
    let evicted = match &cache {
        Some(cache) => db::evicted_paths(cache).await,
        None => std::collections::HashSet::new(),
    };
    // What a run would compute again because an artifact it needs is missing (#252): the same
    // rule as `execute`, predicted from what is on this disk.
    let layout = cfg
        .remote_artifacts()
        .then(|| ArtifactLayout::new(&cfg.local_artifact_dir, &cfg.artifact_root));
    let mut cached_steps = recover::CachedSteps::default();
    let requested = recover::requested(&exec_plan, &target_ids);
    // Steps reported as unknown, by base id, with the reason code their dependents inherit.
    let mut unknown_ids: HashMap<String, &'static str> = HashMap::new();
    // Steps whose run hash cannot be predicted because a sensor upstream of them has no
    // recorded output (a forced step, e.g. under --refresh, is still reported as `run`).
    let mut hash_unknown: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Steps whose run hash depends on the bytes a cutoff asset that would run writes (#628),
    // by base id, with that asset's short name.
    let mut cutoff_unknown: HashMap<String, String> = HashMap::new();
    let mut steps: Vec<StepReport> = Vec::new();
    let mut summary = ExplainSummary::default();

    let user_pins: Vec<db::AssetPin> = match &cache {
        Some(cache) => db::pins_on(cache.conn()).await?.into_values().collect(),
        None => Vec::new(),
    };
    let mut prediction = Prediction {
        targets: &target_ids,
        user_pins: &user_pins,
        dag,
        policy: &policy,
        cache: cache.as_ref(),
        layout: layout.as_ref(),
        evicted: &evicted,
        pool_size,
        state: &mut state,
        all_outputs: &mut all_outputs,
        cached_steps: &mut cached_steps,
        unknown_ids: &mut unknown_ids,
        hash_unknown: &mut hash_unknown,
        cutoff_unknown: &mut cutoff_unknown,
        steps: &mut steps,
        summary: &mut summary,
        narrowing: narrowing.as_mut(),
    };
    crate::status::trace_since("dry run: sensor outputs and evictions", t0);
    for (i, phase) in exec_plan.phases.iter().enumerate() {
        let tp = std::time::Instant::now();
        predict_phase(&mut prediction, phase).await?;
        crate::status::trace_since(&format!("dry run: phase {}", i + 1), tp);
    }
    drop(cache);
    crate::status::trace_since("dry run: decided", t0);

    // The outputs the command was asked for are read by whoever ran it.
    let returned = all_outputs
        .iter()
        .filter(|(id, _)| requested.contains(recover::base_of(id)))
        .map(|(_, o)| o.path.clone())
        .collect();
    recover::predict_recomputes(
        returned,
        &mut cached_steps,
        &mut all_outputs,
        layout.as_ref(),
        &evicted,
        &mut steps,
        &mut summary,
        &state.pinned,
    )?;

    let mut steps = merge_partition_reports(steps);
    if let Some(n) = &narrowing {
        steps.extend(n.skipped_reports(dag, true));
    }
    // The weights a real run would schedule by, and the capacity they share (#436).
    let weights = planned_weights(&exec_plan);
    for r in &mut steps {
        r.weight = weights.get(recover::base_of(&r.id)).copied();
    }
    let capacity = sizing_for(cfg, &crate::run_host::LocalHost).capacity;
    warn_over_capacity(
        &weights,
        capacity,
        sizing_for(cfg, &crate::run_host::LocalHost).capacity_source,
    );
    // Results of an earlier run still waiting for the store: the real run uploads them first.
    let uploads = if cfg.remote_artifacts() {
        crate::pending_uploads::report(&crate::pending_uploads::list(&cfg.db_path).await?, None)
    } else {
        None
    };
    // Per-target predictions: the same step lines, counted over each target's cone.
    let per_target = if targets.len() > 1 {
        targets
            .iter()
            .map(|(name, id)| {
                let cone: std::collections::HashSet<&str> = dag.subgraph(id).into_iter().collect();
                let mut s = ExplainSummary::default();
                for r in steps.iter().filter(|r| cone.contains(r.id.as_str())) {
                    s.add(r);
                }
                (name.clone(), TargetPrediction { summary: s })
            })
            .collect()
    } else {
        Vec::new()
    };
    let (members, will_run) = state.into_key_sets();
    Ok((
        ExplainResult {
            dry_run: true,
            command: command_label.to_string(),
            target: match target_ids.as_slice() {
                [one] => Some(short_name(one).to_string()),
                _ => None,
            },
            targets: per_target,
            steps,
            summary,
            warnings,
            uploads,
            capacity,
            partition_selection: narrowing.as_ref().map(|n| n.report()),
        },
        members,
        will_run,
    ))
}

pub(super) struct Prediction<'a> {
    pub(super) targets: &'a [&'a str],
    pub(super) user_pins: &'a [db::AssetPin],
    pub(super) dag: &'a Dag,
    pub(super) policy: &'a CachePolicy,
    pub(super) cache: Option<&'a crate::cache::CacheReader>,
    pub(super) layout: Option<&'a ArtifactLayout>,
    pub(super) evicted: &'a std::collections::HashSet<String>,
    pub(super) pool_size: usize,
    pub(super) state: &'a mut DecideState,
    pub(super) all_outputs: &'a mut HashMap<String, OutputRef>,
    pub(super) cached_steps: &'a mut recover::CachedSteps,
    pub(super) unknown_ids: &'a mut HashMap<String, &'static str>,
    pub(super) hash_unknown: &'a mut std::collections::HashSet<String>,
    pub(super) cutoff_unknown: &'a mut HashMap<String, String>,
    pub(super) steps: &'a mut Vec<StepReport>,
    pub(super) summary: &'a mut ExplainSummary,
    pub(super) narrowing: Option<&'a mut crate::selection::Narrowing>,
}

pub(super) fn unknown_report(dag: &Dag, base_id: &str, reason: &str, detail: String) -> StepReport {
    StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        action: Some("unknown".to_string()),
        reason: Some(reason.to_string()),
        detail: Some(detail),
        ..Default::default()
    }
}

pub(super) fn expand_prediction(
    ctx: &mut Prediction<'_>,
    phase: &Phase,
) -> Result<Phase, BarcaError> {
    // A `partitions_from` source is read to expand its consumers: if its artifact is
    // missing it runs again first, and its keys are not known until it has.
    let sources = dispatch::partition_sources(phase, ctx.all_outputs)
        .into_iter()
        .map(|o| o.path.clone())
        .collect();
    recover::predict_recomputes(
        sources,
        ctx.cached_steps,
        ctx.all_outputs,
        ctx.layout,
        ctx.evicted,
        ctx.steps,
        ctx.summary,
        &ctx.state.pinned,
    )?;

    // Dynamic partitions need their source's output to know their keys. If the source is
    // not available (it would have to run first) the step cannot be expanded.
    let mut ready = phase.clone();
    for stream in &mut ready.streams {
        stream.steps.retain(|st| {
            let missing_source = st.pending_partitions.values().find(|src| {
                !ctx.all_outputs
                    .keys()
                    .any(|k| k.ends_with(&format!(":{src}")) || k.as_str() == src.as_str())
            });
            match missing_source {
                Some(src) => {
                    let base = st.step_id.base_id();
                    ctx.steps.push(unknown_report(
                        ctx.dag,
                        base,
                        "partitions_unknown",
                        format!(
                            "partition keys come from the output of '{src}', which is not \
                                 available until it runs"
                        ),
                    ));
                    ctx.unknown_ids
                        .insert(base.to_string(), "partitions_unknown");
                    ctx.summary.unknown += 1;
                    false
                }
                None => true,
            }
        });
    }

    Ok(ready)
}

pub(super) async fn predict_steps(
    ctx: &mut Prediction<'_>,
    phase_ref: &Phase,
) -> Result<Vec<crate::planner::StreamStep>, BarcaError> {
    let mut pruned;
    let phase_ref = if ctx.user_pins.is_empty() {
        phase_ref
    } else {
        pruned = phase_ref.clone();
        crate::pins::prune_expanded_inputs(ctx.dag, &mut pruned, ctx.targets, ctx.user_pins);
        &pruned
    };
    ctx.state.register_phase(phase_ref);
    if let Some(ledger) = ctx.cache.and_then(|c| c.ledger()) {
        ledger
            .prefetch(crate::cache::phase_keys(ctx.dag, phase_ref, ctx.state))
            .await?;
    }
    let mut to_run = Vec::new();
    for (_, step) in in_dependency_order(ctx.dag, phase_ref) {
        let base = step.step_id.base_id();
        let up_base = |up: &str| up.split('[').next().unwrap_or(up).to_string();
        let unknown_dep = step
            .inputs
            .values()
            .find(|up| ctx.unknown_ids.get(&up_base(up)) == Some(&"partitions_unknown"));
        if let Some(up) = unknown_dep {
            ctx.steps.push(unknown_report(
                ctx.dag,
                base,
                "partitions_unknown",
                format!(
                    "depends on '{}', whose partitions are not known until it runs",
                    short_name(up)
                ),
            ));
            ctx.unknown_ids
                .insert(base.to_string(), "partitions_unknown");
            ctx.summary.unknown += 1;
            continue;
        }

        // Sensors this step reads, and whether the dry run knows their output.
        let read_sensors = sensor_inputs(ctx.dag, step);
        let missing_sensor = read_sensors
            .iter()
            .find(|s| !has_sensor_output(ctx.state, s))
            .map(|s| s.to_string());
        let hash_unknown_dep = step
            .inputs
            .values()
            .find(|up| ctx.hash_unknown.contains(&up_base(up)))
            .cloned();
        // The first input that is (or depends on) a cutoff asset that would run, and that
        // asset (short names).
        let cutoff_dep = step.inputs.values().find_map(|up| {
            let up = up_base(up);
            ctx.cutoff_unknown
                .get(&up)
                .map(|root| (short_name(&up).to_string(), root.clone()))
        });

        let (step, decision) = decide_step(ctx.dag, ctx.policy, ctx.cache, ctx.state, step).await?;
        // Forced to run whatever the cache holds (task, sensor, refresh).
        let forced = matches!(
            &decision,
            Decision::Run(reason) if *reason != RunReason::NotMaterialized
        ) || matches!(
            &decision,
            Decision::Partitioned {
                refresh: Some(_),
                ..
            }
        );
        if missing_sensor.is_some() || hash_unknown_dep.is_some() {
            ctx.hash_unknown.insert(base.to_string());
            if !forced {
                let detail = match (&missing_sensor, &hash_unknown_dep) {
                    (Some(s), _) => format!(
                        "reads sensor '{}', which has no recorded output: its value is \
                                 not known until it runs",
                        short_name(s)
                    ),
                    (None, Some(up)) => format!(
                        "depends on '{}', whose inputs include a sensor with no \
                                 recorded output",
                        short_name(up)
                    ),
                    (None, None) => unreachable!(),
                };
                // A partitioned step split across streams is one line.
                if !ctx.steps.iter().any(|r| r.id == base) {
                    ctx.steps.push(unknown_report(
                        ctx.dag,
                        base,
                        "sensor_output_unknown",
                        detail,
                    ));
                }
                ctx.unknown_ids
                    .insert(base.to_string(), "sensor_output_unknown");
                ctx.summary.unknown += step.partition_keys.len().max(1);
                continue;
            }
        }
        // It reads a cutoff asset that would run, or depends on one (#628): whether it runs
        // depends on the bytes that asset writes, which only running it tells.
        if let Some((up, root)) = cutoff_dep {
            ctx.cutoff_unknown.insert(base.to_string(), root.clone());
            if !forced {
                let detail = if up == root {
                    format!(
                        "reads '{root}' (cutoff=True), which would run first: this step runs \
                         only if it writes different bytes"
                    )
                } else {
                    format!(
                        "depends on '{up}', which runs only if '{root}' (cutoff=True) writes \
                         different bytes"
                    )
                };
                if !ctx.steps.iter().any(|r| r.id == base) {
                    ctx.steps
                        .push(unknown_report(ctx.dag, base, "cutoff_pending", detail));
                }
                ctx.unknown_ids.insert(base.to_string(), "cutoff_pending");
                ctx.summary.unknown += step.partition_keys.len().max(1);
                continue;
            }
        }
        // A cutoff asset that would run: what reads it is unknown until it has (#628).
        let runs_any = match &decision {
            Decision::Run(_) => true,
            Decision::Partitioned { missing, .. } => !missing.is_empty(),
            Decision::Cached { .. } => false,
        };
        if runs_any && crate::cache::is_cutoff(ctx.dag, base) {
            ctx.cutoff_unknown
                .insert(base.to_string(), short_name(base).to_string());
        }

        let mut report = report_for(ctx.dag, &step, &decision, true);
        // What changed since the last success (#558): the step, or its first key that runs.
        let missed = match &decision {
            Decision::Run(RunReason::NotMaterialized) if step.partition_keys.is_empty() => {
                Some(step.step_id.display())
            }
            Decision::Partitioned { missing, .. } => {
                missing.first().map(|k| k.display_id(&step.step_id.base))
            }
            _ => None,
        };
        if let Some(id) = missed
            && let Some(changed) =
                crate::changes::since_last_success(ctx.cache, ctx.dag, ctx.state, &id).await
        {
            report.detail = Some(format!("changed: {}", crate::changes::summary(&changed)));
            report.changed = Some(changed);
        }
        // And per key, for the keys the report previews: a changed value of a dict
        // `partitions_from` source, a new key (none), an edited upstream key.
        if let (Decision::Partitioned { missing, .. }, Some(summary)) =
            (&decision, report.partitions.as_mut())
        {
            let mut per_key = std::collections::BTreeMap::new();
            for pk in missing
                .iter()
                .filter(|k| summary.will_run_keys.contains(&k.suffix()))
            {
                let id = pk.display_id(&step.step_id.base);
                if let Some(changed) =
                    crate::changes::since_last_success(ctx.cache, ctx.dag, ctx.state, &id).await
                {
                    per_key.insert(pk.suffix(), changed);
                }
            }
            // The step's own `changed`: its first key that ran before, when the first key
            // that runs is new.
            if report.changed.is_none()
                && let Some(changed) = per_key.values().next()
            {
                report.changed = Some(changed.clone());
                if !forced {
                    report.detail = Some(format!("changed: {}", crate::changes::summary(changed)));
                }
            }
            if !per_key.is_empty() {
                summary.changed = Some(per_key);
            }
        }
        if !forced && !read_sensors.is_empty() {
            let note = read_sensors
                .iter()
                .map(|s| {
                    format!(
                        "assumes sensor '{}' returns the same value as its last run",
                        short_name(s)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            report.detail = Some(match report.detail.take() {
                Some(d) => format!("{d}; {note}"),
                None => note,
            });
        }
        ctx.steps.push(report);
        match decision {
            Decision::Run(_) => {
                ctx.summary.will_run += step.partition_keys.len().max(1);
                if !step.partition_keys.is_empty() {
                    ctx.state
                        .will_run
                        .entry(step.step_id.base.to_string())
                        .or_default()
                        .extend(
                            step.partition_keys
                                .iter()
                                .map(|k| k.display_id(&step.step_id.base)),
                        );
                }
                to_run.push(step);
            }
            Decision::Cached { oref, held, .. } => {
                ctx.summary.cached += 1;
                ctx.all_outputs.insert(step.step_id.display(), oref);
                if !matches!(
                    held,
                    Some(crate::cache::Held::Manual { .. } | crate::cache::Held::Pinned)
                ) {
                    ctx.cached_steps.remember(step);
                }
            }
            Decision::Partitioned {
                cached, missing, ..
            } => {
                ctx.summary.cached += cached.len();
                ctx.summary.will_run += missing.len();
                let any_cached = !cached.is_empty();
                for (pdisplay, oref) in cached {
                    ctx.all_outputs.insert(pdisplay, oref);
                }
                if !missing.is_empty() {
                    ctx.state
                        .will_run
                        .entry(step.step_id.base.to_string())
                        .or_default()
                        .extend(missing.iter().map(|k| k.display_id(&step.step_id.base)));
                    // Its run hashes, for the keys that run only; not a copy of every key.
                    let mut partial = step.with_keys(missing);
                    partial.run_hashes = partial
                        .partition_keys
                        .iter()
                        .filter_map(|k| {
                            let id = k.display_id(&step.step_id.base);
                            step.run_hashes.get(&id).map(|h| (id, h.clone()))
                        })
                        .collect();
                    to_run.push(partial);
                }
                if any_cached {
                    ctx.cached_steps.remember(step);
                }
            }
        }
    }

    Ok(to_run)
}

pub(super) async fn predict_phase(
    ctx: &mut Prediction<'_>,
    phase: &Phase,
) -> Result<(), BarcaError> {
    let ready = expand_prediction(ctx, phase)?;
    let expansion = dispatch::expand_pending_partitions_filtered(
        ctx.dag,
        &ready,
        ctx.all_outputs,
        ctx.pool_size,
        |_, dim, source| {
            dispatch::selected_source(
                ctx.all_outputs,
                ctx.narrowing.as_ref().map(|n| &n.selection),
                dim,
                source,
            )
        },
        |_, dims| {
            if let Some(narrowing) = ctx.narrowing.as_ref() {
                narrowing.selection.prefilter(dims);
            }
        },
    );
    for (base, message) in expansion.failures {
        ctx.steps.push(unknown_report(
            ctx.dag,
            &base,
            "partitions_unknown",
            message,
        ));
        ctx.unknown_ids.insert(base, "partitions_unknown");
        ctx.summary.unknown += 1;
    }
    let mut expanded = expansion.phase;
    if let (Some(narrowing), Some(phase)) = (ctx.narrowing.as_deref_mut(), expanded.as_mut()) {
        narrowing.narrow_projected(phase, &expansion.key_counts);
    }
    let phase_ref = expanded.as_ref().unwrap_or(&ready);
    // The steps of this phase that would execute: what they read has to be there.

    let to_run = predict_steps(ctx, phase_ref).await?;
    let running = Phase {
        reason: phase_ref.reason.clone(),
        streams: vec![crate::planner::WorkerStream {
            stream_id: "dry-run".to_string(),
            steps: to_run,
        }],
    };
    let inputs = recover::input_paths(&dispatch::build_provided_inputs(&running, ctx.all_outputs));
    recover::predict_recomputes(
        inputs,
        ctx.cached_steps,
        ctx.all_outputs,
        ctx.layout,
        ctx.evicted,
        ctx.steps,
        ctx.summary,
        &ctx.state.pinned,
    )?;
    Ok(())
}
