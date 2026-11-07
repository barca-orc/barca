//! Preparing a run: the plan, the targets it computes and those blocked on a Manual node, the
//! existing cache and the pins the run holds.

use super::*;
use std::collections::HashSet;

pub(super) struct Prepared {
    pub(super) dag: std::sync::Arc<Dag>,
    pub(super) targets: Vec<(String, String)>,
    pub(super) job_name: String,
    pub(super) keep_going: bool,
    pub(super) target_label: Option<String>,
    pub(super) pool_size: usize,
    /// The capacity the run's steps share (#436).
    pub(super) capacity: usize,
    pub(super) exec_plan: ExecutionPlan,
    pub(super) plan_warnings: Vec<crate::warnings::PlanWarning>,
    /// `Manual` nodes read as they are (RFC-0008), by id.
    pub(super) pinned: HashMap<String, crate::freshness::Pin>,
    /// Under a tick: the targets that read a `Manual` node that has never been computed.
    pub(super) blocked: Vec<(String, TargetOutcome)>,
    /// Under a tick: the paused nodes (`crate::pauses`); a blocked target whose `failed_node`
    /// is one of them was skipped because it is paused.
    pub(super) paused: HashSet<String>,
    /// `--partition`: what the selection narrowed and skipped.
    pub(super) narrowing: Option<crate::selection::Narrowing>,
}

/// The step reports of a tick's targets blocked on a `Manual` node that has never been
/// computed: `skipped`, reason `blocked`, the detail naming the `Manual` node.
/// A target skipped because it (or a never-computed node it reads) is paused has reason `paused`.
pub(super) fn blocked_steps(
    dag: &Dag,
    blocked: &[(String, TargetOutcome)],
    paused: &HashSet<String>,
) -> Vec<StepReport> {
    blocked
        .iter()
        .map(|(id, outcome)| StepReport {
            id: id.clone(),
            kind: kind_str(dag.get_node(id).map(|n| n.kind())),
            status: Some(StepStatus::Skipped.to_string()),
            reason: Some(
                if outcome
                    .failed_node
                    .as_ref()
                    .is_some_and(|n| paused.contains(n))
                {
                    "paused".to_string()
                } else {
                    StepStatus::Blocked.to_string()
                },
            ),
            detail: outcome.error.clone(),
            ..Default::default()
        })
        .collect()
}

/// The metadata DB for a read, or `None` when there is none yet, or it is an empty file
/// (nothing was computed).
pub(super) async fn existing_cache(
    cfg: &crate::config::ResolvedConfig,
) -> Result<Option<db::CacheReader>, BarcaError> {
    // An empty file is a database the run is about to initialize: nothing was computed.
    if std::fs::metadata(&cfg.db_path).is_ok_and(|m| m.len() > 0) {
        Ok(Some(db::CacheReader::for_config(cfg).await?))
    } else {
        Ok(None)
    }
}

/// The `Manual` nodes a request for `targets` reads as they are (RFC-0008), and what is planned:
/// the cone, not walking past them. A `Manual` node that has never been computed is a usage
/// error; under a tick it blocks the targets that read it instead, which are moved to the
/// returned list. Stale ones get a note on stderr (not under a tick).
pub(super) struct Planned<'a> {
    pub(super) cone: Option<Vec<&'a str>>,
    pub(super) pinned: HashMap<String, crate::freshness::Pin>,
    pub(super) blocked: Vec<(String, TargetOutcome)>,
    pub(super) paused: HashSet<String>,
}

/// The outcome of a tick's target that is not run because `node` is paused.
fn paused_outcome(node: &str, error: String) -> TargetOutcome {
    TargetOutcome {
        status: StepStatus::Blocked.to_string(),
        final_output: None,
        error: Some(error),
        failed_node: Some(node.to_string()),
        failed_phase: None,
        partitions: None,
        key_summary: None,
    }
}

pub(super) async fn pin_manual<'a>(
    dag: &'a Dag,
    targets: &mut Vec<(String, String)>,
    cfg: &crate::config::ResolvedConfig,
    policy: &CachePolicy,
    rules: &crate::freshness::RunRules,
    file_args: &[String],
) -> Result<Planned<'a>, BarcaError> {
    let mut planned = Planned {
        cone: None,
        pinned: HashMap::new(),
        blocked: Vec::new(),
        paused: HashSet::new(),
    };
    if targets.is_empty() {
        return Ok(planned);
    }
    // A tick does not compute a paused node (`crate::pauses`): a paused target is skipped.
    if rules.tick.is_some() {
        planned.paused = db::get_asset_pauses(&cfg.db_path)
            .await?
            .into_iter()
            .map(|p| p.id)
            .collect();
        let paused = &planned.paused;
        targets.retain(|(name, id)| {
            if paused.contains(id) {
                planned
                    .blocked
                    .push((name.clone(), paused_outcome(id, crate::pauses::detail(id))));
                false
            } else {
                true
            }
        });
        if targets.is_empty() {
            return Ok(planned);
        }
    }
    let cache = existing_cache(cfg).await?;
    // Assets pinned with `barca pin`: read as their pinned materialization by every run.
    let user = match &cache {
        Some(cache) => db::pins_on(cache.conn()).await?,
        None => Default::default(),
    };
    let user_ids: HashSet<String> = user.keys().cloned().collect();
    if !rules.pin_manual && user.is_empty() {
        return Ok(planned);
    }
    let ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let walk = crate::freshness::walk_with(
        dag,
        &ids,
        policy,
        &user_ids,
        rules.pin_manual,
        &planned.paused,
    );
    if walk.stops.is_empty() {
        return Ok(planned);
    }
    let pins = crate::freshness::load_pins(cache.as_ref(), dag, &walk.stops, &user).await;
    drop(cache);
    if let Some(first) = pins.lost.first()
        && rules.tick.is_none()
    {
        return Err(BarcaError::Usage(crate::freshness::pin_lost(first)));
    }
    if let Some(first) = pins.never.first()
        && rules.tick.is_none()
    {
        return Err(crate::freshness::never_computed(dag, first, file_args));
    }
    if !pins.never.is_empty() || !pins.lost.is_empty() {
        targets.retain(|(name, id)| {
            let stops = crate::freshness::walk_with(
                dag,
                &[id],
                policy,
                &user_ids,
                rules.pin_manual,
                &planned.paused,
            )
            .stops;
            let (node, error) = if let Some(lost) = stops.iter().find(|s| pins.lost.contains(s)) {
                (
                    lost.clone(),
                    format!(
                        "blocked on '{}', pinned to a materialization that can no longer be read",
                        short_name(lost)
                    ),
                )
            } else if let Some(held) = stops
                .iter()
                .find(|s| pins.never.contains(s) && planned.paused.contains(*s))
            {
                (
                    held.clone(),
                    format!(
                        "blocked on '{}', a paused node that has never been computed",
                        short_name(held)
                    ),
                )
            } else if let Some(manual) = stops.iter().find(|s| pins.never.contains(s)) {
                (
                    manual.clone(),
                    format!(
                        "blocked on '{}', a Manual node that has never been computed",
                        short_name(manual)
                    ),
                )
            } else {
                return true;
            };
            planned.blocked.push((
                name.clone(),
                TargetOutcome {
                    status: StepStatus::Blocked.to_string(),
                    final_output: None,
                    error: Some(error),
                    failed_node: Some(node),
                    failed_phase: None,
                    partitions: None,
                    key_summary: None,
                },
            ));
            false
        });
    }
    let ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let mut cone = crate::freshness::walk_with(
        dag,
        &ids,
        policy,
        &user_ids,
        rules.pin_manual,
        &planned.paused,
    )
    .cone;
    // Outside a tick, the sensors a pinned node reads directly are polled too, so the stale note
    // compares against what they return now, not what they returned last time.
    if rules.tick.is_none() {
        let mut planned_ids: std::collections::HashSet<&str> = cone.iter().copied().collect();
        for id in pins
            .pinned
            .iter()
            .filter(|(_, pin)| pin.user.is_none())
            .map(|(id, _)| id)
        {
            for up in dag.upstream(id) {
                if dag
                    .get_node(up)
                    .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
                {
                    planned_ids.insert(up);
                }
            }
        }
        cone = dag
            .topo_order()
            .into_iter()
            .filter(|id| planned_ids.contains(id))
            .collect();
    }
    planned.cone = Some(cone);
    planned.pinned = pins.pinned;
    Ok(planned)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_run(
    supplied_dag: Option<std::sync::Arc<Dag>>,
    request: &ExecuteRequest<'_>,
    trace: impl Fn(&str),
) -> Result<Prepared, BarcaError> {
    let (target_names, file_args, python, policy, command_label, partition) = (
        request.target_names,
        request.file_args,
        request.python,
        &request.policy,
        request.command_label,
        request.partition.as_ref(),
    );
    let dag = match supplied_dag {
        Some(dag) => dag,
        None => std::sync::Arc::new(build_dag(file_args, python).await?),
    };
    trace("dag_built");

    // A tick's targets are its scheduled node and the `Always` nodes below it, of any kind.
    let resolve_as = if request.rules.tick.is_some() {
        ""
    } else {
        command_label
    };
    let mut targets = resolve_targets(&dag, target_names, resolve_as)?;
    let Planned {
        cone,
        pinned,
        blocked,
        paused,
    } = pin_manual(
        &dag,
        &mut targets,
        request.cfg,
        policy,
        &request.rules,
        file_args,
    )
    .await?;
    let cone: Option<Vec<String>> = cone.map(|c| c.into_iter().map(str::to_string).collect());
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    // A tick reports as its scheduled node's job, as a skipped tick does.
    let job_name = match &request.rules.tick {
        Some(scheduled) => scheduled.clone(),
        None => canonical_job(&target_ids),
    };
    if command_label == "get"
        && target_ids.is_empty()
        && let Some(note) = skipped_tasks_note(&dag, file_args)
    {
        crate::run_line!(Always, "{note}");
    }
    // Several targets: a step failure stops only what depends on it, so every target that can
    // still run does (one run reports every failure). One target keeps the stop-at-first-failure
    // behavior: nothing else in its cone could produce its value.
    let keep_going = targets.len() > 1;
    // For the run record: the targets as given, comma-separated.
    let target_label: Option<String> = (!targets.is_empty()).then(|| {
        targets
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    });

    let sizing = sizing_for(request.cfg, request.options.host.as_ref());
    let pool_size = sizing.pool_size;
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let mut exec_plan = match &cone {
        Some(cone) => {
            let cone: Vec<&str> = cone.iter().map(String::as_str).collect();
            crate::planner::plan_for_nodes(&dag, &cone, &config, partition)
        }
        None => plan_for_targets(&dag, &target_ids, &config, command_label, partition),
    };
    let narrowing = match partition {
        Some(selection) => Some(selection.apply(&dag, &mut exec_plan, &target_ids)?),
        None => None,
    };
    trace("planned");

    if let CachePolicy::RefreshSelective { names, .. } = policy {
        let planned: Option<Vec<&str>> = cone
            .as_ref()
            .map(|c| c.iter().map(String::as_str).collect());
        validate_refresh_names(
            &dag,
            &target_ids,
            names,
            command_label == "get",
            planned.as_deref(),
        )?;
        if let Some(n) = &narrowing {
            n.check_refresh(names)?;
        }
    }
    if let Some(note) = narrowing.as_ref().and_then(|n| n.note()) {
        crate::run_line!(Always, "{note}");
    }
    // A planned step whose file imports a helper that does not parse cannot run (#550).
    dag.check_imports(exec_plan.step_ids())?;
    // Plan-time warnings for the steps this command planned, before anything runs.
    let plan_warnings = crate::warnings::for_plan(&dag, &exec_plan);
    crate::warnings::print(&plan_warnings);
    let capacity = sizing.capacity;
    warn_over_capacity(
        &planned_weights(&exec_plan),
        capacity,
        sizing.capacity_source,
    );
    if let Some(shared) = request.options.host.shared_capacity() {
        warn_over_capacity(
            &planned_weights(&exec_plan),
            shared.total as usize,
            shared.source,
        );
    }

    Ok(Prepared {
        dag,
        targets,
        job_name,
        keep_going,
        target_label,
        pool_size,
        capacity,
        exec_plan,
        plan_warnings,
        pinned,
        blocked,
        paused,
        narrowing,
    })
}
